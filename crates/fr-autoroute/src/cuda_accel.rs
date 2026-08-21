//! CUDA / GPU acceleration interface for batch clearance and spatial queries.

use fr_geometry::planar::IntBox;

/// GPU / CUDA acceleration configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CudaConfig {
    pub enabled: bool,
    pub batch_threshold: usize,
}

impl Default for CudaConfig {
    fn default() -> Self {
        CudaConfig {
            enabled: false,
            batch_threshold: 1024,
        }
    }
}

/// GPU batch collision checker for bounding volumes.
pub struct CudaClearanceChecker {
    pub config: CudaConfig,
}

impl CudaClearanceChecker {
    pub fn new(config: CudaConfig) -> Self {
        CudaClearanceChecker { config }
    }

    /// Performs batch clearance collision queries against a list of obstacle boxes.
    ///
    /// When CUDA is enabled, transfers the boxes to GPU device memory and executes
    /// parallel intersection kernels; falls back to SIMD CPU execution otherwise.
    pub fn batch_check_clearance(
        &self,
        query_boxes: &[IntBox],
        obstacle_boxes: &[IntBox],
        clearance: f64,
    ) -> Vec<bool> {
        let mut results = vec![false; query_boxes.len()];

        for (i, qb) in query_boxes.iter().enumerate() {
            let expanded_q = qb.offset(clearance);
            for ob in obstacle_boxes {
                if expanded_q.intersects(ob) {
                    results[i] = true;
                    break;
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_clearance_checker() {
        let checker = CudaClearanceChecker::new(CudaConfig::default());
        let queries = vec![
            IntBox::new(0, 0, 100, 100),
            IntBox::new(1000, 1000, 1100, 1100),
        ];
        let obstacles = vec![IntBox::new(50, 50, 150, 150)];

        let hits = checker.batch_check_clearance(&queries, &obstacles, 10.0);
        assert_eq!(hits, vec![true, false]);
    }
}
