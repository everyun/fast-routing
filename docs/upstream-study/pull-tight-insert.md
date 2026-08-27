# Freerouting upstream study: connection insertion & post-route trace optimization ("pull tight")

Area: turning the maze-search room path into a concrete polyline, inserting it into the board with
shove, and the local trace optimizer (pull-tight) + trace normalization.

Source trees studied:

| v1.9 (classic) | v2.x (2.3.0 release) |
|---|---|
| `autoroute/LocateFoundConnectionAlgo.java` (547 L) | `autoroute/path/FoundConnectionLocator.java` (570 L) |
| `autoroute/LocateFoundConnectionAlgo45Degree.java` (332 L) | `autoroute/path/FoundConnectionLocator45Degree.java` (357 L) |
| `autoroute/LocateFoundConnectionAlgoAnyAngle.java` (506 L) | `autoroute/path/FoundConnectionLocatorAnyAngle.java` (455 L) |
| `autoroute/InsertFoundConnectionAlgo.java` (524 L) | `autoroute/path/FoundConnectionInserter.java` (807 L) |
| `autoroute/Connection.java` (163 L) | `autoroute/path/Connection.java` (155 L) |
| `board/PullTightAlgo.java` (539 L) | `board/optimize/TraceTightener.java` (547 L) |
| `board/PullTightAlgo90.java` (163 L) | `board/optimize/TraceTightener90.java` (169 L) |
| `board/PullTightAlgo45.java` (674 L) | `board/optimize/TraceTightener45.java` (674 L) |
| `board/PullTightAlgoAnyAngle.java` (1015 L) | `board/optimize/TraceTightenerAnyAngle.java` (1004 L) |
| `board/PolylineTrace.java` (1300 L) | `board/trace/PolylineTrace.java` + `PolylineTraceNormalization.java` (133 L) |
| `board/RoutingBoard.java` (entry points) | `board/facade/RoutingBoard.java` + `RoutingBoardOperations.java` |

**Which one does the 2.3.0 release use?** The v2 classes. `autoroute/maze/AutorouteEngine.java:183,265`
calls `FoundConnectionLocator.getInstance` / `FoundConnectionInserter.getInstance`;
`board/facade/RoutingBoardOperations.java:65` builds `TraceTightener.getInstance`. The v2 files are a
mechanical rename (snake_case → camelCase, package moves) of the v19 algorithms — I diffed each pair;
algorithm bodies are line-for-line identical except for the behavioral deltas listed in §10. So one
spec covers both; v19 names are used below, v2 deltas called out explicitly.

> **Caveat on this checkout:** both trees in `/tmp/fr-java` carry *local debug instrumentation* that
> is not upstream: `FRLogger.trace("compare_trace_…")` blocks gated on hardcoded net numbers
> (33/49/66/67/98), e.g. `LocateFoundConnectionAlgo.java:70-84`, `PolylineTrace.java:213-229`,
> `RoutingBoard.java:713-717`. Ignore those when porting; they do not change control flow except one
> case noted in §4.4 (stub-cleanup gate rewritten from a test-level check to `if (true)`-style).

---

## 1. Pipeline overview & call graph

```
AutorouteEngine.autoroute_connection (autoroute/AutorouteEngine.java:~110-215)
 ├─ MazeSearchAlgo.find_connection()          → MazeSearchAlgo.Result (destination_door, section_no)
 ├─ LocateFoundConnectionAlgo.get_instance()  → backtrack + geometric corner placement
 │     (dispatch: 90°/45° → LocateFoundConnectionAlgo45Degree, else AnyAngle;
 │      LocateFoundConnectionAlgo.java:174-209)
 ├─ engine.clear() / reset_all_doors()        (expansion DB dropped BEFORE insertion)
 ├─ delete ripped items + their connections, remove_trace_tails per changed net
 ├─ InsertFoundConnectionAlgo.get_instance()  → vias + traces inserted with shove
 │     ├─ board.insert_forced_trace_polyline  (RoutingBoard.java:682)  [shove + spring-over]
 │     │     └─ new_trace.pull_tight(pull_tight_algo)   ← pull-tight #1 (local, keep_point)
 │     ├─ ForcedViaAlgo.check / .insert
 │     ├─ board.connect_to_trace              (RoutingBoard.java:1143)
 │     └─ board.normalize_traces(net)         (BasicBoard.java:888)
 └─ returns ROUTED | NOT_ROUTED | INSERT_ERROR | ALREADY_CONNECTED

BatchAutorouter.autoroute_item (BatchAutorouter.java:593-690)
 └─ on ROUTED: routing_board.opt_changed_area(all nets, clip=null, accuracy,
        trace_costs, thread, TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP=1000 ms)  ← pull-tight #2 (regional)

RoutingBoard.opt_changed_area (RoutingBoard.java:207-234)
 └─ PullTightAlgo.get_instance(...).opt_changed_area(trace_cost_arr)
       └─ per changed layer region: PolylineTrace.pull_tight(algo), OptViaAlgo.opt_via_location

PolylineTrace.pull_tight(algo) (PolylineTrace.java:791-845)
 └─ PullTightAlgo{90,45,AnyAngle}.pull_tight(Polyline) → fixed-point loop of local moves,
    every candidate move validated by BasicBoard.check_trace_shape (BasicBoard.java:1047)
```

Pull tight therefore runs **after every successfully routed connection** (twice: once inside
`insert_forced_trace_polyline` restricted to the new trace with a keep-point, once as
`opt_changed_area` over the accumulated changed region with a 1 s time cap), plus:

- after ripup/removal: `remove_items_and_pull_tight` (RoutingBoard.java:99-142, `PULL_TIGHT_TIME_LIMIT = 2000` ms);
- after `remove_tails` between batch passes (BatchAutorouter.java:580-590);
- after fanout routing (RoutingBoard.fanout, RoutingBoard.java:1126-1135, 1000 ms cap);
- after forced-via insertion and drill-item moves (RoutingBoard.java:458, 568);
- the batch *optimizer* (`BatchOptRoute.opt_route_item`, BatchOptRoute.java:310-400) does **not** call
  pull-tight directly — it rips a connection and re-runs the autorouter, so pull-tight happens via the
  normal per-connection path. Its accept/reject metric is `ItemRouteResult`
  (via count, weighted trace length, incomplete count), not covered here.

---

## 2. Locating the connection: backtrack list

### 2.1 Data structures (LocateFoundConnectionAlgo.java)

```
class LocateFoundConnectionAlgo (abstract):
  connection_items : List<ResultItem>       // OUTPUT: the traces to insert
  start_item       : Item                   // item at the START door (= end of backtrack)
  start_layer      : int
  target_item      : Item | null            // item at maze destination (null for fanout drill)
  target_layer     : int
  backtrack_array  : BacktrackElement[]     // destination → start order
  ctrl             : AutorouteControl
  angle_restriction: NINETY | FORTYFIVE | NONE
  start_door       : TargetItemExpansionDoor
  // cursor state used while walking:
  current_from_point, previous_from_point : FloatPoint
  current_trace_layer : int
  current_from_door_index, current_to_door_index, current_target_door_index : int
  current_target_shape : TileShape

class ResultItem { IntPoint[] corners; int layer; }        // one trace per layer segment
class BacktrackElement { ExpandableObject door; int section_no_of_door;
                         CompleteExpansionRoom next_room; }
  // next_room = the room common to this door and the NEXT door in the backtrack list
```

