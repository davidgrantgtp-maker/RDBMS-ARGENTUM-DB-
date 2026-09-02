//! crates/aether-storage/src/page.rs:30 - Layout físico de página TRINITY 16KB
//! Diseño PAX + Index-Organized. Inspirado en PostgreSQL PageHeader + DuckDB PAX.

use argentum_common::{PageId, Lsn, PAGE_SIZE, TupleHeader, TxnId};
use std::mem::size_of;

pub type SlotId = u16;

/// PageHeader: 32 bytes, alineado. Compatible con mmap.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PageHeader {
    pub page_id: PageId,
    pub lsn: Lsn,              // último LSN que modificó la página (para ARIES)
    pub checksum: u32,         // crc32c del contenido (sin header)
    pub flags: u16,            // bitmask: LEAF | INTERNAL | TRINITY
    pub lower: u16,            // offset inicio espacio libre
    pub upper: u16,            // offset fin espacio libre
    pub special: u16,          // offset área especial (HNSW/CSR)
    pub num_slots: u16,        // slots activos
    pub free_slots: u16,
    pub _padding: [u8; 2],
}

impl PageHeader {
    pub const SIZE: usize = size_of::<Self>(); // 32
    pub const FLAG_LEAF: u16 = 1 << 0;
    pub const FLAG_TRINITY: u16 = 1 << 1;
    pub const FLAG_INTERNAL: u16 = 1 << 2;

    pub fn new(page_id: PageId) -> Self {
        Self {
            page_id,
            lsn: 0,
            checksum: 0,
            flags: Self::FLAG_LEAF | Self::FLAG_TRINITY,
            lower: Self::SIZE as u16,
            upper: PAGE_SIZE as u16,
            special: PAGE_SIZE as u16, // crece hacia abajo
            num_slots: 0,
            free_slots: 0,
            _padding: [0; 2],
        }
    }

    #[inline]
    pub fn free_space(&self) -> usize {
        self.upper as usize - self.lower as usize
    }
}

/// Slot en el directorio (8 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub offset: u16,  // offset del payload desde inicio de página
    pub length: u16,  // longitud del payload
    pub header: TupleHeader, // xmin/xmax inline para visibilidad sin deserializar payload
    pub flags: u16,   // USED | DELETED | REDIRECT
}

impl Slot {
    pub const FLAG_USED: u16 = 1;
    pub const FLAG_DELETED: u16 = 2;
}

/// Página física de 16KB. Ownership exclusivo via Buffer Pool.
#[repr(C, align(4096))]
pub struct Page {
    pub header: PageHeader,
    pub data: [u8; PAGE_SIZE - PageHeader::SIZE],
}

impl Page {
    pub fn new(page_id: PageId) -> Self {
        Self {
            header: PageHeader::new(page_id),
            data: [0u8; PAGE_SIZE - PageHeader::SIZE],
        }
    }

    /// Obtiene slots como slice tipado. Slots crecen desde lower hacia arriba.
    #[inline]
    fn slots(&self) -> &[Slot] {
        let num = self.header.num_slots as usize;
        let ptr = self.data.as_ptr() as *const Slot;
        unsafe { std::slice::from_raw_parts(ptr, num) }
    }

    #[inline]
    fn slots_mut(&mut self) -> &mut [Slot] {
        let num = self.header.num_slots as usize;
        let ptr = self.data.as_mut_ptr() as *mut Slot;
        unsafe { std::slice::from_raw_parts_mut(ptr, num) }
    }

    /// Inserta tupla con payload arbitrario (row PAX + vector PQ + tokens)
    /// Retorna SlotId. Falla con PageFull si no hay espacio.
    pub fn insert_tuple(&mut self, header: TupleHeader, payload: &[u8]) -> Result<SlotId, &'static str> {
        let required = payload.len() + size_of::<Slot>();
        if required > self.header.free_space() {
            return Err("PageFull");
        }

        // Payload crece desde upper hacia abajo
        let new_upper = self.header.upper as usize - payload.len();
        let data_start = PageHeader::SIZE;
        // Copiar payload a data[new_upper - data_start ..]
        let dst_offset = new_upper - data_start;
        self.data[dst_offset..dst_offset + payload.len()].copy_from_slice(payload);

        // Crear slot
        let slot_id = self.header.num_slots;
        let slot = Slot {
            offset: new_upper as u16,
            length: payload.len() as u16,
            header,
            flags: Slot::FLAG_USED,
        };

        // Escribir slot en posición lower
        let _slot_offset = self.header.lower as usize - data_start;
        // lower apunta al siguiente free slot pos; slots están al inicio de data
        // Implementación simplificada: slots contiguos desde 0
        let slot_ptr = unsafe { self.data.as_mut_ptr().add(slot_id as usize * size_of::<Slot>()) as *mut Slot };
        unsafe { *slot_ptr = slot };

        self.header.upper = new_upper as u16;
        self.header.lower += size_of::<Slot>() as u16;
        self.header.num_slots += 1;

