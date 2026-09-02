//! crates/aether-storage/src/wal/record.rs:12 - Formato físico WAL (ARIES)

use aether_common::{Lsn, PageId, TxnId};

#[derive(Debug, Clone)]
pub enum WalRecord {
    /// Inicio de transacción
    Begin { txn_id: TxnId, lsn: Lsn },
    /// Commit
    Commit { txn_id: TxnId, lsn: Lsn },
    /// Abort
    Abort { txn_id: TxnId, lsn: Lsn },
    /// Inserción híbrida TRINITY: atomiza row + vector + texto + grafo
    TrinityInsert {
        txn_id: TxnId,
        lsn: Lsn,
        page_id: PageId,
        slot_id: u16,
        payload: Vec<u8>, // row+P Q+BM25+CSR serializado (ver page::TrinityPayload)
        prev_lsn: Lsn,    // para undo chain
    },
    /// Delete lógico MVCC
    TrinityDelete {
        txn_id: TxnId,
        lsn: Lsn,
        page_id: PageId,
        slot_id: u16,
        prev_lsn: Lsn,
    },
    /// Checkpoint
    Checkpoint { lsn: Lsn, active_txns: Vec<TxnId> },
    /// DDL
    CreateTable { txn_id: TxnId, lsn: Lsn, table: String, columns: Vec<(String, String)> },
    DropTable { txn_id: TxnId, lsn: Lsn, table: String },
    AlterTableAddColumn { txn_id: TxnId, lsn: Lsn, table: String, column: String, col_type: String },
    AlterTableDropColumn { txn_id: TxnId, lsn: Lsn, table: String, column: String },
}

impl WalRecord {
    pub fn lsn(&self) -> Lsn {
        match self {
            Self::Begin { lsn, .. } => *lsn,
            Self::Commit { lsn, .. } => *lsn,
            Self::Abort { lsn, .. } => *lsn,
            Self::TrinityInsert { lsn, .. } => *lsn,
            Self::TrinityDelete { lsn, .. } => *lsn,
            Self::Checkpoint { lsn, .. } => *lsn,
            Self::CreateTable { lsn, .. } => *lsn,
            Self::DropTable { lsn, .. } => *lsn,
            Self::AlterTableAddColumn { lsn, .. } => *lsn,
            Self::AlterTableDropColumn { lsn, .. } => *lsn,
        }
    }

    pub fn txn_id(&self) -> Option<TxnId> {
        match self {
            Self::Begin { txn_id, .. } => Some(*txn_id),
            Self::Commit { txn_id, .. } => Some(*txn_id),
            Self::Abort { txn_id, .. } => Some(*txn_id),
            Self::TrinityInsert { txn_id, .. } => Some(*txn_id),
            Self::TrinityDelete { txn_id, .. } => Some(*txn_id),
            Self::CreateTable { txn_id, .. } => Some(*txn_id),
            Self::DropTable { txn_id, .. } => Some(*txn_id),
            Self::AlterTableAddColumn { txn_id, .. } => Some(*txn_id),
            Self::AlterTableDropColumn { txn_id, .. } => Some(*txn_id),
            Self::Checkpoint { .. } => None,
        }
    }

    /// Serialización binaria simple: [u8 tag][u64 lsn][u64 txn][payload...][u32 crc]
    /// Producción: usar rkyv / bincode + crc32c
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        match self {
            Self::Begin { txn_id, lsn } => {
                out.push(0x01);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
            }
            Self::Commit { txn_id, lsn } => {
                out.push(0x02);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
            }
            Self::TrinityInsert { txn_id, lsn, page_id, slot_id, payload, prev_lsn } => {
                out.push(0x10);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&page_id.to_le_bytes());
                out.extend_from_slice(&slot_id.to_le_bytes());
                out.extend_from_slice(&prev_lsn.to_le_bytes());
                out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                out.extend_from_slice(payload);
            }
            Self::TrinityDelete { txn_id, lsn, page_id, slot_id, prev_lsn } => {
                out.push(0x11);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&page_id.to_le_bytes());
                out.extend_from_slice(&slot_id.to_le_bytes());
                out.extend_from_slice(&prev_lsn.to_le_bytes());
            }
            Self::CreateTable { txn_id, lsn, table, columns } => {
                out.push(0x20);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&(table.len() as u32).to_le_bytes());
                out.extend_from_slice(table.as_bytes());
                out.extend_from_slice(&(columns.len() as u32).to_le_bytes());
                for (cname, ctype) in columns {
                    out.extend_from_slice(&(cname.len() as u32).to_le_bytes());
                    out.extend_from_slice(cname.as_bytes());
                    out.extend_from_slice(&(ctype.len() as u32).to_le_bytes());
                    out.extend_from_slice(ctype.as_bytes());
                }
            }
            Self::DropTable { txn_id, lsn, table } => {
                out.push(0x21);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&(table.len() as u32).to_le_bytes());
                out.extend_from_slice(table.as_bytes());
            }
            Self::AlterTableAddColumn { txn_id, lsn, table, column, col_type } => {
                out.push(0x22);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&(table.len() as u32).to_le_bytes());
                out.extend_from_slice(table.as_bytes());
                out.extend_from_slice(&(column.len() as u32).to_le_bytes());
                out.extend_from_slice(column.as_bytes());
                out.extend_from_slice(&(col_type.len() as u32).to_le_bytes());
                out.extend_from_slice(col_type.as_bytes());
            }
            Self::AlterTableDropColumn { txn_id, lsn, table, column } => {
                out.push(0x23);
                out.extend_from_slice(&lsn.to_le_bytes());
                out.extend_from_slice(&txn_id.to_le_bytes());
                out.extend_from_slice(&(table.len() as u32).to_le_bytes());
                out.extend_from_slice(table.as_bytes());
                out.extend_from_slice(&(column.len() as u32).to_le_bytes());
                out.extend_from_slice(column.as_bytes());
            }
            _ => {}
        }
        // crc32c placeholder
        let crc = crc32fast(&out);
        out.extend_from_slice(&crc.to_le_bytes());
    }
}

fn crc32fast(data: &[u8]) -> u32 {
    // Evita dependencia externa en skeleton: simple FNV
    let mut h: u32 = 2166136261;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}
