//! crates/aether-server/src/main.rs - Demo CLI AETHER DB v2 BILINGÜE ES/EN
use argentum_engine::{Database, Row, Value};
use argentum_index::{SearchParams, TrinityIndex};
use argentum_storage::buffer_pool::BufferPool;
use argentum_storage::wal::WalManager;
use std::sync::Arc;

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self { Self(seed) }
    fn next_u32(&mut self) -> u32 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1); (self.0 >> 32) as u32 }
    fn next_f32(&mut self) -> f32 { (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0 }
}
fn hash_str(s: &str) -> u64 { let mut h: u64 = 14695981039346656037; for b in s.bytes(){ h ^= b as u64; h = h.wrapping_mul(1099511628211);} h }
fn embed(text: &str, dim: usize) -> Vec<f32> { let mut rng = Lcg::new(hash_str(text)); (0..dim).map(|_| rng.next_f32()).collect() }

fn print_help() {
    println!("Argentum DB v3.0 BILINGÜE - Motor TRINITY + SQL ES/EN + multi-base");
    println!();
    println!("USO:");
    println!("  cargo run -p argentum-server -- --demo        Demo TRINITY + Demo SQL EN + Demo SQL ES");
    println!("  cargo run -p argentum-server -- --demo-sql    Demo SQL completa (EN)");
    println!("  cargo run -p argentum-server -- --demo-es     Demo SQL en ESPAÑOL (traducción)");
    println!("  cargo run -p argentum-server -- --repl        REPL BILINGÜE ES/EN con multi-base");
    println!("  cargo run -p argentum-server -- --help");
    println!();
    println!("BASES DE DATOS / DATABASES (multi-base en REPL):");
    println!("  CREATE DATABASE / CREA BASE          Crear base");
    println!("  DROP DATABASE   / BORRA BASE         Borrar base");
    println!("  USE DATABASE    / USA BASE           Cambiar de base");
    println!("  SHOW DATABASES  / MUESTRA BASES      Listar bases (* = activa)");
    println!("  Nota: 'default' siempre existe y no se puede borrar.");
    println!();
    println!("TRADUCCIÓN DE COMANDOS (EN -> ES) - Costo: BAJO, parser bilingüe sin romper compatibilidad:");
    println!("  CREATE        -> CREA");
    println!("  DATABASE      -> BASE          (CREATE DATABASE -> CREA BASE / CREA BASE DE DATOS)");
    println!("  TABLE         -> TABLA        (CREATE TABLE -> CREA TABLA)");
    println!("  ALTER TABLE   -> CAMBIA TABLA (también ALTERA TABLA)");
    println!("  DROP TABLE    -> BORRA TABLA");
    println!("  INSERT        -> AGREGAR      (INSERT INTO t VALUES -> AGREGAR EN t VALORES)");
    println!("  UPDATE        -> ACTUALIZA    (UPDATE SET -> ACTUALIZA ESTABLECE/FIJA)");
    println!("  DELETE        -> BORRAR       (DELETE FROM t WHERE -> BORRAR DE t DONDE)");
    println!("  SELECT        -> ELIGE        (SELECT * FROM -> ELIGE * DE)");
    println!("  SEARCH        -> BUSCA        (SEARCH * IN -> BUSCA * EN)");
    println!("  ORDER BY      -> ORDENA POR");
    println!("  GROUP BY      -> AGRUPA POR");
    println!("  LIMIT         -> LIMITE");
    println!("  COUNT         -> CUENTA       (COUNT(*) -> CUENTA(*))");
    println!("  WHERE         -> DONDE");
    println!("  FROM          -> DE / DESDE / EN");
    println!("  SET           -> ESTABLECE / FIJA");
    println!("  ADD COLUMN    -> AGREGA COLUMNA");
    println!("  DROP COLUMN   -> BORRA COLUMNA");
    println!("  STATS         -> ESTADO       (REPL)");
    println!("  DESCRIBE      -> MUESTRA ESTRUCTURA / DESCRIBE / ESTRUCTURA / ESQUEMA");
    println!("                  Ej: MUESTRA TABLA t / DESCRIBE TABLA t / ESTRUCTURA t / MUESTRA ESTRUCTURA DE TABLA t");
    println!("  IDENTITY      -> IDENTIDAD / AUTOINCREMENTAL (GENERATED ALWAYS AS IDENTITY -> GENERADO SIEMPRE COMO IDENTIDAD)");
    println!("  SERIAL        -> SERIAL (alias, igual en ES)");
    println!("  AUTOINCREMENT -> AUTOINCREMENTAL");
    println!();
    println!("EJEMPLOS BILINGÜES:");
    println!("  EN: CREATE TABLE productos (id INT, nombre TEXT)");
    println!("  ES: CREA TABLA productos (id INT, nombre TEXT)");
    println!("  EN: CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, nombre TEXT)");
    println!("  ES: CREA TABLA t (id INT GENERADO SIEMPRE COMO IDENTIDAD PRIMARY KEY, nombre TEXT)");
    println!("  EN: CREATE TABLE t (id SERIAL PRIMARY KEY, nombre TEXT)  -- alias");
    println!("  ES: CREA TABLA t (id SERIAL PRIMARY KEY, nombre TEXT)");
    println!("  EN: INSERT INTO t (nombre) VALUES ('Zapatilla')  -- id autogenerado");
    println!("  ES: AGREGAR EN t (nombre) VALORES ('Zapatilla')");
    println!("  EN: SELECT * FROM productos WHERE cat=5 ORDER BY nombre ASC LIMIT 5");
    println!("  ES: ELIGE * DE productos DONDE cat=5 ORDENA POR nombre ASC LIMITE 5");
    println!("  EN: SELECT cat, COUNT(*) FROM t GROUP BY cat ORDER BY COUNT(*) DESC LIMIT 5");
    println!("  ES: ELIGE cat, CUENTA(*) DE t AGRUPA POR cat ORDENA POR CUENTA(*) DESC LIMITE 5");
    println!("  EN: SEARCH * IN productos WHERE cat=5 LIMIT 5");
    println!("  ES: BUSCA * EN productos DONDE cat=5 LIMITE 5");
    println!("  EN: UPDATE t SET nombre='x' WHERE id=1");
    println!("  ES: ACTUALIZA t ESTABLECE nombre='x' DONDE id=1");
    println!("  EN: INSERT INTO t (id, nombre) VALUES (1, 'Zapatilla')");
    println!("  ES: AGREGAR EN t (id, nombre) VALORES (1, 'Zapatilla')");
    println!("  EN: DELETE FROM t WHERE id=1");
    println!("  ES: BORRAR DE t DONDE id=1");
    println!("  EN: DESCRIBE TABLE t  /  SHOW TABLE t");
    println!("  ES: MUESTRA TABLA t  /  DESCRIBE TABLA t  /  ESTRUCTURA t  /  MUESTRA ESTRUCTURA DE TABLA t");
}

