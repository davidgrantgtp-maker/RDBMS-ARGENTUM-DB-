//! argentum-common/src/auth.rs - Seguridad empresarial: usuarios, grupos y privilegios estilo MySQL.
//! Persistencia en `{base_dir}/auth.json` con escritura atómica (tmp + rename + fsync).
//! Passwords con Argon2id (PHC string). Nunca se guarda texto plano.
//!
//! Modelo:
//!   - User: nombre, hash PHC, grupos, superuser, disabled
//!   - Group (rol): nombre + descripción. Miembros vía User.groups.
//!   - Grant: (grantee user|group, privilege, scope db/table|global)
//! Chequeo: superuser bypass, deny si disabled, match directo + por grupos,
//!   con herencia Global -> Database -> Table.

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Privilegios
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Privilege {
    // Nivel servidor / base
    CreateDatabase,
    DropDatabase,
    ShowDatabases,
    UseDatabase,
    CreateUser,
    DropUser,
    Grant,
    Admin, // bypass total (superuser funcional sin ser root)
    // Nivel tabla / datos
    CreateTable,
    AlterTable,
    DropTable,
    DescribeTable,
    ShowTables,
    Insert,
    Delete,
    Update,
    Select,
    Search,
}

impl Privilege {
    pub fn all() -> Vec<Privilege> {
        vec![
            Privilege::CreateDatabase,
            Privilege::DropDatabase,
            Privilege::ShowDatabases,
            Privilege::UseDatabase,
            Privilege::CreateUser,
            Privilege::DropUser,
            Privilege::Grant,
            Privilege::Admin,
            Privilege::CreateTable,
            Privilege::AlterTable,
            Privilege::DropTable,
            Privilege::DescribeTable,
            Privilege::ShowTables,
            Privilege::Insert,
            Privilege::Delete,
            Privilege::Update,
            Privilege::Select,
            Privilege::Search,
        ]
    }

    pub fn parse(s: &str) -> Option<Self> {
        let u = s.trim().to_uppercase().replace(['-', ' '], "_");
        match u.as_str() {
            "CREATE_DATABASE" | "CREATEDB" => Some(Privilege::CreateDatabase),
            "DROP_DATABASE" | "DROPDDB" => Some(Privilege::DropDatabase),
            "SHOW_DATABASES" | "SHOWDB" => Some(Privilege::ShowDatabases),
            "USE_DATABASE" | "USEDB" => Some(Privilege::UseDatabase),
            "CREATE_USER" | "CREATEUSER" => Some(Privilege::CreateUser),
            "DROP_USER" | "DROPUSER" => Some(Privilege::DropUser),
            "GRANT" | "GRANT_OPTION" => Some(Privilege::Grant),
            "ADMIN" | "ALL" | "SUPERUSER" | "SUPER" => Some(Privilege::Admin),
            "CREATE_TABLE" | "CREATETABLE" | "CREATE" => Some(Privilege::CreateTable),
            "ALTER_TABLE" | "ALTERTABLE" | "ALTER" => Some(Privilege::AlterTable),
            "DROP_TABLE" | "DROPTABLE" | "DROP" => Some(Privilege::DropTable),
            "DESCRIBE_TABLE" | "DESCRIBE" => Some(Privilege::DescribeTable),
            "SHOW_TABLES" | "SHOWTABLES" => Some(Privilege::ShowTables),
            "INSERT" => Some(Privilege::Insert),
            "DELETE" => Some(Privilege::Delete),
            "UPDATE" => Some(Privilege::Update),
            "SELECT" => Some(Privilege::Select),
            "SEARCH" => Some(Privilege::Search),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Privilege::CreateDatabase => "CREATE_DATABASE",
            Privilege::DropDatabase => "DROP_DATABASE",
            Privilege::ShowDatabases => "SHOW_DATABASES",
            Privilege::UseDatabase => "USE_DATABASE",
            Privilege::CreateUser => "CREATE_USER",
            Privilege::DropUser => "DROP_USER",
            Privilege::Grant => "GRANT",
            Privilege::Admin => "ADMIN",
            Privilege::CreateTable => "CREATE_TABLE",
            Privilege::AlterTable => "ALTER_TABLE",
            Privilege::DropTable => "DROP_TABLE",
            Privilege::DescribeTable => "DESCRIBE_TABLE",
            Privilege::ShowTables => "SHOW_TABLES",
            Privilege::Insert => "INSERT",
            Privilege::Delete => "DELETE",
            Privilege::Update => "UPDATE",
            Privilege::Select => "SELECT",
            Privilege::Search => "SEARCH",
        }
    }
}