Invariants:
- `backtrack_array[0].door` is the maze destination door; the **last** element's door must be a
  `TargetItemExpansionDoor` (start of route). If not → warn, all fields null, connection empty
  (LocateFoundConnectionAlgo.java:87-95).
- `connection_items` is ordered target→start; each ResultItem's corner list runs in walking order;
  consecutive ResultItems are joined by a via at `corners[0]` of the later item (see §4.1).
- All output corners are rounded to `IntPoint` and deduplicated (LocateFoundConnectionAlgo.java:441-449).

### 2.2 backtrack() (LocateFoundConnectionAlgo.java:228-316)

Walk the per-door `MazeSearchElement` chain from `destination_door` back to null:

```
cur_door = result.destination_door
cur_elem = cur_door.get_maze_search_element(result.section_no_of_door)
next_room = destination-door-specific:
    TargetItemExpansionDoor → door.room
    ExpansionDrill d        → d.room_arr[d.first_layer + section_no]
                              and if elem.room_ripped: add every ObstacleExpansionRoom item of
                              d.room_arr to ripped_item_list (+ ripup_costs map)
loop:
    emit BacktrackElement(cur_door, section_no, next_room)
    cur_door = cur_elem.backtrack_door;  if null → done
    section_no = cur_elem.section_no_of_backtrack_door
        (clamped to door.maze_search_element_count()-1 with a warning if out of range)
    next_room  = drill ? drill.room_arr[section_no] : cur_door.other_room(next_room)
    cur_elem   = cur_door.get_maze_search_element(section_no)
    if cur_elem.room_ripped and next_room is ObstacleExpansionRoom:
        ripped_item_list.add(room.item);  ripup_costs[item] = elem.ripup_cost
```

Side effect: **ripped items are collected here** and deleted by AutorouteEngine before insertion.

### 2.3 Constructor main loop (LocateFoundConnectionAlgo.java:100-171)

```
current_from_point = destination-dependent:
    TargetItemExpansionDoor → calculate_starting_point =
        round(centre_of_gravity( item.get_trace_connection_shape(tree, tree_entry_no)
                                 ∩ door.room.shape ))                      (:215-222)
    ExpansionDrill (fanout destination) → drill.location; at_fanout_end = true
current_trace_layer = target_layer;  current_from_door_index = 0
while not connection_done:
    # find next layer change (via) or the final target
    scan backtrack_array from from_door_index+1 for the first ExpansionDrill door
    if found (layer_changed):
        current_target_door_index = that index
        current_target_shape = point shape at drill.location
    else:
        connection_done = true
        current_target_door_index = last index
        target_shape = start_item.get_trace_connection_shape(tree, start_door.tree_entry_no)
                       ∩ start_door.room.shape
        if target_shape.dimension() >= 2:        # conduction area target
            shrink by compensated_trace_half_width[start_layer]; keep if non-empty  (:154-164)
    current_to_door_index = current_from_door_index + 1
    connection_items.add( calculate_next_trace(layer_changed, at_fanout_end) )
    at_fanout_end = false
```

`calculate_next_trace` (:399-473):
1. corner_list = [current_from_point]
2. unless at_fanout_end: `adjust_start_corner()` (:501-516) — shrink the current room shape by the
   compensated trace half-width; if the from-point is outside the shrunken shape, move it to
   `nearest_point_approx` (rounded), inserting one `calculate_additional_corner(..., horizontal_first=true)`
   between old and new point. This keeps the trace inside its room.
3. repeatedly call subclass `calculate_next_trace_corners()` until it returns empty; append each
   returned corner (skipping no-ops), maintaining previous/current from-point.
4. if layer_changed: advance `current_from_door_index = current_target_door_index + 1` and set the
   next layer from that element's `next_room.get_layer()`.
5. round all corners to IntPoint, dedupe consecutive equal points, emit
   `ResultItem(corners, current_trace_layer)`; then `current_trace_layer = next_layer`.

### 2.4 Angle-restricted corner helper (shared, used by locator *and* neckdown)

`calculate_additional_corner(from, to, horizontal_first, restriction)` (:379-393):
- NINETY: L-corner at `(to.x, from.y)` if horizontal_first else `(from.x, to.y)` (:318-330).
- FORTYFIVE (:332-373): produce the knee of an axis+diagonal path. With `adx=|dx|, ady=|dy|`:
  - if `adx <= ady` (mostly vertical move): horizontal_first → corner at `(to.x, from.y ± adx)`
    (diagonal first, then vertical); else corner at `(from.x, to.y ∓ adx)` (vertical then diagonal).
  - else symmetric with roles of x/y swapped.
- NONE: returns `to` (no extra corner).

---

## 3. Corner placement inside rooms/doors

### 3.1 45°/90° locator (LocateFoundConnectionAlgo45Degree.java)

`calculate_next_trace_corners()` (:104-273), per call emits ≤2 corners and advances
`current_to_door_index` by 1:

1. If `to_door_index > target_door_index`: return empty (trace finished).
2. `room = backtrack[to_door_index-1].next_room` (null → warn, empty).
3. Shrink the room shape: `shrink_offset = trace_halfwidth` for ObstacleExpansionRoom, else
   `trace_halfwidth + AutorouteEngine.TRACE_WIDTH_TOLERANCE` (**TRACE_WIDTH_TOLERANCE = 2**,
   AutorouteEngine.java:30). `trace_halfwidth = ctrl.compensated_trace_half_width[layer]`.
4. If shrunken shape non-empty: move into it — `nearest_room_point = round(shrinked.nearest_point_approx(from))`,
   pick `horizontal_first` via `calc_horizontal_first_from_door` (below), emit
   `[additional_corner, nearest_room_point]`, update from-point. Else use unshrunken room shape.
5. If `to_door_index == target_door_index` (this trace's last step): `nearest = round(target_shape.nearest_point_approx(from))`;
   try `additional_corner` with horizontal_first=true; if it falls outside the (shrunken) room shape retry
   with horizontal_first=false; emit `[add_corner, nearest]`; `++to_door_index`; return.
6. Otherwise the next door must be an `ExpansionDoor`:
   - dimension 2 door (area door, 90° mode only): `nearest = round(shrink(door_shape, shrink_offset).nearest_point_approx(from))`.
   - dimension 1 door (line door): `line_sections = door.get_section_segments(trace_halfwidth)`;
     take the section for `section_no_of_door`, `nearest = section.nearest_segment_point(from)`.
     **Acute-corner guard** (:232-247): convert next room's shape to a Simplex (Box/Octagon border
     lines of length 0 break the query), get its two nearest border points to `nearest`; if the 2nd
     is closer than `trace_halfwidth + 2`, fall back to the section's midpoint.
   - pick `horizontal_first` via `calc_horizontal_first_to_door`; emit `[additional_corner, nearest]`;
     `++to_door_index`.

`calc_horizontal_first_from_door` (:48-101) / `_to_door` (:279-331): heuristics deciding whether the
45° knee goes horizontal-then-diagonal or vice versa, based on the door's dominant orientation:
- door bounding box vs. `door_half_max_width = 0.5 * max(door_dx, |door_dy|)` classifies the door as
  ~vertical / ~horizontal / right-diagonal / left-diagonal;
- vertical door ⇒ from-door: horizontal first (true); to-door: false (mirrored);
- diagonal doors compare `|dx|` vs `|dy|` of the move combined with `sign(dx)==sign(dy)` — see the
  exact truth table at :83-98 and :311-327 (the two functions are sign-mirrored).

