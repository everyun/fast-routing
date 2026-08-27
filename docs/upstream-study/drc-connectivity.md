# DRC, Connectivity and Ratsnest — how Freerouting *judges* correctness

Upstream study written as an implementation-ready spec for an **independent Rust checker** that
referees the benchmark. Readers are assumed to have no access to the Java source.

This spec answers three questions exactly:

1. **When is a pair of items a clearance violation?** (which pairs, which shapes, which metric,
   which clearance number, which layer, which same-net exemptions)
2. **When are two items electrically connected, and when is a net "done"?** (the geometric
   contact predicate, the connected-set closure, the airline/incomplete count)
3. **What exactly does the number printed as `(N unrouted and M violations)` in the batch log
   mean?** (full-board recompute or incremental; deduplicated or not; which weights)

---

## 0. Source map and which tree 2.3.0 runs

| Role | v1.9 ("v19") `/tmp/fr-java/src_v19/main/java/app/freerouting/` | v2.x ("v2", the 2.3.0 line) `/tmp/fr-java/src/main/java/app/freerouting/` |
|---|---|---|
| Violation record | `board/ClearanceViolation.java` | `drc/ClearanceViolation.java` (122 L) |
| Violation compute | `board/Item.clearance_violations()` (Item.java:313-443) | `board/model/items/Item.clearanceViolations()` (Item.java:367-493) |
| Violation aggregation | `interactive/ClearanceViolations.java` (GUI-only, 51 L) | `drc/ClearanceViolation.aggregateSortedBySeverity` + `gui/workspace/progress/ClearanceViolations.java` (thin façade) |
| Whole-board DRC | *(none — v19 has no DRC service class)* | **`drc/DesignRulesChecker.java` (821 L) — new in v2** |
| DRC report I/O | *(none)* | `io/kicad/KiCadDrcReport/Violation/ViolationItem/Position.java` — new in v2 |
| Unconnected-net record | *(none)* | `drc/UnconnectedItems.java` — new in v2 |
| Airline record | `interactive/RatsNest.AirLine` (nested class) | `drc/AirLine.java` (top level, GUI-free) |
| Per-net incompletes | `interactive/NetIncompletes.java` (293 L, has Swing `draw`) | `drc/NetIncompletes.java` (398 L, headless) |
| Ratsnest façade | `interactive/RatsNest.java` (241 L) | `gui/workspace/progress/RatsNest.java` (457 L, delegates to `DesignRulesChecker`) |
| Connectable contract | `board/Connectable.java` | `board/model/items/Connectable.java` |
| Clearance matrix | `rules/ClearanceMatrix.java` | `rules/ClearanceMatrix.java` (identical constants) |
| Spatial index | `board/ShapeSearchTree.java` | `board/searchtree/ShapeSearchTree.java` |
| Board query entry points | `board/BasicBoard.java` | `board/facade/BasicBoard.java` (+ `BoardConnectivityQueries`, `BoardItemRepository`) |
| Triangulation | `datastructures/PlanarDelaunayTriangulation.java` | same file, camelCased (978 L) |
| Progress airline | *(inline in `BatchAutorouter`)* | `autoroute/pipeline/AutorouteAirlineCalculator.java` (214 L) |

**2.3.0 runs the v2 tree.** Evidence: `core/scoring/BoardStatistics` (the class every batch/score
call site uses) imports `app.freerouting.drc.DesignRulesChecker` and
`gui.workspace.progress.RatsNest`; `Freerouting.java:332` constructs `DesignRulesChecker` for the
CLI `--drc` mode; `autoroute/pipeline/*` (the only batch entry, see `batch-orchestration.md` §1)
constructs `DesignRulesChecker` in six places. The v19 `interactive/RatsNest` + `interactive/
NetIncompletes` pair does not exist in v2.

Cross-references to sibling specs — do not duplicate:

* `search-tree-obstacles.md` — the `ShapeSearchTree` node layout, `overlappingTreeEntries`
  traversal, per-item tree-shape caching, drill-hole synthetic obstacles. This spec only restates
  the *query contracts* it needs.
* `batch-orchestration.md` — the pass loop, `BoardHistory`, stagnation. This spec covers only
  where in that loop DRC/ratsnest are recomputed and what that costs.
* `maze-engine.md` / `shove.md` / `pull-tight-insert.md` — the *router's* obstacle tests
  (`checkTraceShape`, `ShapeTraceEntries`) are a different, faster, approximate code path than the
  DRC reporting path; §5 below explains how they differ.

> ⚠️ **Local-instrumentation caveat.** Both checkouts carry debug patches that are not upstream:
> hard-coded net numbers in `DesignRulesChecker.calculateAllIncompletes` (`int[] focusNets = {98,
> 99}`, DesignRulesChecker.java:594), net-49 tracing in `Trace.isCycle` (Trace.java:273-292), and
> a dead `PolylineTrace.validateAndLogPolylineIntegrity()` call commented out at
> DesignRulesChecker.java:611. None change results. Do not port them.

---

## 1. The clearance model

### 1.1 Units

All board geometry is in **integer board units**. `board.communication.resolution` DSN units per
board unit; the DRC log formats board units as millimetres by dividing by `10000.0`
(DesignRulesChecker.java:246-252, ClearanceViolation.java:118-119), i.e. the log assumes the
common 10 000 units/mm case. The *statistics* path is resolution-correct and converts with
`boardUnitToUmFactor = Unit.scale(1.0, boardUnit, UM) / max(resolution,1)`
(BoardStatistics.java:198-201). A Rust port must carry the resolution, not the hardcoded 10 000.

### 1.2 `ClearanceMatrix` (rules/ClearanceMatrix.java)

```
ClearanceMatrix {
    classCount: usize,                       // number of clearance classes; class 0 == "null"
    row: Vec<Row>,                           // row[classJ]
    maxValueOnLayer: Vec<i32>,               // per layer, max over the whole matrix
}
Row { name: String, column: Vec<MatrixEntry>, maxValue: Vec<i32> /*per layer*/ }
MatrixEntry { layer: Vec<i32> }              // one value per board layer
```

* **Lookup**: `getValue(classI, classJ, layer, addSafetyMargin)` reads
  `row[classJ].column[classI].layer[layer]`, then adds `clearance_safety_margin = 16`
  (ClearanceMatrix.java:17) if `addSafetyMargin`. Out-of-range indices return **0** with a trace
  log (ClearanceMatrix.java:133-161) — a silent "no clearance required".
* **Symmetry is by construction, not enforced.** `setValue(i, j, layer, v)` writes only
  `row[j].column[i]`. Every bulk setter writes both orders (`setDefaultValue` loops i,j over all
  pairs; the hole/edge overrides at `HeadlessBoardManager.java:449-450, 534-535` write both). A
  Rust port should store a symmetric matrix and assert symmetry on load.
* **Values are forced even and ≥ 0** on write: `v = max(v,0); if v odd { v += 1 }` (with
  `Integer.MAX_VALUE` decremented instead) — ClearanceMatrix.java:106-113. This matters because
  the algorithm halves clearances.
* `maxValue(classI, layer)` = `row[classI].maxValue[layer]`, i.e. the max over that class's row —
  used to size the candidate search box.
* `clearanceCompensationValue(cc, layer) = (getValue(cc, cc, layer, false) + 1) / 2` — half of
  the class's self-clearance, rounded up (ClearanceMatrix.java:273-275).
* Class 0 (`"null"`) is never written by `setDefaultValue` (loops start at `i = 1`), so all
  clearances to class 0 are **0**. `BoardRules.defaultClearanceClass() == 1`,
  `BoardRules.clearanceClassNone() == 0` (BoardRules.java:64-72).

Per-item-type default classes live in `rules/DefaultItemClearanceClasses` with
`ItemClass ∈ {NONE, TRACE, VIA, PIN, SMD, AREA}`, all initialised to class 1.

### 1.3 Two synthetic clearance classes created headlessly

The headless loader mutates the matrix after DSN parse (`management/HeadlessBoardManager.java`):

* **`board_edge`** (`applyCopperToEdgeClearanceOverride`, :469-545): appends a class, sets
  `matrix[board_edge][c] = matrix[c][board_edge] = copperToEdgeClearanceUm` (converted to board
  units) for every `c ≥ 1` and every layer, then reassigns the `BoardOutline`'s clearance class to
  it. Skipped when the value equals the default **and** the outline already has an explicit DSN
  class (:504-509). Default `copperToEdgeClearanceUm = 500.0` (0.5 mm).
* **`hole_edge`** (`assignHoleKeepoutClearanceClass`, :404-462): every `ObstacleArea` (exact
  class, not a subclass) that belongs to a component and whose area is a `Circle` is treated as an
  NPTH hole and reassigned to the `hole_edge` class, whose matrix entries are
  `max(holeClearanceBoardUnits, matrix[defaultAreaClass][c])` — a **floor**, never a reduction.
  Only when `holeClearanceUm > 0`; default is `0.0`.

After either override the manager calls `searchTreeManager.reinsertTreeItems()`, which drops every
item's cached tree shapes and re-inserts — otherwise DRC would read stale precomputed shapes
(SearchTreeManager.java:186-200).

### 1.4 Which shapes DRC compares — `Item.getTileShape(i)`

```
Item.getTileShape(i)  ==  Item.getTreeShape(board.searchTreeManager.getDefaultTree(), i)
                                                                (Item.java:195-201)
```

So **DRC compares the *default search tree's* shapes, not raw copper.** For the default tree:

* `SearchTreeManager` constructs `defaultTree = ShapeSearchTree(FortyfiveDegreeBoundingDirections,
  board, compensatedClearanceClassNo = 0)` and sets `clearanceCompensationUsed = false`
  (SearchTreeManager.java:33-35).
* `setClearanceCompensationUsed(true)` — which rebuilds the default tree with
  `compensatedClearanceClassNo = 1` — is called **only from GUI code**
  (`gui/workspace/controllers/GuiBoardRoutingSettings.java:122`,
  `gui/windows/routing/WindowClearanceMatrix.java:253`). **In headless/batch runs clearance
  compensation on the default tree is always OFF.** This is the single most important
  configuration fact in this spec.