// ---------------------------------------------------------------------------
// Grantee / Grant
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grantee {
    User(String),  // nombre lower
    Group(String), // nombre lower
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub grantee: Grantee,
    pub privilege: Privilege,
    /// None = global. Some(db) = solo esa base (lower).
    pub db: Option<String>,
    /// None = toda la base. Some(tabla lower) requiere db.is_some().
    pub table: Option<String>,
}

// ---------------------------------------------------------------------------
// User / Group
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub name: String, // lower
    pub display: String,
    pub phc_hash: String, // Argon2 PHC, jamás plaintext
    pub groups: Vec<String>, // grupos lower
    pub is_superuser: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String, // lower
    pub display: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Sesión
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SessionContext {
    pub username: String, // lower
    pub is_superuser: bool,
}

impl SessionContext {
    pub fn new(username: &str, is_superuser: bool) -> Self {
        Self { username: username.to_lowercase(), is_superuser }
    }
    /// Sesión interna/bootstrap (solo para tests y arranque). No usar en REPL de red.
    pub fn bootstrap_root() -> Self {
        Self { username: "root".into(), is_superuser: true }
    }
}

// ---------------------------------------------------------------------------
// AuthCatalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthSnapshot {
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    groups: Vec<Group>,
    #[serde(default)]
    grants: Vec<Grant>,
}

pub struct AuthCatalog {
    users: HashMap<String, User>,   // lower -> User
    groups: HashMap<String, Group>, // lower -> Group
    grants: Vec<Grant>,
    persist_path: Option<String>,
}

impl AuthCatalog {
    pub fn new() -> Self {
        let mut c = Self {
            users: HashMap::new(),
            groups: HashMap::new(),
            grants: Vec::new(),
            persist_path: None,
        };
        c.ensure_root();
        c
    }

    pub fn with_persist(path: &str) -> Self {
        let mut c = Self {
            users: HashMap::new(),
            groups: HashMap::new(),
            grants: Vec::new(),
            persist_path: Some(path.to_string()),
        };
        c.load();
        c.ensure_root();
        c.persist();
        c
    }

    // -- hashing --

