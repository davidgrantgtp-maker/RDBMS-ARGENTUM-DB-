//! Integration tests para DatabaseManager (multi-base).
//! Ejecuta: cargo test -p argentum-engine --test integration_databases -- --nocapture

use argentum_engine::{DatabaseManager, parser};
use std::fs;

fn fresh_dir(name: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("argentum_test_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir.to_string_lossy().to_string()
}

#[test]
fn manager_default_exists() {
    let dir = fresh_dir("default");
    let mgr = DatabaseManager::new(&dir);
    assert!(mgr.show_databases().contains(&"default".to_string()));
    assert_eq!(mgr.current_db(), "default");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn create_use_drop_database() {
    let dir = fresh_dir("cud");
    let mut mgr = DatabaseManager::new(&dir);

    // CREATE
    assert!(mgr.create_database("tienda").is_ok());
    assert!(mgr.show_databases().contains(&"tienda".to_string()));

    // No se puede duplicar
    assert!(mgr.create_database("tienda").is_err());

    // USE
    assert!(mgr.use_database("tienda").is_ok());
    assert_eq!(mgr.current_db(), "tienda");

    // DROP
    assert!(mgr.drop_database("tienda").is_ok());
    assert!(!mgr.show_databases().contains(&"tienda".to_string()));
    // Tras drop, current vuelve a default
    assert_eq!(mgr.current_db(), "default");

    // DROP 'default' no permitido
    assert!(mgr.drop_database("default").is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn use_unknown_database_fails() {
    let dir = fresh_dir("unknown");
    let mut mgr = DatabaseManager::new(&dir);
    let res = mgr.use_database("nope");
    assert!(res.is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn invalid_names_rejected() {
    let dir = fresh_dir("invalid");
    let mut mgr = DatabaseManager::new(&dir);
    assert!(mgr.create_database("").is_err());
    assert!(mgr.create_database("a/b").is_err());
    assert!(mgr.create_database("a\\b").is_err());
    assert!(mgr.create_database(".").is_err());
    assert!(mgr.create_database("..").is_err());
    assert!(mgr.create_database(&"x".repeat(65)).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn persistence_across_instances() {
    let dir = fresh_dir("persist");

    // Sesión 1: crear base. No creamos tablas para evitar el parser manual del Catalog
    // (que tiene un bug OOB pre-existente, fuera del alcance de la Ruta B).
    {
        let mut mgr = DatabaseManager::new(&dir);
        mgr.create_database("app").unwrap();
        mgr.use_database("app").unwrap();
    }

    // Sesión 2: reabrir manager, la base debe seguir ahí.
    {
        let mgr = DatabaseManager::new(&dir);
        assert!(mgr.show_databases().contains(&"app".to_string()));
        // La base se monta en memoria desde el manifest
        let mut mgr = mgr;
        assert!(mgr.use_database("app").is_ok());
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn show_databases_marks_current() {
    let dir = fresh_dir("show");
    let mut mgr = DatabaseManager::new(&dir);
    mgr.create_database("a").unwrap();
    mgr.create_database("b").unwrap();
    mgr.use_database("b").unwrap();
    let dbs = mgr.show_databases();
    assert!(dbs.contains(&"default".to_string()));
    assert!(dbs.contains(&"a".to_string()));
    assert!(dbs.contains(&"b".to_string()));
    assert_eq!(mgr.current_db(), "b");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parser_recognizes_bilingual_db_commands() {
    // EN
    assert!(matches!(parser::parse("CREATE DATABASE foo").unwrap(), argentum_engine::LogicalPlan::CreateDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("DROP DATABASE foo").unwrap(), argentum_engine::LogicalPlan::DropDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("USE DATABASE foo").unwrap(), argentum_engine::LogicalPlan::UseDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("SHOW DATABASES").unwrap(), argentum_engine::LogicalPlan::ShowDatabases));

    // ES
    assert!(matches!(parser::parse("CREA BASE foo").unwrap(), argentum_engine::LogicalPlan::CreateDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("CREA BASE DE DATOS foo").unwrap(), argentum_engine::LogicalPlan::CreateDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("BORRA BASE foo").unwrap(), argentum_engine::LogicalPlan::DropDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("ELIMINA BASE foo").unwrap(), argentum_engine::LogicalPlan::DropDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("USA BASE foo").unwrap(), argentum_engine::LogicalPlan::UseDatabase { ref name } if name == "foo"));
    assert!(matches!(parser::parse("MUESTRA BASES").unwrap(), argentum_engine::LogicalPlan::ShowDatabases));
}

#[test]
fn use_then_drop_then_recreate_works() {
    let dir = fresh_dir("recreate");
    let mut mgr = DatabaseManager::new(&dir);
    mgr.create_database("x").unwrap();
    mgr.use_database("x").unwrap();
    assert!(mgr.drop_database("x").is_ok());
    // Tras drop, podemos crearla de nuevo
    assert!(mgr.create_database("x").is_ok());
    let _ = fs::remove_dir_all(&dir);
}
