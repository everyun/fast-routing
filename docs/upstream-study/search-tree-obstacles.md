# Freerouting upstream study: Spatial index (ShapeSearchTree) and the obstacle model

Sources studied (read completely):

| Area | v1.9 file (`/tmp/fr-java/src_v19/main/java/app/freerouting/`) | v2.x file (`/tmp/fr-java/src/main/java/app/freerouting/`) |
|---|---|---|
| Abstract tree | `datastructures/ShapeTree.java` (226 L) | `datastructures/ShapeTree.java` (236 L) — pure rename refactor |
| Concrete tree | `datastructures/MinAreaTree.java` (184 L) | `datastructures/MinAreaTree.java` (180 L) — pure rename refactor |
| Board-aware tree | `board/ShapeSearchTree.java` (1092 L) | `board/searchtree/ShapeSearchTree.java` (1160 L) |
| 45° specialization | `board/ShapeSearchTree45Degree.java` (592 L) | `board/searchtree/ShapeSearchTree45Degree.java` (648 L) |
| 90° specialization | `board/ShapeSearchTree90Degree.java` (414 L) | `board/searchtree/ShapeSearchTree90Degree.java` (504 L) |
| Tree registry | `board/SearchTreeManager.java` (268 L) | `board/searchtree/SearchTreeManager.java` (285 L) |
| Stored-object iface | `board/SearchTreeObject.java` | `board/searchtree/SearchTreeObject.java` |
| Per-item cache | `board/ItemSearchTreesInfo.java` | `board/actions/ItemSearchTreesInfo.java` |
| Item base | `board/Item.java` (1237 L) | `board/model/items/Item.java` |
| Board storage | `board/BasicBoard.java` (1600 L) | `board/facade/BasicBoard.java` (+ `facade/BoardItemRepository`, `facade/RoutingBoardSearchFacade` splits) |
| Trace/tree sync | (inline in `board/PolylineTrace.java`) | `board/trace/PolylineTraceSearchTreeAdapter.java` (new, stateless forwarding shim) |

v2 naming: `snake_case` → `camelCase` throughout (`overlapping_tree_entries` → `overlappingTreeEntries`, `get_autoroute_tree` → `getAutorouteTree`, `compensated_clearance_class_no` → `compensatedClearanceClassNo`, etc.). The 2.3.0 release builds from the `/src` (v2) tree; the v2 classes are the ones actually imported by the v2 autoroute packages (`autoroute/expansion/*` import `board.searchtree.*`). Algorithmically v2 is a faithful port of v19 **plus** three real semantic changes, documented in §9: (a) synthetic drill-hole obstacles, (b) board-outline compensation is now per-layer, (c) the "slow autoroute algorithm" escape hatch was removed. Note: both checkouts contain locally-added debug instrumentation (hard-coded net numbers 77/84 and coordinate windows, e.g. `ShapeSearchTree45Degree.is_complete_shape_debug_anchor` v19:486-494, `BasicBoard.is_item_activity_debug_candidate` v19:1352-1356). These are diagnostic patches, **not** part of the algorithm — do not port them.

---

## 1. The tree structure itself

### 1.1 Node layout (`datastructures/ShapeTree.java`)

The index is a **binary tree with data only in the leaves** — structurally an unbalanced, insertion-ordered R-tree with fan-out 2 ("MinAreaTree" = minimal-area-increase insertion heuristic; the comment at MinAreaTree.java:13-15 says it descends "so that the increase of the bounding shape of that child is minimal").

```java
class TreeNode  { RegularTileShape bounding_shape; InnerNode parent; }           // ShapeTree.java:169-172
class InnerNode extends TreeNode { TreeNode first_child, second_child; }         // :176-185
class Leaf      extends TreeNode implements Comparable<Leaf> {
    ShapeTree.Storable object;        // the board item / expansion room (MUTABLE — reassigned on trace splits)
    int shape_index_in_object;        // which of the object's tile shapes this leaf is (MUTABLE)
}                                                                                // :189-224
```

Invariants:

* Every inner node has exactly 2 non-null children; a tree with N leaves has exactly N−1 inner nodes. `root == null` iff empty; a single leaf can be the root.
* `node.bounding_shape` always contains the union of its subtree's leaf bounding shapes, but is only **tightened lazily**: `insert` unions on the way down (MinAreaTree.java:96-97); `remove_leaf` re-tightens ancestors bottom-up and stops early as soon as recomputed bounds no longer shrink (`new_bounds.contains(node.bounding_shape)` → break, MinAreaTree.java:171-182). So bounds may be looser than optimal but never wrong.
* `Leaf.bounding_shape` = `object_shape.bounding_shape(bounding_directions)` — a *regular* tile shape (axis-aligned box or 45°-regular octagon) enclosing the exact tile shape (ShapeTree.java:52).
* `leaf_count` tracks the number of leaves (`size()`).
* Leaves are compared by `object.compareTo` then `shape_index_in_object` (ShapeTree.java:206-213); `Item.compareTo` is **descending** id order (`other.id_no - id_no`, Item.java:83-91). This ordering makes `TreeSet<Leaf>` query results deterministic.

`Storable` interface (ShapeTree.java:140-155) — implemented by `Item` and `CompleteFreeSpaceExpansionRoom`:

```
int       tree_shape_count(tree)                 // number of tile shapes to index
TileShape get_tree_shape(tree, index)            // the index-th tile shape FOR THAT TREE (compensated!)
void      set_search_tree_entries(Leaf[], tree)  // back-pointer storage so removal is O(k log n) not a search
```

`TreeEntry` (ShapeTree.java:158-165) is the query result record: `{Storable object; int shape_index_in_object}` — a fresh heap object per hit.

`SearchTreeObject` (board/SearchTreeObject.java) adds the board-level filter contract on top of `Storable`:

```
boolean is_obstacle(int net_no)         // Item: !contains_net(net_no)          (Item.java:112-114)
boolean is_trace_obstacle(int net_no)   // Item: !contains_net(net_no); overridden by ConductionArea etc.
int     shape_layer(int index)          // layer of the index-th shape
int     get_id_no()                     // deterministic sort key
```