* With `compensatedClearanceClassNo == 0`, `clearanceCompensationValue(cc, layer)` =
  `max(0, getValue(cc, 0, layer, false) − clearanceCompensationValue(0, layer))` = `max(0, 0 − 0)`
  = **0** (ShapeSearchTree.java:104-114). So tree shapes are un-inflated copper …

… **except drill items.** `ShapeSearchTree.calculateTreeShapes(DrillItem)`
(ShapeSearchTree.java:870-903) adds
`offsetWidth = clearanceCompensationValue(...) + drillHoleClearanceDelta(drillItem, shape, layer)`,
and `drillHoleClearanceDelta` (:1034-1074) is applied regardless of compensation:

```
delta = max(0, ceil( drillRadius + holeClearance + 10          // DRILL_HOLE_CLEARANCE_MARGIN = 10
                     − copperRadius − copperClearance ))
copperRadius     = holeOnly ? drillRadius : shape.borderDistance(center)   (fallback: pad shape)
copperClearance  = matrix[drillItem.cc][ (compensatedCC>0 ? compensatedCC : 1) ][layer]
```

and when a layer has **no pad shape at all**, `drillHoleObstacle` synthesises a
`Circle(center, ceil(drillRadius))` so the hole still blocks that layer (:1004-1028). Both are
zero when `holeClearance == 0` (the default), so on a stock DSN run drill items are plain copper.

Per item type, the default-tree shapes are:

| Item | tree shapes |
|---|---|
| `PolylineTrace` | one `TileShape` per polyline segment: `polyline.offsetShape(halfWidth + comp, i)` (ShapeSearchTree.java:996-1002). `tileShapeCount == lines.length − 2`. |
| `DrillItem` (Pin/Via) | one per padstack layer, `padShape.boundingOctagon()` (45° mode) / `boundingBox()` (90°) / `boundingTile()` (any-angle), enlarged by `comp + holeDelta` (:870-903). `tileShapeCount = padstack.toLayer − fromLayer + 1`. |
| `ObstacleArea` / `ConductionArea` | `splitToConvex()`, each enlarged by `comp`, then `divideIntoSections(maxTreeShapeWidth)` where `maxTreeShapeWidth = min(500 · resolution(MIL), 50000)` when a host CAD is declared, else `50000` (:906-937). |
| `BoardOutline` | if a keepout-outside-outline area was generated: its convex pieces replicated **per layer**; otherwise 3-line offset strips of half-width `halfWidth + comp` around every outline border line, replicated per layer (:939-987). `shapeLayer(i) = i · layerCount / shapeCount`. |
| `ComponentOutline` | `tileShapeCount() == 0` — never indexed, never a DRC participant. |

`shapeLayer(index)`: `Trace` → its layer; `DrillItem` → `firstLayer() + min(index, lastLayer −
firstLayer)`; `ObstacleArea` → its layer; `BoardOutline` → the striped formula above.

### 1.5 The distance metric is the **octagon norm**, not Euclidean

Every clearance test is expressed as "enlarge both shapes by half the clearance and test
overlap". Enlarging is:

* `IntOctagon.enlarge(d) == offset(d)`: `left −= round(d)`, `right += round(d)`, `bottom/top` the
  same, and the four diagonal bounds move by `round(√2·d)` (IntOctagon.java:288-320). This is a
  Minkowski sum with a **regular octagon**, not a disc.
* `IntBox.enlarge(d) == boundingOctagon().offset(d)` — an `IntBox` becomes an `IntOctagon`
  (IntBox.java:388-393).
* `Simplex.enlarge(d)` = translate every bounding line outward by `d`
  (`Line.translate`, Line.java:347-368 — **rounds the new anchor point to integers**), then
  intersect with the offset bounding octagon (Simplex.java:576-587).

Consequences a Rust checker must replicate bit-for-bit if it wants to agree with Freerouting:

1. The gap between two shapes is measured in the **octagon metric** (`max(|dx|, |dy|,
   (|dx|+|dy|)/√2)`-like), which for a 45° diagonal pair is up to √2 larger than the Euclidean
   gap. A Euclidean checker will report violations Freerouting does not, and vice versa.
2. Offsets are **quantised to integer board units** (`round(d)`, `round(√2 d)`), so results are
   accurate to ±1 board unit at best.
3. `dimension()` of the intersection is the violation test, and it distinguishes 2 (area), 1
   (segment), 0 (point), −1 (empty) — see `Simplex.dimension` (Simplex.java:252-290),
   `IntBox.dimension` (:95-107), `IntOctagon.dimension` (:127-…). **Only `dimension() == 2`
   counts**, so exactly-touching shapes are legal.

---

## 2. The exact clearance-violation computation

### 2.1 Entry point: `Item.clearanceViolations()` (v2 Item.java:367-469)

Returns the violations **of this item with all others**, with `firstItem == this` always.
This is the *only* place a `ClearanceViolation` is ever constructed in either tree.

```
fn clearance_violations(self) -> Vec<ClearanceViolation>:
  result = []
  if self.board == null: return result
  tree = board.searchTreeManager.getDefaultTree()

  for i in 0 .. self.tileShapeCount():                       # per own tile shape
      my_shape  = self.getTileShape(i)                       # == tree shape, §1.4
      my_layer  = self.shapeLayer(i)

      # --- CANDIDATE QUERY (two modes, §2.2) ---
      candidates: Vec<TreeEntry> =
          tree.overlappingTreeEntriesWithClearance(my_shape, my_layer,
                                                   ignoreNetNos = [],   # empty!
                                                   self.clearanceClassIndex)

      for entry in candidates:
          if !(entry.object is Item) or entry.object == self: continue
          other = entry.object as Item

          # --- OBSTACLE PREDICATE, evaluated on the OTHER item (§2.3) ---
          is_obstacle = other.isObstacle(self)

          # --- TIE-PIN EXEMPTION (trace vs trace only, §2.4) ---
          if is_obstacle and self is Trace and other is Trace:
              ... see §2.4; may clear is_obstacle

          if !is_obstacle: continue

          shape1 = my_shape
          shape2 = other.getTileShape(entry.shapeIndexInObject)
          if shape1 == null or shape2 == null: warn; continue

          # --- REQUIRED CLEARANCE (no safety margin here) ---
          min_clearance = matrix.getValue(other.clearanceClassIndex,
                                          self.clearanceClassIndex,
                                          my_layer, addSafetyMargin = false)

          # --- SPLIT THE CLEARANCE BETWEEN THE TWO SHAPES ---
          if tree.isClearanceCompensationUsed():                     # GUI only
              c1 = tree.clearanceCompensationValue(self.clearanceClassIndex,  my_layer)
              c2 = tree.clearanceCompensationValue(other.clearanceClassIndex, my_layer)
          else:                                                      # headless: always this
              c1 = round(0.5 * min_clearance)
              c2 = min_clearance - c1                                # c1 + c2 == min_clearance

          e1 = c1 > 0 ? shape1.enlarge(c1) : shape1
          e2 = c2 > 0 ? shape2.enlarge(c2) : shape2
          inter = e1.intersection(e2)

          if inter.dimension() == 2:                                 # STRICT overlap only
              actual = calculate_clearance_between_two_shapes(shape1, shape2,
                                                              min_clearance, c1, c2)
              if self.smallestClearance < 0 or actual < self.smallestClearance:
                  self.smallestClearance = actual                    # sticky field, §7.6
              result.push(ClearanceViolation{ firstItem: self, secondItem: other,
                                              shape: inter, layer: my_layer,
                                              expectedClearance: min_clearance,
                                              actualClearance: actual })
  return result
```

The v19 body (`Item.java:313-415`) is **line-for-line identical** including the 16-iteration
binary search; only identifiers differ.

### 2.2 The candidate query — two modes

`ShapeSearchTree.overlappingTreeEntriesWithClearance(shape, layer, ignoreNetNos, cc)`
(ShapeSearchTree.java:514-524) dispatches:

* **Compensated tree (GUI)**: falls through to plain `overlappingTreeEntries` — shapes already
  carry their clearance inflation, so plain overlap is the test.
* **Uncompensated tree (headless — the benchmark path)**: the real routine at :443-507.

```
overlappingTreeEntriesWithClearance(shape, layer, ignoreNetNos, cc, out):
  bounds     = shape.boundingShape(boundingDirections)          # 45° regular octagon
  if bounds == null: bounds = board.boundingBox                 # + warn
  maxClearance = (int)(1.2 * matrix.maxValue(cc, layer))        # 1.2 fudge for asymmetric offset
  offsetBounds = bounds.offset(maxClearance)
  leaves = tree.overlaps(offsetBounds)                          # R-tree box query

  sorted = TreeSet<EntrySortedByClearance>                      # ordered by clearance, then entryId
  for leaf in leaves:
      it = leaf.object as Item
      if layer >= 0 and it.shapeLayer(leaf.shapeIndexInObject) != layer: skip
      if any(n in ignoreNetNos where !it.isObstacle(n)): skip   # ignoreNetNos == [] for DRC
      clearance = matrix.getValue(cc, it.clearanceClassIndex, layer, addSafetyMargin = TRUE)
      sorted.insert(EntrySortedByClearance{leaf, clearance})

  currentHalf = 0; currentOffsetShape = shape
  for e in sorted:                        # ascending clearance ⇒ enlarge `shape` at most once
      half = e.clearance / 2              # integer division
      if half != currentHalf: currentHalf = half; currentOffsetShape = shape.enlarge(half)
      otherShape = e.leaf.object.getTreeShape(tree, e.leaf.shapeIndexInObject)
      if currentOffsetShape.intersects(otherShape.enlarge(half)):
          out.push(TreeEntry(e.leaf.object, e.leaf.shapeIndexInObject))
```

Key points:

* The candidate test uses `getValue(..., addSafetyMargin = **true**)` → `clearance + 16`, and
  `intersects()` (any non-empty intersection, including 1-D touching). The final test in
  `clearanceViolations` uses `addSafetyMargin = false` and `dimension() == 2`. So the query
  **over-generates** by 16 board units plus the touching cases, and the caller re-tests. A Rust
  checker may use any candidate generator as long as it is a superset of
  `gap_octagon < matrix[cc_a][cc_b][layer]`.
