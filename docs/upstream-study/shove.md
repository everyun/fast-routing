# Push-and-Shove (Trace/Via/Pad Shoving) — Freerouting Upstream Study

Source trees studied:

- **v1.9 (classic)**: `/tmp/fr-java/src_v19/main/java/app/freerouting/…` — this is the semantic
  reference; the 2.x tree is a rename/repackage of it (verified by structural diff, see §14).
- **v2.x (current, what 2.3.0 ships)**: `/tmp/fr-java/src/main/java/app/freerouting/…`

File map (v19 → v2). Unless flagged, the v2 file is a camelCase rename of the v19 file with
**identical numeric constants and identical control flow** (verified by whitespace/identifier-
normalized diff and by diffing the multiset of numeric literals per file):

| v19 file (board/ unless noted)            | v2 file                                              | Changed? |
|-------------------------------------------|------------------------------------------------------|----------|
| `ShoveTraceAlgo.java`                     | `board/optimize/TraceShover.java`                    | rename only |
| `ShapeTraceEntries.java`                  | `board/searchtree/ShapeTraceEntries.java`            | rename only |
| `CalcFromSide.java`                       | `board/model/structure/ShapeEntrySide.java`          | **YES** — extra fallback, §13.1 |
| `CalcShapeAndFromSide.java`               | `board/model/structure/ShapeAndEntrySide.java`       | rename only |
| `ForcedPadAlgo.java`                      | `board/actions/ForcedPadRouter.java`                 | rename only |
| `ForcedViaAlgo.java`                      | `board/actions/ForcedViaInserter.java`               | **YES** — hole clearance + trace-shape check, §13.2 |
| `MoveDrillItemAlgo.java`                  | `board/actions/DrillItemMover.java`                  | rename only |
| `autoroute/MazeShoveTraceAlgo.java`       | `autoroute/maze/MazeTraceShover.java`                | rename only |
| `autoroute/MazeSearchAlgo.java` (shove/ripup parts) | `autoroute/maze/MazeSearchEngine.java` + `MazeRipupResolver.java` | split, same logic |
| `RoutingBoard.java` (forced-insert entry points) | `board/facade/RoutingBoard.java` (+ `RoutingBoardSearchFacade`) | facade split, same logic |
| `autoroute/InsertFoundConnectionAlgo.java`| `autoroute/…` (same role)                            | same logic |
| `autoroute/AutorouteControl.java`         | `autoroute/maze/AutorouteControl.java`               | same knob values |

There is **no `ShovePadAlgo` / `ShoveViaAlgo` / `CalcShoveTraceEntries`** in either tree; those
roles are filled by `ForcedPadAlgo`, `MoveDrillItemAlgo`, and `ShapeTraceEntries` respectively.
Note the confusing naming: the v19 `MazeSearchAlgo.ShoveResult` class (v19 `MazeSearchAlgo.java:1704-1729`)
references `MazeShoveViaAlgo` / `MazeShoveThinRoomAlgo` in its javadoc — those classes do not
exist in either tree (dead comment inherited from the original Alfons Wirtz sources).

All line references below are to the **v19** files unless prefixed `v2:`.

---

## 1. Where shove fits in the router

Shove is used at **three distinct times**:

1. **During maze expansion** (cheap, geometric, *no board mutation*): when the wavefront wants to
   pass through an `ObstacleExpansionRoom` whose item is a same-width/same-clearance-class trace,
   `MazeSearchAlgo.shove_trace_room` → `MazeShoveTraceAlgo.check_shove_trace_line` →
   static `ShoveTraceAlgo.check(...)` decides whether the obstacle trace *could* be pushed left or
   right, and if so which doors of the obstacle room become reachable. The board is not modified;
   the "shove will happen later" is encoded in the expansion graph via door
   sections + `MazeSearchElement.Adjustment.{LEFT,RIGHT}`.