    pub fn hash_password(password: &str) -> Result<String, String> {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| format!("hash error: {}", e))
    }

    pub fn verify_password(phc: &str, password: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc) else { return false };
        Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
    }

    // -- bootstrap root --

    fn ensure_root(&mut self) {
        if !self.users.contains_key("root") {
            // Password inicial: "root". El operador debe cambiarla en el primer arranque.
            // Se hashea en memoria; se persiste al primer persist().
            if let Ok(phc) = Self::hash_password("root") {
                self.users.insert(
                    "root".into(),
                    User {
                        name: "root".into(),
                        display: "root".into(),
                        phc_hash: phc,
                        groups: vec![],
                        is_superuser: true,
                        disabled: false,
                    },
                );
            }
        } else if let Some(u) = self.users.get_mut("root") {
            // root siempre es superuser y habilitado (anti-lockout).
            u.is_superuser = true;
            u.disabled = false;
        }
        // Grupo admin por defecto.
        if !self.groups.contains_key("admin") {
            self.groups.insert(
                "admin".into(),
                Group { name: "admin".into(), display: "admin".into(), description: "Administradores".into() },
            );
        }
    }

    // -- persistencia atómica --

    pub fn persist(&self) {
        let Some(path) = &self.persist_path else { return };
        let snap = AuthSnapshot {
            users: self.users.values().cloned().collect(),
            groups: self.groups.values().cloned().collect(),
            grants: self.grants.clone(),
        };
        let Ok(out) = serde_json::to_string_pretty(&snap) else { return };
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = format!("{}.tmp", path);
        if std::fs::write(&tmp, out.as_bytes()).is_ok() {
            if std::fs::rename(&tmp, path).is_ok() {
                if let Ok(f) = std::fs::OpenOptions::new().read(true).open(path) {
                    let _ = f.sync_all();
                }
            }
        }
    }

    fn load(&mut self) {
        let Some(path) = self.persist_path.clone() else { return };
        let Ok(content) = std::fs::read_to_string(&path) else { return };
        if content.trim().is_empty() {
            return;
        }
        let Ok(snap): Result<AuthSnapshot, _> = serde_json::from_str(&content) else {
            // auth.json corrupto: no pisar, degradar a vacío (ensure_root recrea root).
            return;
        };
        for u in snap.users {
            self.users.insert(u.name.to_lowercase(), u);
        }
        for g in snap.groups {
            self.groups.insert(g.name.to_lowercase(), g);
        }
        // Normalizar grants a lower.
        for mut gr in snap.grants {
            gr.db = gr.db.map(|d| d.to_lowercase());
            gr.table = gr.table.map(|t| t.to_lowercase());
            match &mut gr.grantee {
                Grantee::User(n) | Grantee::Group(n) => *n = n.to_lowercase(),
            }
            self.grants.push(gr);
        }
    }

    // -- validación de nombres --

    fn valid_name(name: &str) -> Result<String, String> {
        let n = name.trim();
        if n.is_empty() {
            return Err("Nombre vacío / empty name".into());
        }
        if n.len() > 64 {
            return Err("Nombre demasiado largo (max 64)".into());
        }
        if n.contains('/') || n.contains('\\') || n.contains('\0') || n.contains(' ') {
            return Err(format!("Nombre inválido '{}': sin /, \\, espacios ni NUL", n));
        }
        Ok(n.to_lowercase())
    }

    // -- usuarios --

    pub fn create_user(&mut self, name: &str, password: &str, superuser: bool) -> Result<(), String> {
        let key = Self::valid_name(name)?;
        if self.users.contains_key(&key) {
            return Err(format!("Usuario '{}' ya existe / already exists", name));
        }
        if password.len() < 4 {
            return Err("Password muy corto (mín 4) / too short".into());
        }
        let phc = Self::hash_password(password)?;
        self.users.insert(
            key.clone(),
            User {
                name: key,
                display: name.trim().to_string(),
                phc_hash: phc,
                groups: Vec::new(),
                is_superuser: superuser,
                disabled: false,
            },
        );
        self.persist();
        Ok(())
    }

    pub fn drop_user(&mut self, name: &str) -> Result<(), String> {
        let key = name.trim().to_lowercase();
        if key == "root" {
            return Err("No se puede borrar 'root' / cannot drop root".into());
        }
        if self.users.remove(&key).is_none() {
            return Err(format!("Usuario '{}' no existe", name));
        }
        // Limpiar grants directos.
        self.grants.retain(|g| !matches!(&g.grantee, Grantee::User(n) if n == &key));
        self.persist();
        Ok(())
    }

    pub fn set_password(&mut self, name: &str, new_password: &str) -> Result<(), String> {
        let key = name.trim().to_lowercase();
        if new_password.len() < 4 {
            return Err("Password muy corto (mín 4)".into());
        }
        let Some(u) = self.users.get_mut(&key) else {
            return Err(format!("Usuario '{}' no existe", name));
        };
        u.phc_hash = Self::hash_password(new_password)?;
        self.persist();
        Ok(())
    }

    pub fn set_disabled(&mut self, name: &str, disabled: bool) -> Result<(), String> {
        let key = name.trim().to_lowercase();
        if key == "root" && disabled {
            return Err("No se puede deshabilitar 'root'".into());
        }
        let Some(u) = self.users.get_mut(&key) else {
            return Err(format!("Usuario '{}' no existe", name));
        };
        u.disabled = disabled;
        self.persist();
        Ok(())
    }

    pub fn authenticate(&self, name: &str, password: &str) -> Result<SessionContext, String> {
        let key = name.trim().to_lowercase();
        let Some(u) = self.users.get(&key) else {
            return Err("Usuario o password inválidos / invalid credentials".into());
        };
        if u.disabled {
            return Err("Usuario deshabilitado / disabled".into());
        }
        if !Self::verify_password(&u.phc_hash, password) {
            return Err("Usuario o password inválidos / invalid credentials".into());
        }
        Ok(SessionContext::new(&u.name, u.is_superuser))
    }

    // -- grupos --

    pub fn create_group(&mut self, name: &str, description: &str) -> Result<(), String> {
        let key = Self::valid_name(name)?;
        if self.groups.contains_key(&key) {
            return Err(format!("Grupo '{}' ya existe", name));
        }
        self.groups.insert(
            key.clone(),
            Group { name: key, display: name.trim().to_string(), description: description.to_string() },
        );
        self.persist();
        Ok(())
    }

    pub fn drop_group(&mut self, name: &str) -> Result<(), String> {
        let key = name.trim().to_lowercase();
        if key == "admin" {
            return Err("No se puede borrar el grupo 'admin'".into());
        }
        if self.groups.remove(&key).is_none() {
            return Err(format!("Grupo '{}' no existe", name));
        }
        // Quitar membresías y grants del grupo.
        for u in self.users.values_mut() {
            u.groups.retain(|g| g != &key);
        }
        self.grants.retain(|g| !matches!(&g.grantee, Grantee::Group(n) if n == &key));
        self.persist();
        Ok(())
    }

    pub fn add_member(&mut self, username: &str, group: &str) -> Result<(), String> {
        let uk = username.trim().to_lowercase();
        let gk = group.trim().to_lowercase();
        if !self.groups.contains_key(&gk) {
            return Err(format!("Grupo '{}' no existe", group));
        }
        let Some(u) = self.users.get_mut(&uk) else {
            return Err(format!("Usuario '{}' no existe", username));
        };
        if !u.groups.contains(&gk) {
            u.groups.push(gk);
        }
        self.persist();
        Ok(())
    }

    pub fn remove_member(&mut self, username: &str, group: &str) -> Result<(), String> {
        let uk = username.trim().to_lowercase();
        let gk = group.trim().to_lowercase();
        let Some(u) = self.users.get_mut(&uk) else {
            return Err(format!("Usuario '{}' no existe", username));
        };
        u.groups.retain(|g| g != &gk);
        self.persist();
        Ok(())
    }

    // -- grants --

    pub fn grant(
        &mut self,
        privilege: Privilege,
        grantee: Grantee,
        db: Option<&str>,
        table: Option<&str>,
    ) -> Result<(), String> {
        // Validar grantee existe.
        match &grantee {
            Grantee::User(n) => {
                if !self.users.contains_key(&n.to_lowercase()) {
                    return Err(format!("Usuario '{}' no existe", n));
                }
            }
            Grantee::Group(n) => {
                if !self.groups.contains_key(&n.to_lowercase()) {
                    return Err(format!("Grupo '{}' no existe", n));
                }
            }
        }
        let db = db.map(|d| d.to_lowercase());
        let table = table.map(|t| t.to_lowercase());
        if table.is_some() && db.is_none() {
            return Err("GRANT sobre tabla requiere base / table grant needs database".into());
        }
        let mut g = Grant { grantee, privilege, db, table };
        match &mut g.grantee {
            Grantee::User(n) | Grantee::Group(n) => *n = n.to_lowercase(),
        }
        // Idempotente: no duplicar.
        if !self.grants.iter().any(|e| {
            e.privilege == g.privilege && e.grantee == g.grantee && e.db == g.db && e.table == g.table
        }) {
            self.grants.push(g);
            self.persist();
        }
        Ok(())
    }

    pub fn revoke(
        &mut self,
        privilege: Privilege,
        grantee: Grantee,
        db: Option<&str>,
        table: Option<&str>,
    ) -> Result<(), String> {
        let db = db.map(|d| d.to_lowercase());
        let table = table.map(|t| t.to_lowercase());
        let key = match &grantee {
            Grantee::User(n) | Grantee::Group(n) => n.to_lowercase(),
        };
        let before = self.grants.len();
        self.grants.retain(|e| {
            let ek = match &e.grantee {
                Grantee::User(n) | Grantee::Group(n) => n,
            };
            !(e.privilege == privilege
                && *ek == key
                && std::mem::discriminant(&e.grantee) == std::mem::discriminant(&grantee)
                && e.db == db
                && e.table == table)
        });
        if self.grants.len() == before {
            return Err("GRANT no encontrado / grant not found".into());
        }
        self.persist();
        Ok(())
    }

    // -- chequeo (enforcement) --

    /// ¿`username` puede ejercer `required` en scope (db, table)?
    /// Reglas: root/superuser o ADMIN bypass; disabled deny; match directo o por grupos;
    /// Global cubre todo; Database cubre sus tablas; Table es exacto.
    pub fn check(
        &self,
        username: &str,
        required: &Privilege,
        db: Option<&str>,
        table: Option<&str>,
    ) -> Result<(), String> {
        let key = username.trim().to_lowercase();
        let Some(u) = self.users.get(&key) else {
            return Err(format!("Acceso denegado: usuario '{}' desconocido", username));
        };
        if u.disabled {
            return Err("Acceso denegado: usuario deshabilitado".into());
        }
        if u.is_superuser {
            return Ok(());
        }
        let db = db.map(|d| d.to_lowercase());
        let table = table.map(|t| t.to_lowercase());

        // Identidades a evaluar: usuario + sus grupos.
        let mut identities = vec![Grantee::User(key.clone())];
        for g in &u.groups {
            identities.push(Grantee::Group(g.clone()));
        }

        for grant in &self.grants {
            if !identities.contains(&grant.grantee) {
                continue;
            }
            // ADMIN cubre todo.
            let priv_ok = grant.privilege == *required || grant.privilege == Privilege::Admin;
            if !priv_ok {
                continue;
            }
            // Scope: grant global (db None) cubre todo.
            // Grant de db cubre db exacta y, si no pide tabla específica, todas sus tablas.
            // Grant de tabla exige db+tabla exactas.
            match (&grant.db, &grant.table, &db, &table) {
                (None, None, _, _) => return Ok(()),
                (Some(gdb), None, Some(rdb), _) if gdb == rdb => return Ok(()),
                (Some(gdb), Some(gt), Some(rdb), Some(rt)) if gdb == rdb && gt == rt => return Ok(()),
                // Grant global de SHOW/USE listado: ya cubierto por (None,None).
                _ => continue,
            }
        }
        Err(format!(
            "Acceso denegado para '{}': falta {}{} / access denied",
            username,
            required.as_str(),
            match (&db, &table) {
                (Some(d), Some(t)) => format!(" sobre {}.{}", d, t),
                (Some(d), None) => format!(" sobre base {}", d),
                _ => String::new(),
            }
        ))
    }

    // -- listado (para SHOW USERS/GROUPS/GRANTS) --

    pub fn list_users(&self) -> Vec<User> {
        let mut v: Vec<User> = self.users.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn list_groups(&self) -> Vec<Group> {
        let mut v: Vec<Group> = self.groups.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn grants_for(&self, grantee: &Grantee) -> Vec<Grant> {
        self.grants.iter().filter(|g| &g.grantee == grantee).cloned().collect()
    }

    pub fn all_grants(&self) -> Vec<Grant> {
        self.grants.clone()
    }

    pub fn get_user(&self, name: &str) -> Option<User> {
        self.users.get(&name.trim().to_lowercase()).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_exists_and_authenticates() {
        let a = AuthCatalog::new();
        assert!(a.get_user("root").unwrap().is_superuser);
        assert!(a.authenticate("root", "root").is_ok());
        assert!(a.authenticate("root", "wrong").is_err());
        // Nunca plaintext.
        assert!(!a.get_user("root").unwrap().phc_hash.contains("root\""));
        assert!(a.get_user("root").unwrap().phc_hash.starts_with("$argon2"));
    }

    #[test]
    fn user_group_grant_flow() {
        let mut a = AuthCatalog::new();
        a.create_user("ana", "secreta123", false).unwrap();
        a.create_group("ventas", "Equipo ventas").unwrap();
        a.add_member("ana", "ventas").unwrap();
        // Sin grant → denegado.
        assert!(a.check("ana", &Privilege::Select, Some("ventas"), Some("t")).is_err());
        // Grant al grupo sobre la base → permite tablas de esa base.
        a.grant(Privilege::Select, Grantee::Group("ventas".into()), Some("ventas"), None).unwrap();
        assert!(a.check("ana", &Privilege::Select, Some("ventas"), Some("t")).is_ok());
        assert!(a.check("ana", &Privilege::Select, Some("otra"), Some("t")).is_err());
        // Revoke → denegado de nuevo.
        a.revoke(Privilege::Select, Grantee::Group("ventas".into()), Some("ventas"), None).unwrap();
        assert!(a.check("ana", &Privilege::Select, Some("ventas"), Some("t")).is_err());
    }

    #[test]
    fn drop_root_and_admin_protected() {
        let mut a = AuthCatalog::new();
        assert!(a.drop_user("root").is_err());
        assert!(a.drop_group("admin").is_err());
        assert!(a.create_user("x", "123", false).is_err()); // password corto
    }
}