* **The layer filter is exact**: a candidate is dropped unless `other.shapeLayer(idx) == layer`
  of *this* shape. Vias/pins therefore produce one violation per shared layer.
* `EntrySortedByClearance.compareTo` breaks ties by a global monotonically increasing
  `entryId` (ShapeSearchTree.java:…, `lastGeneratedEntryId`) — a **static mutable counter**,
  so query result *order* is not reproducible across runs; the *set* is.
* `ignoreNetNos` is `new int[0]` from DRC — **no net is excluded at the index level**. Same-net
  exclusion happens entirely in the obstacle predicate.

### 2.3 The obstacle predicate — `other.isObstacle(self)` (note the direction!)

`Item.isObstacle(Item)` is **abstract and asymmetric**, and DRC evaluates it on the *candidate*
item with *this* as the argument. Full table (v2; v19 identical except the Pin row):

| `other` (receiver) | `other.isObstacle(self)` returns |
|---|---|
| `Trace` | `false` if `self == other`, `self is ViaObstacleArea`, `self is ComponentObstacleArea`, or (`self is ConductionArea` with `isObstacle == false`); else `!self.sharesNet(other)` (Trace.java:92-102) |
| `Via` | `false` if `self == other` or `self is ComponentObstacleArea`, or `self is ConductionArea` with `isObstacle == false`; `true` if `!self.sharesNet(other)`; `false` if `self is Trace`; else `!other.attachAllowed \|\| !(self is Pin) \|\| !((Pin)self).drillAllowed()` (Via.java:152-166) |
| `Pin` | `false` if `self == other` or `self is ObstacleArea` (**incl. `ConductionArea`, which extends it**); `true` if `!self.sharesNet(other)`; `false` if `self is Trace`; else `!other.drillAllowed() \|\| !(self is Via)` (Pin.java:353-365). **v19 additionally required `\|\| !((Via)self).attach_allowed`** |
| `ObstacleArea` (plain keepout) | `false` if nets shared; else `self is Trace \|\| self is Via` (ObstacleArea.java:175-180) |
| `ViaObstacleArea` | `false` if nets shared; else `self is Via` (ViaObstacleArea.java:92-97) |
| `ComponentObstacleArea` | `self != other && self is ComponentObstacleArea && self.componentId != other.componentId` (ComponentObstacleArea.java:64-69) |
| `ConductionArea` | if `other.isObstacle` flag set → the `ObstacleArea` rule above; else `false` (ConductionArea.java:380-386) |
| `ComponentOutline` | always `false` (ComponentOutline.java:120-122) — and it has 0 tree shapes anyway |
| `BoardOutline` | `!(self is BoardOutline \|\| self is ObstacleArea)` (BoardOutline.java:84-87) — so the outline clears traces, vias, pins and conduction areas, but not other keepouts |

`sharesNet(other)` = any common entry in the two `int[] netNumbers` arrays (Item.java:180-189,
O(n·m) nested loop over usually-length-1 arrays).

**Same-net exclusion summary.** Two items sharing at least one net are never a violation, except:

* `Pin` vs `Via` on the same net when the pin is a through-hole pin (`drillAllowed()` is
  `firstLayer() == lastLayer()`, i.e. only SMD pins allow drills — Pin.java:348-350). So a via
  landing on a same-net **through-hole** pin **is** a violation; landing on a same-net **SMD** pad
  is not.
* `Via` vs `Pin` symmetric case adds `attachAllowed` (a per-via DSN flag).
* Keepouts (`ObstacleArea` family) vs foreign-net traces/vias.
* `ComponentObstacleArea` vs `ComponentObstacleArea` of a *different* component — net-independent.
* `BoardOutline` vs everything except outlines/keepouts — net-independent (the outline has no net).

Because the predicate is asymmetric and the outer loop runs over **every** item, an unordered pair
{A,B} is examined twice (once as `B.isObstacle(A)`, once as `A.isObstacle(B)`) and may disagree.
The reporting layer deduplicates (§2.7) but keeps whichever direction it saw first, so
`expectedClearance`/`actualClearance` may come from either evaluation — they are identical because
`matrix.getValue` is symmetric in practice.

### 2.4 The tie-pin exemption (trace ↔ trace, different nets)

Only when `self is Trace && other is Trace && is_obstacle` (Item.java:383-413):

```
contact_point = self.firstCorner()
contacts      = self.getNormalContacts(contact_point, ignoreNet = true)   # NOTE: ignoreNet
found         = contacts.contains(other)
if !found:
    contact_point = self.lastCorner()
    contacts      = self.getNormalContacts(contact_point, ignoreNet = true)
    found         = contacts.contains(other)
if found:
    for c in contacts:
        if c is Pin and c.sharesNet(self) and c.sharesNet(other):
            is_obstacle = false; break
```

Meaning: two traces of *different* nets that both terminate at the same point, where a **multi-net
pin** ("tie pin", a pin whose `netNumbers` contains both nets) also sits at that point, are
allowed to overlap. Note the subtle bug preserved in both trees: if the other trace touches at
`firstCorner`, the pin scan uses the `firstCorner` contact list; if it touches only at
`lastCorner`, the list is re-fetched — correct — but if it touches at *both* ends only the first
list is scanned.

### 2.5 The escape-via exemption (v2 only)

`Via.clearanceViolations()` (Via.java:89-112) overrides the base method and post-filters:
if `this.isEscapeVia && this.escapeViaSmdLayer >= 0`, every violation whose `layer ==
escapeViaSmdLayer` and whose other item **shares a net** with the via is dropped. Escape vias are
inserted by the v2 fanout escalation phase inside an SMD pad's copper, so the pad↔via overlap on
that one layer would otherwise be reported (recall §2.3: same-net Pin vs Via **is** an obstacle
when the pin is through-hole; escape vias also sit under same-net traces). `isEscapeVia`/
`escapeViaSmdLayer` are propagated by `Via.copy` (:83-84). **No v19 equivalent.**

Note the asymmetry: the *pin's* own `clearanceViolations()` is not filtered, so the pair is still
reported from the pin side and survives deduplication. This is a real v2 inconsistency; a Rust
checker should decide explicitly whether to model escape vias at all.

### 2.6 `actualClearance` — a 16-step binary search (Item.java:471-493)

```
fn calculate_clearance_between_two_shapes(s1, s2, min_clearance, c1, c2) -> f64:
    if s1.intersection(s2).dimension() == 2 { return 0.0 }       # raw copper overlaps
    low = 0.0; high = min_clearance
    sum = c1 + c2
    f1 = sum > 0 ? c1 / sum : 0.5
    f2 = sum > 0 ? c2 / sum : 0.5
    for _ in 0..16 {
        mid = (low + high) * 0.5
        if s1.enlarge(mid*f1).intersection(s2.enlarge(mid*f2)).dimension() == 2 { high = mid }
        else { low = mid }
    }
    return low                                                   # largest known-safe gap
```

Properties:

* Result is in **board units**, in `[0, min_clearance]`, resolved to `min_clearance / 2^16`
  in principle but really to ~1 board unit because `enlarge` rounds (§1.5).
* It is a *lower bound* on the true octagon gap, clamped at `min_clearance` — a pair that is
  farther apart than the requirement can never be reported anyway (the outer test failed).
* Cost: **32 shape enlargements + 16 half-plane intersections per violation**. On a board with
  many violations this dominates DRC time.
* `expectedClearance − actualClearance` is the "severity" used for sorting
  (ClearanceViolation.java:69-74) and for the µm min/max/avg statistics
  (BoardStatistics.java:344-356, `shortfall = max(0, expected − actual)`).

### 2.7 Whole-board aggregation and deduplication

`DesignRulesChecker.getAllClearanceViolations()` (DesignRulesChecker.java:53-84):

```
seen: HashSet<String> = {}
all  = []
for item in board.getItems():                   # ALL items, in itemList order (insertion order)
    for v in item.clearanceViolations():
        key = format!("{}-{}-{}", min(id1,id2), max(id1,id2), v.layer)
        if seen.insert(key) { all.push(v) }
return all
```

* `board.getItems()` iterates the whole `UndoableObjects` item list (BoardItemRepository:
  `getItems()`), which **includes the `BoardOutline`** and all keepouts.
* The dedup key is `(min id, max id, layer)`. So:
  * a pair overlapping on 3 layers counts as **3 violations**;
  * a pair whose two traces overlap in **several distinct places on one layer** counts as
    **1 violation** (only the first-found intersection shape is kept — its `shape` and
    `actual/expectedClearance` come from an arbitrary one of the overlapping shape pairs);
  * the per-item list (`ClearanceViolation.aggregateSortedBySeverity`) is **not** deduplicated
    and therefore double-counts every pair — this is documented in the Javadoc at
    ClearanceViolation.java:52-55 and only used by the GUI overlay.

`ClearanceViolation` record (drc/ClearanceViolation.java:13-44):

```
ClearanceViolation {
    firstItem: &Item,          // always the item whose clearanceViolations() produced it
    secondItem: &Item,
    shape: ConvexShape,        // the 2-D intersection of the two ENLARGED shapes
    layer: i32,                // firstItem.shapeLayer(i)
    expectedClearance: f64,    // matrix value, no safety margin
    actualClearance: f64,      // binary-search lower bound
}
```

---

## 3. How "0 violations" in the batch log is computed

### 3.1 The log line

```
FRLogger.formatScore(score, incomplete, violations)  ->  "123.4 (7 unrouted and 0 violations)"
                                                          (logger/FRLogger.java:78-89)
```

Call sites (all pass `BoardStatistics.connections.incompleteCount` and
`BoardStatistics.clearanceViolations.totalCount`):

| Site | When |
|---|---|
| `management/jobs/RoutingJobSchedulerActionThread.java:148` | once, at end of the routing stage — the headline "final score" line |
| `autoroute/pipeline/AutorouteBatchLoop.java:337, 360, 539, 544` | stagnation report; best-board restore |
| `autoroute/pipeline/AutoroutePassRunner.java:118` | per parallel router thread, per pass |
| `autoroute/pipeline/BatchOptimizer.java:144, 263, 265, 380` | optimizer before/after |
| `gui/workspace/progress/GuiRoutingJobWorker.java:381, 393, 448` | GUI |