### 3.2 Any-angle locator (LocateFoundConnectionAlgoAnyAngle.java)

This is a **funnel/visibility algorithm** over the door sequence. Constants:
`c_tolerance = 1.0` (:22), `trace_halfwidth_max = compensated_half_width + TRACE_WIDTH_TOLERANCE(2)`,
`trace_halfwidth_middle = compensated_half_width + 1.0` (:84-86).

Per call (`calculate_next_trace_corners`, :72-385), emits ≤1 corner and may advance several doors:

1. If `to_door_index >= target_door_index`: emit `target_shape.nearest_point(round(from))` once, done.
2. Door corners: `calc_door_left_corner/right` (:47-65) = extreme corners of the door shape seen from
   the centre of gravity of the *previous* room (`door.other_room(next_room)`).
3. Passed-door handling (:91-109): if `from` is not strictly right of (left_corner,right_corner)
   the door line is already crossed; null out each corner whose scalar product test
   `from.scalar_product(previous_from, corner) >= 0` says it is behind us; if both null → door fully
   passed: `++to_door_index`, re-emit `from` (forces re-entry with next door).
4. **Funnel loop** (:126-275): maintain `door_left_corner`/`door_right_corner` as the tightest
   left/right constraints (with indices `left_ind`, `right_ind`), advancing `curr_door_ind`:
   - `left_tangent_point = from.right_tangential_point(door_left_corner, halfwidth_max)`,
     `right_tangent_point = from.left_tangential_point(door_right_corner, halfwidth_max)` — tangents
     to circles of radius halfwidth_max around the constraint corners (null tangent → warn at debug
     level and fall back to the corner itself).
   - Gap too small (right tangent not right of the (from,left-tangent) line): the trace cannot pass —
     round the nearer corner: `result = left_turn_next_corner(...)` or `right_turn_next_corner(...)`
     (below); `new_door_ind = left_ind` resp. `right_ind`; break.
   - Reached `target_door_index` → `end_of_trace = true`; break.
   - Fetch next door's corners; same passed-door null-outs guarded by the current corner being null.
   - Bend detection (:208-236): if `next_left` is right of (from, door_right) ⇒ bend right:
     `result = right_turn_next_corner(from, halfwidth_max, door_right_corner, next_left)`,
     `new_door_ind = right_ind + 1`; symmetric for bend left.
   - Otherwise shrink the funnel: replace `door_right_corner` by `next_right` if the visibility range
     narrows on the right — narrowing test: `next_right` not right of (from, door_right) **and**
     the segment (from → left_tangential_point(next_right)) stays ≥ halfwidth_max away from the old
     right corner (:238-255); symmetric on the left (:256-273). `++curr_door_ind`.
5. `end_of_trace` (:277-318): candidate corner = `target_shape.nearest_point(round(from))`. If it is
   left of the left tangent, the target lies outside the funnel: aim instead at
   `right_left_tangential_point(from, target_right_most_corner, door_left_corner, halfwidth_max)`
   and continue (`new_door_ind = left_ind + 1`, `end_of_trace = false`). Symmetric on the right side.
   If still end_of_trace: `new_door_ind = target_door_index`.
6. **Rear-window clearance correction** (:326-366): re-check the chosen segment (from → result_corner)
   against the previous up-to-5 doors (`check_from_door_index = max(to_door_index-5, from_door_index+1)`):
   for each door corner within `trace_halfwidth_middle` of the segment, compute the tangential
   detour corner (`right_left_` / `left_right_tangential_point`) and keep the most conservative one;
   if any, override `result_corner` and clamp `new_door_ind = max(corrected_ind, to_door_index)`.
7. `to_door_index = new_door_ind`; emit `result_corner` (if distinct from `from`).

Tangential-corner constructions (:394-505):
- `right_turn_next_corner(from, d, to, next)`: line1 = left tangent from `from` to circle(to, d);
  line2 = right tangent from `to` to circle(next, `2d + 1.0`), then translated by `d` left;
  result = intersection. `left_turn_next_corner` is the mirror (translate right / signs swapped).
- `right_left_tangential_point(from, to, center, d)`: intersection of the right tangent from `from`
  and the left tangent from `to` to circle(center, d); `left_right_` mirrored. Null tangents → null.

---

## 4. Insertion with shove (InsertFoundConnectionAlgo)

### 4.1 Driver (`get_instance`, InsertFoundConnectionAlgo.java:44-106)

```
curr_layer = connection.target_layer
for item in connection.connection_items:            # target→start order
    insert_via(item.corners[0], curr_layer, item.layer)  or fail → return null
    curr_layer = item.layer
    insert_trace(item)                                   or fail → return null
insert_via(last_corner, curr_layer, connection.start_layer)  or fail → return null
if target_item is PolylineTrace:  board.connect_to_trace(first_corner, it,
        ctrl.trace_half_width[start_layer], ctrl.trace_clearance_class_no)
if start_item  is PolylineTrace:  board.connect_to_trace(last_corner, it,
        ctrl.trace_half_width[target_layer], ctrl.trace_clearance_class_no)
board.normalize_traces(ctrl.net_no)                 # v19 wraps in try/catch (throws at depth>16)
```

**Failure semantics:** returning null ⇒ AutorouteEngine returns `INSERT_ERROR`
(AutorouteEngine.java:212). There is **no rollback**: ripped items were already deleted and partial
traces/vias of this connection remain on the board; the batch driver simply counts the item as
failed (BatchAutorouter.java:469-471). (The interactive optimizer protects itself with
`generate_snapshot()` before ripping, BatchOptRoute.java:355.)

### 4.2 insert_trace (:113-295) — incremental shove insertion

- 1-corner trace: just record `last_corner`, done (v2 also sets `first_corner`, see §10).
- Save and disable `rules.pin_edge_to_turn_dist` (set to −1) for the duration — the pin-exit
  correction must not fire while the polyline is inserted line by line (:123-124, restored :289).
- If `ctrl.with_neckdown`: pick start/end `Pin` at the trace's end corners (same net, pin center ==
  corner) for the neckdown fallback (:130-147).
- Main loop over corners `i = 1..n-1` with a sliding window `from_corner_no`:
  ```
  curr = corners[from_corner_no ..= i];  polyline = Polyline(curr)
  ok_point = board.insert_forced_trace_polyline(polyline, trace_half_width[layer], layer,
      {net}, clearance_class, max_shove_trace_recursion_depth (=20),
      max_shove_via_recursion_depth (=5), max_spring_over_recursion_depth,
      tidy_width = MAX_VALUE, pull_tight_accuracy (=500), with_check=true, time_limit=null)
  if ok_point != last corner && with_neckdown && window is a single segment:
      neckdown_inserted = insert_neckdown(ok_point, curr[1], layer, start_pin, end_pin)
  if ok_point == last corner || neckdown_inserted:      from_corner_no = i     # ADVANCE
  elif ok_point == first corner && i != n-1:            # VIOLATION_CORRECTED
      # spring-over may have failed; retry with a wider window
      if from_corner_no > 0 and window length < 3: --from_corner_no
  else: result = false; break                            # FAIL
  ```
  The "violation corrected" branch exists because insertion checks run with clearance compensation
  while the final geometry may differ without it; retrying with more distant corners lets
  spring-over fix it (:199-216 comments).
