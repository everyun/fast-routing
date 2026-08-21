//! 2D Planar Delaunay Triangulation and Minimum Spanning Tree (MST) air-lines calculation.
//!
//! Ported from `app.freerouting.datastructures.PlanarDelaunayTriangulation`.

use std::collections::BTreeSet;

/// A 2D point used for triangulation and distance calculations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

impl Point2D {
    /// Creates a new `Point2D`.
    pub const fn new(x: f64, y: f64) -> Self {
        Point2D { x, y }
    }

    /// Creates a new `Point2D` from integer coordinates.
    pub const fn from_i32(x: i32, y: i32) -> Self {
        Point2D {
            x: x as f64,
            y: y as f64,
        }
    }

    /// Computes the squared Euclidean distance to `other`.
    pub fn distance_sq(&self, other: &Point2D) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    /// Computes the Euclidean distance to `other`.
    pub fn distance(&self, other: &Point2D) -> f64 {
        self.distance_sq(other).sqrt()
    }
}

/// Orientation of a point relative to a directed line segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    OnTheLeft,
    OnTheRight,
    Collinear,
}

impl Orientation {
    pub fn of(det: f64) -> Self {
        const EPS: f64 = 1e-11;
        if det > EPS {
            Orientation::OnTheLeft
        } else if det < -EPS {
            Orientation::OnTheRight
        } else {
            Orientation::Collinear
        }
    }
}

/// Returns `true` if `p` is strictly inside the circumcircle of triangle `(a, b, c)`.
fn inside_circumcircle(p: &Point2D, a: &Point2D, b: &Point2D, c: &Point2D) -> bool {
    let orient = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let (a, b, c) = if orient < 0.0 { (a, c, b) } else { (a, b, c) };

    let ax = a.x - p.x;
    let ay = a.y - p.y;
    let bx = b.x - p.x;
    let by = b.y - p.y;
    let cx = c.x - p.x;
    let cy = c.y - p.y;

    let det = (ax * ax + ay * ay) * (bx * cy - by * cx)
        + (bx * bx + by * by) * (cx * ay - cy * ax)
        + (cx * cx + cy * cy) * (ax * by - ay * bx);

    det > 1e-10
}

/// A line segment edge in the Delaunay Triangulation.
#[derive(Debug, Clone)]
pub struct DelaunayEdge<T: Clone> {
    pub start_point: Point2D,
    pub start_data: Option<T>,
    pub end_point: Point2D,
    pub end_data: Option<T>,
    pub distance: f64,
}

impl<T: Clone> DelaunayEdge<T> {
    pub fn new(
        start_point: Point2D,
        start_data: Option<T>,
        end_point: Point2D,
        end_data: Option<T>,
    ) -> Self {
        let distance = start_point.distance(&end_point);
        DelaunayEdge {
            start_point,
            start_data,
            end_point,
            end_data,
            distance,
        }
    }
}