### 3.2 It is a **full-board recompute**, every time

`BoardStatistics(BasicBoard board)` → `BoardStatistics(board, null, true, true)`
(BoardStatistics.java:83-84). Inside (BoardStatistics.java:105-…):

```
if includeConnections:                                             # :265-272
    drc = DesignRulesChecker(board, null)
    drc.calculateAllIncompletes()                                  # rebuild EVERY net's ratsnest
    connections.maximumCount   = drc.maxConnections
    connections.incompleteCount = drc.getIncompleteCount()

if includeClearanceViolations:                                     # :338-367
    drc2 = DesignRulesChecker(board, null)
    list = drc2.getAllClearanceViolations()                        # FULL-BOARD DRC, deduped
    clearanceViolations.totalCount = list.size()
    ... min/max/avg shortfall in µm
```

There is **no incremental clearance cache anywhere.** Every `new BoardStatistics(board)`:

* walks every item, and for every tile shape of every item runs an R-tree range query enlarged by
  `1.2 · maxClearance`, then a sorted-by-clearance pairwise enlarge/intersect;
* runs the binary search 16× per surviving pair;
* separately rebuilds all `NetIncompletes`, which runs a **Delaunay triangulation per net** plus a
  connected-set closure per net.

Two convenience constructors let callers skip parts:

| Constructor | clearance DRC | incompletes |
|---|---|---|
| `BoardStatistics(board)` | ✔ full | ✔ full |
| `BoardStatistics(board, unit, false)` | ✘ (all fields 0) | ✔ full |
| `BoardStatistics(board, unit, false, false)` | ✘ | ✘ |

Usage in the batch loop (`batch-orchestration.md` §1 has the loop skeleton):

* `AutorouteBatchLoop.java:283` — `new BoardStatistics(router.board)` **before** each pass (full).
* `:295` — `new BoardStatistics(router.board)` **after** each pass (full).
* `BoardHistory.add(board)` → `new BoardStatistics(board).getNormalizedScore(...)`
  (autoroute/BoardHistory.java:56) — full, once per pass, plus again at `:198`.
* `AutoroutePassRunner.java:170, 500` — `BoardStatistics(board, null, false)`: skips clearance,
  still recomputes all incompletes, every `PROGRESS_STATISTICS_ITEM_INTERVAL` routed items.
* `AutoroutePassRunner.java:188`, `BatchAutorouterThread.java:305/413/434`,
  `BatchAutorouter.calculateIncompleteCount` (:556-565) — bare
  `DesignRulesChecker.calculateAllIncompletes()` for the progress counter.

So on a typical run the **whole-board DRC runs ≥ 3× per pass** and the **whole-board ratsnest runs
≥ 6× per pass**, plus once per progress tick. `BatchAutorouter` instruments this
(`profileIncompleteDrcNanos`, `profileBoardStatisticsNanos`, `profileIntermediateStatisticsNanos`)
because it is a known hot spot.

### 3.3 The one *incremental* DRC: strict-DRC mode

`RouterSettings.strictDrc` (default **false**, DefaultSettings.java:110). When enabled,
`AutorouteConnectionRouter` snapshots the board before each connection
(`router.board.serialize(false)`, :84-85) and afterwards calls
`BatchAutorouter.enforceStrictDrc(board, routeNetNo, maxItemIdBefore)` (BatchAutorouter.java:305-329):

```
newItems = [ i for i in board.getConnectableItems(routeNetNo)
             if i.id > maxItemIdBefore and (i is Trace or i is Via) ]
if any(i.clearanceViolations() non-empty for i in newItems):
     board.removeItems(newItems)
     return FAILED("strict_drc: connection ripped because N new item(s) included clearance violations")
return null    # keep
```

i.e. per-item DRC restricted to the items just inserted, and the whole connection is rolled back
if *any* of them has *any* violation. This is the only place where DRC feeds back into routing
decisions. With `strictDrc = false` (benchmark default) the router happily creates violations and
they only show up in the score.

### 3.4 The score formula (what a violation is worth)

`BoardStatistics.calculateScore` (:594-616) and `getNormalizedScore` (:618-635):

```
maximumScore = connections.maximumCount * unroutedNetPenalty
penalties    = incompleteCount * unroutedNetPenalty
             + clearanceViolationCount * clearanceViolationPenalty
             + bendCount * bendPenalty
costs        = traceLengthMm * defaultPreferredDirectionTraceCost + viaCount * viaCosts
score        = maximumScore - penalties - costs
normalized   = maximumScore <= 0 ? 0 : max(0, score / maximumScore) * 1000
```

Defaults (`settings/sources/DefaultSettings.java`):

| Constant | Value | Line |
|---|---|---|
| `DEFAULT_UNROUTED_NET_PENALTY` | `5_000_000.0f` | :27 |
| `DEFAULT_CLEARANCE_VIOLATION_PENALTY` | `1_000_000.0f` | :33 |
| `DEFAULT_BEND_PENALTY` | `10.0f` | :39 |
| `DEFAULT_VIA_COSTS` | `50` | :45 |
| `DEFAULT_PLANE_VIA_COSTS` | `5` | :51 |
| `DEFAULT_START_RIPUP_COSTS` | `100` | :59 |
| `DEFAULT_PREFERRED_DIRECTION_TRACE_COST` | `1.0` | :66 |
| `DEFAULT_UNDESIRED_DIRECTION_TRACE_COST` | `1.0` | :74 |
| `DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM` | `500.0` | :77 |
| `DEFAULT_HOLE_CLEARANCE_UM` | `0.0` | :79 |

v19's hard-coded weights differed: unrouted `1_000_000`, clearance `1_000`, bend `100`, trace `1`,
via `50` (v19 BoardStatistics.java:423-427). **v2 made a clearance violation 1000× more
expensive relative to a bend and 5× cheaper than an unrouted net; in v19 it was 1000× cheaper than
an unrouted net.** Any score comparison across versions is meaningless.

`connections.maximumCount` also differs — §4.6.

---

## 4. Connectivity, connected sets, and the ratsnest

### 4.1 The `Connectable` contract (board/model/items/Connectable.java)

Implemented by `Trace` (→ `PolylineTrace`), `DrillItem` (→ `Pin`, `Via`), and `ConductionArea`.

```
containsNet(netNumber) -> bool
sharesNetNo(&[i32])    -> bool
getAllContacts()       -> Set<Item>          // ANY overlap + shared net
getAllContacts(layer)  -> Set<Item>
getNormalContacts()    -> Set<Item>          // overlap AT A CONNECTION POINT + shared net
getConnectedSet(net)   -> Set<Item>          // transitive closure over normal contacts
getTraceConnectionShape(tree, index) -> TileShape
```

`Item.isConnectable() == (this is Connectable) && netCount() > 0` (Item.java:869-871).

### 4.2 `getAllContacts` — the loose predicate (Item.java:499-548)

For every own tile shape, `board.overlappingObjects(shape, shapeLayer(i))` (exact tile-shape
intersection on that layer, via the default tree), keep every result that is a different `Item`,
is `Connectable`, and `sharesNet(this)`. Used by `isConnected()` / `isConnectedOnLayer(layer)`
only — **not** by the ratsnest.

### 4.3 `getNormalContacts` — the *strict* connection predicate

This is the predicate that defines electrical connectivity in Freerouting. It is **exact-point
based**, not overlap based. Three implementations:

**`Trace.getNormalContacts()` (Trace.java:155-166)** = union of
`getNormalContacts(firstCorner(), false)` and `getNormalContacts(lastCorner(), false)`, where
(Trace.java:173-203):

```
getNormalContacts(point, ignoreNet):
  if point == null or (point != firstCorner() and point != lastCorner()): return {}
  searchShape = TileShape::from_point(point)                 # degenerate 0-area tile
  overlaps    = board.overlappingObjects(searchShape, this.layer)   # layer-restricted
  for o in overlaps where o is Item, o != this, o.sharesLayer(this),
                          (ignoreNet or o.sharesNet(this)):
      match o:
        Trace(t)      => if point == t.firstCorner() or point == t.lastCorner() { add }
        DrillItem(d)  => if point == d.getCenter()                              { add }
        ConductionArea(a) => if a.getArea().contains(point)                     { add }
        _             => (nothing — keepouts/outline are never contacts)
```

**`DrillItem.getNormalContacts()` (DrillItem.java:274-305)** — the same, anchored at
`getCenter()`, but queried with **`layer = -1` (all layers)** and then filtered by
`sharesLayer(this)`:

```
drillCenter = getCenter()
overlaps    = board.overlappingObjects(TileShape::from_point(drillCenter), -1)
for o in overlaps where o != this, o.sharesNet(this), o.sharesLayer(this):
    Trace(t)          => drillCenter == t.firstCorner() || drillCenter == t.lastCorner()
    DrillItem(d)      => drillCenter == d.getCenter()
    ConductionArea(a) => a.getArea().contains(drillCenter)
```

**`ConductionArea.getNormalContacts()` (ConductionArea.java:329-355)** — per own tile shape,
query that shape on the area's layer; a `Trace` connects if the **tile shape contains** either of
its end corners; a `DrillItem` connects if the tile shape contains its centre.

Everything else inherits `Item.getNormalContacts()` → **empty set** (Item.java:569-571).

**Invariants a Rust port must reproduce exactly:**

1. **Exact integer point equality.** `Point.equals` on `IntPoint` is coordinate equality. There is
   *no* tolerance. v2 explicitly removed a tolerance-based variant — the dead
   `DrillItem.isWithinTolerance` (Manhattan, DrillItem.java:311-324) remains unused, with a
   comment explaining that tolerance caused false cycle detection during trace normalisation.
2. **Traces connect only at their two end corners.** A trace crossing another trace mid-span is
   *not* connected, even on the same net and same layer. (The router keeps traces split at
   junctions so this is normally fine; a Rust checker importing a foreign SES must split first.)