fn run_trinity_demo() -> std::io::Result<()> {
    println!("=== AETHER DB --demo TRINITY ===");
    let wal_path = std::env::temp_dir().join(format!("argentum_demo_{}.wal", std::process::id()));
    let _ = std::fs::remove_file(&wal_path);
    let wal = WalManager::open(wal_path.to_str().unwrap()).unwrap();
    let bp = Arc::new(BufferPool::new(128));
    let mut idx = TrinityIndex::new(bp.clone(), wal.clone());
    let productos = vec![
        (1, "Zapatilla Trail Pro", "Zapatilla impermeable Gore-Tex para trail running en lluvia intensa", 5),
        (2, "Zapatilla Urban Light", "Zapatilla ligera de cuero para ciudad, no impermeable", 5),
        (3, "Bota Montaña GTX", "Bota impermeable de montaña con membrana Gore-Tex", 5),
        (4, "Sandalia Verano", "Sandalia abierta transpirable para verano", 5),
        (5, "Campera Impermeable", "Campera impermeable con costuras selladas", 6),
        (6, "Mochila Trail 30L", "Mochila trail con funda impermeable incluida", 6),
        (7, "Reloj GPS Runner", "Reloj con GPS para running y trail", 7),
        (8, "Medias Técnicas", "Medias de compresión para trail", 7),
        (9, "Lentes Sol Sport", "Lentes para running en montaña", 7),
        (10, "Bastones Trekking", "Bastones de trekking plegables", 7),
    ];
    println!("-- INSERT 10 productos --");
    for (id, nombre, desc, cat) in &productos {
        let vector = embed(&format!("{} {}", nombre, desc), 768);
        let row = format!("{}|{}|cat={}", nombre, desc, cat);
        let csr = (*cat as u32).to_le_bytes().to_vec();
        let pq_dummy = &vector[..2].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()[..8];
        let mut payload = Vec::new();
        argentum_storage::page::TrinityPayload::serialize(row.as_bytes(), pq_dummy, desc.as_bytes(), &csr, &mut payload);
        let lsn = wal.append(argentum_storage::WalRecord::TrinityInsert{ txn_id: *id as u64, lsn: 0, page_id: 1, slot_id: 0, payload, prev_lsn: 0 }).unwrap();
        let slot = idx.insert(1, row.as_bytes(), &vector, desc.as_bytes(), &csr, *id as u64, lsn).unwrap();
        println!("  INSERT/AGREGAR id={} '{}' slot={} LSN={}", id, nombre, slot, lsn);
    }
    wal.group_commit().unwrap();
    println!("Stats/Estado: {} tuplas, {} páginas, WAL LSN={}\n", idx.num_tuples, idx.num_pages, wal.flushed_lsn());
    let qvec = embed("zapatilla para correr bajo lluvia intensa trail", 768);
    let params = SearchParams{ query_vector: Some(qvec), query_text: None, top_k: 5, ef_search: 64, alpha_bm25: 0.0, alpha_vector: 1.0, txn_snapshot: (0,1000,vec![]) };
    println!("-- SELECT/BUSCA vector Top5 --");
    for (i,r) in idx.search(&params).iter().enumerate(){
        let nombre = productos.get(r.slot_id as usize).map(|p| p.1).unwrap_or("?");
        println!(" {}. ELIGE/BUSCA slot={} -> {}", i+1, r.slot_id, nombre);
    }
    println!("\n=== TRINITY Demo OK WAL {} ({} bytes) ===\n", wal_path.display(), std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0));
    let _ = std::fs::remove_file(&wal_path);
    Ok(())
}