        Ok(slot_id)
    }

    pub fn get_payload(&self, slot_id: SlotId) -> Option<&[u8]> {
        let slots = self.slots();
        let s = slots.get(slot_id as usize)?;
        if s.flags & Slot::FLAG_USED == 0 {
            return None;
        }
        let data_start = PageHeader::SIZE;
        let offset = s.offset as usize - data_start;
        Some(&self.data[offset..offset + s.length as usize])
    }

    pub fn delete_tuple(&mut self, slot_id: SlotId, deleter_txn: TxnId) {
        if let Some(s) = self.slots_mut().get_mut(slot_id as usize) {
            s.header.xmax = deleter_txn;
            s.flags |= Slot::FLAG_DELETED;
        }
    }

    /// Verifica visibilidad MVCC sin tocar payload
    pub fn is_visible(&self, slot_id: SlotId, xmin: TxnId, xmax: TxnId, active: &[TxnId]) -> bool {
        self.slots()
            .get(slot_id as usize)
            .map(|s| s.header.is_visible(xmin, xmax, active))
            .unwrap_or(false)
    }
}

/// Extensión TRINITY: payload híbrido dentro de la misma página
/// No es una tabla separada, es el layout del payload insertado en Page::insert_tuple
#[derive(Debug)]
pub struct TrinityPayload<'a> {
    /// Row data PAX (columnas fijas)
    pub row_data: &'a [u8],
    /// Vector cuantizado PQ (384 bytes para 768d)
    pub pq_vector: &'a [u8],
    /// Posting list local BM25 (delta-encoded doc_ids + freq)
    pub bm25_postings: &'a [u8],
    /// CSR aristas: [row_ptr, col_idx] para grafo
    pub csr_edges: &'a [u8],
}

impl<'a> TrinityPayload<'a> {
    /// Serializa payload híbrido en buffer contiguo para Page::insert_tuple
    /// Layout: [u16 row_len][row][u16 pq_len][pq][u16 bm25_len][bm25][u16 csr_len][csr]
    pub fn serialize(row: &[u8], pq: &[u8], bm25: &[u8], csr: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(row.len() as u16).to_le_bytes());
        out.extend_from_slice(row);
        out.extend_from_slice(&(pq.len() as u16).to_le_bytes());
        out.extend_from_slice(pq);
        out.extend_from_slice(&(bm25.len() as u16).to_le_bytes());
        out.extend_from_slice(bm25);
        out.extend_from_slice(&(csr.len() as u16).to_le_bytes());
        out.extend_from_slice(csr);
    }

    pub fn deserialize(mut buf: &'a [u8]) -> Option<Self> {
        let read_u16 = |b: &mut &[u8]| {
            if b.len() < 2 { return None; }
            let v = u16::from_le_bytes([b[0], b[1]]) as usize;
            *b = &b[2..];
            Some(v)
        };
        let rl = read_u16(&mut buf)?;
        if buf.len() < rl { return None; }
        let (row_data, rest) = buf.split_at(rl);
        buf = rest;
        let pl = read_u16(&mut buf)?;
        if buf.len() < pl { return None; }
        let (pq_vector, rest) = buf.split_at(pl);
        buf = rest;
        let bl = read_u16(&mut buf)?;
        if buf.len() < bl { return None; }
        let (bm25_postings, rest) = buf.split_at(bl);
        buf = rest;
        let cl = read_u16(&mut buf)?;
        if buf.len() < cl { return None; }
        let (csr_edges, _) = buf.split_at(cl);
        Some(Self { row_data, pq_vector, bm25_postings, csr_edges })
    }
}

/// Wrapper de alto nivel para página hoja TRINITY con micro-HNSW
pub struct TrinityPage {
    pub page: Page,
    // Micro-HNSW en área especial (no serializado en data, reside en special area)
    // Para skeleton: stub
}

impl TrinityPage {
    pub fn new(page_id: PageId) -> Self {
        Self { page: Page::new(page_id) }
    }

    /// Búsqueda híbrida intra-página: filtra por visibilidad + distancia PQ aproximada
    /// Retorna Top-K slot_ids dentro de la página (k <= 64 típico)
    pub fn search_in_page<F>(&self, _query_pq: &[u8], _k: usize, _is_visible: F) -> Vec<(SlotId, f32)>
    where
        F: Fn(SlotId) -> bool,
    {
        // TODO: implementar HNSW local + re-ranking. Skeleton retorna vacío
        // Paso real: 1) recorrer HNSW local 2) calcular distancia PQ asimétrica 3) heap TopK
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argentum_common::{TupleHeader, Rid};

    #[test]
    fn page_insert_and_visibility() {
        let mut p = Page::new(1);
        let hdr = TupleHeader { xmin: 10, xmax: 0, ctid: Rid { page_id: 1, slot_id: 0 } };
        let mut buf = Vec::new();
        TrinityPayload::serialize(b"row", &[1,2,3], b"bm25", b"csr", &mut buf);
        let sid = p.insert_tuple(hdr, &buf).unwrap();
        assert!(p.is_visible(sid, 0, 20, &[]));
        assert!(!p.is_visible(sid, 0, 5, &[])); // xmin 10 no visible para snapshot xmax 5
        p.delete_tuple(sid, 15);
        // xmax 15 commiteado => no visible para snapshot que ve hasta 20
        assert!(!p.is_visible(sid, 0, 20, &[]));
        // pero sí visible si deleter aún activo
        assert!(p.is_visible(sid, 0, 20, &[15]));
    }
}