3. **Symmetry is *not* structural but does hold** for the Trace↔Trace, Trace↔Drill and
   Drill↔Drill cases. The Trace↔ConductionArea case is *asymmetric*: the trace side tests
   `area.getArea().contains(point)` (the true polygon-with-holes area) while the area side tests
   `tileShape.contains(point)` (one convex piece of the *tree* decomposition). These can disagree
   at hole boundaries. `getConnectedSet` walks contacts in one direction only, so the closure can
   depend on the start item.
4. Layer overlap is `sharesLayer` = `max(firstLayer) <= min(lastLayer)` (Item.java:314-318).

### 4.4 `getConnectedSet` — the closure (Item.java:596-635)

```
getConnectedSet(netNumber, stopAtPlane = false) -> Set<Item>:
    if netNumber > 0 and !containsNet(netNumber): return {}
    result = {self}
    dfs(result, netNumber, stopAtPlane)
    return result

dfs(result, netNumber, stopAtPlane):
    for c in self.getNormalContacts():                 # RECOMPUTED at every node, no cache
        if stopAtPlane and c is ConductionArea and c.componentId <= 0: continue
        if netNumber > 0 and !c.containsNet(netNumber): continue
        if result.insert(c) { c.dfs(result, netNumber, stopAtPlane) }
```

* Backed by a `TreeSet<Item>` ordered by `Item.compareTo` = `other.id − this.id`, i.e.
  **descending id**. Iteration order is therefore deterministic given ids.
* `stopAtPlane` is used by the router (to avoid pulling an entire power plane into the start set);
  the DRC/ratsnest path always uses `stopAtPlane = false`.
* **Recursion depth = length of the trace chain.** On a long serpentine net this is a real stack
  risk; both trees use plain recursion.
* Cost: one point-query into the R-tree per node visit, and the *same* node's contacts are
  recomputed every time it is reached (no memoisation). See §6.

`getUnconnectedSet(netNumber)` (Item.java:676-690) = all `Connectable` items containing the net
(`board.getConnectableItems`, a full item-list scan — BoardConnectivityQueries.java:23-35) minus
`getConnectedSet(netNumber)`. Used by the router to decide "is this item already routed?"
(`RoutingBoard.autoroute` :940-943 → `ALREADY_CONNECTED` when empty; `fanout` :997-1000 →
`NO_UNCONNECTED_NETS`) and by the SMD-fanout statistic (BoardStatistics.java:416).

`board.getConnectedSets(netNumber)` (BoardConnectivityQueries.java:99-124) partitions a net into
its connected components: collect all connectable items of the net into a `TreeSet`, repeatedly
take the first remaining item, take its `getConnectedSet`, emit it, and `removeAll`.

### 4.5 Tails, cycles and stubs

* `Trace.isTail()` — `startContacts.isEmpty() || endContacts.isEmpty()` (Trace.java:212-219).
* `Via.isTail()` — `true` if it has ≤ 1 normal contact, **or** if every contact has the identical
  `(firstLayer, lastLayer)` pair (Via.java:170-187) — i.e. the via does not actually change
  layers.
* `Item.isTail()` default `false` (Pin, ConductionArea).
* `Trace.isOverlap()` — start and end contact sets intersect (Trace.java:228-232).
* `Trace.isCycle()` — `isOverlap()`, or a DFS from the start contacts (with the start contacts
  pre-seeded as visited) reaches this trace again; `NetClass.getIgnoreCyclesWithAreas()` can make
  the DFS skip `ConductionArea` nodes (Trace.java:272-330, `Item.isCycleRecu` :646-669).
* `Item.getConnectionItems(stopOption)` (Item.java:693-781) walks from an item along *unique*
  contacts until a fork, a non-routable item, or (per `StopConnectionOption`) a via / fanout via.
  Used by `removeTails`, not by DRC. Its uniqueness test is `normalContactPoint(other)`:
  * `Trace.normalContactPoint(Trace)` — same layer, and exactly one of {first↔first/last,
    last↔first/last} matches; returns null if zero or both (Trace.java:131-152).
  * `DrillItem.normalContactPoint(Trace/DrillItem)` — shared layer + exact centre/endpoint
    equality (DrillItem.java:331-349).
  * Base `Item.normalContactPoint` returns null → conduction areas never yield a unique point.

### 4.6 `NetIncompletes` — the per-net ratsnest (drc/NetIncompletes.java)

```
NetIncompletes {
    incompletes:        Vec<AirLine>,   // the result
    net:                &Net,
    drawMarkerRadius:   f64,            // board.rules.getMinTraceHalfWidth() * 2
    lengthViolation:    f64,            // >0 too long, <0 too short, 0 ok
    connectedGroupCount: i32,
}
AirLine { net, fromItem, fromCorner: FloatPoint, toItem, toCorner: FloatPoint }
   // compareTo == net.name.compareTo(other.net.name)  — NOT a total order!
```

Construction (`NetIncompletes(netNumber, netItems, board)`, :57-226):

**Step 1 — filter (v2 only; v19 has no filter).** For each item of the net (:85-116):

```
if item.isTail()                                        { drop }   # dangling trace / non-transiting via
if !(item is ConductionArea) && !(item is DrillItem)
   && item.getNormalContacts().isEmpty()                { drop }   # isolated non-drill item
keep
```

Rationale in the source: dangling items are *DRC violations*, not *unrouted connections*.

**A load-bearing consequence:** a surviving `Trace` has both ends contacted, and
`Trace.getRatsnestCorners()` (Trace.java:338-368) returns **only uncontacted endpoints** — so a
surviving trace contributes a **zero-length corner array**. Therefore in v2 **traces are never
ratsnest nodes at all**. The triangulation node set is exactly:

```
{ all Pins of the net }  ∪  { non-tail Vias }  ∪  { ConductionAreas }
```

with corners:

| Item | `getRatsnestCorners()` |
|---|---|
| `DrillItem` (Pin/Via) | `[center]` (DrillItem.java:352-356) |
| `ConductionArea` | every corner of `getArea().cornerApproxArr()` rounded to `IntPoint` (ConductionArea.java:365-374) — can be **hundreds of corners**, the dominant cost on plane nets |
| `Trace` | uncontacted endpoints only (0, 1 or 2) — always 0 after the v2 filter |
| anything else | `[]` (Item.java:792-794) |

**Step 2 — group into connected sets** (`calculateNetItems`, :293-322):

```
unique = HashSet(filteredItems)
result: Vec<NetItem> = []
while !unique.is_empty():
    start = unique.iter().next()                       # HashSet order — see §7.3
    set   = start.getConnectedSet(net.netNumber)       # full closure, includes filtered-out items
    inComponent = [i for i in set if unique.contains(i)]
    for i in inComponent: result.push(NetItem{ item: i, connectedSet: set })   # SHARED Collection ref
    unique.removeAll(inComponent)
```

Note `connectedSet` is stored **by reference**: all members of a component share the same
`Collection<Item>` object, and identity (`==`) is the union-find "find" operation.
`connectedGroupCount` = number of distinct `connectedSet` identities (:137-141).

If `groupedNetItems.len() <= 1` → return with zero airlines (:154-163).

**Step 3 — Delaunay triangulation** (`datastructures/PlanarDelaunayTriangulation`):

* Randomised incremental construction (de Berg et al., *Computational Geometry* §9.3) with a
  **fixed seed 99** on a `static Random`, re-seeded per construction
  (PlanarDelaunayTriangulation.java:28-30, 58-60), so results are deterministic given the input
  order.
* All corners of all `NetItem`s are flattened into one list, shuffled, then inserted into a
  bounding triangle with vertices `(C,0)`, `(0,C)`, `(−C,−C)` where `C = Limits.CRIT_INT = 33 554
  432 = 2^25` (:62-68).
* Point location by a DAG of historical triangles (`TriangleGraph`); insertion splits into 3 (or
  2+2 on an edge) and calls `legalizeEdge` recursively.
* **Duplicate corners** (two items at the exact same coordinate) hit the "corner equals a corner
  of the triangle" branch (:145-158): if the two objects differ, a `degenerateEdge` of length 0 is
  emitted; nothing is split. These zero-length edges sort first and are what connects
  co-located pins.
* Legality test `Edge.isLegal()` (:469-485) is the **floating-point in-circle predicate**
  (`FloatPoint.insideCircle`) — not exact arithmetic. Degenerate/co-circular inputs can produce a
  non-Delaunay but still-planar triangulation. Harmless for a ratsnest; a Rust port using exact
  predicates will occasionally produce a *different but equally valid* airline set.
* `getEdgeLines()` returns `ResultEdge{startPoint, startObject, endPoint, endObject}` for the
  degenerate edges plus every leaf triangle's edges (`TreeSet<Edge>`, so deduplicated).

**Step 4 — Kruskal over the triangulation edges** (:171-205):

```
sortedEdges: BTreeSet<Edge>                         # Edge::compareTo, §below
for e in triangulationLines:
    sortedEdges.insert(Edge{ fromItem, fromCorner, toItem, toCorner,
                             lengthSquare: toCorner.distanceSquare(fromCorner) })

for e in sortedEdges (ascending):
    if e.fromItem.connectedSet == e.toItem.connectedSet { continue }   # identity compare
    incompletes.push(AirLine{ net, e.fromItem.item, e.fromCorner,
                                   e.toItem.item,   e.toCorner })
    joinConnectedSets(groupedNetItems, e.fromItem.connectedSet, e.toItem.connectedSet)
```

`joinConnectedSets(items, from, to)` (:328-337) is an **O(n) relabel**: every `NetItem` whose
`connectedSet` is identity-equal to `from` is added to `to` and repointed. No rank/path
compression → the whole merge loop is **O(E · N)** worst case.

`Edge::compareTo` (:361-379): compare `lengthSquare`; on tie compare `fromCorner.x`, then
`fromCorner.y`, then `toCorner.x`, then `toCorner.y`; result through `Signum.asInt`. The tie-break
is mandatory — a `TreeSet` silently drops elements that compare equal, and coincident-length edges
are common on grid-aligned boards.

