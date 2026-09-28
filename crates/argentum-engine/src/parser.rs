//! crates/aether-engine/src/parser.rs - Parser SQL lightweight BILINGÜE ES/EN para AETHER DB v1
//! Soporta CREATE/CREA, ALTER/CAMBIA, DROP/BORRA, UPDATE/ACTUALIZA, INSERT/AGREGAR, DELETE/BORRAR, SELECT/ELIGE, SEARCH/BUSCA
//! con GROUP BY/AGRUPA POR, ORDER BY/ORDENA POR, LIMIT/LIMITE, COUNT/CUENTA, WHERE/DONDE, FROM/DE, SET/ESTABLECE, etc.
//! Sin dependencia externa (offline). Case-insensitive, con y sin tildes.

use crate::{LogicalPlan, Projection, OrderBy};
use argentum_common::auth::{Grantee, Privilege};
use argentum_common::catalog::{ColumnDef, DataType, IdentitySpec};

fn trim_semicolon(s: &str) -> &str {
    s.trim().trim_end_matches(';').trim()
}

#[allow(dead_code)]
fn find_keyword(haystack_upper: &str, keyword: &str) -> Option<usize> {
    haystack_upper.find(keyword)
}

fn extract_table_name(s: &str) -> String {
    s.trim().split_whitespace().next().unwrap_or("").trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

fn is_show_tables(upper: &str) -> bool {
    let u = upper.trim();
    matches!(u, "MUESTRA TABLAS" | "MOSTRAR TABLAS" | "SHOW TABLES" | "LISTA TABLAS" | "LISTAR TABLAS" | "VER TABLAS" | "QUE TABLAS HAY" | "MOSTRAME TODO")
        || u == "MUESTRA TABLAS;" || u == "SHOW TABLES;"
        || u.contains("MUESTRA TABLAS") && !u.contains("MUESTRA TABLA ") // plural, not singular
}

// Helpers para bases de datos
fn strip_create_database(sql: &str) -> Option<String> {
    for prefix in &["CREATE DATABASE", "CREAR BASE", "CREA BASE", "CREA BASE DE DATOS", "CREATE BASE"] {
        if let Some(rest) = strip_prefix_ci(sql, prefix) {
            // Para "CREA BASE DE DATOS foo" el prefix matcheó "CREA BASE" y quedó "DE DATOS foo"
            // (o si matcheó "CREA BASE DE DATOS" quedó "foo"). En el primer caso, saltar "DE DATOS".
            let mut r = rest.trim();
            let r_up = r.to_uppercase();
            if r_up.starts_with("DE DATOS ") || r_up.starts_with("DE DATOS") && r_up.len() > "DE DATOS".len() {
                r = r["DE DATOS".len()..].trim();
            }
            // También soportar "DE <name>" (raro pero tolerante)
            let name = extract_table_name(r);
            if !name.is_empty() && !name.eq_ignore_ascii_case("TABLE") && !name.eq_ignore_ascii_case("TABLA") && name.to_uppercase() != "DE" && name.to_uppercase() != "DATOS" {
                return Some(name);
            }
        }
    }
    None
}
fn strip_drop_database(sql: &str) -> Option<String> {
    for prefix in &["DROP DATABASE", "BORRA BASE", "ELIMINA BASE", "BORRAR BASE", "DROP BASE", "ELIMINAR BASE", "BORRA BASE DE DATOS", "ELIMINA BASE DE DATOS"] {
        if let Some(rest) = strip_prefix_ci(sql, prefix) {
            let mut r = rest.trim();
            let r_up = r.to_uppercase();
            if r_up.starts_with("DE DATOS ") || (r_up.starts_with("DE DATOS") && r_up.len() > "DE DATOS".len()) {
                r = r["DE DATOS".len()..].trim();
            }
            let name = extract_table_name(r);
            if !name.is_empty() && name.to_uppercase() != "DE" && name.to_uppercase() != "DATOS" {
                return Some(name);
            }
        }
    }
    None
}
fn strip_use_database(sql: &str) -> Option<String> {
    for prefix in &["USE DATABASE", "USA BASE", "USA BASE DE DATOS", "USE BASE", "USA", "USE"] {
        if let Some(rest) = strip_prefix_ci(sql, prefix) {
            // Para "USA" solo, el resto es el nombre directo, pero evitar "USA BASE" ya manejado
            let mut r = rest.trim();
            // Si prefix fue "USE" y resto empieza con "DATABASE" o "BASE", saltarlo
            let r_up = r.to_uppercase();
            if r_up.starts_with("DATABASE ") { r = r["DATABASE".len()..].trim(); }
            else if r_up.starts_with("BASE ") { r = r["BASE".len()..].trim(); }
            else if r_up.starts_with("BASE DE DATOS ") { r = r["BASE DE DATOS".len()..].trim(); }
            let name = extract_table_name(r);
            if !name.is_empty() && name.to_uppercase() != "BASE" && name.to_uppercase() != "DATABASE" {
                return Some(name);
            }
        }
    }
    None
}
fn is_show_databases(upper: &str) -> bool {
    let u = upper.trim();
    matches!(u, "MUESTRA BASES" | "MOSTRAR BASES" | "SHOW DATABASES" | "LISTA BASES" | "LISTAR BASES" | "VER BASES" | "SHOW BASES")
        || u.contains("MUESTRA BASES") || u.contains("SHOW DATABASES") || u.contains("MOSTRAR BASES")
}

fn parse_describe(sql: &str) -> Result<LogicalPlan, String> {
    // Soporta múltiples variantes ES/EN, todas case-insensitive, con y sin "TABLA/TABLE"
    // Orden: los más largos primero para evitar prefijo ambiguo
    let prefixes = [
        "MUESTRA ESTRUCTURA DE TABLA",
        "MOSTRAR ESTRUCTURA DE TABLA",
        "ESTRUCTURA DE TABLA",
        "ESQUEMA DE TABLA",
        "SHOW CREATE TABLE",
        "MUESTRA ESTRUCTURA DE",
        "MOSTRAR ESTRUCTURA DE",
        "ESTRUCTURA DE",
        "ESQUEMA DE",
        "MUESTRA ESTRUCTURA",
        "MOSTRAR ESTRUCTURA",
        "MUESTRA TABLA",
        "MOSTRAR TABLA",
        "DESCRIBE TABLE",
        "DESCRIBE TABLA",
        "SHOW TABLE",
        "DESCRIBE",
        "ESTRUCTURA",
        "ESQUEMA",
        "MUESTRA",
        "MOSTRAR",
        "SHOW",
    ];
    let mut rest_opt: Option<&str> = None;
    for p in &prefixes {
        if let Some(r) = strip_prefix_ci(sql, p) {
            rest_opt = Some(r);
            break;
        }
    }
    let rest = rest_opt.ok_or("DESCRIBE/MUESTRA falta prefijo")?;
    let table = extract_table_name(rest);
    if table.is_empty() {
        return Err("Falta nombre de tabla. Uso: DESCRIBE TABLA <tabla> / MUESTRA TABLA <tabla> / ESTRUCTURA <tabla>".into());
    }
    // Validar que no haya tokens extra (solo table name)
    let extra = rest[table.len()..].trim();
    // Permitir que el resto sea solo table name, pero si hay algo extra que no sea vacío y no sea tabla, ignorar?
    // Si hay más tokens, tomar solo primer token como tabla y si hay resto, error si no es vacío
    let remaining_tokens: Vec<&str> = extra.split_whitespace().collect();
    // Si remaining_tokens no está vacío, significa que había más de una palabra, pero table ya extrajo primera, así que si hay más, es error o ignorar
    // Para "MUESTRA ESTRUCTURA DE TABLA productos" ya consumimos prefix, rest es "productos", extra es "" después de tabla, ok
    // Para "DESCRIBE TABLE productos extra" -> extra contiene "extra", debería ser error pero lo ignoramos y solo usamos tabla
    Ok(LogicalPlan::DescribeTable { table })
}

/// Helper: verifica si s empieza con alguno de los prefijos (case-insensitive), retorna resto y índice
fn strip_prefix_any<'a>(s: &'a str, prefixes: &[&str]) -> Option<(&'a str, usize)> {
    let up = s.trim_start().to_uppercase();
    for (i, p) in prefixes.iter().enumerate() {
        let pu = p.to_uppercase();
        if up.starts_with(&pu) {
            // Asegurar que sea palabra completa: siguiente char es espacio, '(' o fin, o es multi-palabra con espacio
            let rest_start = p.len();
            // Verificar que original s tenga al menos p.len() y que coincida case-insensitive
            // Usamos slicing por p.len() ya que p es ASCII
            let remainder = s.trim_start()[p.len()..].trim_start();
            // Para prefijos como "CREA" no debe confundir "CREA TABLA" con "CREA"? Pero strip_prefix_any se llama con lista ordenada por longitud desc
            return Some((remainder, i));
        }
    }
    None
}