- Stub cleanup (:265-282): for every interior corner, `board.get_trace_tail(corner, layer, net)`
  (BasicBoard.java:1474 — a trace ending at the point with **no contacts** at that end) is removed.
  v19 gates this on `test_level < ALL_DEBUGGING_OUTPUT`; v2 runs unconditionally.
- Record `first_corner` (first successful trace only) and `last_corner`.

### 4.3 Neckdown (:297-452)

`try_neck_down(from, to, layer, pin, at_start)`:
- Preconditions: pin on layer; `dist(pin_center, to) < 2*(pin_max_width/2 + clearance(trace_cl, pin_cl, layer))`;
  `pin.get_trace_neckdown_halfwidth(layer) < ctrl.trace_half_width[layer]`.
- `ok_length = board.check_trace_segment(from, to, ..., full width, only_not_shovable=true)`;
  `Integer.MAX_VALUE` ⇒ no conflict ⇒ nothing to do. Else `ok_length -= 2` (TOLERANCE).
- If `ok_length > 2`: split the segment at `neck_down_end_point = from + ok_length·dir` (rounded),
  bridging rounding error with up to two `calculate_additional_corner` knees (horizontal_first chosen
  by dominant axis), inserting each piece at full width via `insert_forced_trace_segment`; any piece
  failing ⇒ give up (return from-corner).
- Finally insert `neck_down_end_point → to` at `neck_down_halfwidth` (reduced width).

### 4.4 insert_via (:461-513)

Scan `ctrl.via_rule` in order (cheapest first); skip padstacks not spanning [from,to] layer range;
first candidate passing `ForcedViaAlgo.check(via_info, location, net, shove_depths, board)` wins;
then `ForcedViaAlgo.insert(...)` with `ctrl.trace_clearance_class_no` and per-layer
`ctrl.trace_half_width`. No candidate or insert failure ⇒ warn ⇒ false. (v2 passes two extra args to
`check` — traceHalfWidth array and trace clearance class — and differentiates "no via span in rule"
from "blocked" in diagnostics, FoundConnectionInserter.java:699-751.)

### 4.5 connect_to_trace (RoutingBoard.java:1143-1195)

Connecting a new route endpoint to the middle of an existing trace:
- if `to_trace.polyline().contains(p)`: nothing to do;
- else insert the perpendicular projection segment `projection_line(p).to_polyline()` (must have 3
  lines) after `check_polyline_trace`; then remove the tails that may have been created at the
  original trace's two end corners (`get_trace_tail`, unless user-fixed).

### 4.6 RoutingBoard.insert_forced_trace_polyline (RoutingBoard.java:682-982) — the shove core

Referenced here because insert and pull-tight interlock; the shove algorithms themselves
(ShoveTraceAlgo/spring_over) belong to a sibling spec.

1. Ends must be IntPoints. `start_marking_changed_area()`.
2. If exactly one same-net/width/class PolylineTrace ends at `from_corner`, remember it as
   `picked_trace` — its polyline is `combine`d with the new one so trace shapes at the junction are
   checked without dog-ears (:706-747).
3. `new_polyline = shove_trace_algo.spring_over_obstacles(...)`; null ⇒ return from_corner.
4. Compute the tail tile shapes of `combined_polyline` at
   `compensated_half_width = half_width + clearance_compensation(cl, layer)`; in 90° mode every
   shape is replaced by its bounding box. For each shape: `shove.check(...)` (only if `with_check`),
   then `shove.insert(...)`; insert failure ⇒ **return null** (board possibly damaged, undo needed).
5. If a check failed at shape k (`last_shape_no < len`): resample — shorten the polyline so the last
   segment is `sample_width = 2*min_trace_half_width` long (`Polyline.shorten`), unless the failed
   segment is longer than `100*sample_width` (too many cycles ⇒ return from_corner); re-check+insert
   the final shape; on success the returned `new_corner` is the shortened endpoint (:814-886).
