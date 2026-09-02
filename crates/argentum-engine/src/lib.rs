//! crates/aether-engine/src/lib.rs - Motor con Catalog + Parser + Executor
#![allow(unused_variables, dead_code)]
use argentum_common::catalog::{Catalog, ColumnDef, DataType};
use argentum_common::TxnId;
use argentum_index::{SearchParams, TrinityIndex};
use argentum_storage::buffer_pool::BufferPool;
use argentum_storage::wal::{WalManager, WalRecord};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub mod parser;

// Re-export
pub use parser::parse;

#[derive(Debug, Clone)]
pub enum Projection {
    Star,
    CountStar,
    CountCol(String),
    Columns(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct OrderBy {
    pub column: String,
    pub asc: bool,
}

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    CreateTable { table: String, columns: Vec<ColumnDef> },
    AlterTableAddColumn { table: String, column: String, col_type: String },
    AlterTableDropColumn { table: String, column: String },
    DropTable { table: String },
    DescribeTable { table: String },
    Insert { table: String, columns: Vec<String>, values: Vec<String> },
    Delete { table: String, where_clause: Option<String> },
    Update { table: String, assignments: Vec<(String, String)>, where_clause: Option<String> },
    Select {
        table: String,
        projection: Projection,
        where_clause: Option<String>,
        group_by: Option<String>,
        order_by: Option<OrderBy>,
        limit: Option<usize>,
        is_search: bool,
    },
    // Legacy
    SeqScan { table: String },
    TrinityScan { table: String, params: SearchParams },
    Filter { input: Box<LogicalPlan>, predicate: String },
    HashJoin { left: Box<LogicalPlan>, right: Box<LogicalPlan>, on: String },
    GraphTraverse { input: Box<LogicalPlan>, pattern: String },
    Limit { input: Box<LogicalPlan>, limit: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Text(String),
    Vector(Vec<f32>),
    Null,
}

impl Value {
    pub fn to_string(&self) -> String {
        match self {
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Text(s) => s.clone(),
            Self::Vector(v) => format!("VECTOR({}d)", v.len()),
            Self::Null => "NULL".into(),
        }
    }
    pub fn as_str_key(&self) -> String {
        match self {
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Text(s) => s.clone(),
            Self::Vector(v) => format!("{:?}", &v[..3.min(v.len())]),
            Self::Null => "NULL".into(),
        }
    }
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Int(a), Self::Float(b)) => (*a as f64).partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
            (Self::Float(a), Self::Int(b)) => a.partial_cmp(&(*b as f64)).unwrap_or(std::cmp::Ordering::Equal),
            _ => self.to_string().cmp(&other.to_string()),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

pub type Row = HashMap<String, Value>;

fn embed_text(text: &str, dim: usize) -> Vec<f32> {
    // Deterministic LCG like aether-server
    let mut h: u64 = 14695981039346656037;
    for b in text.bytes() { h ^= b as u64; h = h.wrapping_mul(1099511628211); }
    let mut s = h;
    let mut v = Vec::with_capacity(dim);
    for _ in 0..dim {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        let u = (s >> 32) as u32;
        v.push((u as f32 / u32::MAX as f32) * 2.0 - 1.0);
    }
    v
}

fn parse_value_raw(raw: &str, expected_type: Option<&DataType>) -> Value {
    let raw = raw.trim();
    // EMBED('text') -> Vector
    let up = raw.to_uppercase();
    if up.starts_with("EMBED(") && raw.ends_with(')') {
        let inner = &raw[6..raw.len()-1].trim().trim_matches(|c| c == '\'' || c == '"');
        let dim = if let Some(DataType::Vector(d)) = expected_type { *d } else { 768 };
        return Value::Vector(embed_text(inner, dim));
    }
    if raw.eq_ignore_ascii_case("NULL") { return Value::Null; }
    if (raw.starts_with('\'') && raw.ends_with('\'')) || (raw.starts_with('"') && raw.ends_with('"')) {
        return Value::Text(raw[1..raw.len()-1].to_string());
    }
    // Try int
    if let Ok(i) = raw.parse::<i64>() { return Value::Int(i); }
    if let Ok(f) = raw.parse::<f64>() { return Value::Float(f); }
    // Fallback text
    Value::Text(raw.to_string())
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < 1e-9,
        (Value::Int(x), Value::Float(y)) => (*x as f64 - *y).abs() < 1e-9,
        (Value::Float(x), Value::Int(y)) => (*x - *y as f64).abs() < 1e-9,
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => a.to_string() == b.to_string(),
    }
}

fn eval_where(row: &Row, where_clause: &str, table_def: Option<&argentum_common::catalog::TableDef>) -> bool {
    let clause = where_clause.trim();
    if clause.is_empty() { return true; }
    // Soporta AND / Y (ES)
    let mut parts: Vec<&str> = Vec::new();
    let upper = clause.to_uppercase();
    let mut start = 0;
    let mut i = 0;
    loop {
        let find_and = upper[start..].find(" AND ");
        let find_y = upper[start..].find(" Y ");
        let (pos, len) = match (find_and, find_y) {
            (Some(a), Some(b)) => if a < b { (a, 5) } else { (b, 3) },
            (Some(a), None) => (a, 5),
            (None, Some(b)) => (b, 3),
            (None, None) => break,
        };
        let end = start + pos;
        parts.push(&clause[start..end]);
        start = end + len;
        i += 1;
        if i > 10 { break; }
    }
    parts.push(&clause[start..]);
    for part in parts {
        let p = part.trim();
        // Soporta =, !=, <>
        let (op, left, right) = if let Some(pos) = p.find("!=") {
            ("!=", p[..pos].trim(), p[pos+2..].trim())
        } else if let Some(pos) = p.find("<>") {
            ("!=", p[..pos].trim(), p[pos+2..].trim())
        } else if let Some(pos) = p.find('=') {
            // Evitar confundir con <-> o <= >= (no soportado en WHERE simple)
            // Si es <-> lo tratamos como no filtro (se ignora para demo)
            if p.contains("<->") { continue; }
            ("=", p[..pos].trim(), p[pos+1..].trim())
        } else {
            // No reconocido, ignora
            continue;
        };
        let col = left.trim();
        let val_raw = right.trim();
        let row_val = row.get(col).or_else(|| {
            // case-insensitive fallback
            row.iter().find(|(k,_)| k.eq_ignore_ascii_case(col)).map(|(_,v)| v)
        });
        if row_val.is_none() {
            // columna no existe => no pasa filtro
            return false;
        }
        let rv = row_val.unwrap();
        // Determinar tipo esperado para EMBED
        let expected_type = table_def.and_then(|t| t.column_index(col)).and_then(|idx| table_def.map(|td| &td.columns[idx].data_type));
        let cmp_val = parse_value_raw(val_raw, expected_type);
        let eq = values_equal(rv, &cmp_val);
        if op == "=" && !eq { return false; }
        if op == "!=" && eq { return false; }
    }
    true
}

/// Resultado de ejecución
#[derive(Debug)]
pub enum ExecutionResult {
    Created { table: String },
    Altered { table: String },
    Dropped { table: String },
    Inserted { count: usize },
    Deleted { count: usize },
    Updated { count: usize },
    Selected { columns: Vec<String>, rows: Vec<Row> },
    // Para compatibilidad
    Other(Vec<Vec<u8>>),
}

impl ExecutionResult {
    pub fn to_display(&self) -> String {
        match self {
            Self::Created { table } => format!("Tabla '{}' creada / Table '{}' created", table, table),
            Self::Altered { table } => format!("Tabla '{}' alterada / Table '{}' altered", table, table),
            Self::Dropped { table } => format!("Tabla '{}' borrada / Table '{}' dropped", table, table),
            Self::Inserted { count } => format!("{} fila(s) agregada(s) / {} row(s) inserted", count, count),
            Self::Deleted { count } => format!("{} fila(s) borrada(s) / {} row(s) deleted", count, count),
            Self::Updated { count } => format!("{} fila(s) actualizada(s) / {} row(s) updated", count, count),
            Self::Selected { columns, rows } => {
                if rows.is_empty() { return "(0 rows)".into(); }
                let mut out = String::new();
                out.push_str(&columns.join(" | "));
                out.push_str("\n");
                out.push_str(&"-".repeat(columns.join(" | ").len()));
                out.push('\n');
                for row in rows {
                    let vals: Vec<String> = columns.iter().map(|c| row.get(c).map(|v| v.to_string()).unwrap_or("NULL".into())).collect();
                    out.push_str(&vals.join(" | "));
                    out.push('\n');
                }
                out.push_str(&format!("\n({} row(s))", rows.len()));
                out
            }
            Self::Other(_) => "OK".into(),
        }
    }
}

/// Database: catálogo + datos + índice TRINITY + WAL
pub struct Database {
    pub catalog: Arc<RwLock<Catalog>>,
    pub data: Arc<RwLock<HashMap<String, Vec<Row>>>>, // table lower -> rows
    pub trinity: Arc<RwLock<TrinityIndex>>,
    pub wal: Arc<WalManager>,
    pub buffer_pool: Arc<BufferPool>,
    heap_path: Option<String>,
    next_txn: std::sync::atomic::AtomicU64,
}

impl Database {
    pub fn new(wal: Arc<WalManager>, buffer_pool: Arc<BufferPool>, trinity: TrinityIndex, catalog_path: Option<String>) -> Self {
        let catalog = if let Some(p) = catalog_path {
            Catalog::with_persist(&p)
        } else {
            Catalog::new()
        };
        Self {
            catalog: Arc::new(RwLock::new(catalog)),
            data: Arc::new(RwLock::new(HashMap::new())),
            trinity: Arc::new(RwLock::new(trinity)),
            wal,
            buffer_pool,
            heap_path: None,
            next_txn: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn with_data_dir(data_dir: &str) -> Self {
        let _ = std::fs::create_dir_all(data_dir);
        let wal_path = format!("{}/aether.wal", data_dir.trim_end_matches('/'));
        let catalog_path = format!("{}/catalog.json", data_dir.trim_end_matches('/'));
        let heap_path = format!("{}/heap.json", data_dir.trim_end_matches('/'));
        let wal = WalManager::open(&wal_path).unwrap();
        let bp = Arc::new(BufferPool::new(1024));
        let trinity = TrinityIndex::new(bp.clone(), wal.clone());
        let mut db = Self::new(wal, bp, trinity, Some(catalog_path));
        db.load_heap(&heap_path);
        // Guardar heap_path para persistencia futura
        db.heap_path = Some(heap_path);
        db
    }

    pub fn new_in_memory() -> Self {
        let wal_path = std::env::temp_dir().join(format!("argentum_db_{}.wal", std::process::id()));
        let _ = std::fs::remove_file(&wal_path);
        let wal = WalManager::open(wal_path.to_str().unwrap()).unwrap();
        let bp = Arc::new(BufferPool::new(128));
        let trinity = TrinityIndex::new(bp.clone(), wal.clone());
        Self::new(wal, bp, trinity, None)
    }

    fn next_txn_id(&self) -> TxnId {
        self.next_txn.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn seq_key(table: &str, col: &str) -> String { format!("{}.{}", table.to_lowercase(), col.to_lowercase()) }

    fn init_sequence(&self, table: &str, col: &str, start: i64) {
        let mut cat = self.catalog.write().unwrap();
        let key = Self::seq_key(table, col);
        cat.sequences.entry(key).or_insert(start);
        cat.persist();
    }

    fn next_identity(&self, table: &str, col: &str) -> i64 {
        let mut cat = self.catalog.write().unwrap();
        cat.get_next_sequence(table, col)
    }

    fn advance_sequence_for_explicit(&self, table: &str, col: &str, explicit: i64) {
        let mut cat = self.catalog.write().unwrap();
        cat.advance_sequence(table, col, explicit);
    }

    fn persist_heap(&self) {
        if let Some(path) = &self.heap_path {
            let data = self.data.read().unwrap();
            let mut out = String::new();
            out.push_str("{\n");
            let mut first_table = true;
            for (tbl, rows) in data.iter() {
                if !first_table { out.push_str(",\n"); }
                first_table = false;
                out.push_str(&format!("  \"{}\": [\n", tbl.replace('"', "\\\"")));
                for (i, row) in rows.iter().enumerate() {
                    if i > 0 { out.push_str(",\n"); }
                    out.push_str("    {");
                    let mut first_col = true;
                    for (k, v) in row {
                        if !first_col { out.push_str(", "); }
                        first_col = false;
                        let v_str = match v {
                            Value::Int(n) => n.to_string(),
                            Value::Float(f) => f.to_string(),
                            Value::Text(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")),
                            Value::Null => "null".to_string(),
                            Value::Vector(vec) => {
                                let nums: Vec<String> = vec.iter().map(|x| x.to_string()).collect();
                                format!("[{}]", nums.join(", "))
                            }
                        };
                        out.push_str(&format!("\"{}\": {}", k.replace('"', "\\\""), v_str));
                    }
                    out.push_str("}");
                }
                out.push_str("\n  ]");
            }
            out.push_str("\n}\n");
            if let Some(parent) = std::path::Path::new(path).parent() { let _ = std::fs::create_dir_all(parent); }
            let tmp = format!("{}.tmp", path);
            if std::fs::write(&tmp, &out).is_ok() {
                let _ = std::fs::rename(&tmp, path);
                if let Ok(f) = std::fs::OpenOptions::new().read(true).open(path) { let _ = f.sync_all(); }
            }
        }
    }

    fn load_heap(&self, path: &str) {
        if let Ok(content) = std::fs::read_to_string(path) {
            // Parser JSON simple para heap: {"table": [{"col": val}, ...]}
            let mut data = self.data.write().unwrap();
            // Si el contenido es vacío o "{}", no hacer nada
            if content.trim().is_empty() || content.trim() == "{}" { return; }
            // Buscar cada tabla: "table": [
            let mut pos = 0;
            while let Some(q1) = content[pos..].find('"') {
                let abs_q1 = pos + q1;
                if let Some(q2) = content[abs_q1+1..].find('"') {
                    let abs_q2 = abs_q1 + 1 + q2;
                    let table_name = content[abs_q1+1..abs_q2].to_string();
                    // Buscar inicio de array [
                    if let Some(br) = content[abs_q2..].find('[') {
                        let arr_start = abs_q2 + br + 1;
                        // Encontrar cierre del array de filas: buscar "]" correspondiente al de tabla (con depth)
                        let mut depth = 1;
                        let mut arr_end = arr_start;
                        let mut in_str = false;
                        let mut esc = false;
                        for (i, c) in content[arr_start..].char_indices() {
                            if esc { esc = false; continue; }
                            if c == '\\' && in_str { esc = true; continue; }
                            if c == '"' { in_str = !in_str; continue; }
                            if in_str { continue; }
                            if c == '[' { depth += 1; }
                            if c == ']' { depth -= 1; if depth == 0 { arr_end = arr_start + i; break; } }
                        }
                        let rows_json = &content[arr_start..arr_end];
                        // Parsear filas: cada { ... }
                        let mut rows: Vec<Row> = Vec::new();
                        let mut rpos = 0;
                        while let Some(ob) = rows_json[rpos..].find('{') {
                            let abs_ob = rpos + ob;
                            // Encontrar cierre }
                            let mut d = 0;
                            let mut in_s = false;
                            let mut es = false;
                            let mut ob_end = abs_ob;
                            for (i, c) in rows_json[abs_ob..].char_indices() {
                                if es { es = false; continue; }
                                if c == '\\' && in_s { es = true; continue; }
                                if c == '"' { in_s = !in_s; continue; }
                                if in_s { continue; }
                                if c == '{' { d += 1; }
                                if c == '}' { d -= 1; if d == 0 { ob_end = abs_ob + i; break; } }
                            }
                            let row_json = &rows_json[abs_ob+1..ob_end];
                            let mut row = Row::new();
                            // Parsear pares "col": val
                            let mut cpos2 = 0;
                            while let Some(q1) = row_json[cpos2..].find('"') {
                                let a1 = cpos2 + q1;
                                if let Some(q2) = row_json[a1+1..].find('"') {
                                    let a2 = a1 + 1 + q2;
                                    let col_name = row_json[a1+1..a2].to_string();
                                    if let Some(colon) = row_json[a2..].find(':') {
                                        let val_start = a2 + colon + 1;
                                        let val_trim = row_json[val_start..].trim_start();
                                        let (val, consumed) = if val_trim.starts_with('"') {
                                            // Texto
                                            let mut end = 1;
                                            let mut esc2 = false;
                                            let in_s2 = true;
                                            for (j, ch) in val_trim[1..].char_indices() {
                                                if esc2 { esc2 = false; continue; }
                                                if ch == '\\' { esc2 = true; continue; }
                                                if ch == '"' { end = j + 1; break; }
                                            }
                                            let raw = &val_trim[1..end];
                                            let unesc = raw.replace("\\\"", "\"").replace("\\\\", "\\").replace("\\n", "\n");
                                            (Value::Text(unesc), end + 2)
                                        } else if val_trim.starts_with("null") {
                                            (Value::Null, 4)
                                        } else if val_trim.starts_with('[') {
                                            // Vector
                                            if let Some(end) = val_trim.find(']') {
                                                let inner = &val_trim[1..end];
                                                let nums: Vec<f32> = inner.split(',').filter_map(|s| s.trim().parse::<f32>().ok()).collect();
                                                (Value::Vector(nums), end + 1)
                                            } else { (Value::Null, 4) }
                                        } else {
                                            // Número
                                            let end = val_trim.find(|c| c == ',' || c == '}').unwrap_or(val_trim.len());
                                            let num_str = val_trim[..end].trim();
                                            if let Ok(i) = num_str.parse::<i64>() { (Value::Int(i), end) }
                                            else if let Ok(f) = num_str.parse::<f64>() { (Value::Float(f), end) }
                                            else { (Value::Text(num_str.to_string()), end) }
                                        };
                                        row.insert(col_name, val);
                                        // Avanzar cpos2
                                        let next_comma = row_json[a2..].find(',').map(|v| a2 + v + 1).unwrap_or(row_json.len());
                                        // Buscar próxima "col"
                                        if let Some(next_q) = row_json[next_comma..].find('"') {
                                            cpos2 = next_comma + next_q;
                                        } else { break; }
                                    } else { break; }
                                } else { break; }
                            }
                            rows.push(row);
                            rpos = ob_end + 1;
                            if rpos >= rows_json.len() { break; }
                        }
                        if !rows.is_empty() {
                            data.insert(table_name.to_lowercase(), rows);
                        }
                        pos = arr_end + 1;
                    } else { break; }
                } else { break; }
            }
        }
    }

    pub fn execute(&self, plan: LogicalPlan) -> Result<ExecutionResult, String> {
        match plan {
            LogicalPlan::CreateTable { table, columns } => self.exec_create(table, columns),
            LogicalPlan::AlterTableAddColumn { table, column, col_type } => self.exec_alter_add(table, column, col_type),
            LogicalPlan::AlterTableDropColumn { table, column } => self.exec_alter_drop(table, column),
            LogicalPlan::DropTable { table } => self.exec_drop(table),
            LogicalPlan::DescribeTable { table } => self.exec_describe(table),
            LogicalPlan::Insert { table, columns, values } => self.exec_insert(table, columns, values),
            LogicalPlan::Delete { table, where_clause } => self.exec_delete(table, where_clause),
            LogicalPlan::Update { table, assignments, where_clause } => self.exec_update(table, assignments, where_clause),
            LogicalPlan::Select { table, projection, where_clause, group_by, order_by, limit, is_search } => {
                self.exec_select(table, projection, where_clause, group_by, order_by, limit, is_search)
            }
            // Legacy passthrough
            LogicalPlan::SeqScan { table } => self.exec_select(table, Projection::Star, None, None, None, None, false),
            LogicalPlan::TrinityScan { table, params } => {
                let res = self.trinity.read().unwrap().search(&params);
                let mut rows = Vec::new();
                for r in res {
                    let mut row = Row::new();
                    row.insert("page_id".into(), Value::Int(r.page_id as i64));
                    row.insert("slot_id".into(), Value::Int(r.slot_id as i64));
                    row.insert("score".into(), Value::Float(r.score_fused as f64));
                    rows.push(row);
                }
                Ok(ExecutionResult::Selected { columns: vec!["page_id".into(), "slot_id".into(), "score".into()], rows })
            }
            LogicalPlan::Limit { input, limit } => {
                // Delegar
                let res = self.execute(*input)?;
                match res {
                    ExecutionResult::Selected { columns, rows } => {
                        let truncated = rows.into_iter().take(limit).collect();
                        Ok(ExecutionResult::Selected { columns, rows: truncated })
                    }
                    other => Ok(other),
                }
            }
            _ => Err("Unsupported plan in v1".into()),
        }
    }

    fn exec_create(&self, table: String, columns: Vec<ColumnDef>) -> Result<ExecutionResult, String> {
        let txn = self.next_txn_id();
        let wal_cols: Vec<(String, String)> = columns.iter().map(|c| (c.name.clone(), c.data_type.to_string())).collect();
        let lsn = self.wal.append(WalRecord::CreateTable { txn_id: txn, lsn: 0, table: table.clone(), columns: wal_cols }).map_err(|e| e.to_string())?;
        {
            let mut cat = self.catalog.write().unwrap();
            cat.create_table(&table, columns.clone())?;
        }
        // Inicializar secuencias para columnas IDENTITY
        for col in &columns {
            if let Some(id) = &col.identity {
                self.init_sequence(&table, &col.name, id.start);
            }
        }
        {
            let mut data = self.data.write().unwrap();
            data.insert(table.to_lowercase(), Vec::new());
        }
        self.persist_heap();
        self.wal.append(WalRecord::Commit { txn_id: txn, lsn: 0 }).map_err(|e| e.to_string())?;
        let _ = lsn;
        Ok(ExecutionResult::Created { table })
    }

    fn exec_alter_add(&self, table: String, column: String, col_type: String) -> Result<ExecutionResult, String> {
        let txn = self.next_txn_id();
        self.wal.append(WalRecord::AlterTableAddColumn { txn_id: txn, lsn: 0, table: table.clone(), column: column.clone(), col_type: col_type.clone() }).map_err(|e| e.to_string())?;
        let dt = DataType::parse(&col_type).unwrap_or(DataType::Text);
        {
            let mut cat = self.catalog.write().unwrap();
            cat.alter_add_column(&table, ColumnDef::new(&column, dt))?;
        }
        {
            let mut data = self.data.write().unwrap();
            if let Some(rows) = data.get_mut(&table.to_lowercase()) {
                for row in rows.iter_mut() {
                    row.insert(column.clone(), Value::Null);
                }
            }
        }
        self.persist_heap();
        Ok(ExecutionResult::Altered { table })
    }

    fn exec_alter_drop(&self, table: String, column: String) -> Result<ExecutionResult, String> {
        let txn = self.next_txn_id();
        self.wal.append(WalRecord::AlterTableDropColumn { txn_id: txn, lsn: 0, table: table.clone(), column: column.clone() }).map_err(|e| e.to_string())?;
        {
            let mut cat = self.catalog.write().unwrap();
            cat.alter_drop_column(&table, &column)?;
        }
        {
            let mut data = self.data.write().unwrap();
            if let Some(rows) = data.get_mut(&table.to_lowercase()) {
                for row in rows.iter_mut() {
                    row.remove(&column);
                    // case-insensitive
                    let keys: Vec<String> = row.keys().cloned().collect();
                    for k in keys { if k.eq_ignore_ascii_case(&column) { row.remove(&k); } }
                }
            }
        }
        self.persist_heap();
        Ok(ExecutionResult::Altered { table })
    }

    fn exec_drop(&self, table: String) -> Result<ExecutionResult, String> {
        let txn = self.next_txn_id();
        self.wal.append(WalRecord::DropTable { txn_id: txn, lsn: 0, table: table.clone() }).map_err(|e| e.to_string())?;
        {
            let mut cat = self.catalog.write().unwrap();
            cat.drop_table(&table)?;
        }
        {
            let mut data = self.data.write().unwrap();
            data.remove(&table.to_lowercase());
        }
        self.persist_heap();
        Ok(ExecutionResult::Dropped { table })
    }

    fn exec_describe(&self, table: String) -> Result<ExecutionResult, String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(&table).cloned().ok_or(format!("Tabla '{}' no encontrada / Table '{}' not found", table, table))?
        };
        let mut rows: Vec<Row> = Vec::new();
        for col in &cat_table.columns {
            let mut row = Row::new();
            row.insert("Columna".into(), Value::Text(col.name.clone()));
            row.insert("Tipo".into(), Value::Text(col.data_type.to_string()));
            row.insert("Nulo".into(), Value::Text(if col.nullable { "Sí".into() } else { "No".into() }));
            row.insert("Clave Primaria".into(), Value::Text(if col.is_primary_key { "Sí".into() } else { "No".into() }));
            let identidad = if let Some(id) = &col.identity {
                match id.kind {
                    argentum_common::catalog::IdentityKind::Always => "GENERADO SIEMPRE COMO IDENTIDAD / GENERATED ALWAYS".to_string(),
                    argentum_common::catalog::IdentityKind::ByDefault => "GENERADO POR DEFECTO COMO IDENTIDAD / BY DEFAULT".to_string(),
                }
            } else {
                "-".to_string()
            };
            row.insert("Identidad".into(), Value::Text(identidad));
            let auto = if col.is_identity() { "Sí / Yes" } else { "No" };
            row.insert("Autoincremental".into(), Value::Text(auto.to_string()));
            // Mostrar secuencia actual si es identity
            let seq_val = if col.is_identity() {
                let cat = self.catalog.read().unwrap();
                cat.peek_sequence(&table, &col.name).map(|v| v.to_string()).unwrap_or("1 (siguiente)".to_string())
            } else {
                "-".to_string()
            };
            row.insert("Siguiente Valor".into(), Value::Text(seq_val));
            rows.push(row);
        }
        let cols = vec!["Columna".into(), "Tipo".into(), "Nulo".into(), "Clave Primaria".into(), "Identidad".into(), "Autoincremental".into(), "Siguiente Valor".into()];
        Ok(ExecutionResult::Selected { columns: cols, rows })
    }

    fn exec_insert(&self, table: String, columns: Vec<String>, values: Vec<String>) -> Result<ExecutionResult, String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(&table).cloned().ok_or(format!("Tabla '{}' no encontrada / Table '{}' not found", table, table))?
        };
        // Manejo IDENTITY: detectar columnas autoincrementales
        let identity_cols: Vec<&argentum_common::catalog::ColumnDef> = cat_table.columns.iter().filter(|c| c.identity.is_some()).collect();
        // Caso 1: INSERT sin lista de columnas -> inferir
        let (target_cols, values) = if columns.is_empty() {
            if values.len() == cat_table.columns.len() {
                // Usuario proveyó todos los valores incluyendo identity -> verificar ALWAYS
                (cat_table.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), values.clone())
            } else if values.len() + identity_cols.len() == cat_table.columns.len() {
                // Usuario omitió identity cols -> generar automáticamente
                // Mapear valores a columnas no-identity en orden
                let non_id_cols: Vec<String> = cat_table.columns.iter().filter(|c| c.identity.is_none()).map(|c| c.name.clone()).collect();
                if non_id_cols.len() != values.len() {
                    return Err(format!("Columnas {} (sin identity) no coinciden con valores {} / Columns mismatch", non_id_cols.len(), values.len()));
                }
                (non_id_cols, values.clone())
            } else {
                // Intentar con columnas completas pero con DEFAULT para identity
                (cat_table.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), values.clone())
            }
        } else {
            (columns.clone(), values.clone())
        };
        if target_cols.len() != values.len() {
            return Err(format!("Columnas {} no coinciden con valores {} / Columns {} mismatch values {}", target_cols.len(), values.len(), target_cols.len(), values.len()));
        }
        let mut row = Row::new();
        for (col, val_raw) in target_cols.iter().zip(values.iter()) {
            let idx = cat_table.column_index(col).ok_or(format!("Columna '{}' no encontrada / Column '{}' not found", col, col))?;
            let col_def = &cat_table.columns[idx];
            let is_default = val_raw.trim().eq_ignore_ascii_case("DEFAULT");
            if let Some(id_spec) = &col_def.identity {
                if is_default {
                    let gen = self.next_identity(&table, col);
                    row.insert(col.clone(), Value::Int(gen));
                    continue;
                }
                // Valor explícito para columna IDENTITY
                match id_spec.kind {
                    argentum_common::catalog::IdentityKind::Always => {
                        return Err(format!("No se puede insertar valor explícito en columna GENERATED ALWAYS AS IDENTITY '{}' / Cannot insert explicit value into GENERATED ALWAYS column '{}' (use GENERATED BY DEFAULT o DEFAULT)", col, col));
                    }
                    argentum_common::catalog::IdentityKind::ByDefault => {
                        let val = parse_value_raw(val_raw, Some(&col_def.data_type));
                        // Avanzar secuencia si es mayor
                        if let Value::Int(i) = &val { self.advance_sequence_for_explicit(&table, col, *i); }
                        row.insert(col.clone(), val);
                        continue;
                    }
                }
            } else {
                if is_default {
                    row.insert(col.clone(), Value::Null);
                    continue;
                }
            }
            let expected = &col_def.data_type;
            let val = parse_value_raw(val_raw, Some(expected));
            row.insert(col.clone(), val);
        }
        // Rellenar columnas faltantes: generar identity o NULL
        let mut full_row = Row::new();
        for col_def in &cat_table.columns {
            if let Some(v) = row.get(&col_def.name) {
                full_row.insert(col_def.name.clone(), v.clone());
            } else if let Some(found) = row.iter().find(|(k,_)| k.eq_ignore_ascii_case(&col_def.name)).map(|(_,v)| v).cloned() {
                full_row.insert(col_def.name.clone(), found);
            } else if let Some(id_spec) = &col_def.identity {
                // Generar autoincremental si no se proveyó
                let gen = self.next_identity(&table, &col_def.name);
                full_row.insert(col_def.name.clone(), Value::Int(gen));
                let _ = id_spec;
            } else {
                full_row.insert(col_def.name.clone(), Value::Null);
            }
        }
        let txn = self.next_txn_id();
        // WAL para insert (reusar TrinityInsert como stub o crear log genérico)
        let mut payload = Vec::new();
        payload.extend_from_slice(table.as_bytes());
        self.wal.append(WalRecord::TrinityInsert { txn_id: txn, lsn: 0, page_id: 0, slot_id: 0, payload, prev_lsn: 0 }).map_err(|e| e.to_string())?;
        {
            let mut data = self.data.write().unwrap();
            let vec = data.get_mut(&table.to_lowercase()).ok_or(format!("Tabla '{}' no encontrada", table))?;
            vec.push(full_row);
        }
        Ok(ExecutionResult::Inserted { count: 1 })
    }

    fn exec_delete(&self, table: String, where_clause: Option<String>) -> Result<ExecutionResult, String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(&table).cloned().ok_or(format!("Tabla '{}' no encontrada / Table '{}' not found", table, table))?
        };
        let mut data = self.data.write().unwrap();
        let rows = data.get_mut(&table.to_lowercase()).ok_or(format!("Tabla '{}' no encontrada", table))?;
        if where_clause.is_none() {
            let count = rows.len();
            rows.clear();
            return Ok(ExecutionResult::Deleted { count });
        }
        let wc = where_clause.unwrap();
        let mut retained = Vec::new();
        let mut deleted = 0;
        for row in rows.drain(..) {
            if eval_where(&row, &wc, Some(&cat_table)) {
                deleted += 1;
            } else {
                retained.push(row);
            }
        }
        *rows = retained;
        Ok(ExecutionResult::Deleted { count: deleted })
    }

    fn exec_update(&self, table: String, assignments: Vec<(String, String)>, where_clause: Option<String>) -> Result<ExecutionResult, String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(&table).cloned().ok_or(format!("Table '{}' not found", table))?
        };
        let mut data = self.data.write().unwrap();
        let rows = data.get_mut(&table.to_lowercase()).ok_or(format!("Table '{}' not found", table))?;
        let mut count = 0;
        for row in rows.iter_mut() {
            let matches = if let Some(ref wc) = where_clause {
                eval_where(row, wc, Some(&cat_table))
            } else { true };
            if matches {
                for (col, val_raw) in &assignments {
                    // Validar columna existe
                    let col_idx = cat_table.column_index(col).ok_or(format!("Column '{}' not found", col))?;
                    let expected = &cat_table.columns[col_idx].data_type;
                    let val = parse_value_raw(val_raw, Some(expected));
                    // Si es UPDATE de embedding con EMBED, ya está vectorizado
                    row.insert(col.clone(), val);
                }
                count += 1;
            }
        }
        // También actualizar TRINITY si hay vector
        // Para v1, no reindexamos físicamente, solo dato en memoria
        Ok(ExecutionResult::Updated { count })
    }

    pub fn exec_select(
        &self,
        table: String,
        projection: Projection,
        where_clause: Option<String>,
        group_by: Option<String>,
        order_by: Option<OrderBy>,
        limit: Option<usize>,
        _is_search: bool,
    ) -> Result<ExecutionResult, String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(&table).cloned().ok_or(format!("Table '{}' not found", table))?
        };
        let data = self.data.read().unwrap();
        let rows = data.get(&table.to_lowercase()).ok_or(format!("Table '{}' not found", table))?;
        // 1. WHERE filter
        let mut filtered: Vec<Row> = rows.iter().filter(|r| {
            if let Some(ref wc) = where_clause {
                eval_where(r, wc, Some(&cat_table))
            } else { true }
        }).cloned().collect();

        // 2. GROUP BY + COUNT
        if let Some(gb_col) = group_by {
            // Validar columna existe
            if cat_table.column_index(&gb_col).is_none() {
                return Err(format!("GROUP BY column '{}' not found", gb_col));
            }
            let mut groups: HashMap<String, (Value, usize)> = HashMap::new();
            for row in &filtered {
                let key_val = row.get(&gb_col).or_else(|| row.iter().find(|(k,_)| k.eq_ignore_ascii_case(&gb_col)).map(|(_,v)| v)).unwrap_or(&Value::Null);
                let key = key_val.as_str_key();
                groups.entry(key).or_insert_with(|| (key_val.clone(), 0)).1 += 1;
            }
            // Construir filas agregadas
            let mut agg_rows: Vec<Row> = Vec::new();
            for (key, (val, cnt)) in groups {
                let mut r = Row::new();
                // Determinar projection: puede ser COUNT(*) o COUNT(col)
                // Para v1, si projection es CountStar o CountCol, mostramos gb_col + count
                r.insert(gb_col.clone(), val);
                // Detectar si projection quiere COUNT
                match &projection {
                    Projection::CountStar | Projection::CountCol(_) => {
                        r.insert("COUNT(*)".into(), Value::Int(cnt as i64));
                    }
                    Projection::Columns(cols) if cols.iter().any(|c| c.to_uppercase().contains("COUNT")) => {
                        r.insert("COUNT(*)".into(), Value::Int(cnt as i64));
                    }
                    _ => {
                        r.insert("COUNT(*)".into(), Value::Int(cnt as i64));
                    }
                }
                let _ = key;
                agg_rows.push(r);
            }
            filtered = agg_rows;
            // Columnas para SELECT GROUP BY: gb_col + COUNT(*)
            let cols = vec![gb_col.clone(), "COUNT(*)".into()];
            // ORDER BY puede ser COUNT(*) o gb_col - NULLS LAST siempre
            if let Some(ob) = order_by {
                if ob.column.to_uppercase().contains("COUNT") {
                    filtered.sort_by(|a, b| {
                        let av = a.get("COUNT(*)").unwrap_or(&Value::Null);
                        let bv = b.get("COUNT(*)").unwrap_or(&Value::Null);
                        match (av, bv) {
                            (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                            (Value::Null, _) => std::cmp::Ordering::Greater,
                            (_, Value::Null) => std::cmp::Ordering::Less,
                            _ => { let ord = av.compare(bv); if ob.asc { ord } else { ord.reverse() } }
                        }
                    });
                } else {
                    filtered.sort_by(|a, b| {
                        let av = a.get(&ob.column).or_else(|| a.iter().find(|(k,_)| k.eq_ignore_ascii_case(&ob.column)).map(|(_,v)| v)).unwrap_or(&Value::Null);
                        let bv = b.get(&ob.column).or_else(|| b.iter().find(|(k,_)| k.eq_ignore_ascii_case(&ob.column)).map(|(_,v)| v)).unwrap_or(&Value::Null);
                        match (av, bv) {
                            (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                            (Value::Null, _) => std::cmp::Ordering::Greater,
                            (_, Value::Null) => std::cmp::Ordering::Less,
                            _ => { let ord = av.compare(bv); if ob.asc { ord } else { ord.reverse() } }
                        }
                    });
                }
            }
            if let Some(lim) = limit { filtered.truncate(lim); }
            return Ok(ExecutionResult::Selected { columns: cols, rows: filtered });
        }

        // 3. ORDER BY sin GROUP BY - NULLS LAST
        if let Some(ob) = order_by {
            filtered.sort_by(|a, b| {
                let av = a.get(&ob.column).or_else(|| a.iter().find(|(k,_)| k.eq_ignore_ascii_case(&ob.column)).map(|(_,v)| v)).unwrap_or(&Value::Null);
                let bv = b.get(&ob.column).or_else(|| b.iter().find(|(k,_)| k.eq_ignore_ascii_case(&ob.column)).map(|(_,v)| v)).unwrap_or(&Value::Null);
                match (av, bv) {
                    (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                    (Value::Null, _) => std::cmp::Ordering::Greater,
                    (_, Value::Null) => std::cmp::Ordering::Less,
                    _ => { let ord = av.compare(bv); if ob.asc { ord } else { ord.reverse() } }
                }
            });
        }

        // 4. Projection y LIMIT
        match projection {
            Projection::Star => {
                if let Some(lim) = limit { filtered.truncate(lim); }
                let cols: Vec<String> = cat_table.columns.iter().map(|c| c.name.clone()).collect();
                Ok(ExecutionResult::Selected { columns: cols, rows: filtered })
            }
            Projection::CountStar => {
                let cnt = filtered.len() as i64;
                let mut row = Row::new();
                row.insert("COUNT(*)".into(), Value::Int(cnt));
                Ok(ExecutionResult::Selected { columns: vec!["COUNT(*)".into()], rows: vec![row] })
            }
            Projection::CountCol(col) => {
                // COUNT(col) cuenta no-null
                let cnt = filtered.iter().filter(|r| {
                    let v = r.get(&col).or_else(|| r.iter().find(|(k,_)| k.eq_ignore_ascii_case(&col)).map(|(_,v)| v));
                    !matches!(v, Some(Value::Null) | None)
                }).count() as i64;
                let mut row = Row::new();
                row.insert(format!("COUNT({})", col), Value::Int(cnt));
                Ok(ExecutionResult::Selected { columns: vec![format!("COUNT({})", col)], rows: vec![row] })
            }
            Projection::Columns(cols) => {
                // Si no hay group by pero proyección incluye COUNT, tratar como CountStar
                if cols.iter().any(|c| c.to_uppercase().contains("COUNT")) {
                    // Simplificado: retorna count
                    let cnt = filtered.len() as i64;
                    let mut row = Row::new();
                    row.insert("COUNT(*)".into(), Value::Int(cnt));
                    return Ok(ExecutionResult::Selected { columns: vec!["COUNT(*)".into()], rows: vec![row] });
                }
                // Filtrar columnas válidas y proyectar
                if let Some(lim) = limit { filtered.truncate(lim); }
                // Validar columnas existen
                for c in &cols {
                    if cat_table.column_index(c).is_none() {
                        return Err(format!("Column '{}' not found", c));
                    }
                }
                let mut projected: Vec<Row> = Vec::new();
                for row in filtered {
                    let mut new_row = Row::new();
                    for c in &cols {
                        let v = row.get(c).or_else(|| row.iter().find(|(k,_)| k.eq_ignore_ascii_case(c)).map(|(_,v)| v)).cloned().unwrap_or(Value::Null);
                        new_row.insert(c.clone(), v);
                    }
                    projected.push(new_row);
                }
                Ok(ExecutionResult::Selected { columns: cols, rows: projected })
            }
        }
    }

    // Helper para inserts directos (usado por demo y tests) - bypass SQL parser
    pub fn insert_row(&self, table: &str, row: Row) -> Result<(), String> {
        let cat_table = {
            let cat = self.catalog.read().unwrap();
            cat.get_table(table).cloned().ok_or(format!("Table '{}' not found", table))?
        };
        // Validar columnas y manejo IDENTITY
        for (col, val) in &row {
            if cat_table.column_index(col).is_none() {
                return Err(format!("Column '{}' not found in '{}'", col, table));
            }
            // Verificar GENERATED ALWAYS no permite valor explícito (excepto DEFAULT)
            if let Some(idx) = cat_table.column_index(col) {
                let col_def = &cat_table.columns[idx];
                if let Some(id) = &col_def.identity {
                    if id.kind == argentum_common::catalog::IdentityKind::Always {
                        // Si valor es DEFAULT, se permite (se generará)
                        let is_default = matches!(val, Value::Text(s) if s.eq_ignore_ascii_case("DEFAULT"));
                        if !is_default {
                            // Si es ALWAYS y se da valor explícito, error
                            // Pero para compatibilidad con tests que insertan id explícito sin identity, solo error si es identity
                            // Verificar si valor es no-null y no DEFAULT
                            if !matches!(val, Value::Null) {
                                // Permitir si es Value::Int pero es ALWAYS? Debe error
                                // Para v1, solo error si es Int y se intenta insertar explícito en ALWAYS
                                // Sin embargo, tests que usan identity con AGREGAR sin id no pasarán por aquí (no tienen col)
                                // Así que si row contiene id para identity Always, error
                                return Err(format!("No se puede insertar valor explícito en columna GENERATED ALWAYS AS IDENTITY '{}' / Cannot insert explicit value into GENERATED ALWAYS column '{}' (use DEFAULT o omita columna)", col, col));
                            }
                        }
                    }
                }
            }
        }
        // Rellenar y generar IDENTITY para columnas faltantes
        let mut full_row = Row::new();
        for col_def in &cat_table.columns {
            if let Some(v) = row.get(&col_def.name) {
                // Manejar DEFAULT -> generar
                if matches!(v, Value::Text(s) if s.eq_ignore_ascii_case("DEFAULT")) && col_def.identity.is_some() {
                    let gen = self.next_identity(table, &col_def.name);
                    full_row.insert(col_def.name.clone(), Value::Int(gen));
                } else {
                    // Si es identidad BY DEFAULT con valor explícito, avanzar secuencia
                    if col_def.identity.is_some() {
                        if let Value::Int(i) = v { self.advance_sequence_for_explicit(table, &col_def.name, *i); }
                    }
                    full_row.insert(col_def.name.clone(), v.clone());
                }
            } else {
                let found = row.iter().find(|(k,_)| k.eq_ignore_ascii_case(&col_def.name)).map(|(_,v)| v).cloned();
                if let Some(v) = found {
                    if matches!(&v, Value::Text(s) if s.eq_ignore_ascii_case("DEFAULT")) && col_def.identity.is_some() {
                        let gen = self.next_identity(table, &col_def.name);
                        full_row.insert(col_def.name.clone(), Value::Int(gen));
                    } else {
                        if col_def.identity.is_some() {
                            if let Value::Int(i) = &v { self.advance_sequence_for_explicit(table, &col_def.name, *i); }
                        }
                        full_row.insert(col_def.name.clone(), v);
                    }
                } else if let Some(id_spec) = &col_def.identity {
                    let gen = self.next_identity(table, &col_def.name);
                    full_row.insert(col_def.name.clone(), Value::Int(gen));
                    let _ = id_spec;
                } else {
                    full_row.insert(col_def.name.clone(), Value::Null);
                }
            }
        }
        let mut data = self.data.write().unwrap();
        let vec = data.get_mut(&table.to_lowercase()).ok_or(format!("Table '{}' not found", table))?;
        vec.push(full_row);
        Ok(())
    }
}

/// Optimizador cost-based (skeleton)
pub struct Optimizer;

impl Optimizer {
    pub fn optimize(plan: LogicalPlan, trinity: &TrinityIndex) -> LogicalPlan {
        if let LogicalPlan::TrinityScan { ref params, .. } = plan {
            let _cost = trinity.estimate_cost(params);
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimizer_passthrough() {
        use argentum_storage::buffer_pool::BufferPool;
        use argentum_storage::wal::WalManager;
        use std::sync::Arc;
        let bp = Arc::new(BufferPool::new(16));
        let wal = WalManager::open(std::env::temp_dir().join("argentum_engine_test.wal").to_str().unwrap()).unwrap();
        let idx = TrinityIndex::new(bp, wal);
        let plan = LogicalPlan::SeqScan { table: "productos".into() };
        let out = Optimizer::optimize(plan, &idx);
        assert!(matches!(out, LogicalPlan::SeqScan { .. }));
    }

    #[test]
    fn test_database_crud() {
        let db = Database::new_in_memory();
        // CREATE
        let p = parser::parse("CREATE TABLE productos (id INT PRIMARY KEY, nombre TEXT, categoria_id INT)").unwrap();
        assert!(db.execute(p).is_ok());
        // INSERT via API
        let mut row = Row::new();
        row.insert("id".into(), Value::Int(1));
        row.insert("nombre".into(), Value::Text("Zapatilla".into()));
        row.insert("categoria_id".into(), Value::Int(5));
        assert!(db.insert_row("productos", row.clone()).is_ok());
        let mut row2 = Row::new();
        row2.insert("id".into(), Value::Int(2));
        row2.insert("nombre".into(), Value::Text("Bota".into()));
        row2.insert("categoria_id".into(), Value::Int(5));
        assert!(db.insert_row("productos", row2).is_ok());
        // SELECT * WHERE
        let p = parser::parse("SELECT * FROM productos WHERE categoria_id = 5 ORDER BY nombre ASC LIMIT 10").unwrap();
        let res = db.execute(p).unwrap();
        match res {
            ExecutionResult::Selected { rows, .. } => assert_eq!(rows.len(), 2),
            _ => panic!(),
        }
    }

    #[test]
    fn test_group_by_count() {
        let db = Database::new_in_memory();
        db.execute(parser::parse("CREATE TABLE t (id INT, cat INT)").unwrap()).unwrap();
        for i in 0..6 {
            let mut r = Row::new();
            r.insert("id".into(), Value::Int(i));
            r.insert("cat".into(), Value::Int(if i < 4 { 5 } else { 6 }));
            db.insert_row("t", r).unwrap();
        }
        let p = parser::parse("SELECT cat, COUNT(*) FROM t GROUP BY cat ORDER BY COUNT(*) DESC LIMIT 10").unwrap();
        let res = db.execute(p).unwrap();
        match res {
            ExecutionResult::Selected { rows, columns } => {
                assert_eq!(columns, vec!["cat", "COUNT(*)"]);
                assert_eq!(rows.len(), 2);
                // cat 5 count 4 debe estar primero
                let first_cat = rows[0].get("cat").unwrap();
                assert_eq!(first_cat, &Value::Int(5));
                assert_eq!(rows[0].get("COUNT(*)").unwrap(), &Value::Int(4));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_alter_update() {
        let db = Database::new_in_memory();
        db.execute(parser::parse("CREATE TABLE t (id INT, nombre TEXT)").unwrap()).unwrap();
        let mut r = Row::new();
        r.insert("id".into(), Value::Int(1));
        r.insert("nombre".into(), Value::Text("viejo".into()));
        db.insert_row("t", r).unwrap();
        // ADD COLUMN
        db.execute(parser::parse("ALTER TABLE t ADD COLUMN precio FLOAT").unwrap()).unwrap();
        // UPDATE
        db.execute(parser::parse("UPDATE t SET nombre = 'nuevo' WHERE id = 1").unwrap()).unwrap();
        // SELECT verify
        let res = db.execute(parser::parse("SELECT * FROM t WHERE id = 1").unwrap()).unwrap();
        match res {
            ExecutionResult::Selected { rows, .. } => assert_eq!(rows[0].get("nombre").unwrap(), &Value::Text("nuevo".into())),
            _ => panic!(),
        }
        // DROP COLUMN
        db.execute(parser::parse("ALTER TABLE t DROP COLUMN precio").unwrap()).unwrap();
        assert!(db.catalog.read().unwrap().get_table("t").unwrap().column_index("precio").is_none());
    }

    #[test]
    fn test_identity_always() {
        let db = Database::new_in_memory();
        // CREATE con GENERATED ALWAYS AS IDENTITY
        let p = parser::parse("CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, nombre TEXT)").unwrap();
        assert!(db.execute(p).is_ok());
        // INSERT sin id -> debe generar 1, 2
        assert!(db.execute(parser::parse("INSERT INTO t (nombre) VALUES ('a')").unwrap()).is_ok());
        assert!(db.execute(parser::parse("INSERT INTO t (nombre) VALUES ('b')").unwrap()).is_ok());
        // INSERT con DEFAULT también genera
        assert!(db.execute(parser::parse("INSERT INTO t (id, nombre) VALUES (DEFAULT, 'c')").unwrap()).is_ok());
        // SELECT verificar ids 1,2,3
        let res = db.execute(parser::parse("SELECT * FROM t ORDER BY id ASC").unwrap()).unwrap();
        match res {
            ExecutionResult::Selected { rows, .. } => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].get("id").unwrap(), &Value::Int(1));
                assert_eq!(rows[1].get("id").unwrap(), &Value::Int(2));
                assert_eq!(rows[2].get("id").unwrap(), &Value::Int(3));
            }
            _ => panic!(),
        }
        // INSERT con id explícito en ALWAYS debe fallar
        let err = db.execute(parser::parse("INSERT INTO t (id, nombre) VALUES (99, 'x')").unwrap()).unwrap_err();
        assert!(err.contains("GENERATED ALWAYS") || err.contains("ALWAYS"));
        // Español
        let db2 = Database::new_in_memory();
        assert!(db2.execute(parser::parse("CREA TABLA t2 (id SERIAL PRIMARY KEY, nombre TEXT)").unwrap()).is_ok());
        assert!(db2.execute(parser::parse("AGREGAR EN t2 (nombre) VALORES ('hola')").unwrap()).is_ok());
        let res2 = db2.execute(parser::parse("ELIGE * DE t2 LIMITE 10").unwrap()).unwrap();
        match res2 {
            ExecutionResult::Selected { rows, .. } => assert_eq!(rows[0].get("id").unwrap(), &Value::Int(1)),
            _ => panic!(),
        }
    }

    #[test]
    fn test_identity_by_default() {
        let db = Database::new_in_memory();
        db.execute(parser::parse("CREATE TABLE t (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, nombre TEXT)").unwrap()).unwrap();
        db.execute(parser::parse("INSERT INTO t (nombre) VALUES ('a')").unwrap()).unwrap();
        // Con BY DEFAULT se permite insertar explícito
        assert!(db.execute(parser::parse("INSERT INTO t (id, nombre) VALUES (99, 'b')").unwrap()).is_ok());
        // Siguiente autoincrement debe ser 100
        db.execute(parser::parse("INSERT INTO t (nombre) VALUES ('c')").unwrap()).unwrap();
        let res = db.execute(parser::parse("SELECT * FROM t ORDER BY id ASC").unwrap()).unwrap();
        match res {
            ExecutionResult::Selected { rows, .. } => {
                assert_eq!(rows.len(), 3);
                // ids: 1, 99, 100
                assert_eq!(rows[0].get("id").unwrap(), &Value::Int(1));
                assert_eq!(rows[1].get("id").unwrap(), &Value::Int(99));
                assert_eq!(rows[2].get("id").unwrap(), &Value::Int(100));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_describe_table() {
        let db = Database::new_in_memory();
        db.execute(parser::parse("CREATE TABLE productos (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, nombre TEXT, precio FLOAT)").unwrap()).unwrap();
        for sql in &["DESCRIBE TABLE productos", "MUESTRA TABLA productos", "ESTRUCTURA productos", "MUESTRA ESTRUCTURA DE TABLA productos", "SHOW TABLE productos"] {
            let res = db.execute(parser::parse(sql).unwrap()).unwrap();
            match res {
                ExecutionResult::Selected { columns, rows } => {
                    assert_eq!(columns[0], "Columna");
                    assert_eq!(rows.len(), 3);
                    let id_row = rows.iter().find(|r| r.get("Columna").unwrap() == &Value::Text("id".into())).unwrap();
                    assert_eq!(id_row.get("Clave Primaria").unwrap(), &Value::Text("Sí".into()));
                    assert!(id_row.get("Identidad").unwrap().to_string().contains("GENERADO SIEMPRE"));
                    assert_eq!(id_row.get("Autoincremental").unwrap(), &Value::Text("Sí / Yes".into()));
                }
                _ => panic!("failed for {}", sql),
            }
        }
        // Español
        let res_es = db.execute(parser::parse("MUESTRA ESTRUCTURA DE TABLA productos").unwrap()).unwrap();
        assert!(matches!(res_es, ExecutionResult::Selected { .. }));
        // Tabla no existe
        assert!(db.execute(parser::parse("DESCRIBE TABLE noexiste").unwrap()).is_err());
    }
}