2. **At insertion time** (destructive, mutates the board): after `LocateFoundConnectionAlgo`
   reconstructs the corner list from the maze backtrack, `InsertFoundConnectionAlgo` inserts each
   trace polyline via `RoutingBoard.insert_forced_trace_polyline`, which per tile shape runs
   instance `ShoveTraceAlgo.check(...)` (non-destructive full check) and then
   `ShoveTraceAlgo.insert(...)` (cuts obstacle traces, inserts substitute pieces, recursively
   shoves what's behind them). Vias are inserted through `ForcedViaAlgo.check/insert` →
   `ForcedPadAlgo.check_forced_pad/forced_pad` per padstack layer.

3. **Interactive routing / component drag**: `RoutingBoard.move_drill_item` and interactive
   `Route` use the same `MoveDrillItemAlgo` / `insert_forced_trace_segment` machinery.

Contract everywhere: **`check` is side-effect-free; `insert` may damage the board if it fails**
(returns false ⇒ caller must undo). Comments state this explicitly
(`ShoveTraceAlgo.java:380-383`, `ForcedPadAlgo.java:337-341`, `RoutingBoard.java:676-681`).

---

## 2. Core data structure: `CalcFromSide` (v2 `ShapeEntrySide`)

`board/CalcFromSide.java` (125 lines). Immutable value: which border edge of a tile shape the
shove "comes from", plus an approximate intersection point on that edge.

```java
final int no;                        // border-line index of the shape, -1 = not calculated
final FloatPoint border_intersection; // approx point on that border line (may be null)
static final CalcFromSide NOT_CALCULATED = new CalcFromSide(-1, null);
```

Four constructors (all used):

1. **`CalcFromSide(Polyline p_polyline, int p_no, TileShape p_shape)`** (`:22-58`) — walk trace
   line segments *backwards* from index `p_no` down to 1; first segment that intersects the shape
   border gives `fromside_no = intersections[0]` and the intersection point. If no border
   intersection found (first polyline corner is inside the shape), fall back to: for every border
   line i, intersect with `p_polyline.arr[1]`, pick the border line whose intersection is nearest
   to corner 0. Used to compute the shove direction when inserting a trace shape that continues an
   existing polyline.

2. **`CalcFromSide(Point p_from_point, TileShape p_shape)`** (`:64-71`) — nearest border point of
   the shape to `p_from_point`, side = `contains_on_border_line_no(projection)`. Used by
   `MoveDrillItemAlgo` (shove direction when translating a via: from-side is the side facing the
   original center).

3. **`CalcFromSide(LineSegment p_line_segment, TileShape p_shape, boolean p_shove_to_the_left)`**
   (`:77-119`) — the *maze-time* constructor. Finds the "front side" of the shape relative to the
   segment: iterate corners, detect the border line where consecutive corners lie on opposite
   sides of the segment's line **and** whose intersection with the line is closer to the segment
   start than to its end (i.e. the side behind the start point). Then:
   - `shove_to_the_left`  ⇒ `no = (front_side_no + 2) % border_line_count`
   - else                 ⇒ `no = (front_side_no + border_line_count - 2) % border_line_count`
   `border_intersection` = middle point of that side. Failure (front side not found) ⇒ warn,
   `no = -1` in v19 (v2 adds a fallback; §13.1).

4. **`CalcFromSide(int p_no, FloatPoint p_border_intersection)`** — raw.

Semantics of `no`: with entries sorted counterclockwise around the shape border, a shove "from
side k" means substitute traces are routed around the *other* sides; `+2 / -2` in constructor 3
selects the lateral side so shoving left vs right walks around opposite sides of the trace shape.

---

## 3. Core data structure: `ShapeTraceEntries`

`board/ShapeTraceEntries.java` (793 lines). One instance per shove shape. It classifies every
item overlapping the shove shape and computes the *substitute traces*: the pieces of obstacle
traces re-routed around the shape border.

State:

```java
static final double c_offset_add = 1;      // :15  — +1 board unit safety on every offset shape
final Collection<Via> shove_via_list;      // foreign vias overlapping the shape (LinkedList)
private final TileShape shape;             // the shove shape (trace shape / pad shape)
private final int layer;
private final int[] own_net_nos;           // nets of the item being inserted
private final int cl_class;                // clearance class of the inserted item
private CalcFromSide from_side;            // may start as NOT_CALCULATED, computed lazily
private EntryPoint list_anchor;            // singly linked, sorted CCW around the border
private int trace_piece_count;             // number of substitute traces to produce
private int max_stack_level;               // nesting depth of foreign traces in the shape
private boolean shape_contains_trace_tails;
private Item found_obstacle;               // blame item on failure
```

`EntryPoint` (`:777-792`): `{PolylineTrace trace; int trace_line_no; FloatPoint entry_approx;
int edge_no; int stack_level = -1; EntryPoint next;}` — one intersection of an obstacle trace's
polyline with the *offset* border of the shove shape. `edge_no` may exceed
`border_line_count` after `resort()` (wrap marker).

### 3.1 `store_items(item_list, is_pad_check, copper_sharing_allowed)` (`:162-208`)

Classification of each overlapping item (obstacles come from
`ShapeSearchTree.overlapping_items_with_clearance(shape, layer, {}, cl_class)`):

| item | handling |
|---|---|
| `ViaObstacleArea` (unless pad check) / `ComponentObstacleArea` | ignored |
| `ConductionArea` with own net, or with `!get_is_obstacle()` | ignored |
| any item `is_shove_fixed()` and not own net | **hard fail**, `found_obstacle = it` |
| `Via` | added to `shove_via_list` iff pad check OR foreign net |
| `PolylineTrace` | `store_trace` (below); fail propagates |
| anything else, own net | ok iff `copper_sharing_allowed`; for pad check must additionally be a `Pin` with `drill_allowed()` |
| anything else, foreign net | **hard fail** |

Then: `search_from_side()` (if `from_side.no < 0`, pick the edge of the first entry belonging to
the own net, `:430-446`), `resort()`, and returns `calculate_stack_levels()`.

Note `is_shove_fixed()` = `fixed_state >= SHOVE_FIXED` (`Item.java:808`), states are
`UNFIXED < SHOVE_FIXED < USER_FIXED < SYSTEM_FIXED` (`FixedState.java`).
`Trace.is_routable()` / `Via.is_routable()` = `!is_user_fixed() && net_count() > 0`.

### 3.2 `store_trace(trace)` (`:310-428`) — the entry-point computation

1. Build `offset_shape` from the shove shape:
   - clearance compensation ON: `shape.offset(trace.compensated_half_width + c_offset_add)`
   - OFF: two symmetric steps: `shape.offset(trace.half_width)` then
     `.offset(clearance(trace.cl_class, this.cl_class, trace.layer) + c_offset_add)`
   (comment `:317`: "enlarge the shape in 2 steps for symmetry reasons"; comment `:325`:
   using `enlarge` instead of `offset` breaks an EntryPoint comparison).
2. `entries = offset_shape.entrance_points(trace.polyline())` — pairs
   `(trace_line_no, border_line_no)`; for each, compute
   `entry_approx = trace_line ∩ border_line` and `insert_entry_point(...)`.
3. **Trace-end-inside-shape handling** (foreign nets only, `:341-425`): for each trace end corner
   contained in `offset_shape`:
   - non-normal net (`nets_normal()` false) ⇒ fail.
   - inspect the end's contact items:
     - contact not `is_routable()` ⇒ **fail**, blame contact.
     - contact `Trace` that is shove-fixed OR different half-width OR different clearance class,
       with end corner strictly inside ⇒ **fail** (can't splice).
     - contact `Via`: compute `via_trace_diff = via_shape.smallest_radius() −
       trace.compensated_half_width`, adjusted by `via_clearance − trace_clearance` when trace
       clearance is larger (no compensation case). `< 0` (via thinner than trace) ⇒ **fail**;
       `== 0` and end corner on the border ⇒ via needn't be shoved, don't store the end corner.
   - exactly 1 contact and `store_end_corner` ⇒ synthesize an entry point at the projection of the
     end corner onto the border (`nearest_border_point`), `trace_line_no` = 0 or last.
   - 0 contacts and corner strictly inside ⇒ `shape_contains_trace_tails = true` (used only by the
     static maze-time check to bail out).

### 3.3 `insert_entry_point` (`:734-771`)

Sorted insertion into the linked list by `edge_no`, ties broken by scalar-product projection along
the edge (`prev_corner.scalar_product(p_entry, next_corner)`), i.e. CCW arc-length order around
the border.

### 3.4 `resort()` (`:449-573`)

Rotates the list so it starts at the middle of `from_side.no` (or at the projection of
`from_side.border_intersection` onto that edge when provided; if the projection is past the edge's
far corner, the border_intersection is dropped, `:466-473`). Entries "before" the cut get
`edge_no += edge_count` so ordering stays monotone. Then:

- Collapse runs: if three consecutive entries `prev,curr,next` all belong to the same net set,
  drop `curr` (only first and last intersection of a connected set matter, `:541-553`).
- Strip own-net entries from the start and end of the list (`:556-573`).

### 3.5 `calculate_stack_levels()` (`:576-671`) — the "can it be shoved" stack calculus

This is the heart of the "trace entries" from-side calculus. Walking the resorted CCW list, each
maximal group of same-net entries becomes one substitute trace piece with a *stack level* =
nesting depth relative to the from side:

```
curr_level = own-net first entry ? 0 : 1
loop over curr_entry:
  if curr_entry.stack_level < 0:
      ++trace_piece_count
      curr_entry.stack_level = curr_level
      max_stack_level = max(...)          // if raising past 1, blame curr_entry.trace
  scan the remainder of the list:
      mark all entries with the same net set: stack_level = curr_level (last one = last_own_entry)
      remember first entry of a foreign net set (first_foreign_entry)
  if a foreign set starts BEFORE our last occurrence:       // interleaved ⇒ nested
      next = first_foreign_entry
      if next.stack_level already set ⇒ FAIL (stack property violated: crossing, not nesting)
      ++curr_level
  else if our set has later occurrences:
      next = last_own_entry                                  // close our own bracket
  else:
      next = first_foreign_entry
      if next.stack_level already set:                       // popping a bracket
          --curr_level;  if next.stack_level != curr_level ⇒ FAIL
  splice out everything between curr_entry and next; continue from next
final check: curr_level must be back to 1, else warn + FAIL
```

Interpretation: the sequence of border entries must form a properly nested bracket structure
(like balanced parentheses) — traces overlapping the shove shape must be strictly nested "in
front of" each other seen from the from-side; any crossing pattern (a genuine weave) is
unshovable. `stack_depth() = max_stack_level`; **every caller treats `stack_depth() > 1` as a
failure for check paths** (`ShoveTraceAlgo.java:76-78,270-273`, `ForcedPadAlgo.java:289-292`),
i.e. only a single layer of obstacle traces may be between the shover and free space; deeper
nesting is handled instead by the recursion in `check/insert` (the substitute trace of level-1
obstacles is itself shoved).

### 3.6 `next_substitute_trace_piece()` / `pop_piece()` (`:214-269`, `:678-732`)

`pop_piece` removes from the list the first maximal run of entries at `max_stack_level` with equal
nets, returns `[first_entry, last_entry]`, then recomputes `max_stack_level` over the remainder
and decrements `trace_piece_count`. Own-net pieces are skipped (recursion) — they occur only at
level 0. So pieces are produced **outermost (deepest level) first**.

`next_substitute_trace_piece` builds the actual substitute polyline for the popped pair:

```
offset_shape = shove shape offset by (obstacle trace half-width [+ clearance] + c_offset_add)
                                     (same 1-step/2-step rule as store_trace)
edge_diff    = entries[1].edge_no - entries[0].edge_no     // may span the wrap thanks to +edge_count
piece_lines  = [ trace_line(entry0) ,
                 offset_shape.border_line(edge0 % edge_count), … CCW … ,   // edge_diff+1 border lines
                 trace_line(entry1) ]
```

⇒ a `PolylineTrace` (UNFIXED, same net/half-width/clearance-class as the obstacle) hugging the
offset border between the two entry lines. Empty polyline ⇒ recurse to next piece.

### 3.7 `cutout_trace` (static, `:52-93`) and `fast_cutout_trace` (`:96-136`)

`cutout_traces(obstacles)` (insert path only) removes the middle of each foreign obstacle trace:
compute the same offset shape, `offset_shape.cutout(trace.polyline())` ⇒ pieces. Optimization: if
exactly 2 pieces and both remaining ends are outside the shape, `fast_cutout_trace` re-uses the
search-tree entries (`search_tree_manager.reuse_entries_after_cutout(old, start, end)`) instead of
full remove+insert — flagged "performance critical" (`:95`). Otherwise: remove item, insert each
piece with `insert_trace_without_cleaning`.

---

## 4. `CalcShapeAndFromSide` (v2 `ShapeAndEntrySide`)

`board/CalcShapeAndFromSide.java` (117 lines). Prepares one tile shape of a *substitute trace*
for the recursive shove, cutting off "dog ears" and computing its from-side.

- Orthogonal mode (shove shape was an `IntBox`): `shape = tree_shape.bounding_box()`, from-side
  computed later.
- Else: `shape = tree_shape.to_Simplex()`; if the segment is the last one of the trace (index ==
  line_count−3) or its end corner is within `compensated_half_width` of the trace's final corner,
  intersect with the half-plane of the trace's **last** line (oriented so corner −3 is right of
  it) — `calc_cutline_at_end` (`:77-97`); symmetric `calc_cutline_at_start` with the first line
  (`:99-116`). If a cut was applied, the from-side is that cut line's index in the new shape with
  `border_intersection` = cut line ∩ that border line.
- `p_in_shove_check == true` (check paths): if no cut produced a from-side, leave from_side
  `null` — comment `:68-70`: computing it from the polyline here "may produce an undesired
  stack_level > 1". `null` from-side makes `ShapeTraceEntries` compute it lazily from own-net
  entries. In insert paths (`p_in_shove_check == false`) fall back to
  `CalcFromSide(polyline, index, shape)`.

---

## 5. `ShoveTraceAlgo` (v2 `TraceShover`) — the trace shover

`board/ShoveTraceAlgo.java` (830 lines). Holds only `final RoutingBoard board`.

### 5.1 Static `check(board, line_segment, shove_to_the_left, layer, net_nos, half_width, cl_type, max_rec, max_via_rec) → double` (`:37-204`)

Maze-time primitive. Returns the **maximum length from segment start for which the shove
succeeds**; `Integer.MAX_VALUE` ⇒ fully shovable; `≤ 0` ⇒ not at all.

```
half_width += clearance_compensation(cl_type, layer)  if compensation on
trace_shape = line_segment.to_polyline().offset_shapes(half_width)[0]   // must be exactly 1
fail(0) if shape empty, or not inside board bounding box
from_side  = CalcFromSide(line_segment, trace_shape, shove_to_the_left)   // constructor 3
entries    = ShapeTraceEntries(trace_shape, layer, net_nos, cl_type, from_side, board)
obstacles  = search_tree.overlapping_items_with_clearance(trace_shape, layer, {}, cl_type)
fail(0) if !entries.store_items(obstacles, is_pad_check=false, copper_sharing=true)
fail(0) if entries.trace_tails_in_shape()          // ONLY the static check tests this
fail(0) if entries.stack_depth() > 1
result = MAX_VALUE
for via in entries.shove_via_list, foreign net only:                        // :90-135
    if max_via_rec > 0:
        new_centers = MoveDrillItemAlgo.try_shove_via_points(shape, layer, via, cl_type,
                                                             extended=false, board)
        fail(0) if empty
        via_ok = MoveDrillItemAlgo.check(via, delta=new_centers[0]−center,
                                         max_rec, max_via_rec−1, [], board, null)
    if !via_ok:      // via can't move: how far along the segment can we still go?
        projection = scalar_product(start,end,via_center) / segment_length
        ok_len = projection − via_radius − half_width [− clearance(cl_type, via.cl, layer)]
        fail(0) if ok_len <= 0 else result = min(result, ok_len)
if trace_piece_count == 0: return result
fail(0) if max_rec <= 0
for each substitute trace piece:                                           // :144-202
    for its segments (swapped to opposite() when shove_to_the_left so lengths line up):
        only the segment whose direction equals the original line direction ("is_in_front"):
            rec = check(board, seg, shove_to_the_left, layer, piece.nets, piece.half_width,
                        piece.cl_class, max_rec−1, max_via_rec)
            if rec < MAX_VALUE:
                fail(0) if rec <= 0
                projection = min(proj(seg.start), proj(seg.end)) / segment_length
                ok_len = rec + projection − half_width − piece.half_width − clearance(...)
                fail(0) if ok_len <= 0 else result = min(result, ok_len)
            break   // only the first in-front segment per piece
return result
```

Key properties: the partial-length arithmetic converts a blocked recursive shove into "how far
along my own segment can I still advance", enabling the maze to still expand a *shortened* shove
(see §10). Every clearance subtraction uses the asymmetric `ClearanceMatrix.get_value(a,b,layer,
true)`.

### 5.2 Instance `check(trace_shape, from_side, dir, layer, net_nos, cl_type, max_rec, max_via_rec, max_spring_over_rec, time_limit) → boolean` (`:210-378`)

Full non-destructive verification used right before an insert (and by
`check_forced_trace_polyline`). Differences vs. static check:

- `p_dir` "is used internally to prevent the check from bouncing back" — for substitute pieces,
  only segments whose direction equals `p_dir` are recursed into (`:355-374`); top-level callers
  pass `null` (all segments checked).
- Time limit checked on entry (`:221`) ⇒ fail.
- Empty shape ⇒ **true** (warn); shape outside board ⇒ fail with
  `board.set_shove_failing_obstacle(board.get_outline())`.
- Removes `get_ignore_items_at_tie_pins(...)` from obstacles (`:238`): all contacts (on layer) of
  own-net pins overlapping the shape are ignored — allows shoving across tie pins (`:544-557`).
- `store_items` fail / `stack_depth() > 1` ⇒ fail, blame `entries.get_found_obstacle()`.
- Via shoving with *extended* candidates (`:278-318`): `try_shove_via_points(..., extended=true)`
  (up to 4 candidates, §6.3); a candidate other than the first is admissible only if
  `dist²(old_center, candidate) ≤ (via_max_halfwidth + shape_min_halfwidth)²`; each candidate is
  verified with `MoveDrillItemAlgo.check(via, delta, max_rec, max_via_rec−1, [], board, tl)`.
  No candidate works ⇒ fail. `max_via_rec ≤ 0` with a foreign via present ⇒ fail, blame via.
- `trace_piece_count == 0` ⇒ true; `max_rec ≤ 0` ⇒ fail.
- Per substitute piece, if `max_spring_over_rec > 0`: run `spring_over(piece.polyline, …)` first
  (§5.4); null ⇒ fail; if it changed the polyline, `--max_spring_over_rec` and mutate the piece.
- Recursion per segment via `CalcShapeAndFromSide(piece, i, orthogonal, in_shove_check=true)`
  (orthogonal ⇔ shove shape `instanceof IntBox`), `max_rec−1`, direction filtering as above.

### 5.3 `insert(trace_shape, from_side, layer, net_nos, cl_type, ignore_items, max_rec, max_via_rec, max_spring_over_rec) → boolean` (`:384-542`)

Destructive twin. "If the shove does not work, the database may be damaged. To prevent this, call
check first" (`:380-383`).

```
true if shape empty;  fail(outline) if outside board
fail if !MoveDrillItemAlgo.shove_vias(shape, from_side, layer, nets, cl_type, ignore,
                                      max_rec, max_via_rec, copper_sharing=true, board)  // §6.2
entries = ShapeTraceEntries(...); obstacles = overlaps − tie-pin contacts
store_items(obstacles, false, true)
fail(blame first via)  if shove_via_list still non-empty      // vias must be gone by now
fail(blame obstacle)   if !obstacles_shovable
true                    if trace_piece_count == 0
fail                    if max_rec <= 0
tails_exist_before = board.contains_trace_tails(obstacles, nets)
entries.cutout_traces(obstacles)                              // NOW the board is modified
for each substitute piece (skip zero-length):
    optional spring_over (as in check; failure ⇒ FAIL, board damaged)
    for each tile shape of the piece:
        CalcShapeAndFromSide(piece, i, orthogonal, in_shove_check=false)
        fail if !insert(sub.shape, sub.from_side, layer, piece.nets, piece.cl_class,
                        ignore_items, max_rec−1, max_via_rec, max_spring_over_rec)
    join_changed_area(each corner)                            // pull-tight region bookkeeping
    board.insert_item(piece);  piece.normalize(changed_area.get_area(layer))
    if !tails_exist_before:                                   // :529-539
        for both end corners: tail = board.get_trace_tail(corner, layer, piece.nets)
            if tail: remove tail's connection items (stop at via), combine_traces(net)
```

Note the recursion order: **space is cleared behind the substitute pieces before they are
inserted** (insert recursion precedes `insert_item`). Stack-depth is *not* re-checked here; the
preceding `check` is the guard.

### 5.4 `spring_over` (`:565-773`) and `spring_over_obstacles` (`:782-829`)

Not a shove but tightly coupled: wraps a polyline **around** an unshovable obstacle instead of
pushing it. Used (a) on substitute pieces inside check/insert (bounded by
`max_spring_over_recursion_depth`, default 5), and (b) on the *whole inserted polyline* at the top
of `insert_forced_trace_polyline` with its own constant `c_max_spring_over_recursion_depth = 20`
(`:789`), trying counterclockwise and clockwise (`reverse()`) and keeping the shorter result.

Obstacle scan per polyline segment shape: own-net items are obstacles only when they are pins not
in `p_contact_pins` ("to avoid acid traps", `:591`); `ConductionArea` per `get_is_obstacle()`;
`ViaObstacleArea`/`ComponentObstacleArea` never; foreign `PolylineTrace` only when shove-fixed
(and even then not when it has an own-net contact — tie-pin exit stubs, `:604-610`); other items
obstacle iff `!is_routable()`. Two distinct overlapping obstacles: keep the one whose bounding box
contains the other, else fail (`:624-637`).

Failure conditions: recursion exhausted, obstacle is `BoardOutline`, or obstacle is an unfixed
trace (those should be shoved, not sprung over) (`:649-654`). If the obstacle has a trace contact
on this layer and `p_over_connected_pins == false` ⇒ fail (don't wrap around connected pads,
`:656-665`). Obstacle shape must be a single tree shape (traces/areas) or the drill item's layer
shape.

Wrap construction (`:683-762`): enlarge obstacle shape by `half_width + 1` (+ clearance in two
half steps when compensation off: `enlarge(offset + cl/2)` then `enlarge(cl/2)`); snap to
bounding box (90°) or bounding octagon (45°); fail if polyline endpoints are inside the offset
shape; `entrance_points`/`cutout` the polyline against it; build substitute lines
`[entry_line, border lines CCW from first to last intersection side, exit_line]` (side_diff
disambiguation when both intersections are on the same side, `:727-741`); recombine with cutout
pieces; recurse with depth−1.

---

## 6. Via shoving: `MoveDrillItemAlgo` (v2 `DrillItemMover`)

### 6.1 `check(drill_item, vector, max_rec, max_via_rec, ignore_items, board, time_limit)` (`:22-91`)

Can this via/pin be translated by `vector`? Fail if: time limit, `is_shove_fixed()`, or any
normal contact is not a `Trace`/`ConductionArea` (`:38-44` — vias connected to other vias or pins
can't be moved). Adds itself to `ignore_items` (shared collection — deliberately accumulates
across the recursion so the cascade ignores every via already scheduled to move). For each layer
of the drill item: translate its tree shape, snap to bounding box (90°) / bounding octagon (else),
`from_side = CalcFromSide(drill_item.center, tile_shape)` (constructor 2 — shove direction away
from the old position), then `ForcedPadAlgo.check_forced_pad(..., p_check_only_front=true, ...)`.
Any layer `NOT_DRILLABLE` ⇒ false.

### 6.2 `insert(drill_item, vector, …, tidy_region, board)` (`:98-155`)

Same per-layer loop but `forced_pad(...)` (destructive), accumulates tidy region, joins changed
area, finally `drill_item.move_by(vector)`.

`shove_vias(obstacle_shape, from_side, layer, nets, cl_type, ignore, max_rec, max_via_rec,
copper_sharing, board)` (`:161-244`) — called at the top of `ShoveTraceAlgo.insert` and
`ForcedPadAlgo.forced_pad`: builds its own `ShapeTraceEntries`, and for each foreign via in the
shape (skipping when `max_via_rec ≤ 0` — note: returns **true**, leaving the via to be re-detected
and failed by the caller), tries candidate centers (distance-gated as in §5.2), `check`s then
`insert`s the move with `max_via_rec−1`. Only an `insert` failure returns false (database
damaged); "no candidate" just leaves the via in place.

### 6.3 `try_shove_via_points(obstacle_shape, layer, via, cl_class, extended_check, board)` (`:251-320`)

Candidate new centers for pushing a via out of a shape:

- `shove_distance` = `0.5 * via_shape.bbox.max_width() [+ clearance]` for 90° or IntOctagon
  shapes; else 0 `[+ 0.5*clearance]` ("synchronize with the check algorithm in
  ShapeSearchTree.overlapping_tree_entries_with_clearance", `:277-279`).
- **`shove_distance += 2`** — "empirical value for the tolerance in case of diagonal shoving"
  (`:284-285`).
- 90°: `bbox.offset(shove_distance).nearest_border_projections(center, try_count)`,
  `try_count = extended ? 2 : 1`.
- IntOctagon: `bounding_octagon().enlarge(...).nearest_border_projections(center, try_count)`,
  `try_count = extended ? 4 : 1`.
- General simplex: enlarge shape by shove_distance, via shape by `0.5*clearance` (compensation
  off), `nearest_relative_outside_locations(via_shape, try_count)` (`try_count = extended ? 4 :
  1`), returned as deltas applied to the center.

---

## 7. Pad shoving: `ForcedPadAlgo` (v2 `ForcedPadRouter`)

### 7.1 `check_forced_pad(pad_shape, from_side, layer, nets, cl_type, copper_sharing_allowed, ignore_items, max_rec, max_via_rec, check_only_front, time_limit) → CheckDrillResult` (`:212-335`)

`CheckDrillResult ∈ {DRILLABLE, DRILLABLE_WITH_ATTACH_SMD, NOT_DRILLABLE}` (`:490-494`).

Pipeline mirrors §5.2 but with `store_items(obstacles, is_pad_check=true, copper_sharing)`:
- pad outside board ⇒ NOT_DRILLABLE (blame outline).
- unshovable obstacle ⇒ NOT_DRILLABLE.
- every via in `shove_via_list` must be movable: `max_via_rec ≤ 0` ⇒ NOT_DRILLABLE;
  `try_shove_via_points(extended=false)` empty ⇒ NOT_DRILLABLE; `MoveDrillItemAlgo.check(via,
  delta_to_first_candidate, max_rec, max_via_rec−1, …)` fail ⇒ NOT_DRILLABLE. (Note: pad check
  is stricter than trace check — no partial-length fallback.)
- if copper sharing allowed and a `Pin` overlaps ⇒ result upgraded to
  `DRILLABLE_WITH_ATTACH_SMD` (via would land on an SMD pad).
- `trace_piece_count == 0` ⇒ result. `max_rec ≤ 0` or `stack_depth() > 1` ⇒ NOT_DRILLABLE.
- Per substitute piece segment: when `check_only_front` (used when moving drill items — the only
  direction of interest is away from the from-side, `:206-210`), skip segments not
  `in_front_of_pad(line, pad_shape, from_side.no, piece.half_width, with_sides=true)`; else check
  all. Recurse into `ShoveTraceAlgo.check(sub.shape, sub.from_side, segment_dir, …, max_rec−1,
  max_via_rec, spring_over=0, time_limit)`.

`in_front_of_pad` (`:46-203`): only implemented for `IntOctagon` pads with integer line points
(otherwise returns true = conservative "in front"). For each of the 8 from-sides there is a
hand-written predicate over the octagon's 8 parameters (`lx, ly, rx, uy, ulx, lrx, llx, urx`)
testing whether both segment endpoints are beyond the pad on the relevant side, with
`p_width` orthogonal margin and `diag_width = width * √2` diagonal margin; `p_with_sides` adds
the two adjacent flanks. (Bug preserved in both trees: case 0 uses `line_b.x + line_b.x` where
`line_b.x + line_b.y` is intended — `ForcedPadAlgo.java:69`; v2 `ForcedPadRouter.java` same.)

### 7.2 `forced_pad(...)` (`:342-461`) — destructive twin

Same skeleton as `ShoveTraceAlgo.insert`: `shove_vias` first, re-scan, fail if any via remains or
obstacles unshovable, `cutout_traces`, per substitute piece per segment
`ShoveTraceAlgo.insert(..., spring_over=0)`, insert piece, `normalize`, tail cleanup identical to
§5.3.

### 7.3 `calc_from_side(shape, shape_center, layer, offset, cl_class)` (`:467-488`)

For a *free-standing* pad (interactive via placement): for each border line of
`shape.offset(offset)`, build a thin probe shape from the center toward that side
(`calc_check_shape_for_from_side`, `:31-43`: 3-line polyline center→projection, offset 1) and
return the first side whose probe passes `board.check_trace_shape(probe, layer, {}, cl_class,
null)`; second pass with clearance class 0; else `NOT_CALCULATED`.

---

## 8. `ForcedViaAlgo` (v2 `ForcedViaInserter`)

- `check_layer(via_radius, cl_class, attach_smd_allowed, room_shape, location, layer, nets,
  max_rec, max_via_rec, board)` (`:23-81`): used by the autoroute drill machinery. Builds a
  `Circle(location, ceil(radius))`, snaps to bbox (90°) or bounding octagon; computes from-side
  via the private `calculate_from_side(location, tile_shape, room_shape.to_Simplex(),
  check_radius, is_90)` where `check_radius = via_radius + 0.5*clearance(cl,cl,layer) +
  min_trace_half_width` (`:43-46`): probes the 4 axis directions (then 4 diagonals for 45°,
  distance `p_dist/√2`) for a probe point still inside the room shape; from-side index `i` (90°)
  or `2i`/`2i+1` (octagon) with a border point on the via box (`:221-305`). No room ⇒
  NOT_DRILLABLE. Then `check_forced_pad(..., check_only_front=false, null)`.
- `check(via_info, location, nets, max_rec, max_via_rec, board)` (`:87-131`): per padstack layer
  with a shape: translate, snap, `calc_from_side` (§7.3, offset = `min_trace_half_width`),
  `check_forced_pad`; failure ⇒ `board.set_shove_failing_layer(i)`, false.
- `insert(via_info, location, nets, trace_cl_class, trace_pen_halfwidth[], max_rec, max_via_rec,
  board)` (`:140-219`): per layer `forced_pad` for the via pad shape **and** (when
  `trace_pen_halfwidth[i] > 0`) for a circle of the trace half-width around the location
  ("necessary in case start_trace_shape is bigger than tile_shape", `:194-196`) — makes room to
  start a fat trace at the via. Finally `board.insert_via(padstack, …, UNFIXED, attach_allowed)`.

---

## 9. Board-level entry points (`RoutingBoard`, v19)

State for failure reporting: `transient Item shove_failing_obstacle; transient int
shove_failing_layer = -1` (`:42-43`), getters `:1386-1399`, cleared at the start of each public
forced operation. The interactive GUI uses these to flash the blocking item; the autorouter
ignores them (it relies on boolean/length results).

- **`check_trace_segment(line_segment, layer, nets, half_width, cl_class,
  only_not_shovable_obstacles)`** (`:270-340`): pure geometric probe, no `ShapeTraceEntries`.
  Builds one offset shape for the segment; for each overlapping tree entry (skipping routable
  non-shove-fixed items when `only_not_shovable_obstacles`), intersects obstacle shape with the
  clearance-expanded segment shape and computes
  `ok_length = max(0, projection_of_nearest_obstacle_point − (half_width + clearance) − 1)`;
  returns min over obstacles, `Integer.MAX_VALUE` if free. Used by the maze shover (§10) as the
  hard-obstacle length cap, and by neckdown.
- **`check_forced_trace_polyline(polyline, half_width, layer, nets, cl_class, max_rec,
  max_via_rec, max_spring_over_rec)`** (`:633-674`): per offset shape (bbox in 90° mode),
  `CalcFromSide(polyline, i+1, shape)` then instance `ShoveTraceAlgo.check(..., dir=null, …,
  time_limit=null)`. Used by the maze when connecting to targets through *thin* rooms
  (`MazeSearchAlgo.java:555-578`).
- **`insert_forced_trace_polyline(polyline, half_width, layer, nets, cl_class, max_rec,
  max_via_rec, max_spring_over_rec, tidy_width, pull_tight_accuracy, with_check, time_limit) →
  Point`** (`:682-982`). The main insertion driver. Steps:
  1. Non-IntPoint endpoints ⇒ warn, return `from_corner`. `start_marking_changed_area()`.
  2. Pick an own-net trace ending at `from_corner` with equal nets/half-width/clearance class ⇒
     `picked_trace` for dog-ear-free from-side computation (`:706-727`).
  3. `spring_over_obstacles(polyline, compensated_half_width, …)` on the whole polyline; null ⇒
     return `from_corner` (nothing inserted, board clean).
  4. `combined_polyline = new_polyline.combine(picked_trace.polyline)` (or plain);
     compute the trailing `trace_shapes` (only the shapes of the new part).
  5. For each shape (bbox in 90° mode): `CalcFromSide(combined_polyline, corner_index, shape)`;
     if `with_check`: instance `check(...)`; failure ⇒ remember `last_shape_no = i`, break.
     Then `insert(...)`; **insert failure ⇒ return `null`** (database damaged ⇒ caller undo).
  6. Partial-failure resampling (`:814-886`): if check stopped at `last_shape_no`, shorten the
     failing segment: `sample_width = 2 * min_trace_half_width`; if the failing segment is longer
     than `100 * sample_width` ⇒ give up (return `from_corner`, "too many cycles to sample");
     else `new_polyline.shorten(...)` by `sample_width`, recombine, re-`check` and `insert` the
     final shape; check fail ⇒ `from_corner`, insert fail ⇒ null. The returned `new_corner` is
     the shortened endpoint.
  7. Insert the own trace (`insert_trace_without_cleaning`, UNFIXED), `combine()`, `normalize`
     (cycle removal), `split_traces_at_keep_point`, re-pick the trace at `new_corner`,
     `pull_tight` when `tidy_width > 0`. Returns `new_corner` (== `to_corner` on full success).
- **`insert_forced_trace_segment(from, to, …)`** (`:586-627`): 2-corner wrapper, maps the result
  point back to the input objects.
- **`forced_via(via_info, location, …, tidy_width, accuracy, time_limit)`** (`:532-577`):
  `ForcedViaAlgo.insert` + changed-area pull-tight.
- **`move_drill_item(drill_item, vector, …)`** (`:412-461`): unfixes SHOVE_FIXED contacts first
  (`:421-427`), `MoveDrillItemAlgo.insert`, then `opt_changed_area` with a tidy region.

---

## 10. Maze-time shove: `MazeShoveTraceAlgo.check_shove_trace_line` (v2 `MazeTraceShover.checkShoveTraceLine`)

### 10.1 When it triggers (`MazeSearchAlgo.expand_to_room_doors`, `:408-450`)

Preconditions, in order:
- `next_room` is an `ObstacleExpansionRoom` (one tile shape of a board item; index in item =
  `ObstacleExpansionRoom.index_in_item`).
- ripup check ran first (if `ctrl.ripup_allowed`): `ripup_costs =
  check_ripup(...)` (§10.4); `ripup_costs != ALREADY_RIPPED_COSTS` (== 1, `:41`).
- `next_room_is_thick` — the obstacle trace's half-width ≥ our compensated half-width on that
  layer (`room_shape_is_thick`, `:1360-1385`); a *thin* obstacle can't hide our trace behind it.
- entering door is not "small" and `ctrl.max_shove_trace_recursion_depth > 0` and the obstacle
  item is a `PolylineTrace`.

Then `shove_trace_room(list_element, obstacle_room)` (`:1523-1597`):
- Only outermost door sections shove: `section_no_of_door` must be 0 or last
  (`:1525-1531`) — "inner sections of a door are currently not shoved".
- Try **shove right** (`p_shove_to_the_left=false`) unless this element already carries
  `Adjustment.RIGHT`, and **shove left** unless `Adjustment.LEFT` — i.e. an element that already
  passed through a shoved link door keeps shoving the same way and cannot flip sides.
- Each successful direction yields `DoorSection`s to expand:
  `Adjustment.LEFT/RIGHT` is attached only when the reached door is a 2-dim **link door**
  (continuation along the same obstacle trace to its next tile shape); side doors get
  `Adjustment.NONE`.
- If **neither** direction returned true and `ripup_costs > 0`, the ripup occupation is *delayed*:
  a copy of the list element with `expansion_value += ripup_costs` and `already_checked=true` is
  re-queued (`:424-441`) so other door sections may try to shove the room before it gets ripped.

`Adjustment` lives per door section (`MazeSearchElement.java:28-32`: `NONE, RIGHT, LEFT`) and is
also consulted by `check_ripup` (`room_was_shoved`, `:1406-1417`) to forbid ripping a fork the
shove created.

### 10.2 The check itself (`MazeShoveTraceAlgo.java:27-315`)

Returns **false ⇒ "trying to shove from another door section may be more successful"** (caller
treats any false as "this direction found nothing", not an error). Adds reachable door sections
to `p_to_door_list` as a side effect.

Gate conditions:
- from-door must be an `ExpansionDoor`; obstacle item a `PolylineTrace`.
- **only traces with the same half-width and the same clearance class can be shoved**
  (`:44-47`) — `obstacle.half_width == ctrl.trace_half_width[layer] &&
  obstacle.cl_class == ctrl.trace_clearance_class_no`. (Because the maze-time model swaps our
  trace into the obstacle's corridor; different widths would invalidate the door geometry.)
- `from_door_shape.max_width() ≥ 2 * compensated_trace_half_width` (door big enough).

Building the **shove line segment** (the segment of the obstacle trace we want to push sideways):

- From a 2-dim **link door** (`:66-90`): the other room must be an `ObstacleExpansionRoom` whose
  item shares a matching endpoint with the obstacle trace (`end_points_matching`, `:321-346`:
  same item, or same net with coinciding corner / drill center — prevents shoving across a
  T-junction that isn't a real continuation). Segment = trace polyline segment
  `trace_corner_no+1`, oriented away from the door (`opposite()` if the door center is nearer the
  far corner). Degenerate (<1 unit²) ⇒ false.
- From a 1-dim door (`:90-171`): let `polar_line_segment` be the door segment seen from the
  neighbor room's center of gravity. Only the **outermost door section on the matching side**
  may initiate: `section_no_of_door` must be the last (or 0, depending on door orientation
  `door_line_swapped` vs `p_shove_to_the_left`), and the section's `shape_entry` endpoint must be
  within `check_dist = compensated_trace_half_width + 5` of the corresponding door corner
  (`:103-127`). Then the shove segment starts at a **closing line** perpendicular
  (`turn_45_degree(2)`) to the trace line through the door segment shrunk by
  `compensated_trace_half_width` (`shrink_segment`), and continues along the obstacle polyline in
  the direction determined by (side of door vs trace line) × (shove left/right): four cases
  select `{closing_line, arr[c+1], arr[c+2]}` or the opposite lines backwards (`:134-170`).

Length capping and feasibility (`:172-215`):

```
shove_width = board.check_trace_segment(shove_line_segment, layer, [net], half_width,
                                        cl_class, only_not_shovable_obstacles=true)
if shove_width < MAX: shove_width -= 1; if ≤0 return true (nothing reachable, but done);
                      shove_line_segment = change_length_approx(shove_width); segment_shortened
if segment is not a point (length² ≥ 0.1):
    shove_width = ShoveTraceAlgo.check(board, shove_line_segment, p_shove_to_the_left, layer,
                                       [net], half_width, cl_class,
                                       ctrl.max_shove_trace_recursion_depth,   // 20
                                       ctrl.max_shove_via_recursion_depth)     // 5
    if shove_width <= 0 return true
if segment_shortened: shove_width = min(shove_width, |from−to|)
```

Door harvesting (`:217-313`): iterate the obstacle room's doors (skipping the from-door and any
door connecting two *different* obstacle items — "topological problems at a trace fork",
`:238-246`):
- 2-dim link doors: added (section 0) only when `shove_width == MAX` (full success) and the door
  shape contains `to_corner` — the shove can continue into the next tile of the same trace.
- 1-dim doors: must lie entirely on the shove side of the trace line (`side_of(corner, 0)` both
  endpoints strictly LEFT resp. RIGHT); must be located toward `to_corner`
  (`to_corner.distance² < from_door_compare_distance`); door reachable iff
  `projection_of_nearest_corner_onto_shove_line.distance(from_corner) +
  compensated_trace_half_width ≤ shove_width`; then each door *section* whose nearest corner
  projects within `shove_width` is added with its section line.

So the maze "walks through" the obstacle trace sideways: the shove is virtual; only if this path
wins does `InsertFoundConnectionAlgo` later make it real, and then the *generic* trace shover
(§5) actually pushes the obstacle — possibly slightly differently than the maze predicted, which
is why insertion can still fail (§11).

### 10.3 Thin-room target expansion also shoves

`expand_to_target_doors` (`:527-589`): when the room is *thin*, the connection line from the
entry middle to the target shape is validated with
`board.check_forced_trace_polyline(check_polyline, …, max_shove_trace_recursion_depth,
max_shove_via_recursion_depth, max_spring_over_recursion_depth)` (`:563-578`) — i.e., a full
recursive shove check is run inside the maze for target hookups.

### 10.4 Ripup interplay (`check_ripup`, `:1392-1516`)

Not shove itself, but the alternative the maze weighs against it:
`cost_factor = trace.half_width` (traces) or `max contact half_width * 0.5 * max(contact_count−1,
0)` (vias, unless fanout); `fanout_via_cost_factor = clamp≥1((half_width/length)² * 20000)` for
traces guarding fanout vias (`:117-148`); `ripup_cost = ctrl.ripup_costs * cost_factor / detour *
fanout_factor`, where `detour` comes from the item's `Connection` and is randomized by
`0.5 + rnd²` when `ripup_pass_no ≥ 4 && pass % 3 != 0` (`:1482-1488`); clamped to
`[1, Integer.MAX_VALUE/100]`. `ctrl.ripup_costs = start_ripup_costs * pass_no`
(`BatchAutorouter.java:516,623`), default start 100 (`interactive/AutorouteSettings.java:40`),
control default 1000 before init (`AutorouteControl.java:126`). `ALREADY_RIPPED_COSTS = 1`.

---

## 11. Insertion-time integration: `InsertFoundConnectionAlgo`

`get_instance(connection, board, ctrl)` (`:44-106`): for each `ResultItem` (a per-layer corner
array from `LocateFoundConnectionAlgo`): `insert_via(corners[0], curr_layer, item.layer)` then
`insert_trace(item)`; any failure ⇒ return null (caller = `AutorouteEngine` reacts by undoing the
whole connection attempt). Finally the trailing via, `connect_to_trace` for trace targets, and
`board.normalize_traces(net)`.

`insert_trace` (`:113-295`) — the incremental advance loop:

```
from_corner_no = 0
for i in 1 .. corners.length-1:
    insert_polyline = Polyline(corners[from_corner_no ..= i])
    ok_point = board.insert_forced_trace_polyline(insert_polyline, half_width, layer, [net],
                   cl_class, max_shove_trace_rec(20), max_shove_via_rec(5),
                   max_spring_over_rec(5), tidy=MAX, pull_tight_accuracy(500),
                   with_check=true, time_limit=null)
    if ok_point == last_corner (or a neckdown to it succeeded):  from_corner_no = i   // ADVANCE
    elif ok_point == first_corner && i != last:
        // spring-over may have failed because check ran with clearance compensation but
        // insertion without; retry with more distant corners:
        if from_corner_no > 0 && len(curr_corner_arr) < 3: --from_corner_no          // RETREAT
    else: FAIL (result=false, break)                     // includes ok_point == null (db damaged)
cleanup: remove trace tails at every intermediate corner (get_trace_tail + remove_item)
```

Around it: `pin_edge_to_turn_dist` is temporarily set to −1 (`:120-124`, restored `:289`) so
pin-exit correction doesn't fire per segment. Neckdown (`ctrl.with_neckdown`): when a partial
insert ends near a start/end pin, `insert_neckdown`/`try_neck_down` (`:297-452`) tries the pin's
`get_trace_neckdown_halfwidth(layer)`; gates: pin on layer, distance to corner
`< 2*(pad_halfwidth + clearance)`, neck half-width < normal; probes with `check_trace_segment`,
shortens by `TOLERANCE = 2`, inserts up to three staircase segments (via
`LocateFoundConnectionAlgo.calculate_additional_corner`) at full width then the final segment at
neck width, each through `insert_forced_trace_segment`.

`insert_via` (`:461-513`): iterate `ctrl.via_rule`'s vias whose padstack spans
`[from_layer, to_layer]`; first one passing `ForcedViaAlgo.check(via_info, location, [net], 20,
5, board)` is chosen; then `ForcedViaAlgo.insert(...)` with `ctrl.trace_clearance_class_no` and
`ctrl.trace_half_width[]`. No candidate or insert failure ⇒ overall failure.

---

## 12. Knobs, constants, and limits (defaults)

| Knob | Value | Where |
|---|---|---|
| `max_shove_trace_recursion_depth` | **20** | `AutorouteControl.java:112` (v2 `:171`) |
| `max_shove_via_recursion_depth` | **5** | `AutorouteControl.java:113` |
| `max_spring_over_recursion_depth` | **5** | `AutorouteControl.java:114` |
| top-level spring-over depth (`c_max_spring_over_recursion_depth`) | **20** | `ShoveTraceAlgo.java:789` |
| `ShapeTraceEntries.c_offset_add` | **1** | `ShapeTraceEntries.java:15` |
| stack depth allowed in any single shove shape | **≤ 1** | §3.5 callers |
| via shove candidate counts | 1 / extended: 2 (90°), 4 (oct/simplex) | `MoveDrillItemAlgo.java:290-309` |
| via shove diagonal tolerance | **+2** | `MoveDrillItemAlgo.java:285` |
| via candidate distance gate | `(via_max_hw + shape_min_hw)²` | `ShoveTraceAlgo.java:291-299` |
| maze section proximity | `compensated_half_width + 5` (squared) | `MazeShoveTraceAlgo.java:107` |
| maze shove shortening | `−1`, point iff len² < 0.1 | `MazeShoveTraceAlgo.java:187,197` |
| partial-insert sample width | `2 * min_trace_half_width`, give up beyond `100×` | `RoutingBoard.java:821-827` |
| neckdown tolerance | **2** | `InsertFoundConnectionAlgo.java:341` |
| `check_trace_segment` safety | `−1` after clearance | `RoutingBoard.java:329` |
| `pull_tight_accuracy` | 500 | `AutorouteControl.java:111` |
| `ripup_costs` | `start(100) × pass_no` (ctor default 1000) | §10.4 |
| `ALREADY_RIPPED_COSTS` | 1 | `MazeSearchAlgo.java:41` |
| `FANOUT_COST_CONST` | 20000 | `MazeSearchAlgo.java:118` |
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE/100` | `MazeSearchAlgo.java:1493` |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | 2 | `AutorouteEngine.java:30` |

Work bounding: recursion depths are the *only* bound on shove cascades besides `TimeLimit`
(checked in instance `check` and `MoveDrillItemAlgo.check`; **not** in the insert paths —
comment `MoveDrillItemAlgo.java:214`: "No time limit here because the item database is already
changed"). `max_rec` decrements only when recursing through substitute traces; via cascades
decrement `max_via_rec` independently; spring-over depth decrements only when it changes a
polyline.

## 12b. Failure detection & handling summary

- `check` paths: boolean/length results + `board.set_shove_failing_obstacle(item)` /
  `set_shove_failing_layer(layer)` for UI blame. No board mutation ever.
- `insert` paths: `false`/`null` ⇒ **board possibly damaged**; every caller either aborts the
  connection (autoroute engine undoes via its own snapshot mechanism) or returns null upward
  (`insert_forced_trace_polyline` step 5).
- The maze-time check is optimistic (compensated clearances, same-width restriction, stack ≤ 1);
  insertion re-checks everything for real and has two mitigation layers: corner-window
  retreat/advance in `InsertFoundConnectionAlgo.insert_trace`, and segment resampling inside
  `insert_forced_trace_polyline`.

---

## 13. v2 (2.3.0) semantic deltas

The v2 tree is what release 2.3.0 builds (there is no build file selecting `src_v19`; the v19
tree is kept as a reference snapshot; the `integrations/` dir has no Java sources). Verified
callers: `MazeSearchEngine.java:1130-1195` calls `MazeTraceShover.checkShoveTraceLine`;
`board/facade/RoutingBoard.java:423,521` news up `board.optimize.TraceShover`. Semantic deltas
found in the shove area:

### 13.1 `ShapeEntrySide` (was `CalcFromSide`) — new fallback in constructor 3

v2 `ShapeEntrySide.java:108-145`: when the "front side" scan fails (v19 warned and produced
`no = -1`, which downstream makes `ShapeTraceEntries` recompute the side from own-net entries),
v2 instead finds the nearest border side by perpendicular projection (accepting a projection
that lies within the side's corner box with **0.01** tolerance,
`projection.isContainedInBox(sideStart, sideEnd, 0.01)`), applies the same ±2 rotation, and sets
`borderIntersection` to the middle of the chosen side. Also `borderIntersection` is now a
mutable public field. Rust port: implement the v2 fallback (it removes a warning-path
nondeterminism), but keep in mind v19's `-1` → lazy `search_from_side()` behavior as the
reference for equivalence tests.

### 13.2 `ForcedViaInserter` (was `ForcedViaAlgo`) — hole clearance + stronger check

v2 `ForcedViaInserter.java`:
- `holeCheckShape(padstack, location, board)` (`:363-375`): when `rules.getHoleClearance() > 0`
  and the padstack has a drill radius, a `Circle(center, ceil(drillRadius + holeClearance + 10))`
  is used (with clearance class **0**) (a) as the pad shape on copper-less layers of the padstack
  (`:148-154`) and (b) as an *additional* forced-pad check/insert on layers that do have copper
  (`:183-208`, `:310-330`) — enforcing hole-to-copper spacing that the copper clearance check
  might under-constrain.
- `check(...)` gained `tracePenHalfwidthArr` and `traceClearanceClassIndex` parameters and now
  also *checks* the start-trace circle shape (`:210-237`); in v19 that shape was only handled in
  `insert`, so v19 could pass `check` yet fail `insert` when the trace pen was wider than the via
  pad.
- `insert` mirrors both additions (`:263-347`).

### 13.3 Structural moves (no semantics)

`MazeSearchAlgo` split into `MazeSearchEngine` (expansion, `shoveTraceRoom` at `:1130`) and
`MazeRipupResolver` (`checkRipup` at `:72`); `RoutingBoard` split into facade classes;
`ShapeTraceEntries` moved to `board/searchtree` with `shoveViaList` public; knob defaults
unchanged (`v2 AutorouteControl.java:171-186`).

---

## 14. Verification notes

- Whitespace/identifier-normalized diffs of v19 vs v2 for `ShoveTraceAlgo/TraceShover`,
  `ForcedPadAlgo/ForcedPadRouter`, `MoveDrillItemAlgo/DrillItemMover`,
  `CalcShapeAndFromSide/ShapeAndEntrySide`, `ShapeTraceEntries`, `MazeShoveTraceAlgo/
  MazeTraceShover`: identical numeric-literal multisets and matching structure ⇒ pure renames.
- `CalcFromSide/ShapeEntrySide` and `ForcedViaAlgo/ForcedViaInserter` differ as documented above.
- The v19 tree studied here carries extra `FRLogger.trace` instrumentation
  (`[shove_check_obstacles]`, `compare_trace_*` blocks in `ShoveTraceAlgo.java:246-268,432-454`,
  `InsertFoundConnectionAlgo`, `RoutingBoard`) not present in upstream 1.9; it is diagnostic
  only and must not be treated as algorithm.

---

## 15. Rust design implications

### Keep semantically (bit-for-bit behavior worth preserving)

- The **entry-point / stack-level calculus** (§3.3-3.6) including: CCW sorted insertion with
  scalar-product tie-break, resort-around-from-side with `edge_no += edge_count` wrap, the
  run-collapse rule, bracket-nesting stack levels with the `stack_depth ≤ 1` gate, and
  outermost-first `pop_piece`. This is the semantics of "can this trace be pushed"; any deviation
  changes routing results globally.
- The **two-step offset rule** (`offset(half_width)` then `offset(clearance + 1)`) vs one-step
  compensated offset, and the `c_offset_add = 1` epsilon. The comments warn these are load-bearing
  (`ShapeTraceEntries.java:64,317,325`; the "check with compensation / insert without" mismatch is
  explicitly compensated for in `InsertFoundConnectionAlgo.java:199-216`).
- The **check/insert split** with "insert may damage the board" contract, and the exact partial
  failure protocol of `insert_forced_trace_polyline` (return `from_corner` vs `new_corner` vs
  `null`) — `InsertFoundConnectionAlgo` decodes these three cases distinctly.
- Recursion budgets and their exact decrement points (trace depth on substitute recursion only;
  via depth on via cascades only; spring-over only when the polyline changed), plus all constants
  in §12 — they define the router's effort/quality envelope.
- The maze-time restrictions: same half-width + same clearance class only, outermost door
  sections only, no side-flip once `Adjustment` is set, fork exclusion via `end_points_matching`
  and the two-obstacle-rooms door skip. These prune the search space deliberately.
- Direction filtering (`is_in_front` / `p_dir` anti-bounce-back) in both static and instance
  check — removing it causes infinite mutual shoving.

### Restructure for performance

- **Arena the shove transaction.** One shove cascade allocates, per recursion level: a
  `ShapeTraceEntries`, a `LinkedList<Via>`, an obstacle `Collection`, an `EntryPoint` node per
  intersection, offset `TileShape`s, and a fresh `PolylineTrace` per substitute piece — all
  garbage on failure. In Rust: a per-cascade bump arena (`bumpalo`) holding entry points,
  candidate polylines, and offset shapes; `EntryPoint` as a `Vec<EntryPoint>` with `u32 next`
  indices (or plain sorted `Vec` + ranges — the linked list exists only to splice, which a
  retain/partition over a small Vec does cheaper and cache-friendly). Substitute traces should be
  *plans* (net, half_width, cl_class, line array) not board items; materialize only on commit.
- **Make insert transactional instead of "may damage the board".** The Java double-walk
  (full `check` then `insert`, ~2× the geometry work, and still able to fail) exists because
  mutation is eager. With a journal/copy-on-write board (or by executing the cascade on the plan
  arena and committing atomically), the separate check pass for the common path can be dropped
  and failures become free rollbacks. Keep an equivalence mode that mimics check-then-insert for
  regression testing.
- **Batch the search-tree churn.** `cutout_trace` + piece insertion currently mutate the shape
  search tree item-by-item (with `fast_cutout_trace` as a special case). A Rust rewrite should
  make "replace trace T by pieces P1..Pn reusing leaf entries" the primitive, not the
  optimization.
- **Parallelism boundary**: a shove cascade is inherently sequential (each level depends on the
  cutout of the previous), but cascades for different nets/regions are independent given board
  partitioning. The clean seam is at `AutorouteEngine` granularity (per connection), with the
  maze-time static `check` being read-only and thus trivially parallel *if* the search tree is
  concurrent-read. Do not try to parallelize inside a cascade.
- **Precompute per (cl_type, layer, obstacle_class) offset parameters.** The clearance matrix +
  compensation lookups appear inside every store_trace/next_piece/cutout; hoist to a small struct
  computed once per `ShapeTraceEntries`.

### Java-isms observed (file:line evidence)

- Per-call object churn: `new ShapeTraceEntries` + `new LinkedList` per recursion level
  (`ShoveTraceAlgo.java:233-238,415-420`; `ForcedPadAlgo.java:229-232`); fresh
  `Collection<Item> ignore_items = new LinkedList<>()` per via candidate
  (`ShoveTraceAlgo.java:301`, `MoveDrillItemAlgo.java:209`); `new ForcedPadAlgo(board)` /
  `new ShoveTraceAlgo(board)` constructed per operation (`MoveDrillItemAlgo.java:52,113`,
  `RoutingBoard.java:649,731`); `TreeSet<Item>` built per check for tie-pin ignores
  (`ShoveTraceAlgo.java:547`).
- Exact rational geometry on hot paths: `Polyline`/`Line` are `BigInteger`-backed rationals
  (`geometry.planar`); every `offset_shapes`, `entrance_points`, `cutout`, `intersection`,
  `to_Simplex` in the loops above hits them. `next_substitute_trace_piece` builds a `Polyline`
  from raw `Line[]` per piece (`ShapeTraceEntries.java:238-254`); `CalcShapeAndFromSide` does
  `to_Simplex()` + two `TileShape.intersection`s per substitute segment
  (`CalcShapeAndFromSide.java:33-51`). Rust: i64 fixed-point predicates with exact (i128/rug)
  fallback only on filter failure; cache tile shapes on the piece plan.
- Clone/copy-heavy: `Arrays.copyOfRange(p_trace.corners, from, i+1)` + new `Polyline` per
  advance-loop iteration (`InsertFoundConnectionAlgo.java:153-154`);
  `combined_polyline.offset_shapes(...)` recomputed wholesale after every resample
  (`RoutingBoard.java:750-752,850`); `obstacles.removeAll(...)` linear scans
  (`ShoveTraceAlgo.java:238,420`).
- Recursion via the call stack with depth 20 × per-level allocations; Rust can keep recursion
  (bounded) but should pass a context struct (`&mut ShoveCtx`) instead of 9-10 positional
  parameters repeated at every site (`ShoveTraceAlgo.java:210-220,384-393` — the parameter lists
  are a defect magnet; the v19→v2 rename churn shows how fragile they are).
- Diagnostic string building unconditionally guarded only by call (`StringBuilder` obstacle dumps
  in `ShoveTraceAlgo.java:246-268,432-454`) — in Rust, gate behind `tracing` levels or feature
  flags so release builds pay nothing.
- Latent bug to *not* copy blindly: `in_front_of_pad` case 0 `line_b.x + line_b.x` typo
  (`ForcedPadAlgo.java:69`, still present in v2) — decide consciously (fix + regression-test
  routing diffs, or preserve for bit-compat).

---

## Release-jar conformance (2.3.0 @2d4de019)

Audited against `/tmp/fr-java-release/src/main/java/app/freerouting` (the true source of the
shipped `freerouting-2.3.0.jar`, `Build-Revision 2d4de019aa89e9fa3dc1dc44e09bf509760cafc1`,
`Build-Date 2026-08-07`), cross-checked against the jar's bytecode with `javap -p -c`.
All line references in this section are **release-tree** unless marked `v19:`.

**Headline: the release ships the v19 class names, not the v2 ones.** `jar tf` over
`freerouting-2.3.0.jar` (2002 `app/freerouting/*` entries) contains
`board/ShoveTraceAlgo.class`, `board/ShapeTraceEntries.class`, `board/CalcFromSide.class`,
`board/CalcShapeAndFromSide.class`, `board/ForcedPadAlgo.class`, `board/ForcedViaAlgo.class`,
`board/MoveDrillItemAlgo.class`, `autoroute/MazeShoveTraceAlgo.class`,
`autoroute/MazeSearchAlgo.class` (+ `MazeSearchAlgo$ShoveResult`, `MazeShoveTraceAlgo$DoorSection`),
`autoroute/InsertFoundConnectionAlgo.class`. It contains **zero** of
`TraceShover`, `ShapeEntrySide`, `ShapeAndEntrySide`, `ForcedPadRouter`, `ForcedViaInserter`,
`DrillItemMover`, `MazeTraceShover`, `MazeSearchEngine`, `MazeRipupResolver`,
`RoutingBoardSearchFacade`, and none of the packages `board/optimize`, `board/searchtree`,
`board/facade`, `board/actions`, `board/model`, `autoroute/maze`, `autoroute/pipeline`.
⇒ **The whole "v2 file map" at the top of this document (§File map, §13.3) describes a tree that
2.3.0 does not ship.** The porting reference is the flat `board/` + `autoroute/` packages.
Confusingly, the *semantic* v2 deltas of §13.1/§13.2 **are** in the release — they were
back/forward-ported into the v19-named classes. See table 3 below.

### 1. What the release actually executes

#### 1.1 Headless (CLI / API server) — always `BatchAutorouter`

```
Freerouting.main
  → management/RoutingJobSchedulerActionThread.thread_action()
      :106-108   // comment: "Always use standard BatchAutorouter"
                 router = new BatchAutorouter(job);          // no V19 branch here
      :118       batchRouter.runBatchLoop();
  → autoroute/BatchAutorouter.runBatchLoop()                 :757
      :824       BatchFanout.fanout_board(...)               // optional fanout pre-pass
      :948       continueAutorouting = autoroute_pass(currentPass);
  → BatchAutorouter.autoroute_pass(int)                      :431
      :517       autoroute_item(curr_item, net_no, ripped_item_list, ripup_costs, pass_no)
  → BatchAutorouter.autoroute_item(...)                      :1257
      :1280      autoroute_control.ripup_costs = this.start_ripup_costs * p_ripup_pass_no;
      :1311-1313 TimeLimit = 100000 * 2^(pass-1) ms, capped at Integer.MAX_VALUE
  → board.init_autoroute(...) → autoroute/AutorouteEngine.autoroute_connection()  :127
      :140/:155  MazeSearchAlgo.get_instance / find_connection      ← maze-time shove (§10)
      :175       LocateFoundConnectionAlgo.get_instance
      :241       InsertFoundConnectionAlgo.get_instance             ← insert-time shove (§11)
```

`RoutingJobSchedulerActionThread` at `:103-105` logs "Unknown router algorithm … using default"
for any algorithm value and then constructs `BatchAutorouter` unconditionally. There is **no
headless path to `BatchAutorouterV19`.** A fanout-only headless mode (`:163-175`) also uses
`new BatchAutorouter(job)` with `maxPasses = 0`.

#### 1.2 GUI — `BatchAutorouter` vs `BatchAutorouterV19` selection

`interactive/AutorouterAndRouteOptimizerThread` ctor `:205-218`:

```java
String algorithm = routingJob.routerSettings.algorithm;
if (RouterSettings.ALGORITHM_V19.equals(algorithm))   // "freerouting-router-v19"
    this.batchAutorouter = new BatchAutorouterV19(routingJob);
else {                                                 // unknown → warn + reset to default
    if (!RouterSettings.ALGORITHM_CURRENT.equals(algorithm)) { ...logWarning...; algorithm = ALGORITHM_CURRENT; }
    this.batchAutorouter = new BatchAutorouter(routingJob);
}
```

(`settings/RouterSettings.java:14-15` — `ALGORITHM_CURRENT = "freerouting-router"`,
`ALGORITHM_V19 = "freerouting-router-v19"`.) `thread_action()` `:478` dispatches via
`instanceof` at `:513-526`. **Both classes are live**, but they differ only in the pass/ripup
driver loop; the shove subsystem is shared and identical:
`BatchAutorouterV19.java:402` and `BatchAutorouter.java:1280` set
`autoroute_control.ripup_costs = start_ripup_costs * ripup_pass_no` with the same formula, and
both then reach the same `AutorouteEngine.autoroute_connection`. **Nothing in §1-§11 of this
document is affected by the V19/current selection.**

#### 1.3 Dead code in the shove neighbourhood (do not port)

- `BatchAutorouter.autoroute_pass_multi_thread(int)` `:315` is `private` and **has no caller**
  anywhere in the release tree (verified by exhaustive grep). Its worker
  `autoroute/BatchAutorouterThread.java` (referenced only from `:327,:343,:368,:398` inside that
  dead method) is therefore also dead. **The 2.3.0 autoroute/shove path is strictly
  single-threaded**, which is the empirical backing for §15's "do not parallelize inside a
  cascade" — upstream tried board-cloning parallelism at pass granularity and shipped it disabled.
- `MazeSearchAlgo$ShoveResult` (`autoroute/MazeSearchAlgo.java:1651-1680`) still ships with the
  same dead javadoc referencing non-existent `MazeShoveViaAlgo` / `MazeShoveThinRoomAlgo`.
  §File-map note confirmed verbatim in the release.

#### 1.4 Debug scaffolding **shipped in the release jar** (must not be ported)

This is new information; the spec (§14) attributes instrumentation only to the local v19 snapshot.
The release is *more* instrumented, including hardcoded board-specific gates:

| site | gate | release file:line |
|---|---|---|
| 5× `compare_trace_insert_forced_*` / obstacle dumps | `p_net_no_arr[0] == 94` (literal net number!) | `board/RoutingBoard.java:660,726,742,801,1088` |
| `compare_trace_shove_shape` / `compare_trace_insert_forced_sub` id-generator deltas | any net | `board/RoutingBoard.java:698,708,1155` |
| `FANOUT_DIAG` | `ctrl.is_fanout && ctrl.fanout_start_pin_name.startsWith("U27-")` | `autoroute/InsertFoundConnectionAlgo.java:490-504` |
| `[shove_check_obstacles]` StringBuilder dumps | unconditional call | `board/ShoveTraceAlgo.java:193,212,346` |
| `CHECK_RIPUP` full-state dump incl. per-connection item-id StringBuilder | unconditional call | `autoroute/MazeSearchAlgo.java:1424-1441,1456-1474` |
| `compare_trace_insert_segment*` ADVANCE / RETREAT / FAIL dumps | `if (true)` | `autoroute/InsertFoundConnectionAlgo.java:159,199,229` |
| `compare_trace_stub_cleanup` (echoes `FRLogger.isTraceEnabled()` into its own message) | unconditional | `autoroute/InsertFoundConnectionAlgo.java:272-281` |

`bipush 94` is present 5× in the shipped `RoutingBoard.class` bytecode. `fanout_start_pin_name`
*is* assigned (`board/RoutingBoard.java:1038` = `component.name + "-" + pin.name()`), so the
`U27-` gate is live on any board with a component named `U27`. The `CHECK_RIPUP` /
`compare_trace_*` paths build strings **before** any level check — this is a real per-call
allocation cost in the shipped router, reinforcing §15's "gate behind tracing levels".

### 2. Claims table — spec (v19/master) vs release

#### 2.1 Constants — §12 table, audited exhaustively (all values verified in source **and** in
`freerouting-2.3.0.jar` bytecode where the source was ambiguous)

| §12 knob | spec value | release value | verdict | release file:line |
|---|---|---|---|---|
| `max_shove_trace_recursion_depth` | 20 | **20** | ✅ match (`bipush 20` in `AutorouteControl.<init>`) | `autoroute/AutorouteControl.java:183` |
| `max_shove_via_recursion_depth` | 5 | **5** | ✅ match (`iconst_5`) | `autoroute/AutorouteControl.java:184` |
| `max_spring_over_recursion_depth` | 5 | **5** | ✅ match | `autoroute/AutorouteControl.java:185` |
| top-level `c_max_spring_over_recursion_depth` | 20 | **20** (method-local `final int`) | ✅ match; line moved (v19:789) | `board/ShoveTraceAlgo.java:618` |
| `ShapeTraceEntries.c_offset_add` | 1 | **1.0d**, and it is `private static final double` | ✅ value match; line moved (spec said :15) and it is `private`, not package-visible | `board/ShapeTraceEntries.java:18` |
| `c_offset_add` use sites | store_trace / next_piece / cutout | 4 sites: `:57,:61` (cutout_trace), `:186,:191` (next_substitute_trace_piece), `:269,:273` (store_trace) | ✅ match; 1-step compensated vs 2-step `offset(half_width)` then `offset(clearance + 1)` confirmed verbatim | `board/ShapeTraceEntries.java:57-61,186-191,269-275` |
| stack depth gate `> 1` | ≤1 everywhere | **3 call sites**, all `> 1 ⇒ fail` | ✅ match (spec cited 3 sites) | `board/ShoveTraceAlgo.java:66`, `:215`, `board/ForcedPadAlgo.java:196` |
| via shove candidate counts | 1 / ext 2 (90°), 4 (oct), 4 (simplex) | **1 / 2 / 4 / 4** | ✅ match | `board/MoveDrillItemAlgo.java:217,221,227,237` |
| via shove diagonal tolerance | +2 | **`shove_distance += 2;`** with the same "empirical value" comment | ✅ match (v19:285) | `board/MoveDrillItemAlgo.java:211-213` |
| via candidate distance gate | `(via_max_hw + shape_min_hw)²` | `max_dist = 0.5*via_bbox.max_width() + 0.5*shape_bbox.min_width()`, squared; `i == 0` always admitted | ✅ match (v19:291-299) | `board/ShoveTraceAlgo.java:219,234-238` |
| maze section proximity | `compensated_half_width + 5` (squared) | **`+ 5`**, squared | ✅ match (v19:107) | `autoroute/MazeShoveTraceAlgo.java:110-111` |
| maze shove shortening `−1` | −1 | **`shove_width = shove_width - 1`** | ✅ match (v19:187) | `autoroute/MazeShoveTraceAlgo.java:161` |
| maze "segment is a point" `len² < 0.1` | 0.1 | **0.1** | ✅ match (v19:197) | `autoroute/MazeShoveTraceAlgo.java:171` |
| partial-insert sample width | `2 * min_trace_half_width`, give up beyond `100×` | **`2 * get_min_trace_half_width()`, `> 100 * sample_width ⇒ return from_corner`** (`bipush 100` in bytecode) | ✅ match (v19:821-827) | `board/RoutingBoard.java:721,725` |
| neckdown `TOLERANCE` | 2 | **`final int TOLERANCE = 2;`** | ✅ match (v19:341) | `autoroute/InsertFoundConnectionAlgo.java:370,379,381` |
| `check_trace_segment` safety `−1` | −1 after clearance | **`Math.max(0.0, projection - shorten_value - 1)`** | ✅ match (v19:329) | `board/RoutingBoard.java:296` |
| `pull_tight_accuracy` | 500 | **500** (`sipush 500`) | ✅ match | `autoroute/AutorouteControl.java:182` |
| `ripup_costs` ctor default | 1000 | **1000** (`sipush 1000`) | ✅ match | `autoroute/AutorouteControl.java:197` |
| `start_ripup_costs` default | 100, cited at `interactive/AutorouteSettings.java:40` | **100**, but **that file does not exist in the release**; the default now lives in the settings layer | ⚠️ value match, **citation stale** | `settings/sources/DefaultSettings.java:60` (`DEFAULT_START_RIPUP_COSTS = 100`), applied `:147`; floor of 1 at `settings/RouterSettings.java:474,481` |
| `ctrl.ripup_costs = start × pass_no` | `BatchAutorouter.java:516,623` | same formula, **two live drivers** | ⚠️ value match, lines moved | `autoroute/BatchAutorouter.java:1280,1400`; `autoroute/BatchAutorouterV19.java:402` |
| `ALREADY_RIPPED_COSTS` | 1 | **1** | ✅ match; line moved (v19:41) | `autoroute/MazeSearchAlgo.java:43` (used `:442`, returned `:1381`) |
| `FANOUT_COST_CONST` | 20000 | **20000**, a *method-local* `final double` | ✅ match; line moved (v19:118) | `autoroute/MazeSearchAlgo.java:155` |
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE/100` | same, method-local `final int` | ✅ match; line moved (v19:1493) | `autoroute/MazeSearchAlgo.java:1454` |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | 2 | **2** | ✅ match; line moved (v19:30) | `autoroute/AutorouteEngine.java:33` (used `MazeSearchAlgo.java:339`) |
| ripup randomization `0.5 + rnd²`, pass ≥ 4 && pass%3 != 0 | as documented | **identical** | ✅ match | `autoroute/MazeSearchAlgo.java:1443-1449` |
| ripup clamp `[1, MAX_RIPUP_COSTS]` | as documented | **identical** | ✅ match | `autoroute/MazeSearchAlgo.java:1453-1455` |
| spring-over wrap offset `half_width + 1` (+ two half-clearance steps) | as documented | **identical** | ✅ match | `board/ShoveTraceAlgo.java:539-548` |
| `ForcedPadAlgo` probe shape offset 1 | offset 1, 3-line polyline | **`check_line.offset_shape(1, 0)`** | ✅ match | `board/ForcedPadAlgo.java:32-43` |
| `ForcedViaAlgo` `check_radius = via_radius + 0.5*cl(cl,cl,layer) + min_trace_half_width` | as documented | **identical** | ✅ match | `board/ForcedViaAlgo.java:41` |
| `in_front_of_pad` typo `line_b.x + line_b.x` | present, v19:69 | **still present**, case 0 | ✅ bug confirmed shipped | `board/ForcedPadAlgo.java:65` |

**Constants in the release that the §12 table does not have (new, must be ported):**

| new knob | value | release file:line |
|---|---|---|
| per-connection `TimeLimit` for the shove `check` paths | `100000 * 2^(ripup_pass_no − 1)` ms, capped at `Integer.MAX_VALUE` | `autoroute/BatchAutorouter.java:1311-1313` (identically `autoroute/BatchAutorouterThread.java:501`, dead) |
| hole-clearance probe radius | `ceil(drill_radius + hole_clearance + 10)`, **clearance class 0** | `board/ForcedViaAlgo.java:240-251` |
| ripup fanout-protection window | `ctrl.ripup_costs <= start_ripup_costs * 2` (i.e. only passes 1-2 protect fanout traces) | `autoroute/MazeSearchAlgo.java:1386-1387` |
| fanout micro-neckdown candidate half-widths | pin neckdown half-widths, then `max(1, base*3/4)`, `max(1, base*3/5)`, `max(1, base/2)`; skip if `≤0` or `≥ base` | `autoroute/InsertFoundConnectionAlgo.java:289-333` (candidates built `:299-309`) |
| maze drill "required radius" | `max(via_radius_on_layer, ctrl.trace_half_width[layer])` | `autoroute/MazeSearchAlgo.java:1193` |
| `CalcFromSide` fallback box tolerance | **0.01** | `board/CalcFromSide.java:120` |

#### 2.2 Behaviour deltas — where the release differs from what §1-§11 document

| # | spec claim | release behaviour | release file:line | port impact |
|---|---|---|---|---|
| **B1** | §10.2: guard is `trace_corner_no >= polyline.arr.length - 1 ⇒ FRLogger.warn + return false` | **`if (trace_corner_no < 0 \|\| trace_corner_no >= trace_polyline.arr.length - 2) return false;`** — one index stricter, also rejects negative, **no warning**. Comment: "Check if trace_corner_no allows access to indices up to trace_corner_no + 2 … Stale indices can occur when traces are modified during routing" | `autoroute/MazeShoveTraceAlgo.java:57-63` | **Port the release form.** v19 could index `arr[c+2]` out of range; this is a real crash fix and it also prunes one maze-time shove case that v19 attempted. |
| **B2** | §10.2 door harvesting: empty door shape ⇒ `FRLogger.warn` gated on `TestLevel.ALL_DEBUGGING_OUTPUT` | unconditional `FRLogger.trace(...)`, then `continue` — same control flow, `TestLevel` type is gone from the release entirely | `autoroute/MazeShoveTraceAlgo.java:318-322` | none (logging only) |
| **B3** | §8: `ForcedViaAlgo.check_layer(via_radius, cl, attach_smd, room_shape, location, layer, nets, max_rec, max_via_rec, board)` — 10 args, checks the via pad only | **12 args**, `+ p_trace_half_width, p_trace_clearance_class`; after the via-pad `check_forced_pad` it **additionally** runs `check_forced_pad` on a `Circle(location, trace_half_width)` bounding box/octagon with the trace clearance class and `copper_sharing = true`; result is `DRILLABLE_WITH_ATTACH_SMD` if **either** sub-check says so | `board/ForcedViaAlgo.java:28-84` | **Real behaviour change in the maze drill machinery.** The maze now refuses drills where the start-trace stub would not fit. |
| **B4** | §8 caller: maze passes `ctrl.via_radius_arr[layer]` for a single via | release iterates `ctrl.via_rule.via_count()` per layer, takes `requiredRadius = max(0.5*viaShape.max_width(), ctrl.trace_half_width[layer])`, returns `DRILLABLE` on the first via that fits, remembering `DRILLABLE_WITH_ATTACH_SMD` | `autoroute/MazeSearchAlgo.java:1185-1204` | changes which layers are drillable; port with B3 |
| **B5** | §11 `insert_via`: `ForcedViaAlgo.check(via_info, location, [net], 20, 5, board)` | **`check(via_info, location, nets, max_rec, max_via_rec, board, ctrl.trace_half_width, ctrl.trace_clearance_class_no)`**; also tracks `found_suitable_span` to distinguish "no via spans these layers" from "blocked by clearance/shove" in the log | `autoroute/InsertFoundConnectionAlgo.java:424-489` (the `check` call at `:449-451`, `found_suitable_span` at `:445`) | signature + stricter check |
| **B6** | §11 `insert_trace` advance loop: advance / retreat / fail, neckdown only | release adds a **third** fallback: `insert_fanout_micro_neckdown(...)`, tried only when `!neckdown_inserted && ok_point != last_corner && ctrl.is_fanout && curr_corner_arr.length == 2`; it retries `insert_forced_trace_segment` at progressively thinner half-widths (see constants table). Success counts as ADVANCE | invoked `autoroute/InsertFoundConnectionAlgo.java:148-157`, ADVANCE test `:157`; body `:289-333` | fanout pass only (`ctrl.is_fanout` is set **only** in `board/RoutingBoard.java:1018`, reached from `BatchFanout`), but it changes fanout results |
| **B7** | §10.4 `cost_factor` fanout protection gated on `!ctrl.remove_unconnected_vias` | gated on `preserveFanoutProtection = !ctrl.remove_unconnected_vias && ctrl.ripup_costs <= start_ripup_costs * 2` — since `ripup_costs = start × pass_no`, **fanout protection switches off from ripup pass 3 onward** | `autoroute/MazeSearchAlgo.java:1386-1396` | changes ripup costs from pass 3; port it |
| **B8** | §11: `p_board.connect_to_trace(first_corner/last_corner, …)` unconditional | null-guarded; `first_corner == null` / `last_corner == null` ⇒ warn + skip `connect_to_trace` instead of NPE | `autoroute/InsertFoundConnectionAlgo.java:71-84`; plus a new "set both corners when the trace has 1 corner" guard at `:95-101` | defensive; keep |
| **B9** | §9 `insert_forced_trace_polyline` step 1: non-IntPoint endpoints ⇒ warn, return `from_corner` | release adds an earlier guard: `from_corner == null \|\| to_corner == null ⇒ trace + return null` (i.e. the *damaged-board* code, before anything is modified) | `board/RoutingBoard.java:712-722` | **Note the asymmetry**: this returns `null`, which `InsertFoundConnectionAlgo` decodes as "DB damaged ⇒ FAIL". A Rust port should return the clean "nothing inserted" code here instead. |
| **B10** | §9/§5.3 tail cleanup: `board.remove_items(items, false)` (two-arg, `p_with_delete_fixed`) | one-arg `remove_items(items)`; `BasicBoard.remove_items` now refuses items where `isDeletionForbidden() \|\| is_user_fixed()` — semantically identical to the v19 `p_with_delete_fixed = false` call | `board/BasicBoard.java:722-733`; call sites `board/ShoveTraceAlgo.java:409`, `board/ForcedPadAlgo.java:315` | none |
| **B11** | §11 trace-tail cleanup loop gated on `board.get_test_level() < ALL_DEBUGGING_OUTPUT` | `TestLevel` removed from the release; the tail-removal loop now runs **unconditionally** | `autoroute/InsertFoundConnectionAlgo.java:256-271` | ports as "always clean tails" |
| **B12** | (spec is silent) | `FRLogger.trace` gained a 5-arg overload `(method, operation, message, impactedItems, Point[] impactedPoints)`; every shove-path trace site now passes `new Point[0]` or a `{from_corner, to_corner}` pair. `board.normalize_traces(net)` itself is **unchanged, one-arg** (`autoroute/InsertFoundConnectionAlgo.java:86`) | `logger/FRLogger.java:367,383` | logging only |
| **B13** | §3.7 `fast_cutout_trace` notifies observers unconditionally | wrapped in `if (board.communication != null && board.communication.observers != null)` | `board/ShapeTraceEntries.java:149-153` | none |
| **B14** | §7.1 `in_front_of_pad` uses octagon fields `lx, ly, rx, uy, ulx, lrx, llx, urx` | the `IntOctagon` fields were renamed to `leftX, bottomY, rightX, topY, upperLeftDiagonalX, lowerRightDiagonalX, lowerLeftDiagonalX, upperRightDiagonalX`; **all 8 predicates are otherwise token-identical**, typo included | `board/ForcedPadAlgo.java:48-135` | naming only |

**Spec statements that are inaccurate about *both* trees (found during this audit):**

- §5.1 "Every clearance subtraction uses the asymmetric `ClearanceMatrix.get_value(a,b,layer,true)`" —
  false for the substitute-trace branch: when clearance compensation is **on**, the code subtracts
  `search_tree.clearance_compensation_value(piece.cl_class, layer)` instead
  (`board/ShoveTraceAlgo.java:145-149`). Only the via branch (`:104`) is unconditionally
  `cl_matrix.get_value(...)`, and it applies only when compensation is off.
- §3.5 "if raising past 1, blame `curr_entry.trace`" — the guard is
  `if (curr_level > max_stack_level) { if (max_stack_level > 1) found_obstacle = curr_entry.trace; max_stack_level = curr_level; }`
  i.e. the blame is recorded only when `max_stack_level` was **already** > 1 before the raise
  (`board/ShapeTraceEntries.java:530-540`). Confirmed identical in v19; the spec's phrasing is off.
- §3.1 "`Item.java:808`" for `is_shove_fixed()` — release `board/Item.java:834`; note `Trace`
  overrides it at `board/Trace.java:251`. `FixedState` order
  `UNFIXED < SHOVE_FIXED < USER_FIXED < SYSTEM_FIXED` confirmed
  (`board/FixedState.java:7-10`; the per-constant comments there are shifted by one and are wrong —
  ignore them).

#### 2.3 Explicit confirmations — everything else in §1-§11 matches

Verified by comment-stripped, whitespace-normalised token-level diff of v19 vs release. Files whose
**only** differences are cosmetic (`++i`→`i++`, Java-16 pattern-matching `instanceof`, javadoc
rewrap) and therefore have **byte-identical algorithmic content**:

| file | release path | token deltas |
|---|---|---|
| `ShoveTraceAlgo` (§5, incl. static check, instance check, insert, spring_over, spring_over_obstacles) | `board/ShoveTraceAlgo.java` (640 L) | cosmetic only + B10 |
| `ShapeTraceEntries` (§3, incl. store_items, store_trace, insert_entry_point, resort, calculate_stack_levels, pop_piece, next_substitute_trace_piece, cutout_trace, fast_cutout_trace) | `board/ShapeTraceEntries.java` (725 L) | cosmetic only + B13 |
| `CalcShapeAndFromSide` (§4, incl. the `p_in_shove_check` stack-level comment at `:65-70`) | `board/CalcShapeAndFromSide.java` (108 L) | cosmetic only |
| `MoveDrillItemAlgo` (§6, incl. `check`, `insert`, `shove_vias`, `try_shove_via_points`, the "no time limit here" comment at `:165`) | `board/MoveDrillItemAlgo.java` (247 L) | cosmetic only (+ private ctor) |
| `ForcedPadAlgo` (§7, incl. `check_forced_pad`, `forced_pad`, `calc_from_side`, `in_front_of_pad`) | `board/ForcedPadAlgo.java` (351 L) | B14 renames + B10 |
| `MazeShoveTraceAlgo` (§10.2, incl. link-door/1-dim-door segment construction, `end_points_matching`, door harvesting) | `autoroute/MazeShoveTraceAlgo.java` (315 L) | B1 + B2 only |
| `MazeSearchAlgo.expand_to_room_doors` (§10.1) | `autoroute/MazeSearchAlgo.java:323-521` | **zero** semantic deltas |
| `MazeSearchAlgo.shove_trace_room` (§10.1) | `autoroute/MazeSearchAlgo.java:1484-1546` | **zero** deltas (normalised diff empty) |
| `MazeSearchAlgo.check_ripup` (§10.4) | `autoroute/MazeSearchAlgo.java:1356-1476` | B7 only |
| `MazeSearchAlgo.expand_to_target_doors` thin-room `check_forced_trace_polyline` (§10.3) | `autoroute/MazeSearchAlgo.java:571-586` | unchanged (plus new `tree_entry_no` / `target_shape == null` range guards at `:560-570`) |
| `RoutingBoard.check_trace_segment` (§9) | `board/RoutingBoard.java:250-306` | unchanged |
| `RoutingBoard.check_forced_trace_polyline` (§9) | `board/RoutingBoard.java:559-582` | unchanged |
| `RoutingBoard.insert_forced_trace_polyline` (§9, all 7 steps incl. resampling) | `board/RoutingBoard.java:591-~1210` | B9 + shipped debug blocks only |
| `RoutingBoard.move_drill_item` SHOVE_FIXED unfixing (§9) | `board/RoutingBoard.java:384-394` | unchanged |
| `ForcedPadAlgo.calc_from_side` two-pass probe, 2nd pass with cl class 0 (§7.3) | `board/ForcedPadAlgo.java:329-347` | unchanged |
| `MazeSearchElement.Adjustment {NONE, RIGHT, LEFT}` (§10.1) | `autoroute/MazeSearchElement.java:18,37` | unchanged |

**Recursion budgets and their decrement points (explicitly re-verified, §12 "Work bounding"):**
`max_rec` decrements **only** on substitute-trace recursion
(`ShoveTraceAlgo.java:137` static, `:284` instance check, `:383` insert;
`ForcedPadAlgo.java:218` check → `ShoveTraceAlgo.check(…, p_max_recursion_depth - 1, …)`,
`:281` insert → `ShoveTraceAlgo.insert(…, p_max_recursion_depth - 1, …)`);
`max_via_rec` decrements **only** on via cascades
(`ShoveTraceAlgo.java:94,241`, `ForcedPadAlgo.java:175`, `MoveDrillItemAlgo.java:166,176`);
`max_spring_over_rec` decrements **only** when `spring_over` actually changed the polyline
(`ShoveTraceAlgo.java:275,376`). `TimeLimit` is checked in instance `check` and
`MoveDrillItemAlgo.check` and **never** in the insert paths. All identical to §12.

**`c_offset_add` semantics re-verified verbatim** (`board/ShapeTraceEntries.java:265-282`):

```java
if (search_tree.is_clearance_compensation_used()) {
  double curr_offset = p_trace.get_compensated_half_width(search_tree) + c_offset_add;
  offset_shape = (TileShape) shape.offset(curr_offset);
} else {
  // enlarge the shape in 2 steps  for symmetry reasons
  double cl_offset = board.clearance_value(p_trace.clearance_class_no(), this.cl_class, p_trace.get_layer()) + c_offset_add;
  offset_shape = (TileShape) shape.offset(p_trace.get_half_width());
  offset_shape = (TileShape) offset_shape.offset(cl_offset);
}
// using enlarge here instead offset causes problems because of a
// comparison in the constructor of class EntryPoint
```

Both load-bearing comments (§15 "Keep semantically") are present unchanged.

### 3. §13 "v2" features — PRESENT/ABSENT in the release (porting go/no-go)

| §13 item attributed to v2 | in release 2.3.0? | evidence |
|---|---|---|
| **§13.1** `CalcFromSide` ctor-3 nearest-side fallback when the front-side scan fails | **PRESENT** — implemented inside `CalcFromSide` (v19 name), `is_contained_in_box(side_start, side_end, 0.01)`, same `±2` rotation, `border_intersection` = middle of the chosen side, **and the v19 `FRLogger.warn` is gone** | `board/CalcFromSide.java:106-142` |
| §13.1 `border_intersection` made a mutable public field | **PARTIAL** — it is non-`final` and package-visible (`FloatPoint border_intersection;`), **not** `public` | `board/CalcFromSide.java:17` |
| §13.1 note: fallback leaves `nearest_side = 0` when no projection lands in any side box | **PRESENT (latent)** — `nearest_side` is initialised to 0 and never validated; also `this.border_intersection = projection` inside the loop is dead (overwritten at `:140`) | `board/CalcFromSide.java:110,125,140` |
| **§13.2** `holeCheckShape` / hole-clearance circle `ceil(drill_radius + hole_clearance + 10)` with clearance class 0 | **PRESENT** as `hole_check_shape(...)` | `board/ForcedViaAlgo.java:240-251` |
| §13.2 hole shape used as the pad shape on copper-less padstack layers | **PRESENT** in both `check` and `insert` | `board/ForcedViaAlgo.java:99-106` (check), `:172-180` (insert) |
| §13.2 hole shape as an **additional** forced-pad check/insert on copper layers (`curr_clearance_class != 0`) | **PRESENT** in both | `board/ForcedViaAlgo.java:121-136` (check), `:205-217` (insert) |
| §13.2 `check(...)` gains `tracePenHalfwidthArr` + `traceClearanceClassIndex` and now also checks the start-trace circle | **PRESENT** | `board/ForcedViaAlgo.java:88-90,138-154` |
| §13.2 `insert` mirrors both additions | **PRESENT** (start-trace circle at `:218-224` with the "necessary in case start_trace_shape is bigger than tile_shape" comment) | `board/ForcedViaAlgo.java:165-229` |
| **§13.3** `MazeSearchAlgo` split into `MazeSearchEngine` + `MazeRipupResolver` | **ABSENT** — single 1682-line `autoroute/MazeSearchAlgo.java`; `shove_trace_room` at `:1484`, `check_ripup` at `:1356` | jar class list |
| §13.3 `RoutingBoard` facade split (`board/facade/RoutingBoard` + `RoutingBoardSearchFacade`) | **ABSENT** — single 1420-line `board/RoutingBoard.java` | jar class list |
| §13.3 `ShapeTraceEntries` moved to `board/searchtree` with `shoveViaList` public | **ABSENT** — stays `board/ShapeTraceEntries.java`; the field is package-visible `final Collection<Via> shove_via_list` | `board/ShapeTraceEntries.java:18-21` |
| §13.3 "knob defaults unchanged (`v2 AutorouteControl.java:171-186`)" | **CONFIRMED as values**, but the file is `autoroute/AutorouteControl.java:182-185` | see §2.1 |
| §File-map `board/optimize/TraceShover`, `board/model/structure/ShapeEntrySide`, `board/actions/ForcedPadRouter` etc. | **ABSENT (all)** | jar class list |
| §13 "Verified callers: `MazeSearchEngine.java:1130-1195` … `board/facade/RoutingBoard.java:423,521`" | **INVALID for the release** — real callers are `autoroute/MazeSearchAlgo.java:1484-1546` → `MazeShoveTraceAlgo.check_shove_trace_line`, and `board/RoutingBoard.java:566` (`check_forced_trace_polyline`) + `:638` (`insert_forced_trace_polyline`) → `new ShoveTraceAlgo(this)` | grep |

**New in the release that §13 does not mention** (also go/no-go items): B1, B3-B7 above, plus
`autoroute/RoutingFailureLog` (`board/RoutingBoard.java:50,70`) — a per-board failure log
attached to `RoutingBoard`, not consulted by any shove decision.

### 4. Port-from verdict — canonical source file per algorithm

All paths relative to `/tmp/fr-java-release/src/main/java/app/freerouting`.

| algorithm (this document's §) | port from | notes |
|---|---|---|
| From-side calculus, all 4 constructors (§2) | `board/CalcFromSide.java` (159 L) | includes the §13.1 fallback; drop the dead `border_intersection = projection` write |
| Entry-point list, offsets, stack-level bracket calculus, substitute-piece generation, trace cutout (§3) | `board/ShapeTraceEntries.java` (725 L) | `c_offset_add` at `:18`; `cutout_trace` `:48-79`; `fast_cutout_trace` `:84-105`; `net_nos_equal` `:107-124`; `store_items` `:130-171`; `next_substitute_trace_piece` `:176-221`; `store_trace` `:265-370`; `search_from_side` `:372-388`; `resort` `:393-513`; `calculate_stack_levels` `:515-609`; `pop_piece` `:615-667`; `insert_entry_point` `:669-704`; `EntryPoint` `:709-725` |
| Dog-ear cutting + per-segment from-side for substitute pieces (§4) | `board/CalcShapeAndFromSide.java` (108 L) | `p_in_shove_check` semantics at `:65-70` |
| Trace shover: static length-check, instance check, destructive insert, spring-over (§5) | `board/ShoveTraceAlgo.java` (640 L) | static `check` `:37-161`; instance `check` `:166-292`; `insert` `:297-418`; `get_ignore_items_at_tie_pins` `:420-431`; `spring_over` `:437-610`; `spring_over_obstacles` `:617-640` |
| Via/drill-item shoving (§6) | `board/MoveDrillItemAlgo.java` (247 L) | `check` `:26-76`; `insert` `:82-123`; `shove_vias` `:128-181`; `try_shove_via_points` `:187-247` |
| Pad shoving + `in_front_of_pad` + free-standing pad from-side (§7) | `board/ForcedPadAlgo.java` (351 L) | `calc_check_shape_for_from_side` `:32-43`; `in_front_of_pad` `:48-136`; `check_forced_pad` `:141-226` (stack gate `:196`, recursion `:218`); `forced_pad` `:232-324` (tail cleanup `:311-320`); `calc_from_side` `:329-347`; `CheckDrillResult` `:349-351` |
| Forced via insertion incl. hole clearance and start-trace circle (§8) | `board/ForcedViaAlgo.java` (333 L) | **use this, not v19** — `check_layer` `:28-84`, `check` `:89-158`, `insert` `:165-232`, `hole_check_shape` `:240-252`, `calculate_from_side` `:254-333` |
| Board-level entry points: `check_trace_segment`, `check_forced_trace_polyline`, `insert_forced_trace_polyline`/`_segment`, `forced_via`, `move_drill_item` (§9) | `board/RoutingBoard.java` (1420 L) | `:250, :384, :496, :533, :559, :591`; **strip the `net == 94` / id-generator-delta trace blocks** |
| Maze-time virtual shove (§10.2) | `autoroute/MazeShoveTraceAlgo.java` (315 L) | **use the release bounds guard at `:57-63` (B1)** |
| Maze shove gating + ripup weighing (§10.1, §10.4) | `autoroute/MazeSearchAlgo.java` (1682 L) | `expand_to_room_doors` `:323`; `shove_trace_room` `:1484`; `check_ripup` `:1356` (with B7); `room_shape_is_thick` `:1331`; `calc_fanout_via_ripup_cost_factor` `:154`; maze drill check `:1185` (B4) |
| Insertion-time integration, advance/retreat loop, neckdown (§11) | `autoroute/InsertFoundConnectionAlgo.java` (514 L) | `get_instance` `:42-91`; `insert_trace` `:94-288` (advance/retreat loop `:126-250`, tail cleanup `:256-271`); `insert_fanout_micro_neckdown` `:289-333` (B6); `insert_neckdown` `:335-347`; `try_neck_down` `:349-422` (`TOLERANCE` `:370`); `insert_via` `:424-489` (B5); dead-weight `shouldTraceFanoutDiagnostics`/`traceFanoutDiagnostic`/`formatPoint` `:490-513`. **Strip `FANOUT_DIAG` / `compare_trace_*`.** |
| Knob defaults (§12) | `autoroute/AutorouteControl.java:150-200` (the private base ctor; defaults block `:178-200`) + `settings/sources/DefaultSettings.java:60` + `settings/RouterSettings.java:473-482` | `AutorouteSettings` no longer exists |
| Driver / pass + ripup schedule feeding the shove budget | `autoroute/BatchAutorouter.java` (headless & GUI default) | `runBatchLoop` `:757`; `autoroute_item` `:1257-1360`. `BatchAutorouterV19.java` is GUI-only and irrelevant to shove semantics. **Do not port `autoroute_pass_multi_thread` / `BatchAutorouterThread` — dead code.** |
