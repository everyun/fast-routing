//! Board layer definition and layer structure, ported from `app.freerouting.board.LayerStructure`
//! and `app.freerouting.board.Layer`.

/// Describes the structure of a board layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Layer {
    /// The name of the layer.
    pub name: String,
    /// True if this is a signal layer which can be used for routing.
    pub is_signal: bool,
}

impl Layer {
    /// Creates a new layer.
    pub fn new(name: impl Into<String>, is_signal: bool) -> Self {
        Layer {
            name: name.into(),
            is_signal,
        }
    }
}

/// Describes the layer structure of the board.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LayerStructure {
    /// List of board layers ordered by physical stackup.
    pub arr: Vec<Layer>,
}

impl LayerStructure {
    /// Creates a new `LayerStructure` with the given layers.
    pub fn new(layers: Vec<Layer>) -> Self {
        LayerStructure { arr: layers }
    }

    /// Returns the number of layers.
    pub fn len(&self) -> usize {
        self.arr.len()
    }

    /// Returns true if there are no layers.
    pub fn is_empty(&self) -> bool {
        self.arr.is_empty()
    }

    /// Returns the index of the layer with the given name, or `None` if not found.
    pub fn get_no(&self, name: &str) -> Option<usize> {
        self.arr.iter().position(|l| l.name == name)
    }

    /// Returns the count of signal layers in this structure.
    pub fn signal_layer_count(&self) -> usize {
        self.arr.iter().filter(|l| l.is_signal).count()
    }

    /// Gets the `no`-th signal layer in this layer structure.
    pub fn get_signal_layer(&self, no: usize) -> Option<&Layer> {
        let mut found = 0;
        for layer in &self.arr {
            if layer.is_signal {
                if found == no {
                    return Some(layer);
                }
                found += 1;
            }
        }
        self.arr.last()
    }

    /// Returns the count of signal layers with a smaller index than `layer_index`.
    pub fn get_signal_layer_no(&self, layer_index: usize) -> Option<usize> {
        if layer_index >= self.arr.len() {
            return None;
        }
        let mut count = 0;
        for i in 0..layer_index {
            if self.arr[i].is_signal {
                count += 1;
            }
        }
        Some(count)
    }

    /// Gets the board layer index of the `signal_layer_no`-th signal layer.
    pub fn get_layer_no(&self, signal_layer_no: usize) -> Option<usize> {
        let mut count = 0;
        for (i, layer) in self.arr.iter().enumerate() {
            if layer.is_signal {
                if count == signal_layer_no {
                    return Some(i);
                }
                count += 1;
            }
        }
        None
    }
}

/// Enum for trace angle restrictions: none, 45 degree, or 90 degree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AngleRestriction {
    None = 0,
    #[default]
    FortyFiveDegree = 1,
    NinetyDegree = 2,
}

impl AngleRestriction {
    /// Returns the integer value of this enum.
    pub fn to_i32(self) -> i32 {
        self as i32
    }

    /// Parses integer value into `AngleRestriction`.
    pub fn from_i32(val: i32) -> Option<Self> {
        match val {
            0 => Some(AngleRestriction::None),
            1 => Some(AngleRestriction::FortyFiveDegree),
            2 => Some(AngleRestriction::NinetyDegree),
            _ => None,
        }
    }
}