/// Helper: encuentra la primera ocurrencia de cualquiera de los keywords (con espacios alrededor) en haystack
fn find_any_with_spaces(hay_upper: &str, keywords: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, kw) in keywords.iter().enumerate() {
        let pattern = format!(" {} ", kw.to_uppercase());
        // Buscar con espacios, también al inicio/fin
        if let Some(pos) = hay_upper.find(&pattern) {
            let real_pos = pos + 1; // skip leading space
            if best.is_none() || real_pos < best.unwrap().0 {
                best = Some((real_pos, idx));
            }
        }
        // También buscar al inicio "KW "
        let start_pat = format!("{} ", kw.to_uppercase());
        if hay_upper.starts_with(&start_pat) {
            if best.is_none() || 0 < best.unwrap().0 {
                best = Some((0, idx));
            }
        }
        // Buscar " KW" al final
        let end_pat = format!(" {}", kw.to_uppercase());
        if hay_upper.ends_with(&end_pat) {
            let pos = hay_upper.len() - end_pat.len() + 1;
            if best.is_none() || pos < best.unwrap().0 {
                // actually we want earliest, so not update if later
            }
        }
    }
    // Fallback: búsqueda simple sin exigir espacios (para casos como "WHERE " al inicio de remaining)
    if best.is_none() {
        for (idx, kw) in keywords.iter().enumerate() {
            let kw_up = kw.to_uppercase();
            if let Some(pos) = hay_upper.find(&kw_up) {
                // Verificar que sea palabra separada
                let before_ok = pos == 0 || hay_upper.as_bytes()[pos-1] == b' ';
                let after = pos + kw_up.len();
                let after_ok = after >= hay_upper.len() || hay_upper.as_bytes()[after] == b' ' || hay_upper.as_bytes()[after] == b'(';
                if before_ok && after_ok {
                    if best.is_none() || pos < best.unwrap().0 {
                        best = Some((pos, idx));
                    }
                }
            }
        }
    }
    best
}

fn parse_column_defs(defs: &str) -> Result<Vec<ColumnDef>, String> {
    let mut cols = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in defs.char_indices() {
        if c == '(' { depth += 1; }
        if c == ')' { depth -= 1; }
        if c == ',' && depth == 0 {
            let col_str = defs[start..i].trim();
            if !col_str.is_empty() { cols.push(parse_single_col(col_str)?); }
            start = i + c.len_utf8();
        }
    }
    let last = defs[start..].trim();
    if !last.is_empty() { cols.push(parse_single_col(last)?); }
    if cols.is_empty() { return Err("CREATE TABLE requires at least one column".into()); }
    Ok(cols)
}

fn detect_identity(s_upper: &str) -> Option<IdentitySpec> {
    if s_upper.contains("GENERATED ALWAYS AS IDENTITY") || s_upper.contains("GENERADO SIEMPRE COMO IDENTIDAD") {
        return Some(IdentitySpec::new_always());
    }
    if s_upper.contains("GENERATED BY DEFAULT AS IDENTITY") || s_upper.contains("GENERADO POR DEFECTO COMO IDENTIDAD") {
        return Some(IdentitySpec::new_by_default());
    }
    if s_upper.contains("SERIAL") && !s_upper.contains("VECTOR") {
        return Some(IdentitySpec::new_always());
    }
    if s_upper.contains("AUTOINCREMENT") || s_upper.contains("AUTOINCREMENTAL") || s_upper.contains("AUTO_INCREMENT") {
        return Some(IdentitySpec::new_always());
    }
    if s_upper.contains("IDENTITY") && !s_upper.contains("GENERATED") {
        return Some(IdentitySpec::new_always());
    }
    None
}

fn parse_single_col(s: &str) -> Result<ColumnDef, String> {
    let s_trim = s.trim();
    if s_trim.is_empty() { return Err("Empty column definition".into()); }
    let s_upper = s_trim.to_uppercase();
    let identity = detect_identity(&s_upper);
    let is_pk = s_upper.contains("PRIMARY KEY") || s_upper.contains("CLAVE PRIMARIA");
    let mut parts = s_trim.split_whitespace();
    let name = parts.next().ok_or("Missing column name")?.to_string();
    let mut type_raw = parts.next().ok_or(format!("Missing type for column '{}'", name))?.to_string();
    let type_up = type_raw.to_uppercase();
    if type_up == "SERIAL" || type_up == "BIGSERIAL" || type_up == "AUTOINCREMENT" || type_up == "AUTOINCREMENTAL" || type_up == "AUTO_INCREMENT" {
        type_raw = "INT".to_string();
    }
    if type_raw.to_uppercase() == "VECTOR" && s_upper.contains("VECTOR(") {
        if let Some(start) = s_upper.find("VECTOR(") {
            if let Some(end) = s_trim[start..].find(')') { type_raw = s_trim[start..start+end+1].to_string(); }
        }
    }
    let base_type_str = if type_raw.to_uppercase().starts_with("VECTOR") {
        if let Some(p) = s_upper.find("VECTOR") {
            let substr = &s_trim[p..];
            if let Some(end) = substr.find(')') { substr[..end+1].to_string() } else { "VECTOR".into() }
        } else { type_raw.clone() }
    } else {
        type_raw.clone()
    };
    let dt = DataType::parse(&base_type_str).unwrap_or(DataType::Text);
    let mut col = ColumnDef::new(&name, dt);
    col.is_primary_key = is_pk;
    if let Some(id_spec) = identity {
        col.identity = Some(id_spec);
        col.is_primary_key = true;
        col.nullable = false;
    }
    if is_pk && col.identity.is_none() && (s_upper.contains("AUTOINCREMENT") || s_upper.contains("AUTO_INCREMENT")) {
        col.identity = Some(IdentitySpec::new_always());
        col.nullable = false;
    }
    if is_pk { col.is_primary_key = true; }
    Ok(col)
}

