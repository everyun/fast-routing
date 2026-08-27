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