6. Join every corner into `changed_area`; `insert_trace_without_cleaning(...)`; `new_trace.combine()`.
7. `new_trace.normalize(changed_area.get_area(layer))` — removes generated cycles ("otherwise
   pull_tight may not work correctly", :923). If normalization changed something, re-pick the trace
   containing `new_corner` (the object identity may have changed) and
   `pull_tight_algo.split_traces_at_keep_point()` first so `new_corner` survives optimization.
8. If `tidy_width > 0`: `new_trace.pull_tight(pull_tight_algo)` where the algo was built with
   `keep_point = new_corner`, `clip = new_corner.surrounding_octagon().enlarge(tidy_width)`
   (tidy_width==MAX_VALUE ⇒ clip null), `only_net_no = p_net_no_arr` iff `max_recursion_depth <= 0`
   (i.e. optimize only the own net when shoving was disabled), time_limit −1.
9. Return `new_corner` (== to_corner on full success; an intermediate point on partial success).

---

## 5. Pull-tight framework (PullTightAlgo / TraceTightener)

### 5.1 Construction & knobs

`PullTightAlgo.get_instance(board, only_net_no_arr, clip_shape, min_translate_dist, stoppable,
time_limit_ms, keep_point, keep_point_layer)` (PullTightAlgo.java:70-112) dispatches on
`rules.get_trace_angle_restriction()` to PullTightAlgo90 / 45 / AnyAngle and sets:

- `min_translate_dist = max(p_min_translate_dist, 100)` — p_min_translate_dist is the user's
  `trace_pull_tight_accuracy`, default **500** (Settings.java:84; AutorouteControl.pull_tight_accuracy
  also 500). This is the termination granularity of every binary-search translation (board units).
- `curr_clip_shape : IntOctagon` — moves whose touched corners fall outside are rejected.
- `keep_point/_layer` — traces through this point are split (`split_traces_at_keep_point`,
  :475-490) whenever a pull-tight succeeded, guaranteeing the point stays on the net.
- time limit: checked in `is_stop_requested()` (:191-220) together with the stoppable thread.
- Numeric guards (class constants): `c_max_cos_angle = 0.999` (angles closer than ~2.6° to 180° are
  not divided further, :21), `c_min_corner_dist_square = 0.9` (corners closer than √0.9 are "equal",
  :24), AnyAngle `SKIP_LENGTH = 10.0` (squared-distance threshold for short end segments,
  PullTightAlgoAnyAngle.java:20).

Working state per trace (set by `pull_tight(Polyline, layer, half_width, nets, cl, contact_pins)`,
:174-188): `curr_half_width = half_width + clearance_compensation(cl, layer)`, `contact_pins` = pins
touching the trace end corners (these are *not* obstacles; all other pins are, even own-net ones —
acid-trap avoidance, see check §5.3).

### 5.2 opt_changed_area (:119-168) — the regional driver

```
while something_changed:
  for layer i:
    region = changed_area.get_area(i);  skip if empty;  changed_area.set_empty(i)
    region = region.enlarge( 1.5 * (clearance_matrix.max_value(i) + 2*max_trace_half_width) )
    for obj in board.overlapping_objects(region, i):        # search-tree query
        if stop_requested: return
        if PolylineTrace t:
            if t.pull_tight(this):  something_changed = true
                                    if split_traces_at_keep_point(): break
            elif smoothen_end_corners_at_trace_1(t): something_changed = true; break  # items removed
        elif Via v && trace_cost_arr != null:
            OptViaAlgo.opt_via_location(board, v, trace_cost_arr, min_translate_dist, 10)
```

`ChangedArea` (board/ChangedArea.java; v2 board/state/ChangedArea.java) is one mutable
double-precision octagon per layer (8 extremes: lx, ly, rx, uy, x−y min/max, x+y min/max), grown by
`join(point/shape, layer)` from every mutation site (shove, split, combine, each pull-tight move),
converted to `IntOctagon` by floor/ceil. Pull-tight moves re-join their touched corners, so the loop
naturally chases ripples until a fixed point (or the time limit).

### 5.3 The clearance oracle: BasicBoard.check_trace_shape (BasicBoard.java:1047-1105)

Every candidate move is validated by building the would-be trace tile shape
(`Polyline.offset_shape(curr_half_width, i)` = convex shape of segment i widened by half-width)
and checking:
- shape must be inside the board bounding box;
- search-tree overlap query (with clearance compensation baked into the tree, or
  `overlapping_tree_entries_with_clearance` otherwise);
- an overlapping item is an obstacle unless `!item.is_trace_obstacle(net)` for some routed net;
- if `contact_pins != null`: contained pins are ignored, any **other** pin is an obstacle even if
  own-net (acid-trap prevention); foreign-net traces overlapping only inside a shared tie-pin's pad
  are tolerated (:1081-1099).

### 5.4 PolylineTrace.pull_tight (PolylineTrace.java:791-845)

Guards: on-board, not shove-fixed, `nets_normal()`, net filter (`only_net_no_arr`), net-class
`get_pull_tight()` flag (a per-net-class disable knob). Then
`new_lines = algo.pull_tight(lines, layer, half_width, nets, cl, touching_pins_at_end_corners())`;
if changed → `change(new_lines)` (§7.3) → true. If unchanged and angle mode ≠ 90° and
`rules.get_pin_edge_to_turn_dist() > 0`, try in order `swap_connection_to_pin(start/end)` and
`correct_connection_to_pin(start/end)` (§7.4); each success recurses into `pull_tight` once.

### 5.5 Termination / idempotence

Each `pull_tight(Polyline)` implementation is a **fixed-point loop by object identity**: functions
return the *input object* when nothing changed, and the loop `while (new_result != prev_result &&
!is_stop_requested())` exits when a whole round produces no new object. Individual moves guarantee
strict improvement (length decreases, corner count decreases, or a translation of at least
`min_translate_dist`); the reduce_corners length check `length_after >= length_before → reject`
(PullTightAlgoAnyAngle.java:168-177, incl. +1.5 rounding allowance) exists precisely to prevent
rounding-induced oscillation. Global termination is additionally bounded by the caller's time limit
(1000/2000 ms). Pull-tight is *not* strictly idempotent across calls — `opt_changed_area` may keep
finding work as neighbours move — but each polyline pass is monotone.

---

## 6. The local optimization rules per angle mode

### 6.1 Any-angle (PullTightAlgoAnyAngle.java) — the default and richest pipeline

```
pull_tight(polyline):                                   (:38-60)
  new = avoid_acid_traps(polyline)      # NOTE: dead — body starts `if (true) return p_polyline;`
                                        # (PullTightAlgo.java:516-519); spring-over wrap disabled
  loop until unchanged:
     p1 = skip_segments_of_length_0(p)      (base class, :347-401)
     p2 = reduce_lines(p1)                  (:284-396)
     p3 = skip_lines(p2)                    (:672-765)
     p4 = reduce_corners(p3)                (:65-222)
     p5 = reposition_lines(p4)              (:253-278 → reposition_line :501-670)
     p  = smoothen_corners(p5)              (:225-250 → smoothen_corner :398-498)
```

A Polyline here is an array of `Line`s (exact integer homogeneous lines); corner k = intersection of
lines k and k+1; lines 0 and n−1 are only direction carriers for the end corners.

- **skip_segments_of_length_0** (base): remove line i when corners i−1 and i coincide (exact equality
  at the ends — first/last corner must be preserved exactly; `distance_square < 0.9` in the middle).
  Removing a line joins two segments, so unless the removed line was a multiple of 45° (whose shapes
  get clipped by the bounding octagon anyway) the one or two affected offset shapes are re-checked
  with `check_trace_shape`.

- **reduce_lines**: for each middle line i, translate it onto the nearer of prev/next corner
  (corners of lines i−2/i−1 and i+1/i+2). Both corners must be on the same side (else the line is a
  genuine zig). After translating, step ±0.5 until the target corner is actually crossed; reject if
  both corners got crossed, or if the corner *beyond* the crossed one is on the wrong side. Build the
  reduced line array (dropping the crossed corner's line), check the one affected shape, accept ⇒
  `--i` (retry window). Effect: removes staircase artifacts by sliding a segment until a neighbor
  corner disappears.

- **skip_lines**: try deleting line i+1 (one or two lines) when the two corners adjacent to a line
  sit on opposite sides of a nearby line (`side1 != side2`), i.e. the polyline crosses back —
  `p_polyline.skip_lines(i+1, i+1|i+2)` then single-shape check; first success returns immediately.

- **reduce_corners**: replace two consecutive segments by one straight line between rounded
  IntPoints near corner i and corner i+2 (`new Line(round(new_a), round(new_b))`). End corners must
  be IntPoints to be touched; special perpendicular capping lines are synthesized at the ends
  (`dir.turn_45_degree(2)`), plus numerical-stability guards: the rebuilt intersections must be
  within `check_dist = 100` (squared units) of the intended points (:126-141), side-consistency
  checks near non-IntPoint ends (splitting-safety, :142-161), and the **anti-loop length rule**:
  `length_after (+1.5) < length_before` (:168-177). Then one shape check. Parallel neighbor lines are
  merged on the fly (:211-214).

- **reposition_lines / reposition_line** (segment sliding): for line i+2 with prev corner (i,i+1)
  and next corner (i+3,i+4): both on the same side ⇒ translate the line toward the nearer one by
  **binary search**: start at full `max_translate_dist`, halve `delta_dist` each iteration, add on
  success / subtract on failure, stop when `|delta| <= min_translate_dist`; first-iteration success
  at full distance short-circuits ("biggest possible change"). Numerical backoff: if the translated
  line lands on the wrong side of the nearest point, shave 0.5 off all distances and retry. Special
  cases: corners lying *on* the translate line (`|dist| < 1e-3`) extend the move to skipped corners
  before/after, which are then chained-translated onto the new line (:597-627) — "important because…
  3 or more consecutive corners are equal". Each trial builds the modified polyline and checks one
  offset shape. Success joins 4 corners into changed_area.
  The **base-class** variant (PullTightAlgo.java:244-341, used by 45°/90° and fork smoothing) is the
  same binary search with a first-iteration snap: it first tries the exact parallel line *through*
  the nearest IntPoint corner (`Line.get_instance(nearest_point, direction)`), which makes the common
  case exact and rounding-free.

- **smoothen_corners / smoothen_corner** (corner cutting): for corner (i+1,i+2) with angle not too
  flat (`cos > 0.999` ⇒ skip), build `translate_line` through the rounded corner with direction
  `middle_approx(prev_dir, next_dir)`, then binary-search the largest chamfer translation toward the
  nearer neighboring corner (same backoff/`min_translate_dist` scheme; `|max_translate_dist| < 1` ⇒
  skip). Success **inserts** a new line (corner count +1 lines, corner cut into two corners) — the
  next round's reduce/reposition passes then shorten further. This is what converges any-angle
  routes toward taut rubber-band paths.

- **smoothen_start/end_corner_at_trace** (:768-1014): fork/end-joint smoothing, see §6.4.

### 6.2 45° mode (PullTightAlgo45.java)

```
pull_tight: loop { reduce_corners; smoothen_corners; reposition_lines(base) }   (:38-48)
```

- **reduce_corners** (:57-218): corner-array based (all corners must be IntPoints; bail out
  otherwise). Sliding window of 4 corners. Colinear middle corners are dropped outright (side_of ==
  COLLINEAR, :91-105). Otherwise try to remove corner[1]/corner[2] by translating segment
  (2→1) by the vector (3−2) — new_corner = corner1 + (corner3 − corner2) — accepted iff the new
  corner is colinear with (corner0, corner1) (i.e. the two segments merge into one 45°-legal
  segment) and the two check polylines (new_corner↔corner1, new_corner↔corner3) pass
  `check_trace_shape`; on failure the mirrored translation toward corner0 is tried (:149-187).
  Duplicate corners are removed unconditionally.
- **smoothen_corners** (:224-264): for every pair of consecutive 45°-multiple lines whose
  directions' projection is not POSITIVE (i.e. ≥90° turn), insert a diagonal cut:
  - `smoothen_corner` (:374-457): direction = vector sum of the two segment directions; binary-search
    the cut distance toward the nearer neighbor corner (max capped at `dist−1`, min 1; standard
    check/backoff loop) — clearance-checked;
  - fallback `smoothen_sharp_corner` (:271-307): fixed small cut at
    `translate_dist = (√2 − 1) * curr_half_width` (clamped by neighbor corner distances, must be
    ≥ 0.99, then `max(dist−1, 1)`) — deliberately so small that **no clearance check is needed**;
  - `smoothen_non_integer_corner` (:313-367): two diagonal lines can intersect at a half-integer;
    replace the corner with a short axis-parallel line through the ceil/floor lattice point chosen by
    the quadrant of the neighbors — restores the all-IntPoint invariant that reduce_corners needs.
- reposition_lines: base-class version (§6.1).
- 90°-mode traces passing through here stay 90° except where smoothen introduces 45° diagonals —
  which is the intended "45° smoothing" of ninety-degree maze output when the board is in 45° mode.

### 6.3 90° mode (PullTightAlgo90.java)

```
pull_tight: loop { try_skip_second_corner; try_skip_corners; reposition_lines(base) }   (:30-40)
```

- **try_skip_second_corner** (:43-70): special-case flip of the first corner: reorder lines
  [1,0,3,4,…] (swapping which axis is traversed first), accept if both new shapes check out.
- **try_skip_corners** (:75-152): staircase removal — for each window, build the 4-line check
  polyline [prev, cur, arr[i−1], arr[i]] that jumps over two corners; both offset shapes must pass;
  bookkeeping for the second-last corner being skipped (:116-117, 136-147). Changed corners joined
  into changed_area.
- No fork smoothing (`smoothen_*_corner_at_trace` return null, :154-162): in 90° mode acute angles
  cannot occur.

### 6.4 Fork/end smoothing (smoothen_end_corners_at_trace, base + per-mode)

Driver `smoothen_end_corners_at_trace_1` (PullTightAlgo.java:413-469): only for non-shove-fixed
traces; temporarily sets `contact_pins = null` (allows sliding onto a contact trace that ends at a
pin). Loop: `smoothen_end_corners_at_trace_2` = try start corner then end corner
(`smoothen_start_corner_at_trace` / `smoothen_end_corner_at_trace`, abstract); a non-null adjusted
polyline is post-processed by `skip_segments_of_length_0`, then the trace is **removed and
re-inserted** (`insert_trace_without_cleaning`), followed by `split_traces` at both new end corners,
`normalize_traces(net)`, and `split_traces_at_keep_point`; repeat while improving. Note :432+444
double-remove of `curr_trace` (harmless; second call no-ops).

AnyAngle implementation (PullTightAlgoAnyAngle.java:768-1014), 45° analogous with an extra
"orthogonal contact line" requirement (PullTightAlgo45.java:499-501):

- Examine each PolylineTrace contact at the trace's end corner (any non-trace or shove-fixed contact
  ⇒ abort). Take the contact's adjacent line/corner (from whichever end matches; opposite() when
  entering from its far end).