pub fn parse(sql: &str) -> Result<LogicalPlan, String> {
    let trimmed = trim_semicolon(sql);
    if trimmed.is_empty() { return Err("Empty query".into()); }
    let upper = trimmed.to_uppercase();

    // SHOW TABLES / MUESTRA TABLAS - debe ir antes que DESCRIBE (para distinguir TABLA vs TABLAS)
    if is_show_tables(&upper) {
        return Ok(LogicalPlan::ShowTables);
    }
    // SHOW DATABASES / MUESTRA BASES
    if is_show_databases(&upper) {
        return Ok(LogicalPlan::ShowDatabases);
    }
    // CREATE DATABASE / CREA BASE
    if let Some(name) = strip_create_database(trimmed) {
        return Ok(LogicalPlan::CreateDatabase { name });
    }
    // DROP DATABASE / BORRA BASE
    if let Some(name) = strip_drop_database(trimmed) {
        return Ok(LogicalPlan::DropDatabase { name });
    }
    // USE DATABASE / USA BASE
    if let Some(name) = strip_use_database(trimmed) {
        return Ok(LogicalPlan::UseDatabase { name });
    }

    // Seguridad empresarial (antes de DESCRIBE: SHOW USERS/GROUPS/GRANTS no es DESCRIBE TABLE)
    if let Some(res) = parse_security(trimmed, &upper) {
        return res;
    }

    // DESCRIBE / MUESTRA / ESTRUCTURA / ESQUEMA / SHOW - comando para ver estructura (ES) - debe ir primero
    let describe_prefixes = ["DESCRIBE", "SHOW", "MUESTRA", "MOSTRAR", "ESTRUCTURA", "ESQUEMA"];
    let up_trim = upper.trim_start();
    for pref in &describe_prefixes {
        if up_trim.starts_with(pref) {
            // Intentar parsear como DESCRIBE, si falla y era realmente DESCRIBE, propagar error; si no, continuar
            match parse_describe(trimmed) {
                Ok(plan) => return Ok(plan),
                Err(e) => {
                    // Si el input realmente empezaba con un prefijo de describe, es un error de describe, no intentar otros parsers
                    // Verificar que el error no sea por falta de tabla genérica: si up_trim es exactamente DESCRIBE etc, es describe
                    if describe_prefixes.iter().any(|p| up_trim.starts_with(p)) {
                        // Si es DESCRIBE/SHOW etc y falló, retornar error directamente para dar feedback claro
                        // Pero solo si el fallo fue por parse_describe (falta tabla), sino dejar que otros parsers intenten
                        // Distinguir: si el sql es "DESCRIBE TABLE x" -> es describe, si es "DESCRIBE" solo -> error describe
                        // Para evitar falsos positivos con tablas llamadas "mostrar", solo si el primer token es exactamente el prefijo
                        let first_token = up_trim.split_whitespace().next().unwrap_or("");
                        if describe_prefixes.contains(&first_token) || up_trim.starts_with("DESCRIBE TABLE") || up_trim.starts_with("DESCRIBE TABLA") || up_trim.starts_with("SHOW CREATE TABLE") || up_trim.starts_with("SHOW TABLE") || up_trim.starts_with("MUESTRA ESTRUCTURA") || up_trim.starts_with("MOSTRAR ESTRUCTURA") || up_trim.starts_with("ESTRUCTURA DE") || up_trim.starts_with("ESQUEMA DE") {
                            return Err(e);
                        }
                    }
                    break;
                }
            }
        }
    }

    // Orden importante: los más largos primero para evitar prefijo ambiguo BUSCA vs BUSCAR, BORRA TABLA vs BORRAR
    // SEARCH/BUSCA
    if upper.starts_with("SEARCH") || upper.starts_with("BUSCA") {
        return parse_search(trimmed);
    }
    // CREATE/CREA
    if strip_prefix_any(trimmed, &["CREATE TABLE", "CREA TABLA", "CREA TABLA", "CREATE TABLA"]).is_some() {
        return parse_create(trimmed);
    }
    if upper.starts_with("CREATE") || upper.starts_with("CREA") {
        // fallback for CREATE without TABLE? still try
        if upper.contains("TABLE") || upper.contains("TABLA") {
            return parse_create(trimmed);
        }
    }
    // ALTER / CAMBIA
    if upper.starts_with("ALTER TABLE") || upper.starts_with("CAMBIA TABLA") || upper.starts_with("ALTERA TABLA") || upper.starts_with("CAMBIA") {
        // Verificar que sea realmente ALTER
        if upper.contains("TABLE") || upper.contains("TABLA") {
            return parse_alter(trimmed);
        }
    }
    // DROP / BORRA
    if upper.starts_with("DROP TABLE") || upper.starts_with("BORRA TABLA") || upper.starts_with("ELIMINA TABLA") {
        return parse_drop(trimmed);
    }
    // INSERT / AGREGAR
    if upper.starts_with("INSERT") || upper.starts_with("AGREGAR") || upper.starts_with("AGREGA") || upper.starts_with("INSERTAR") {
        return parse_insert(trimmed);
    }
    // DELETE / BORRAR (pero no BORRA TABLA que ya se capturó)
    if upper.starts_with("DELETE") || (upper.starts_with("BORRAR") && !upper.starts_with("BORRA TABLA") && !upper.starts_with("BORRAR TABLA")) {
        return parse_delete(trimmed);
    }
    // UPDATE / ACTUALIZA
    if upper.starts_with("UPDATE") || upper.starts_with("ACTUALIZA") || upper.starts_with("ACTUALIZAR") {
        return parse_update(trimmed);
    }
    // SELECT / ELIGE
    if upper.starts_with("SELECT") || upper.starts_with("ELIGE") || upper.starts_with("ELIGE") || upper.starts_with("SELECCIONA") {
        return parse_select(trimmed, false);
    }
    Err(format!("Comando no soportado: {}", trimmed.split_whitespace().next().unwrap_or("")))
}

