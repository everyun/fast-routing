# Freerouting Maze Expansion Engine — Implementation Spec for the Rust Rewrite

Scope: the core maze search that finds **one** connection (one net's two-point link) on a
shape-based (not grid-based) routing model. Covers rooms, doors, drills, the cost model, the
priority queue, target capture, ripup-during-search, and state lifecycle between connections.

Source trees studied (read in full):

* **v1.9 (classic)** — `/tmp/fr-java/src_v19/main/java/app/freerouting/autoroute/`
  `MazeSearchAlgo.java` (1730 ln), `MazeListElement.java`, `MazeSearchElement.java`,
  `ExpansionRoom.java`, `FreeSpaceExpansionRoom.java`, `IncompleteFreeSpaceExpansionRoom.java`,
  `CompleteFreeSpaceExpansionRoom.java`, `ObstacleExpansionRoom.java`, `CompleteExpansionRoom.java`,
  `ExpansionDoor.java`, `TargetItemExpansionDoor.java`, `ExpandableObject.java`,
  `ExpansionDrill.java`, `DrillPage.java`, `DrillPageArray.java`, `DestinationDistance.java`,
  `AutorouteEngine.java`, `AutorouteControl.java`, `SortedRoomNeighbours.java`,
  `Sorted45DegreeRoomNeighbours.java`, `SortedOrthogonalRoomNeighbours.java`, `ItemAutorouteInfo.java`.
* **v2.x (2.3.0 line)** — `/tmp/fr-java/src/main/java/app/freerouting/autoroute/`
  `maze/` (`MazeSearchEngine`, `MazeExpansionEngine`, `MazeRipupResolver`, `MazeTraceShover`,
  `MazeListElement`, `MazeSearchElement`, `AutorouteEngine`, `AutorouteControl`,
  `DestinationDistance`, `MazeFanoutDiagnostics`), `expansion/` (all room/door classes +
  the three `Sorted*RoomNeighbours`), `drill/` (`DrillPage`, `DrillPageArray`, `ExpansionDrill`),
  `pipeline/AutorouteConnectionRouter.java`, `board/facade/RoutingBoard.java` (engine lifecycle).

**Which one the 2.3.0 release uses:** the v2 tree contains no `MazeSearchAlgo`; the pipeline
(`pipeline/AutorouteConnectionRouter.java:6,76-90`) imports `autoroute.maze.AutorouteEngine`, which
calls `MazeSearchEngine.getInstance` (`maze/AutorouteEngine.java:130-146`). So 2.3.0 runs the
`maze/*` + `expansion/*` + `drill/*` classes. They are a **camelCase rename + package split of
v1.9** with a handful of real behavioral additions, all itemized in §12.

> ⚠️ Provenance caveat: both trees in `/tmp/fr-java` carry local debug instrumentation that is not
> in stock upstream (e.g. hardcoded `net_no == 33 || 66 || 67` traces in
> v19 `AutorouteEngine.java:131`, `FANOUT_DIAG` blocks in v19 `MazeSearchAlgo.java:1042-1078`, and
> deterministic tie-breakers appended to several comparators). The algorithms themselves are
> upstream; the tie-breakers are noted where they matter because a Rust port should keep them
> (they fix nondeterminism bugs of the original float-only comparators).

---

## 1. Big picture

Freerouting's router is a **shape-expansion (tile-expansion) maze router**, not a grid router. The
free space of each layer is decomposed **lazily, on demand** into maximal convex tiles ("rooms").
Adjacent rooms share 1-dimensional boundary segments ("doors"). The search is a Dijkstra/A*
best-first expansion over **door sections** (not rooms): each door is subdivided into ≤10×trace-width
sections, and each section is a graph node that can be occupied exactly once. Layer changes happen
through "expansion drills" (candidate via sites), which are cached per rectangular "drill page".
Obstacles belonging to routable items are themselves modeled as rooms (`ObstacleExpansionRoom`) so
the search can plan to **rip up** or **shove** them, paying an extra cost.

Per-connection flow (v19 `AutorouteEngine.autoroute_connection`, v2 identical shape):

```
MazeSearchAlgo.get_instance(start_items, dest_items, engine, ctrl)   // builds start frontier
result = maze.find_connection()                                      // best-first loop
conn   = LocateFoundConnectionAlgo.get_instance(result, ...)         // backtrack → polyline corners
engine.clear() or engine.reset_all_doors()                           // per maintain_database
delete ripped connections; remove trace tails; InsertFoundConnectionAlgo // mutate board
```

The maze search itself never mutates the board; ripups are *planned* (recorded per door section via
`room_ripped`/`ripup_cost`) and executed only after backtracking, by
`LocateFoundConnectionAlgo`/`InsertFoundConnectionAlgo` (out of scope for this spec, covered by the
locate-insert spec).

---

## 2. Rooms — what they are geometrically

All room shapes are `TileShape`s: convex intersections of half-planes. Concrete subtype depends on
the angle restriction: `IntBox` (90°), `IntOctagon` (45°), `Simplex` (any-angle). One room lives on
exactly one layer.

### 2.1 Class hierarchy (v19 names; v2 = same under `expansion/`)

```
interface ExpansionRoom            // doors list, shape, layer, id
 ├─ abstract FreeSpaceExpansionRoom            (shape, layer, List<ExpansionDoor> doors)
 │   ├─ IncompleteFreeSpaceExpansionRoom       (+ contained_shape)      // shape not final yet
 │   └─ CompleteFreeSpaceExpansionRoom          implements CompleteExpansionRoom, SearchTreeObject
 │        (+ id_no, ShapeTree.Leaf[] tree_entries, Collection<TargetItemExpansionDoor> target_doors,
 │           boolean room_is_net_dependent)
 └─ ObstacleExpansionRoom           implements CompleteExpansionRoom
      (Item item, int index_in_item, TileShape shape = item tree shape, doors, doors_calculated)
interface CompleteExpansionRoom extends ExpansionRoom { target_doors; get_object(); }
```

Invariants:

* An **IncompleteFreeSpaceExpansionRoom** has a *lower bound* shape (`shape`, possibly `null` =
  whole plane, or a half-plane) and a `contained_shape` that MUST remain inside the final completed
  shape (v19 `FreeSpaceExpansionRoom.java:20-27`). Incomplete rooms are placeholders hanging off
  doors of completed rooms ("there is free space beyond this edge, not yet computed").
* A **CompleteFreeSpaceExpansionRoom** is a maximal obstacle-free convex tile. It is inserted into
  the autoroute shape search tree as a `SearchTreeObject` and reports itself as an obstacle
  (`is_obstacle`/`is_trace_obstacle` return true, v19 `CompleteFreeSpaceExpansionRoom.java:77-84`)
  so that later room completions tile *around* it instead of overlapping it. Free-space rooms on the
  same layer never overlap 2-dimensionally (checked by `validate()`, `:168-196`).
* `room_is_net_dependent` is set when the completed room's shape was computed ignoring same-net
  items or when it has target doors (`SortedRoomNeighbours.calculate_target_doors:125-127`); such
  rooms must be discarded when the routed net changes (§10).
* An **ObstacleExpansionRoom** wraps one *tree shape* of one routable `Item` (one convex piece of a
  trace, or a via padstack shape on a layer). Its id is `(item_id << 10) | index_in_item`
  (v19 `ObstacleExpansionRoom.java:50`). Created lazily and cached per item in
  `ItemAutorouteInfo.expansion_room_arr` (`ItemAutorouteInfo.java:50-63`) — an
  `ObstacleExpansionRoom[tree_shape_count]` array on the item, so identity is stable across the
  whole search. `doors_calculated` marks whether its neighbour doors were computed.

### 2.2 Doors

`ExpansionDoor implements ExpandableObject` (v19 `ExpansionDoor.java`):

* `first_room`, `second_room` (either side; either may be incomplete), `dimension` ∈ {1, 2}.
* `get_shape()` = intersection of the two room shapes, **recomputed on every call** (`:37-42`) —
  hot-path allocation, see §13.
* dimension 1 = shared boundary segment (the normal case). dimension 2 = area overlap, allowed only
  (a) between two ObstacleExpansionRooms of the same net (`ObstacleExpansionRoom.create_overlap_door`,
  `:75-100`: both routable, share a net; for two pieces of the same `PolylineTrace` only consecutive
  segments `|i-j|==1` get a door), and (b) in 90°/45° mode between free rooms overlapping at a
  corner (door segment then = the "restraint line" between the two shapes,
  `ExpansionDoor.calc_door_line_segment:139-166`).
* **Sections**: `section_arr: MazeSearchElement[]`, allocated lazily by `get_section_segments(half_width)`
  (`:100-137`):
  1. `offset = half_width + TRACE_WIDTH_TOLERANCE(=2)`.
  2. Compute the door line segment: dim-1 → `door_shape.diagonal_corner_segment()`; dim-2 between two
     complete free rooms → restraint line (empty result if one room inside the other, or segment
     length < 2·offset → `[]`, "2-dim small doors are not expanded"); otherwise (obstacle overlap
     doors) → degenerate point segment at the door shape's centre of gravity.
  3. Shrink the segment by `offset` at each end (`shrink_segment`).
  4. `section_count = floor(len(unshrunk) / (10*offset)) + 1` — i.e. **max section width =
     10 × (half_width + 2)** (`c_max_door_section_width`, `:129`).
  5. `allocate_sections(section_count)`: reuse `section_arr` if the count matches, else reallocate.
  6. Return the shrunk segment divided into `section_count` equal sections (`FloatLine[]`).
* `reset()` clears every section's `MazeSearchElement` (backtrack info + occupied flag).

`TargetItemExpansionDoor` (v19 file): door from a room to a connectable start/destination item.
Fields `item`, `tree_entry_no` (which tree shape of the item), `room`, precomputed
`shape = item_tree_shape ∩ room_shape`, and a **single** `MazeSearchElement`. `dimension()==2`.
`is_destination_door()` = `!item.get_autoroute_info().is_start_info()` — i.e. start/destination
classification lives on the *item*, set during `init` (§5).

`ExpandableObject` interface (what the queue actually points at): `get_shape`, `get_dimension`,
`other_room(CompleteExpansionRoom)`, `maze_search_element_count`, `get_maze_search_element(i)`,
`reset`, `get_id_no`. Implementors: `ExpansionDoor`, `TargetItemExpansionDoor`, `ExpansionDrill`,
`DrillPage`.

---

## 3. On-demand free-space room computation

### 3.1 Who calls `complete_expansion_room`

`AutorouteEngine.complete_expansion_room(IncompleteFreeSpaceExpansionRoom)` (v19
`AutorouteEngine.java:358-480`) is invoked from:

1. `MazeSearchAlgo.init` — completing the **start rooms** seeded from start items (§5).
2. `AutorouteEngine.complete_neighbour_rooms(room)` (`:543-568`) — called from
   `MazeSearchAlgo.expand_to_room_doors` (`MazeSearchAlgo.java:313`) each time the search enters a
   room, before enumerating its doors: for every door of the entered room whose other side is an
   `IncompleteFreeSpaceExpansionRoom`, complete it (and **restart the door iterator**, because
   completing a neighbour mutates the entered room's door list); for every neighbouring
   `ObstacleExpansionRoom` with `!all_doors_calculated()`, run `calculate_doors` on it once and set
   the flag. This is the "doors of this room will not change later on" guarantee that makes it safe
   to iterate and expand them.
3. `ExpansionDrill.calculate_expansion_rooms` (§7.1) — to obtain a room at the drill point on every
   layer.

### 3.2 The completion algorithm

```
complete_expansion_room(p_room):
  # find "from door": a 2-dim door to an already-complete free room; that neighbour and its
  # door shape are passed to complete_shape as (ignore_object, from_door_shape)
  for door in p_room.doors:
      if other side is CompleteFreeSpaceExpansionRoom and door.dimension == 2:
          from_door_shape = door.get_shape(); ignore_object = that room; break

  completed_shapes = autoroute_search_tree.complete_shape(p_room, net_no, ignore_object, from_door_shape)
      # ShapeSearchTree.complete_shape: shrink p_room.shape around p_room.contained_shape until
      # it overlaps no obstacle for net_no in the compensated tree (this is the O(obstacles)
      # geometric core; lives in board/ShapeSearchTree*, outside this spec's file set).
      # May return several disjoint candidate shapes.

  remove_incomplete_expansion_room(p_room)          # detach doors, drop from incomplete list
  for (i, cand) in completed_shapes where cand.shape.dimension == 2:
      if i == 0:
          result += add_complete_room(cand)
      else:
          # first room's shape may have changed during door calc; re-complete the rest
          for r in tree.complete_shape(cand, net_no, ignore_object, from_door_shape):
              result += add_complete_room(r)
  return result       # Collection<CompleteFreeSpaceExpansionRoom>
```

`add_complete_room` (`:483-503`) = `calculate_doors(room)` (dispatch below) → if resulting shape is
2-dim: append to `complete_expansion_rooms` list, insert into the autoroute search tree.

`calculate_doors` dispatch (`:526-536`): 90° tree → `SortedOrthogonalRoomNeighbours.calculate`,
45° tree → `Sorted45DegreeRoomNeighbours.calculate`, else `SortedRoomNeighbours.calculate`.
(v2 folds this dispatch into `expansion/SortedRoomNeighbours.selectCalculationMode`.)

### 3.3 SortedRoomNeighbours (any-angle; the reference variant)

`calculate(p_room, engine)` (v19 `SortedRoomNeighbours.java:52-99`):

1. **calculate_neighbours** (`:151-298`): make the `CompleteFreeSpaceExpansionRoom` (new id from
   `engine.generate_room_id_no()`, a monotonically increasing per-engine counter) or reuse the
   ObstacleExpansionRoom being completed. Query the search tree for all tree entries overlapping the
   room shape on its layer. (Both trees sort the overlap list by `(object_id, shape_index)` for
   determinism — local addition, `:173-179`.) For each overlapping object:
   * object == room itself → skip.
   * completing a free room and object is **not a trace obstacle for this net** (i.e. same-net item)
     → defer into `own_net_objects` (target-door candidates; processed only after the shape is
     final).
   * else intersect shapes; by intersection dimension:
     * **2** → only legal for obstacle-room completion: `create_overlap_door` with the other item's
       ObstacleExpansionRoom (fetched via `ItemAutorouteInfo.get_expansion_room`).
     * **1** → record a `SortedRoomNeighbour` (with the touching side indices of both shapes,
       `room_shape.touching_sides`), and insert an `ExpansionDoor(room, neighbour, 1)` unless
       `insert_door_ok` rejects it. Neighbour room = the object itself if it's an `ExpansionRoom`
       (a previously completed free room), else the item's ObstacleExpansionRoom (only if
       `item.is_routable()` — non-routable obstacles get no door at all: they are pure walls).
     * **0** (point touch) → record a SortedRoomNeighbour with corner bookkeeping (needed to build
       correct gap rooms), no door.
2. **try_remove_edge** (`:386-483`): if some border line of the (incomplete) room shape has *no*
   touching neighbour, that edge is not obstacle-constrained → remove that border line from the
   simplex, re-run `complete_shape` on the enlarged shape, and if the area strictly grew, replace
   the room shape and **restart `calculate` from scratch** (doors removed via
   `engine.remove_all_doors`). Guarantees maximal tiles and termination via the strict area
   increase. Orthogonal variant: per-side `edge_interior_touches_obstacle[4]` flags with the box
   edge pushed to ±`Limits.CRIT_INT` (`SortedOrthogonalRoomNeighbours.java:211-222,425-588`; it also
   picks the largest-area 2-dim door as `ignore_object/ignore_shape`). 45° variant: same with 8
   flags, removing **all** untouched octagon edges at once
   (`Sorted45DegreeRoomNeighbours.java:177-237,299-417`).
3. **calculate_new_incomplete_rooms** (`:488-643`): walk the neighbours sorted counterclockwise
   around the border. For every gap between consecutive neighbours' touch intervals, build one or
   more `IncompleteFreeSpaceExpansionRoom`s on the far side of the border edge(s) spanning the gap:
   shape = half-plane(s) bounded by the flipped border line (plus optional flipped start/end
   neighbour edges when the corner is convex), `contained_shape` = `completed_room.shape ∩ new_shape`
   (that's the 1-dim door segment area), and a dim-1 `ExpansionDoor` linking them. Special case in
   the any-angle variant: when a gap spans a corner of the room in open space, a **cut line**
   from prev neighbour's last corner to next neighbour's first corner *shrinks the completed room
   itself* (corner cut-off, `:543-565`) to keep incomplete-room geometry consistent.
   The sort comparator (`SortedRoomNeighbour.compareTo`) orders by touching side then distance of
   first corner along the border, with careful tie-breaks (`:694-740`); orthogonal/45° variants use
   side-index + coordinate comparisons.
   Empty-neighbour ObstacleExpansionRoom → one incomplete room per border line
   (`calculate_incomplete_rooms_with_empty_neighbours:101-119`), 4 board-quadrant boxes in the
   orthogonal variant.
4. **calculate_target_doors** (`:121-149`): for each deferred same-net tree entry that is
   `Connectable` and contains the current net: `connection_shape =
   item.get_trace_connection_shape(tree, shape_index)`; if it intersects the room shape, add a
   `TargetItemExpansionDoor(item, shape_index, room)` and mark the room net-dependent.

`insert_door_ok` (`:301-360`): no duplicate door to the same neighbour; obstacle↔obstacle doors only
if the items share a net; for a door touching the **first or last** tile of a `PolylineTrace`, the
door line must be parallel to the trace's end line (prevents entering a fork sideways during ripup
planning).

### 3.4 Removing a completed room

`remove_complete_expansion_room` (v19 `AutorouteEngine.java:316-351`): for each neighbour, detach
the shared door and, if the intersection is 1-dim, plant a fresh half-plane incomplete room + door
on the neighbour (so the neighbour's frontier is repaired); then remove all doors, remove from the
search tree, remove from the list, and `drill_page_array.invalidate(room_shape)` (cached drills may
have depended on that free space). Called when net-dependent rooms are invalidated on a net change
(§10) and from `RoutingBoard.additional_update_after_change` when items change under
`maintain_database` (v2 `RoutingBoard.java:100-113`).

---

## 4. The priority queue and MazeListElement lifecycle

### 4.1 Data structures

`MazeListElement` (immutable except `ripup_cost`; v19 file, v2 rename only):

| field | meaning |
|---|---|
| `door: ExpandableObject` | the door/drill/drill-page/target-door being entered |
| `section_no_of_door` | section index (door section, drill layer index, or drill-page layer) |
| `backtrack_door`, `section_no_of_backtrack_door` | predecessor (for path reconstruction) |
| `expansion_value: double` | g — weighted cost from start |
| `sorting_value: double` | f = g + h (admissible lower bound to destination) |
| `next_room: CompleteExpansionRoom` | room that will be expanded when this element is popped (null for drills) |
| `shape_entry: FloatLine` | segment on the door where the path crosses (cost is measured between segment midpoints) |
| `room_ripped: bool` | path so far rips an obstacle room |
| `adjustment: {NONE, RIGHT, LEFT}` | trace-shove adjustment state (§8.3) |
| `already_checked: bool` | ripup/shove checks already done for this element (re-queued elements) |
| `ripup_cost: int` (mutable) | cost paid at *this* door (>0 only if this door directly caused a ripup) |

The queue: `maze_expansion_list = TreeSet<MazeListElement>` ordered by `compareTo`
(`MazeListElement.java:71-104`): `sorting_value`, then `expansion_value`, then `door.get_id_no()`,
then `section_no_of_door`; equal on all four ⇒ treated as duplicate and dropped by the TreeSet.
(Stock upstream compared only sorting_value/expansion_value; the id tie-breaks are the local
determinism fix. Keep them.) **This is lazy-deletion Dijkstra**: elements are never decreased in
place; a better path to a door section simply inserts another element, and stale ones are skipped
on pop because the section is already occupied.

`MazeSearchElement` — the *per-door-section persistent node state* (embedded in the door):
`is_occupied`, `backtrack_door`, `section_no_of_backtrack_door`, `room_ripped`, `adjustment`,
`ripup_cost`, and `reset()`. This is what survives after the search and is read by the backtracker.

### 4.2 The main loop

```
find_connection():
    while occupy_next_element(): pass
    return destination_door? Result(destination_door, section_no) : null

occupy_next_element():                                        # MazeSearchAlgo.java:209-277
    if destination_door != null: return false
    loop:                                                     # pop until unoccupied section found
        if queue empty: return false
        if engine.is_stop_requested(): return false           # time limit / user stop
        e = queue.pop_min()
        sec = e.door.get_maze_search_element(e.section_no_of_door)
        if !sec.is_occupied: break
    # settle: copy backtrack info into the door section (occupied flag NOT yet set)
    sec.{backtrack_door, section_no_of_backtrack_door, room_ripped, ripup_cost, adjustment} = e.*

    if e.door is DrillPage:            expand_to_drills_of_page(e); return true
    if e.door is TargetItemExpansionDoor and it.is_destination_door():
                                        destination_door = e.door; section = e.section; return false
    if ctrl.is_fanout and e.door is ExpansionDrill and e.backtrack_door is ExpansionDrill:
                                        destination_door = e.door; ...; return false   # fanout stops at first via
    if ctrl.vias_allowed and e.door is ExpansionDrill and e.backtrack_door is not ExpansionDrill:
        expand_to_other_layers(e)                              # §7.3
    if e.next_room != null:
        if !expand_to_room_doors(e): return true               # delayed occupation (§8.3) or nothing expanded
    sec.is_occupied = true
    return true
```

Subtle invariant: when `expand_to_room_doors` returns false the section is left **unoccupied** so a
different path (another section of the same door, or a shove from the other side) may claim it
later; but the backtrack info written above stays (it will be overwritten on the next settle).

### 4.3 expand_to_room_doors (per-room expansion, `MazeSearchAlgo.java:283-524`)

Pseudocode with all the checks, in order:

```
layer = next_room.layer
if !ctrl.layer_active[layer] and layer is a signal layer: return true       # dead end, occupy
half_width = ctrl.compensated_trace_half_width[layer]

# "small door" test (only for ExpansionDoor entries)
if door is ExpansionDoor:
    hw_add = half_width + 2
    if ctrl.with_neckdown: hw_add = min(hw_add, neckdown_halfwidth(dest pin in next_room)); half_width may shrink
    curr_door_is_small = door_is_small(door, 2*hw_add)
        # door_is_small (:650-679): for dim-1 doors or free↔free dim-2 doors, door length
        # (bbox max_width / bounding-octagon max_width / diagonal segment length, per angle mode)
        # < trace width ⇒ small.

engine.complete_neighbour_rooms(next_room)          # freeze this room's door list (§3.1)
entry_mid = midpoint(e.shape_entry)
if ctrl.with_neckdown and door is TargetItemExpansionDoor(start pin):
    half_width = min(half_width, pin.neckdown_halfwidth(layer))

# thickness of next_room relative to the trace
if next_room is ObstacleExpansionRoom:
    thick = room_shape_is_thick(room)               # obstacle half width (+ compensation) >= trace half width
else:
    thick = shape.min_width() >= 2*half_width
    and (for 1-dim non-small doors, unless already_checked)
        second-nearest border point of the room shape from entry_mid is > half_width+1 away
        # i.e. the trace disc actually fits at the entry point

if !layer_active and door is ExpansionDrill:        # split-plane rule
    if any CONDUCTION item at drill location on this layer is foreign-net: return true

something_expanded = expand_to_target_doors(e, thick, small, entry_mid)     # §6

if !layer_active: return true                       # plane layer: targets only, no through-routing

ripup_costs = 0
if next_room is FreeSpaceExpansionRoom:
    if !already_checked and small:
        # forbidden unless we are exiting a ripped item through the small gap
        if !(thick and check_leaving_ripped_item(e)): return something_expanded
elif next_room is ObstacleExpansionRoom obstacle:
    if !already_checked:
        if ctrl.ripup_allowed:
            ripup_costs = check_ripup(e, obstacle.item, small)               # §8.1; -1 = can't rip
            room_rippable = ripup_costs >= 0
        if ripup_costs != ALREADY_RIPPED_COSTS(1) and thick
           and !small and ctrl.max_shove_trace_recursion_depth > 0
           and obstacle.item is PolylineTrace:
            if !shove_trace_room(e, obstacle):                              # §8.3
                if ripup_costs > 0:
                    requeue e with +ripup_costs on both g and f, already_checked=true,
                            room_ripped=true, element.ripup_cost=ripup_costs   # delayed ripup
                return something_expanded            # section NOT occupied yet
        if !room_rippable: return true               # hard wall: occupy and stop

for to_door in snapshot(next_room.doors):            # snapshot: completion may mutate list
    if to_door == e.door: continue
    something_expanded |= expand_to_door(to_door, e, ripup_costs, thick, Adjustment.NONE)

# via expansion (§7)
if ctrl.vias_allowed and e.door is not ExpansionDrill:
    if (something_expanded or thick) and next_room is CompleteFreeSpaceExpansionRoom:
        for page in drill_page_array.overlapping_pages(next_room.shape):
            expand_to_drill_page(page, e); something_expanded = true
    elif next_room is ObstacleExpansionRoom whose item is a Via:
        expand_to_drill(via.get_autoroute_drill_info(tree), e, ripup_costs)  # rip-and-reuse an existing via site

return something_expanded
```

### 4.4 expand_to_door / expand_to_door_section

`expand_to_door` (`:592-647`): compute `line_sections = to_door.get_section_segments(half_width)`,
then for each unoccupied section:

* room **thick**: `new_shape_entry = section segment` as-is; plus a guard for a single-section dim-1
  door between two complete free rooms — reject the whole door if the room is too narrow at the door
  midpoint (acute-corner squeeze check, `:609-626`).
* room **thin**: the trace can only pass straight through — `new_shape_entry =
  segment_projection(e.shape_entry, section)` (perpendicular projection of the entry segment onto
  the exit segment, `:158-189`); null projection ⇒ this section unreachable. Point-like doors
  (section length < 1) are skipped.

`expand_to_door_section(door, i, shape_entry, from, add_costs, adjustment)` (`:682-834`):

```
if door.section[i].is_occupied or shape_entry == null: return false
layer = from.next_room.layer
g' = from.expansion_value + add_costs
     + weighted_distance(mid(shape_entry), mid(from.shape_entry),
                         ctrl.trace_costs[layer].horizontal, ctrl.trace_costs[layer].vertical)
f' = g' + destination_distance.calculate(mid(shape_entry), layer)
room_ripped = (add_costs>0 && adjustment==NONE) || (from.already_checked && from.room_ripped)
push MazeListElement(door, i, from.door, from.section_no, g', f',
                     door.other_room(from.next_room), shape_entry, room_ripped, adjustment, false)
     with element.ripup_cost = add_costs when (add_costs>0 && adjustment==NONE)
return true
```

**The edge cost function** is the octilinear weighted distance between door-section midpoints:
`FloatPoint.weighted_distance(p, hcost, vcost)` ≈ `hcost*|dx| + vcost*|dy|` (with diagonal handling
inside the geometry lib) — this is where **preferred-direction weighting** enters: per-layer
`trace_costs[layer] = (horizontal, vertical)` from the router settings (typically 1.0 in the
preferred direction and >1 against it). v2 adds an optional bend penalty here (§12).

---

## 5. Initialization (`init`, v19 `MazeSearchAlgo.java:1256-1363`)

1. `reduce_trace_shapes_at_tie_pins(start_items)` and `(destination_items)`: for multi-net pins,
   shrink foreign-net trace shapes in the compensated tree so the pin centre stays reachable
   (`:95-111`).
2. **Destinations**: for each destination item, `item.autoroute_info.set_start_info(false)`; for
   each of its tree shapes, `destination_distance.join(shape.bounding_box(), layer)`. Fanout mode
   with no destinations: join the whole board bbox on layer 0 and layer `n-1`.
3. **Starts**: for each start item, `set_start_info(true)`; for each tree shape of a `Connectable`,
   create `IncompleteFreeSpaceExpansionRoom(shape=null /*whole plane*/, layer,
   contained=item.get_trace_connection_shape(tree,i))` and `complete_expansion_room` it. If
   `maintain_database`, also reuse `engine.get_rooms_with_target_items(start_items)` (previously
   completed rooms that already have target doors to a start item, `AutorouteEngine.java:599-613`).
4. **Seed queue**: for every `TargetItemExpansionDoor` of the completed start rooms that is *not* a
   destination door: `center = centre_of_gravity(connection_shape ∩ room_shape)`, push
   `MazeListElement(door, 0, null, 0, g=0, f=destination_distance.calculate(center, layer), room,
   FloatLine(center,center), false, NONE, false)`.
5. Fail (`get_instance` returns null → connection NOT_ROUTED) if no destination or no start door.

Start/destination assignment upstream: `AutorouteConnectionRouter` picks the *unconnected* set as
start and the connected set as destination (or vice versa when routing from a plane), so target
doors on items with `start_info==false` are capture points.

---

## 6. Target capture

Two ways a search reaches a destination:

1. **Via room target doors** (`expand_to_target_doors`, `:527-589`): when expanding a room, for each
   `TargetItemExpansionDoor` of the room (skipping the door we came through): compute
   `target_shape = item.get_trace_connection_shape(tree, entry_no)`; `connection_point =
   target_shape.nearest_point_approx(entry_mid)`; if the room is **thin**, additionally verify a
   real trace can be forced from `entry_mid` to `connection_point`
   (`board.check_forced_trace_polyline` with the shove/spring-over recursion depths); then
   `expand_to_door_section(target_door, 0, FloatLine(pt,pt), e, 0, NONE)`. Entering through a
   *small* door is allowed only when coming out of an ObstacleExpansionRoom (post-ripup exit).
   When that element is later **popped** and `is_destination_door()` is true, the search stops with
   `Result(door, section)` (§4.2). The `get_trace_connection_shape` is the item's tree shape
   possibly shrunk (e.g. pin exit restrictions).
2. **Fanout termination**: in fanout mode the pop of an `ExpansionDrill` whose backtrack door is
   also an `ExpansionDrill` ends the search — the "connection" is pin → escape trace → via.

Backtracking (consumer contract): starting at `Result.destination_door/section`, follow
`door.get_maze_search_element(section).backtrack_door/section_no_of_backtrack_door` chain to the
start door (whose backtrack_door is null); each hop also carries `room_ripped/ripup_cost/adjustment`
for the inserter to know which items to rip and how the shove adjusted the geometry.

---

## 7. Layer changes: drills and drill pages

### 7.1 ExpansionDrill

A candidate via site: `shape` (convex free area for the via), `location` (drill point), layer range
`[first_layer, last_layer]`, `room_arr[layer_count]` = the CompleteExpansionRoom containing
`location` on each layer, and one `MazeSearchElement` per layer (a drill is an ExpandableObject
whose "sections" are layers). `calculate_expansion_rooms(engine)` (v19 `ExpansionDrill.java:47-85`):
point-query the search tree at `location` over all layers; for each layer pick the overlapping
`CompleteExpansionRoom`, or complete a fresh incomplete room
(`IncompleteFreeSpaceExpansionRoom(null, layer, point_shape)`); if completion doesn't yield exactly
1 room (obstacle at the point) the drill is discarded.

### 7.2 DrillPage / DrillPageArray — the drill cache

`DrillPageArray` (v19 file): the board bbox is split into a grid of pages of roughly
`max(5 * default_via_diameter, 10000)` internal units (`AutorouteEngine.java:71-73`).
`overlapping_pages(shape)`: bbox → page index range → filter by 2-dim intersection.
`invalidate(shape)` marks intersecting pages dirty. `reset()` resets all pages' maze elements + all
cached drills' maze elements.

`DrillPage.get_drills(engine, attach_smd)` (v19 `DrillPage.java:62-135`) — computed lazily,
**cached per (page, net)**; recomputed when `net_no` changes or after `invalidate()`:

1. Collect all items overlapping the page whose shapes are *not drillable* for this net
   (`item.is_drillable(net)`); pins with `drill_allowed()` are skipped when `attach_smd`.
2. Cut those obstacle∩page shapes out of the page box (`PolylineArea(page, holes)`), split the rest
   into convex pieces (`split_to_convex`).
3. Each convex piece becomes one `ExpansionDrill` spanning **all layers** (0..n-1); location = a
   contained drillable pin centre if `attach_smd` (component-side first, then solder-side), else the
   piece's centre of gravity rounded; keep only drills whose `calculate_expansion_rooms` succeeds.

Note the deliberate coarseness: drills always span all layers here; per-layer feasibility is decided
later in `expand_to_other_layers`.

### 7.3 Via expansion chain (three queue hops)

1. **room → DrillPage** (`expand_to_drill_page`, `:1007-1035`): pushed for every page overlapping a
   thick complete free room. `g' = g + ctrl.min_normal_via_cost`;
   `f' = g' + weighted_distance(nearest page point, entry_mid) + h(nearest_point, layer)`.
   `section_no` = the layer; door = the page; `next_room` kept = the room. Note h is *not* stored in
   g — the travel-to-page distance counts only for ordering.
2. **DrillPage → ExpansionDrill** (`expand_to_drills_of_page`, `:1037-1080`): on popping the page
   element, `get_drills(...)`; for each drill whose `room_arr[layer]` **is exactly the room the page
   was entered from** and whose layer element is unoccupied: `expand_to_drill(drill, e, 0)`.
3. **`expand_to_drill`** (`:930-1001`): for a thin from-room, only drills intersecting the backtrack
   door are allowed (straight-through constraint). `shrinked = drill.shape.shrink(via_radius[layer])`;
   the entry point is `shrinked.nearest_point_approx(compare_corner)` where `compare_corner` is the
   entry midpoint or, when coming from a pin's target door via a page, the pin's nearest trace exit
   corner. `g' = g + add_costs + weighted_distance(nearest, compare_corner)`. If the hop came via a
   DrillPage, the backtrack door *skips the page* (points at the pre-page door) and the via cost was
   already paid in hop 1; if it came directly from an existing Via's ObstacleExpansionRoom, backtrack
   = the from door and `g' += ctrl.min_normal_via_cost`. `section_no = layer - first_layer`,
   `next_room = null` (drills expand specially).
4. **ExpansionDrill pop → `expand_to_other_layers`** (`:1083-1253`): only when the previous hop was
   not itself a drill. Determine the reachable contiguous layer interval:
   * If `room_arr[from_section]` is an ObstacleExpansionRoom: this is a **via ripup** — only allowed
     if `ctrl.ripup_allowed`, the item is a Via, its padstack is in `ctrl.via_rule`, and its
     clearance class matches; interval = padstack layer range; `room_ripped = true`.
   * Else scan down then up from `from_layer`, on each layer calling
     `ForcedViaAlgo.check_layer(via_radius[layer], via_clearance_class, attach_smd, room_shape,
     location, layer, net, max_shove_trace_recursion_depth, 0, board)`; stop at `NOT_DRILLABLE`.
     `DRILLABLE_WITH_ATTACH_SMD` on outer layers sets smd-attached flags. The scan is bounded by
     `[max(first_layer, ctrl.via_lower_bound), min(last_layer, ctrl.via_upper_bound)]`; if the
     resulting interval doesn't cover the drill's full `[first_layer, last_layer]` range, **abort
     entirely** (`:1152-1154,1186-1188` — the drill only fires where the whole free-space column is
     drillable; partial vias are handled by the via masks below).
   * For each `to_layer` in the interval (≠ from_layer): require an entry in `ctrl.via_info_arr`
     (via masks = padstack layer ranges of the net's via rule) with `mask.from<=first`,
     `mask.to>=last`, mask inside the drillable interval, and `attach_smd_allowed` if an outer layer
     is smd-attached. Then push
     `MazeListElement(drill, to_layer-first_layer, backtrack=drill, from_section,
     g + ctrl.add_via_costs[from_layer].to_layer[to_layer], g' + h(entry_mid, to_layer),
     room_arr[to_index], same shape_entry, room_ripped, NONE, false)`.
     NB: `add_via_costs[i].to_layer[j]` is initialized to **0** for all pairs
     (`AutorouteControl.java:115-119`) and never reassigned in either tree; the effective per-via
     cost is thus the `min_normal_via_cost` paid at hop 1/3. (Keep the matrix — it's the intended
     hook for buried/blind via costing.)

The drill's per-layer maze elements later get occupied like door sections, and the popped drill
element with `next_room = room_arr[layer]` proceeds through normal `expand_to_room_doors` on the new
layer.

---

## 8. Ripup and shove during search

### 8.1 check_ripup (v19 `MazeSearchAlgo.java:1392-1516`; v2 `MazeRipupResolver.checkRipup`)

Returns −1 (unrippable), `ALREADY_RIPPED_COSTS = 1` (continuing through the same obstacle item as
the previous room), or a positive int cost added to g at each door of the obstacle room.

```
if !item.is_routable(): -1                       # fixed/user-fixed items are walls
if door_is_small and !enter_through_small_door(e, item): -1
prev_item = item of previous room if it was an ObstacleExpansionRoom
if adjustment != NONE:                           # room was shoved
    if prev_item != null && prev_item != item && prev_item.shares_net(item): -1   # fork
elif prev_item == item: return 1                 # ALREADY_RIPPED_COSTS

cost_factor = 1; fanout_factor = 1
if item is Trace:
    cost_factor = trace.half_width
    if !ctrl.remove_unconnected_vias: fanout_factor = calc_fanout_via_ripup_cost_factor(trace)
elif item is Via:
    every contact must be a non-user-fixed Trace else -1
    cost_factor = max(contact.half_width); fanout_factor from contacts (unless is_fanout)
    if fanout_factor <= 1: cost_factor *= 0.5 * max(contact_count-1, 0)

ripup_cost = ctrl.ripup_costs * cost_factor
detour = Connection.get(item).get_detour()  (>=1; how much the existing connection detours)
if ctrl.ripup_pass_no >= 4 and pass_no % 3 != 0:
    detour *= 0.5 + rand()^2                      # deterministic RNG seeded with ripup_costs
ripup_cost = ripup_cost / detour * fanout_factor
return clamp(int(ripup_cost), 1, Integer.MAX_VALUE/100)
```

Constants: `FANOUT_COST_CONST = 20000` in `calc_fanout_via_ripup_cost_factor` (`:117-152`):
a trace whose single end contact is a through-pin, or a 2-corner SHOVE_FIXED exit trace, is a
protected fanout stub with factor `max(20000 * (half_width/length)^2, 1)` — short stubs become
practically unrippable. `ctrl.ripup_costs` default 1000, escalated per pass by the outer loop.
The RNG seed = `ctrl.ripup_costs` (`MazeSearchAlgo.java:63-64`) for reproducibility.
v2 gate difference: fanout protection only while
`ripupCosts <= 2 * settings.getStartRipupCosts()` (`MazeRipupResolver.java:98-99`), so late passes
drop the protection.

The cost is *not* applied when the element enters the destination-check or target expansion; it is
passed as `p_add_costs` to `expand_to_door(...)` for every exit door of the obstacle room, so a
ripped room behaves as a normal room with a toll at the entrance.

`enter_through_small_door(e, ignore_item)` (`:1619-1674`): build a probe shape around the door
(door line translated ±`check_radius = compensated_half_width + 2`, capped, then
`offset_shape(check_radius)`), query overlapping objects excluding this net; entry OK only if every
foreign object is the ignored item or one of its direct same-net contacts.
`check_leaving_ripped_item` (`:1677-1691`): same probe used when exiting a routable
ObstacleExpansionRoom into a thick free room through a small door.

### 8.2 What "ripped" means downstream

`room_ripped`/`ripup_cost` propagate through elements into the settled `MazeSearchElement`s. The
backtracker (`LocateFoundConnectionAlgo`) collects the ripped items into `p_ripped_item_list` (+
`p_ripup_costs` map); `AutorouteEngine.autoroute_connection` then deletes those items' whole
connections (respecting `StopConnectionOption.FANOUT_VIA` when `!remove_unconnected_vias`) and trims
trace tails before inserting the new connection (`AutorouteEngine.java:177-206`). Ripped nets get
re-queued by the batch loop.

### 8.3 Trace shoving (delayed occupation protocol)

For a rippable-or-not `PolylineTrace` obstacle room that is thick, entered through a non-small
**edge section** (first or last section only, `:1525-1531`), `shove_trace_room` (`:1523-1597`,
v2 `MazeTraceShover`) tries `MazeShoveTraceAlgo.check_shove_trace_line` in both directions
(left/right, skipped if `adjustment` already constrains the side). Success yields `DoorSection`s to
expand with adjustment LEFT/RIGHT (for dim-2 link doors into the next trace segment) or NONE; each
goes through `expand_to_door_section` with `add_costs = 0` (shoving is free at search time; the real
shove happens on insert). If shoving found nothing but ripup is possible (`ripup_costs > 0`), the
element is **re-queued** at `f + ripup_costs` with `already_checked=true, room_ripped=true` and the
section left unoccupied — giving other door sections a chance to shove the room before the ripup
plan wins ("delay the occupation by ripup", `:424-441`). When the re-queued element pops,
`already_checked` short-circuits all the checks and it goes straight to door enumeration.
`MazeShoveTraceAlgo`/`MazeTraceShover` internals are out of this spec's assigned scope.

---

## 9. DestinationDistance — the admissible A* heuristic

(v19 `DestinationDistance.java`, v2 identical modulo naming.)

Maintains **three aggregate bounding boxes** of all destination shapes: `component_side_box`
(layer 0), `solder_side_box` (last layer), `inner_side_box` (everything between), populated by
`join(bbox, layer)`. Precomputes from `trace_costs[]` (active layers only):

* `min/max_component_side_trace_cost` = min/max of (h,v) on layer 0; same for solder side.
* `max_inner_side_trace_cost` = min over inner layers of max(h,v), also min'd with the outer maxima
  ("for inner layers we assume cost in preferred direction is 1").
* `min_component_inner_trace_cost = min(min_component_side, max_inner_side)`, same for solder, and
  `min_component_solder_inner_trace_cost` = min of those two.

`calculate(point_or_box, layer)` returns a **lower bound** on remaining cost = min over route
sketches with 0..3 vias:

* Axis distances `dx, dy` from the query box to each destination box (0 if overlapping), split into
  `max_delta = max(dx,dy)`, `min_delta = min(dx,dy)`.
* Same layer class: exact weighted box distance `p_box.weighted_distance(dest_box, h, v)`.
* One via to the "other" side: `min1*max_delta + min2*min_delta + min_normal_via_cost` with
  min1/min2 the two smallest per-side minimum costs, ordered so the smaller multiplies the larger
  delta.
* Via inner layer(s): `max_delta * 1 + min_delta * min_*_inner_trace_cost + k*min_normal_via_cost`
  (k = 1 for one hop to an inner layer, 2 via inner to the opposite side, 3 for
  outer→inner→inner→outer). Inner-layer distance in preferred direction is priced at 1.
* Early-outs based on `active_layer_count` (≤1, ==2, ==3).
* Result `Integer.MAX_VALUE` if no destination joined.

`calculate_cheap_distance` swaps `min_normal_via_cost` for `min_cheap_via_cost`
(= 0.8 × normal, `AutorouteControl.java:190`) by *temporarily mutating the field* — not used by the
maze loop itself (used by the batch ordering); a Rust port should just parameterize the via cost.

Admissibility: every term underestimates (straight-line octilinear distances at the minimum possible
per-unit cost, the cheapest possible via count), so the search is A*-optimal w.r.t. its own cost
model. The heuristic is **coarse for multi-destination inner boxes** (a single union box), which is
fine because it only under-estimates.

`min_normal_via_cost = user_via_costs(default 50) * max(max_via_radius, 1)` where `max_via_radius` =
max over layers of max(via padstack radius, trace half width) (`AutorouteControl.init_net:183-190`).
So via cost auto-scales with geometry size.

---

## 10. State reuse vs rebuild between connections

`AutorouteEngine` per-instance state: `autoroute_search_tree` (clearance-compensated tree shared
with the board), `drill_page_array`, `net_no`, `incomplete_expansion_rooms` list,
`complete_expansion_rooms` list, `expansion_room_instance_count` (id source), stop/time-limit refs.

* **maintain_database = false** (interactive route): after every connection `clear()`
  (`AutorouteEngine.java:236-246`) — remove every complete room from the tree, drop both lists,
  reset id counter, and `board.clear_all_item_temporary_autoroute_data()` (drops every item's
  `ItemAutorouteInfo`, hence all ObstacleExpansionRooms). Everything is rebuilt from scratch next
  connection.
* **maintain_database = true** (batch autorouter): after every connection `reset_all_doors()`
  (`:634-649`) — keep all rooms and doors, but reset every door's/target-door's `MazeSearchElement`s,
  every item's ObstacleExpansionRoom doors, every item's precalculated `Connection`, and
  `drill_page_array.reset()`. On `init_connection(new_net)` (`:77-104`), if the net changed:
  remove every **net-dependent** complete room (`is_net_dependent`) via
  `remove_complete_expansion_room` (planting repair frontiers, §3.4), and call
  `board.additional_update_after_change(item)` for all items of the new net (which removes complete
  rooms overlapping those items and invalidates their drill pages — v2
  `RoutingBoard.java:100-113`). Net-independent free-space tiling is thus **shared across nets**;
  that is the main amortization in batch mode.
  The engine itself is cached on the board and recreated only when the trace clearance class
  changes (v2 `RoutingBoard.initAutoroute:882-896`).
* **Per-connection always rebuilt**: the maze queue, `DestinationDistance` boxes, start rooms
  (start rooms are net-dependent → removed on net change anyway), `start_info` flags on items.
* **DrillPage.drills** cache: keyed by net (`net_no` field), so it survives *within* a net's
  connections in batch mode and is invalidated by geometry changes via `invalidate()`.

Timing/stop: `is_stop_requested()` checks a `TimeLimit` (`100000 * 2^(pass-1)` ms per connection,
capped, set in `AutorouteConnectionRouter:72-74`) and the thread's stop flag; checked every pop and
in every completion loop.

---

## 11. Constants and configuration summary

| constant | value | where |
|---|---|---|
| `TRACE_WIDTH_TOLERANCE` | 2 (internal units) | `AutorouteEngine.java:30`; added to half widths in door sectioning/small-door checks |
| `c_max_door_section_width` | `10 * (half_width + 2)` | `ExpansionDoor.get_section_segments:129` |
| `ALREADY_RIPPED_COSTS` | 1 | `MazeSearchAlgo.java:41` |
| `FANOUT_COST_CONST` | 20000 | `:118` |
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE / 100` | `:1493` |
| default `ripup_costs` | 1000 (escalates per pass) | `AutorouteControl.java:126` |
| ripup randomization | pass ≥ 4 && pass % 3 != 0; `detour *= 0.5 + U(0,1)^2`; RNG seed = ripup_costs | `:1482-1488` |
| `min_normal_via_cost` | `via_costs × max(max_via_radius,1)` | `AutorouteControl.java:187-189` |
| `min_cheap_via_cost` | `0.8 × min_normal_via_cost` | `:190` |
| `add_via_costs[i][j]` | all 0 (hook only) | `:115-119` |
| drill page size | `max(5 × default_via_diameter, 10000)` | `AutorouteEngine.java:71-72` |
| shove recursion depths | trace 20, via 5, spring-over 5 | `AutorouteControl.java:112-114` |
| pull tight accuracy | 500 | `:111` |
| thin-room test | `min_width < 2*half_width`, or 2nd nearest border point ≤ `half_width+1` | `MazeSearchAlgo.java:354-371` |
| small 2-dim door threshold | segment length < `2*(half_width+2)` | `ExpansionDoor.java:121-124` |
| per-connection time limit | `min(100000·2^(pass−1), MAX_INT)` ms | `AutorouteConnectionRouter.java:72-74` |
| v2 bend penalty trigger | sin²(angle) > 0.01 (≈5.7°) | `MazeSearchEngine.java:867-871` |
| v2 pure-SMD via factor | ×0.1 | `AutorouteControl.java:277-281` |
| v2 fanout escape limits | min 500 µm / max 3000 µm defaults | `MazeSearchEngine.java:96-118` |

`AutorouteControl` per-net derived data: `trace_half_width[layer]` (net class rule),
`compensated_trace_half_width[layer]` (+ clearance compensation for the trace clearance class),
`via_radius_arr[layer]` (max over via rule padstacks, floored by trace half width),
`via_info_arr` (ViaMask = from_layer/to_layer/attach_smd per padstack), `via_clearance_class`
(taken from the *first* via of the rule), `layer_active[]` (settings ∧ net-class active layers;
v2 additionally forces power planes inactive with a warning, `maze/AutorouteControl.java:152-161`).

---

## 12. v1.9 → v2 rename map and real behavioral deltas

Rename map (mechanical; camelCase + package split):

| v1.9 | v2 (2.3.0) |
|---|---|
| `autoroute.MazeSearchAlgo` | `autoroute.maze.MazeSearchEngine` (core loop, doors, targets, shove entry) split with: `maze.MazeExpansionEngine` (expand_to_drill/drill_page/drills_of_page/other_layers) and `maze.MazeRipupResolver` (check_ripup, enter_through_small_door, check_leaving_ripped_item) |
| `MazeShoveTraceAlgo` | `maze.MazeTraceShover` |
| `autoroute.AutorouteEngine/AutorouteControl/DestinationDistance/MazeListElement/MazeSearchElement` | same names under `autoroute.maze.*` |
| room/door classes | `autoroute.expansion.*` (identical semantics; `LinkedList`→`ArrayList` for doors) |
| `DrillPage/DrillPageArray/ExpansionDrill` | `autoroute.drill.*` |
| `LocateFoundConnectionAlgo*/InsertFoundConnectionAlgo/Connection` | `autoroute.path.FoundConnectionLocator*/FoundConnectionInserter/Connection` |
| `AutorouteEngine.AutorouteResult` enum | `AutorouteAttemptResult` + `AutorouteAttemptState` (with human-readable failure details) |
| `Item.get_autoroute_info()` etc. | `getAutorouteInfo()` etc. |

Real behavioral differences in v2 (each needs a keep/drop decision in Rust):

1. **Bend cost** (`MazeSearchEngine.expandToDoorSection:855-873`): optional per-layer
   `bendCosts[layer]` added to g when the segment backtrack-CoG→from-mid→new-mid turns by more than
   ≈5.7°. Off (0) by default per settings.
2. **Fanout escape-length gating** inside a subclassed `TreeSet.add`
   (`MazeSearchEngine.java:84-125`): in fanout mode, rejects queue insertions beyond
   `maxEscapeLengthMm` from the start pin on the pin's layer and drills closer than
   `minEscapeLengthMm`. (Filtering in `add` is a hack; in Rust gate at push sites.)
3. **Pure-SMD net handling** (`maze/AutorouteControl.java:189-284`): a net whose connectables are
   all single-layer pins forces `attachSmdAllowed=true` and multiplies via cost factor by 0.1.
4. **Fanout ripup protection gate**: only while `ripupCosts ≤ 2×startRipupCosts`
   (`MazeRipupResolver.java:98-99`); v19 protected whenever `!remove_unconnected_vias`.
5. **Per-via-info drill check**: `expandToOtherLayers` calls `checkLayerWithAnyMatchingVia`
   (`MazeExpansionEngine.java:377-414`) which tries **every via in the rule** (its own radius,
   clearance class, attach flag) instead of v19's single aggregate `via_radius_arr` +
   `via_clearance_class` check; DRILLABLE if any padstack works.
6. **Stale-target guard**: `expandToTargetDoors` validates `treeEntryNo` against the item's current
   tree shape count and null-checks the connection shape (`MazeSearchEngine.java:653-668`).
7. Power-plane layers forced inactive in the control (see §11).
8. `remove_unconnected_vias`-related result handling and failure diagnostics moved into
   `AutorouteAttemptResult` (semantics of ROUTED/NOT_ROUTED/INSERT_ERROR preserved).

Everything else (queue discipline, cost formulas, room completion, thin/thick/small logic, drills,
DestinationDistance) is line-for-line equivalent between the trees.

---

## 13. Rust design implications

### Keep semantically (correctness-load-bearing)

* The **lazy-deletion best-first queue over door sections** with the settle-then-maybe-occupy
  protocol, including the *delayed occupation* return path (§8.3) — several behaviors (shove vs
  ripup racing, thin-room multi-section occupation) depend on sections staying claimable after a
  failed expansion.
* The exact **cost model**: midpoint-to-midpoint `weighted_distance` with per-layer (h,v) factors;
  via cost = `via_costs × max_via_radius` paid once per drill hop; ripup toll formula with
  fanout-protection factor, detour division, pass-based randomization with the seeded RNG, and the
  clamp to `[1, i32::MAX/100]`. These constants embody years of tuning.
* **DestinationDistance** exactly as specified (it's admissible for this cost model; any
  "improvement" risks breaking optimality or, worse, slowing convergence).
* Room completion contract: contained_shape ⊆ completed shape; completed rooms are obstacles in the
  tree; `try_remove_edge` maximality loop with strict-area-increase termination;
  `complete_neighbour_rooms` before iterating a room's doors; incomplete-room repair frontier on
  room removal.
* Small/thin/thick door gating and the `enter_through_small_door` probe — these prevent DRC-invalid
  paths that the inserter cannot realize.
* The three-hop via chain including the backtrack-door skip of the DrillPage and the "drill must be
  drillable across its full layer span" abort — the locator depends on backtrack chains having this
  shape.
* Deterministic tie-breaking everywhere floats are compared (the local additions to
  `MazeListElement.compareTo` and the neighbour sorts): port them, don't regress to nondeterminism.
* v2 deltas 3–6 (§12) — they fix real routing failures (pure-SMD escape, stale tree entries).

### Restructure for performance

* **Node identity**: replace `ExpandableObject` + per-object `MazeSearchElement[]` with arena
  indices. Concretely: `RoomId(u32)`, `DoorId(u32)`, `DrillId(u32)`; door sections addressed as
  `(DoorId, u16)`; one contiguous `Vec<MazeNodeState>` (occupied/backtrack/ripup/adjustment packed
  into ~16 B) indexed by a flat section index allocated when a door is first sectioned. Reset
  between connections = bump a `generation: u32` stored per node instead of walking every door
  (v19 `reset_all_doors` walks *all* rooms and *all* board items every connection —
  `AutorouteEngine.java:634-649`).
* **Queue**: a `BinaryHeap<(OrderedF64 f, OrderedF64 g, u32 door_id, u16 section)>` (32 B POD)
  with lazy deletion beats `TreeSet<MazeListElement>` (Java pays a red-black node + a 100+ B object
  per push; `it.next(); it.remove()` per pop — `MazeSearchAlgo.java:221-225`). Elements' payload
  (backtrack, shape_entry, flags) goes in a parallel arena addressed by push index, or recompute
  shape_entry on settle.
* **Door shapes**: `ExpansionDoor.get_shape()` recomputes `first ∩ second` on **every call**
  (`ExpansionDoor.java:37-42`), and it's called from `get_section_segments`, `door_is_small`,
  drill-intersection tests, and the small-door probe — several times per expansion. Cache the door
  segment (two `f64` points + dimension) at door creation; invalidate with the room.
* **Room storage**: rooms/doors as SoA arenas with `SmallVec`-style inline door lists (rooms
  typically have 3–8 doors). Java uses `LinkedList<ExpansionDoor>` in v19 (pointer-chasing on the
  hottest iteration) and still allocates a defensive `new LinkedList<>(doors)` snapshot per room
  expansion (`MazeSearchAlgo.java:452`); in Rust, iterate by index with an epoch check instead of
  cloning.
* **Geometry**: `TileShape.intersection`, `shrink_segment`, `divide_segment_into_sections`,
  `nearest_border_points_approx` all allocate fresh objects per call in Java. The underlying exact
  predicates use i64/BigInt line arithmetic (`Line`, `Simplex`) — in the maze loop only the
  *approximate* float paths are hot; keep exact arithmetic for room completion (correctness of the
  tiling) and use pure f64 for costs/entries as Java already does.
* **Parallelism**: a single connection's search is inherently sequential (shared occupation state),
  but: (a) room completion per neighbour is parallelizable if the search tree supports concurrent
  reads + batched inserts; (b) across connections, the batch loop can route independent nets in
  parallel against **net-independent** room databases — the maintain_database design already splits
  net-dependent vs net-independent rooms (§10); make net-dependent state (target doors, start rooms,
  node states, drill caches keyed by net) thread-local and share the immutable net-independent
  tiling behind an epoch/COW scheme. (c) `DrillPage.get_drills` recomputation is embarrassingly
  parallel per page.
* **DrillPageArray**: keep, but store drills in a flat `Vec` per page and key the cache by
  `(net, geometry_epoch)` instead of the mutable `net_no` field + `invalidate()` flag.
* The `calculate_cheap_distance` temporary-field-mutation trick (`DestinationDistance.java:381-388`)
  → pass via cost as a parameter.

### Java-isms observed (evidence)

* Per-push allocation of `MazeListElement` (11-field object) + TreeSet node; millions per board
  (`MazeSearchAlgo.java:758-770,832`).
* `FloatLine`/`FloatPoint` churn: every expansion computes `middle_point` 2–4× and builds new
  `FloatLine`s (`:744-751,581,964`).
* String-building debug traces unconditionally constructing large concatenations before the logger
  filters them, in the hottest functions (`:315-332,453-481,691-831`) — in the studied trees these
  are guarded only inside `FRLogger.trace`; a Rust port should use lazy/log-level-gated tracing (or
  compile-time features).
* `TreeSet<SearchTreeObject>` for the small-door probe result (`:1656-1658`) where a Vec + dedup
  would do.
* `LinkedList` everywhere for build-and-iterate collections (v19; v2 partially fixed to
  `ArrayList` — `expansion/FreeSpaceExpansionRoom.java` comment "arraylist for better performance").
* Iterator restart after mutation in `complete_neighbour_rooms` (`AutorouteEngine.java:556-559`):
  O(doors²) worst case; in Rust use an index cursor over a stable arena list with tombstones.
* `instanceof` dispatch on every pop for door kind (`MazeSearchAlgo.java:240-267`) → enum-tagged
  node kinds.
* `ExpansionDoor.get_id_no()` computes a *hash* of the two room ids (`min*31+max`,
  `ExpansionDoor.java:186-191`) that can collide, weakening the deterministic tie-break — in Rust
  give doors real arena ids.

### Suggested Rust module cut

```
maze/
  queue.rs        // f64-ordered binary heap, lazy deletion, element arena
  nodes.rs        // MazeNodeState arena + generation reset
  search.rs       // occupy_next_element / expand_to_room_doors / targets  (§4-6)
  drills.rs       // DrillPageGrid, ExpansionDrill, via chain              (§7)
  ripup.rs        // check_ripup + small-door probes                       (§8)
  heuristic.rs    // DestinationDistance                                   (§9)
rooms/
  arena.rs        // rooms, doors, ids
  complete.rs     // complete_expansion_room + engine lifecycle            (§3, §10)
  neighbours_{any,45,90}.rs                                                (§3.3)
ctrl.rs           // AutorouteControl derived per-net data                 (§11)
```

Port the any-angle `SortedRoomNeighbours` first (it is the general case; 90°/45° are
specializations that can share the gap-room builder via an `AngleFamily` trait), and validate the
Rust engine against the Java one by diffing the settle order `(door_id, section, g, f)` on identical
boards with the deterministic tie-breaks enabled.

---

## Release-jar conformance (2.3.0 @2d4de019)

Audit of the body above against the **actual source of the released jar**,
`/tmp/fr-java-release/src/main/java/app/freerouting` (all paths in this section are relative to that
root unless stated). Constants were cross-checked against the shipped bytecode in
`/tmp/frbase/freerouting-2.3.0.jar` with `javap -p -c`.

Bytecode spot-checks that pin this source tree to the jar:

* `autoroute/MazeSearchAlgo.class` constant pool/code: `double 20000.0d` (×2), `double 0.01d`,
  `int 21474836` (= `Integer.MAX_VALUE / 100`, ×2), `double 0.5d`, `double 2.0d`.
* `autoroute/MazeSearchAlgo$1.class` (the anonymous `TreeSet` subclass): `1000.0d`, `3000.0d`, `500.0d`.
* `autoroute/AutorouteControl.class`: `0.1d`, `0.8d`, `sipush 500`, `bipush 20`, `sipush 1000`.
* `autoroute/AutorouteEngine.class`: `bipush 33 / 66 / 67` — the "net 33/66/67" debug trace is
  **shipped upstream**, not local instrumentation (see D1).
* `unzip -l … | grep 'autoroute/(maze|expansion|drill|pipeline|path)/'` → **0 entries**.

---

### 1. Which tree the release actually executes

**Verdict: the release runs the flat `app.freerouting.autoroute.*` package — v1.9 class names with
v2-era patches applied in place. The `autoroute.maze` / `autoroute.expansion` / `autoroute.drill` /
`autoroute.pipeline` / `autoroute.path` packages that §1 and §12 of this spec describe do not exist
in 2.3.0 (0 classes in the jar).** Every subsystem this spec covers is therefore ported from the
flat package.

#### 1.1 Headless / CLI / API (the porting-relevant path)

```
management/RoutingJobSchedulerActionThread.java:96-108
    String algorithm = job.routerSettings.algorithm;
    if (!RouterSettings.ALGORITHM_CURRENT.equals(algorithm))
        job.logInfo("Unknown router algorithm '"+algorithm+"', using default (freerouting-router)");
    // :106 comment: "Always use standard BatchAutorouter"
    router = new BatchAutorouter(job);              // :107  <-- unconditional
  → BatchAutorouter.runBatchLoop()                  autoroute/BatchAutorouter.java:948
  → BatchAutorouter.autoroute_pass(int)             :431
  → BatchAutorouter.autoroute_item(...)             :1257
  → board.init_autoroute(net, clearance, thread, time_limit, retain=false)
                                                    :1316-1317 → board/RoutingBoard.java:906-913
  → AutorouteEngine.autoroute_connection(...)       autoroute/AutorouteEngine.java:127
  → MazeSearchAlgo.get_instance(start,dest,engine,ctrl)   AutorouteEngine.java:140
  → maze_search_algo.find_connection()              AutorouteEngine.java:155
  → LocateFoundConnectionAlgo.get_instance(...)     AutorouteEngine.java:175
  → clear() / reset_all_doors()                     AutorouteEngine.java:184-188
  → InsertFoundConnectionAlgo.get_instance(...)     AutorouteEngine.java:241
```

**`BatchAutorouterV19` is unreachable headless.** The scheduler thread never instantiates it; the
`algorithm` setting is only logged.

#### 1.2 GUI (`BatchAutorouter` vs `BatchAutorouterV19` selection — both live)

```
interactive/AutorouterAndRouteOptimizerThread.java:206-219
    if (RouterSettings.ALGORITHM_V19.equals(routingJob.routerSettings.algorithm))
        this.batchAutorouter = new BatchAutorouterV19(routingJob);      // :209
    else {                                                             // unknown → coerced+warned
        routingJob.routerSettings.algorithm = ALGORITHM_CURRENT;        // :215
        this.batchAutorouter = new BatchAutorouter(routingJob);         // :217
    }
    thread_action()  :451  →  :513-517  instanceof dispatch → runBatchLoop() on either
```

**This selection does not change the maze engine.** `BatchAutorouterV19.autoroute_item` calls the
same `board.init_autoroute(...)` → `AutorouteEngine.autoroute_connection` →
`MazeSearchAlgo` (`autoroute/BatchAutorouterV19.java:438-446`). The two Batch classes differ only in
the *outer* pass loop / item ordering / ripup-cost escalation, not in the search. A Rust port needs
exactly one maze engine.

#### 1.3 Fanout pre-pass

```
BatchAutorouter.java:824  BatchFanout.fanout_board(board, settings, thread, …)
  → BatchFanout.java:217  routing_board.fanout(pin, settings, ripup_costs, thread, time_limit)
  → board/RoutingBoard.java:977-1071
        ctrl.is_fanout = true                            :1018
        ctrl.via_rule = via_rule ∪ board default vias    :1019-1036 (fallbackToBoardVias)
        ctrl.fanout_start_pin_name/center/layer          :1038-1043
        ctrl.remove_unconnected_vias = false             :1044
        init_autoroute(..., retain=false)                :1050-1051
  → same AutorouteEngine.autoroute_connection → same MazeSearchAlgo
```

#### 1.4 Optimizer re-route

`autoroute/BatchOptimizer.java:377` → `BatchAutorouter.autoroute_passes_for_optimizing_item`
(`BatchAutorouter.java:146-157`) → `autoroute_pass` → same chain.

#### 1.5 Dead in the release

* `BatchAutorouter.autoroute_pass_multi_thread` (`:315`) — **no callers**; therefore
  `autoroute/BatchAutorouterThread.java` (the per-thread board-clone router) never runs in 2.3.0.
  Multi-threading is applied in the *optimizer* (`BatchOptimizerMultiThreaded`), not the router.
* `AutorouteEngine.reset_all_doors()` / `init_connection`'s net-dependent invalidation — see D3.
* The ripup randomization branch — see D4.

---

### 2. Claims table (spec vs release source)

#### 2.1 Load-bearing DIFFERENCES

| # | Spec claim (§) | Release behaviour | Evidence (file:line) |
|---|---|---|---|
| D1 | Provenance caveat (spec ¶ before §1): the net-33/66/67 traces, `FANOUT_DIAG` blocks and deterministic tie-breakers are *local* instrumentation not in stock upstream | **All shipped in 2.3.0.** They are upstream code, not local patches. Keep them; they also mean the trace strings are in the jar | `AutorouteEngine.java:162-169`; `MazeSearchAlgo.java:866,1016`; `MazeListElement.java:81-113`; `SortedRoomNeighbours.java:141-147,663-667`; bytecode `AutorouteEngine.class` `bipush 33/66/67` |
| D2 | §1, §12 rename map: 2.3.0 runs `maze/* + expansion/* + drill/*`, `MazeSearchEngine`, `MazeRipupResolver`, `MazeTraceShover`, `path.FoundConnectionLocator`, `pipeline.AutorouteConnectionRouter` | **None of these classes exist in the release.** The whole §12 rename table is a map to *git master*, not to 2.3.0. Port targets are `MazeSearchAlgo`, `MazeShoveTraceAlgo`, `LocateFoundConnectionAlgo*`, `InsertFoundConnectionAlgo`, all in flat `autoroute/` | `ls autoroute/`; jar has 0 classes under those packages |
| D3 | §10: `maintain_database = true` for the batch autorouter; net-independent tiling shared across nets is "the main amortization in batch mode" | **`retain_autoroute_database` is hard-coded `false` at every construction site**, and both `RoutingBoard` call sites pass literal `false`. So `maintain_database` is *always* false in 2.3.0: `clear()` runs after every connection, and `reset_all_doors()`, `init_connection`'s net-dependent room removal, `get_rooms_with_target_items` reuse and `RoutingBoard.additional_update_after_change` are dead. `RoutingBoard.init_autoroute` also re-creates the engine every connection because `!p_retain_autoroute_database` is true | `BatchAutorouter.java:136`; `BatchAutorouterV19.java:104`; `BatchAutorouterThread.java:79`; `board/RoutingBoard.java:959-960, 1050-1051, 906-913`; dead paths at `AutorouteEngine.java:93-116, 186-188, 588, 624-639`, `MazeSearchAlgo.java:1267-1270`, `RoutingBoard.java:78-97` |
| D4 | §8.1 / §11: ripup randomization fires when `ripup_pass_no >= 4 && pass % 3 != 0` | **Dead code.** `ctrl.ripup_pass_no` is set to 1 in the `AutorouteControl` ctor and never reassigned anywhere except `interactive/ExpandTestState.java:157`, which also sets 1. The batch loop escalates `ripup_costs` but not `ripup_pass_no`. The seeded RNG is therefore never consumed | `AutorouteControl.java:198`; `MazeSearchAlgo.java:1443-1448`; `BatchAutorouter.java:1280`; `BatchAutorouterThread.java:470`; `interactive/ExpandTestState.java:157` |
| D5 | §11: default `ripup_costs` = 1000 | 1000 is only the ctor default and is overwritten on every real call by `start_ripup_costs × ripup_pass_no`, where `start_ripup_costs` default is **100** (`DEFAULT_START_RIPUP_COSTS`), falling back to **1** when `scoring == null`. With D4, pass-1 and every later pass see `ripup_costs = 100` unless the caller escalates it | `AutorouteControl.java:197`; `BatchAutorouter.java:1280, 1400`; `settings/sources/DefaultSettings.java:60,147`; `settings/RouterSettings.java:473-475` |
| D6 | §8.1: `detour = Connection.get(item).get_detour()` unconditionally | Release only consults the connection when `fanout_via_cost_factor <= 1 && !ctrl.is_fanout`; otherwise `detour` stays 1 (no division). Also collects `trace_length`, `item_count`, `min_trace_length` for the `CHECK_RIPUP` trace | `MazeSearchAlgo.java:1419-1442` |
| D7 | §12.4 "v2 gate difference": fanout protection only while `ripupCosts <= 2×startRipupCosts` — listed as a v2-only delta | **Present in the release**, and it gates *both* the trace branch and the via branch via one `preserveFanoutProtection` flag | `MazeSearchAlgo.java:1386-1396` |
| D8 | §11: per-connection time limit lives in `AutorouteConnectionRouter.java:72-74` | Same formula, different home: `min(100000·2^(pass−1), Integer.MAX_VALUE)` ms | `BatchAutorouter.java:1311-1313`; `BatchAutorouterThread.java:501-503` |
| D9 | §7.2: `DrillPage.get_drills` collects obstacles from the board search tree | Release queries the **net-clearance-compensated autoroute tree** (`p_autoroute_engine.autoroute_search_tree`), so page cutouts are compensated for the routed net's clearance class | `DrillPage.java:70` (v19 used `board.search_tree_manager.get_default_tree()`) |
| D10 | §3.2: `for (i, cand) in completed_shapes where dim==2: if i == 0 → add_complete_room else re-complete` | The "first" is the first **2-dimensional** candidate (dimension filter applied *before* the first/rest split via an `is_first_completed_room` flag), not raw index 0. `add_complete_room` returns null (room silently dropped) when `calculate_doors` returns null or the shape is <2-dim. Whole body is wrapped in a `try/catch(Exception)` that returns an empty collection | `AutorouteEngine.java:401-477, 484-500` |
| D11 | §2.1: `ItemAutorouteInfo.expansion_room_arr` gives ObstacleExpansionRoom identity "stable across the whole search" | Release re-allocates and `System.arraycopy`s the array whenever the item's `tree_shape_count` changes, so identity is stable only for the surviving prefix; higher indices are dropped | `ItemAutorouteInfo.java:70-79` |
| D12 | §6 / §12.6: stale-target guard is a v2 feature | **Present in the release**: `tree_entry_no` range check + null `target_shape` check, both `continue` | `MazeSearchAlgo.java:556-567` |
| D13 | §4.4: `g' = g + add_costs + weighted_distance(...)` | Release inserts a bend term: `g' = g + add_costs + bend_cost_penalty + weighted_distance(...)`; `f' = g' + h`. `room_ripped` and `ripup_cost` assignment unchanged | `MazeSearchAlgo.java:717-745` |
| D14 | §8.3: `MazeShoveTraceAlgo` bounds check | Release rejects silently on `trace_corner_no < 0 \|\| trace_corner_no >= arr.length - 2` (v19 warned at `>= arr.length - 1`) | `MazeShoveTraceAlgo.java` (`check_shove_trace_line` head) |
| D15 | §11: "drill page size = `max(5 × default_via_diameter, 10000)`" | That is the *maximum* page width fed to the grid; the actual page size is `PAGE_W = ceil(board_width / ceil(board_width / max_page_width))` (same for height), with the last column/row extended to the bbox edge | `AutorouteEngine.java:87-89`; `DrillPageArray.java:38-60` |
| D16 | §12.2: fanout escape defaults "min 500 µm / max 3000 µm" | 500/3000 µm are only the **hardcoded fallbacks** used when `settings.fanout == null`. Real defaults are `minEscapeLengthMm = 2.5`, `maxEscapeLengthMm = 4.5` (2500 µm / 4500 µm) | `MazeSearchAlgo.java:87-88, 98-99`; `settings/sources/DefaultSettings.java:117-118` |
| D17 | §12.5: v2's `checkLayerWithAnyMatchingVia` uses "its own radius, clearance class, attach flag" | Present, plus: `requiredRadius = max(0.5·viaShape.max_width(), ctrl.trace_half_width[layer])`, and `ForcedViaAlgo.check_layer` in the release takes **two extra args** (`trace_half_width[layer]`, `trace_clearance_class_no`) that v19 did not pass; spring-over depth is passed as literal `0` | `MazeSearchAlgo.java:1182-1204` |
| D18 | §12.8: result handling | Present, and the release adds a gate this spec does not mention: a located connection is rejected as `FAILED` when `!ctrl.layer_active[start_layer] \|\| !ctrl.layer_active[target_layer]` | `AutorouteEngine.java:202-205` |
| D19 | §11 table lacks the detour constants | `DETOUR_ADD = 100`, `DETOUR_ITEM_COST = 0.1`; `get_detour()` returns `Integer.MAX_VALUE` when `start_point`/`end_point` is null, which collapses ripup cost to the clamp floor 1 | `Connection.java:15-16, 149-156` |

#### 2.2 Explicitly CONFIRMED (spec matches the release line-for-line)

| Spec § | Confirmed in release at |
|---|---|
| §2.2 door sectioning: `offset = half_width + 2`; dim-1 → `diagonal_corner_segment`; free↔free dim-2 → restraint line, empty if one room inside the other; else degenerate CoG point; `shrink_segment(offset)`; `section_count = floor(len_unshrunk / (10·offset)) + 1`; `allocate_sections` reuses the array when the count matches | `ExpansionDoor.java:113-148, 199-207` |
| §2.2 `get_shape()` recomputes `first ∩ second` on every call | `ExpansionDoor.java:51-56` |
| §2.2 `ExpansionDoor.get_id_no() = min(id1,id2)·31 + max(id1,id2)` (collidable) | `ExpansionDoor.java:189-195` |
| §2.1 `ObstacleExpansionRoom` id `(item_id << 10) \| index_in_item` | `ObstacleExpansionRoom.java:55-57` |
| §2.2 `TargetItemExpansionDoor` single search element, `is_destination_door() = !start_info` | `TargetItemExpansionDoor.java` (id now `31·item_id + room_id`) |
| §3.1 `complete_neighbour_rooms`: iterator **restarted** after completing an incomplete neighbour; `calculate_doors` once per ObstacleExpansionRoom guarded by `all_doors_calculated()` | `AutorouteEngine.java:532-557` |
| §3.3 deterministic sort of the overlap list by `(object_id, shape_index)` and the `search_tree_object.get_id_no()` tie-break in `SortedRoomNeighbour.compareTo` | `SortedRoomNeighbours.java:141-147, 663-667` |
| §3.3 `calculate_target_doors` marks the room net-dependent | `CompleteFreeSpaceExpansionRoom.calculate_target_doors` |
| §3.4 `remove_complete_expansion_room`: detach door, plant half-plane incomplete room + dim-1 door on each 1-dim neighbour, remove from tree/list, `drill_page_array.invalidate(room_shape)` | `AutorouteEngine.java:359-394` |
| §4.1 `MazeListElement.compareTo` = `sorting_value`, `expansion_value`, `door.get_id_no()`, `section_no_of_door`, then 0 (dedup) | `MazeListElement.java:81-113` |
| §4.2 `occupy_next_element` order: stop check → pop+remove → skip occupied → settle backtrack fields → DrillPage → destination target door → fanout drill-after-drill → `expand_to_other_layers` → `expand_to_room_doors` → set `is_occupied`. DrillPage branch returns **without** occupying | `MazeSearchAlgo.java:249-316` |
| §4.3 dead-layer early-out only for **signal** layers; plane layers fall through to target-door expansion only | `MazeSearchAlgo.java:327-334, 412-415` |
| §4.3 split-plane CONDUCTION check for drills on inactive layers | `MazeSearchAlgo.java:398-409` |
| §4.3 thin/thick test: `min_width() < 2·half_width`, else (dim-1, not small, not already_checked) 2nd nearest border point `> half_width + 1` | `MazeSearchAlgo.java:377-397` |
| §4.3 `room_shape_is_thick` for obstacle rooms (trace half width + clearance compensation, or 0.5·via shape max width) `>= compensated_trace_half_width` | `MazeSearchAlgo.java:1331-1347` |
| §4.3 delayed-occupation requeue at `+ripup_costs` on both g and f with `already_checked=true, room_ripped=true`, section left unoccupied | `MazeSearchAlgo.java:446-470` |
| §4.3 door snapshot (`new LinkedList<>(next_room.get_doors())`) before iterating | `MazeSearchAlgo.java:476` |
| §4.4 thick path acute-corner guard (single-section dim-1 door between two complete free rooms) rejects the whole door | `MazeSearchAlgo.java:610-630` |
| §4.4 thin path: skip point-like first section (`dist² < 1`), else `segment_projection` | `MazeSearchAlgo.java:632-639, 195-215` |
| §4.4 `room_ripped = (add_costs>0 && adjustment==NONE) \|\| (from.already_checked && from.room_ripped)`; `element.ripup_cost = add_costs` under the same condition | `MazeSearchAlgo.java:741-…` |
| §5 init: tie-pin shape reduction, destination `set_start_info(false)` + bbox join, fanout board-bbox join on layer 0 and n−1, start rooms from `get_trace_connection_shape` with `shape=null`, seed elements `g=0, f=h(centre)`, fail when no destination or no start door | `MazeSearchAlgo.java:1213-1326` |
| §6 target capture incl. `check_forced_trace_polyline` for thin rooms and the "small door only when coming from an ObstacleExpansionRoom" rule | `MazeSearchAlgo.java:534-599` |
| §7.1 `ExpansionDrill.calculate_expansion_rooms`: point query per layer, complete a fresh room otherwise, discard unless exactly 1 room results | `ExpansionDrill.java:47-…` |
| §7.3 hop 1 `g' = g + min_normal_via_cost`, `f' = g' + weighted_distance(nearest page point, entry_mid) + h`; `section_no = layer`; `next_room` kept | `MazeSearchAlgo.java:955-980` |
| §7.3 hop 2 drill accepted only when `room_arr[layer] == from_room` and the layer element is free | `MazeSearchAlgo.java:982-1038` |
| §7.3 hop 3 thin-room backtrack-door intersection rule, `shrink(via_radius[layer])`, pin `nearest_trace_exit_corner` compare corner, DrillPage backtrack skip, `+min_normal_via_cost` only on the direct-from-via path, `next_room = null` | `MazeSearchAlgo.java:872-948` |
| §7.3 hop 4 via-ripup branch (padstack in `via_rule` + clearance class match), down-then-up scan, **abort unless the drillable interval covers the drill's full `[first_layer,last_layer]`**, via-mask matching incl. `attach_smd_allowed`, `g' += add_via_costs[from][to]` | `MazeSearchAlgo.java:1044-1180` |
| §8.1 check_ripup structure: `!is_routable → −1`; small door probe; shoved-fork rule; `prev_item == item → ALREADY_RIPPED_COSTS`; trace `cost_factor = half_width`; via contact rules and `cost_factor *= 0.5·max(contacts−1,0)`; `ripup_cost = ripup_costs·cost_factor / detour · fanout_factor`; clamp `min(max(int(x),1), Integer.MAX_VALUE/100)` | `MazeSearchAlgo.java:1356-1456` |
| §8.1 `FANOUT_COST_CONST = 20000`, `max(20000·(half_width/length)², 1)`, through-pin end contact or 2-corner SHOVE_FIXED exit trace | `MazeSearchAlgo.java:154-190` |
| §8.1 RNG seeded with `ctrl.ripup_costs` in the ctor | `MazeSearchAlgo.java:76-77` |
| §8.1 `enter_through_small_door` probe: `check_radius = compensated_half_width + 2`, ±translate, `offset_shape(check_radius, 0)`, allow only the ignored item and its same-net contacts; `check_leaving_ripped_item` reuses it | `MazeSearchAlgo.java:1558-1636` |
| §8.3 shove restricted to the **first or last** door section; left/right attempts skipped per existing `adjustment`; `LEFT/RIGHT` only for dim-2 link doors; `add_costs = 0` | `MazeSearchAlgo.java:1484-1540` |
| §8.2 downstream: `StopConnectionOption.FANOUT_VIA` when `!remove_unconnected_vias`, remove ripped connections, `remove_trace_tails` per changed net, then insert | `AutorouteEngine.java:213-244` |
| §9 DestinationDistance: three aggregate boxes, all cost precomputations, the 0..3-via sketches, `active_layer_count` early-outs, `Integer.MAX_VALUE` when nothing joined, and the `calculate_cheap_distance` temporary-field-mutation trick | `DestinationDistance.java` — **byte-for-byte equivalent to v19 modulo formatting**; cheap-distance swap at `:346-353` |
| §11 `TRACE_WIDTH_TOLERANCE = 2` | `AutorouteEngine.java:33` |
| §11 `ALREADY_RIPPED_COSTS = 1` | `MazeSearchAlgo.java:43` |
| §11 `MAX_RIPUP_COSTS = Integer.MAX_VALUE / 100` (= 21474836, verified in bytecode) | `MazeSearchAlgo.java:1454` |
| §11 `min_normal_via_cost = via_costs × max(max_via_radius, 1)`; `max_via_radius = max over layers of max(via padstack radius, trace_half_width)` | `AutorouteControl.java:265-276` |
| §11 `min_cheap_via_cost = 0.8 × min_normal_via_cost` | `AutorouteControl.java:277` |
| §11 `add_via_costs[i].to_layer[j] = 0` for all pairs, never reassigned | `AutorouteControl.java:186-190` (only reader: `MazeSearchAlgo.java:1159`) |
| §11 shove depths 20 / 5 / 5, pull-tight accuracy 500 | `AutorouteControl.java:182-185` |
| §11 small 2-dim door threshold: `dist² < 4·offset²` ⇔ length < 2·(half_width+2) | `ExpansionDoor.java:134-137` |
| §11 `via_clearance_class` from the **first** via of the rule; `via_info_arr` = one `ViaMask(from,to,attach_smd)` per padstack; `compensated_trace_half_width` = half width + clearance compensation; net-class inactive layers ANDed in | `AutorouteControl.java:214-254, 229-233` |
| §10 stop/time-limit checked on every pop and inside `init` loops | `MazeSearchAlgo.java:258-260, 1218, 1246, 1274, 1288`; `AutorouteEngine.java:266-278` |
| §10 `generate_room_id_no()` = monotonically increasing per-engine counter, reset by `clear()` | `AutorouteEngine.java:289, 641-643` |
| §13 Java-isms: per-push `MazeListElement` + red-black node, `it.next(); it.remove()` pop, `LinkedList` door lists (now `ArrayList` in the room classes), unconditional trace-string concatenation in the hot path, `instanceof` dispatch per pop | `MazeSearchAlgo.java:262-264, 476-495, 686-712`; `FreeSpaceExpansionRoom.java` ("ArrayList for better performance") |

---

### 3. Features this spec attributes to "v2" — PRESENT/ABSENT in the release (porting go/no-go)

| §12 item | Release status | Where in the release |
|---|---|---|
| 1. Per-layer **bend cost** on `expand_to_door_section`, sin²(angle) > 0.01 (~5.7°), off by default | **PRESENT** (identical algorithm) | `MazeSearchAlgo.java:717-733`; `ctrl.bendCosts[]` built at `AutorouteControl.java:159-162` from `RouterSettings.get_bend_cost(layer)` (`settings/RouterSettings.java:590-600`), clamped to `[MIN_BEND_COST=0.0, MAX_BEND_COST=9.9]` (`RouterSettings.java:16-17,587,596-598`), default `scoring.defaultBendCost = 0.0` (`settings/sources/DefaultSettings.java:151`). Only applied when `p_from_element.backtrack_door != null` |
| 2. **Fanout escape-length gating inside `TreeSet.add`** | **PRESENT**, as an anonymous `TreeSet` subclass in the `MazeSearchAlgo` ctor (jar class `MazeSearchAlgo$1`) | `MazeSearchAlgo.java:80-108`. Gate 1: reject any element whose `next_room.layer == ctrl.fanout_start_pin_layer` and whose entry midpoint is farther than `maxEscapeLengthMm·1000·resolution(UM)` from the pin centre. Gate 2: reject `ExpansionDrill` elements whose `drill.location` is closer than `minEscapeLengthMm·1000·resolution(UM)`. Fallbacks 3000.0 / 500.0 µm; real defaults 4.5 / 2.5 mm (see D16) |
| 3. **Pure-SMD net handling**: force `attachSmdAllowed`, via cost × 0.1 | **PRESENT** | `AutorouteControl.rebuild_via_info`: `isPureSmdNet` at `:280-293` (non-empty connectable set and every connectable is a `Pin` with `first_layer()==last_layer()`); attach relaxation at `:257-263` (only when `!attach_smd_allowed && layer_count > 1 && pure_smd_net`); `via_cost_factor *= 0.1` at `:271-275` (unconditional on the attach flag). Net effect: `min_normal_via_cost = via_costs × max(max_via_radius,1) × 0.1` |
| 4. **Fanout ripup-protection gate** `ripupCosts <= 2×startRipupCosts` | **PRESENT** (not a v2-only delta) | `MazeSearchAlgo.java:1386-1387` (`preserveFanoutProtection`), applied to both trace (`:1390`) and via (`:1395`) branches |
| 5. **Per-via-info drill check** (try every via in the rule) | **PRESENT**, as `check_layer_with_any_matching_via` | `MazeSearchAlgo.java:1182-1204`; called from both scan directions at `:1080` and `:1108`. Returns `DRILLABLE` on the first success, else `DRILLABLE_WITH_ATTACH_SMD` if any padstack got that, else `NOT_DRILLABLE` |
| 6. **Stale-target guard** in `expand_to_target_doors` | **PRESENT** | `MazeSearchAlgo.java:556-567` |
| 7. **Power-plane layers forced inactive** with a warning | **PRESENT** | `AutorouteControl.java:166-173` (`!layer_structure.arr[i].is_signal && activeSetting` → warn + `layer_active[i] = false`) |
| 8. `AutorouteAttemptResult` / `AutorouteAttemptState` with failure detail | **PRESENT** | `autoroute/AutorouteAttemptResult.java`, `autoroute/AutorouteAttemptState.java`; used throughout `AutorouteEngine.autoroute_connection` (`:142-244`) |
| — `autoroute.maze` / `expansion` / `drill` / `path` / `pipeline` packages, `MazeSearchEngine`, `MazeExpansionEngine`, `MazeRipupResolver`, `MazeTraceShover`, `MazeFanoutDiagnostics`, `AutorouteConnectionRouter`, `board.facade.RoutingBoard` | **ABSENT** | 0 classes in the jar; `ls autoroute/` shows the flat v1.9 layout |
| — `LinkedList → ArrayList` for door lists (listed as a v2 change) | **PARTIALLY PRESENT** | `FreeSpaceExpansionRoom.java`, `IncompleteFreeSpaceExpansionRoom.java`, `ObstacleExpansionRoom.java` use `ArrayList`; the per-expansion defensive snapshot in `MazeSearchAlgo.java:476` is still a `LinkedList` |

**Go/no-go summary for M2:** port items 1–8 — all eight are live in the shipped jar and must be
reproduced. Do *not* port the §12 package/class restructuring; it does not exist in 2.3.0 and
mapping to it will misalign line-level references.

---

### 4. Specifically resolved for the record

**(a) Do bend costs exist in the release maze? — YES.**
`MazeSearchAlgo.java:717-733`, inside `expand_to_door_section`, before the distance term:

```java
double bend_cost_penalty = 0.0;
if (ctrl.bendCosts[layer] > 0.0 && p_from_element.backtrack_door != null) {
  FloatPoint from_mid       = p_from_element.shape_entry.a.middle_point(p_from_element.shape_entry.b);
  FloatPoint backtrack_cog  = p_from_element.backtrack_door.get_shape().centre_of_gravity();
  double prev_dx = from_mid.x - backtrack_cog.x,  prev_dy = from_mid.y - backtrack_cog.y;
  double next_dx = shape_entry_middle.x - from_mid.x, next_dy = shape_entry_middle.y - from_mid.y;
  double cross_product = prev_dx * next_dy - prev_dy * next_dx;
  double sq_len_prev = prev_dx*prev_dx + prev_dy*prev_dy;
  double sq_len_next = next_dx*next_dx + next_dy*next_dy;
  if (sq_len_prev > 0.0 && sq_len_next > 0.0
      && (cross_product * cross_product) > 0.01 * sq_len_prev * sq_len_next) {
    bend_cost_penalty = ctrl.bendCosts[layer];
  }
}
double expansion_value = p_from_element.expansion_value + p_add_costs + bend_cost_penalty
    + shape_entry_middle.weighted_distance(from_mid, trace_costs[layer].h, trace_costs[layer].v);
```

Notes for the port: the penalty is a **flat per-bend adder** (not scaled by angle); the threshold is
`sin²θ > 0.01` on the *normalized* cross product, i.e. θ ≳ 5.74°; it costs nothing when the previous
hop is a start seed (`backtrack_door == null`) and nothing when the backtrack door is a drill/page
whose CoG is degenerate. `bendCosts[layer]` is a `double`, clamped to `[0.0, 9.9]`, default `0.0`,
per-layer overridable via `layers[i].bend_cost` in the settings JSON. It is a *cost-model* term, so
the `DestinationDistance` heuristic (which knows nothing about bends) stays admissible.

**(b) Pure-SMD-net via-cost ×0.1 / `attachSmdAllowed` relaxation — PRESENT.**
Both live in `AutorouteControl.rebuild_via_info` (`AutorouteControl.java:228-278`), which is also
re-invoked by the fanout path after the via-rule fallback merge (`RoutingBoard.java:1036`):

```java
boolean pure_smd_net = isPureSmdNet(p_board, p_net_no);                 // :257
if (!this.attach_smd_allowed && layer_count > 1 && pure_smd_net) {      // :258
  this.attach_smd_allowed = true;                                      // :262
}
for (int j = 0; j < layer_count; j++) {                                 // :265
  via_radius_arr[j] = max(via_radius_arr[j], trace_half_width[j]);
  max_via_radius    = max(max_via_radius, via_radius_arr[j]);
}
double via_cost_factor = max(this.max_via_radius, 1);                   // :269-270
if (pure_smd_net) { via_cost_factor *= 0.1; }                           // :271-275
min_normal_via_cost = p_via_costs * via_cost_factor;                    // :276
min_cheap_via_cost  = 0.8 * min_normal_via_cost;                        // :277
```

`isPureSmdNet` (`:280-293`): `board.get_connectable_items(net)` non-empty **and** every item is a
`Pin` with `first_layer() == last_layer()`. Two asymmetries worth porting verbatim: the attach
relaxation is guarded by `!attach_smd_allowed && layer_count > 1`, while the ×0.1 is not; and the
×0.1 is applied *after* the `max(...,1)` floor, so it can drive the effective via cost below 1
(e.g. `via_costs=50`, `max_via_radius=1` → 5.0 instead of 50.0). Because `min_normal_via_cost` is
also fed to `DestinationDistance` (`MazeSearchAlgo.java:109-110`), the heuristic scales with it
automatically.

**(c) §11 constants table verified against `autoroute/`**

| constant | spec value | release value | release location | verdict |
|---|---|---|---|---|
| `TRACE_WIDTH_TOLERANCE` | 2 | 2 | `AutorouteEngine.java:33` | ✅ |
| `c_max_door_section_width` | `10 × (half_width + 2)` | `10 * offset`, `offset = p_offset + 2` | `ExpansionDoor.java:114, 144` | ✅ |
| `ALREADY_RIPPED_COSTS` | 1 | 1 | `MazeSearchAlgo.java:43` | ✅ |
| `FANOUT_COST_CONST` | 20000 | 20000.0 (bytecode `20000.0d`) | `MazeSearchAlgo.java:155` | ✅ |
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE / 100` | 21474836 (bytecode `ldc_w int 21474836`) | `MazeSearchAlgo.java:1454-1455` | ✅ |
| default `ripup_costs` | 1000 | ctor 1000, but always overwritten by `start_ripup_costs × ripup_pass_no`, default start = **100** (fallback 1) | `AutorouteControl.java:197`; `BatchAutorouter.java:1280`; `DefaultSettings.java:60,147`; `RouterSettings.java:473-475` | ⚠️ **D5** |
| ripup randomization | pass ≥ 4 && pass % 3 ≠ 0; `detour *= 0.5 + U(0,1)²`; seed = `ripup_costs` | code identical, but **unreachable** (`ripup_pass_no` is always 1) | `MazeSearchAlgo.java:1443-1448`; `AutorouteControl.java:198` | ⚠️ **D4** |
| `min_normal_via_cost` | `via_costs × max(max_via_radius, 1)` | same, **× 0.1 extra for pure-SMD nets** | `AutorouteControl.java:269-276` | ⚠️ (add the SMD factor) |
| `min_cheap_via_cost` | `0.8 × min_normal_via_cost` | 0.8 (bytecode `0.8d`) | `AutorouteControl.java:277` | ✅ |
| `add_via_costs[i][j]` | all 0 (hook only) | all 0, never reassigned; sole reader `MazeSearchAlgo.java:1159` | `AutorouteControl.java:186-190` | ✅ |
| drill page size | `max(5 × default_via_diameter, 10000)` | that is the max *width* input; page W/H = `ceil(span / ceil(span / max_width))` | `AutorouteEngine.java:87-89`; `DrillPageArray.java:38-60` | ⚠️ **D15** |
| shove recursion depths | 20 / 5 / 5 | 20 / 5 / 5 (bytecode `bipush 20`, `iconst_5` ×2) | `AutorouteControl.java:183-185` | ✅ |
| pull tight accuracy | 500 | 500 (bytecode `sipush 500`) | `AutorouteControl.java:182` | ✅ |
| thin-room test | `min_width < 2·half_width`, or 2nd-nearest border point ≤ `half_width + 1` | identical (`next_room_is_thick = curr_dist > half_width + 1`) | `MazeSearchAlgo.java:381-396` | ✅ |
| small 2-dim door threshold | length < `2·(half_width + 2)` | `distance_square < 4 * offset * offset` | `ExpansionDoor.java:134-137` | ✅ |
| per-connection time limit | `min(100000·2^(pass−1), MAX_INT)` ms | identical formula, different file | `BatchAutorouter.java:1311-1313` | ⚠️ **D8** (location only) |
| bend penalty trigger | sin² > 0.01 (≈5.7°) | `cross² > 0.01 · |prev|² · |next|²` (bytecode `0.01d`) | `MazeSearchAlgo.java:730` | ✅ |
| bend cost value/range | (not stated) | `[0.0, 9.9]`, default 0.0, per-layer | `RouterSettings.java:16-17,587,596-598`; `DefaultSettings.java:151` | ➕ new |
| pure-SMD via factor | ×0.1 | ×0.1 (bytecode `0.1d`) | `AutorouteControl.java:274` | ✅ |
| fanout escape limits | min 500 µm / max 3000 µm | those are fallbacks; defaults are 2.5 mm / 4.5 mm | `MazeSearchAlgo.java:87-88,98-99`; `DefaultSettings.java:117-118` | ⚠️ **D16** |
| `DETOUR_ADD` / `DETOUR_ITEM_COST` | (missing) | 100 / 0.1; `get_detour() = Integer.MAX_VALUE` when an endpoint is null | `Connection.java:15-16, 149-156` | ➕ new |
| `DestinationDistance` result when nothing joined | `Integer.MAX_VALUE` | identical | `DestinationDistance.java` | ✅ |

**(d) Drill-page constants**

| item | release value | location |
|---|---|---|
| max page width fed to the grid | `max((int)(5 × rules.get_default_via_diameter()), 10000)` internal units | `AutorouteEngine.java:87-88` |
| grid dimensions | `COLUMN_COUNT = ceil(board_width / max_page_width)`, `ROW_COUNT = ceil(board_height / max_page_width)` | `DrillPageArray.java:41-43` |
| actual page size | `PAGE_WIDTH = ceil(board_width / COLUMN_COUNT)`, `PAGE_HEIGHT = ceil(board_height / ROW_COUNT)`; last column/row extended to `bounds.ur` | `DrillPageArray.java:44-60` |
| `overlapping_pages(shape)` | `shape.bounding_box() ∩ bounds` → `min_i/min_j = floor(delta / PAGE_*)`, `max_i/max_j` kept as `double` and compared with `<`, then filtered by `intersection.dimension() > 1` | `DrillPageArray.java:82-102` |
| drill cache key | a single mutable `net_no` field, initialised to `-1`; recomputed when `net_no != engine.get_net_no()` or after `invalidate()` (`drills = null`) | `DrillPage.java:32, 67-68, 167-169` |
| obstacle source tree | **the autoroute (net-compensated) tree**, not the default board tree | `DrillPage.java:70` |
| obstacle skip rules | skip `item.is_drillable(net_no)`; skip `Pin` with `drill_allowed()` when `attach_smd`; skip a cutout when `prev_obstacle_shape.contains(curr_obstacle_shape)` (dedups via shapes repeated per layer); keep only 2-dim cutouts | `DrillPage.java:80-98` |
| drill construction | `PolylineArea(page_box, holes)` → `split_to_convex(stoppable_thread)`; location = contained drillable pin centre (component side `layer 0` first, then solder side `layer n−1`) when `attach_smd`, else `centre_of_gravity().round()`; drill spans **all layers 0..n−1**; kept only if `calculate_expansion_rooms` succeeds | `DrillPage.java:99-126`; `calc_pin_center_in_drill` at `:46-61` |
| per-page maze elements | one `MazeSearchElement` per **layer** (`maze_search_info_arr[layer_count]`) — a page's "section" index is the layer | `DrillPage.java:23, 39-43, 142-149` |
| `DrillPage.reset()` | resets every cached drill's elements *and* the page's own per-layer elements (drills themselves are **not** discarded) | `DrillPage.java:154-164` |
| `DrillPage.invalidate()` | `drills = null` only | `DrillPage.java:167-169` |
| `DrillPageArray.reset()` / `invalidate(shape)` | reset walks the whole 2-D array; invalidate marks every 2-dim-overlapping page | `DrillPageArray.java:70-77, 106-115` |
| ids (queue tie-break) | `DrillPage.get_id_no() = 31·shape.get_id_no() + net_no`; `ExpansionDrill.get_id_no() = 31·(31·location.get_id_no() + first_layer) + last_layer` | `DrillPage.java:190-194`; `ExpansionDrill.java:128-131` |
| `attach_smd` argument to `get_drills` | `ctrl.attach_smd_allowed` (which the pure-SMD relaxation may have forced true — see (b)) | `MazeSearchAlgo.java:985` |

---

### 5. Port-from verdict — the authoritative release file per algorithm

| Algorithm / subsystem | Port from (in `/tmp/fr-java-release/src/main/java/app/freerouting/`) |
|---|---|
| Maze main loop, settle/occupy protocol, door & target expansion, thin/thick/small gating, shove entry, ripup decision, drill/via chain, `init` | `autoroute/MazeSearchAlgo.java` (1682 ln) — **single file**; §4, §5, §6, §7.3, §8.1, §8.3 all live here |
| Priority-queue element + ordering (incl. the 4-key deterministic `compareTo`) | `autoroute/MazeListElement.java:81-113` |
| Per-door-section persistent node state (`is_occupied`, backtrack, `room_ripped`, `ripup_cost`, `Adjustment`) | `autoroute/MazeSearchElement.java` |
| Fanout escape-length queue gate | `autoroute/MazeSearchAlgo.java:80-108` (anonymous `TreeSet`; in Rust gate at the push sites) |
| A* heuristic | `autoroute/DestinationDistance.java` (v19-verbatim; parameterize the via cost instead of the `calculate_cheap_distance` field swap at `:346-353`) |
| Per-net derived control data (half widths, via radii/masks, layer activity, costs, bend costs, pure-SMD rules) | `autoroute/AutorouteControl.java` |
| Engine lifecycle: room database, `complete_expansion_room`, `complete_neighbour_rooms`, room removal + repair frontier, `clear`/`reset_all_doors`, drill-page ownership, stop/time limit | `autoroute/AutorouteEngine.java` |
| Room/door model | `autoroute/ExpansionRoom.java`, `CompleteExpansionRoom.java`, `FreeSpaceExpansionRoom.java`, `IncompleteFreeSpaceExpansionRoom.java`, `CompleteFreeSpaceExpansionRoom.java`, `ObstacleExpansionRoom.java`, `ExpansionDoor.java`, `TargetItemExpansionDoor.java`, `ExpandableObject.java`, `ItemAutorouteInfo.java` |
| Room completion / neighbour tiling (port the any-angle one first) | `autoroute/SortedRoomNeighbours.java` (716 ln, any-angle reference), `autoroute/SortedOrthogonalRoomNeighbours.java` (551 ln, 90°), `autoroute/Sorted45DegreeRoomNeighbours.java` (855 ln, 45°) |
| Drills & drill cache | `autoroute/ExpansionDrill.java`, `autoroute/DrillPage.java`, `autoroute/DrillPageArray.java` |
| Trace shoving during search | `autoroute/MazeShoveTraceAlgo.java` (incl. `DoorSection`) |
| Ripup cost inputs (detour / trace length / item count) | `autoroute/Connection.java` |
| Backtracking → geometry → board mutation (out of this spec's scope, next spec) | `autoroute/LocateFoundConnectionAlgo.java` + `LocateFoundConnectionAlgoAnyAngle.java` + `LocateFoundConnectionAlgo45Degree.java`, `autoroute/InsertFoundConnectionAlgo.java` |
| Outer batch driver (pass loop, ripup escalation, time limits, airline) — **not** the maze | `autoroute/BatchAutorouter.java` (`BatchAutorouterV19.java` is the GUI-only alternate driver over the same engine; `BatchAutorouterThread.java` is dead) |
| Fanout pre-pass driver | `autoroute/BatchFanout.java` + `board/RoutingBoard.java:977-1071` |
| Settings that feed the cost model | `settings/RouterSettings.java` (bend cost, via costs, start ripup costs, layer activity), `settings/ScoringSettings.java`, `settings/FanoutSettings.java`, `settings/sources/DefaultSettings.java` (all defaults) |

**Net effect on §13's Rust plan:** the "restructure for performance" list stands unchanged, but two
of its premises must be corrected. (i) The `maintain_database` cross-net room sharing that §13's
parallelism bullet builds on is *never enabled* in 2.3.0 (D3) — treat cross-connection room reuse as
a new capability to design, not a behaviour to reproduce. (ii) `reset_all_doors`'s cost (the
generation-counter optimization) is likewise not on the shipped hot path; the shipped hot path pays
`clear()` + full rebuild per connection instead, which is a much larger prize.
