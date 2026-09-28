//! Integration tests de seguridad empresarial: auth + grants + enforcement.
//! Ejecuta: cargo test -p argentum-engine --test integration_security -- --nocapture

use argentum_common::auth::{Grantee, Privilege};
use argentum_engine::{parser, DatabaseManager};
use std::fs;

fn fresh_dir(name: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("argentum_sec_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir.to_string_lossy().to_string()
}

#[test]
fn root_bootstrap_and_auth_persisted() {
    let dir = fresh_dir("root");
    let mgr = DatabaseManager::new(&dir);
    // auth.json existe y root autentica con "root".
    assert!(std::path::Path::new(&format!("{}/auth.json", dir)).exists());
    assert!(mgr.authenticate("root", "root").is_ok());
    assert!(mgr.authenticate("root", "mala").is_err());
    // Nunca plaintext.
    let raw = fs::read_to_string(format!("{}/auth.json", dir)).unwrap();
    assert!(raw.contains("$argon2"));
    assert!(!raw.contains("\"root\"") || raw.contains("phc_hash"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unprivileged_user_denied_then_granted() {
    let dir = fresh_dir("grant");
    let mut mgr = DatabaseManager::new(&dir);
    mgr.auth_mut().create_user("ana", "secreta123", false).unwrap();
    let ana = mgr.authenticate("ana", "secreta123").unwrap();

    // Sin grants: crear tabla denegado.
    let p = parser::parse("CREA TABLA t (id INT)").unwrap();
    assert!(mgr.execute_current(&ana, p).is_err());

    // Grant CREATE_TABLE + INSERT + SELECT sobre ventas (activa = default; creamos ventas y la usamos como root).
    let root = mgr.authenticate("root", "root").unwrap();
    mgr.create_database("ventas").unwrap();
    mgr.use_database("ventas").unwrap();
    mgr.auth_mut()
        .grant(Privilege::CreateTable, Grantee::User("ana".into()), Some("ventas"), None)
        .unwrap();
    mgr.auth_mut()
        .grant(Privilege::Insert, Grantee::User("ana".into()), Some("ventas"), None)
        .unwrap();
    mgr.auth_mut()
        .grant(Privilege::Select, Grantee::User("ana".into()), Some("ventas"), None)
        .unwrap();

    let p = parser::parse("CREA TABLA t (id INT)").unwrap();
    assert!(mgr.execute_current(&ana, p).is_ok());
    let p = parser::parse("AGREGAR EN t (id) VALORES (1)").unwrap();
    assert!(mgr.execute_current(&ana, p).is_ok());
    let p = parser::parse("ELIGE * DE t").unwrap();
    assert!(mgr.execute_current(&ana, p).is_ok());

    // DROP sin privilegio sigue denegado.
    let p = parser::parse("BORRA TABLA t").unwrap();
    assert!(mgr.execute_current(&ana, p).is_err());

    // Root bypassa todo.
    let p = parser::parse("BORRA TABLA t").unwrap();
    assert!(mgr.execute_current(&root, p).is_ok());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn group_grant_covers_members_and_use_database_scoped() {
    let dir = fresh_dir("groups");
    let mut mgr = DatabaseManager::new(&dir);
    mgr.auth_mut().create_user("pepe", "clave1234", false).unwrap();
    mgr.auth_mut().create_group("vendedores", "Ventas").unwrap();
    mgr.auth_mut().add_member("pepe", "vendedores").unwrap();
    mgr.create_database("tienda").unwrap();

    // Sin USE_DATABASE sobre tienda → denegado.
    let pepe = mgr.authenticate("pepe", "clave1234").unwrap();
    let p = parser::parse("USA BASE tienda").unwrap();
    assert!(mgr.check_plan(&pepe, &p).is_err());

    // Grant USE al grupo → permitido.
    mgr.auth_mut()
        .grant(Privilege::UseDatabase, Grantee::Group("vendedores".into()), Some("tienda"), None)
        .unwrap();
    assert!(mgr.check_plan(&pepe, &p).is_ok());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parser_recognizes_security_commands_bilingue() {
    use argentum_engine::LogicalPlan;
    assert!(matches!(
        parser::parse("CREATE USER ana IDENTIFIED BY 'x1234'").unwrap(),
        LogicalPlan::CreateUser { .. }
    ));
    assert!(matches!(
        parser::parse("CREA USUARIO ana IDENTIFICADO POR 'x1234'").unwrap(),
        LogicalPlan::CreateUser { .. }
    ));
    assert!(matches!(
        parser::parse("DROP USER ana").unwrap(),
        LogicalPlan::DropUser { .. }
    ));
    assert!(matches!(
        parser::parse("BORRA USUARIO ana").unwrap(),
        LogicalPlan::DropUser { .. }
    ));
    assert!(matches!(
        parser::parse("GRANT SELECT ON ventas.* TO GROUP vendedores").unwrap(),
        LogicalPlan::Grant { .. }
    ));
    assert!(matches!(
        parser::parse("OTORGA SELECT EN ventas.* A GRUPO vendedores").unwrap(),
        LogicalPlan::Grant { .. }
    ));
    assert!(matches!(
        parser::parse("SHOW USERS").unwrap(),
        LogicalPlan::ShowUsers
    ));
    assert!(matches!(
        parser::parse("MUESTRA USUARIOS").unwrap(),
        LogicalPlan::ShowUsers
    ));
}