/// Seguridad: parsea DDL de usuarios/grupos/grants (EN + ES). Devuelve None si no es comando de seguridad.
fn parse_security(sql: &str, upper: &str) -> Option<Result<LogicalPlan, String>> {
    let u = upper.trim();
    // SHOW USERS / GROUPS / GRANTS
    if matches!(u, "SHOW USERS" | "MUESTRA USUARIOS" | "MOSTRAR USUARIOS" | "LISTA USUARIOS" | "VER USUARIOS") {
        return Some(Ok(LogicalPlan::ShowUsers));
    }
    if matches!(u, "SHOW GROUPS" | "SHOW ROLES" | "MUESTRA GRUPOS" | "MUESTRA ROLES" | "MOSTRAR GRUPOS" | "LISTA GRUPOS") {
        return Some(Ok(LogicalPlan::ShowGroups));
    }
    if u == "SHOW GRANTS" || u == "MUESTRA PERMISOS" || u == "MOSTRAR PERMISOS" || u == "VER PERMISOS" {
        return Some(Ok(LogicalPlan::ShowGrants { grantee: None }));
    }
    if let Some(rest) = strip_prefix_ci(sql, "SHOW GRANTS FOR GROUP")
        .or_else(|| strip_prefix_ci(sql, "SHOW GRANTS FOR"))
        .or_else(|| strip_prefix_ci(sql, "MUESTRA PERMISOS DE GRUPO"))
        .or_else(|| strip_prefix_ci(sql, "MUESTRA PERMISOS DE"))
    {
        let name = extract_table_name(rest);
        if name.is_empty() {
            return Some(Err("SHOW GRANTS falta usuario/grupo".into()));
        }
        let is_group = u.contains("GROUP") || u.contains("GRUPO");
        let g = if is_group { Grantee::Group(name) } else { Grantee::User(name) };
        return Some(Ok(LogicalPlan::ShowGrants { grantee: Some(g) }));
    }
    // CREATE USER / CREA USUARIO
    if let Some(rest) = strip_prefix_any(sql, &["CREATE USER", "CREA USUARIO", "CREAR USUARIO", "CREATE USUARIO"]).map(|(r, _)| r) {
        return Some(parse_create_user(rest));
    }
    // DROP USER / BORRA USUARIO
    if let Some(rest) = strip_prefix_any(sql, &["DROP USER", "BORRA USUARIO", "ELIMINA USUARIO", "BORRAR USUARIO", "ELIMINAR USUARIO"]).map(|(r, _)| r) {
        let name = extract_table_name(rest);
        if name.is_empty() {
            return Some(Err("DROP USER falta nombre".into()));
        }
        return Some(Ok(LogicalPlan::DropUser { name }));
    }
    // SET PASSWORD / CAMBIA CONTRASEÑA
    if let Some(rest) = strip_prefix_any(sql, &["SET PASSWORD FOR", "CAMBIA CONTRASEÑA DE", "CAMBIA CONTRASENA DE", "CAMBIAR CONTRASEÑA DE"]).map(|(r, _)| r) {
        return Some(parse_set_password(rest));
    }
    // CREATE GROUP/ROLE / CREA GRUPO/ROL
    if let Some(rest) = strip_prefix_any(sql, &["CREATE GROUP", "CREATE ROLE", "CREA GRUPO", "CREA ROL", "CREAR GRUPO", "CREAR ROL"]).map(|(r, _)| r) {
        let name = extract_table_name(rest);
        if name.is_empty() {
            return Some(Err("CREATE GROUP falta nombre".into()));
        }
        return Some(Ok(LogicalPlan::CreateGroup { name, description: String::new() }));
    }
    // DROP GROUP/ROLE
    if let Some(rest) = strip_prefix_any(sql, &["DROP GROUP", "DROP ROLE", "BORRA GRUPO", "BORRA ROL", "ELIMINA GRUPO", "ELIMINA ROL"]).map(|(r, _)| r) {
        let name = extract_table_name(rest);
        if name.is_empty() {
            return Some(Err("DROP GROUP falta nombre".into()));
        }
        return Some(Ok(LogicalPlan::DropGroup { name }));
    }
    // ADD MEMBER: ADD USER x TO GROUP y / AGREGA USUARIO x A GRUPO y
    if let Some(rest) = strip_prefix_any(sql, &["ADD USER", "AGREGA USUARIO", "AGREGAR USUARIO"]).map(|(r, _)| r) {
        return Some(parse_add_member(rest));
    }
    if let Some(rest) = strip_prefix_any(sql, &["REMOVE USER", "REMOVE MEMBER", "QUITA USUARIO", "QUITAR USUARIO"]).map(|(r, _)| r) {
        return Some(parse_remove_member(rest));
    }
    // GRANT / OTORGA - REVOCA / REVOKE
    if u.starts_with("GRANT ") || u.starts_with("OTORGA ") || u.starts_with("OTORGAR ") {
        return Some(parse_grant(sql, false));
    }
    if u.starts_with("REVOKE ") || u.starts_with("REVOCA ") || u.starts_with("REVOCAR ") {
        return Some(parse_grant(sql, true));
    }
    None
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

fn parse_create_user(rest: &str) -> Result<LogicalPlan, String> {
    // Formas: `ana IDENTIFIED BY 'x'` / `ana IDENTIFICADO POR 'x'` / `ana PASSWORD 'x'` / `ana SUPERUSER`
    let up = rest.to_uppercase();
    let superuser = up.contains("SUPERUSER") || up.contains("SUPERUSUARIO") || up.contains("ADMINISTRADOR") || up.contains("ADMIN ");
    // Buscar password tras BY / POR / PASSWORD / CONTRASEÑA
    let mut password = String::new();
    for kw in ["IDENTIFIED BY", "IDENTIFICADO POR", "IDENTIFICADA POR", "PASSWORD", "CONTRASEÑA", "CONTRASENA", " POR "] {
        if let Some(idx) = up.find(kw) {
            let after = rest[idx + kw.len()..].trim();
            // password es primer token (con o sin comillas); resto puede traer SUPERUSER
            let tok = after.split_whitespace().next().unwrap_or("");
            // Si kw fue " POR " y tok es parte de "POR ..." sin password, ignorar
            if kw == " POR " && (tok.eq_ignore_ascii_case("SUPERUSER") || tok.is_empty()) {
                continue;
            }
            password = unquote(tok);
            break;
        }
    }
    // Nombre = primer token antes de keywords
    let first = rest.split_whitespace().next().unwrap_or("");
    let name = unquote(first);
    if name.is_empty() {
        return Err("CREATE USER falta nombre / CREA USUARIO falta nombre".into());
    }
    if password.is_empty() {
        return Err("CREATE USER requiere password: IDENTIFIED BY 'x' / IDENTIFICADO POR 'x'".into());
    }
    Ok(LogicalPlan::CreateUser { name, password, superuser })
}

fn parse_set_password(rest: &str) -> Result<LogicalPlan, String> {
    // `ana = 'x'` / `ana TO 'x'` / `ana A 'x'`
    let up = rest.to_uppercase();
    // nombre = primer token
    let name = unquote(rest.split_whitespace().next().unwrap_or(""));
    if name.is_empty() {
        return Err("SET PASSWORD falta usuario".into());
    }
    // password = último token entre comillas o tras =/TO/A
    let mut pwd = String::new();
    if let Some(eq) = rest.find('=') {
        pwd = unquote(&rest[eq + 1..]);
    } else {
        // buscar TO/A <pwd>
        let toks: Vec<&str> = rest.split_whitespace().collect();
        if toks.len() >= 3 {
            pwd = unquote(toks[toks.len() - 1]);
        }
    }
    let _ = up;
    if pwd.is_empty() || pwd.eq_ignore_ascii_case(&name) {
        return Err("SET PASSWORD falta nueva password".into());
    }
    Ok(LogicalPlan::SetPassword { name, new_password: pwd })
}

fn parse_add_member(rest: &str) -> Result<LogicalPlan, String> {
    // `ana TO GROUP ventas` / `ana A GRUPO ventas`
    let up = rest.to_uppercase();
    let sep = [" TO GROUP ", " TO ROLE ", " A GRUPO ", " A ROL ", " EN GRUPO ", " EN ROL "]
        .iter()
        .find_map(|s| up.find(s).map(|i| (i, *s)));
    let Some((idx, sep)) = sep else {
        return Err("Sintaxis: ADD USER <usuario> TO GROUP <grupo> / AGREGA USUARIO <u> A GRUPO <g>".into());
    };
    let user = unquote(&rest[..idx]);
    let group = unquote(&rest[idx + sep.len()..].split_whitespace().next().unwrap_or(""));
    if user.is_empty() || group.is_empty() {
        return Err("ADD USER requiere usuario y grupo".into());
    }
    Ok(LogicalPlan::AddMember { user, group })
}

fn parse_remove_member(rest: &str) -> Result<LogicalPlan, String> {
    // `ana FROM GROUP ventas` / `ana DE GRUPO ventas`
    let up = rest.to_uppercase();
    let sep = [" FROM GROUP ", " FROM ROLE ", " DE GRUPO ", " DE ROL ", " DEL GRUPO "]
        .iter()
        .find_map(|s| up.find(s).map(|i| (i, *s)));
    let Some((idx, sep)) = sep else {
        return Err("Sintaxis: REMOVE USER <u> FROM GROUP <g> / QUITA USUARIO <u> DE GRUPO <g>".into());
    };
    let user = unquote(&rest[..idx]);
    let group = unquote(&rest[idx + sep.len()..].split_whitespace().next().unwrap_or(""));
    if user.is_empty() || group.is_empty() {
        return Err("REMOVE USER requiere usuario y grupo".into());
    }
    Ok(LogicalPlan::RemoveMember { user, group })
}

fn parse_grant(sql: &str, is_revoke: bool) -> Result<LogicalPlan, String> {
    // GRANT SELECT ON ventas.t TO USER ana | OTORGA SELECT EN ventas.t A USUARIO ana
    // GRANT SELECT ON ventas.* TO GROUP vendedores | GRANT ADMIN TO USER root
    let up = sql.to_uppercase();
    let head_len = if !is_revoke {
        if up.starts_with("GRANT ") { 6 } else if up.starts_with("OTORGAR ") { 8 } else { 7 }
    } else if up.starts_with("REVOKE ") { 7 } else if up.starts_with("REVOCAR ") { 8 } else { 7 };
    let body = sql[head_len..].trim();
    let body_up = body.to_uppercase();
    // Separar "<PRIV> ON <scope> TO <grantee>" — ON/EN es opcional para GLOBAL (ADMIN/CREATE_USER).
    let (priv_part, after_on) = if let Some(i) = find_kw(body_up.as_str(), " ON ") {
        (body[..i].trim(), body[i + 4..].trim())
    } else if let Some(i) = find_kw(body_up.as_str(), " EN ") {
        (body[..i].trim(), body[i + 4..].trim())
    } else {
        // Sin ON: "<PRIV> TO ..."  (global)
        if let Some(i) = find_kw(body_up.as_str(), " TO ") {
            (body[..i].trim(), body[i + 4..].trim())
        } else if let Some(i) = find_kw(body_up.as_str(), " A ") {
            (body[..i].trim(), body[i + 3..].trim())
        } else {
            return Err("Sintaxis: GRANT <PRIV> [ON base[.tabla]] TO USER|GROUP <nombre>".into());
        }
    };
    let privilege = Privilege::parse(priv_part)
        .ok_or_else(|| format!("Privilegio desconocido '{}'. Válidos: SELECT/INSERT/UPDATE/DELETE/SEARCH/CREATE_TABLE/ALTER_TABLE/DROP_TABLE/SHOW_TABLES/DESCRIBE_TABLE/CREATE_DATABASE/DROP_DATABASE/SHOW_DATABASES/USE_DATABASE/CREATE_USER/DROP_USER/GRANT/ADMIN", priv_part))?;
    // after_on = "<scope> TO <grantee>" o directo "<TO ...>" si era global
    let (scope_part, grantee_part) = if let Some(i) = find_kw(after_on.to_uppercase().as_str(), " TO ") {
        (after_on[..i].trim(), after_on[i + 4..].trim())
    } else if let Some(i) = find_kw(after_on.to_uppercase().as_str(), " A ") {
        // Evitar confundir "A" dentro de nombres: exigir " A " con espacios; si scope vacío era global.
        let left = after_on[..i].trim();
        let right = after_on[i + 3..].trim();
        if left.eq_ignore_ascii_case("USER") || left.eq_ignore_ascii_case("GROUP") || left.eq_ignore_ascii_case("USUARIO") || left.eq_ignore_ascii_case("GRUPO") {
            ("", after_on.trim())
        } else {
            (left, right)
        }
    } else {
        ("", after_on.trim())
    };
    let (db, table) = if scope_part.is_empty() || scope_part == "*" {
        (None, None)
    } else if scope_part.ends_with(".*") {
        (Some(scope_part[..scope_part.len() - 2].to_string()), None)
    } else if let Some(dot) = scope_part.find('.') {
        let d = scope_part[..dot].trim();
        let t = scope_part[dot + 1..].trim().trim_matches('*');
        (Some(d.to_string()), if t.is_empty() || t == "*" { None } else { Some(t.to_string()) })
    } else {
        (Some(scope_part.to_string()), None)
    };
    // grantee: [USER|GROUP|USUARIO|GRUPO] <nombre>
    let gup = grantee_part.to_uppercase();
    let (is_group, name) = if gup.starts_with("GROUP ") {
        (true, unquote(&grantee_part[6..]))
    } else if gup.starts_with("GRUPO ") {
        (true, unquote(&grantee_part[6..]))
    } else if gup.starts_with("ROLE ") || gup.starts_with("ROL ") {
        (true, unquote(&grantee_part[5..]))
    } else if gup.starts_with("USER ") {
        (false, unquote(&grantee_part[5..]))
    } else if gup.starts_with("USUARIO ") {
        (false, unquote(&grantee_part[8..]))
    } else {
        // Por defecto usuario.
        (false, unquote(grantee_part.split_whitespace().next().unwrap_or("")))
    };
    if name.is_empty() {
        return Err("GRANT/REVOKE requiere destinatario: TO USER|GROUP <nombre>".into());
    }
    let grantee = if is_group { Grantee::Group(name) } else { Grantee::User(name) };
    if is_revoke {
        Ok(LogicalPlan::Revoke { privilege, grantee, db, table })
    } else {
        Ok(LogicalPlan::Grant { privilege, grantee, db, table })
    }
}

fn find_kw(hay_up: &str, needle_spaced: &str) -> Option<usize> {
    hay_up.find(needle_spaced)
}

fn strip_create_table_prefix<'a>(sql: &'a str) -> Option<&'a str> {
    for prefix in &["CREATE TABLE", "CREA TABLA", "CREA TABLA", "CREATE TABLA"] {
        if let Some(rem) = strip_prefix_ci(sql, prefix) { return Some(rem); }
    }
    None
}
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let up = s.trim_start().to_uppercase();
    let pu = prefix.to_uppercase();
    if up.starts_with(&pu) {
        // longitud en chars ASCII segura
        Some(s.trim_start()[prefix.len()..].trim_start())
    } else { None }
}

