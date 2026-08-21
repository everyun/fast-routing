//! High-performance spatial grid for O(1) point & box collision queries during 3D A* maze routing.

use fr_geometry::planar::{IntBox, IntPoint};

/// 2D flat spatial cell bucket grid for single layer obstacle queries.
#[derive(Debug, Clone)]
pub struct LayerSpatialGrid {
    pub min_x: i32,
    pub min_y: i32,
    pub cell_size: i32,
    pub cols: usize,
    pub rows: usize,
    pub buckets: Vec<Vec<IntBox>>,
}

impl LayerSpatialGrid {
    pub fn new(bounds: IntBox, cell_size: i32) -> Self {
        let cs = cell_size.max(100);
        let min_x = bounds.ll.x - cs;
        let min_y = bounds.ll.y - cs;
        let width = (bounds.ur.x - min_x).max(cs) + cs * 2;
        let height = (bounds.ur.y - min_y).max(cs) + cs * 2;
        let cols = (width / cs) as usize + 1;
        let rows = (height / cs) as usize + 1;
        let buckets = vec![Vec::new(); cols * rows];

        LayerSpatialGrid {
            min_x,
            min_y,
            cell_size: cs,
            cols,
            rows,
            buckets,
        }
    }

    #[inline(always)]
    fn get_grid_coord(&self, x: i32, y: i32) -> (usize, usize) {
        let gx = ((x - self.min_x) / self.cell_size).clamp(0, (self.cols - 1) as i32) as usize;
        let gy = ((y - self.min_y) / self.cell_size).clamp(0, (self.rows - 1) as i32) as usize;
        (gx, gy)
    }

    pub fn insert(&mut self, b: IntBox) {
        let (gx_min, gy_min) = self.get_grid_coord(b.ll.x, b.ll.y);
        let (gx_max, gy_max) = self.get_grid_coord(b.ur.x, b.ur.y);

        for gy in gy_min..=gy_max {
            let row_offset = gy * self.cols;
            for gx in gx_min..=gx_max {
                self.buckets[row_offset + gx].push(b);
            }
        }
    }

    #[inline(always)]
    pub fn collides_point(&self, pt: &IntPoint) -> bool {
        let (gx, gy) = self.get_grid_coord(pt.x, pt.y);
        let cell = &self.buckets[gy * self.cols + gx];
        cell.iter().any(|b| pt.is_contained_in(b))
    }

    #[inline(always)]
    pub fn collides_box(&self, b: &IntBox) -> bool {
        let (gx_min, gy_min) = self.get_grid_coord(b.ll.x, b.ll.y);
        let (gx_max, gy_max) = self.get_grid_coord(b.ur.x, b.ur.y);

        for gy in gy_min..=gy_max {
            let row_offset = gy * self.cols;
            for gx in gx_min..=gx_max {
                if self.buckets[row_offset + gx].iter().any(|existing| existing.intersects(b)) {
                    return true;
                }
            }
        }
        false
    }
}
