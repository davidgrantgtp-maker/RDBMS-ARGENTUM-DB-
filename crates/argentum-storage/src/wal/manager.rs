//! crates/aether-storage/src/wal/manager.rs:18 - WAL Manager con group commit
//! Garantiza WAL-before-data y fsync por LSN. Un solo writer secuencial.

use argentum_common::Lsn;
use crate::wal::record::WalRecord;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::wal::record::WalRecord as WalRecordReexport;

/// WAL Manager: append-only, group commit cada 1ms o 32KB
pub struct WalManager {
    file: Mutex<File>,
    next_lsn: AtomicU64,
    /// LSN hasta donde se hizo fsync durable
    flushed_lsn: AtomicU64,
    /// Buffer de grupo (skeleton: Vec<u8> protegido por Mutex)
    group_buffer: Mutex<Vec<u8>>,
}

impl WalManager {
    pub fn open(path: &str) -> std::io::Result<Arc<Self>> {
        let file = OpenOptions::new().create(true).append(true).read(true).open(path)?;
        // Recuperar next_lsn desde tamaño del archivo (skeleton)
        let len = file.metadata()?.len();
        let next = if len == 0 { 1 } else { len + 1 };
        Ok(Arc::new(Self {
            file: Mutex::new(file),
            next_lsn: AtomicU64::new(next),
            flushed_lsn: AtomicU64::new(next - 1),
            group_buffer: Mutex::new(Vec::with_capacity(32 * 1024)),
        }))
    }

    /// Reserva LSN monotónico atómico
    pub fn next_lsn(&self) -> Lsn {
        self.next_lsn.fetch_add(1, Ordering::SeqCst)
    }

    /// Append sin fsync (para group commit). Retorna LSN asignado.
    /// Caller debe llamar `flush_up_to` o `group_commit` para durabilidad.
    pub fn append(&self, mut rec: WalRecord) -> std::io::Result<Lsn> {
        let lsn = match &mut rec {
            WalRecord::Begin { lsn, .. } => { *lsn = self.next_lsn(); *lsn }
            WalRecord::Commit { lsn, .. } => { *lsn = self.next_lsn(); *lsn }
            WalRecord::TrinityInsert { lsn, .. } => { *lsn = self.next_lsn(); *lsn }
            WalRecord::TrinityDelete { lsn, .. } => { *lsn = self.next_lsn(); *lsn }
            _ => self.next_lsn(),
        };
        let mut buf = Vec::new();
        rec.encode(&mut buf);

        // Append al archivo + buffer de grupo
        {
            let mut f = self.file.lock().unwrap();
            f.write_all(&buf)?;
            // No fsync aún - group commit lo hará
        }
        {
            let mut gb = self.group_buffer.lock().unwrap();
            gb.extend_from_slice(&buf);
        }
        Ok(lsn)
    }

    /// Append + fsync síncrono (para COMMIT - debe ser durable antes de ack al cliente)
    pub fn append_sync(&self, rec: WalRecord) -> std::io::Result<Lsn> {
        let lsn = self.append(rec)?;
        self.flush_up_to(lsn)?;
        Ok(lsn)
    }

    /// Fuerza fsync hasta LSN dado (ARIES WAL-before-data)
    pub fn flush_up_to(&self, lsn: Lsn) -> std::io::Result<()> {
        let flushed = self.flushed_lsn.load(Ordering::Acquire);
        if lsn <= flushed {
            return Ok(());
        }
        let f = self.file.lock().unwrap();
        f.sync_all()?;
        self.flushed_lsn.store(lsn, Ordering::Release);
        Ok(())
    }

    /// Group commit: flush colectivo cada N ms (llamado por background thread)
    pub fn group_commit(&self) -> std::io::Result<()> {
        let next = self.next_lsn.load(Ordering::Acquire) - 1;
        self.flush_up_to(next)
    }

    /// Recovery: replay WAL desde checkpoint (skeleton scan secuencial)
    pub fn replay<F>(&self, mut apply: F) -> std::io::Result<()>
    where
        F: FnMut(WalRecord),
    {
        // TODO: leer archivo, decodificar records, filtrar por LSN, re-aplicar
        // Skeleton no implementa decode, solo estructura
        let _ = &mut apply;
        Ok(())
    }

    pub fn flushed_lsn(&self) -> Lsn {
        self.flushed_lsn.load(Ordering::Acquire)
    }
}

// Ejemplo de uso transaccional atómico:
// wal.append(TrinityInsert{...})?; // WAL-before-data
// page.insert_tuple(...);           // luego modifica página en BufferPool
// page.header.lsn = lsn;            // marca página con LSN
// // En commit: wal.append_sync(Commit{txn})?; // fsync antes de responder OK

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_append_and_flush() {
        let dir = std::env::temp_dir().join("argentum_wal_test.log");
        let _ = std::fs::remove_file(&dir);
        let wal = WalManager::open(dir.to_str().unwrap()).unwrap();
        let lsn = wal.append(WalRecord::Begin { txn_id: 1, lsn: 0 }).unwrap();
        assert!(lsn >= 1);
        wal.flush_up_to(lsn).unwrap();
        assert_eq!(wal.flushed_lsn(), lsn);
        let _ = std::fs::remove_file(&dir);
    }
}

