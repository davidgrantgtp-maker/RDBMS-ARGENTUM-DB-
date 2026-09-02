//! tests/integration_wal.rs - WAL crash recovery y MVCC
//! Ejecuta: cargo test -p aether-storage --test integration_wal -- --nocapture

use argentum_common::{TupleHeader, Rid, PAGE_SIZE};
use argentum_storage::page::{Page, TrinityPayload};
use argentum_storage::wal::{WalManager, WalRecord};

#[test]
fn wal_crash_recovery_replay() {
    let path = std::env::temp_dir().join(format!("argentum_wal_recovery_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let wal = WalManager::open(path.to_str().unwrap()).unwrap();

    // Simula 3 transacciones con TrinityInserts
    let lsn1 = wal.append(WalRecord::Begin { txn_id: 1, lsn: 0 }).unwrap();
    let payload = {
        let mut buf = Vec::new();
        TrinityPayload::serialize(b"row1", &[1,2,3], b"bm25", b"", &mut buf);
        buf
    };
    let lsn2 = wal.append(WalRecord::TrinityInsert {
        txn_id: 1, lsn: 0, page_id: 1, slot_id: 0, payload: payload.clone(), prev_lsn: lsn1,
    }).unwrap();
    let lsn3 = wal.append_sync(WalRecord::Commit { txn_id: 1, lsn: 0 }).unwrap();

    assert!(lsn3 > lsn2);
    assert_eq!(wal.flushed_lsn(), lsn3, "commit debe hacer fsync");

    // Simula crash: reabre WAL y verifica que next_lsn continúa monotónico
    drop(wal);
    let wal2 = WalManager::open(path.to_str().unwrap()).unwrap();
    let lsn4 = wal2.append(WalRecord::Begin { txn_id: 2, lsn: 0 }).unwrap();
    assert!(lsn4 > lsn3, "LSN debe ser monotónico tras recovery");

    // Replay stub (skeleton no decodifica, solo verifica que no crashee)
    wal2.replay(|_rec| {}).unwrap();

    let _ = std::fs::remove_file(&path);
    println!("[wal] recovery LSN monotonic: {} -> {} -> {}", lsn1, lsn3, lsn4);
}

#[test]
fn wal_group_commit_batches() {
    let path = std::env::temp_dir().join(format!("argentum_group_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let wal = WalManager::open(path.to_str().unwrap()).unwrap();

    // 100 appends sin fsync + 1 group_commit
    for i in 0..100 {
        wal.append(WalRecord::Begin { txn_id: i, lsn: 0 }).unwrap();
    }
    let flushed_before = wal.flushed_lsn();
    wal.group_commit().unwrap();
    let flushed_after = wal.flushed_lsn();
    assert!(flushed_after > flushed_before, "group_commit debe avanzar flushed_lsn");
    println!("[wal] group_commit: {} -> {}", flushed_before, flushed_after);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn page_mvcc_visibility_with_wal() {
    let mut page = Page::new(1);
    // Txn 10 inserta, Txn 20 borra
    let hdr = TupleHeader { xmin: 10, xmax: 0, ctid: Rid { page_id: 1, slot_id: 0 } };
    let mut buf = Vec::new();
    TrinityPayload::serialize(b"row", &[0u8; 32], b"tok", b"", &mut buf);
    let sid = page.insert_tuple(hdr, &buf).unwrap();

    // Snapshot (xmin=0, xmax=15) ve la tupla (xmin 10 <15, no activo)
    assert!(page.is_visible(sid, 0, 15, &[]));
    // Snapshot (xmin=0, xmax=5) no ve xmin 10
    assert!(!page.is_visible(sid, 0, 5, &[]));
    // Txn 10 aún activo => no visible
    assert!(!page.is_visible(sid, 0, 15, &[10]));

    page.delete_tuple(sid, 20);
    // Después de delete, snapshot xmax=30 ve xmax 20 commiteado => no visible
    assert!(!page.is_visible(sid, 0, 30, &[]));
    // Pero si deleter 20 está activo, sí visible
    assert!(page.is_visible(sid, 0, 30, &[20]));

    println!("[mvcc] visibility checks passed");
}

#[test]
fn page_trinity_payload_roundtrip() {
    let mut buf = Vec::new();
    TrinityPayload::serialize(b"row_data_pax", &[1,2,3,4], b"posting1", b"\x00\x01", &mut buf);
    assert!(buf.len() < PAGE_SIZE);
    let decoded = TrinityPayload::deserialize(&buf).unwrap();
    assert_eq!(decoded.row_data, b"row_data_pax");
    assert_eq!(decoded.pq_vector, &[1,2,3,4]);
    assert_eq!(decoded.bm25_postings, b"posting1");
    println!("[page] payload roundtrip {} bytes ok", buf.len());
}