**Result**: `incompletes.len()` == (number of connected groups reachable through the Delaunay
graph) − 1. If the Delaunay graph does not connect two groups (possible after node contraction —
Kruskal over Delaunay is the exact EMST only for a *point set*, not for a contracted graph), the
count under-reports. In practice this is rare.

**Step 5 — length violation** (`calcLengthViolation`, :257-275):

```
maxLength = net.netClass.maximumTraceLength;  minLength = net.netClass.minimumTraceLength
if maxLength <= 0 && minLength <= 0 { lengthViolation = 0; return false }
traceLength = net.getTraceLength()        # sum of Trace.getLength() over all net items
v = 0
if maxLength > 0 && traceLength > maxLength { v = traceLength - maxLength }
if minLength > 0 && traceLength < minLength && incompletes.is_empty() { v = traceLength - minLength }
changed = |v - old| > 0.1
```

### 4.7 `DesignRulesChecker` — the ratsnest service (drc/DesignRulesChecker.java)

```
DesignRulesChecker {
    board: &BasicBoard,
    drcSettings: Option<DesignRulesCheckerSettings>,     // null in every internal call site
    maxConnections: i32,
    netIncompletes: Option<Vec<NetIncompletes>>,         // index i == net (i+1)
}
```

`calculateAllIncompletes()` (:542-623):

```
maxNetNo = board.rules.nets.maxNetNumber()
netItemLists = vec![Vec::new(); maxNetNo]
for item in board.itemList:                      # raw UndoableObjects walk
    if item is Connectable:
        for k in 0..item.netCount(): netItemLists[item.getNetNumber(k) - 1].push(item)

maxConnections = Σ over non-empty netItemLists of
                    max(0, count(items that are Pin or ConductionArea) - 1)

netIncompletes = (1..=maxNetNo).map(|n| NetIncompletes(n, netItemLists[n-1], board))
```

The `maxConnections` formula is a **v2 rewrite** (comment at :562-566 explains that the old
`total_items − netCount` formula produced values that were too small and could be ≤ 0, forcing
`getNormalizedScore()` to always return 0). v19 used `max(0, pins.size() − nets.totalCount)`
then `max(that, incompleteCount)` (v19 BoardStatistics.java:266-271).

`getIncompleteCount()` = Σ `netIncompletes[i].count()`. `getIncompleteCount(net)` = per net.
`getAllAirlines()` (:780-795) flattens all `incompletes` into an array sized by
`getIncompleteCount()`.

Incremental API (used by the GUI façade only): `recalculateNetIncompletes(netNumber)` refetches
`board.getConnectableItems(netNumber)` and rebuilds that one net (:630-639);
`recalculateNetIncompletes(netNumber, itemList)` takes a caller-supplied list, **copying it**
because the v19 constructor mutated its argument (:647-660 — the copy is now vestigial since v2's
constructor builds its own `filteredItems`).

`recalculateLengthViolations()` (:765-777) re-runs `calcLengthViolation` on every net and returns
whether any changed. `getLengthViolationCount()` counts nets with non-zero violation.

`gui/workspace/progress/RatsNest` is a pure delegation wrapper around this plus a `boolean[]
isFiltered` per net and a `hidden` flag.

### 4.8 `UnconnectedItems` and the KiCad DRC report

`DesignRulesChecker.getAllUnconnectedItems()` (:91-178) is a **different, independent** notion of
"unconnected" used only by the exported DRC report — it is *not* the ratsnest:

```
# 1. Per-net disconnection (one record per net, not per missing link)
group items by netNumber (first net only: item.getNetNumber(0)), Connectable && netCount>0
for each net with >= 2 items:
    partition into connected sets via item.getConnectedSet(netNumber) ∩ netItems
    if #sets >= 2:
        rep1 = representative(set0); rep2 = representative(set1)   # prefer Pin, then Trace, then any
        emit UnconnectedItems{ rep1, rep2, allItems = set0 ∪ set1, type = "unconnectedItems" }

# 2. Dangling traces
for trace in board.getItems():
    if trace.getStartContacts().isEmpty() || trace.getEndContacts().isEmpty():
        if no existing record has firstItem == trace:
            emit UnconnectedItems{ trace, null, type = "track_dangling" }

# 3. Dangling vias
for via in board.getItems():
    if via.isTail(): emit UnconnectedItems{ via, null, type = "via_dangling" }
```

Note the **quadratic scan** at :160 (`unconnectedItems.stream().anyMatch(...)` inside the trace
loop) and the fact that only *two* of a net's groups are ever reported.

`generateReport(sourceFile, coordinateUnit)` (:210-290) emits a KiCad `drc.v1.json` document:

```
{ "$schema": "https://schemas.kicad.org/drc.v1.json", "coordinateUnits", "date",
  "kicadVersion": "N/A", "freeroutingVersion", "source",
  "violations": [ KiCadDrcViolation ],           # clearance + track_dangling + via_dangling
  "unconnectedItems": [ KiCadDrcViolation ],     # type "unconnectedItems" only
  "schematicParity": [], "qualityScore": <normalized score> }

KiCadDrcViolation { type, description, severity, items: [ {description, pos:{x,y}, uuid} ] }
```

* `type` is `"holeClearance"` when either item is a `Via` **or a `Pin`** (`isHole`, :424-431 —
  the code admits this wrongly includes SMD pads), else `"clearance"`.
* `severity` is `"error"` for clearance, `"warning"` for the unconnected/dangling kinds.
* `uuid` is the decimal item id; `pos` is the item's **bounding-box centre of gravity**, converted
  through `board.communication.coordinateTransform.boardToDsn` then `Unit.scale` to the requested
  unit (`convertCoordinate`, :504-533).

CLI path: `Freerouting.java:332-352` builds a `DesignRulesChecker` on a board optionally
overlaid with an imported SES file, generates the report, attaches
`board.getStatistics().getNormalizedScore(routerSettings.scoring)` as `qualityScore`.
API path: `api/v1/JobOutputResource.java:645-654`.

### 4.9 `AutorouteAirlineCalculator` (autoroute/pipeline, v2 only)

Progress/diagnostic geometry, **not** part of correctness. Only `calculateAirline` is reachable
(`AutorouteConnectionRouter.java:70`):

```
calculateAirline(fromItems, toItems) -> FloatLine:
    min over all pairs of DrillItems (a ∈ from, b ∈ to) of distanceSquare(a.center, b.center)
    returns FloatLine(bestFrom, bestTo)     # both null if either side has no DrillItem
```

Note it silently returns `FloatLine(null, null)` when no drill items exist; `BatchAutorouter`
guards for that at :520-526. `nearestPointOnTrace`, `findClosestPointsBetweenTraces` and
`calculateItemDistance` are **dead code** in 2.3.0 (no callers anywhere in the tree).

---

## 5. The *router's* clearance tests are a different code path

The reporting DRC (§2) is not what the maze search / shove use. `BasicBoard` exposes three
insertion-time predicates that a Rust port must keep separate from the referee:

* `checkShape(area, layer, netNumbers, cc)` (BasicBoard.java:953-978) — every convex piece must be
  inside the board bounding box and must have no obstacle from
  `overlappingObjectsWithClearance(..., ignoreNetNos = netNumbers, cc)`. Note that here the
  *net numbers are passed as `ignoreNetNos`*, so same-net items are excluded at the index level.
* `checkTraceShape(shape, layer, netNumbers, cc, contactPins)` (:984-1040) — same query with
  `ignoreNetNos = []`, then per candidate: skip if it is one of `contactPins`; **fail** if it is
  any other `Pin` (acid-trap avoidance); otherwise obstacle iff `isTraceObstacle(net)` for all
  nets; plus a tie-pin escape where a foreign-net `PolylineTrace` is ignored if the overlap
  region is contained in a shared multi-net contact pin's shape.
* `checkPolylineTrace(polyline, layer, penHalfWidth, netNumbers, cc)` (:1044-1063) — builds a
  temporary `PolylineTrace`, computes `touchingPinsAtEndCorners()`, and runs `checkTraceShape` per
  segment.

`isTraceObstacle(net)` defaults to `!containsNet(net)` (Item.java:170-172) and is overridden:
`ConductionArea` → `isObstacle && !containsNet(net)`; `ViaObstacleArea` and
`ComponentObstacleArea` → always `false`.

These use `getValue(..., addSafetyMargin = true)` inside the tree query (i.e. **+16 board units**)
and never compute an actual gap. So the router is *conservative by 16 units* relative to the
referee — which is why a clean route can still show 0 violations while a shoved one shows some.

---

## 6. Performance characteristics (measured shape of the work)

| Operation | Complexity | Notes |
|---|---|---|
| `Item.clearanceViolations()` | `O(S · (log N + K · (Q + 16·E)))` | S = own tile shapes, K = candidates, Q = enlarge+intersect, E = enlarge cost |
| `getAllClearanceViolations()` | full-board, `O(Σ_items above)` + a `HashSet<String>` of formatted keys | **string** keys — one `String` allocation per violation found, before dedup |
| `getConnectedSet` | `O(V · point-query)` with **no memoisation** | each node's `getNormalContacts` re-runs an R-tree point query; a node reached from k neighbours is queried k times |
| `NetIncompletes` per net | Delaunay `O(C log C)` expected + Kruskal-with-relabel `O(E · N)` | C = total corners (a plane net contributes hundreds) |
| `calculateAllIncompletes` | full item-list walk + per-net the above | run ≥ 6× per routing pass (§3.2) |
| `getAllUnconnectedItems` | `O(N · |net|)` + a quadratic `anyMatch` over the growing result list (:160) | report-only |
| `getUnconnectedSet` | full item-list scan per call (`getConnectableItems`) | called once per routed item by the router |

Hot allocation sites (per call, all in the inner loops):

* `TreeEntry` per candidate hit (`ShapeSearchTree.java:429, 504`).
* `EntrySortedByClearance` per candidate, into a `TreeSet` (`:486-487`).
* `LinkedList<TreeEntry>` per tile shape (`:516`).
* `TreeSet<Item>` per `getNormalContacts()` call — and one per node visit in the closure.
* `TileShape` per `enlarge` (32 per violation in the binary search) — `Simplex.enlarge` allocates
  a `Line[]` plus a bounding octagon plus the intersection result.