#[derive(Debug, Clone)]
struct Vertex<T> {
    point: Point2D,
    data: Option<T>,
    is_super: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tri {
    v: [usize; 3], // CCW order
}

/// 2D Planar Delaunay Triangulation computed via Bowyer-Watson incremental insertion.
#[derive(Debug, Clone)]
pub struct PlanarDelaunayTriangulation<T: Clone> {
    vertices: Vec<Vertex<T>>,
    edges: Vec<DelaunayEdge<T>>,
}

impl<T: Clone> PlanarDelaunayTriangulation<T> {
    /// Constructs a Delaunay Triangulation from a set of 2D points and their associated data.
    pub fn new(points: Vec<(Point2D, T)>) -> Self {
        if points.is_empty() {
            return PlanarDelaunayTriangulation {
                vertices: Vec::new(),
                edges: Vec::new(),
            };
        }

        if points.len() == 1 {
            let (pt, data) = points.into_iter().next().unwrap();
            return PlanarDelaunayTriangulation {
                vertices: vec![Vertex {
                    point: pt,
                    data: Some(data),
                    is_super: false,
                }],
                edges: Vec::new(),
            };
        }

        if points.len() == 2 {
            let mut iter = points.into_iter();
            let (p1, d1) = iter.next().unwrap();
            let (p2, d2) = iter.next().unwrap();
            let edge = DelaunayEdge::new(p1, Some(d1.clone()), p2, Some(d2.clone()));
            return PlanarDelaunayTriangulation {
                vertices: vec![
                    Vertex {
                        point: p1,
                        data: Some(d1),
                        is_super: false,
                    },
                    Vertex {
                        point: p2,
                        data: Some(d2),
                        is_super: false,
                    },
                ],
                edges: vec![edge],
            };
        }

        // Compute bounding box of input points
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;

        for (pt, _) in &points {
            min_x = min_x.min(pt.x);
            min_y = min_y.min(pt.y);
            max_x = max_x.max(pt.x);
            max_y = max_y.max(pt.y);
        }

        let dx = (max_x - min_x).max(1.0);
        let dy = (max_y - min_y).max(1.0);
        let dmax = dx.max(dy);
        let mid_x = (min_x + max_x) * 0.5;
        let mid_y = (min_y + max_y) * 0.5;

        // Super-triangle enclosing all points in CCW orientation
        let p0 = Point2D::new(mid_x - 20.0 * dmax, mid_y - dmax);
        let p1 = Point2D::new(mid_x + 20.0 * dmax, mid_y - dmax);
        let p2 = Point2D::new(mid_x, mid_y + 20.0 * dmax);

        let mut vertices: Vec<Vertex<T>> = Vec::with_capacity(points.len() + 3);
        vertices.push(Vertex {
            point: p0,
            data: None,
            is_super: true,
        });
        vertices.push(Vertex {
            point: p1,
            data: None,
            is_super: true,
        });
        vertices.push(Vertex {
            point: p2,
            data: None,
            is_super: true,
        });

        for (pt, data) in points {
            vertices.push(Vertex {
                point: pt,
                data: Some(data),
                is_super: false,
            });
        }

        let mut triangles = vec![Tri { v: [0, 1, 2] }];

        // Incremental insertion
        for i in 3..vertices.len() {
            let pt = vertices[i].point;
            let mut bad_triangles = Vec::new();

            for (t_idx, tri) in triangles.iter().enumerate() {
                let a = &vertices[tri.v[0]].point;
                let b = &vertices[tri.v[1]].point;
                let c = &vertices[tri.v[2]].point;

                if inside_circumcircle(&pt, a, b, c) {
                    bad_triangles.push(t_idx);
                }
            }

            // Find boundary polygon of bad triangles (edges not shared by 2 bad triangles)
            let mut polygon_edges = Vec::new();
            for &t_idx in &bad_triangles {
                let tri = triangles[t_idx];
                let tri_edges = [
                    (tri.v[0], tri.v[1]),
                    (tri.v[1], tri.v[2]),
                    (tri.v[2], tri.v[0]),
                ];

                for (ea, eb) in tri_edges {
                    let mut shared = false;
                    for &other_t_idx in &bad_triangles {
                        if other_t_idx == t_idx {
                            continue;
                        }
                        let otri = triangles[other_t_idx];
                        let other_edges = [
                            (otri.v[0], otri.v[1]),
                            (otri.v[1], otri.v[2]),
                            (otri.v[2], otri.v[0]),
                        ];
                        if other_edges.iter().any(|&(oa, ob)| (ea == oa && eb == ob) || (ea == ob && eb == oa)) {
                            shared = true;
                            break;
                        }
                    }
                    if !shared {
                        polygon_edges.push((ea, eb));
                    }
                }
            }

            // Remove bad triangles in reverse order
            bad_triangles.sort_unstable();
            for &t_idx in bad_triangles.iter().rev() {
                triangles.swap_remove(t_idx);
            }

            // Retriangulate the polygonal hole with the new vertex
            for (ea, eb) in polygon_edges {
                triangles.push(Tri { v: [ea, eb, i] });
            }
        }

        // Filter out super-triangle vertices to produce real triangulation edges
        let mut edge_set = BTreeSet::new();
        let mut edges = Vec::new();

        for tri in &triangles {
            let tri_edges = [
                (tri.v[0], tri.v[1]),
                (tri.v[1], tri.v[2]),
                (tri.v[2], tri.v[0]),
            ];

            for (u, v) in tri_edges {
                if !vertices[u].is_super && !vertices[v].is_super {
                    let key = if u < v { (u, v) } else { (v, u) };
                    if edge_set.insert(key) {
                        let pu = vertices[u].point;
                        let pv = vertices[v].point;
                        let du = vertices[u].data.clone();
                        let dv = vertices[v].data.clone();
                        edges.push(DelaunayEdge::new(pu, du, pv, dv));
                    }
                }
            }
        }

        PlanarDelaunayTriangulation {
            vertices,
            edges,
        }
    }

