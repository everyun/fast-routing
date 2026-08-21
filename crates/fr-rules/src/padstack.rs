//! Padstack definitions for pins and vias. Ported from `app.freerouting.core.Padstack`.

use fr_geometry::{Direction, IntBox, IntOctagon};

/// Convex shape for a padstack on a particular layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PadShape {
    /// Orthogonal integer bounding box.
    Box(IntBox),
    /// 45-degree integer octagon.
    Octagon(IntOctagon),
    /// Circular pad with integer radius centered at origin.
    Circle { radius: i32 },
}

impl PadShape {
    /// Returns the maximal width / dimension of this shape.
    pub fn max_width(&self) -> f64 {
        match self {
            PadShape::Box(b) => b.max_width(),
            PadShape::Octagon(o) => {
                let bb = o.bounding_box();
                bb.max_width()
            }
            PadShape::Circle { radius } => 2.0 * (*radius as f64),
        }
    }

    /// Returns the bounding box of this shape.
    pub fn bounding_box(&self) -> IntBox {
        match self {
            PadShape::Box(b) => *b,
            PadShape::Octagon(o) => o.bounding_box(),
            PadShape::Circle { radius } => {
                let r = *radius;
                IntBox::new(-r, -r, r, r)
            }
        }
    }
}

/// Describes padstack masks for pins or vias located at the origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Padstack {
    /// Name of the padstack.
    pub name: String,
    /// Padstack identification number.
    pub no: usize,
    /// Whether vias of the own net may overlap with this padstack.
    pub attach_allowed: bool,
    /// If false, the layers of the padstack are mirrored when placed on the back side.
    pub placed_absolute: bool,
    /// True for padstacks whose shapes were synthesized for non-plated drill holes.
    pub hole_only: bool,
    /// Shapes for each board layer (`None` if no copper on that layer).
    pub shapes: Vec<Option<PadShape>>,
}

impl Padstack {
    /// Creates a new `Padstack`.
    pub fn new(
        name: impl Into<String>,
        no: usize,
        shapes: Vec<Option<PadShape>>,
        attach_allowed: bool,
        placed_absolute: bool,
    ) -> Self {
        Padstack {
            name: name.into(),
            no,
            attach_allowed,
            placed_absolute,
            hole_only: false,
            shapes,
        }
    }

    /// Returns the shape of this padstack on the specified layer.
    pub fn get_shape(&self, layer: usize) -> Option<&PadShape> {
        self.shapes.get(layer).and_then(|s| s.as_ref())
    }

    /// Returns the first layer index of this padstack with a non-null shape.
    pub fn from_layer(&self) -> usize {
        for (i, shape) in self.shapes.iter().enumerate() {
            if shape.is_some() {
                return i;
            }
        }
        0
    }

    /// Returns the last layer index of this padstack with a non-null shape.
    pub fn to_layer(&self) -> usize {
        if self.shapes.is_empty() {
            return 0;
        }
        for (i, shape) in self.shapes.iter().enumerate().rev() {
            if shape.is_some() {
                return i;
            }
        }
        self.shapes.len().saturating_sub(1)
    }

    /// Returns the layer count of the board of this padstack.
    pub fn board_layer_count(&self) -> usize {
        self.shapes.len()
    }

    /// Returns the smallest radius among all defined shapes.
    fn get_smallest_radius(&self) -> f64 {
        let mut min_radius = f64::MAX;
        for shape in self.shapes.iter().flatten() {
            let bb = shape.bounding_box();
            let radius = (bb.width().min(bb.height()) as f64) / 2.0;
            if radius < min_radius {
                min_radius = radius;
            }
        }
        if min_radius == f64::MAX {
            0.0
        } else {
            min_radius
        }
    }

    /// Returns the drill radius of this padstack in board units.
    pub fn get_drill_radius(&self) -> f64 {
        if let Some(colon_idx) = self.name.find(':') {
            let after_colon = &self.name[colon_idx + 1..];
            let drill_part = match after_colon.find('_') {
                Some(us_idx) => &after_colon[..us_idx],
                None => after_colon,
            };
            let drill_cleaned: String = drill_part
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(drill_dia) = drill_cleaned.parse::<f64>() {
                let before_colon = &self.name[..colon_idx];
                if let Some(last_us) = before_colon.rfind('_') {
                    let outer_part = &before_colon[last_us + 1..];
                    let outer_cleaned: String = outer_part
                        .chars()
                        .filter(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    if let Ok(outer_dia) = outer_cleaned.parse::<f64>() {
                        if outer_dia > 0.0 {
                            let actual_outer_radius = self.get_smallest_radius();
                            if actual_outer_radius > 0.0 {
                                return actual_outer_radius * (drill_dia / outer_dia);
                            }
                        }
                    }
                }
            }
        }
        self.get_smallest_radius() * 0.45
    }

    /// Calculates the allowed trace exit directions on a layer.
    pub fn get_trace_exit_directions(&self, layer: usize, factor: f64) -> Vec<Direction> {
        let shape = match self.get_shape(layer) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let curr_box = shape.bounding_box();
        let width = curr_box.width() as f64;
        let height = curr_box.height() as f64;
        let all_dirs = width.max(height) < factor * width.min(height);

        let mut result = Vec::new();
        if all_dirs || width >= height {
            result.push(Direction::RIGHT);
            result.push(Direction::LEFT);
        }
        if all_dirs || width <= height {
            result.push(Direction::UP);
            result.push(Direction::DOWN);
        }
        result
    }
}