fn parse_create(sql: &str) -> Result<LogicalPlan, String> {
    let rest = strip_create_table_prefix(sql).ok_or("CREATE TABLE missing prefix")?;
    let paren_start = rest.find('(').ok_or("CREATE TABLE falta '(' / CREA TABLA falta '('")?;
    let table_name = rest[..paren_start].trim().trim_matches(|c| c == '"' || c == '\'' ).to_string();
    if table_name.is_empty() || table_name.contains(' ') { return Err("Nombre de tabla inválido en CREATE TABLE / CREA TABLA".into()); }
    let mut depth = 0;
    let mut end = None;
    for (i, c) in rest[paren_start..].char_indices() {
        if c == '(' { depth += 1; }
        if c == ')' { depth -= 1; if depth == 0 { end = Some(paren_start + i); break; } }
    }
    let end = end.ok_or("CREATE TABLE falta ')'")?;
    let cols_str = &rest[paren_start+1..end];
    let cols = parse_column_defs(cols_str)?;
    Ok(LogicalPlan::CreateTable { table: table_name, columns: cols })
}

fn parse_alter(sql: &str) -> Result<LogicalPlan, String> {
    // Soporta ALTER TABLE / CAMBIA TABLA / ALTERA TABLA
    let rest = if let Some(r) = strip_prefix_ci(sql, "ALTER TABLE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "CAMBIA TABLA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ALTERA TABLA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "CAMBIA") { r } // fallback sin TABLA? pero necesita tabla
        else { return Err("ALTER TABLE / CAMBIA TABLA falta prefijo".into()); };
    let rest = rest.trim();
    let mut tokens = rest.split_whitespace();
    let table = tokens.next().ok_or("ALTER TABLE falta nombre de tabla / CAMBIA TABLA falta nombre")?.to_string();
    let op_raw = tokens.next().ok_or("ALTER TABLE falta ADD/DROP / CAMBIA TABLA falta AGREGA/BORRA")?.to_uppercase();
    // Normalizar op bilingüe
    let op = match op_raw.as_str() {
        "ADD" | "AGREGA" | "AGREGAR" | "AÑADE" | "ANADDE" | "AÑADIR" => "ADD",
        "DROP" | "BORRA" | "BORRAR" | "ELIMINA" | "ELIMINAR" | "QUITA" | "QUITAR" => "DROP",
        _ => return Err(format!("ALTER TABLE op desconocida '{}', esperado ADD/AGREGA o DROP/BORRA", op_raw)),
    };
    match op {
        "ADD" => {
            let next = tokens.next().ok_or("ADD falta columna / AGREGA falta columna")?.to_string();
            let (col_name, col_type) = if next.to_uppercase() == "COLUMN" || next.to_uppercase() == "COLUMNA" {
                let cname = tokens.next().ok_or("ADD COLUMN falta nombre / AGREGA COLUMNA falta nombre")?.to_string();
                let ctype = tokens.next().ok_or("ADD COLUMN falta tipo / AGREGA COLUMNA falta tipo")?.to_string();
                let ctype_full = normalize_vector_type(&ctype, sql);
                (cname, ctype_full)
            } else {
                let ctype = tokens.next().ok_or("ADD falta tipo / AGREGA falta tipo")?.to_string();
                let ctype_full = normalize_vector_type(&ctype, sql);
                (next, ctype_full)
            };
            Ok(LogicalPlan::AlterTableAddColumn { table, column: col_name, col_type })
        }
        "DROP" => {
            let next = tokens.next().ok_or("DROP falta columna / BORRA falta columna")?.to_string();
            let col_name = if next.to_uppercase() == "COLUMN" || next.to_uppercase() == "COLUMNA" {
                tokens.next().ok_or("DROP COLUMN falta nombre / BORRA COLUMNA falta nombre")?.to_string()
            } else { next };
            Ok(LogicalPlan::AlterTableDropColumn { table, column: col_name })
        }
        _ => unreachable!(),
    }
}

fn normalize_vector_type(ctype: &str, sql: &str) -> String {
    if ctype.to_uppercase() == "VECTOR" && sql.to_uppercase().contains("VECTOR(") {
        let up = sql.to_uppercase();
        if let Some(p) = up.find("VECTOR(") {
            let substr = &sql[p..];
            if let Some(e) = substr.find(')') { return substr[..e+1].to_string(); }
        }
    }
    ctype.to_string()
}

fn parse_drop(sql: &str) -> Result<LogicalPlan, String> {
    let rest = if let Some(r) = strip_prefix_ci(sql, "DROP TABLE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BORRA TABLA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ELIMINA TABLA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BORRA") { r } // fallback
        else { return Err("DROP TABLE / BORRA TABLA falta prefijo".into()); };
    let mut rest = rest.trim();
    let rest_upper = rest.to_uppercase();
    if rest_upper.starts_with("IF EXISTS") || rest_upper.starts_with("SI EXISTE") {
        // saltar IF EXISTS / SI EXISTE
        if rest_upper.starts_with("IF EXISTS") { rest = rest["IF EXISTS".len()..].trim(); }
        else { rest = rest["SI EXISTE".len()..].trim(); }
    }
    let table = extract_table_name(rest);
    if table.is_empty() { return Err("DROP TABLE falta nombre / BORRA TABLA falta nombre".into()); }
    Ok(LogicalPlan::DropTable { table })
}