* `String` dedup key per violation (`DesignRulesChecker.java:70-73`).
* `LinkedList<Corner>` of every ratsnest corner, then `Collections.shuffle` on a `LinkedList`
  (which is `O(n²)`-ish because `LinkedList.set` is `O(n)` — `Collections.shuffle` dumps to an
  array first for large lists, but the list construction itself is per-corner allocation).

---

## 7. Edge cases, quirks and outright bugs to decide about

1. **Compensated-mode double counting.** When `isClearanceCompensationUsed()` is true (GUI only),
   `getTileShape()` already returns *inflated* shapes, and `clearanceViolations` inflates them
   again by `clearanceCompensationValue` (Item.java:431-443). Violations are over-reported by
   roughly one full clearance. Headless is unaffected. **Do not port the compensated branch.**
2. **Out-of-range matrix lookups return 0.** A clearance class index beyond `classCount` (e.g.
   after a class removal that missed an item) silently disables the check
   (ClearanceMatrix.java:133-161).
3. **`intersection().dimension() == 2` only.** Exactly-touching enlarged shapes are legal.
   Because offsets are integer-rounded, a pair at exactly the required clearance may land on
   either side depending on rounding direction.
4. **Multi-place overlaps on one layer collapse to one violation** because of the
   `(id,id,layer)` dedup key, but the reported `shape`/`actualClearance` come from an arbitrary
   one of them.
5. **`smallestClearance` is a sticky, never-reset mutable field** on `Item` (Item.java:47,
   451-453). `clearDerivedData()` (Item.java:1060-1065) does not clear it. It only ever decreases over a board's
   lifetime, so `ClearanceViolation.smallestClearance(items)` reports a historical minimum, not
   the current one. It also makes `clearanceViolations()` **not thread-safe** and not idempotent
   with respect to observable state.
6. **`AirLine.compareTo` compares net names only** — inconsistent with `equals`; putting airlines
   in a `TreeSet` silently loses all but one per net.
7. **`NetIncompletes` drops dangling traces and isolated non-drill items** from the ratsnest
   (v2 only). Combined with `Trace.getRatsnestCorners()` returning only free ends, **traces never
   participate in v2 ratsnest geometry at all**. A net whose only remaining gap is a dangling
   trace reports **0 incompletes** and instead surfaces as a `track_dangling` DRC warning. Any
   independent checker that counts "unrouted connections" as "components of the pin graph − 1"
   will disagree with Freerouting whenever tails exist.
8. **`getAllUnconnectedItems` groups by `getNetNumber(0)` only**, so multi-net (tie) items are
   assigned to a single arbitrary net.
9. **`ConductionArea` contributes every polygon corner** to the triangulation. On a ground plane
   this can be thousands of points and dominates ratsnest time; it also means a plane's airline
   endpoint is a polygon vertex, not the nearest point on the plane.
10. **Escape-via violation filtering is one-sided** (§2.5) — the pin side still reports it.
11. **`Via.isTail()` returns true for a via whose contacts are all on the same layer pair**, which
    includes the legitimate case of a via connecting two traces that both span the same layer
    range — for `Trace` contacts `firstLayer()==lastLayer()==layer`, so two traces on *different*
    layers give `false` (correct), two on the *same* layer give `true` (correct, the via is
    useless).
12. **Recursion**: `getConnectedSetRecu`, `isCycleRecu` and `legalizeEdge` are all unbounded
    recursion. Deep nets / degenerate triangulations can blow the stack.
13. **`Signum.asInt` on a `double` difference** is used for both the airline `Edge` order and the
    `EntrySortedByClearance` order — subtraction of large doubles can lose the sign for values
    near the float epsilon; the coordinate tie-breaks mitigate this for edges.
14. **`static int lastGeneratedEntryId`** in `ShapeSearchTree` (wraps at `Integer.MAX_VALUE`)
    makes candidate iteration order globally stateful — irrelevant to the result set, fatal to
    reproducibility of any order-dependent downstream logic.
15. **Empty-board / single-item nets**: `NetIncompletes` returns early at `groupedNetItems.len()
    <= 1`; `maxConnections` contributes 0 for nets with < 2 Pin/ConductionArea endpoints; if
    `maxConnections == 0` the normalized score is defined as `0` (BoardStatistics.java:624-631).

---

## 8. Call graph

**Into this area:**

```
Freerouting.main (--drc)                 -> DesignRulesChecker.generateReport
api/v1/JobOutputResource                 -> DesignRulesChecker.generateReportJson
core/results/RoutingResultManifest:117   -> new BoardStatistics(job.board)      [full DRC+ratsnest]
board/facade/RoutingBoard.getStatistics  -> new BoardStatistics(this)           [full DRC+ratsnest]
autoroute/BoardHistory.add / .score      -> new BoardStatistics(board)          [full]
autoroute/pipeline/AutorouteBatchLoop    -> new BoardStatistics(board) x4 per pass
autoroute/pipeline/AutoroutePassRunner   -> BoardStatistics(board,null,false) + DesignRulesChecker
autoroute/pipeline/BatchAutorouterThread -> DesignRulesChecker (x3 per pass)
autoroute/pipeline/BatchOptimizer        -> BoardStatistics (full and no-DRC variants)
autoroute/pipeline/BatchAutorouter.enforceStrictDrc -> Item.clearanceViolations  [incremental]
autoroute/pipeline/AutorouteUnroutedReport          -> DesignRulesChecker.getAllAirlines
gui/workspace/progress/RatsNest           -> DesignRulesChecker
gui/workspace/progress/ClearanceViolations-> ClearanceViolation.aggregateSortedBySeverity
board/facade/RoutingBoard.autoroute/fanout-> Item.getConnectedSet / getUnconnectedSet
```

**Out of this area:**

```
DesignRulesChecker ─┬─ Item.clearanceViolations ─┬─ ShapeSearchTree.overlappingTreeEntriesWithClearance
                    │                            ├─ ClearanceMatrix.getValue / maxValue
                    │                            ├─ TileShape.enlarge / intersection / dimension
                    │                            └─ Item.isObstacle(Item)  [per-subclass]
                    ├─ Item.getConnectedSet ───── Item.getNormalContacts ── BasicBoard.overlappingObjects
                    │                                                        └─ ShapeSearchTree.overlappingTreeEntries
                    ├─ NetIncompletes ─┬─ Item.isTail / getNormalContacts / getRatsnestCorners
                    │                  ├─ PlanarDelaunayTriangulation
                    │                  └─ Net.getTraceLength / NetClass min/max trace length
                    └─ KiCad* report DTOs + coordinateTransform.boardToDsn
```

---

## 9. v1.9 → v2 semantic deltas (what actually changed, not renames)

| # | Delta | Evidence |
|---|---|---|
| D1 | **A headless DRC service exists.** v19 had no `drc` package; violations were only reachable through the Swing `interactive.ClearanceViolations`. | v2 `drc/DesignRulesChecker.java` (new, 821 L) |
| D2 | **Ratsnest is GUI-free.** `AirLine` promoted to a top-level class without `Locale`/`ResourceBundle`; `NetIncompletes` lost its `draw`/`Graphics` methods. | v19 `interactive/RatsNest.AirLine` vs v2 `drc/AirLine.java` |
| D3 | **Incompletes now filter dangling/isolated items** before triangulation. v19 triangulated every connectable item of the net. This changes the incomplete count on any board with tails. | v2 NetIncompletes.java:76-116 (absent in v19) |
| D4 | **`maxConnections` formula rewritten** to `Σ max(0, #(Pin|ConductionArea) − 1)` per non-empty net. v19: `max(pins − netCount, incompleteCount)`. Affects every normalized score. | v2 DesignRulesChecker.java:562-578 vs v19 BoardStatistics.java:266-271 |
| D5 | **Scoring weights changed** — clearance penalty 1 000 → 1 000 000, unrouted 1 000 000 → 5 000 000, bend 100 → 10, and the weights moved from hard-coded floats to `ScoringSettings`/`DefaultSettings`. | DefaultSettings.java:27-77 vs v19 BoardStatistics.java:423-427 |
| D6 | **Strict-DRC ripup mode** (`router.strict_drc`, default off) that rolls back a connection whose new items violate clearance. No v19 equivalent. | BatchAutorouter.java:305-329, AutorouteConnectionRouter.java:84-149 |
| D7 | **Escape vias** with a per-layer same-net violation exemption. | Via.java:40-46, 89-112 |
| D8 | **Hole clearance and copper-to-edge clearance overrides** that synthesise `hole_edge`/`board_edge` clearance classes and inflate drill-item tree shapes; both feed the DRC because DRC reads the default tree's shapes. | HeadlessBoardManager.java:346-545, ShapeSearchTree.java:1004-1074 |
| D9 | **`Pin.isObstacle` dropped the `attach_allowed` term** for same-net vias over SMD pads. v19: `!drill_allowed() \|\| !(other is Via) \|\| !other.attach_allowed`; v2: `!drillAllowed() \|\| !(other is Via)`. So v2 tolerates non-attach vias on SMD pads. | Pin.java:353-365 vs v19 Pin.java |
| D10 | **KiCad JSON DRC report + `unconnectedItems`/`track_dangling`/`via_dangling` categories.** | `io/kicad/*`, DesignRulesChecker.java:91-422 |
| D11 | **Deduplication of violations moved into the DRC service.** In v19 the dedup lived (in this checkout) only inside `BoardStatistics`; the GUI list was double-counted. v2's `getAllClearanceViolations` is the canonical deduped API. | DesignRulesChecker.java:64-79 |
| D12 | `AutorouteAirlineCalculator` extracted (with three now-dead helpers). | v2 autoroute/pipeline/AutorouteAirlineCalculator.java |

Everything else — `Item.clearanceViolations` including the 16-step binary search,
`getNormalContacts` per type, `getConnectedSet`, `ClearanceMatrix`, the Delaunay triangulation,
`Edge`/`NetItem`/`joinConnectedSets` — is **byte-for-byte equivalent** modulo identifier renames.

---