`CompleteFreeSpaceExpansionRoom` implements it with `tree_shape_count = 1`, `get_tree_shape = its shape`, `is_obstacle/is_trace_obstacle = true` always (CompleteFreeSpaceExpansionRoom.java:181-203) — i.e. **free-space rooms are indexed in the same tree as items** and act as obstacles to further room expansion.

### 1.2 Bounding directions / key shapes

`ShapeBoundingDirections` (geometry/planar) has two singletons:

* `OrthogonalBoundingDirections` → bounds are `IntBox` (2×`IntPoint` ll/ur). Used only by `ShapeSearchTree90Degree`.
* `FortyfiveDegreeBoundingDirections` → bounds are `IntOctagon`. Used by the default tree and the any-angle/45° trees (FortyfiveDegreeBoundingDirections.java:26-48: box→`to_IntOctagon()`, simplex/circle/polygon→`bounding_octagon()`).

`IntOctagon` layout (IntOctagon.java:23-40): 8 ints, all final —

```
lx  = x of left vertical border        rx  = x of right vertical border
ly  = y of bottom horizontal border    uy  = y of top horizontal border
ulx = x-axis intercept of upper-left  45° line   lrx = intercept of lower-right
llx = x-axis intercept of lower-left 135° line   urx = intercept of upper-right
```

plus a cached `precalculated_to_simplex`. `intersects(IntOctagon)` is 8 compare pairs, short-circuiting (IntOctagon.java:684+). `union` is componentwise min/max. `normalize()` clips the diagonal intercepts against the box so degenerate/overshooting octagons become canonical. `EMPTY` is the inverted octagon (all mins > maxes).

Exact tile shapes stored at the leaves are `TileShape` = one of `IntBox` | `IntOctagon` | `Simplex`. `Simplex` = intersection of half-planes: `Line[] arr` where each `Line` is a **directed line through two `IntPoint`s** (Line.java:10-30); interior is left of the line. All exact predicates on lines/points cascade `long` → `BigInteger`/`RationalPoint` on overflow (geometry/planar/{Point,Vector,Line,RationalPoint,RationalVector,BigIntDirection}.java).

### 1.3 Insertion (`MinAreaTree.insert`, MinAreaTree.java:52-120)

```
insert(Storable obj):                              # ShapeTree.java:33-43
    k = obj.tree_shape_count(this); if k <= 0 return
    for i in 0..k-1: leaf[i] = insert_one(obj, i)  # null shape → null leaf slot
    obj.set_search_tree_entries(leaf[], this)

insert_one(obj, i):                                # ShapeTree.java:46-61
    shape  = obj.get_tree_shape(this, i)           # lazily computed & cached (see §5)
    bounds = shape.bounding_shape(bounding_directions)
    leaf   = new Leaf(obj, i, null, bounds)
    insert_leaf(leaf); return leaf

insert_leaf(leaf):                                 # MinAreaTree.java:52-89
    ++leaf_count
    if root == null: root = leaf; return
    victim = position_locate(root, leaf):
        walk down; at each inner node:
            node.bounds = leaf.bounds ∪ node.bounds            # grow on the way (line 96)
            go to child with min( area(child.bounds ∪ leaf.bounds) − area(child.bounds) )
        until a Leaf is reached
    # splice: replace victim with a new InnerNode(victim.bounds ∪ leaf.bounds)
    #         whose children are {victim, leaf}
```

There is **no rebalancing, no node splits, no reinsertion** — tree quality depends purely on insertion order. `statistics()` (ShapeTree.java:113-137) exists to log avg/max depth.

### 1.4 Removal (`MinAreaTree.remove_leaf`, MinAreaTree.java:124-183)

O(depth), pointer surgery using the parent back-pointer (why `set_search_tree_entries` exists — removal never searches):

1. Null out the leaf's fields (bounding_shape/parent/object) and `--leaf_count`.
2. If parent == null → tree empty (`root = null`).
3. Otherwise the sibling replaces the parent at the grandparent (parent node is discarded, fields nulled to help GC).
4. Walk up from the grandparent recomputing `bounds = first.bounds ∪ second.bounds`, stopping as soon as the recomputed bounds don't shrink (`new_bounds.contains(old)` → break).

`ShapeTree.remove(Leaf[])` just loops `remove_leaf` (ShapeTree.java:98-105); null entries in the array are tolerated (`remove_leaf(null)` returns).

### 1.5 The core stabbing query (`MinAreaTree.overlaps`, MinAreaTree.java:27-50)

```
overlaps(RegularTileShape q) -> TreeSet<Leaf>:
    explicit DFS using a reused member ArrayStack<TreeNode> node_stack (initial capacity 10_000)
    push root; pop; if node.bounding_shape.intersects(q):
        leaf  -> add to TreeSet (sorted → deterministic iteration)
        inner -> push both children
```

Notes: the stack is a **member** reused across calls (`node_stack.reset()`), i.e. the tree object is not re-entrant/thread-safe; result is a sorted `TreeSet`, costing an O(log m) insert per hit purely for determinism.

---

## 2. ShapeSearchTree: the board-aware layer

`ShapeSearchTree extends MinAreaTree` (ShapeSearchTree.java:35). State: `final int compensated_clearance_class_no` (0 = uncompensated) and `final BasicBoard board`. v2 adds `final String key` = `"<Class>_<Directions>_cc<N>"` used only for logging (v2 ShapeSearchTree.java:63,80-92).

### 2.1 Clearance compensation — the exact rule

`ClearanceMatrix` (rules/ClearanceMatrix.java): N×N×layers int matrix; values forced **non-negative and even** on set (`value += value % 2`, :113-116); per-row and global per-layer running maxima maintained; `get_value(i,j,layer,add_safety_margin)` optionally adds `clearance_safety_margin = 16` internal units (:21,127-141). Class 0 is "null" (no clearance), class 1 "default".

```java
// ClearanceMatrix.java:211-213
clearance_compensation_value(cl, layer) = (get_value(cl, cl, layer, false) + 1) / 2   // = half the self-clearance

// ShapeSearchTree.java:77-86
tree.clearance_compensation_value(item_cl, layer) =
    max(0, matrix(item_cl, tree.compensated_class, layer)
           − matrix.clearance_compensation_value(tree.compensated_class, layer))
// returns 0 if item_cl <= 0 (the "null" class is never inflated)
```

