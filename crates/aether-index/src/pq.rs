//! crates/aether-index/src/pq.rs:10 - Product Quantization para vectores 768d
//! Compresión 8x: 768*f32 (3072B) -> 96*u8 (96B) o 384B según M.

/// Codebook PQ: M subespacios x K centroides
pub struct Codebook {
    pub m: usize,        // num subespacios (ej 96)
    pub k: usize,        // centroides por subespacio (256 => u8)
    pub dim: usize,      // dim total (768)
    pub centroids: Vec<f32>, // [M * K * (dim/M)]
}

impl Codebook {
    pub fn new(m: usize, k: usize, dim: usize) -> Self {
        assert!(dim % m == 0);
        Self { m, k, dim, centroids: vec![0.0; m * k * (dim / m)] }
    }

    /// Entrena codebook con k-means por subespacio (skeleton: random)
    pub fn train(&mut self, _vectors: &[Vec<f32>]) {
        // TODO: k-means real por subespacio
    }

    /// Cuantiza vector -> códigos PQ
    pub fn encode(&self, vector: &[f32], out: &mut Vec<u8>) {
        out.clear();
        out.reserve(self.m);
        let sub_dim = self.dim / self.m;
        for s in 0..self.m {
            let sub = &vector[s * sub_dim..(s + 1) * sub_dim];
            // Nearest centroid (skeleton: brute force)
            let mut best = 0u8;
            let mut best_dist = f32::MAX;
            for c in 0..self.k {
                let cent = &self.centroids[(s * self.k + c) * sub_dim..][..sub_dim];
                let d: f32 = sub.iter().zip(cent.iter()).map(|(a, b)| (a - b).powi(2)).sum();
                if d < best_dist {
                    best_dist = d;
                    best = c as u8;
                }
            }
            out.push(best);
        }
    }

    /// Distancia asimétrica: query f32 vs código PQ (lookup table)
    pub fn asymmetric_distance(&self, query: &[f32], code: &[u8]) -> f32 {
        let sub_dim = self.dim / self.m;
        let mut dist = 0.0;
        for s in 0..self.m {
            let qsub = &query[s * sub_dim..(s + 1) * sub_dim];
            let c = code[s] as usize;
            let cent = &self.centroids[(s * self.k + c) * sub_dim..][..sub_dim];
            dist += qsub.iter().zip(cent.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f32>();
        }
        dist.sqrt()
    }
}