## 10. Rust design implications

### 10.1 Semantics to keep exactly (these define "correct")

* **Octagon-metric offsets with integer rounding** (§1.5). If the Rust checker uses Euclidean
  distance it will disagree with Freerouting on every 45° pair. Implement `TileShape` as a
  half-plane list (`Simplex`), plus `IntBox`/`IntOctagon` fast paths, and replicate
  `offset(d) → {left−round(d), …, diag±round(√2·d)}` and
  `Line::translate(d)` with its `round()` to integers.
* **Violation ⇔ `dimension(enlarge(s1,c1) ∩ enlarge(s2,c2)) == 2`** with `c1 = round(cl/2)`,
  `c2 = cl − c1`. Not `gap < cl` computed analytically — the quantisation differs.
* **Required clearance is `matrix[other.cc][self.cc][layer]` with no safety margin**; the +16
  margin belongs to the candidate query and to the router only.
* **Per-layer accounting**: a pair overlapping on k layers is k violations; dedup key is
  `(min_id, max_id, layer)`.
* **The asymmetric `isObstacle` table (§2.3) verbatim**, including the tie-pin exemption (§2.4)
  and the `BoardOutline`/keepout special cases.
* **Connectivity is exact-point**: traces connect only at their two end corners, drills only at
  their centre, conduction areas by point-in-area. No tolerance, ever.
* **Ratsnest node set = Pins ∪ non-tail Vias ∪ ConductionAreas**, airline count = components − 1,
  Kruskal over the Delaunay edges with the exact `lengthSquare → x → y → x → y` tie-break.
* The **fixed shuffle seed 99** if bit-identical airline geometry is required; otherwise any MST
  is acceptable since only the *count* feeds the score.

### 10.2 Restructure for cache locality and arena allocation

* **Struct-of-arrays item store.** `Item` in Java is a 12-field object with two transient
  side-tables (`ItemSearchTreesInfo`, `ItemAutorouteInfo`). Model it as
  ```rust
  struct Items {
      net_ranges:  Vec<Range<u32>>,   nets: Vec<NetId>,   // flattened netNumbers
      clearance_class: Vec<u16>,      layer_lo: Vec<u8>, layer_hi: Vec<u8>,
      kind: Vec<ItemKind>,            component: Vec<u32>, fixed: Vec<FixedState>,
      shape_ranges: Vec<Range<u32>>,                       // into a flat TileShape arena
  }
  ```
  The whole DRC pass becomes a linear scan over `shape_ranges` with an index probe — no pointer
  chasing, no `instanceof`.
* **`ItemKind` as an enum + a precomputed `obstacle_matrix: [[ObstacleRule; K]; K]`.** The
  `isObstacle` dispatch is 10 virtual calls deep in Java and is executed once per candidate. In
  Rust it collapses to a table lookup on `(kind_a, kind_b)` plus at most one `shares_net` /
  `drill_allowed` / `is_obstacle_flag` test.
* **Tile shapes in one arena**, `Simplex` as `(offset, len)` into a flat `Vec<Line>`; `IntBox` and
  `IntOctagon` as inline 8×i32 variants so 90 %+ of shapes never touch the line arena.
* **Interned nets, ids as dense `u32` indices**, not sparse ids. `Item.compareTo` is
  descending-id — replace `TreeSet<Item>` with a `SmallVec<ItemId>` + sort, or a bitset for
  connected sets.
* **Connected sets as a union-find over item indices**, not `Collection<Item>` identity. This
  removes both the O(E·N) `joinConnectedSets` relabel and the `HashSet<Collection>` identity
  count. Airline computation becomes: build node array → Delaunay → sort edges → union-find →
  count merges.
* **Contact adjacency built once per DRC/ratsnest pass, not per node visit.** Java recomputes
  `getNormalContacts()` every time a node is reached. Instead: bucket all connection *points*
  (trace endpoints, drill centres) into a hash map `(layer_range, IntPoint) → SmallVec<ItemId>`
  in one linear pass, then the closure is a plain BFS over that adjacency. This turns
  `getConnectedSet` from `O(V · log N)` R-tree probes into `O(V + E)`.
* **Clearance matrix as a flat `Vec<i32>` of `class_count² · layer_count`**, row-major with the
  layer innermost (all DRC lookups fix the layer and vary the classes → keep `[i][j][layer]` if
  you also cache `max_value[class][layer]`). Assert symmetry once at load.

### 10.3 Parallelism

* **Full-board clearance DRC is embarrassingly parallel over items** *if* you remove the two
  shared-mutable hazards: `Item.smallestClearance` (make it a per-thread reduction) and
  `ShapeSearchTree.lastGeneratedEntryId` (drop it; use a stable `(clearance, leaf_id)` sort key).
  Then `par_iter()` over items, collect per-thread `Vec<Violation>`, dedup at the end with a
  `HashSet<(u32,u32,u16)>` — **not a formatted `String` key** (DesignRulesChecker.java:70-73
  allocates one `String` per violation candidate).
* **Ratsnest is embarrassingly parallel over nets.** Each `NetIncompletes` touches only its own
  net's items plus read-only board state. The only shared write is the `netIncompletes[i]` slot.
  The `static Random` seed-99 shuffle must become a per-net `SmallRng::seed_from_u64(99)` to stay
  deterministic under parallelism.
* Keep an **incremental clearance cache** the Java has none of: index violations by item id and
  invalidate only the items whose shapes changed plus everything within
  `1.2 · max_clearance` of the changed AABBs. The batch loop recomputes the entire board ≥ 3×
  per pass today (§3.2); an incremental checker turns that into O(changed).

### 10.4 Java-isms observed (file:line evidence)

| Pattern | Evidence | Rust replacement |
|---|---|---|
| Per-call `String` keys for a set | `DesignRulesChecker.java:70-73` — `id1 + "-" + id2 + "-" + layer` per violation candidate | `(u32, u32, u16)` tuple key |
| Per-hit heap record | `ShapeSearchTree.java:429, 504` — `new TreeEntry(...)` per candidate; `:486` `new EntrySortedByClearance(...)` into a `TreeSet` | reusable `SmallVec<(LeafId, u16)>` scratch buffer, sort in place |
| `LinkedList` everywhere | `Item.java:368`, `ShapeSearchTree.java:516`, `NetIncompletes.java:59,80`, `BoardConnectivityQueries.java:24` | `Vec`, `SmallVec` |
| `TreeSet<Item>` per query | `Item.java:500,526,704`; `Trace.java:156,179`; `DrillItem.java:99,278` — a red-black tree allocated and thrown away per contact lookup | `SmallVec<ItemId; 4]` + `sort_unstable` |
| Recompute-on-every-visit closure | `Item.java:617-635` — `getNormalContacts()` inside the DFS, no memo | prebuilt adjacency (§10.2) |
| 32 shape allocations per violation | `Item.java:482-491` — `enlarge` twice per binary-search iteration, 16 iterations | closed-form octagon-metric gap, or a reusable scratch `Simplex` with in-place line translation |
| O(n) union relabel | `NetIncompletes.java:328-337` — walks the whole `NetItem[]` per merge | union-find with path compression |
| Full-list scan per query | `BoardConnectivityQueries.java:23-35` (`getConnectableItems`) called from `getUnconnectedSet` once per routed item | per-net item index maintained incrementally |
| Quadratic `anyMatch` in a loop | `DesignRulesChecker.java:160` | `HashSet<ItemId>` of already-reported items |
| Sticky mutable field on a shared object | `Item.java:47, 451-453` (`smallestClearance`), never reset by `clearDerivedData` | return the minimum from the function; no field |
| Static mutable counter in a "pure" query | `ShapeSearchTree.lastGeneratedEntryId` | remove; sort by `(clearance, leaf_index)` |
| `static Random` with `setSeed` per use | `PlanarDelaunayTriangulation.java:28-30, 58-60` | per-call `SmallRng` |
| Float in-circle predicate | `PlanarDelaunayTriangulation.java:476-484` (`FloatPoint.insideCircle`) | exact `robust::incircle` — accept that the *set* of Delaunay edges may differ; the MST **count** does not |
| Unbounded recursion | `Item.java:617-635`, `Item.java:646-669`, `PlanarDelaunayTriangulation.java:220-252` | explicit stack (`Vec<ItemId>` worklist) |
| Clone-per-copy | `Item.clone()` → `copy(id)` per subclass (`Item.java:258-266`, `Via.java:71-86`) — used by the undo/serialize path that `strictDrc` and `BoardHistory` invoke per connection / per pass | copy-on-write board snapshots, or a delta log |
| Boxed nullable settings | `ScoringSettings` uses `Float`/`Integer`/`Double` wrappers everywhere so the merger can see "unset" | `Option<f32>` merged once into a plain `struct ScoringWeights { f32, … }` before the hot loop |

No `BigInteger` appears in this area (unlike the geometry kernel, where `Limits.CRIT_INT_BIG`
guards overflow paths — see `search-tree-obstacles.md`). The only overflow-sensitive constant here
is the triangulation's bounding triangle at `±2^25`, which must stay well outside any real board
coordinate.

### 10.5 Referee checklist (what the Rust checker must report)

To be able to say "this route is as good as Freerouting's" the checker must emit, for a given
board + route:

1. `clearance_violations`: deduped list of `(item_a, item_b, layer, expected, actual, shape)`
   under §2. Count = the `M` in `(N unrouted and M violations)`.
2. `incompletes`: per-net airline list under §4.6 (filter → node set → Delaunay → Kruskal).
   Count = the `N`.
3. `max_connections` = `Σ_nets max(0, #(Pin|ConductionArea) − 1)` — needed to normalise the score.
4. `bends`, `trace_length_mm`, `via_count` — trivially derived, needed for the score
   (BoardStatistics.java:275-330, 189-201, 322-336).
5. Optional report-only categories: `track_dangling`, `via_dangling`, `unconnectedItems` (§4.8).

With those five, `normalized_score = max(0, (maxConn·5e6 − N·5e6 − M·1e6 − bends·10 −
len_mm·1.0 − vias·50) / (maxConn·5e6)) · 1000` reproduces Freerouting's headline number exactly.