fn parse_insert(sql: &str) -> Result<LogicalPlan, String> {
    // Soporta:
    // INSERT INTO t (a,b) VALUES (1,'x')
    // AGREGAR EN t (a,b) VALORES (1,'x')
    // AGREGAR t (a,b) VALORES ...
    // INSERT t VALUES ...
    let up = sql.to_uppercase();
    // Determinar prefijo
    let rest = if let Some(r) = strip_prefix_ci(sql, "INSERT INTO") { r }
        else if let Some(r) = strip_prefix_ci(sql, "AGREGAR EN") { r }
        else if let Some(r) = strip_prefix_ci(sql, "AGREGAR") { r }
        else if let Some(r) = strip_prefix_ci(sql, "INSERTAR EN") { r }
        else if let Some(r) = strip_prefix_ci(sql, "INSERTAR") { r }
        else if let Some(r) = strip_prefix_ci(sql, "INSERT") { r }
        else if let Some(r) = strip_prefix_ci(sql, "AGREGA EN") { r }
        else if let Some(r) = strip_prefix_ci(sql, "AGREGA") { r }
        else { return Err("INSERT / AGREGAR falta prefijo".into()); };
    let rest = rest.trim();
    // Extraer tabla: hasta '(' o 'VALUES'/'VALORES' o espacio
    // Tabla es primera palabra
    let table_end = rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len());
    let table = rest[..table_end].trim().to_string();
    if table.is_empty() { return Err("INSERT falta tabla / AGREGAR falta tabla".into()); }
    let mut remaining = rest[table_end..].trim();
    // Si remaining empieza con '(' => lista de columnas
    let mut columns: Vec<String> = Vec::new();
    if remaining.starts_with('(') {
        let mut depth = 0;
        let mut end = None;
        for (i,c) in remaining.char_indices() {
            if c == '(' { depth += 1; }
            if c == ')' { depth -= 1; if depth == 0 { end = Some(i); break; } }
        }
        let end = end.ok_or("INSERT falta ')' en columnas / AGREGAR falta ')'")?;
        let cols_str = &remaining[1..end];
        columns = cols_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        remaining = remaining[end+1..].trim();
    }
    // Ahora esperar VALUES / VALORES
    let rem_up = remaining.to_uppercase();
    if rem_up.starts_with("VALUES") {
        remaining = remaining["VALUES".len()..].trim();
    } else if rem_up.starts_with("VALORES") {
        remaining = remaining["VALORES".len()..].trim();
    } else if rem_up.starts_with("VALUES") || rem_up.starts_with("VALORES") {
        // ya manejado
    } else {
        // Si no hay VALUES, puede ser solo columnas? error
        if !remaining.starts_with('(') {
            return Err("INSERT falta VALUES / AGREGAR falta VALORES".into());
        }
    }
    // remaining debe ser '(' ... ')' con valores
    if !remaining.starts_with('(') { return Err("INSERT falta '(' en valores / AGREGAR falta '('".into()); }
    let mut depth = 0;
    let mut end = None;
    for (i,c) in remaining.char_indices() {
        if c == '(' { depth += 1; }
        if c == ')' { depth -= 1; if depth == 0 { end = Some(i); break; } }
    }
    let end = end.ok_or("INSERT falta ')' en valores")?;
    let vals_str = &remaining[1..end];
    let values = split_values(vals_str)?;
    // Si columns vacío, inferir que son todas las columnas en orden del catálogo (se resolverá en executor)
    Ok(LogicalPlan::Insert { table, columns, values })
}

fn split_values(s: &str) -> Result<Vec<String>, String> {
    let mut res = Vec::new();
    let mut depth = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '\'' && !in_double { in_single = !in_single; }
        if c == '"' && !in_single { in_double = !in_double; }
        if !in_single && !in_double {
            if c == '(' { depth += 1; }
            if c == ')' { depth -= 1; }
            if c == ',' && depth == 0 {
                res.push(s[start..i].trim().to_string());
                start = i + c.len_utf8();
            }
        }
    }
    res.push(s[start..].trim().to_string());
    Ok(res.into_iter().filter(|x| !x.is_empty()).collect())
}

fn parse_delete(sql: &str) -> Result<LogicalPlan, String> {
    // DELETE FROM t WHERE ...  /  BORRAR DE t DONDE ...  / BORRAR EN t DONDE ...
    let rest = if let Some(r) = strip_prefix_ci(sql, "DELETE FROM") { r }
        else if let Some(r) = strip_prefix_ci(sql, "DELETE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BORRAR DE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BORRAR EN") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BORRAR") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ELIMINAR DE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ELIMINAR") { r }
        else { return Err("DELETE / BORRAR falta prefijo".into()); };
    let rest = rest.trim();
    let rest_up = rest.to_uppercase();
    if let Some(idx) = rest_up.find("WHERE") {
        let tbl = rest[..idx].trim().to_string();
        let wc = rest[idx+5..].trim().to_string();
        if tbl.is_empty() { return Err("DELETE falta tabla / BORRAR falta tabla".into()); }
        return Ok(LogicalPlan::Delete { table: tbl, where_clause: if wc.is_empty() { None } else { Some(wc) } });
    }
    if let Some(idx) = rest_up.find("DONDE") {
        let tbl = rest[..idx].trim().to_string();
        let wc = rest[idx+5..].trim().to_string();
        if tbl.is_empty() { return Err("DELETE falta tabla / BORRAR falta tabla".into()); }
        return Ok(LogicalPlan::Delete { table: tbl, where_clause: if wc.is_empty() { None } else { Some(wc) } });
    }
    if rest.is_empty() { return Err("DELETE falta tabla / BORRAR falta tabla".into()); }
    Ok(LogicalPlan::Delete { table: rest.to_string(), where_clause: None })
}

fn find_any_ci(hay: &str, needles: &[&str]) -> Option<usize> {
    let up = hay.to_uppercase();
    let mut best: Option<usize> = None;
    for n in needles {
        if let Some(pos) = up.find(&n.to_uppercase()) {
            if best.is_none() || pos < best.unwrap() { best = Some(pos); }
        }
    }
    best
}

fn parse_update(sql: &str) -> Result<LogicalPlan, String> {
    // UPDATE / ACTUALIZA
    let rest = if let Some(r) = strip_prefix_ci(sql, "UPDATE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ACTUALIZA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ACTUALIZAR") { r }
        else { return Err("UPDATE / ACTUALIZA falta prefijo".into()); };
    let rest = rest.trim();
    // Buscar SET / ESTABLECE / FIJA / COLOCA / ASIGNA
    let set_keywords = [" SET ", " ESTABLECE ", " FIJA ", " COLOCA ", " ASIGNA ", " SET", " ESTABLECE", " FIJA"];
    let mut set_pos: Option<(usize, &str)> = None;
    let up = format!(" {} ", rest.to_uppercase());
    for kw in &[" SET ", " ESTABLECE ", " FIJA ", " COLOCA ", " ASIGNA "] {
        if let Some(p) = up.find(kw) {
            if set_pos.is_none() || p < set_pos.unwrap().0 { set_pos = Some((p, kw.trim())); }
        }
    }
    // Fallback búsqueda sin espacios estrictos
    if set_pos.is_none() {
        for kw in &["SET", "ESTABLECE", "FIJA", "COLOCA", "ASIGNA"] {
            if let Some(p) = rest.to_uppercase().find(&format!(" {} ", kw)) {
                if set_pos.is_none() || p < set_pos.unwrap().0 { set_pos = Some((p, *kw)); }
            }
        }
    }
    let (set_idx, kw) = set_pos.ok_or("UPDATE falta SET / ACTUALIZA falta ESTABLECE/FIJA")?;
    // set_idx es en up con espacios, convertir a rest
    // Buscar realmente en rest: encontrar kw case-insensitive
    let rest_up = rest.to_uppercase();
    let kw_pos = rest_up.find(&kw.to_uppercase()).ok_or("SET no encontrado")?;
    let table = rest[..kw_pos].trim().to_string();
    if table.is_empty() { return Err("UPDATE falta tabla / ACTUALIZA falta tabla".into()); }
    let after_kw = &rest[kw_pos + kw.len()..].trim();
    // Separar WHERE / DONDE
    let where_keywords = [" WHERE ", " DONDE "];
    let mut where_pos: Option<usize> = None;
    let mut where_kw = "";
    let after_up = format!(" {} ", after_kw.to_uppercase());
    for kw in &where_keywords {
        if let Some(p) = after_up.find(kw) {
            if where_pos.is_none() || p < where_pos.unwrap() { where_pos = Some(p); where_kw = kw.trim(); }
        }
    }
    let (assign_str, where_opt) = if let Some(pos) = where_pos {
        // pos es en after_up con espacios, pero necesitamos en after_kw
        let real_pos = after_kw.to_uppercase().find(where_kw).unwrap();
        (&after_kw[..real_pos], Some(after_kw[real_pos + where_kw.len()..].trim().to_string()))
    } else {
        // Buscar WHERE/DONDE sin espacios estrictos
        let after_up2 = after_kw.to_uppercase();
        if let Some(p) = after_up2.find("WHERE") {
            (&after_kw[..p], Some(after_kw[p+5..].trim().to_string()))
        } else if let Some(p) = after_up2.find("DONDE") {
            (&after_kw[..p], Some(after_kw[p+5..].trim().to_string()))
        } else {
            (after_kw.trim(), None)
        }
    };
    let assignments = parse_assignments(assign_str)?;
    if assignments.is_empty() { return Err("UPDATE SET requiere asignaciones / ACTUALIZA requiere asignaciones".into()); }
    Ok(LogicalPlan::Update { table, assignments, where_clause: where_opt })
}

