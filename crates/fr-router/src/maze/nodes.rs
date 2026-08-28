//! The per-door-section node state arena.
//!
//! Upstream's `MazeSearchElement` is one object per door section, embedded in
//! the door, and `AutorouteEngine.reset_all_doors()` walks every room and every
//! board item to clear them between connections (`AutorouteEngine.java:634-649`
//! — dead in 2.3.0, but the same walk happens inside `clear()`).
//!
//! Here every expandable object owns a contiguous *block* of
//! [`MazeNodeState`] in one `Vec`, allocated when the object is first
//! sectioned, and the reset is a generation bump: a node whose `generation`
//! differs from the arena's reads as pristine, so clearing the whole database
//! is `generation += 1` (`maze-engine.md` §13, "Node identity").

/// Which way a shove adjusted the room this node was entered through
/// (`MazeSearchElement.Adjustment`).
///
/// M2a never produces anything but [`None`](Adjustment::None) — the shove hooks
/// return "blocked" — but the field is carried end to end because
/// `check_ripup`'s fork rule and the locator's geometry both read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Adjustment {
    /// No shove.
    #[default]
    None,
    /// The room was shoved to the right.
    Right,
    /// The room was shoved to the left.
    Left,
}

/// A flat index into [`NodeArena`]. One per (expandable object, section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIndex(pub u32);

impl NodeIndex {
    #[inline]
    pub(crate) fn get(self) -> usize {
        self.0 as usize
    }
}

/// The persistent state of one door section: what upstream stores in
/// `MazeSearchElement` and what the backtracker reads after the search.
#[derive(Debug, Clone, Copy)]
pub struct MazeNodeState {
    /// Arena generation this state belongs to; anything older reads as reset.
    generation: u32,
    /// Set once the section has been settled *and* the expansion committed.
    pub is_occupied: bool,
    /// Predecessor object, or `None` for a start seed.
    pub backtrack: Option<crate::rooms::ExpandableRef>,
    /// Section index inside the predecessor.
    pub section_no_of_backtrack: u16,
    /// The path reaching this section rips an obstacle room.
    pub room_ripped: bool,
    /// Toll paid at *this* door, non-zero only when this door caused the ripup.
    pub ripup_cost: i32,
    /// Shove adjustment state.
    pub adjustment: Adjustment,
}

impl MazeNodeState {
    const fn pristine() -> MazeNodeState {
        MazeNodeState {
            generation: 0,
            is_occupied: false,
            backtrack: None,
            section_no_of_backtrack: 0,
            room_ripped: false,
            ripup_cost: 0,
            adjustment: Adjustment::None,
        }
    }
}

/// One contiguous `Vec<MazeNodeState>` with generation-stamped reset.
#[derive(Debug, Clone)]
pub struct NodeArena {
    states: Vec<MazeNodeState>,
    generation: u32,
}

impl Default for NodeArena {
    fn default() -> Self {
        NodeArena::new()
    }
}

impl NodeArena {
    /// An empty arena at generation 1 (so the `pristine` stamp of 0 is always
    /// stale and a freshly allocated block needs no initialisation pass).
    pub fn new() -> NodeArena {
        NodeArena {
            states: Vec::new(),
            generation: 1,
        }
    }

    /// Number of node slots handed out so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// True when nothing has been allocated.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// The current generation stamp.
    #[inline]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Reserves `count` consecutive nodes and returns the index of the first.
    ///
    /// The block is *not* written: every read goes through
    /// [`get`](Self::get), which returns the pristine state for a stale stamp.
    pub fn allocate(&mut self, count: usize) -> NodeIndex {
        let base = self.states.len();
        self.states.resize(base + count, MazeNodeState::pristine());
        NodeIndex(base as u32)
    }

