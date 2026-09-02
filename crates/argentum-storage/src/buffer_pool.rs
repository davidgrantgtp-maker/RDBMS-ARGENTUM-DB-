//! crates/aether-storage/src/buffer_pool.rs:15 - Buffer Pool con clock sweep
//! Maneja fix/unfix de páginas TRINITY con pin count + dirty flag + LSN.

use argentum_common::PageId;
use crate::page::Page;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Frame en buffer pool
struct Frame {
    page: Box<Page>,
    pin_count: usize,
    dirty: bool,
    ref_bit: bool, // para clock
}

pub struct BufferPool {
    capacity: usize,
    frames: RwLock<HashMap<PageId, Frame>>,
    // Para skeleton: HashMap simple. Producción: clock sweep + sharded lru
}

impl BufferPool {
    pub fn new(capacity_pages: usize) -> Self {
        Self {
            capacity: capacity_pages,
            frames: RwLock::new(HashMap::with_capacity(capacity_pages)),
        }
    }

    /// Fix página en memoria (pin). Si no existe, la crea zeroed.
    pub fn fix_page(&self, page_id: PageId) -> Arc<Page> {
        // Simplificado: clon para skeleton sin unsafe pinning
        let mut guard = self.frames.write().unwrap();
        let frame = guard.entry(page_id).or_insert_with(|| Frame {
            page: Box::new(Page::new(page_id)),
            pin_count: 0,
            dirty: false,
            ref_bit: true,
        });
        frame.pin_count += 1;
        frame.ref_bit = true;
        // En producción retornar PageGuard con Deref + Drop que hace unpin
        // Aquí clonamos para evitar lifetimes complejos en skeleton
        let page_copy = unsafe { std::ptr::read(&*frame.page as *const Page) };
        Arc::new(page_copy)
    }

    pub fn unpin_page(&self, page_id: PageId, is_dirty: bool) {
        let mut guard = self.frames.write().unwrap();
        if let Some(f) = guard.get_mut(&page_id) {
            f.pin_count = f.pin_count.saturating_sub(1);
            if is_dirty {
                f.dirty = true;
            }
        }
    }

    /// Evicción clock sweep (skeleton)
    pub fn evict(&self) -> Option<PageId> {
        let mut guard = self.frames.write().unwrap();
        if guard.len() < self.capacity {
            return None;
        }
        // Busca frame con pin_count 0 y ref_bit false
        let victim = guard.iter().find_map(|(pid, f)| {
            if f.pin_count == 0 && !f.ref_bit { Some(*pid) } else { None }
        });
        if let Some(pid) = victim {
            // En producción: flush si dirty antes de remover
            guard.remove(&pid);
        }
        victim
    }

    pub fn flush_all(&self) -> std::io::Result<()> {
        // TODO: iterar dirty frames y pwrite a file segment
        Ok(())
    }
}