fn parse_assignments(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut res = Vec::new();
    let mut depth = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '\'' && !in_double { in_single = !in_single; }
        if c == '"' && !in_single { in_double = !in_double; }
        if !in_single && !in_double {
            if c == '(' { depth += 1; }
            if c == ')' { depth -= 1; }
            if c == ',' && depth == 0 {
                let part = s[start..i].trim();
                if !part.is_empty() { res.push(parse_one_assign(part)?); }
                start = i + c.len_utf8();
            }
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() { res.push(parse_one_assign(last)?); }
    Ok(res)
}

fn parse_one_assign(s: &str) -> Result<(String, String), String> {
    let eq = s.find('=').ok_or(format!("Asignación inválida '{}', falta '='", s))?;
    let col = s[..eq].trim().to_string();
    let val = s[eq+1..].trim().to_string();
    if col.is_empty() || val.is_empty() { return Err(format!("Asignación inválida '{}'", s)); }
    Ok((col, val))
}

fn parse_search(sql: &str) -> Result<LogicalPlan, String> {
    // SEARCH/BUSCA * IN/EN table WHERE/DONDE ... GROUP BY/AGRUPA POR ... ORDER BY/ORDENA POR ... LIMIT/LIMITE ...
    let rest = if let Some(r) = strip_prefix_ci(sql, "SEARCH") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BUSCA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "BUSCAR") { r }
        else { return Err("SEARCH/BUSCA falta prefijo".into()); };
    let mut rest = rest.trim();
    let rest_upper = rest.to_uppercase();
    if rest_upper.starts_with("*") { rest = rest[1..].trim(); }
    let rest_upper2 = rest.to_uppercase();
    if rest_upper2.starts_with("IN") { rest = rest[2..].trim(); }
    else if rest_upper2.starts_with("EN") { rest = rest[2..].trim(); }
    else if rest_upper2.starts_with("FROM") { rest = rest[4..].trim(); }
    else if rest_upper2.starts_with("DE") { rest = rest[2..].trim(); }
    // Ahora rest empieza con tabla
    let fake_select = format!("SELECT * FROM {}", rest);
    // Normalizar tabla para parse_select bilingüe: parse_select ya soporta FROM/DE etc pero espera SELECT
    // Creamos un fake con SELECT para reutilizar lógica, pero necesitamos que WHERE/DONDE etc sean bilingües
    // parse_select bilingüe ya maneja WHERE/DONDE etc, así que delegamos
    // Pero necesitamos construir fake que mantenga resto original (que puede contener DONDE, AGRUPA, ORDENA)
    // parse_select lo manejará porque es bilingüe
    let mut plan = parse_select(&fake_select, true)?;
    // Marcar como búsqueda para diferenciar si se quiere
    if let LogicalPlan::Select { ref mut is_search, .. } = plan { *is_search = true; }
    Ok(plan)
}

fn parse_select(sql: &str, is_search: bool) -> Result<LogicalPlan, String> {
    // Prefijo SELECT/ELIGE
    let rest = if let Some(r) = strip_prefix_ci(sql, "SELECT") { r }
        else if let Some(r) = strip_prefix_ci(sql, "ELIGE") { r }
        else if let Some(r) = strip_prefix_ci(sql, "SELECCIONA") { r }
        else if let Some(r) = strip_prefix_ci(sql, "SELECCIONAR") { r }
        else { return Err("SELECT/ELIGE falta prefijo".into()); };
    let rest = rest.trim();
    // Encontrar FROM/DE/DESDE/EN - DESDE antes que DE por substring
    let from_keywords = [" FROM ", " DESDE ", " DE ", " EN "];
    let mut from_pos: Option<(usize, &str)> = None;
    let rest_up = format!(" {} ", rest.to_uppercase());
    for kw in &from_keywords {
        if let Some(p) = rest_up.find(kw) {
            if from_pos.is_none() || p < from_pos.unwrap().0 { from_pos = Some((p, kw.trim())); }
        }
    }
    // Fallback sin espacios estrictos: buscar "FROM" al inicio - DESDE antes que DE
    if from_pos.is_none() {
        let up = rest.to_uppercase();
        for kw in &["FROM", "DESDE", "DE", "EN"] {
            if let Some(p) = up.find(&format!(" {} ", kw)) {
                if from_pos.is_none() || p < from_pos.unwrap().0 { from_pos = Some((p, kw)); }
            }
        }
    }
    let (proj_str, kw) = if let Some((pos, kw)) = from_pos {
        // pos es en rest_up con espacio inicial, ajustar
        // En rest, FROM está en pos (porque rest_up tiene espacio extra al inicio)
        // Buscar posición real en rest
        let real_pos = rest.to_uppercase().find(kw).ok_or("FROM/DE no encontrado")?;
        (rest[..real_pos].trim(), kw)
    } else {
        return Err("SELECT falta FROM/DE / ELIGE falta DE".into());
    };
    let kw_len = kw.len();
    let after_from_start = rest.to_uppercase().find(&kw.to_uppercase()).unwrap() + kw_len;
    let after_from = rest[after_from_start..].trim();
    let projection = parse_projection(proj_str)?;

    // after_from: "<table> WHERE/DONDE ... GROUP BY/AGRUPA POR ... ORDER BY/ORDENA POR ... LIMIT/LIMITE ..."
    let table_end = after_from.find(|c: char| c.is_whitespace()).unwrap_or(after_from.len());
    let table = after_from[..table_end].trim().to_string();
    if table.is_empty() { return Err("SELECT falta tabla / ELIGE falta tabla".into()); }
    let mut remaining = after_from[table_end..].trim();
    let mut remaining_upper = remaining.to_uppercase();

    let mut where_clause: Option<String> = None;
    let mut group_by: Option<String> = None;
    let mut order_by: Option<OrderBy> = None;
    let mut limit: Option<usize> = None;

    // WHERE / DONDE
    let where_kw = if remaining_upper.contains("WHERE") { Some("WHERE") } else if remaining_upper.contains("DONDE") { Some("DONDE") } else { None };
    if let Some(kw) = where_kw {
        let pos = remaining_upper.find(kw).unwrap();
        let after_where = &remaining[pos + kw.len()..].trim_start();
        // Buscar siguiente GROUP BY/AGRUPA POR, ORDER BY/ORDENA POR, LIMIT/LIMITE (LIMITE primero por substring)
        let next_group = find_any_ci(after_where, &["GROUP BY", "AGRUPA POR"]);
        let next_order = find_any_ci(after_where, &["ORDER BY", "ORDENA POR"]);
        let next_limit = find_any_ci(after_where, &["LIMITE", "LIMIT"]);
        let mut end = after_where.len();
        if let Some(p) = next_group { if p < end { end = p; } }
        if let Some(p) = next_order { if p < end { end = p; } }
        if let Some(p) = next_limit { if p < end { end = p; } }
        where_clause = Some(after_where[..end].trim().to_string());
        remaining = &remaining[pos + kw.len() + end..];
        remaining_upper = remaining.to_uppercase();
    }

    // GROUP BY / AGRUPA POR
    let group_kw = if remaining_upper.contains("GROUP BY") { Some("GROUP BY") } else if remaining_upper.contains("AGRUPA POR") { Some("AGRUPA POR") } else { None };
    if let Some(kw) = group_kw {
        let pos = remaining_upper.find(kw).unwrap();
        let after = &remaining[pos + kw.len()..].trim_start();
        let next_order = find_any_ci(after, &["ORDER BY", "ORDENA POR"]);
        let next_limit = find_any_ci(after, &["LIMITE", "LIMIT"]);
        let mut end = after.len();
        if let Some(p) = next_order { if p < end { end = p; } }
        if let Some(p) = next_limit { if p < end { end = p; } }
        group_by = Some(after[..end].trim().to_string());
        remaining = &remaining[pos + kw.len() + end..];
        remaining_upper = remaining.to_uppercase();
    }

    // ORDER BY / ORDENA POR
    let order_kw = if remaining_upper.contains("ORDENA POR") { Some("ORDENA POR") } else if remaining_upper.contains("ORDER BY") { Some("ORDER BY") } else { None };
    if let Some(kw) = order_kw {
        let pos = remaining_upper.find(kw).unwrap();
        let after = &remaining[pos + kw.len()..].trim_start();
        let next_limit = find_any_ci(after, &["LIMITE", "LIMIT"]);
        let mut end = after.len();
        if let Some(p) = next_limit { end = p; }
        let order_str = after[..end].trim();
        order_by = Some(parse_order_by(order_str)?);
        remaining = &remaining[pos + kw.len() + end..];
        remaining_upper = remaining.to_uppercase();
    }

    // LIMIT / LIMITE - LIMITE primero por substring de LIMIT
    let limit_kw = if remaining_upper.contains("LIMITE") { Some("LIMITE") } else if remaining_upper.contains("LIMIT") { Some("LIMIT") } else { None };
    if let Some(kw) = limit_kw {
        let pos = remaining_upper.find(kw).unwrap();
        let after = &remaining[pos + kw.len()..].trim_start();
        let num_str = after.split_whitespace().next().ok_or("LIMIT falta número / LIMITE falta número")?;
        let n: usize = num_str.parse().map_err(|_| format!("LIMIT inválido '{}' / LIMITE inválido '{}'", num_str, num_str))?;
        limit = Some(n);
    }

    Ok(LogicalPlan::Select { table, projection, where_clause, group_by, order_by, limit, is_search })
}