**Why the split:** the routed object (trace/via being searched) is *itself* inflated by the half-self-clearance of the tree's class — `AutorouteControl.init_net` sets `compensated_trace_half_width[layer] = trace_half_width[layer] + matrix.clearance_compensation_value(trace_clearance_class_no, layer)` (AutorouteControl.java:149-152). Obstacle shapes in the compensated tree are inflated by `clearance(item_cl, tree_cl) − half_self_clearance(tree_cl)`. Sum = `trace_half_width + clearance(item_cl, tree_cl)` exactly. So in a compensated tree, *touching = clearance violation*, and all autoroute geometry (room completion, maze doors) can treat the routed trace as a zero-width point path with `compensated_trace_half_width` radius. This is the single most load-bearing idea in the whole subsystem.

The default tree (class 0) stores **uncompensated** shapes, unless the board has only classes {null, default}, in which case `SearchTreeManager.set_clearance_compensation_used(true)` rebuilds the default tree as class-1-compensated (SearchTreeManager.java:78-97) so even interactive routing gets the touching-=-violation property.

### 2.2 Query API (all in ShapeSearchTree.java)

All queries take `p_layer` (−1 = ignore layers) and `int[] p_ignore_net_nos` (an object is skipped if it is *not* an obstacle to **any** listed net, i.e. own-net objects are skipped).

1. **`overlapping_tree_entries(shape, layer, ignore_nets, out Collection<TreeEntry>)`** (:369-417) — the workhorse.
   * `bounds = shape.bounding_shape(bounding_directions)`; `overlaps(bounds)` (§1.5);
   * per leaf: layer filter → net filter → fetch exact `curr_shape = object.get_tree_shape(this, idx)` → exact `curr_shape.intersects(shape)`.
   * **Fast path** (:387,403-407): if the query shape is an `IntOctagon` *and* the stored shape is an `IntOctagon`, the exact test is skipped ("redundant") because octagon∩octagon was already decided by the bounding-shape test (bounding shape of an octagon in a 45° tree *is* the octagon itself).
2. **`overlapping_objects(shape, layer[, ignore_nets, out Set<SearchTreeObject>])`** (:330-350) — dedups entries to objects via `TreeSet`.
3. **`overlapping_tree_entries_with_clearance(shape, layer, ignore_nets, cl_type, out)`** (:430-493) — used **only when the tree is NOT compensated** (dispatcher at :500-519, :554-564 picks plain `overlapping_tree_entries` when compensated). Algorithm:
   * broad-phase bounds = `bounds.offset(1.2 × matrix.max_value(cl_type, layer))` (:449-454; the 1.2 factor is a fudge noted in the comment "a factor less than sqr2 has evtl. be added because enlarging is not symmetric");
   * candidates are wrapped in `EntrySortedByClearance{leaf, clearance, entry_id_no}` (:1068-1092; a static counter `last_generated_id_no` breaks ties — mutable static, not thread-safe) and sorted **ascending by required clearance**;
   * sweep in that order maintaining `curr_offset_shape = shape.enlarge(clearance/2)`, recomputing only when the half-clearance bucket changes (:476-483); each candidate's stored shape is also enlarged by the same half clearance (symmetric enlargement, :484-488) and tested with exact `intersects`. Note integer division `clearance / 2` — clearances are even by construction, so exact.
4. **`overlapping_objects_with_clearance` / `overlapping_items_with_clearance`** (:500-544) — set-dedup wrappers; the latter filters to `Item`.
5. **`complete_shape(...)`** — room completion, §4.
6. Maintenance entry points: `change_entries`, `merge_entries_in_front`, `merge_entries_at_end`, `reuse_entries_after_cutout`, `change_item_shape`, `reduce_trace_shape_at_tie_pin` — §6.
7. `validate_entries(item)` (:1052-1062) — asserts `leaf.shape_index_in_object == i`.

### 2.3 `enlarge` vs `offset`

`ConvexShape.offset(d)` is the exact Minkowski-ish offset for regular shapes (`IntBox.offset` grows the box; `IntOctagon.offset` grows all 8 params). `Shape.enlarge(d)` may change class (Shape.java:53-56); `Simplex.enlarge` translates each border line outward by d then intersects with the enlarged bounding octagon (Simplex.java:577-585) — i.e. an octagon-clipped offset, an over-approximation-free but corner-rounding-free enlargement.

---

## 3. Tile shape computation per item type (the obstacle model)

`Item` is `abstract` with `calculate_tree_shapes(ShapeSearchTree)` (Item.java:187) implemented by double dispatch: every subclass calls back `p_search_tree.calculate_tree_shapes(this)` so **the tree variant controls the geometry flavor** (PolylineTrace.java:143-145, DrillItem.java:207-209, ObstacleArea.java:180-182, BoardOutline.java:292-293).

Shape counts:

| Item | `tile_shape_count()` | layer of shape i |
|---|---|---|
| `PolylineTrace` | `max(polyline.arr.length − 2, 0)` — one shape per line **segment** (PolylineTrace.java:149-151) | trace layer for all i |
| `DrillItem` (Pin/Via) | `padstack.to_layer − from_layer + 1` — one per spanned layer (DrillItem.java:199-204) | `from_layer + i` |
| `ObstacleArea`/`ConductionArea` | count after convex decomposition + sectioning | area's layer |
| `BoardOutline` | per border line × layer count (or keepout pieces × layers) | encoded per shape |
| `ComponentOutline` | 0 — **not indexed** (ComponentOutline.java:99-105 returns 0 shapes) | — |

### 3.1 Traces — any-angle tree (`ShapeSearchTree.calculate_tree_shapes(PolylineTrace)`, :1003-1020)

`offset_width = half_width + clearance_compensation_value(trace_cl, layer)`; shape i = `polyline.offset_shape(offset_width, i)`.

`Polyline.offset_shapes(half_width, from, to)` (Polyline.java:363-520) — one convex `TileShape` per middle line of the polyline (lines are the primary representation; corners are line intersections):