    /// Returns all Delaunay triangulation edges between input points.
    pub fn get_edge_lines(&self) -> &[DelaunayEdge<T>] {
        &self.edges
    }

    /// Computes the Minimum Spanning Tree (MST) over the Delaunay triangulation edges using Kruskal's algorithm.
    ///
    /// This is used directly by Freerouting to generate ratsnest airlines connecting all unrouted pins/pads.
    pub fn minimum_spanning_tree(&self) -> Vec<DelaunayEdge<T>> {
        if self.edges.is_empty() {
            return Vec::new();
        }

        // Map non-super vertices to 0..N indices for Union-Find
        let non_super: Vec<usize> = self
            .vertices
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.is_super)
            .map(|(i, _)| i)
            .collect();

        let mut uf = UnionFind::new(self.vertices.len());
        let mut sorted_edges = self.edges.clone();
        sorted_edges.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());

        let mut mst = Vec::with_capacity(non_super.len().saturating_sub(1));

        for edge in sorted_edges {
            // Find vertex indices corresponding to start and end points
            let u = self.find_vertex_index(&edge.start_point);
            let v = self.find_vertex_index(&edge.end_point);

            if let (Some(u_idx), Some(v_idx)) = (u, v) {
                if uf.union(u_idx, v_idx) {
                    mst.push(edge);
                    if mst.len() == non_super.len() - 1 {
                        break;
                    }
                }
            }
        }

        mst
    }

    fn find_vertex_index(&self, pt: &Point2D) -> Option<usize> {
        self.vertices.iter().position(|v| {
            !v.is_super
                && (v.point.x - pt.x).abs() < 1e-6
                && (v.point.y - pt.y).abs() < 1e-6
        })
    }
}

/// Disjoint-Set Union (Union-Find) with path compression and rank optimization.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        UnionFind {
            parent: (0..size).collect(),
            rank: vec![0; size],
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        while self.parent[x] != root {
            let next = self.parent[x];
            self.parent[x] = root;
            x = next;
        }
        root
    }

    fn union(&mut self, x: usize, y: usize) -> bool {
        let root_x = self.find(x);
        let root_y = self.find(y);
        if root_x == root_y {
            return false;
        }
        if self.rank[root_x] < self.rank[root_y] {
            self.parent[root_x] = root_y;
        } else if self.rank[root_x] > self.rank[root_y] {
            self.parent[root_y] = root_x;
        } else {
            self.parent[root_y] = root_x;
            self.rank[root_x] += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_triangle_delaunay() {
        let points = vec![
            (Point2D::new(0.0, 0.0), "A"),
            (Point2D::new(10.0, 0.0), "B"),
            (Point2D::new(5.0, 10.0), "C"),
        ];

        let dt = PlanarDelaunayTriangulation::new(points);
        let edges = dt.get_edge_lines();
        assert_eq!(edges.len(), 3);

        let mst = dt.minimum_spanning_tree();
        assert_eq!(mst.len(), 2);
    }

    #[test]
    fn test_grid_delaunay_and_mst() {
        // 4 points forming a square: (0,0), (10,0), (10,10), (0,10)
        let points = vec![
            (Point2D::new(0.0, 0.0), "P1"),
            (Point2D::new(10.0, 0.0), "P2"),
            (Point2D::new(10.0, 10.0), "P3"),
            (Point2D::new(0.0, 10.0), "P4"),
        ];

        let dt = PlanarDelaunayTriangulation::new(points);
        let edges = dt.get_edge_lines();
        // A triangulated 4-point convex quad has 5 edges (4 boundary + 1 diagonal)
        assert_eq!(edges.len(), 5);

        let mst = dt.minimum_spanning_tree();
        assert_eq!(mst.len(), 3);
        let total_weight: f64 = mst.iter().map(|e| e.distance).sum();
        assert!((total_weight - 30.0).abs() < 1e-4);
    }
}