fn parse_projection(s: &str) -> Result<Projection, String> {
    let s = s.trim();
    if s == "*" { return Ok(Projection::Star); }
    let up = s.to_uppercase();
    // COUNT/CUENTA
    let count_star_en = "COUNT(*)";
    let count_star_es = "CUENTA(*)";
    if up == count_star_en || up == count_star_es { return Ok(Projection::CountStar); }
    if (up.starts_with("COUNT(") || up.starts_with("CUENTA(")) && up.ends_with(")") {
        let start = if up.starts_with("COUNT(") { 6 } else { 7 };
        let inner = s[start..s.len()-1].trim();
        if inner == "*" { return Ok(Projection::CountStar); }
        if inner.is_empty() { return Err("COUNT() falta columna / CUENTA() falta columna".into()); }
        return Ok(Projection::CountCol(inner.to_string()));
    }
    if up.contains("COUNT") || up.contains("CUENTA") {
        let parts: Vec<String> = s.split(',').map(|p| p.trim().to_string()).collect();
        if parts.iter().any(|p| p.to_uppercase() == "COUNT(*)" || p.to_uppercase() == "CUENTA(*)") && parts.len() == 2 {
            let col = parts.iter().find(|p| p.to_uppercase() != "COUNT(*)" && p.to_uppercase() != "CUENTA(*)").unwrap().clone();
            return Ok(Projection::CountCol(col));
        }
        return Ok(Projection::Columns(parts));
    }
    if s.contains(',') {
        let cols = s.split(',').map(|p| p.trim().to_string()).collect();
        return Ok(Projection::Columns(cols));
    }
    if !s.is_empty() { return Ok(Projection::Columns(vec![s.to_string()])); }
    Err("Proyección inválida".into())
}

fn parse_order_by(s: &str) -> Result<OrderBy, String> {
    let s = s.trim();
    if s.is_empty() { return Err("ORDER BY falta columna / ORDENA POR falta columna".into()); }
    let mut parts = s.split_whitespace();
    let col = parts.next().ok_or("ORDER BY falta columna")?.to_string();
    let dir_raw = parts.next().map(|d| d.to_uppercase()).unwrap_or("ASC".into());
    // Traducir ASC/DESC español: ASCENDENTE/DESCENDENTE?
    let asc = match dir_raw.as_str() {
        "ASC" | "ASCENDENTE" => true,
        "DESC" | "DESCENDENTE" | "DESCENDIENTE" => false,
        _ => return Err(format!("ORDER BY dirección inválida '{}', usa ASC/DESC / ORDENA POR usa ASC/DESC", dir_raw)),
    };
    if parts.next().is_some() { return Err("ORDER BY solo una columna en v1 / ORDENA POR solo una columna".into()); }
    Ok(OrderBy { column: col, asc })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_create_bilingue() {
        for sql in &[
            "CREATE TABLE productos (id INT PRIMARY KEY, nombre TEXT, embedding VECTOR(768))",
            "CREA TABLA productos (id INT PRIMARY KEY, nombre TEXT, embedding VECTOR(768))",
            "crea tabla productos (id INT, nombre TEXT)",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::CreateTable { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_alter_bilingue() {
        for sql in &[
            "ALTER TABLE productos ADD COLUMN precio FLOAT",
            "CAMBIA TABLA productos AGREGA COLUMNA precio FLOAT",
            "ALTERA TABLA productos AÑADE COLUMNA precio FLOAT",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::AlterTableAddColumn { .. }), "failed: {}", sql);
        }
        for sql in &[
            "ALTER TABLE productos DROP COLUMN precio",
            "CAMBIA TABLA productos BORRA COLUMNA precio",
            "CAMBIA TABLA productos ELIMINA COLUMNA precio",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::AlterTableDropColumn { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_drop_bilingue() {
        for sql in &["DROP TABLE productos", "BORRA TABLA productos", "ELIMINA TABLA productos"] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::DropTable { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_update_bilingue() {
        for sql in &[
            "UPDATE productos SET nombre = 'Zapatilla Pro', precio = 100 WHERE id = 1",
            "ACTUALIZA productos ESTABLECE nombre = 'Zapatilla Pro' DONDE id = 1",
            "ACTUALIZA productos FIJA nombre = 'x' DONDE id = 1",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Update { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_select_bilingue() {
        for sql in &[
            "SELECT * FROM productos WHERE categoria_id = 5 ORDER BY nombre ASC LIMIT 10",
            "ELIGE * DE productos DONDE categoria_id = 5 ORDENA POR nombre ASC LIMITE 10",
            "ELIGE * DESDE productos DONDE cat = 5 ORDENA POR nombre DESC LIMITE 5",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Select { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_select_group_bilingue() {
        for sql in &[
            "SELECT categoria_id, COUNT(*) FROM productos GROUP BY categoria_id ORDER BY COUNT(*) DESC LIMIT 5",
            "ELIGE categoria_id, CUENTA(*) DESDE productos AGRUPA POR categoria_id ORDENA POR CUENTA(*) DESC LIMITE 5",
            "ELIGE cat, CUENTA(*) DE t AGRUPA POR cat ORDENA POR CUENTA(*) DESC LIMITE 5",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Select { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_search_bilingue() {
        for sql in &[
            "SEARCH * IN productos WHERE categoria_id = 5 LIMIT 5",
            "BUSCA * EN productos DONDE categoria_id = 5 LIMITE 5",
            "BUSCA * DE productos DONDE cat = 5 LIMITE 5",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Select { is_search: true, .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_insert_bilingue() {
        for sql in &[
            "INSERT INTO productos (id, nombre) VALUES (1, 'Zapatilla')",
            "AGREGAR EN productos (id, nombre) VALORES (1, 'Zapatilla')",
            "AGREGAR productos (id, nombre) VALORES (1, 'Zapatilla')",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Insert { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_delete_bilingue() {
        for sql in &[
            "DELETE FROM productos WHERE id = 1",
            "BORRAR DE productos DONDE id = 1",
            "BORRAR EN productos DONDE id = 1",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::Delete { .. }), "failed: {}", sql);
        }
    }
    #[test]
    fn test_count_bilingue() {
        let p = parse("ELIGE CUENTA(*) DE productos DONDE cat = 5").unwrap();
        assert!(matches!(p, crate::LogicalPlan::Select { projection: Projection::CountStar, .. }));
        let p2 = parse("SELECT COUNT(*) FROM productos").unwrap();
        assert!(matches!(p2, crate::LogicalPlan::Select { projection: Projection::CountStar, .. }));
    }
    #[test]
    fn test_describe_bilingue() {
        for sql in &[
            "DESCRIBE TABLE productos",
            "DESCRIBE productos",
            "MUESTRA TABLA productos",
            "MUESTRA ESTRUCTURA DE TABLA productos",
            "ESTRUCTURA productos",
            "ESQUEMA productos",
            "MOSTRAR TABLA productos",
            "SHOW TABLE productos",
            "SHOW CREATE TABLE productos",
            "DESCRIBE TABLA productos",
            "ESTRUCTURA DE TABLA productos",
            "ESQUEMA DE TABLA productos",
        ] {
            let p = parse(sql).unwrap();
            assert!(matches!(p, crate::LogicalPlan::DescribeTable { table } if table=="productos"), "failed: {}", sql);
        }
    }
}