1. Start from 4 lines: current line translated right and left by `half_width` (`translate(−half_width)` and `opposite().translate(−half_width)`), plus front/back cap lines derived from the neighbor lines — the neighbor line itself (translated) if the polyline turns left there, else its opposite, so consecutive segment shapes share a border line and their union covers the joint without gaps (:376-404).
2. **Dog-ear cutting** (:405-485): scans forward (j = i+2 …) and backward while corners stay within `sqrt(2·hw²)` of the joint, adding neighbor border lines as extra cut lines when the miter corner (`curr_line ∩ check_line`) pokes past them — prevents outstanding miter spikes at acute joints. Stops on direction-sense change.
3. `s1 = TileShape.get_instance(lines[4] ⊕ cut_lines)`; then intersect with a bounding regular shape of the segment: with `USE_BOUNDING_OCTAGON_FOR_OFFSET_SHAPES = true` (Polyline.java:19) that is `bounding_octagon(i−1, i).offset(half_width)`; result via `intersection_with_simplify`.

So an any-angle trace segment shape is a small `Simplex` (typically 4–8 border lines), octagon-clipped. The default (interactive) tree and the any-angle autoroute tree share this path.

### 3.2 Traces — 90° tree (`ShapeSearchTree90Degree.offset_shape/offset_shapes`, :396-413)

`polyline.offset_box(half_width, i)` = segment bounding box offset by half_width (Polyline.java:541-544) — an `IntBox`. (Correct only because the 90° router guarantees axis-parallel segments.)

### 3.3 Traces — 45° tree

**No override** — `ShapeSearchTree45Degree` inherits the any-angle `offset_shape` (Simplex). Room completion converts on the fly with `get_tree_shape(...).bounding_octagon()` (ShapeSearchTree45Degree.java:158). For 45°-restricted geometry the simplex *is* an octagon-shaped simplex, so `bounding_octagon` is tight.

### 3.4 Drill items (pins & vias)

Any-angle tree (ShapeSearchTree.java:870-899): per layer i, take padstack shape `get_shape(i)` (Pin applies rotation/mirror; may be `null` on unused inner layers), convert per board angle mode — `bounding_box()` (90°) / `bounding_octagon()` (45°) / `bounding_tile()` (any-angle exact convex hull) — then `enlarge(clearance_compensation_value(item_cl, layer_of(i)))`.
Nuance: the conversion mode follows `board.rules.get_trace_angle_restriction()`, not the tree subclass — so the default tree's pin shapes are octagons on a 45° board.

90° tree override (ShapeSearchTree90Degree.java:352-375): `bounding_box().offset(comp_value)` — `IntBox.offset`, exact.

45° tree override (ShapeSearchTree45Degree.java:427-454): `bounding_octagon()`, but **if that octagon is actually a box, use `bounding_box()` instead** "to avoid small corner cutoffs when taking the offset as an octagon" (:439-445); then `offset(comp_value)` and re-`bounding_octagon()`. (Offsetting a true octagon cuts its corners at 45°, which would complicate room division; offsetting a box keeps square corners.)

### 3.5 Obstacle / conduction areas (ShapeSearchTree.java:901-934)

`area.split_to_convex()` → per convex piece: `enlarge(comp_value)`, then `divide_into_sections(max_tree_shape_width)` where `max_tree_shape_width = 50000`, tightened to `min(500 × resolution(MIL), 50000)` when a host CAD exists (:909-916 — KiCad low-resolution workaround). `TileShape.divide_into_sections` grids the bounding box into ≤width sections and intersects (TileShape.java:904-924). Purpose: cap leaf shape size so huge planes don't ruin tree selectivity. 90°/45° overrides then coarsen each piece with `bounding_box()`/`bounding_octagon()` (90Degree:377-384, 45Degree:456-463). `ConductionArea.is_obstacle(net)` respects its `is_obstacle` flag (routing through planes allowed when false).

### 3.6 Board outline (ShapeSearchTree.java:936-984)

Two modes:
* `keepout_outside_outline_generated()`: index the precomputed outside-keepout area (convex pieces × every layer), enlarged by comp value.
* else: for each border line of each outline shape × each layer, build a 3-line polyline and index `offset_shape(half_width + comp_value, 0)` — i.e. the outline is a ring of thin segment shapes. **v19 bug/quirk:** comp value is always taken at layer 0 (`clearance_compensation_value(cl, 0)` at :950,975); **v2 fixes this to the actual `layerIndex`** (v2 ShapeSearchTree.java:955,978).

---

## 4. Room completion — how the autoroute engine consumes the tree

### 4.1 `complete_shape` (the negative-space query)

`Collection<IncompleteFreeSpaceExpansionRoom> complete_shape(room, net_no, ignore_object, ignore_shape)` — grows a maximal obstacle-free convex region on `room.layer` containing `room.contained_shape`, starting from `room.shape ∩ board.bounding_box`. Three implementations selected by tree subclass.

**Any-angle** (ShapeSearchTree.java:580-682): iterative DFS over the tree (reusing `node_stack`), pruned by a running `bounding_shape` = union of current candidate rooms' bounds (starts as the start shape's bounds and shrinks as obstacles carve the rooms — pruning gets *stronger* during the walk). For each leaf with `is_trace_obstacle(net_no) && shape_layer == room_layer && object != ignore_object`:

* for each current candidate room whose shape 2-dim-intersects the obstacle shape:
  * if obstacle is a `CompleteFreeSpaceExpansionRoom` and its intersection ⊆ `ignore_shape` (the 2-dim door to the from-room) → keep unchanged (prevents re-carving against the room we came from);
  * else replace the candidate with `restrain_shape(candidate, obstacle_shape)`.
* recompute the running bound over all survivors.

`restrain_shape` (:693-808), the geometric heart:

```
obstacle := obstacle_shape.to_Simplex()      # octagons → simplex so zero-length borders behave
contained := room.contained_shape.to_Simplex()
# Pass 1: among obstacle border lines whose SEGMENT crosses the room interior
#         (room_shape.is_intersected_interior_by), pick the one with MAX
#         contained.distance_to_the_left(line)  (i.e. contained fully right of it,
#         as far as possible)  → cut room with the OPPOSITE half-plane. One room out.
# Pass 2 (no such line ⇒ obstacle overlaps 'contained'): find a border line COLLINEAR
#         with 'contained' (splits it); cut the room into both half-planes; the far
#         piece keeps contained ∩ half-plane; recurse restrain_shape on the near piece.
# Rooms of dimension < 2 are dropped.
```

