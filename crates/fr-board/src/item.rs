//! Board Item base model and properties.

/// Fixed state of an item (cannot be moved or ripped up by router).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixedState {
    Unfixed,
    ShoveFixed,
    UserFixed,
    SystemFixed,
}

/// Abstract item classification on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Pin,
    Via,
    Trace,
    ObstacleArea,
    ConductionArea,
    ComponentOutline,
    BoardOutline,
}

/// Common metadata for every placed item on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemHeader {
    pub id_no: i32,
    pub net_no_arr: Vec<i32>,
    pub clearance_class: i32,
    pub component_no: i32,
    pub fixed_state: FixedState,
    pub on_the_board: bool,
}

impl ItemHeader {
    pub fn new(id_no: i32, net_no_arr: Vec<i32>, clearance_class: i32, component_no: i32) -> Self {
        ItemHeader {
            id_no,
            net_no_arr,
            clearance_class,
            component_no,
            fixed_state: FixedState::Unfixed,
            on_the_board: true,
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed_state != FixedState::Unfixed
    }

    pub fn contains_net(&self, net_no: i32) -> bool {
        self.net_no_arr.contains(&net_no)
    }
}