- **acute_angle** case: `projection(this_end_dir, other_line_dir) == POSITIVE` and the previous end
  corner not collinear with the other line ⇒ the joint doubles back at an acute angle (acid trap).
  Fix: add a chamfer line at the end — direction = other line's direction turned ±90°
  (`turn_45_degree(2|6)` depending on which side the previous corner lies), translate distance
  `(√2−1)*curr_half_width` clamped by the two neighbor distances, must be ≥0.99, then `max(d−1,1)`;
  new polyline = [other_trace_line, add_line, …rest]. No clearance check here — the re-insert path
  splits/normalizes and later pull_tight passes will validate.
- **bend** case: `projection == ZERO` (perpendicular joint) and the *next* line continues in the
  contact's direction ⇒ append the contact's two lines and run `reposition_line` over the junction
  window; on success the trace's first/last line is replaced by the contact's line, effectively
  sliding the junction along the contact trace.
- AnyAngle extra: `skip_short_segment` — if the end corner is non-integer and closer than
  `SKIP_LENGTH=10.0` (squared) to its neighbor, operate one line further in.

---

## 7. Trace normalization (split / combine / cycle removal)

### 7.1 split (PolylineTrace.java:436-622) — self- and cross-intersection splitting

For each own segment i (optionally only those intersecting `p_clip_shape`):
query the default search tree for entries overlapping the segment's tile shape on the layer; for
each same-net found item:

- **own trace**: skip neighbor segments (index within ±1) and zero-length-intermediate artifacts
  (corner equality tests :477-485); otherwise treated like any other trace (self-touch ⇒ split).
