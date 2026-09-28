//! aether-common: tipos fundamentales compartidos por todo el motor.
//! Sin dependencias externas para garantizar compilación offline y layout estable.

pub type PageId = u32;
pub type SegmentId = u32;
pub type TxnId = u64;
pub type Lsn = u64; // Log Sequence Number

pub const PAGE_SIZE: usize = 16 * 1024; // 16KB
pub const INVALID_PAGE_ID: PageId = u32::MAX;
pub const INVALID_TXN_ID: TxnId = 0;

/// Identificador físico de una tupla (PageId + slot)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rid {
    pub page_id: PageId,
    pub slot_id: u16,
}

/// Header MVCC presente en cada tupla
#[derive(Debug, Clone, Copy)]
pub struct TupleHeader {
    pub xmin: TxnId, // txn que creó la tupla
    pub xmax: TxnId, // txn que borró la tupla (0 = viva)
    pub ctid: Rid,   // puntero a versión siguiente (update chain)
}

impl TupleHeader {
    pub fn is_visible(&self, _snapshot_xmin: TxnId, snapshot_xmax: TxnId, active_txns: &[TxnId]) -> bool {
        // SSI simplificado: visible si xmin commiteado y xmax no visible
        if self.xmin == INVALID_TXN_ID {
            return false;
        }
        // xmin debe ser < snapshot_xmax y no estar activo
        if self.xmin >= snapshot_xmax || active_txns.contains(&self.xmin) {
            return false;
        }
        // xmax == 0 => viva. Si xmax visible => borrada
        if self.xmax != 0 && self.xmax < snapshot_xmax && !active_txns.contains(&self.xmax) {
            return false;
        }
        // Adicional: xmin >= snapshot_xmin check para snapshots
        self.xmin < snapshot_xmax
    }
}

/// Error unificado del motor
#[derive(Debug)]
pub enum AetherError {
    Io(std::io::Error),
    Corruption(String),
    TransactionAborted(String),
    PageFull,
    SlotNotFound,
}

impl From<std::io::Error> for AetherError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub mod auth;
pub mod catalog;

pub type Result<T> = std::result::Result<T, AetherError>;
