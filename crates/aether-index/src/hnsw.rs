//! crates/aether-index/src/hnsw.rs:18 - Micro-HNSW latch-free versionado para TRINITY
//! Cada página TRINITY tiene un micro-HNSW de <= 512 nodos. El HNSW global es federado.

use std::collections::BinaryHeap;

/// Nodo HNSW versionado MVCC
#[derive(Debug, Clone)]
pub struct HnswNode {
    pub slot_id: u16,          // slot en la página
    pub level: usize,          // nivel máximo del nodo
    pub neighbors: Vec<Vec<u16>>, // neighbors[level] -> lista de slot_ids
    pub xmin: u64,
    pub xmax: u64, // 0 = vivo
}

/// Micro-HNSW por página (skeleton)
pub struct MicroHnsw {
    pub nodes: Vec<HnswNode>,
    pub entry_point: Option<u16>,
    pub m: usize,       // grado máximo
    pub m_l: f64,       // 1/ln(M)
    pub ef_construction: usize,
}

impl MicroHnsw {
    pub fn new(m: usize, ef_construction: usize) -> Self {
        Self {
            nodes: Vec::new(),
            entry_point: None,
            m,
            m_l: 1.0 / (m as f64).ln(),
            ef_construction,
        }
    }

    /// Nivel aleatorio para nuevo nodo: l = floor(-ln(unif)*m_l)
    pub fn random_level(&self, uniform: f64) -> usize {
        // uniform en (0,1]
        (-uniform.ln() * self.m_l).floor() as usize
    }

    /// Inserta nodo con conexiones. En producción: latch-free CAS
    pub fn insert(&mut self, slot_id: u16, level: usize, neighbors_per_level: Vec<Vec<u16>>, xmin: u64) {
        let node = HnswNode {
            slot_id,
            level,
            neighbors: neighbors_per_level,
            xmin,
            xmax: 0,
        };
        if self.entry_point.is_none() || level > self.nodes.iter().map(|n| n.level).max().unwrap_or(0) {
            self.entry_point = Some(slot_id);
        }
        self.nodes.push(node);
    }

    /// Búsqueda Top-K aproximada dentro de la página con distancia PQ
    /// Retorna candidatos ordenados por distancia
    pub fn search<F>(&self, _query_code: &[u8], k: usize, _dist_fn: F, is_visible: impl Fn(u16) -> bool) -> Vec<(u16, f32)>
    where
        F: Fn(u16) -> f32,
    {
        // Skeleton: linear scan filtrado por visibilidad (producción: greedy beam search HNSW)
        let mut heap: BinaryHeap<(OrderedFloat, u16)> = BinaryHeap::new();
        for n in &self.nodes {
            if !is_visible(n.slot_id) { continue; }
            if n.xmax != 0 { continue; }
            let d = _dist_fn(n.slot_id);
            heap.push((OrderedFloat(d), n.slot_id));
            if heap.len() > k {
                heap.pop();
            }
        }
        heap.into_sorted_vec().into_iter().map(|(d, id)| (id, d.0)).collect()
    }

    /// Marca nodo como borrado MVCC (no lo elimina físicamente)
    pub fn delete(&mut self, slot_id: u16, xmax: u64) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.slot_id == slot_id) {
            n.xmax = xmax;
        }
    }
}

// Wrapper para ordenar f32 en BinaryHeap
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct OrderedFloat(f32);
impl Eq for OrderedFloat {}
impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}