    /// Invalidates every node without touching memory
    /// (`AutorouteEngine.reset_all_doors` in O(1)).
    ///
    /// Wrapping is safe: a wrap can only alias a node whose stamp survived
    /// `u32::MAX` resets untouched, and the reset that wraps clears the arena's
    /// contents as a belt-and-braces measure.
    pub fn reset(&mut self) {
        match self.generation.checked_add(1) {
            Some(next) => self.generation = next,
            None => {
                self.states
                    .iter_mut()
                    .for_each(|state| *state = MazeNodeState::pristine());
                self.generation = 1;
            }
        }
    }

    /// The live state of `index`, or the pristine state when its stamp is old.
    #[inline]
    pub fn get(&self, index: NodeIndex) -> MazeNodeState {
        let state = self.states[index.get()];
        if state.generation == self.generation {
            state
        } else {
            MazeNodeState {
                generation: self.generation,
                ..MazeNodeState::pristine()
            }
        }
    }

    /// Whether `index` is occupied in the current generation.
    #[inline]
    pub fn is_occupied(&self, index: NodeIndex) -> bool {
        let state = &self.states[index.get()];
        state.generation == self.generation && state.is_occupied
    }

    /// Marks `index` occupied, refreshing its stamp.
    #[inline]
    pub fn occupy(&mut self, index: NodeIndex) {
        let generation = self.generation;
        let state = &mut self.states[index.get()];
        if state.generation != generation {
            *state = MazeNodeState {
                generation,
                ..MazeNodeState::pristine()
            };
        }
        state.is_occupied = true;
    }

    /// Writes the settle payload — backtrack pointer, ripup flags, adjustment
    /// — without setting `is_occupied`.
    ///
    /// This is the *settle* half of upstream's settle-then-maybe-occupy
    /// protocol (`MazeSearchAlgo.java:277-281`): the fields land immediately,
    /// the occupied flag only if the expansion commits.
    pub fn settle(
        &mut self,
        index: NodeIndex,
        backtrack: Option<crate::rooms::ExpandableRef>,
        section_no_of_backtrack: u16,
        room_ripped: bool,
        ripup_cost: i32,
        adjustment: Adjustment,
    ) {
        let generation = self.generation;
        let state = &mut self.states[index.get()];
        let was_occupied = state.generation == generation && state.is_occupied;
        *state = MazeNodeState {
            generation,
            is_occupied: was_occupied,
            backtrack,
            section_no_of_backtrack,
            room_ripped,
            ripup_cost,
            adjustment,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_block_reads_as_unoccupied() {
        let mut arena = NodeArena::new();
        let base = arena.allocate(4);
        for offset in 0..4 {
            assert!(!arena.is_occupied(NodeIndex(base.0 + offset)));
        }
    }

    #[test]
    fn reset_clears_without_walking() {
        let mut arena = NodeArena::new();
        let base = arena.allocate(2);
        arena.occupy(base);
        arena.settle(base, None, 0, true, 7, Adjustment::Left);
        assert!(arena.is_occupied(base));
        assert_eq!(arena.get(base).ripup_cost, 7);

        arena.reset();

        assert!(!arena.is_occupied(base));
        let state = arena.get(base);
        assert_eq!(state.ripup_cost, 0);
        assert!(!state.room_ripped);
        assert_eq!(state.adjustment, Adjustment::None);
    }

    #[test]
    fn settle_preserves_the_occupied_flag_of_the_current_generation() {
        let mut arena = NodeArena::new();
        let base = arena.allocate(1);
        arena.occupy(base);
        arena.settle(base, None, 3, false, 0, Adjustment::None);
        assert!(arena.is_occupied(base), "settle must not clear occupation");
        assert_eq!(arena.get(base).section_no_of_backtrack, 3);
    }

    #[test]
    fn allocate_hands_out_disjoint_blocks() {
        let mut arena = NodeArena::new();
        let first = arena.allocate(3);
        let second = arena.allocate(2);
        assert_eq!(first.0, 0);
        assert_eq!(second.0, 3);
        assert_eq!(arena.len(), 5);
    }
}