- **PolylineTrace**: compute `LineSegment.intersection` both ways; first split the *found* trace at
  each intersection line (`found_trace.split(line_no, intersection_line)` :685-719 — splits the
  polyline, removes the item, inserts the two pieces with `insert_trace_without_cleaning`), re-read
  the overlapping entries afterwards (board changed); then split the own trace; own pieces recurse
  into `split(p_clip_shape)`. After any split, run cycle removal over all pieces —
  `board.remove_if_cycle(piece)` — found-trace pieces first, own pieces last ("to preserve them, if
  possible", :575-578).
- **DrillItem**: if the segment contains the drill center, split there (perpendicular split line).
- **ConductionArea** (own net, trace not user-fixed, net class not `ignore_cycles_with_areas`): if
  the area is contacted at *both* trace ends the trace is a cycle through the plane ⇒ remove it and
  return.

Split refusal guard `split_inside_drill_pad_prohibited` (:630-657): never split inside a pin pad
except exactly at the pin center, unless another trace ends at the intersection (vias intentionally
excluded — splitting at via off-centers broke autoroute connections, comment :627).

`Trace[] split(Point)` (:665-678): public point-split used by `split_traces_at_keep_point` and
`BasicBoard.split_traces` — finds the containing segment, splits with the perpendicular line.

### 7.2 combine (:185-426) — merging colinear/joined pieces

`combine()` repeatedly tries `combine_at_start` / `combine_at_end` (recursing until neither fires),
then notifies observers. `combine_at_start` (`_at_end` mirrored):
- contacts at the corner (ConductionAreas ignored) must be exactly one, and it must be a
  PolylineTrace with **same layer, same nets, same half-width, same fixed state**, ending (or
  starting ⇒ `reverse_order`, lines reversed with `opposite()`) at that corner.
- Concatenate line arrays, dropping one line if the junction lines are equal-or-opposite
  (`skip_line`, exact colinear join). If the Polyline constructor collapses consecutive parallel
  lines (length mismatch), fall back to remove+reinsert in the search tree; otherwise reuse tree
  entries via `search_tree_manager.merge_entries_in_front/_at_end` (performance path).
- Traces reduced below 3 lines are removed; the other trace is always removed; junction corner is
  joined into changed_area.

### 7.3 change(new_polyline) (:921-987)

In-place geometry update used by pull-tight: save undo, find first/last differing line vs. the old
polyline, keep the unchanged head/tail search-tree entries
(`keep_at_start = max(first_diff−2, 0)`, `keep_at_end = max(len−last_diff−3, 0)`,
`search_tree_manager.change_entries`), notify observers, then **normalize against the current
changed-area clip octagon** for this layer.

### 7.4 normalize (:725-784; v2 PolylineTraceNormalization.java)

```
normalize(clip, depth):
    if depth > MAX_NORMALIZATION_DEPTH (=16): v19 THROWS Exception; v2 logs+returns false
    pieces = this.split(clip);  result = (pieces.size() != 1)
    for piece on board:
        combined = piece.combine()
        if piece has 2 corners and first==last: remove it (degenerate)   [v2: skip if USER_FIXED,
                                                        preventing an infinite outer loop]
        elif combined: piece.normalize(clip, depth+1); result = true
    return result
```

Board-level `normalize_traces(net)` (BasicBoard.java:888-920): loop over all items of the net until
a full pass changes nothing; per trace `normalize(null)`; if unchanged and not user-fixed,
`remove_if_cycle`. A `ConcurrentModificationException` on the item iterator simply restarts the pass.
`combine_traces(net)` (:862-885) is the combine-only variant used after ripup.

`remove_if_cycle(trace)` (BasicBoard.java:1504-1536): if `trace.is_cycle()`, snapshot whether tails
exist at its two end corners, remove the trace's whole connection (`get_connection_items`), then
remove any *newly created* tails (only where there was none before) with their connections.

`get_trace_tail(location, layer, nets)` (BasicBoard.java:1474-1498): a same-net trace whose
first/last corner equals the location and has zero contacts at that end.

### 7.5 Connection quality metric (autoroute/Connection.java)

Used by MazeSearchAlgo to price ripping an existing connection (MazeSearchAlgo.java:1465-1467):
- `Connection.get(item)`: walk from a routable item through unique normal contacts until a fork
  (contact count ≠ 1 at a point, or ambiguous contact point) or non-route item on each side;
  produces `{start_point/layer, end_point/layer, item_list}`; cached per item in
  `ItemAutorouteInfo.precalculated_connection`.
- `get_detour() = (trace_length + DETOUR_ADD) / (airline + DETOUR_ADD) + DETOUR_ITEM_COST * (n_items − 1)`
  with **DETOUR_ADD = 100**, **DETOUR_ITEM_COST = 0.1** (Connection.java:13-14); MAX if either
  endpoint is null. Higher detour ⇒ cheaper to rip.

---

## 8. Cost/quality knobs (consolidated)

| Knob | Value / default | Where |
|---|---|---|
| `pull_tight_accuracy` → `min_translate_dist` | 500 → `max(x,100)` | Settings.java:84, PullTightAlgo.java:110 |
| `trace_pull_tight_region_width` (interactive tidy width) | `Integer.MAX_VALUE` | Settings.java:83 |
| `PULL_TIGHT_TIME_LIMIT` (removals, interactive) | 2000 ms | RoutingBoard.java:37 |
| `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` (per routed conn.) | 1000 ms | BatchAutorouter.java:39, RoutingBoard.java:1063 |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | 2 | AutorouteEngine.java:30 |
| AnyAngle locator `c_tolerance` | 1.0 | LocateFoundConnectionAlgoAnyAngle.java:22 |
| rear-window clearance re-check depth | 5 doors | ...AnyAngle.java:328 |
| `c_max_cos_angle` | 0.999 | PullTightAlgo.java:21 |
| `c_min_corner_dist_square` | 0.9 | PullTightAlgo.java:24 |
| AnyAngle `SKIP_LENGTH` (squared) | 10.0 | PullTightAlgoAnyAngle.java:20 |
| reduce_corners `check_dist` (squared) | 100 | ...AnyAngle.java:126 |
| reposition on-line epsilon | 0.001 | ...AnyAngle.java:524 |
| numeric backoff step | 0.5 | all binary-search loops |
| 45° chamfer distance | `(√2−1)·half_width`, clamp ≥0.99 → `max(d−1,1)` | PullTightAlgo45.java:290-298, AnyAngle:853-861 |
| changed-area enlargement | `1.5·(max_clearance(layer) + 2·max_half_width)` | PullTightAlgo.java:137-140 |
| shove resample width | `2·min_trace_half_width`; give up if segment > 100× | RoutingBoard.java:821-827 |
| `max_shove_trace_recursion_depth` / via / spring-over | 20 / 5 / (ctrl) | AutorouteControl.java:112-113 |
| neckdown TOLERANCE | 2 | InsertFoundConnectionAlgo.java:341 |
| `MAX_NORMALIZATION_DEPTH` | 16 | PolylineTrace.java:33 |
| detour constants | ADD=100, ITEM_COST=0.1 | Connection.java:13-14 |
| via-opt iterations | `opt_via_location(..., min_translate_dist, 10)` | PullTightAlgo.java:160-162 |
| pin edge-to-turn (enables pin-exit correction) | `rules.get_pin_edge_to_turn_dist() > 0`; forced to −1 during insertion | PolylineTrace.java:824-825, InsertFoundConnectionAlgo.java:124 |

---

## 9. Edge cases worth preserving

- Start info door not a TargetItemExpansionDoor ⇒ locator yields a null skeleton; engine treats as
  NOT_ROUTED (LocateFoundConnectionAlgo.java:87-95, AutorouteEngine.java:163-165).
- Fanout destinations end at an `ExpansionDrill` — the first trace starts *at* the drill without
  start-corner adjustment (`at_fanout_end`), and `target_item == null` so no `connect_to_trace`.
- Conduction-area targets shrink the target shape by the trace half-width so the connection lands
  safely inside (LocateFoundConnectionAlgo.java:154-164).
- `insert_forced_trace_polyline` returns three distinct things: `to_corner` (success), some
  intermediate/first corner (partial; caller may neckdown or widen the window), `null`
  (**board damaged; undo required**).
- Pull-tight of a trace can delete *other* items (splits, cycle removal, fork smoothing) — hence the
  `break`-and-rescan discipline in `opt_changed_area` and the re-picking of `new_trace` by point
  after normalization (RoutingBoard.java:944-953).
- `avoid_acid_traps` is disabled dead code in both trees (PullTightAlgo.java:516-519 `if (true) return`).
- v19 `normalize` throws past depth 16 (caught and logged by callers); v2 stops silently — port the
  v2 behavior.
- The 45° locator's acute-corner guard needs a Simplex conversion because IntBox/IntOctagon carry
  zero-length border lines (LocateFoundConnectionAlgo45Degree.java:233-236).
- combine requires identical `FixedState` — a SHOVE_FIXED pin-exit stub never merges with the
  routed trace (until `swap_connection_to_pin` flips the states, PolylineTrace.java:1296).
- `check_trace_shape` treats own-net pins that are *not* endpoint contacts as obstacles; tie-pin
  foreign-net overlap is allowed only inside the shared pad (BasicBoard.java:1066-1099).

## 10. v2 (2.3.0) behavioral deltas vs v19

1. **Normalization depth**: v2 returns false at depth 16 instead of throwing
   (PolylineTraceNormalization.java:26-41); degenerate 1-corner traces that are USER_FIXED are
   skipped instead of removed (infinite-loop fix, :102-121).
2. **FoundConnectionInserter** (FoundConnectionInserter.java):
   - single-corner ResultItem sets `firstCorner` too (:127-136); null-guards before
     `connectToTrace` (:77-106);
   - fanout **micro-neckdown fallback** (:210-217, 455-523): when a fanout segment insert fails,
     retry the segment at candidate half-widths {pin neckdown widths, ¾, ⅗, ½ of base} until one
     fits — new in v2;
   - stub-tail cleanup runs unconditionally (v19 gated by test level);
   - `ForcedViaInserter.check` takes traceHalfWidth[] + clearance class; via-failure diagnostics
     distinguish "no padstack spans the layers" from "blocked by clearance/shove".
3. Everything in the pull-tight family (`TraceTightener*`) and locators is a pure rename; constants
   and control flow are unchanged. `PullTightAlgo.only_net_no_arr` became public; `ShoveTraceAlgo` →
   `TraceShover`, `OptViaAlgo` → `ViaOptimizer`, `ForcedViaAlgo` → `ForcedViaInserter`.

---

## 11. Rust design implications

### Keep semantically

- The three-phase contract: locate (pure, produces `Vec<ResultItem>{ corners: Vec<IntPoint>, layer }`)
  → insert (mutating, incremental with shove, per-segment ok-point protocol) → regional pull-tight
  driven by a per-layer changed-area octagon. Keep the exact ok-point trichotomy
  (success / partial / corrupted) — the whole retry ladder (window widening, neckdown, resampling)
  hangs off it.
- Exact-line polyline representation (lines, not points) for pull-tight: every move is "replace /
  insert / delete one Line", corners are derived. This is what makes tree-entry reuse
  (`change_entries`, `merge_entries_*`) and shape-index bookkeeping possible.
- The fixed-point-by-identity idiom maps cleanly to returning `Option<Polyline>`
  (None = unchanged) or a `Cow<'_, Polyline>`; keep per-move monotonicity guards (length check with
  +1.5 allowance, min_translate_dist granularity, 0.5 numeric backoff) — they are load-bearing for
  termination, not cosmetics.
- All constants of §8 verbatim; behavior parity depends on them (especially min_translate_dist
  floor 100, TRACE_WIDTH_TOLERANCE 2, the 5-door rear window, and the (√2−1)·hw chamfer).
- v2 deltas (§10) — they are bug fixes.

### Restructure

- **Arena the polylines.** Every candidate move in Java allocates: `new Line[...]` + arraycopy + `new
  Polyline(...)` per *trial* of the binary search (PullTightAlgoAnyAngle.java:440-443 allocates the
  full line array once per smoothen_corner call, then `new Polyline(curr_lines)` per iteration at
  :456; reposition_line same at :566-569/628; base reposition_line builds a fresh 3-line Polyline per
  iteration, PullTightAlgo.java:310). In Rust: one scratch `Vec<Line>` per worker + a small
  `SmallVec<[Line;8]>` for 3-line check windows; offset-shape trials should write into a reusable
  shape buffer.
- **Exact-arithmetic hot spots**: `Line` is integer homogeneous; `Line.translate`, `intersection`,
  `side_of` are the inner loop of every binary search, and Freerouting's `Point.side_of`/
  `scalar_product` escalate to BigInteger on overflow (the locator's tangential-point math is
  FloatPoint, but every check polyline construction and `side_of` on Line/IntPoint is exact).
  Use i64 with overflow-checked fast path → i128 fallback; never heap-allocate rationals.
  `Polyline`'s constructor deduplicates parallel lines — make that an explicit normalization step
  returning counts, since callers branch on `tmp.arr.length == expected` as a validity check
  (e.g. PullTightAlgoAnyAngle.java:376, 458; keep that contract).
- **check_trace_shape** dominates runtime: each trial does a search-tree region query + obstacle
  filtering. Restructure as (a) a cached query context per pull-tight pass (segment shapes usually
  shrink monotonically during the binary search — the *first* failing obstacle is very likely the
  binding constraint, so cache it and re-test it first before a full query), and (b) SoA tree
  entries with layer-partitioned R-tree for cache locality. The Java code re-queries from scratch
  every iteration.
- **Iterator invalidation discipline** (`split` re-reading tree entries mid-loop,
  PolylineTrace.java:524-529; `opt_changed_area` breaking out after item removal) should become an
  explicit worklist: collect candidate (trace-id, segment) pairs, validate liveness by generation
  counter, requeue on mutation. That both removes the O(n) rescans and gives a natural unit for
  parallelism.
- **Parallelism**: pull-tight is board-mutating and clearance-coupled, so parallelize at the
  *region* level: changed-area octagons per layer are already independent per iteration of the outer
  loop; shard by disjoint enlarged regions (the 1.5·(clearance+2·width) margin defines the
  interference radius). Within a region stay single-threaded — moves invalidate neighbors. The batch
  optimizer (rip + reroute per item) is the coarse-grained parallel level (upstream's BatchOptRouteMT
  already does board-clone parallelism; keep that shape).
- **Recursion → loops**: `combine()` self-recurses per merged neighbor (PolylineTrace.java:190-198)
  and `normalize` recurses to depth 16 with `split` recursing per piece (:546-549) — make both
  explicit worklists; also avoids v19's depth-16 exception path entirely.
- `Connection::get` caching (precalculated_connection per item) should become a net-indexed
  connection cache invalidated by item-generation, not per-item Option fields.

### Java-isms observed (file:line evidence)

- Per-call `LinkedList` for corner lists and results: LocateFoundConnectionAlgo.java:400, 441;
  ...AnyAngle.java:73; splitter's tree-entry lists PolylineTrace.java:454-456 (fresh LinkedList per
  segment per pass).
- `TreeSet<Item>` (ordered by id) for ripped sets and contact sets everywhere
  (BatchAutorouter, AutorouteEngine.java:178-180, Connection.java:50) — order only matters for
  determinism; a sorted Vec of ids suffices.
- `Arrays.copyOfRange` + `new Polyline` per insertion window (InsertFoundConnectionAlgo.java:153-154)
  — every corner-window retry re-allocates.
- Repeated full-board iterator scans with restart-on-change: `normalize_traces` / `combine_traces`
  (BasicBoard.java:862-920) are O(items) per changed trace; index by net.
- Clone-heavy line-array editing: `System.arraycopy` into a fresh `Line[]` for every candidate in
  reduce_corners/smoothen/reposition (...AnyAngle.java:230-231, 259-260, 440-443, 566-569) and every
  combine (PolylineTrace.java:265-306).
- Exception-as-control-flow: `ConcurrentModificationException` restart (BasicBoard.java:898-901),
  normalization-depth Exception (PolylineTrace.java:730-732) — both should be ordinary control flow.
- Identity comparison (`!=`) as change signal (`new_lines != lines`, PolylineTrace.java:819;
  `new_result != prev_result` loops) — in Rust use explicit change flags, not pointer identity.