**45°** (ShapeSearchTree45Degree.java:95-249,276-359): same skeleton but everything is `IntOctagon`, and cutting is O(1) per line: `signed_line_distance` (:60-81) computes the separation of contained-octagon from each of the 8 obstacle border lines directly from the 8 ints, with **diagonal distances scaled by 0.5 "to prefer orthogonal … restraining lines slightly"** (:68-74; true diagonal distance factor would be 1/√2 ≈ 0.707); `obstacle_segment_touches_inside` (:32-58) checks the border segment against the room by 8 corner-side tests; `calc_outside/inside_restrained_shape` (:366-425) replace exactly one of the 8 octagon parameters and `normalize()`. Extra v19/v2 guard: drop result rooms whose `contained_shape.contains(shape)` "to prevent endless loop" (:247), and the ignore-shape branch also skips rooms fully inside the ignore shape (:186-189).

**90°** (ShapeSearchTree90Degree.java:37-350): `IntBox` version; `restrain_shape` tries the 4 obstacle edges that cross the room, picking max distance to `contained` (cut_line_distance starts at 0, strictly `>`); overlap case splits the room at the obstacle edge aligned with the intersection and recurses.

**`divide_large_room`** (ShapeSearchTree.java:1029-1050): if exactly 1 room results and its bbox is wider than half the board in **both** axes, split it into sections of `0.5 × max(board_h, board_w)` — "otherwise the maze search algorithm gets problems with vias". 45° override re-octagonizes results (45Degree:258-267); 90° doesn't call it at all (its `complete_shape` returns directly, :150).

### 4.2 Call sites and the real access mix

* `AutorouteEngine` constructor: `autoroute_search_tree = board.search_tree_manager.get_autoroute_tree(trace_clearance_class_no)` (AutorouteEngine.java:70) — clearance class comes from the routed net's class (`AutorouteControl.init_net`, AutorouteControl.java:136).
* `AutorouteEngine.complete_expansion_room` (:358-480): finds the 2-dim door to an already-complete neighbor room (→ `ignore_object`/`ignore_shape`), calls `complete_shape`, keeps the first 2-dim result, **re-runs `complete_shape` for every further result** (since the first inserted room becomes a new obstacle, :441-464). Each accepted room → `calculate_doors` then `autoroute_search_tree.insert(completed_room)` (:493).
* `SortedRoomNeighbours.calculate_neighbours` (SortedRoomNeighbours.java:150-…, ditto `Sorted45DegreeRoomNeighbours.java:101`, `SortedOrthogonalRoomNeighbours.java:131`): `overlapping_tree_entries(room_shape, layer)` (no net filter), **explicitly re-sorted by `(object.get_id_no(), shape_index)`** for determinism (v19 patch, :173-179), then for each entry: own-net non-obstacles are deferred as target-door candidates; others get exact `intersection` with the room shape; dimension 1 → neighbor door via `touching_sides`; dimension 0 → corner bookkeeping for the sorted-neighbour ring; dimension 2 only legal against `ObstacleExpansionRoom`s. The "sorted neighbours" (counterclockwise around the room border) then drive door creation and gap-filling with further `complete_shape` calls (SortedRoomNeighbours.java:439).
* `MazeSearchAlgo` (~:1656): small ad-hoc probes — builds a check shape around a door (`offset_shape` of a 3-line polyline with `compensated_trace_half_width + TRACE_WIDTH_TOLERANCE` radius) and calls `overlapping_objects(check_shape, layer, ignore_net_nos, out)` to test door passability/ripup contacts.
* `ExpansionDrill.java:50`: `overlapping_objects(search_shape, −1)` — all layers, for via drills.
* `DrillPage.java:67`: `overlapping_tree_entries(page_shape, −1, out)` for via-location pages.
* `CompleteFreeSpaceExpansionRoom.calculate_target_doors` / `.validate` (:172-…): `overlapping_tree_entries(room_shape, layer, [net_no], out)`.
* Interactive/board layer: `BasicBoard.overlapping_objects` (BasicBoard.java:976-978 — default tree), `pick_items` (point shape query, :1181-1194), `check_trace_shape`/`check_polyline_trace` (:1047-1137 — dispatcher on `is_clearance_compensation_used`), `Item.get_all_contacts` (Item.java:449-496), `Item.clearance_violations` (Item.java:313-415 — uses `overlapping_tree_entries_with_clearance` on the default tree, then symmetric-enlarge intersection + a 16-iteration bisection `calculate_clearance_between_two_shapes` to measure actual clearance), `RoutingBoard.additional_update_after_change` (below).

**Query-shape mix:** overwhelmingly (a) room-sized regular shapes (boxes/octagons) during room completion & neighbor scans, (b) segment-sized offset shapes during shove/check, (c) point shapes for picking. Result sets are typically small (units to tens). `complete_shape` is the hot query in autorouting; `overlapping_tree_entries` is the hot query in shoving/pull-tight.

---

## 5. Item ↔ tree wiring, caching, and board insert/remove paths

### 5.1 Per-item cache (`ItemSearchTreesInfo`)

`Item.search_trees_info` (transient, Item.java:48) → a **`LinkedList<SearchTreeInfo>`** where `SearchTreeInfo = {ShapeTree tree; Leaf[] entry_arr; TileShape[] precalculated_tree_shapes}` (ItemSearchTreesInfo.java:81-90). Every lookup is a linear scan comparing `tree` by identity (:26-31 etc.) — fine because ≤ a handful of trees exist.

* `Item.get_tree_shape(tree, i)` / `tree_shape_count(tree)` lazily compute-and-cache `calculate_tree_shapes(tree)` (Item.java:155-184). **Shapes are computed per (item, tree) pair once**, then shared by all leaves.
* `Item.clear_derived_data()` clears cached shapes for **all** trees + autoroute info (Item.java:1035-1040) — called on geometry change (`translate_by`, trace polyline change).
* `clear_search_tree_entries()` nulls the whole info (Item.java:1009-1011) — only on removal from *all* trees.
* `Item.change_clearance_class` reinserts the item into the trees only if compensation is used (Item.java:912-924).