fn run_demo_sql() -> std::io::Result<()> {
    println!("=== AETHER DB --demo-sql EN (SQL Inglés) ===\n");
    let wal_path = std::env::temp_dir().join(format!("argentum_demo_sql_{}.wal", std::process::id()));
    let cat_path = std::env::temp_dir().join(format!("argentum_catalog_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&cat_path);
    let wal = WalManager::open(wal_path.to_str().unwrap()).unwrap();
    let bp = Arc::new(BufferPool::new(128));
    let trinity = TrinityIndex::new(bp.clone(), wal.clone());
    let db = Database::new(wal.clone(), bp.clone(), trinity, Some(cat_path.to_str().unwrap().into()));
    let exec = |sql: &str| {
        println!("> {}", sql);
        match argentum_engine::parser::parse(sql) {
            Ok(plan) => match db.execute(plan) { Ok(res) => println!("{}\n", res.to_display()), Err(e) => println!("Error: {}\n", e), },
            Err(e) => println!("Error parse: {}\n", e),
        }
    };
    exec("CREATE TABLE productos (id INT PRIMARY KEY, nombre TEXT, descripcion TEXT, categoria_id INT, embedding VECTOR(768))");
    exec("DESCRIBE TABLE productos");
    exec("MUESTRA ESTRUCTURA DE TABLA productos");
    println!("-- INSERT 6 filas --");
    for (id, nombre, desc, cat) in vec![(1, "Zapatilla Trail Pro", "impermeable gore-tex", 5),(2, "Zapatilla Urban Light", "cuero ciudad", 5),(3, "Bota Montaña GTX", "impermeable montaña", 5),(4, "Campera Impermeable", "costuras selladas", 6),(5, "Mochila Trail 30L", "funda impermeable", 6),(6, "Reloj GPS", "running trail", 7)] {
        let mut row = Row::new();
        row.insert("id".into(), Value::Int(id));
        row.insert("nombre".into(), Value::Text(nombre.into()));
        row.insert("descripcion".into(), Value::Text(desc.into()));
        row.insert("categoria_id".into(), Value::Int(cat));
        row.insert("embedding".into(), Value::Vector(embed(&format!("{} {}", nombre, desc), 768)));
        db.insert_row("productos", row).unwrap();
        println!("  INSERT id={} {}", id, nombre);
    }
    println!();
    exec("SELECT * FROM productos LIMIT 10");
    exec("SELECT * FROM productos WHERE categoria_id = 5 ORDER BY nombre ASC LIMIT 5");
    exec("SELECT categoria_id, COUNT(*) FROM productos GROUP BY categoria_id ORDER BY COUNT(*) DESC LIMIT 5");
    exec("SELECT COUNT(*) FROM productos WHERE categoria_id = 5");
    exec("UPDATE productos SET nombre = 'Zapatilla Pro v2', embedding = EMBED('zapatilla trail pro v2 impermeable') WHERE id = 1");
    exec("SELECT * FROM productos WHERE id = 1");
    exec("ALTER TABLE productos ADD COLUMN precio FLOAT");
    exec("UPDATE productos SET precio = 199.99 WHERE id = 1");
    exec("SELECT nombre, precio FROM productos ORDER BY precio DESC LIMIT 5");
    exec("ALTER TABLE productos DROP COLUMN precio");
    exec("SEARCH * IN productos WHERE categoria_id = 5 LIMIT 5");
    // 9b IDENTITY demo EN
    println!("-- Demo IDENTITY AUTOINCREMENTAL (EN) --");
    exec("CREATE TABLE productos_auto (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, nombre TEXT)");
    exec("DESCRIBE TABLE productos_auto");
    exec("INSERT INTO productos_auto (nombre) VALUES ('Zapatilla')");
    exec("INSERT INTO productos_auto (nombre) VALUES ('Bota')");
    exec("INSERT INTO productos_auto (id, nombre) VALUES (DEFAULT, 'Sandalia')");
    exec("SELECT * FROM productos_auto ORDER BY id ASC");
    exec("INSERT INTO productos_auto (id, nombre) VALUES (99, 'Falla siempre')"); // debe fallar ALWAYS
    exec("DROP TABLE productos_auto");
    // Alias SERIAL
    exec("CREATE TABLE t_serial (id SERIAL PRIMARY KEY, nombre TEXT)");
    exec("DESCRIBE TABLE t_serial");
    exec("INSERT INTO t_serial (nombre) VALUES ('a')");
    exec("INSERT INTO t_serial (nombre) VALUES ('b')");
    exec("SELECT * FROM t_serial ORDER BY id ASC");
    exec("DROP TABLE t_serial");
    exec("DROP TABLE productos");
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&cat_path);
    println!("=== Demo SQL EN OK ===\n");
    Ok(())
}

fn run_demo_es() -> std::io::Result<()> {
    println!("=== AETHER DB --demo-es ES (SQL Español 100% traducido) ===\n");
    let wal_path = std::env::temp_dir().join(format!("argentum_demo_es_{}.wal", std::process::id()));
    let cat_path = std::env::temp_dir().join(format!("argentum_catalog_es_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&cat_path);
    let wal = WalManager::open(wal_path.to_str().unwrap()).unwrap();
    let bp = Arc::new(BufferPool::new(128));
    let trinity = TrinityIndex::new(bp.clone(), wal.clone());
    let db = Database::new(wal.clone(), bp.clone(), trinity, Some(cat_path.to_str().unwrap().into()));
    let exec = |sql: &str| {
        println!("> {}", sql);
        match argentum_engine::parser::parse(sql) {
            Ok(plan) => match db.execute(plan) { Ok(res) => println!("{}\n", res.to_display()), Err(e) => println!("Error: {}\n", e), },
            Err(e) => println!("Error parse: {}\n", e),
        }
    };
    // Mismos tests pero en español
    exec("CREA TABLA productos (id INT PRIMARY KEY, nombre TEXT, descripcion TEXT, categoria_id INT, embedding VECTOR(768))");
    exec("MUESTRA TABLA productos");
    exec("ESTRUCTURA productos");
    exec("DESCRIBE TABLE productos");
    // AGREGAR EN ... VALORES
    exec("AGREGAR EN productos (id, nombre, descripcion, categoria_id) VALORES (1, 'Zapatilla Trail Pro', 'impermeable gore-tex', 5)");
    exec("AGREGAR EN productos (id, nombre, descripcion, categoria_id) VALORES (2, 'Zapatilla Urban Light', 'cuero ciudad', 5)");
    exec("AGREGAR EN productos (id, nombre, descripcion, categoria_id) VALORES (3, 'Bota Montaña GTX', 'impermeable montaña', 5)");
    exec("AGREGAR EN productos (id, nombre, descripcion, categoria_id) VALORES (4, 'Campera Impermeable', 'costuras selladas', 6)");
    // ELIGE
    exec("ELIGE * DE productos LIMITE 10");
    exec("ELIGE * DE productos DONDE categoria_id = 5 ORDENA POR nombre ASC LIMITE 5");
    exec("ELIGE categoria_id, CUENTA(*) DE productos AGRUPA POR categoria_id ORDENA POR CUENTA(*) DESC LIMITE 5");
    exec("ELIGE CUENTA(*) DE productos DONDE categoria_id = 5");
    exec("ELIGE CUENTA(categoria_id) DE productos");
    // ACTUALIZA
    exec("ACTUALIZA productos ESTABLECE nombre = 'Zapatilla Pro v2' DONDE id = 1");
    exec("ELIGE * DE productos DONDE id = 1");
    // CAMBIA TABLA
    exec("CAMBIA TABLA productos AGREGA COLUMNA precio FLOAT");
    exec("ACTUALIZA productos ESTABLECE precio = 199.99 DONDE id = 1");
    exec("ELIGE nombre, precio DE productos ORDENA POR precio DESC LIMITE 5");
    exec("CAMBIA TABLA productos BORRA COLUMNA precio");
    // Demo IDENTIDAD ES
    println!("-- Demo IDENTIDAD AUTOINCREMENTAL (ES) --");
    exec("CREA TABLA productos_auto (id INT GENERADO SIEMPRE COMO IDENTIDAD PRIMARY KEY, nombre TEXT)");
    exec("MUESTRA TABLA productos_auto");
    exec("AGREGAR EN productos_auto (nombre) VALORES ('Zapatilla')");
    exec("AGREGAR EN productos_auto (nombre) VALORES ('Bota')");
    exec("AGREGAR EN productos_auto (id, nombre) VALORES (DEFAULT, 'Sandalia')");
    exec("ELIGE * DE productos_auto ORDENA POR id ASC");
    exec("AGREGAR EN productos_auto (id, nombre) VALORES (99, 'Falla siempre')"); // debe fallar ALWAYS
    exec("BORRA TABLA productos_auto");
    exec("CREA TABLA t_serial (id SERIAL PRIMARY KEY, nombre TEXT)");
    exec("MUESTRA TABLA t_serial");
    exec("AGREGAR EN t_serial (nombre) VALORES ('a')");
    exec("AGREGAR EN t_serial (nombre) VALORES ('b')");
    exec("ELIGE * DE t_serial ORDENA POR id ASC");
    exec("BORRA TABLA t_serial");
    // BUSCA
    exec("BUSCA * EN productos DONDE categoria_id = 5 LIMITE 5");
    exec("BUSCA * EN productos LIMITE 3");
    // BORRAR
    exec("BORRAR DE productos DONDE id = 2");
    exec("ELIGE * DE productos LIMITE 10");
    exec("BORRA TABLA productos");
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&cat_path);
    println!("=== Demo SQL ES OK ===\n");
    Ok(())
}

/// Render prompt con usuario y base actual: argentum[user@db]>
fn repl_prompt(username: &str, current_db: &str) {
    use std::io::Write;
    print!("argentum[{}@{}]> ", username, current_db);
    let _ = std::io::stdout().flush();
}

/// Planes de seguridad: chequea privilegio y ejecuta contra AuthCatalog.
fn handle_secure_plan(
    mgr: &mut argentum_engine::DatabaseManager,
    session: &argentum_engine::SessionContext,
    plan: &argentum_engine::LogicalPlan,
) -> Option<String> {
    use argentum_engine::LogicalPlan;
    // Self-service: cambiar la propia password no exige CREATE_USER.
    if let LogicalPlan::SetPassword { name, new_password } = plan {
        if name.to_lowercase() == session.username.to_lowercase() {
            return Some(match mgr.auth_mut().set_password(name, new_password) {
                Ok(_) => "Password actualizado / password updated".into(),
                Err(e) => format!("Error: {}", e),
            });
        }
    }
    if let Err(e) = mgr.check_plan(session, plan) {
        return Some(format!("Error: {}", e));
    }
    match plan {
        LogicalPlan::CreateUser { name, password, superuser } => {
            Some(match mgr.auth_mut().create_user(name, password, *superuser) {
                Ok(_) => format!("Usuario '{}' creado / user created", name),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::DropUser { name } => Some(match mgr.auth_mut().drop_user(name) {
            Ok(_) => format!("Usuario '{}' borrado / user dropped", name),
            Err(e) => format!("Error: {}", e),
        }),
        LogicalPlan::SetPassword { name, new_password } => {
            Some(match mgr.auth_mut().set_password(name, new_password) {
                Ok(_) => "Password actualizado / password updated".into(),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::CreateGroup { name, description } => {
            Some(match mgr.auth_mut().create_group(name, description) {
                Ok(_) => format!("Grupo '{}' creado / group created", name),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::DropGroup { name } => Some(match mgr.auth_mut().drop_group(name) {
            Ok(_) => format!("Grupo '{}' borrado / group dropped", name),
            Err(e) => format!("Error: {}", e),
        }),
        LogicalPlan::AddMember { user, group } => {
            Some(match mgr.auth_mut().add_member(user, group) {
                Ok(_) => format!("'{}' agregado a '{}' / added", user, group),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::RemoveMember { user, group } => {
            Some(match mgr.auth_mut().remove_member(user, group) {
                Ok(_) => format!("'{}' quitado de '{}' / removed", user, group),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::Grant { privilege, grantee, db, table } => {
            Some(match mgr.auth_mut().grant(privilege.clone(), grantee.clone(), db.as_deref(), table.as_deref()) {
                Ok(_) => "GRANT OK / permiso otorgado".into(),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::Revoke { privilege, grantee, db, table } => {
            Some(match mgr.auth_mut().revoke(privilege.clone(), grantee.clone(), db.as_deref(), table.as_deref()) {
                Ok(_) => "REVOKE OK / permiso revocado".into(),
                Err(e) => format!("Error: {}", e),
            })
        }
        LogicalPlan::ShowUsers => {
            let mut s = String::from("Usuarios / Users:\n");
            for u in mgr.auth().list_users() {
                s.push_str(&format!(
                    "- {} (superuser: {}, disabled: {}, grupos: {})\n",
                    u.display,
                    if u.is_superuser { "sí" } else { "no" },
                    if u.disabled { "sí" } else { "no" },
                    if u.groups.is_empty() { "-".into() } else { u.groups.join(",") }
                ));
            }
            Some(s)
        }
        LogicalPlan::ShowGroups => {
            let mut s = String::from("Grupos / Groups:\n");
            for g in mgr.auth().list_groups() {
                s.push_str(&format!("- {} ({})\n", g.display, g.description));
            }
            Some(s)
        }
        LogicalPlan::ShowGrants { grantee } => {
            let mut s = String::from("Permisos / Grants:\n");
            let grants = match grantee {
                Some(g) => mgr.auth().grants_for(g),
                None => mgr.auth().all_grants(),
            };
            for gr in &grants {
                let who = match &gr.grantee {
                    argentum_common::auth::Grantee::User(n) => format!("USER {}", n),
                    argentum_common::auth::Grantee::Group(n) => format!("GROUP {}", n),
                };
                let scope = match (&gr.db, &gr.table) {
                    (Some(d), Some(t)) => format!(" ON {}.{}", d, t),
                    (Some(d), None) => format!(" ON {}.*", d),
                    _ => String::new(),
                };
                s.push_str(&format!("- GRANT {}{} TO {}\n", gr.privilege.as_str(), scope, who));
            }
            if grants.is_empty() {
                s.push_str("(sin grants)\n");
            }
            Some(s)
        }
        _ => None,
    }
}

fn handle_db_plan(
    mgr: &mut argentum_engine::DatabaseManager,
    session: &argentum_engine::SessionContext,
    plan: &argentum_engine::LogicalPlan,
) -> Option<String> {
    use argentum_engine::LogicalPlan;
    // Seguridad primero: planes de seguridad y de base exigen privilegio.
    if let Some(out) = handle_secure_plan(mgr, session, plan) {
        return Some(out);
    }
    match plan {
        LogicalPlan::CreateDatabase { name } => {
            if let Err(e) = mgr.check_plan(session, plan) {
                return Some(format!("Error: {}", e));
            }
            match mgr.create_database(name) {
                Ok(_) => {
                    // Auto-USE a la base recién creada (flujo MySQL-like).
                    let _ = mgr.use_database(name);
                    Some(format!("Base '{}' creada y en uso / database created and in use", name))
                }
                Err(e) => Some(format!("Error: {}", e)),
            }
        }
        LogicalPlan::DropDatabase { name } => {
            if let Err(e) = mgr.check_plan(session, plan) {
                return Some(format!("Error: {}", e));
            }
            match mgr.drop_database(name) {
                Ok(_) => Some(format!("Base '{}' borrada / database dropped", name)),
                Err(e) => Some(format!("Error: {}", e)),
            }
        }
        LogicalPlan::UseDatabase { name } => {
            if let Err(e) = mgr.check_plan(session, plan) {
                return Some(format!("Error: {}", e));
            }
            match mgr.use_database(name) {
                Ok(_) => Some(format!("Usando base '{}' / switched to database '{}'", name, name)),
                Err(e) => Some(format!("Error: {}", e)),
            }
        }
        LogicalPlan::ShowDatabases => {
            if let Err(e) = mgr.check_plan(session, plan) {
                return Some(format!("Error: {}", e));
            }
            let dbs = mgr.show_databases();
            let mut s = String::from("Bases de datos / Databases:\n");
            for d in &dbs {
                let marker = if d == mgr.current_db() { "* " } else { "  " };
                s.push_str(&format!("{}{}\n", marker, d));
            }
            s.push_str(&format!("(* = activa / current)\n({} total)", dbs.len()));
            Some(s)
        }
        LogicalPlan::ShowTables => {
            match mgr.execute_current(session, argentum_engine::LogicalPlan::ShowTables) {
                Ok(argentum_engine::ExecutionResult::Selected { columns, rows }) => {
                    let mut s = String::from("Tablas de la base activa / Tables in current database:\n");
                    // Header
                    s.push_str(&format!("{}\n", columns.join(" | ")));
                    s.push_str(&"-".repeat(s.len()));
                    s.push('\n');
                    for row in &rows {
                        let vals: Vec<String> = columns.iter().map(|c| {
                            row.get(c).map(|v| format!("{}", v)).unwrap_or_default()
                        }).collect();
                        s.push_str(&format!("{}\n", vals.join(" | ")));
                    }
                    Some(s)
                }
                Ok(other) => Some(format!("{:?}", other)),
                Err(e) => Some(format!("Error: {}", e)),
            }
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn run_repl() -> std::io::Result<()> {
    run_repl_with_dir(None)
}

fn run_repl_with_dir(data_dir: Option<&str>) -> std::io::Result<()> {
    run_repl_full(data_dir, None, None)
}

fn run_repl_full(data_dir: Option<&str>, user: Option<&str>, password: Option<&str>) -> std::io::Result<()> {
    use std::io::{self, Write};
    println!("Argentum DB REPL v4.0 BILINGÜE ES/EN + multi-base + seguridad");
    println!("HELP/AYUDA para ayuda, EXIT/SALIR para salir");
    println!("LOGIN <usuario> <password> para cambiar de usuario. Usuario inicial: root/root (cambiar en producción).");
    // Base del REPL: --data-dir <path> si se pasó, si no ./data/repl_<pid>/.
    // Con --data-dir los datos persisten entre invocaciones (útil para tests y
    // para usar Argentum como almacenamiento real entre sesiones).
    let base_dir = data_dir
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("./data/repl_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&base_dir);
    let mut mgr = argentum_engine::DatabaseManager::new(&base_dir);
    // Sesión: --user/--password si se pasaron, si no root bootstrap (compatibilidad).
    // En producción usar un usuario no-root y cambiar password de root.
    let mut session = match (user, password) {
        (Some(u), Some(p)) => match mgr.authenticate(u, p) {
            Ok(s) => {
                println!("Autenticado como '{}' / authenticated", s.username);
                s
            }
            Err(e) => {
                eprintln!("Login inicial falló ({}). Se usa root bootstrap solo para esta sesión.", e);
                argentum_engine::SessionContext::bootstrap_root()
            }
        },
        _ => {
            println!("AVISO: sesión bootstrap 'root'. Usá LOGIN <usuario> <password> y cambiá root/root en producción.");
            argentum_engine::SessionContext::bootstrap_root()
        }
    };
    // Crear tabla de ejemplo "productos" en la base activa si no existe.
    if let Some(db) = mgr.current() {
        let plan_sql = "CREATE TABLE productos (id INT PRIMARY KEY, nombre TEXT, descripcion TEXT, categoria_id INT, embedding VECTOR(768))";
        if let Ok(plan) = argentum_engine::parser::parse(plan_sql) {
            let _ = db.execute(plan);
        }
    }
    let bp_legacy = Arc::new(BufferPool::new(128));
    let wal_legacy = WalManager::open(std::env::temp_dir().join("argentum_repl_legacy.wal").to_str().unwrap()).unwrap();
    let mut trinity_legacy = TrinityIndex::new(bp_legacy, wal_legacy);
    let mut next_id: u64 = 100;
    loop {
        repl_prompt(&session.username, mgr.current_db());
        io::stdout().flush().unwrap();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() { break; }
        let cmd = line.trim();
        if cmd.is_empty() { continue; }
        let up = cmd.to_uppercase();
        if up == "EXIT" || up == "SALIR" || up == "QUIT" || up == "\\Q" { break; }
        // LOGIN <user> <password> / QUIENSOY / WHOAMI
        if up.starts_with("LOGIN ") || up.starts_with("ENTRAR ") {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.len() < 3 {
                println!("Uso: LOGIN <usuario> <password>");
                continue;
            }
            match mgr.authenticate(parts[1], parts[2]) {
                Ok(s) => {
                    println!("Autenticado como '{}'", s.username);
                    session = s;
                }
                Err(e) => println!("Error: {}", e),
            }
            continue;
        }
        if up == "QUIENSOY" || up == "WHOAMI" || up == "CURRENT_USER" {
            println!("{} (superuser: {})", session.username, if session.is_superuser { "sí" } else { "no" });
            continue;
        }
        if up == "HELP" || up == "AYUDA" {
            println!("SQL BILINGÜE:");
            println!("  CREA TABLA / CREATE TABLE  |  CAMBIA TABLA / ALTER TABLE  |  BORRA TABLA / DROP TABLE");
            println!("  AGREGAR EN / INSERT INTO   |  ACTUALIZA / UPDATE  |  BORRAR DE / DELETE FROM");
            println!("  ELIGE / SELECT  |  BUSCA / SEARCH  |  CUENTA / COUNT");
            println!("  MUESTRA TABLAS / SHOW TABLES  (lista tablas de la base activa)");
            println!("  MUESTRA TABLA / DESCRIBE TABLE / ESTRUCTURA / ESQUEMA  (ver estructura)");
            println!("Bases de datos / Databases:");
            println!("  CREA BASE mi_tienda      | CREATE DATABASE my_store");
            println!("  USA BASE mi_tienda       | USE DATABASE my_store");
            println!("  BORRA BASE mi_tienda     | DROP DATABASE my_store");
            println!("  MUESTRA BASES            | SHOW DATABASES");
            println!("  (siempre existe 'default')");
            println!("Seguridad / Security (requiere privilegio):");
            println!("  LOGIN <usuario> <password> | QUIENSOY / WHOAMI");
            println!("  CREATE USER ana IDENTIFIED BY 'x' | CREA USUARIO ana IDENTIFICADO POR 'x'");
            println!("  DROP USER ana | BORRA USUARIO ana");
            println!("  SET PASSWORD FOR ana = 'y' | CAMBIA CONTRASEÑA DE ana A 'y'");
            println!("  CREATE GROUP ventas | CREA GRUPO ventas | CREATE ROLE admin");
            println!("  ADD USER ana TO GROUP ventas | AGREGA USUARIO ana A GRUPO ventas");
            println!("  GRANT SELECT ON ventas.* TO GROUP ventas | OTORGA SELECT EN ventas.* A GRUPO ventas");
            println!("  REVOKE SELECT ON ventas.* FROM GROUP ventas | REVOCA ... DE ...");
            println!("  SHOW USERS | MUESTRA USUARIOS | SHOW GROUPS | SHOW GRANTS");
            println!("Ejemplos:");
            println!("  CREA TABLA t (id INT, nombre TEXT)");
            println!("  MUESTRA TABLA t  /  DESCRIBE TABLA t  /  ESTRUCTURA t  /  MUESTRA ESTRUCTURA DE TABLA t");
            println!("  ELIGE * DE t DONDE cat=5 ORDENA POR nombre ASC LIMITE 5");
            println!("  ELIGE cat, CUENTA(*) DE t AGRUPA POR cat ORDENA POR CUENTA(*) DESC LIMITE 5");
            println!("  BUSCA * EN t DONDE cat=5 LIMITE 5");
            println!("  AGREGAR EN t (id, nombre) VALORES (1, 'Zapatilla')");
            println!("  ACTUALIZA t ESTABLECE nombre='x' DONDE id=1");
            println!("  BORRAR DE t DONDE id=1");
            println!("REPL corto:");
            println!("  INSERT <nombre> | <desc> | <cat>  o  AGREGAR <nombre> | <desc> | <cat>");
            println!("  SEARCH <texto>  o  BUSCA <texto>");
            println!("  STATS / ESTADO");
            println!("  ESTRUCTURA <tabla> / MUESTRA TABLA <tabla> / DESCRIBE <tabla>");
            continue;
        }
        if up == "STATS" || up == "ESTADO" {
            println!("Base activa / current: {}", mgr.current_db());
            let tables = mgr.tables();
            println!("Tablas: {}", if tables.is_empty() { "(ninguna)".into() } else { tables.join(", ") });
            if let Some(db) = mgr.current() {
                let data = db.data.read().unwrap();
                for (tbl, rows) in data.iter() { println!("  {}: {} filas", tbl, rows.len()); }
            }
            println!("Trinity: {} tuplas {} páginas", trinity_legacy.num_tuples, trinity_legacy.num_pages);
            continue;
        }
        // Shortcuts INSERT/AGREGAR con pipes (con chequeo de privilegios, sin bypass)
        if (up.starts_with("INSERT ") || up.starts_with("AGREGAR ") || up.starts_with("AGREGA ")) && !up.starts_with("INSERT INTO") && !up.starts_with("AGREGAR EN") {
            let payload = if up.starts_with("INSERT ") { &cmd[7..] } else if up.starts_with("AGREGAR ") { &cmd[8..] } else { &cmd[7..] };
            if payload.contains('|') {
                // Enforcement: equivale a INSERT en productos de la base activa.
                let probe = argentum_engine::LogicalPlan::Insert { table: "productos".into(), columns: vec![], values: vec![] };
                if let Err(e) = mgr.check_plan(&session, &probe) {
                    println!("Error: {}", e);
                    continue;
                }
                let parts: Vec<&str> = payload.split('|').collect();
                if parts.len() != 3 { println!("Uso: INSERT <nombre> | <desc> | <cat>  o  AGREGAR <nombre> | <desc> | <cat>"); continue; }
                let nombre = parts[0].trim(); let desc = parts[1].trim(); let cat: i64 = parts[2].trim().parse().unwrap_or(5);
                let mut row = Row::new();
                row.insert("id".into(), Value::Int(next_id as i64));
                row.insert("nombre".into(), Value::Text(nombre.into()));
                row.insert("descripcion".into(), Value::Text(desc.into()));
                row.insert("categoria_id".into(), Value::Int(cat));
                row.insert("embedding".into(), Value::Vector(embed(&format!("{} {}", nombre, desc), 768)));
                let outcome = if let Some(db) = mgr.current() {
                    db.insert_row("productos", row)
                } else { Err("No hay base activa".into()) };
                match outcome {
                    Ok(_) => println!("OK AGREGAR id={} (SQL)", next_id),
                    Err(e) => {
                        let vec = embed(&format!("{} {}", nombre, desc), 768);
                        let row2 = format!("{}|{}|cat={}", nombre, desc, cat);
                        let slot = trinity_legacy.insert(1, row2.as_bytes(), &vec, desc.as_bytes(), &(cat as u32).to_le_bytes(), next_id, 0).unwrap();
                        println!("OK id={} slot={} (TRINITY) - {}", next_id, slot, e);
                    }
                }
                next_id += 1;
                continue;
            }
        }
        // Shortcuts SEARCH/BUSCA sin asterisco (legacy TRINITY; exige SEARCH o SELECT)
        if (up.starts_with("SEARCH ") || up.starts_with("BUSCA ") || up.starts_with("BUSCAR ")) && !up.starts_with("SEARCH *") && !up.starts_with("BUSCA *") {
            let probe = argentum_engine::LogicalPlan::Select {
                table: "productos".into(),
                projection: argentum_engine::Projection::Star,
                where_clause: None, group_by: None, order_by: None, limit: None, is_search: true,
            };
            if let Err(e) = mgr.check_plan(&session, &probe) {
                println!("Error: {}", e);
                continue;
            }
            let q = if up.starts_with("SEARCH ") { &cmd[7..] } else if up.starts_with("BUSCA ") { &cmd[6..] } else { &cmd[7..] };
            let qvec = embed(q, 768);
            let params = SearchParams{ query_vector: Some(qvec), query_text: Some(q.to_string()), top_k: 5, ef_search: 64, alpha_bm25: 0.4, alpha_vector: 0.6, txn_snapshot: (0,1000,vec![]) };
            let res = trinity_legacy.search(&params);
            if res.is_empty() { println!("(sin resultados)"); } else { for (i,r) in res.iter().enumerate(){ println!("{}. BUSCA/SEARCH slot={} score={:.4}", i+1, r.slot_id, r.score_fused); } }
            continue;
        }
        // SQL estándar: planes de base/seguridad primero, resto con enforcement.
        match argentum_engine::parser::parse(cmd) {
            Ok(plan) => {
                if let Some(out) = handle_db_plan(&mut mgr, &session, &plan) {
                    println!("{}", out);
                    continue;
                }
                // Si no es plan de BD/seguridad, delegamos con chequeo de privilegios.
                match mgr.execute_current(&session, plan) {
                    Ok(res) => println!("{}", res.to_display()),
                    Err(e) => println!("Error: {}", e),
                }
            }
            Err(e) => println!("Error parse: {} (HELP/AYUDA)", e),
        }
    }
    println!("¡Hasta luego! / Bye.");
    println!("(datos en {} / data persisted at)", base_dir);
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 || args.iter().any(|a| a == "--help" || a == "-h") { print_help(); return Ok(()); }
    if args.iter().any(|a| a == "--demo") { run_trinity_demo()?; run_demo_sql()?; run_demo_es()?; return Ok(()); }
    if args.iter().any(|a| a == "--demo-sql") { return run_demo_sql(); }
    if args.iter().any(|a| a == "--demo-es") { return run_demo_es(); }
    if args.iter().any(|a| a == "--repl") {
        // --data-dir <path> opcional: persistir en un directorio fijo entre sesiones
        let data_dir = args.iter()
            .position(|a| a == "--data-dir")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let user = args.iter()
            .position(|a| a == "--user" || a == "-u")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let password = args.iter()
            .position(|a| a == "--password" || a == "-p")
            .and_then(|i| args.get(i + 1))
            .cloned();
        return run_repl_full(data_dir.as_deref(), user.as_deref(), password.as_deref());
    }
    eprintln!("Opción desconocida. Usa --help"); std::process::exit(1);
}