### 5.2 `SearchTreeManager` — how many trees exist, and when

State: `Collection<ShapeSearchTree> compensated_search_trees` (LinkedList), `default_tree`, `boolean clearance_compensation_used` (SearchTreeManager.java:13-16).

* Construction: one default tree, `ShapeSearchTree(FortyfiveDegree, board, 0)` (:19-25).
* `insert(item)`: inserts into **every** active tree, then `item.set_on_the_board(true)` (:28-33). `remove(item)`: per tree, fetch the item's cached `Leaf[]` and `tree.remove(entries)`; then clear entries + `on_the_board=false` (:36-51). So *the set of active trees is maintained incrementally on every board mutation* — creating an autoroute tree makes every subsequent trace insert/rip ~2× tree work.
* `get_autoroute_tree(cl_class)` (:129-159): linear scan by `compensated_clearance_class_no`; on miss builds the angle-appropriate subclass — v19: `ShapeSearchTree90Degree` / `ShapeSearchTree45Degree` when `!rules.get_slow_autoroute_algorithm()` and angle mode matches, else the generic 45°-bounded tree; **v2 drops the slow-algorithm flag entirely** (v2 SearchTreeManager.java:140-172) — and bulk-inserts *all* board items by iterating `board.item_list`.
* Trees in practice: default tree (cc 0, or cc 1 with compensation on) + **one compensated tree per distinct trace-clearance class used by autorouted nets** (usually 1, i.e. 2 trees total during autorouting; more only with per-net-class clearance rules).
* Invalidation: `clearance_value_changed()` drops all non-default trees and, if compensating, reinserts everything (:100-108); `clearance_class_removed(no)` drops matching trees (:111-123); `reset_compensated_trees()` drops all but default (:168-170) — called when autorouting ends; `reinsert_tree_items()` full rebuild (:173-176). Deserialization rebuilds from scratch (BasicBoard.readObject, :1551-1560).
* `set_clearance_compensation_used(bool)` (:78-97): full remove-all → new default tree with cc 1 or 0 → reinsert-all (+ `clear_derived_data` per item in `insert_all_board_items`, :204).

**Memory cost per additional tree** (Java, 64-bit compressed oops, rough): per leaf ≈ Leaf(~40 B) + IntOctagon bound (~48 B) + share of inner nodes (another Leaf-sized node + bound each) ⇒ ~180 B/leaf; plus per (item,tree): `SearchTreeInfo` + `Leaf[]` + `TileShape[]` + the compensated `TileShape`s themselves — Simplexes dominate (each border `Line` = 2 `IntPoint` + Line ≈ 100 B; a 6-line simplex ≈ 700 B). A 10k-segment board is order 5–10 MB per extra tree, dominated by simplex shapes, not nodes.

### 5.3 Board storage & mutation paths (`BasicBoard`)

Item storage is `UndoableObjects item_list` — an undo-layered object list traversed by `start_read_object()/read_object(it)` cursors; all getters (`get_items`, `get_traces`, `get_pin`, …) are full linear scans (BasicBoard.java:644-857). There is **no id→item map**; `get_item(id)` is O(n) (:630-642).

* `insert_item(item)` (:1314-1344): clearance-class range check (fallback 0) → `item.board = this` → `item_list.insert` → `search_tree_manager.insert` → observers → `additional_update_after_change`.
* `remove_item(item)` (:602-624): `additional_update_after_change` **before** deletion (must invalidate rooms while shapes are still known) → `search_tree_manager.remove` → `item_list.delete` → observers.
* `undo`/`redo` (:1381-1454): `item_list.undo/redo` yields cancelled/restored object sets; cancelled → `search_tree_manager.remove`; restored → re-`insert` + `clear_autoroute_info`.
* `Item.move_by` (Item.java:250-257): save_for_undo → tree remove → translate (clears derived data) → tree insert.
* `RoutingBoard.additional_update_after_change` (RoutingBoard.java:70-93): when an autoroute engine with `maintain_database` is live, for each tree shape of the changed item: invalidate drill pages and `overlapping_objects(shape, layer)` → `remove_complete_expansion_room` for every `CompleteFreeSpaceExpansionRoom` hit (which in turn does `room.remove_from_tree(autoroute_search_tree)`, AutorouteEngine.java:343). This is the incremental free-space invalidation on every rip/insert during routing.

---

## 6. Incremental maintenance when traces change (the delicate part)

Trace edits during shove/pull-tight/combine are extremely frequent, so the code avoids full remove+reinsert with three surgical operations, each fanned out to all active trees by `SearchTreeManager` (:213-267) and implemented in `ShapeSearchTree`. All of them **manually rewrite `Leaf.object` and `Leaf.shape_index_in_object` in place** and rebuild the item's `Leaf[]`/shape caches, calling `set_precalculated_tree_shapes` **before** the `insert(obj, i)` calls "because it is used in this.insert" (the tree pulls shapes from the cache).

1. **`change_entries(trace, new_polyline, keep_at_start, keep_at_end)`** (ShapeSearchTree.java:95-140) — used when only a middle stretch of a polyline changed (pull-tight). Computes only the changed offset shapes via `offset_shapes(new_polyline, comp_half_width, keep_start, len−1−keep_end)`; keeps the first/last leaves untouched (re-indexing the tail leaves' `shape_index_in_object`), removes the middle leaves, inserts new middle leaves.
2. **`merge_entries_in_front` / `merge_entries_at_end`** (:147-284) — trace combination. Removes exactly 2 leaves (the joint-adjacent shape of each trace), computes the link shapes across the joint from the joined polyline, transfers all other leaves from the absorbed trace to the surviving trace **without touching the tree structure** (rewrites `leaf.object = to_trace` and new indices; handles orientation reversal via `change_order` when first/last corners coincide), inserts only the link leaves.
3. **`reuse_entries_after_cutout(from_trace, start_piece, end_piece)`** (:293-323) — the shove "cut a hole in a trace" fast path, driven by `ShapeTraceEntries.fast_cutout_trace` (ShapeTraceEntries.java:96-136: insert the two piece items into `item_list` *bypassing* `search_tree_manager.insert`, set `on_the_board` manually, transfer entries, then `remove_item(from_trace)` — whose tree-removal is a no-op-safe because transferred slots were nulled in `from_trace_entries`). Start piece keeps leading leaves + 1 new leaf at its cut end; end piece gets 1 new leaf at its cut start + trailing leaves. Guarded by shape-count/geometry preconditions at the call site (ShapeTraceEntries.java:77-80).
4. **`change_item_shape(item, shape_no, new_shape)`** (:851-868) — single-shape replacement; used by `reduce_trace_shape_at_tie_pin` (:815-844), which shrinks the trace's end shape by cutting out a tie-pin's shape (`trace_shape.cutout(pin_shape)`, keep the piece containing the second corner) so other nets of a multi-net (tie) pin remain routable.

Correctness invariant across all of these: for every tree and item, `entries[i].shape_index_in_object == i` and `precalculated_tree_shapes[i]` is the shape the leaf was inserted under (`validate_entries` checks the former).

---

## 7. Constants and thresholds (all values verified in source)

| Constant | Value | Where / meaning |
|---|---|---|
| `clearance_safety_margin` | 16 (internal units) | ClearanceMatrix.java:21; added by `get_value(..., true)` — used in the *uncompensated* sorted-clearance query (:471) and `BasicBoard.clearance_value` |
| clearance values | forced even, ≥0 | ClearanceMatrix.set_value:113-116 (`value += value % 2`) — makes `/2` exact |
| compensation split | `(self_clearance+1)/2` | ClearanceMatrix.java:211-213 |
| broad-phase clearance inflation | `1.2 × max_value(cl,layer)` | ShapeSearchTree.java:449-454 |
| `max_tree_shape_width` (area sectioning) | 50000; `min(500 × res(MIL), 50000)` with host CAD | ShapeSearchTree.java:909-916 |
| `divide_large_room` trigger / section | room bbox > board/2 in both axes; section = `0.5 × max(board_w, board_h)` | ShapeSearchTree.java:1029-1050 |
| diagonal preference factor (45° restrain) | 0.5 (instead of 1/√2) | ShapeSearchTree45Degree.java:68-74 |
| `node_stack` initial capacity | 10000 nodes | MinAreaTree.java:19 |
| dog-ear scan radius² | `2 × hw²` | Polyline.java:415 |
| `USE_BOUNDING_OCTAGON_FOR_OFFSET_SHAPES` | true | Polyline.java:19 |
| `DRILL_HOLE_CLEARANCE_MARGIN` (v2 only) | 10 | v2 ShapeSearchTree.java:52 |
| clearance-measure bisection iterations | 16 | Item.java:432 |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | (see maze area spec) added to compensated half-width in door probes | MazeSearchAlgo.java:1645 |

---

## 8. Edge cases catalog

* Null shapes: `DrillItem.get_shape(i)` may be null on unused padstack layers → null leaf slot (`insert` returns null; `remove` tolerates null entries). v2 replaces null with a synthetic hole obstacle (§9).
* Unbounded query shapes: `overlapping_tree_entries` warns and returns; `..._with_clearance` falls back to the board bounding box (ShapeSearchTree.java:444-448).
* `complete_shape` with degenerate start (`dimension < 2`): logged, no initial room; may return empty ⇒ caller logs "No shapes returned" (AutorouteEngine.java:410-416).
* `restrain_shape` "no cut line found": returns empty (space already occupied) — any-angle logs which algo failed (:769-777).
* Octagon borders of length 0: any-angle restrain converts obstacles `to_Simplex()` first specifically for this (:702-704).
* Tie pins: overlapping same-net traces at multi-net pins are excluded from violations (Item.clearance_violations:330-361) and trace end shapes are clipped (`reduce_trace_shape_at_tie_pin`).
* `EntrySortedByClearance` tie-break uses a **mutable static counter** with wraparound at `Integer.MAX_VALUE` (ShapeSearchTree.java:1070-1083) — a determinism kludge and a thread-safety hazard.
* `overlaps` / `complete_shape` reuse the member `node_stack` ⇒ one in-flight query per tree instance; queries must not recurse into other queries on the same tree.
* Items are compared descending by id (`Item.compareTo`), rooms descending too (CompleteFreeSpaceExpansionRoom.java:45-53) — result iteration order is *reverse* creation order.
* `ComponentOutline` contributes zero tree shapes; `BoardOutline` shapes exist per layer even though it is layer-independent.

## 9. v19 → v2 semantic deltas (beyond renames/package moves)

1. **Synthetic drill-hole obstacles** (the one real feature add): in all three `calculateTreeShapes(DrillItem)` variants, a null pad shape on a spanned layer is replaced by `drillHoleObstacle` — a `Circle(center, ceil(drillRadius))` (v2 ShapeSearchTree.java:876-899, 1012-1029) — and every drilled layer's offset gets `drillHoleClearanceDelta` added: `max(0, ceil(drillRadius + holeClearance + 10 − copperRadius − copperClearance))` where `holeClearance = board.rules.getHoleClearance()` (0 disables), `copperRadius` is the pad's center-to-border distance (drillRadius for hole-only padstacks), `copperClearance = matrix(item_cl, tree_cl_or_default, layer)` (:1035-1082). Net effect: other-net copper keeps `holeClearance` from the *hole*, not just from the pad. Hooked in v2 45°/90° variants at 45Degree:497,513 and 90Degree:443,452.
2. **Board outline compensation per layer** (v2 ShapeSearchTree.java:955,978) vs constant layer 0 in v19 (:950,975).
3. **`get_slow_autoroute_algorithm` removed**: v2 `getAutorouteTree` always picks the angle-specialized tree (v2 SearchTreeManager.java:148-160).
4. `ShapeSearchTree.key` string id for logging; `SearchTreeObject.getId()` rename of `get_id_no`.
5. `PolylineTraceSearchTreeAdapter` — no behavior change, just extracts the trace↔manager calls (`replaceGeometry` documents the mandatory remove → mutate → clear-derived → insert order).
6. v2 `BasicBoard` facade splits query helpers into `RoutingBoardSearchFacade`/`BoardConnectivityQueries`, same semantics.

---

## 10. Rust design implications

### Keep semantically (conformance-critical)

* The **compensated-tree contract**: obstacle inflation = `clearance(item_cl, tree_cl) − (self_clearance(tree_cl)+1)/2`, router radius = `half_width + (self_clearance+1)/2`, even-clearance normalization, class 0 never inflated. All downstream autoroute geometry assumes touching == violation.
* One tree per (bounding-direction flavor × compensated class); shapes per (item, tree) cached once and shared by leaves; leaves keyed `(object, shape_index)` with back-pointers for O(depth) removal.
* Per-item shape decomposition and counts (trace: one shape per segment with shared joint border lines + dog-ear cuts + octagon clip; drill item: one per layer with the per-angle box/octagon/tile conversion incl. the 45° "octagon-that-is-a-box → keep box" rule; area sectioning at 50000/500-mil; outline ring shapes; v2 hole obstacles with margin 10).
* `complete_shape`/`restrain_shape` cut-line selection rules exactly (max signed distance; 0.5 diagonal preference in 45°; the ignore-shape door exemption; `divide_large_room`; the 45° `contained.contains(shape)` endless-loop guard) — routing results are highly sensitive to these.
* Deterministic ordering: stable ids and the `(id, shape_index)` sort of neighbor scans. In Rust, sort explicitly rather than relying on set iteration order; drop the `EntrySortedByClearance` static-counter hack in favor of `(clearance, id, shape_index)` keys.
* Lazy bound tightening on removal (stop when bounds stop shrinking) — cheap and adequate.

### Restructure

* **Arena/SoA nodes.** Replace pointer-linked `TreeNode` objects with a slab: `nodes: Vec<Node>` where `Node { bounds: OctBounds([i32; 8]), parent: u32, kind: enum { Inner{left,right}, Leaf{object: ItemId, shape_idx: u16} } }`, free-list for removals. Bounds inline (32 B) → the DFS touches one cache line per node instead of chasing `IntOctagon` heap objects. `IntBox` trees can use the same 8-int layout (box = degenerate octagon) to keep one code path, as Java's 45° default tree effectively does.
* **Query without allocation.** Java allocates per query: `TreeSet<Leaf>` (MinAreaTree.java:28), `LinkedList<TreeEntry>` + a `TreeEntry` per hit (ShapeSearchTree.java:332,412), `TreeSet<SearchTreeObject>` dedup (:347), plus `RegularTileShape` unions on every `position_locate` step (MinAreaTree.java:102-111 creates two union octagons per level per insert). In Rust: caller-provided `&mut Vec<(ItemId,u16)>` results, sort+dedup in place; compute union *areas* without materializing union shapes (pure i64 arithmetic on the 8 params).
* **Reentrancy/parallelism.** Kill the member `node_stack`; use a per-query SmallVec. Then reads are `&self` and naturally parallel. The batch autorouter's per-net engines share one compensated tree; with interior mutability only at insert/remove (rooms!), an RwLock or an epoch scheme works — but note **room insertion happens mid-search** (`AutorouteEngine.add_complete_room` → tree insert), so either keep rooms in a separate per-engine tree/index (they are engine-private anyway and are all discarded on `reset_compensated_trees`) — this is the cleanest parallelism win: static items tree = read-only during routing; rooms tree = engine-local — or accept a locked shared tree as upstream does implicitly (Java v2 runs engines that share the tree under a coarse lock elsewhere).
* **Shape storage.** `TileShape` polymorphism → tagged enum `{ Box(IntBox), Oct(IntOctagon), Simplex(SmallVec<[Line; 8]>) }`; per-(item,tree) shape arrays in an arena keyed by dense item ids, replacing the `LinkedList<SearchTreeInfo>` linear scans (ItemSearchTreesInfo.java:26-31) with a `[Option<TreeSlot>; MAX_TREES]` inline array (tree count ≤ 3-4 in practice).
* **Item registry.** Replace `UndoableObjects` linear scans (`get_item` O(n), `get_pin` O(n)) with a slotmap + generation, keeping the undo-layer semantics as a separate journal.
* Consider bulk-loading (STR pack) when `get_autoroute_tree` builds a new tree from the full board — Java inserts in arbitrary `item_list` order, which is exactly the worst case for the greedy min-area heuristic; a packed build is both faster and yields better trees, and is behavior-safe because tree *shape* never affects results, only ordering (which must be re-sorted anyway).

### Java-isms observed (file:line evidence)

* Per-call churn: `MinAreaTree.overlaps` allocates a `TreeSet` + tree-node boxes per query (MinAreaTree.java:28); `position_locate` allocates 2 union `RegularTileShape`s per descent level (:102-110); `overlapping_tree_entries` a `TreeEntry` per hit (ShapeSearchTree.java:412); `complete_shape` allocates new `LinkedList`s + `IncompleteFreeSpaceExpansionRoom`s per obstacle iteration (:627-671) and recomputes `bounding_shape(...)` unions of every surviving room per obstacle (:659-668, quadratic-ish in room count).
* BigInt-capable exact predicates sit on the hot path: every exact `Simplex.intersects` / `side_of` may promote to `BigInteger`/`RationalPoint` (geometry/planar/Line.java, Point.java, RationalPoint.java); the 45°/90° specializations exist precisely to bypass this — in Rust, i64 covers all products of coordinates bounded by `Limits.CRIT_INT`, use i128 as overflow tier, never heap bignums.
* Clone/copy-heavy: `BasicBoard.clone()` is serialize→deserialize of the whole board (BasicBoard.java:126-152) with a full tree rebuild in `readObject` (:1551-1560); `SearchTreeManager` rebuild paths do full remove-all/insert-all on clearance edits (:100-108,78-97).
* Mutable statics: `ShapeSearchTree.last_generated_id_no` (:37,1070-1083).
* Debug landmines to drop: hard-coded debug anchors and trace logging inside the hot loops (`ShapeSearchTree45Degree`:120-236, `ShapeSearchTree90Degree`:62-143, `BasicBoard`:1352-1356, `AutorouteEngine`:374-407) — in this checkout they run string concatenation only behind boolean guards, but the anchor checks themselves execute per leaf.
