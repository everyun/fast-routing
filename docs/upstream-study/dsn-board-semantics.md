# Freerouting upstream study: Specctra DSN → board translation semantics, and SES output

**Scope of this document.** Everything between "bytes of a `.dsn` file" and "a fully populated
`RoutingBoard`", plus everything between "a routed `RoutingBoard`" and "bytes of a `.ses` file".
This is the contract `fr-io` and `fr-core` must reproduce for real KiCad/Eagle/Allegro/Mentor
boards. Geometry kernels, the router itself, and the search tree are covered by the sibling specs:

| Sibling spec | Covers | Cross-reference points from here |
|---|---|---|
| `search-tree-obstacles.md` | `ShapeSearchTree`, clearance compensation, per-item tile shapes | §11 (how keepouts/planes become obstacles), §7.2 (padstack → `ConvexShape`) |
| `maze-engine.md` | A* expansion, drills, ripup | §9.4 (via rules feed `ExpansionDrill` selection), §9.3 (`useLayer` → active routing layers) |
| `shove.md` | push-and-shove | §10.3 (`FixedState` gates shoving), §9.3 (`shoveFixed`, `pullTight` per net class) |
| `pull-tight-insert.md` | insertion & optimization | §10.4 (`insertTraceWithoutCleaning` + deferred normalization) |
| `batch-orchestration.md` | pass scheduling, scoring | §6.6 (`autoroute_settings` → `RouterSettings`) |

Everything below is verified against source. Line references are `file:line`. Unless noted, the
v2 (2.3.0) file is authoritative and the v1.9 file is named in §15.

---

## 0. Which code the 2.3.0 release actually runs

v2.x moved the package `app.freerouting.designforms.specctra` → `app.freerouting.io.specctra`
and split it:

```
io/CoordinateTransform.java                        (was designforms/specctra/CoordinateTransform.java)
io/KiCadNetClassNames.java                         (NEW in v2)
io/specctra/DsnReader.java                         (NEW; read path extracted out of DsnFile)
io/specctra/DsnWriter.java                         (NEW; write path extracted out of DsnFile)
io/specctra/SesWriter.java                         (was SpecctraSesFileWriter.java)
io/specctra/SesReader.java                         (NEW; v19 read path lived in GUI BoardHandling)
io/specctra/RulesReader.java / RulesWriter.java    (was RulesFile.java)
io/specctra/parser/*.java                          (the 36 scope classes; renamed methods only)
io/specctra/parser/SpecctraDsnStreamReader.java    (was SpecctraDsnFileReader.java — the JFlex DFA)
```

Every v19 `designforms/specctra/*.java` has a v2 counterpart under `io/specctra/parser/` except
four: `CoordinateTransform` (moved to `io/`), `RulesFile` (split into RulesReader/RulesWriter),
`SpecctraSesFileWriter` (renamed `SesWriter`), `SpecctraDsnFileReader` (renamed
`SpecctraDsnStreamReader`). Diffs are dominated by the `snake_case` → `camelCase` rename and
Google-Java-Format reflow; the genuine behavioral deltas are enumerated in §15.

`io/specctra/parser/DsnFile.java` is now a static-utility shell (`private DsnFile() {}`,
`DsnFile.java:22`) holding only `CLASS_CLEARANCE_SEPARATOR`, the small `read*Scope` helpers, and
`adjustPlaneAutorouteSettings`. The real read driver is `DsnReader.readBoard`.

---

## 1. Top-level call graph

### 1.1 Read (`.dsn` → board)

```
DsnReader.readBoard(InputStream, BoardObservers, IdGenerator, designName)   DsnReader.java:64
  ├ new SpecctraDsnStreamReader(in)                            // JFlex DFA, 16 MiB initial buffer
  ├ header check: 3 tokens must be  "(" , PCB_SCOPE, <name>    DsnReader.java:80-107
  │    after PCB_SCOPE it calls scanner.yybegin(NAME) so a numeric design name parses as a String
  ├ new ReadScopeParameter(scanner, observers, idGenerator)    // carries ALL cross-scope state
  ├ Keyword.PCB_SCOPE.readScope(scopeParameter)                // generic ScopeKeyword driver
  │    dispatches on "(" + ScopeKeyword instance:
  │      PARSER_SCOPE      → Parser.readScope           (string_quote, host_cad/version, constants)
  │      RESOLUTION_SCOPE  → Resolution.readScope       (unit + resolution)
  │      STRUCTURE_SCOPE   → Structure.readScope        ***creates the board***
  │      PLACEMENT_SCOPE   → Placement (ScopeKeyword base) → COMPONENT_SCOPE → Component.readScope
  │      LIBRARY_SCOPE     → Library.readScope          (padstacks + packages)
  │      PART_LIBRARY_SCOPE→ PartLibrary.readScope      (logical parts / gate+pin swap)
  │      NETWORK_SCOPE     → Network.readScope          ***inserts pins, netclasses, vias, keepouts***
  │      WIRING_SCOPE      → Wiring.readScope           (pre-existing traces/vias/planes)
  │      anything else     → ScopeKeyword.skipScope     (bracket counting)
  ├ if no (autoroute …) scope was seen: DsnFile.adjustPlaneAutorouteSettings(board)
  └ returns BoardReadResult.{Success | OutlineMissing | ParseError | IoError}
```

Ordering is **not** enforced by the parser but is load-bearing:

* `Structure` must precede `Library` — `Library.readScope` dereferences
  `scopeParameter.layerStructure` and `scopeParameter.coordinateTransform`, both set by
  `Structure.createBoard` (`Library.java:263-265`, `Library.java:289-293`).
* `Library` must precede `Network` — `Network.insertComponent` looks up
  `routingBoard.library.packages.get(libKey, …)` (`Network.java:939-940`).
* `Placement` may precede or follow `Library`; component placements are buffered in
  `ReadScopeParameter.placementList` and only materialized at the *end* of `Network.readScope`
  (`Network.java:1321 insertComponents`).
* `Structure`'s `(plane …)` records are likewise buffered in `ReadScopeParameter.planeList`
  and inserted at the end of `Structure.readScope` (`Structure.java:1068-1122`), because nets
  do not exist yet while the structure scope is being read.

`DsnReader.readMetadata` (`DsnReader.java:178-268`) is a fast path that parses only
`parser`/`resolution`/`structure` and then closes the stream — used for board previews.

### 1.2 Write (`board` → `.dsn`)

```
DsnWriter.write(board, out, designName, compatMode)            DsnWriter.java:45
  "(pcb <name>"
   Parser.writeScope(reduced=false)      → (string_quote …)(space_in_quoted_tokens on)(host_cad …)
                                           (host_version …)(constant …)*(write_resolution …)
                                           (generated_by_freeroute)
   Resolution.writeScope                 → (resolution <unit> <int>)
   Unit.writeScope                       → (unit <unit>)
   Structure.writeScope                  → layers, boundary, (via …), rules, snap_angle,
                                           control, autoroute_settings?, planes, keepouts
   Placement.writeScope                  → (place_control (flip_style rotate_first))? + components
   Library.writeScope                    → images (packages) then padstacks
   PartLibrary.writeScope                → logical parts (only if any)
   Network.writeScope                    → nets, via infos, via rules, classes
   Wiring.writeScope                     → wires, vias, signal-layer conduction areas
```

### 1.3 SES write / read

```
SesWriter.write(board, out, designName)                        SesWriter.java:55
  "(session <name>.ses" (base_design <name>) (placement …) (was_is …) (routes …)
SesReader.read(in, board) → SesImportSummary(wires, vias, errors)   SesReader.java:60
  parses only (routes (network_out (net (wire …) (via …))))
```

---

## 2. The lexer — states, quoting, and the string bypass

`SpecctraDsnStreamReader` is JFlex-generated (`SpecctraFileDescription.flex` is the grammar
source, kept in the tree but stale w.r.t. some v2 keywords). The DFA is table-driven
(`ZZ_CMAP`/`ZZ_ACTION`/`ZZ_ROWMAP`/`ZZ_TRANS`), the caller controls lexical state explicitly.

### 2.1 Lexical states (`SpecctraDsnStreamReader.java:26-37`)

| Const | Value | Meaning |
|---|---|---|
| `YYINITIAL` | 0 | Keyword-recognizing state. Numbers become `Integer`/`Double`. |
| `STRING1` | 1 | Inside a `"`-quoted string. |
| `STRING2` | 2 | Inside a `'`-quoted string. |
| `NAME` | 3 | **Next token is a name even if it starts with a digit** (`NameIdentifier` allows leading digit / `-` / `+`). Auto-reverts to `YYINITIAL` on the produced token. |
| `LAYER_NAME` | 4 | Like `NAME` but `pcb`/`signal` still resolve to `PCB_SCOPE`/`SIGNAL`. |
| `COMPONENT_NAME` | 5 | Like `NAME` but excludes `-` from the identifier so `U1-3` splits. |
| `SPEC_CHAR` | 6 | Return exactly one special char (used to eat the `-` between component and pin). |
| `IGNORE_QUOTE` | 7 | Read an identifier treating `"`/`'` as ordinary chars — used for `(string_quote ")`. |

The scanner **self-enters** `NAME`/`LAYER_NAME` on keywords whose next token is a name.
Confirmed from the generated action table (`SpecctraDsnStreamReader.java:877+`):
`via, path, polygon/poly, padstack, clearance_class, component/comp, image, layer_rule, wire,
layer, host_version, host_cad, place, use_layer, use_via, plane, net, class, keepout,
via_keepout, place_keepout, circle/circ, rect/rectangle, polyline_path, logical_part,
logical_part_mapping, string_quote (→ IGNORE_QUOTE)`.

Callers additionally force the state — e.g. `Package.readPinInfo` does `yybegin(NAME)` **twice**
(`Package.java:252` and `Package.java:264-265`) so both the padstack id and the pin name survive
even when they are bare integers.

### 2.2 The keyword set actually recognized

109 distinct `return Keyword.X` actions exist in the generated DFA. Two `Keyword` constants are
declared but **never produced by the lexer**:

* `Keyword.PN` — `(PN "MPN123")` inside a `(place …)` record arrives as the *string* `"PN"`.
  `Component.readPlaceScope` handles both (`Component.java:281-283`):
  `nextToken == Keyword.PN || (nextToken instanceof String && "PN".equalsIgnoreCase(...))`.
  A minimal parser that only compares against a keyword enum drops part numbers.
* `Keyword.JUMPER` — `(type jumper)` inside a layer scope arrives as a string; `Structure`
  compares `nextToken.toString()` against `Keyword.JUMPER.getName()` (`Structure.java:349`) and
  treats it as a signal layer.

Alias spellings the lexer accepts: `circ`/`circle`, `poly`/`polygon`, `rect`/`rectangle`,
`comp`/`component`, `clear`/`clearance`, `wire_keepout` → `KEEPOUT`,
`prefered_direction` ≡ `preferred_direction` (and the `_trace_costs` variants).

> Note on `Keyword.getName()` in v2: the debug names were camelCased
> (`new Keyword("clearanceClass")`, `new Keyword("snapAngle")`, `new Keyword("viaRule")`,
> `Keyword.java:18-119`). These are **debug strings only** — the wire format is still
> `clearance_class`, `snap_angle`, `via_rule`, matched inside the DFA. The two `getName()` calls
> that *are* semantic compare against `"pcb"` and `"signal"` (`Shape.getLayer`, `Shape.java:81-84`)
> and `"jumper"`, all of which stayed lowercase.

### 2.3 `nextString` — the raw-buffer bypass (critical, easy to get wrong)

`nextString(boolean ignoreNewline, char leading)` (`SpecctraDsnStreamReader.java`, method block
after the DFA) **does not run the DFA at all**. It walks `zzBuffer` from `zzMarkedPos` directly:

```
skipLeading  = ignoreNewline ? {8,10,13,32} : {8,32}      // BS, LF, CR, space
skipLeading += {leading}                                  // caller-chosen extra separator
advance i while zzBuffer[zzMarkedPos+i] ∈ skipLeading

if zzBuffer[zzMarkedPos+i] == '"' (34):
    stopAt = {10,13,34}      // LF, CR, closing quote   → quoted string, spaces allowed inside
    i++, skipLastChar = true
else:
    stopAt = {8,10,13,32,40,41} ∪ {leading}   // BS, LF, CR, space, '(' , ')'

append chars while not in stopAt
commit: zzStartRead += i-1; zzCurrentPos += i-1; zzMarkedPos += i; zzLexicalState = YYINITIAL
if the char just consumed was '(' or ')':  back up one   // so it becomes the next token
```

Consequences you must replicate:

1. **Quoting is positional, not `string_quote`-driven.** Only ASCII `"` (34) opens a
   bypass-quoted string here, regardless of what `(string_quote ')` declared. `'`-quoting only
   works through the DFA path (`nextToken`), not through `nextString`.
2. A quoted string is terminated by `"` **or by end-of-line**. An unterminated quote does not
   run away past the line.
3. **Escapes are not processed.** In the DFA path, `\` is appended literally
   (`SpecctraFileDescription.flex:228`); in the bypass path `\` is an ordinary char. There is
   no `\"` escape anywhere.
4. `nextString` never returns `null` — it returns `""` at exhaustion. `nextStringList` uses that
   as its terminator.

### 2.4 `nextStringList(char separator)`

```
first = nextString(true, separator)        // KiCad 8 emits a leading "" in (class … "" NET1 …)
if first non-empty: add
loop: s = nextString(true, separator); if s.isEmpty() break; add
rewind: zzStartRead = zzCurrentPos = zzMarkedPos - 1
```

The leading-empty tolerance is explicitly commented as a KiCad 8 netlist-export bug workaround.
`Rule.readClearanceRule` calls `nextStringList(DsnFile.CLASS_CLEARANCE_SEPARATOR)` = `'-'`
(`Rule.java:278`, `DsnFile.java:20`), i.e. clearance class-pair names are split on `-`, not `_`.

### 2.5 `nextDouble`

`nextString()` then `NumberFormat.getInstance(Locale.US).parse(...)`, returning `null` on
failure (`SpecctraDsnStreamReader`, `nextDouble`). The `NumberFormat` instance is a **static
mutable field** (`public static NumberFormat nf`) — lazily initialized, not thread safe.

### 2.6 Buffering

`ZZ_BUFFERSIZE = 16 * 1024 * 1024` (`SpecctraDsnStreamReader.java:40`) and `zzRefill` doubles
`zzBuffer` when exhausted. Practically the whole DSN file ends up resident in a `char[]` (2 bytes
per byte of input). This is what makes `nextString`'s direct `zzBuffer` indexing safe-ish, and it
is a hard constraint if you want to keep the same string semantics.

---

## 3. Units, resolution, and `CoordinateTransform` end to end

### 3.1 The two numbers from the file

```
(resolution <unit> <positive-int>)     Resolution.readScope, Resolution.java:28-72
```
* `unit ∈ {mil, inch, mm, um}`, parsed by `Unit.fromString` = `Unit.valueOf(s.toUpperCase())`
  (`board/model/structure/Unit.java:27-33`). Anything else → the resolution scope fails.
* `resolution` must be an `Integer`. Defaults if the scope is absent:
  `unit = MIL`, `resolution = 100` (`ReadScopeParameter.java:87-89`).
* `Unit` micrometer factors: `MIL 25.4`, `INCH 25400`, `MM 1000`, `UM 1`
  (`Unit.java:7-10`). `Unit.scale(v, from, to) = v * from.µm / to.µm`.

DSN coordinates are integers in *units of `1/resolution` of `unit`*. Example: KiCad emits
`(resolution um 10)`, so `12345` means 1234.5 µm.

### 3.2 Deriving the board scale factor (`Structure.createBoard`, `Structure.java:1189-1205`)

```
scaleFactor = max(resolution, 1)
maxCoor = max over the 4 coords of the bounding rectangle of |coor[i] * resolution|
if maxCoor == 0:  boardOutlineOk = false; abort            // degenerate outline
while 5 * maxCoor >= Limits.CRIT_INT:                      // CRIT_INT = 33_554_432 = 2^25
    scaleFactor /= 10                                      // INTEGER division
    maxCoor    /= 10
coordinateTransform = new CoordinateTransform(scaleFactor, 0, 0)
```

Notes:
* `Limits.CRIT_INT = 2^25` (`geometry/planar/Limits.java:12`) — the bound above which products of
  two coordinates overflow `long`-free arithmetic in the geometry kernel.
* The `5 *` headroom factor is deliberate slack for intermediate products.
* `scaleFactor` is a `double` field but is seeded from an `int` and reduced by *integer* division,
  so it collapses to 0 after enough iterations on pathological boards — reproduce the integer
  division exactly if you want bug-compatible behavior, but a Rust port should clamp to ≥1.
* `base_x`/`base_y` are always `0` in practice: `new CoordinateTransform(scaleFactor, 0, 0)` in
  `Structure.createBoard` and again in `SesWriter.writeSessionScope`. The base offset machinery
  exists but is dead.

### 3.3 The transform itself (`io/CoordinateTransform.java`)

```
dsnToBoard(v)          = v * scaleFactor
boardToDsn(v)          = v / scaleFactor
dsnToBoard([x,y])      = FloatPoint((x - baseX) * s, (y - baseY) * s)
dsnToBoardRel([x,y])   = FloatPoint(x * s, y * s)          // no base translation
boardToDsn(FloatPoint) = [p.x/s + baseX, p.y/s + baseY]
boardToDsnRel(...)     = [p.x/s, p.y/s]
boardToDsn(IntBox)     = [ll.x/s+bX, ll.y/s+bY, ur.x/s+bX, ur.y/s+bY]
```

### 3.4 Where the factor is applied — the complete list

| Quantity | Direction | Formula | Site |
|---|---|---|---|
| absolute point (outline, keepout, plane, wire corner, component location, via location) | in | `dsnToBoard(tuple)` then `.round()` | `Rectangle.transformToBoard` `Rectangle.java:59-69`, `Polygon.transformToBoard` `Polygon.java:26-36`, `Network.java:951`, `Wiring.java:497,556,698` |
| relative point (padstack shape, package pin offset, package outline, package keepout) | in | `dsnToBoardRel` / `dsnToBoard(scalar)` | `Library.java:172,317-319`, `Shape.transformAreaToBoardRel` `Shape.java:561-594` |
| trace / path width | in | `halfWidth = round(dsnToBoard(width/2))` | `Wiring.java:454` |
| default trace width (`(rule (width w))`) | in | `traceHalfwidth = round(dsnToBoard(w) / 2)` | `Structure.java:634`, `Structure.java:655` |
| net-class trace width | in | `traceHalfwidth = round(dsnToBoard(w / 2))` | `Network.java:485`, `Network.java:510` |
| clearance value | in | `round(dsnToBoard(value))` | `Structure.setClearanceRule` `Structure.java:683`, `Network.addClearanceRule` `Network.java:736`, `addMixedClearanceRule` `Network.java:629` |
| min/max trace length (`(circuit (length …))`) | in | `dsnToBoard(len)` (no rounding) | `Network.java:466-471` |
| circle radius | in | `round(dsnToBoard(coor[0]) / 2)` — **the DSN number is a diameter** | `Circle.transformToBoard` `Circle.java:40` |
| path half-offset (aperture) | in | `offset = dsnToBoard(width) / 2` | `PolygonPath.java:68` |
| SES / DSN export of any length | out | `boardToDsn(v)` | `Rule.writeDefaultRule` `Rule.java:170-190`, `Wiring.writeWireScope` `Wiring.java:122` |

**Rounding discrepancy to preserve.** `Structure.updateBoardRules` computes
`round(dsnToBoard(w)) / 2` semantics as `round(dsnToBoard(w) / 2)` (double division then round,
`Structure.java:634`), while `Network.insertNetClass` computes `round(dsnToBoard(w / 2))` (halve
in DSN units first, `Network.java:485`). For odd widths at low resolution these differ by one
board unit. Both are in the shipping build.

**Clearance parity.** `ClearanceMatrix.setValue` clamps negative to 0 and **rounds odd values up
to the next even integer** (`ClearanceMatrix.java:104-113`), except `Integer.MAX_VALUE` which is
decremented. Every stored clearance is therefore non-negative and even. Reads may add
`clearance_safety_margin = 16` (`ClearanceMatrix.java:17`) when `addSafetyMargin` is true.

### 3.5 The SES scale factor is a *different* number

```
SesWriter.writeSessionScope, SesWriter.java:80-82
scaleFactor = board.communication.coordinateTransform.dsnToBoard(1) / board.communication.resolution
            = dsnScaleFactor / resolution
sesTransform = new CoordinateTransform(scaleFactor, 0, 0)
```
So `boardToDsn` in the SES writer divides by `dsnScaleFactor/resolution`, i.e. multiplies the
"pure DSN units" result by `resolution` — which is exactly what a SES file wants, because SES
coordinates are in the *same* `(resolution …)` grid, and the SES `(routes … (resolution …))` scope
re-declares it (`SesWriter.java:228`). The SES reader inverts with
`boardCoord = round(sesCoord / sessionFileScaleDenominator)` where the denominator is the same
`dsnToBoard(1)/resolution` (`SesReader.java:71-72`, `SesReader.java:305`).

---

## 4. `ReadScopeParameter` — the cross-scope state bag

`io/specctra/parser/ReadScopeParameter.java`. This is the only mutable state threaded through
the whole parse; a Rust port should model it as one owned struct.

| Field | Type | Set by | Consumed by |
|---|---|---|---|
| `scanner` | `IJFlexScanner` | ctor | everything |
| `boardHandling` | `BoardParserCallback` | ctor (`MinimalBoardManager`) | `Structure.createBoard`, all inserters |
| `netlist` | `NetList` (TreeMap keyed by `Net.Id{name, subnetNumber}`) | `Network.readNetScope`, `Structure` plane fixup | `Network.insertComponent` (pin→net lookup) |
| `observers`, `idGenerator` | | ctor | `Communication` |
| `warnings` | `List<String>` | `Wiring`, `Structure` | `BoardReadResult` |
| `planeList` | `Collection<PlaneInfo{area, netName}>` | `Plane.readScope` | end of `Structure.readScope` |
| `placementList` | `Collection<ComponentPlacement>` | `Component.readScope` | end of `Network.readScope` |
| `constants` | `Collection<String[2]>` | `Parser` | `Communication.SpecctraParserInfo` |
| `viaPadstackNames` | `Collection<String>` | `Structure` `(via a b c (spare d e))`, plus every net class's `use_via` | `Network.readScope` tail |
| `viaAtSmdAllowed` | `boolean` | `(control (via_at_smd on))` | `Network.insertViaInfos`, `Wiring.readViaScope` |
| `snapAngle` | `AngleRestriction` (default `FORTYFIVE_DEGREE`) | `(snap_angle …)` | `boardRules.setTraceAngleRestriction` |
| `logicalPartMappings`, `logicalParts` | | `PartLibrary` | `Network.insertLogicalParts` |
| `stringQuote` | `String` (default `"\""`) | `(string_quote …)` | `Structure.setClearanceRule`, writers |
| `hostCad`, `hostVersion` | `String` | `(host_cad …)`, `(host_version …)` | `Plane.readScope` (Allegro), `Communication.hostIsOldKicad` |
| `dsnFileGeneratedByHost` | `boolean` (default true) | set **false** by `(generated_by_freeroute)` | round-trip detection |
| `boardOutlineOk` | `boolean` (default true) | `Structure.createBoard` | `BoardReadResult.OutlineMissing` |
| `writeResolution` | `WriteResolution{charName, positiveInt}` | `(write_resolution <s> <int>)` | echoed back on write |
| `coordinateTransform` | | `Structure.createBoard` | everything geometric after structure |
| `layerStructure` | dsn `LayerStructure` | `Structure` (rebuilt each time a layer is added, `Structure.java:975-978`) | shape layer resolution |
| `autorouteSettings` | `RouterSettings` \| null | `(autoroute_settings …)` | `RoutingJob`; null ⇒ `adjustPlaneAutorouteSettings` |
| `unit`, `resolution` | `Unit`, `int` | `(resolution …)` | scale factor |

---

## 5. Layers

### 5.1 `Layer` (dsn-side, `io/specctra/parser/Layer.java`)

```
Layer { String name; int no; boolean isSignal; Collection<String> netNames; }
static Layer PCB    = Layer("pcb",    -1, false)     // "all layers" pseudo-layer
static Layer SIGNAL = Layer("signal", -1, true)      // "all signal layers" pseudo-layer
```
`no == -1` means "not a physical layer". `netNames` is only populated for power planes via
`(use_net …)`.

### 5.2 Reading `(layer <name> (type signal|power|jumper) (rule …)* (use_net …)*)`

`Structure.readLayerScope`, `Structure.java:327-409`:

* Layer name read with `scanner.nextString()` (bypass path — so a numeric layer name works).
* `(type power)` ⇒ `isSignal = false`. `(type signal)` and `(type jumper)` ⇒ signal.
  Anything else ⇒ **the layer is dropped entirely** (`layerOk = false`, the layer is not added
  and `foundLayerCount` is not incremented) and an error is logged. Note the layer *index*
  therefore comes from `foundLayerCount`, i.e. rejected layers shift all later indices.
* `(rule …)` inside a layer scope is stored as a `LayerRule{layerName, rules}` on
  `BoardConstructionInfo.layerDependentRules` — resolved to a layer index later in
  `updateBoardRules`.
* `(use_net a b c)` collects the plane net names. Each name is read with an explicit
  `yybegin(NAME)` so numeric net names survive (`Structure.java:379`).

### 5.3 Layer name → index (`LayerStructure.getNo`, `LayerStructure.java:31-45`)

```
exact match on name (case sensitive!) → index
else if name.contains("Top")    → 0
else if name.contains("Bottom") → layers.length - 1
else -1
```
The `Top`/`Bottom` substring fallback exists for Electra-generated outlines. It is
**case-sensitive and substring-based** — a layer literally named `"Stopper"` maps to layer 0.

`containsPlane(netName)` returns true iff some **non-signal** layer lists `netName` in
`use_net` (`LayerStructure.java:58-68`). That flag is passed to `Nets.add(name, subnet,
containsPlane)` when the net is created (`Network.java:1412-1414`).

---

## 6. The `structure` scope

`Structure.readScope`, `Structure.java:936-1137`.

### 6.1 Dispatch table

| Token | Action |
|---|---|
| `boundary` | `readBoundaryScope` → fills `boundingShape` / `outlineShapes` / `outlineClearanceClassName` |
| `layer` | `readLayerScope`; if `layerStructure != null` it is **rebuilt** from the accumulated list |
| `via` | `viaPadstackNames = readViaPadstacks(scanner)` |
| `rule` | `defaultRules += Rule.readScope(scanner)` |
| `keepout` / `via_keepout` / `place_keepout` | `Shape.readAreaScope(scanner, layerStructure, false)` buffered into three lists; `layerStructure` is materialized on first use |
| `plane` | `Plane.readScope` → buffers a `PlaneInfo` |
| `autoroute_settings` | **only parsed if `layerStructure == null`** — see §6.6 |
| `control` | `readControlScope` → `(via_at_smd on\|off)` |
| `flip_style` | `PlaceControl.readFlipStyleRotateFirst` → `flipStyleRotateFirst` |
| `snap_angle` | `readSnapAngle` → `NINETY_DEGREE` \| `FORTYFIVE_DEGREE` \| `NONE` |
| anything else | `skipScope` |

After the loop: `createBoard` (if not already created), then `flipStyleRotateFirst` is pushed
onto `board.components`, then keepouts, then via-keepouts, then place-keepouts, then planes,
then `insertMissingPowerPlanes`.

### 6.2 Boundary and outline (`readBoundaryScope`/`addBoundaryShape`, `Structure.java:273-325`)

Each `(boundary <shape>)` produces one shape. Classification (`addBoundaryShape`):

```
if shape is PolylinePath or PolygonPath  → outlineShapes         // paths are always outline
else if shape.layer == Layer.PCB:
        first one       → boundingShape                          // the board bounding rectangle
        subsequent ones → outlineShapes
else if shape.layer == Layer.SIGNAL      → outlineShapes
else warn "unexpected layer at boundary"                          // dropped
```
A nested `(clearance_class <name>)` inside a boundary scope sets
`outlineClearanceClassName`, which becomes the outline item's clearance class
(`ReadScopeParameter.MinimalBoardManager.createBoard`, `ReadScopeParameter.java:146-157`):
`clearanceMatrix.getNo(name)` clamped at 0, else `defaultItemClearanceClasses[AREA]`.

`createBoard` (`Structure.java:1139-1286`):

1. `layerCount == 0` ⇒ fail.
2. If `boundingShape == null`: if `outlineShapes` is also empty ⇒ `boardOutlineOk = false`, fail
   (the caller returns `BoardReadResult.OutlineMissing`). Otherwise the bounding box is the union
   of all outline shapes' bounding boxes.
3. Validate each layer's `no ∈ [0, layerCount)`.
4. Compute `scaleFactor` (§3.2), build `coordinateTransform`.
5. `bounds = boundingBox.transformToBoard(...)` then **`bounds.offset(1000)`**
   (`Structure.java:1208`) — the board's `IntBox` is inflated by 1000 board units on every side.
   This margin is what lets the router shove traces slightly outside the nominal bbox.
6. Transform outline shapes. **A `PolygonPath` with non-zero width is rewritten to width 0**
   before transforming (`Structure.java:1212-1219`), because the offset function used by
   `transformToBoard` is only implemented for convex shapes. Shapes whose transformed
   `dimension() <= 0` are dropped.
7. If everything was dropped, the bounding shape itself becomes the single outline shape.
8. `separateHoles(boardOutlineShapes)` (§6.3).
9. `ClearanceMatrix.getDefaultInstance(boardLayerStructure, 0)` — a 2×2 matrix with class 0 named
   `"null"` and class 1 named `"default"`, all values 0 (`ClearanceMatrix.java:44-52`).
10. `new BoardRules(layerStructure, clearanceMatrix)`.
11. Build `Communication` with unit/resolution/parser-info/transform.
12. `hostIsOldKicad()` warning: `host_cad` contains "kicad" (case-insensitive) and the first
    integer in `host_version` is ≤ 5 (`Communication.java:67-82`).
13. `updateBoardRules(...)` (§7), `setTraceAngleRestriction(snapAngle)`.
14. `boardHandling.createBoard(bounds, layerStructure, outlineShapes, outlineClearanceClassName,
    rules, communication)`.
15. **Each outline hole is inserted as an `ObstacleArea` on every layer** with clearance class 0
    and `FixedState.SYSTEM_FIXED` (`Structure.java:1279-1283`).

### 6.3 Hole detection in the outline (`separateHoles`, `Structure.java:577-605`)

```
for each shape i:
  for each shape j != i, j not already a hole:
     if !bbox(j).contains(bbox(i)): continue
     shape_i.isHole = shape_j.containsAllCorners(shape_i)
holes = { shapes with isHole }; remove them from outlineShapes
```
`containsAllCorners` (`Structure.java:1332-1352`) splits the candidate container into convex
pieces (`shape.splitToConvex()`) and checks that every corner of the inner shape lies in at least
one piece. If `splitToConvex()` returned null (failure) the answer is `false`.

Note the bug-shaped detail: `currentShape.isHole` is **assigned, not OR-ed**, inside the inner
loop, so only the *last* qualifying `j` decides. With more than two nested rings the result
depends on iteration order.

### 6.4 Via padstack names (`readViaPadstacks`, `Structure.java:411-444`)

```
(via  P1 P2 P3 (spare P4 P5))
→ normalVias = [P1,P2,P3];  spareVias = recursive read of the spare scope
→ result = normalVias ++ spareVias                 // spares appended at the END
```
Unknown nested scopes are skipped. Any non-string token aborts with `null`.

These are just **names**; resolution to `Padstack` objects happens at the end of
`Network.readScope` (`Network.java:1286-1314`) after the library has been read:

```
for each name: cleaned = name.replaceAll("\\.\\d+", "")     // strip KiCad ".1" duplicates
               padstack = board.library.padstacks.get(cleaned)
               if found: keep; else warn and drop
board.library.setViaPadstacks(compacted array)
```

The `\.\d+` stripping is applied in four places and must be replicated everywhere:
`Library.readPadstackScope` (padstack *definition* name, `Library.java:113`), package pin padstack
lookup (`Library.java:322`), via padstack name list (`Network.java:1293`), and
`Wiring.readViaScope` / `SesReader.processViaScope` (`Wiring.java:659`, `SesReader.java:384`).

### 6.5 `(control (via_at_smd on|off))`

`readControlScope`, `Structure.java:446-476` → `scopeParameter.viaAtSmdAllowed`. This is ANDed
with the per-padstack `attachAllowed` flag when creating `ViaInfo`s
(`Network.createDefaultViaInfos`, `Network.java:366`) and when inserting wiring vias
(`Wiring.java:705`).

On write, `writeControlScope` (`Structure.java:208-227`) emits `on` if **any** `ViaInfo` has
`attachSmdAllowed()`.

### 6.6 `(autoroute_settings …)` — the guard bug

```java
} else if (nextToken == Keyword.AUTOROUTE_SETTINGS) {
  if (scopeParameter.layerStructure == null) {                       // Structure.java:1007
    scopeParameter.layerStructure = new LayerStructure(boardConstructionInfo.layerInfo);
    scopeParameter.autorouteSettings =
        AutorouteSettings.readScope(scopeParameter.scanner, scopeParameter.layerStructure);
  }
}
```
If `layerStructure` was already materialized (which happens as soon as **any** `keepout`,
`via_keepout`, `place_keepout`, or `plane` scope precedes the `autoroute_settings` scope, and
also every time a `layer` scope is read after the first materialization, `Structure.java:975-978`),
the `autoroute_settings` scope is **silently not parsed and not skipped** — the tokens fall
through to the outer loop, where the nested `(autoroute on)` etc. are treated as unknown scopes
and skipped one by one. Net effect: settings are ignored, parsing still terminates. This is
identical in v19 (`Structure.java:891-896`). Freerouting's own DSN writer emits
`autoroute_settings` *before* keepouts (`Structure.writeScope`, `Structure.java:75-88`) so its
own round-trip works.

Because `autorouteSettings == null` is also the trigger for `adjustPlaneAutorouteSettings`
(`DsnReader.java:132`), the practical consequence is that plane auto-detection runs on most
third-party DSN files.

`AutorouteSettings.readScope` (`AutorouteSettings.java:18-71`) reads:

| Token | Target |
|---|---|
| `(fanout on\|off)` | parsed and **discarded** |
| `(autoroute on\|off)` | `setRunRouter` |
| `(postroute on\|off)` | `setRunOptimizer` |
| `(vias on\|off)` | `setViasAllowed` |
| `(via_costs <int>)` | `setViaCosts` (clamped ≥ 1, `RouterSettings.java:609`) |
| `(plane_via_costs <int>)` | `setPlaneViaCosts` (clamped ≥ 1) |
| `(start_ripup_costs <int>)` | `setStartRipupCosts` (clamped ≥ 1) |
| `(layer_rule <name> (active on\|off) (preferred_direction horizontal\|vertical) (preferred_direction_trace_costs <f>) (against_preferred_direction_trace_costs <f>))` | per-layer |

Defaults when unset: `viaCosts = 1`, `planeViaCosts = 1`, `startRipupCosts = 1`
(`RouterSettings.java:538,601,614`), per-layer trace costs `1.0/1.0`
(`RouterSettings.java:475-476`), preferred direction horizontal on odd layer indices
(`layer % 2 == 1`, `RouterSettings.java:744`).

### 6.7 `(snap_angle …)`

`NINETY_DEGREE | FORTYFIVE_DEGREE | NONE` → `AngleRestriction`. Default when absent:
`FORTYFIVE_DEGREE` (`ReadScopeParameter.java:59`). Applied as
`boardRules.setTraceAngleRestriction(snapAngle)` in `createBoard` (`Structure.java:1267`); note
it is applied to `boardRules` *before* the board is constructed, and again (redundantly) nothing
re-applies it if `snap_angle` appears after the board was created. Since `snap_angle` almost
always precedes any keepout, this works in practice.

---

## 7. Rule scoping and clearance-class construction

This is the single most intricate part of the importer. There are **four** scoping levels, each
with its own code path.

### 7.1 The rule AST (`Rule.java`)

```
abstract Rule
  WidthRule    { double value }                                  // (width w)
  ClearanceRule{ double value; Collection<String> clearanceClassPairs }   // (clearance v (type a-b c-d …))
LayerRule      { Collection<String> layerNames; Collection<Rule> rules }  // (layer_rule L1 L2 (rule …))
```
`Rule.readScope` (`Rule.java:24-64`) loops until the scope's `)`, handling only `(width …)` and
`(clearance …)`; every other nested scope is skipped.

`readClearanceRule` (`Rule.java:257-303`):
```
value = scanner.nextDouble()
if next token != ')':
    require '(' then 'type'
    classPairs = scanner.nextStringList('-')     // split on '-', NOT '_'
    require ')' twice   (closes (type …) then (clearance …))
```
So `(clearance 200 (type smd-via))` yields `classPairs = ["smd", "via"]` (two entries!), while
`(clearance 200 (type smd_via))` yields `["smd_via"]` (one entry). **Both encodings occur in the
wild** and the downstream code handles both — see §7.3.

`readLayerRuleScope` (`Rule.java:67-107`) reads a `LAYER_NAME`-mode list of layer names until it
hits `(`, then one or more `(rule …)` blocks.

### 7.2 PCB-level and layer-level rules (`Structure.updateBoardRules`, `Structure.java:611-670`)

Order matters:

```
1. for each ClearanceRule in defaultRules:  setClearanceRule(rule, layer = -1, …)
        (tracks whether "smd_to_turn_gap" was seen)
2. for each WidthRule in defaultRules:
        boardRules.setDefaultTraceHalfWidths( round(dsnToBoard(w) / 2) )
3. for each LayerRule (from (layer L (rule …))):
        layerIndex = layerStructure.getNo(L);   if < 0 skip the whole rule
        WidthRule     → boardRules.setDefaultTraceHalfWidth(layerIndex, round(dsnToBoard(w)/2))
        ClearanceRule → setClearanceRule(rule, layerIndex, …)
4. if no "smd_to_turn_gap" was found:
        boardRules.setPinEdgeToTurnDist( boardRules.getMinTraceHalfWidth() )
```

`setDefaultTraceHalfWidths` refuses values ≤ 0 (`BoardRules.java:126-134`) and updates the
running `minTraceHalfWidth` / `maxTraceHalfWidth` (seeded 100000 / 100, `BoardRules.java:57-59`).

### 7.3 `Structure.setClearanceRule` — the class-pair parser (`Structure.java:676-808`)

```
currentClearance = round(dsnToBoard(rule.value))            // even-rounded on store

if classPairs is empty:                                     // (clearance 200)
    layer < 0 ? clearanceMatrix.setDefaultValue(v)          // ALL classes ≥1 × ALL classes ≥1
              : clearanceMatrix.setDefaultValue(layer, v)
    return

if any pair startsWith "wire_" or endsWith "_wire":         // containsWireClearancePair
    createDefaultClearanceClasses(boardRules)               // appends "via","smd","pin","area"
                                                            // and binds them into defaultItemClearanceClasses

for each string s in classPairs:
    if s.equalsIgnoreCase("smd_to_turn_gap"):
        boardRules.setPinEdgeToTurnDist(v); mark found; continue

    # --- split s into a two-element pair ---
    if classPairs.size() == 2:                       # the '-'-separated case: use the WHOLE list
        pair = [first, second] of classPairs
        strip all '"' from both; if pair[1] startsWith '_' drop that leading '_'
    elif s startsWith stringQuote:                   # quoted first name, e.g.  "My Class"_via
        s = s.substring(stringQuote.length())
        pair = s.split(stringQuote, 2)
        require pair.length == 2 && pair[1] startsWith "_"   else warn+skip
        pair[1] = pair[1].substring(1)
    else:
        pair = s.split("_", 2)                       # first underscore only
        if pair.length != 2: continue                # e.g. "smd_via_same_net" is NOT supported

    firstNo  = pair[0]=="wire" ? 1 : clearanceMatrix.getNo(pair[0]);  if <0 append the class
    secondNo = pair[1]=="wire" ? 1 : clearanceMatrix.getNo(pair[1]);  if <0 append the class
    set BOTH (firstNo, secondNo) and (secondNo, firstNo) to v, on `layer` or all layers
```

Details you must reproduce:

* `"wire"` is a magic alias for clearance class **1** (`"default"`), never a new class.
* `appendClearanceClass(rules, name)` (`Structure.java:826-840`) appends to the matrix **and**,
  for the four reserved names, binds the class index into the default net class's
  `defaultItemClearanceClasses`: `via→VIA`, `pin→PIN`, `smd→SMD`, `area→AREA`. Any other name
  gets a matrix class but no item-class binding.
* `createDefaultClearanceClasses` appends them in the fixed order `via, smd, pin, area`
  (`Structure.java:819-824`) — so on a fresh board they get indices 2,3,4,5. **Index stability
  matters** because keepouts/pins/vias are later resolved by index.
* `ClearanceMatrix.appendClass` (`ClearanceMatrix.java:281-322`) initializes the new row/column
  from **class 1 (`default`)**: `newValue[i] = getValue(1, i, layer)` for every existing class,
  and `newValue[new,new] = getValue(1,1,layer)`.
* `setDefaultValue(v)` writes `v` into **every** `(i,j)` with `i,j ≥ 1` — it wipes any
  previously-set specific pair. That is why the PCB-level `(clearance v)` with no `(type …)`
  must be processed *before* the specific pairs, and it is (`Structure.java:617-628` iterates all
  clearance rules in file order, and the no-type rule is conventionally emitted first).

**v2-only branch.** The `classPairs.size() == 2` fast path (`Structure.java:719-728`) is new in
v2 and exists to handle the `-`-separated `(type a-b)` form produced by `nextStringList('-')`.
v19 had no such branch; instead it had a *different* post-processing step that stripped quotes
from `pair[1]` and rejected `pair[1]` containing further underscores
(v19 `Structure.java`, `set_clearance_rule`). See §15.

### 7.4 Net-class-level rules (`Network.insertNetClass`, `Network.java:436-543`)

Reading a `(class …)` scope produces a dsn `NetClass` record (`NetClass.readScope`,
`NetClass.java:55-143`):

```
(class <name> <net> <net> …
   (rule (width w) (clearance c (type …))…)
   (layer_rule L1 L2 (rule …))
   (via_rule <name>)
   (circuit (use_via V1 V2) (use_layer L1 L2) (length <max> <min>))
   (clearance_class <name>)
   (shove_fixed on|off)
   (pull_tight on|off))
```
Class name read via `yybegin(NAME); nextString()`. The member net list is read with
`nextStringList()` (space-separated, tolerant of a leading `""`).
Defaults: `pullTight = true`, `shoveFixed = false`, `minTraceLength = maxTraceLength = 0`.

`insertNetClass` then:

1. **KiCad default-name folding (v2 only).**
   `KiCadNetClassNames.isKiCadDefaultNetClassName(name)` is true for `"default"` and
   `"kicad_default"` (case-insensitive, `io/KiCadNetClassNames.java:25-30`). Such a class maps
   onto `board.rules.getDefaultNetClass()` instead of appending a new one
   (`Network.java:442-445`). This is the fix for KiCad renaming its `Default` class to
   `kicad_default` in DSN export to avoid colliding with Freerouting's reserved `default`.
2. `(clearance_class N)` → `boardNetClass.setTraceClearanceClass(clearanceMatrix.getNo(N))`,
   warn if absent.
3. `(via_rule R)` → `setViaRule(board.rules.getViaRule(R))`, warn if absent.
4. `(length max min)` → `setMaximumTraceLength(dsnToBoard(max))` / `setMinimumTraceLength(...)`,
   only when `> 0`.
5. Each member net name (`board.rules.nets.get(name)` returns **all** subnets, case-insensitive
   comparison, `Nets.java:53-61`) gets `setClass(boardNetClass)`.
6. `(rule (width w))` → `setTraceHalfWidth(round(dsnToBoard(w/2)))` (all layers).
7. `(rule (clearance …))` → `addClearanceRule(matrix, netClass, rule, layer=-1, transform)`.
8. `(layer_rule L… (rule …))` → same, per resolved layer index.
9. `setPullTight`, `setShoveFixed`.
10. If **any** clearance rule was found *and* this is not the default net class:
    `createDefaultViaInfos(board, boardNetClass, viaAtSmdAllowed)` — one `ViaInfo` per via
    padstack, named `<padstackName>-<netClassName>` (separator `-`, `Network.java:371`), with
    clearance class `netClass.defaultItemClearanceClasses[VIA]`.
11. If `use_via` is non-empty → `createViaRule(useVia, netClass, board, attachAllowed)`;
    else if via infos were created in step 10 →
    `board.rules.createDefaultViaRule(netClass, netClass.getName())`.
12. If `use_layer` is non-empty → `createActiveTraceLayers` (§9.3).

### 7.5 `Network.addClearanceRule` (`Network.java:730-784`) — different from `Structure`'s

```
v = round(dsnToBoard(rule.value))
classNo = clearanceMatrix.getNo(netClass.getName())
if classNo < 0:                                   # the net class has no matrix class yet
    appendClass(netClassName); classNo = getNo(netClassName)
    # seed the whole new row/column with the MAX of v and whatever appendClass initialized:
    for i in 1..classCount-1, for layer in 0..layerCount-1:
        val = max( getValue(classNo, i, layer), v )
        setValue(classNo, i, layer, val); setValue(i, classNo, layer, val)
    netClass.defaultItemClearanceClasses.setAll(classNo)      # ALL item classes point here
netClass.setTraceClearanceClass(classNo)

if rule.classPairs empty:
    setValue(classNo, classNo, [layer,] v);  return

if containsWireClearancePair(classPairs):
    createDefaultClearanceClasses(netClass, clearanceMatrix)   # via, smd, pin, area — per NET CLASS

for each s in classPairs:
    pair = s.split("_")            # NOTE: split on EVERY underscore, then require length==2
    if pair.length != 2: continue
    firstNo  = getClearanceClass(matrix, netClass, pair[0])
    secondNo = getClearanceClass(matrix, netClass, pair[1])
    set both (first,second) and (second,first) to v
```

Differences from `Structure.setClearanceRule` worth flagging:
* `split("_")` with no limit (vs `split("_", 2)`), so `a_b_c` yields 3 parts and is skipped.
* No `stringQuote` handling, no `-`-pair handling, no `smd_to_turn_gap` handling.
* Class names are **namespaced per net class**, see below.

### 7.6 `Network.getClearanceClass` — the per-net-class item-class matrix

`Network.java:790-830`:
```
newClassName = (itemClassName == "wire") ? netClassName
                                         : netClassName + "-" + itemClassName
if matrix already has newClassName: return its index
appendClass(newClassName);  result = getNo(newClassName)
netClassNo = getNo(netClassName)
# copy the ENTIRE row/column of the parent net class into the new class, all layers:
for i in 1..classCount-1, for layer:
    v = getValue(netClassNo, i, layer)
    setValue(result, i, layer, v);  setValue(i, result, layer, v)
switch itemClassName:
   "via"  → netClass.defaultItemClearanceClasses[VIA]  = result
   "pin"  → …[PIN]  = result
   "smd"  → …[SMD]  = result
   "area" → …[AREA] = result
   default: ignore
```

So a DSN with
```
(class SIGNAL NET1 NET2 (rule (clearance 200 (type wire_wire)) (clearance 300 (type wire_via))))
```
produces matrix classes `SIGNAL`, `SIGNAL-via`, `SIGNAL-smd`, `SIGNAL-pin`, `SIGNAL-area`
(the last four from `createDefaultClearanceClasses` triggered by `containsWireClearancePair`),
with `(SIGNAL,SIGNAL)=200` and `(SIGNAL,SIGNAL-via)=(SIGNAL-via,SIGNAL)=300`.

### 7.7 Class-pair rules (`(class_class …)`)

`NetClass.readClassClassScope` (`NetClass.java:145-180`) reads
`(class_class (classes A B C) (rule …) (layer_rule …))`. `Network.insertClassPairs`
(`Network.java:545-576`) resolves each name via `KiCadNetClassNames.resolveNetClass` and applies
`addMixedClearanceRule` to every ordered pair.

> **Iteration bug preserved from v19.** The inner loop reuses the *same* iterator:
> `Iterator<String> it2 = it1;` (`Network.java:557`). It is therefore not an O(n²) pairing over
> `{A,B,C}` but a single pass that consumes the outer iterator. With `(classes A B C)` you get
> exactly the pairs `(A,B)` and `(A,C)` — never `(B,C)`, and `it1.hasNext()` is false after the
> first outer iteration. Any Rust reimplementation that does the "obvious" nested loop will
> produce a *different clearance matrix* on boards that use 3+ classes in one `class_class`.

`addMixedClearanceRule` (`Network.java:622-679`): both class names are appended to the matrix if
missing; with no `(type …)` it sets the symmetric pair directly; with `(type a_b)` it applies the
rule **twice**, once as `(first.a, second.b)` and once as `(second.a, first.b)`.

### 7.8 The default clearance matrix, summarized

| Index | Name | Origin |
|---|---|---|
| 0 | `null` | `ClearanceMatrix.getDefaultInstance` — the "no clearance" class. `BoardRules.clearanceClassNone() == 0`. |
| 1 | `default` | ditto. `BoardRules.defaultClearanceClass() == 1`. `"wire"` aliases to it. |
| 2..5 | `via`, `smd`, `pin`, `area` | appended in that order by `Structure.createDefaultClearanceClasses` when a `wire_*` pair is seen |
| … | `<netclass>`, `<netclass>-via`, `<netclass>-smd`, `<netclass>-pin`, `<netclass>-area` | per net class, appended lazily |

`getNo` is **case-insensitive** (`ClearanceMatrix.java:58-65`) but `getName` returns the stored
casing.

Item → clearance class resolution at insert time uses `DefaultItemClearanceClasses.ItemClass`
ordinals `{NONE=0, TRACE=1, VIA=2, PIN=3, SMD=4, AREA=5}`
(`rules/DefaultItemClearanceClasses.java:42-49`). `setAll(index)` writes indices **1..5** and
leaves `NONE` alone (`DefaultItemClearanceClasses.java:35-39`). A fresh instance is
`setAll(1)` — everything defaults to the `default` class.

---

## 8. `library` scope — padstacks and packages

### 8.1 `(padstack <name> (shape …)* (attach on|off) (absolute on|off))`

`Library.readPadstackScope`, `Library.java:101-224`.

```
padstackName = nextToken as String, then .replaceAll("\\.\\d+", "")
isDrillable   = true    (i.e. attachAllowed)   ← (attach off) sets it false
placedAbsolute = false                          ← (absolute on) sets it true
shapeList = every (shape <shape>)
```
Inside a `(shape …)` scope, extra nested scopes after the shape are skipped
(`Library.java:132-137`) — this is where KiCad's `(reduced …)` and `(connect …)` go.

If a padstack with that name already exists, the scope is parsed and **discarded**
(`Library.java:158-161`), first-definition-wins.

Shape → per-layer `ConvexShape[layerCount]` conversion (`Library.java:169-221`):

```
for each padShape:
    s = padShape.transformToBoardRel(coordinateTransform)      # RELATIVE — no base offset
    if s is ConvexShape:  convex = s
    else:
        if s is PolygonShape: s = s.convexHull()               # non-convex pads are hulled!
        pieces = s.splitToConvex()
        if pieces.length != 1: warn                            # and silently take pieces[0]
        convex = pieces[0]; if Simplex: convex = convex.simplify()
    if convex.dimension() < 2:                                  # zero-area pad
        warn "we will enlarge it as a workaround"
        convex = convex.offset(1)                               # ONE board unit
        if still < 2: convex = null
    if padShape.layer is PCB or SIGNAL:  Arrays.fill(shapes, convex)   # all layers
    else:  shapes[layerStructure.getNo(padShape.layer.name)] = convex
padstacks.add(name, shapes, isDrillable, placedAbsolute)
```

**Non-convex pads are replaced by their convex hull.** This is why a KiCad "custom" pad shape or
a chamfered pad becomes a fat convex blob in Freerouting's DRC model.

`Padstack` (`core/library/Padstack.java`):
* `fromLayer()` = first index with a non-null shape; `toLayer()` = last such index
  (`Padstack.java:137-152`). A layer gap in the middle is *not* representable — the range is
  contiguous from the router's point of view even if `shapes[i] == null` inside.
* `attachAllowed` — "vias of the own net may overlap this padstack".
* `placedAbsolute` — **if false, the padstack's layers are mirrored when the component is on the
  back side** (`Padstack.java:24-28`). See §9.2.
* `getDrillRadius()` (`Padstack.java:72-112`) *parses the padstack name* looking for
  `…_<outer>:<drill>[_…]`, computes `actualOuterRadius * (drillDia/outerDia)`, else falls back to
  `getSmallestRadius() * 0.45`. Cached in `cachedDrillRadius`. Only used for rendering and hole
  clearance, but it means padstack **names carry geometry** in KiCad exports (`Via[0-1]_800:400_um`).
* `getTraceExitDirections(layer, factor)` (`Padstack.java:168-195`) only returns directions for
  `IntBox`/`IntOctagon` shapes: `allDirs = max(w,h) < factor * min(w,h)`; if `allDirs` or
  `w >= h` add `RIGHT|LEFT`; if `allDirs` or `w <= h` add `UP|DOWN`.

### 8.2 `(image <name> (side front|back) (pin …)* (outline …)* (keepout …)* (via_keepout …)* (place_keepout …)*)`

`Package.readScope`, `Package.java:57-150`. `(side back)` ⇒ `isFront = false`
(`readPlacementSide` returns `nextToken != Keyword.BACK`, so *any* token other than `back` means
front — including a typo).

`(pin <padstack> [(rotate <deg>)] <pinName> <x> <y> [(rotate <deg>)] …)`, `readPinInfo`,
`Package.java:249-330`:

```
yybegin(NAME); padstackName = nextToken.toString()     # accepts String OR Integer
yybegin(NAME); tok = nextToken
if tok == '(':                                          # the "rotate-before-name" variant
    tok2 = nextToken
    if tok2 == ROTATE: rotation = readRotation()
    else: skipScope
    yybegin(NAME); tok = nextToken
pinName = tok.toString()                                # accepts String OR Integer
x = nextToken (Double|Integer);  y = nextToken (Double|Integer)
then loop over trailing sub-scopes: (rotate <deg>) sets rotation; everything else skipped
```

`readRotation` (`Package.java:332-352`) reads the angle with `scanner.nextString()` and
`Double.parseDouble` — so `(rotate 90)`, `(rotate 90.0)` and `(rotate -45.5)` all work.

**Numeric pin ids.** Both the padstack reference and the pin name accept `Integer` and are
`toString()`-ed. This is exactly what KiCad emits (`(pin Round[A]Pad_600_um 1 0 0)` — pin name
`1` is an `Integer` token). Two `yybegin(NAME)` calls are what make it work; a parser that only
sets `NAME` once loses either the padstack or the pin name.

### 8.3 Package materialization (`Library.readScope`, `Library.java:261-452`)

Per package:
* Pin: `relCoor = IntVector(round(dsnToBoard(x)), round(dsnToBoard(y)))`; padstack looked up by
  `padstackName.replaceAll("\\.\\d+","")`; **if the padstack is missing the whole library read
  fails** (`return false`, `Library.java:333`).
* Outline shapes: transformed with `transformToBoardRel`. Three parallel arrays are built
  (**v2-only**, `Library.java:339-366`):
  - `outlineWidths[i]` = `path.width` for `Path` subclasses, `0.0` for polygons/rects.
  - `outlineIsClosed[i]` = for a `Path`, whether first and last coordinate pair are identical;
    `true` for non-path shapes.
* Keepout names: `generateMissingKeepoutNames` (`Library.java:454-472`) — if **any** keepout in a
  list has a null name, **all** of them are renamed `keepout_1`, `keepout_2`, … (respectively
  `via_keepout_N`, `place_keepout_N`). Names matter because `(place … (keepout <name>
  (clearance_class …)))` binds clearance overrides by name.
* Keepout layer: taken from the **first shape** of the area (`shapeList.iterator().next().layer`),
  storing `layer.no` — which is `-1` for `Layer.SIGNAL`/`Layer.PCB`, meaning "all signal layers".

**Package deduplication (v2-only, `Library.java:409-449`).** KiCad exports the same footprint
name multiple times when pad stacks differ. v2 strips a `::<digits>` suffix to get
`basePackageName`, then probes `basePackageName`, `basePackageName::1`, `::2`, … For each
candidate it calls `packages.get(testName, isFront)`; if nothing matches by exact name it adds
under that name and stops; if an existing package with the same name has **identical pins**
(`arePackagePinsIdentical`, tolerance 0.001 board units on x/y and on rotation,
`Library.java:226-259`) it reuses it; otherwise it bumps the suffix. `Packages.get(name, isFront)`
(`core/library/Packages.java:27-52`) does a case-insensitive scan, prefers the matching side,
falls back to the other side, and also retries after stripping `::\d+$`.

Note: there is no separately-stored "back side" package geometry. `Network.insertComponent`
fetches `packages.get(libKey, true)` and `packages.get(libKey, false)`
(`Network.java:939-940`); on a normal DSN both calls return the *same* `Package` object (the
side-fallback in `Packages.get`). The mirroring is done at pin level, not by storing a mirrored
package. See §9.2.

---

## 9. Placement, components, and the exact mirroring math

### 9.1 `(place <name> <x> <y> front|back <rotation> …)`

`Component.readPlaceScope`, `Component.java:187-309`.

```
name = scanner.nextString(true)                   # ignoreNewline = true
x, y = two numeric tokens
   ↳ if the FIRST of them is ')' → the component is "not yet placed":
        ComponentLocation(name, coor=null, isFront=true, rotation=0, positionFixed=false, …)
side = BACK ? back : front           # anything not FRONT warns but is treated as front
rotation = numeric token (Double or Integer)
then, while next token is '(' :
    (lock_type … position …)  → positionFixed = true      # readLockType scans for POSITION
    (pin  <name> (clearance_class <n>))        → pin_infos[name]
    (keepout <name> (clearance_class <n>))     → keepout_infos[name]
    (via_keepout …)                            → via_keepout_infos[name]
    (place_keepout …)                          → place_keepout_infos[name]
    (PN <string>)  or  ("PN" <string>)         → partNumber
    else skipScope
require ')'
```

`readItemClearanceInfo` (`Component.java:311-350`) accepts the inner key as the `CLEARANCE_CLASS`
keyword **or** as a raw string `"clearance_class"` / `"clearanceClass"` (case-insensitive) —
another v2 robustness addition. If no clearance class is found the whole info record is rejected
(`null`), which aborts the enclosing `place` scope.

`ComponentPlacement.ComponentLocation` fields (`ComponentPlacement.java:24-79`): `name`,
`coor[2]`, `isFront`, `rotation` (degrees), `positionFixed`, four `Map<String, ItemClearanceInfo>`,
`partNumber`.

### 9.2 Component insertion and the mirroring math (`Network.insertComponent`, `Network.java:934-1204`)

```
frontPkg = library.packages.get(libKey, true);  backPkg = library.packages.get(libKey, false)
if either is null: warn and return
componentLocation = coor == null ? null : coordinateTransform.dsnToBoard(coor).round()
component = board.components.add(name, componentLocation, rotationInDegree, isFront,
                                 frontPkg, backPkg, positionFixed, partNumber)
if componentLocation == null: return             # unplaced components have no board items
componentTranslation = componentLocation - Point.ZERO
fixedState = positionFixed ? SYSTEM_FIXED : UNFIXED
```

`board.model.structure.Component` normalizes rotation into `[0, 360)` in the constructor by
repeated ±360 (`Component.java:66-72`) and returns `libPackageFront` or `libPackageBack` from
`getPackage()` based on `onFront` (`Component.java:238-246`).

#### 9.2.1 Pin center (`Pin.relativeLocation`, `board/model/items/Pin.java:65-89`)

```
relLocation = packagePin.relativeLocation                   # from the library, front-side coords
componentRotation = component.getRotationInDegree()

if (!onFront && !flipStyleRotateFirst):
    relLocation = relLocation.mirrorAtYAxis()               # MIRROR FIRST

if componentRotation % 90 == 0:
    k = (int)componentRotation / 90
    if k != 0: relLocation = relLocation.turn90Degree(k)    # exact integer rotation
else:
    p = relLocation.toFloat().rotate(toRadians(componentRotation), FloatPoint.ZERO)
    relLocation = p.round().differenceBy(Point.ZERO)        # approximate rotation

if (!onFront && flipStyleRotateFirst):
    relLocation = relLocation.mirrorAtYAxis()               # MIRROR LAST

pinCenter = component.getLocation() + relLocation
```

`Pin.getCenter` (`Pin.java:92-120`) then **validates** the center against the pad shape: if the
computed center is not `containsInside` the first non-null pad shape, it is replaced by
`currentShape.centreOfGravity().round()`. Cached in the item's `center` field.

#### 9.2.2 Pad shape (`Pin.getShape`, `Pin.java:165-245`) — all shapes computed at once

```
mirrorOnYaxis = !onFront && !flipStyleRotateFirst
relLocation   = mirrorOnYaxis ? packagePin.relativeLocation.mirrorAtYAxis()
                              : packagePin.relativeLocation
componentTranslation = component.getLocation() - ZERO

for shapeIndex in 0 .. (padstack.toLayer()-padstack.fromLayer()):
    padstackLayer = getPadstackLayer(shapeIndex)                     # ← the layer flip, below
    shape = padstack.getShape(padstackLayer);  if null: continue

    # 1. pin's own rotation, about the origin
    pinRotation = packagePin.rotationInDegree
    if pinRotation % 90 == 0:  shape = shape.turn90Degree(pinRotation/90, Point.ZERO)
    else:                      shape = shape.rotateApprox(toRadians(pinRotation), FloatPoint.ZERO)

    # 2. mirror-first flip of the PAD ITSELF
    if mirrorOnYaxis:          shape = shape.mirrorVertical(Point.ZERO)

    # 3. translate into package coordinates
    shape = shape.translateBy(relLocation)

    # 4. component rotation, about the origin
    if componentRotation % 90 == 0: shape = shape.turn90Degree(componentRotation/90, Point.ZERO)
    else:                           shape = shape.rotateApprox(toRadians(componentRotation), FloatPoint.ZERO)

    # 5. rotate-first flip
    if !onFront && flipStyleRotateFirst:  shape = shape.mirrorVertical(Point.ZERO)

    # 6. translate to the board
    shapes[shapeIndex] = shape.translateBy(componentTranslation)
```

Notes:
* `mirrorAtYAxis` (for vectors) and `mirrorVertical(pole)` (for shapes) are the same operation:
  negate `x`. The pole for shapes here is always `Point.ZERO`, so it is a pure `x → -x`.
* Both flip modes mirror about **the vertical axis** (x negation). `flip_style rotate_first`
  changes only the *order*: rotate-then-mirror instead of mirror-then-rotate. For a component at
  rotation θ on the back side, the two orders differ by an effective rotation of `-θ` vs `+θ`.
  `Component.rotate` compensates for interactive rotation with `turnAngle = 360 - angle` when
  `flipStyleRotateFirst && !onFront` (`Component.java:132-136`).
* Exact `turn90Degree` is used whenever the angle is a multiple of 90 (integer arithmetic, no
  rounding); otherwise `rotateApprox` with rounding. The `%` test is on the raw `double`, so
  `90.0000001` takes the approximate path.
* `flipStyleRotateFirst` is a **board-global** flag on `board.components`, set either from
  `(place_control (flip_style rotate_first))` (`PlaceControl.readScope`, `PlaceControl.java:68-70`)
  or from a bare `(flip_style …)` inside the `structure` scope, because Electra puts it there
  (`Structure.java:1014-1015`, `Structure.java:1041-1043`).

#### 9.2.3 Layer flip for back-side components

`Pin.getPadstackLayer(index)` (`Pin.java:248-258`):
```
if (component.placedOnFront() || padstack.placedAbsolute):
      padstackLayer = index + firstLayer()
else: padstackLayer = padstack.boardLayerCount() - index - firstLayer() - 1
```
`DrillItem.firstLayer()/lastLayer()` (`board/model/items/DrillItem.java:162-184`):
```
if (isPlacedOnFront() || padstack.placedAbsolute):
      firstLayer = padstack.fromLayer();                 lastLayer = padstack.toLayer()
else: firstLayer = boardLayerCount - padstack.toLayer() - 1
      lastLayer  = boardLayerCount - padstack.fromLayer() - 1
```
Both are memoized (`precalculatedFirstLayer/-LastLayer`, cleared by `clearDerivedData`).

So `placedAbsolute == true` disables *both* the layer flip and nothing else — the geometric
mirroring in §9.2.2 still applies. That asymmetry is deliberate: `(absolute on)` means "the
padstack's per-layer shapes are already given in board-layer order".

#### 9.2.4 Pin nets and clearance class

```
pinNets = netlist.getNets(componentName, pinName)      # linear scan over ALL nets, O(nets × pins)
netNumberArray = [board.rules.nets.get(name, subnet).netNumber for each]
netClass = netNumberArray.isEmpty() ? defaultNetClass
                                    : nets.get(netNumberArray[0]).getNetClass()
clearanceClass = -1
if location.pin_infos[pinName] exists: clearanceClass = clearanceMatrix.getNo(that name)
if clearanceClass < 0:
    clearanceClass = (padstack.fromLayer() == padstack.toLayer())
                       ? netClass.defaultItemClearanceClasses[SMD]
                       : netClass.defaultItemClearanceClasses[PIN]
board.insertPin(component.id, pinIndex, netNumberArray, clearanceClass, fixedState)
```
**SMD vs through-hole is decided purely by `fromLayer() == toLayer()`** — a single-copper-layer
padstack is an SMD pad. `Pin.drillAllowed()` uses the same test (`Pin.java:349`), which is what
lets fanout drop a via directly on an SMD pad.

#### 9.2.5 Package keepouts → board obstacles

Three passes `k = 0,1,2` over `keepouts` / `viaKeepouts` / `placeKeepoutArr`
(`Network.java:1039-1157`):

```
layer = keepout.layer
if layer >= board.getLayerCount(): warn, skip
if layer >= 0 && !isFront:  layer = board.getLayerCount() - keepout.layer - 1      # LAYER FLIP
clearanceClass = defaultNetClass.defaultItemClearanceClasses[AREA]
if location.<k>_keepout_infos[keepout.name] resolves to index > 0: use it
if layer >= 0:
    k==0 → board.insertObstacle(area, layer, translation, rotation, sideChanged=!isFront,
                                clearanceClass, componentId, keepoutName, fixedState)
    k==1 → board.insertViaObstacle(...)      # ViaObstacleArea
    k==2 → board.insertComponentObstacle(...)# ComponentObstacleArea
else:                                        # layer == -1 ⇒ "all signal layers"
    for each signal layer j: same insert with layer = j
```
The `sideChanged = !isFront` argument makes the `ObstacleArea` apply the same mirror-then-rotate
transform to its relative area (see `ObstacleArea`'s `translation`/`rotationInDegree`/
`sideChanged` fields).

#### 9.2.6 Package outline → `ComponentOutline` (v2-only classification)

`Network.java:1158-1203`:
```
courtyardIdx = index of the outline shape with the LARGEST bounding-box area (only if >1 outline)
for each outline i:
    isCourtyard  = (i == courtyardIdx) || outlineWidths[i] == 0.0
    isFabrication= !isCourtyard && outlineWidths[i] <= 110.0
    isClosed     = outlineIsClosed[i]
    board.insertComponentOutline(outline[i], isFront, translation, rotation, componentId,
                                 isCourtyard, isFabrication, isClosed, fixedState)
```
`110.0` is a raw **board-unit** width threshold (not DSN units — `outlineWidths` is the DSN path
width copied verbatim at `Library.java:350`, so this constant is effectively "DSN width units",
which makes it resolution-dependent). It only affects rendering/classification, not routing.
v19 called `insert_component_outline(outline[i], isFront, translation, rotation, componentId,
fixedState)` with no classification (v19 `Network.java:1142-1149`).

### 9.3 Net classes → routing constraints

| DSN construct | Board effect | Site |
|---|---|---|
| `(rule (width w))` in a class | `NetClass.setTraceHalfWidth(round(dsnToBoard(w/2)))` on all layers | `Network.java:485-486` |
| `(layer_rule L (rule (width w)))` | `setTraceHalfWidth(layerIndex, …)` | `Network.java:509-511` |
| `(circuit (use_layer L1 L2))` | `createActiveTraceLayers`: **all** layers deactivated, then the listed ones activated; **every inactive layer gets `traceHalfWidth = 0`** | `Network.java:711-728` |
| `(circuit (use_via V1 V2))` | `createViaRule`: a new `ViaRule` named after the net class, containing every `ViaInfo` whose clearance index equals `netClass.defaultItemClearanceClasses[VIA]` **and** whose padstack name equals one of the listed names | `Network.java:689-709` |
| `(circuit (length max min))` | `setMaximumTraceLength(dsnToBoard(max))`, `setMinimumTraceLength(dsnToBoard(min))`, only when `> 0` | `Network.java:466-471` |
| `(shove_fixed on)` | `NetClass.setShoveFixed(true)` — see `shove.md` | `Network.java:527` |
| `(pull_tight off)` | `NetClass.setPullTight(false)` — see `pull-tight-insert.md` | `Network.java:526` |
| `(clearance_class N)` | `setTraceClearanceClass(matrix index)` | `Network.java:446-456` |
| `(via_rule R)` | `setViaRule(rules.getViaRule(R))` | `Network.java:457-465` |

`NetClass` defaults (`rules/NetClass.java:39-54`): `activeRoutingLayerArr[i] = layer[i].isSignal`,
`pullTight = true`, `shoveFixed = false`, min/max trace length 0.
`BoardRules.createDefaultNetClass` (`BoardRules.java:202-210`) creates the class named `default`
with `traceHalfWidth = 1500` and `traceClearanceClass = 1`.

`appendNetClass(name)` copies from class 0: a **copy** of `defaultItemClearanceClasses`, the via
rule, the layer-0 trace half width (applied to all layers), and the trace clearance class
(`BoardRules.java:225-240`). `appendNetClass()` (no name) does **not** copy
`defaultItemClearanceClasses` — a real asymmetry.

### 9.4 Via infos and via rules

`(via <viaName> <padstackName> <clearanceClassName> [attach])` — `Network.readViaInfo`
(`Network.java:255-322`):
```
each of the three names is read with an explicit yybegin(NAME) first
padstack = library.getViaPadstack(padstackName)
   if null: padstack = library.padstacks.get(padstackName)     # not yet promoted to a via padstack
            if null: warn, return null
            library.addViaPadstack(padstack)                   # promote it
clearanceClass = clearanceMatrix.getNo(name);  if < 0 → BoardRules.defaultClearanceClass() (= 1)
optional trailing `attach` keyword → attachAllowed = true
```
Note: the `.\d+` name cleanup is **not** applied here.

`(via_rule <ruleName> <viaName>…)` — `Network.readViaRule` returns the raw name list;
`Network.insertViaRules` (`Network.java:379-395`):
```
for each list with size >= 2:  addViaRule(list, board)
if no rule was successfully added: rules.createDefaultViaRule(defaultNetClass, "default")
for EVERY net class: setViaRule(rules.getDefaultViaRule())        # ← overwrites, then §7.4 re-sets
```
`addViaRule` (`Network.java:398-421`) resolves each via name through `rules.viaInfos.get(name)`;
any missing name makes `ruleOk = false` and the rule is **not** added (but the partially built
`ViaRule` object is discarded). An existing rule with the same name is removed first.

`insertViaInfos` (`Network.java:347-357`): if the file declared **no** `(via …)` info scopes,
`createDefaultViaInfos(board, defaultNetClass, viaAtSmdAllowed)` synthesizes one `ViaInfo` per via
padstack, named exactly `padstack.name` for the default class (and `padstack.name + "-" +
className` otherwise), with clearance class `defaultItemClearanceClasses[VIA]` and
`attachAllowed = viaAtSmdAllowed && padstack.attachAllowed`.

`BoardRules.createDefaultViaRule` (`BoardRules.java:169-200`) is the *deduplicating* variant: it
groups candidate `ViaInfo`s by `(fromLayer, toLayer)` and keeps only the one whose
`padstack.getShape(fromLayer).maxWidth()` is smallest. Only via infos whose clearance class equals
the net class's `[VIA]` index are considered. Order in a `ViaRule` is priority order — the maze
engine prefers earlier entries (`ViaRule.java:11-13`).

`getDefaultViaRule()` is simply `viaRules.getFirst()` (`BoardRules.java:242-248`).

### 9.5 `(net <name> [subnet] (pins …) | (order …) | (fromto …) (rule …))`

`Network.readNetScope`, `Network.java:1326-1464`.

```
netName = scanner.nextString()
tok = nextToken; if Integer → subnetNumber = tok else subnetNumber = 1
scopeIsEmpty = (tok == ')')
(pins  A-1 B-2 …)  → pinList
(order A-1 B-2 …)  → pinList, pinOrderFound = true
(fromto A-1 B-2)   → one subnet pin list (a TreeSet)
(rule (width w))   → netRules
(layer_rule …)     → warned as "not yet implemented", skipped

if no (fromto …): subnetPinLists = pinOrderFound ? createOrderedSubnets(pinList)   # consecutive pairs
                                                 : [pinList]                        # one subnet
for each subnetPinList (index increments subnetNumber each time!):
    netId = Net.Id(netName, subnetNumber)
    if not in netlist:
        netlist.addNet(netId)
        board.rules.nets.add(name, subnet, layerStructure.containsPlane(netName))
    currentSubnet.setPins(pinList)
    for each netRule of type WidthRule:
        halfWidth = round(dsnToBoard(w) / 2)
        netRule = board.rules.netClasses.find(halfWidth, defaultClass.traceClearanceClass,
                                              defaultClass.viaRule)
        if netRule == null: netRule = board.rules.getNewNetClass()      # auto-generated name
        netRule.setTraceHalfWidth(halfWidth)
        boardNet.setClass(netRule)
    ++subnetNumber
```

`createOrderedSubnets` (`Network.java:193-210`): for `(order A B C D)` produces the sequential
2-pin subnets `{A,B}, {B,C}, {C,D}`.

#### 9.5.1 `readNetPins` and the `-` separator

`Network.readNetPins`, `Network.java:212-253`:
```
while ((componentName = scanner.nextString(true, '-')).length() > 0):
    scanner.yybegin(SPEC_CHAR); scanner.nextToken()     # eat exactly the '-'
    pinName = scanner.nextString(true)
    pinList.add(Net.Pin(componentName, pinName))
then one nextToken must be ')'  (a warning only if it isn't)
```
`nextString(true, '-')` means "skip leading whitespace/newlines **and** `-`, and stop at
whitespace/brackets **or** `-`". So `U1-3` yields `"U1"`, then `SPEC_CHAR` consumes the `-`,
then `"3"`. Component names may contain `-` only if quoted (`"MY-PART"-3` works because the
quoted-string path uses a different `stopAt` set).

`Net.Pin` is `(componentName, pinName)` with a lexicographic `compareTo`
(`Net.java:105-128`) — pins are stored in `TreeSet`s.

`Net.Id` is `(name, subnetNumber)` with lexicographic-then-numeric `compareTo`
(`Net.java:84-102`). **`Net.Id.compareTo` is case-sensitive**, while
`Nets.get(name, subnet)` on the board side is `equalsIgnoreCase` (`Nets.java:41-49`). A DSN with
`VCC` and `vcc` gets two `NetList` entries but one board net.

### 9.6 Logical parts (pin/gate swap)

`PartLibrary.readScope` collects `(logical_part_mapping <part> (comp C1 C2 …))` and
`(logical_part <name> (pin <pinName> 0 <gateName> <gateSwapCode> <gatePinName>
<gatePinSwapCode>)*)`. `Network.insertLogicalParts` (`Network.java:844-898`) resolves each part's
pin names to package pin indices via `libPackage.getPinIndex(name)` — **any unresolved pin aborts
the whole import** (`return false`, `Network.java:861`, though the caller ignores the return
value at `Network.java:1322`). `Pin.getSwappablePins` (`Pin.java:390+`) then groups by
`(gateName, gatePinSwapCode)`.

---

## 10. `wiring` scope — re-importing existing copper

`Wiring.readScope`, `Wiring.java:307-354`. Two record types plus conduction areas.

### 10.1 `(wire <shape> (net <name> [subnet]) (clearance_class <n>) (type normal|fix|shove_fixed|protect))`

`readWireScope`, `Wiring.java:356-577`:

```
shape source (mutually exclusive):
  (path L w x0 y0 x1 y1 …)          → PolygonPath   (corner list)
  (polyline_path L w ax ay bx by …) → PolylinePath  (LINE list: 4 numbers per line)
  (rect|polygon|circle …)           → borderShape   → a CONDUCTION AREA, not a trace
  (window <shape>)                  → hole of the conduction area
if neither path nor borderShape: warn + record warning + skip

nets  = getSubnets(netId, rules)     # subnet>0 → exact; subnet==0 → ALL subnets of that name
netClass = last found net's class, else defaultNetClass
clearanceClassIndex = clearance_class ? matrix.getNo(name) : -1
layerIndex = path ? path.layer.no : borderShape.layer.no
halfWidth  = path ? round(dsnToBoard(path.width/2)) : 0
if layerIndex out of range: warn + record warning + skip      # v2 only; v19 threw
```

Then, by shape kind:

* **Conduction area** (`borderShape != null`): clearance defaults to
  `netClass.defaultItemClearanceClasses[AREA]`; area built from border + windows via
  `Shape.transformAreaToBoard`; `board.insertConductionArea(area, layer, nets, cl, isObstacle=false,
  fixed)`.
* **`PolygonPath`**: clearance defaults to `[TRACE]`. Each corner is transformed and
  **bounds-checked against `board.getBoundingBox()`** — a corner outside the box aborts the whole
  wire with a warning (`Wiring.java:498-510`). Then a degeneracy test: fewer than 2 corners, or
  all corners identical after rounding ⇒ skip with a warning ("KiCad 4.0.7 exports 3 identical
  vertices"). Otherwise `new Polyline(polygon)` and
  `board.insertTraceWithoutCleaning(polyline, layer, halfWidth, nets, cl, fixed)`.
* **`PolylinePath`**: clearance defaults to `[TRACE]`. Each group of 4 numbers becomes a
  `Line(round(a), round(b))`; `new Polyline(lines)`; same insert. **No bounds check and no
  degeneracy check on this path.**

`tryCorrectNet` (`Wiring.java:583-599`): if the inserted item ended up with zero nets, look at the
normal contacts at both endpoints and adopt the first contact that has exactly one net. Comment
attributes this to Mentor "turret" traces.

After the whole scope, `board.normalizeAllTraces()` is called inside a `try/catch` that downgrades
any exception to a warning (`Wiring.java:345-352`). Normalization is deferred to this point on
purpose — the comment at `Wiring.java:532` says "Traces are not yet normalized here because cycles
may be removed premature". See `pull-tight-insert.md` §7.4 for what normalization does.

### 10.2 `(via <padstack> <x> <y> (net …) (clearance_class …) (type …))`

`readViaScope`, `Wiring.java:601-714`:
```
padstack = library.padstacks.get(padstackName.replaceAll("\\.\\d+",""))
   if null: warn + record warning + return false      # ← aborts the ENTIRE wiring scope
clearanceClassIndex defaults to netClass.defaultItemClearanceClasses[VIA]
boardLocation = dsnToBoard(location).round()
if viaExists(boardLocation, padstack, nets, board): warn "duplicate via skipped"   # v2-only
else: board.insertVia(padstack, loc, nets, cl, fixed,
                      attachAllowed = viaAtSmdAllowed && padstack.attachAllowed)
```

`viaExists` (`Wiring.java:238-255`) picks items at the location on `padstack.fromLayer()` with a
VIAS filter and compares net set, exact center, `firstLayer()`, and `lastLayer()`.

> **Live bug (v2 and v19).** In `readViaScope` the net-number array is filled with
> `netNumbers[currentIndex] = currentNet.netNumber;` but `currentIndex` is **never incremented**
> (`Wiring.java:684-689`; v19 does increment it — v19 `Wiring.java:62-64`). A via on a net name
> with more than one subnet therefore gets `[lastSubnetNumber, 0, 0, …]` instead of the full set.
> Since `(net X)` without a subnet number returns *all* subnets, this bites on multi-subnet nets.
> **Do not reproduce this**; treat the v19 behavior as the intent.

### 10.3 `FixedState` semantics

`board/model/structure/FixedState.java`: `UNFIXED < SHOVE_FIXED < USER_FIXED < SYSTEM_FIXED`
("the strongest fixed states came last"; note the inline comments on `SHOVE_FIXED`/`USER_FIXED`
are swapped relative to their names).

`Wiring.calcFixed` (`Wiring.java:257-278`) maps the `(type …)` token:
```
shove_fixed → SHOVE_FIXED
fix         → SYSTEM_FIXED
normal      → UNFIXED
anything else (including "protect") → USER_FIXED
```
So `protect` and any unknown token both become `USER_FIXED`.

What each level means downstream:
* `UNFIXED` — freely rippable and shovable.
* `SHOVE_FIXED` — may not be shoved, may still be ripped up.
* `USER_FIXED` — user-protected: neither shoved nor ripped.
* `SYSTEM_FIXED` — board-owned geometry (keepouts, outline holes, fixed component pins/keepouts).
  **`SesWriter.writeNet` skips every `SYSTEM_FIXED` item** (`SesWriter.java:339-341`), which is how
  DSN-imported keepouts and fixed component geometry stay out of the session file.

Write-side mapping differs between the DSN writer and the SES writer:
* `Wiring.writeFixedState` (`Wiring.java:207-221`): `UNFIXED` → nothing; `SHOVE_FIXED` →
  `(type shove_fixed)`; `SYSTEM_FIXED` → `(type fix)`; else → `(type protect)`.
* `SesWriter.writeFixedState` (`SesWriter.java:478-490`): **anything ≤ `SHOVE_FIXED` writes
  nothing** (so `shove_fixed` is lost in SES); `SYSTEM_FIXED` → `(type fix)`; else →
  `(type protect)`.

### 10.4 Insert-time behavior of re-imported wires

`insertTraceWithoutCleaning` (`BasicBoard.java:178`) inserts without split/merge/normalize;
`insertTrace` (`BasicBoard.java:209`) additionally cleans. The wiring reader deliberately uses the
former and defers to `normalizeAllTraces()`. `SesReader` uses the *cleaning* `insertTrace`
(`SesReader.java:323`) and marks everything `USER_FIXED`.

---

## 11. Keepouts, planes, conduction areas — the obstacle model

### 11.1 Reading an area scope (`Shape.readAreaScope`, `Shape.java:169-251`)

```
(keepout <optional name> <shape> (window <shape>)* (clearance_class <n>) …)
```
* The first token, if it is a `String`, is the area name (empty string ⇒ null name). It also
  becomes the scanner's `scopeIdentifier` for error messages.
* The first shape is the border; every `(window <shape>)` adds a hole.
* `skipWindowScopes = true` makes windows be *skipped* — used only for Cadence Allegro power
  planes (`Plane.readScope`, `Plane.java:57-59`): "Allegro cutouts the pins on power planes, which
  leads to performance problems when dividing a conduction area into convex pieces."
* Returns `null` on any failure, which callers mostly tolerate.

`Shape.transformAreaToBoard` (`Shape.java:522-555`): the first shape becomes the boundary; if there
are holes, the boundary **must** be a `PolylineShape` (else null) and the result is
`new PolylineArea(border, holes)`. Otherwise the boundary shape itself is the area.

### 11.2 Structure-level keepouts (`Structure.insertKeepout`, `Structure.java:857-933`)

```
area = Shape.transformAreaToBoard(shapeList, coordinateTransform)
if area.dimension() < 2:
    warn "Keepout zone '<name>' was skipped because its geometry is degenerate"
    return true                                   # NOT an error — routing simply gets permissive
layer = first shape's layer
if layer == Layer.SIGNAL:  insert on EVERY signal layer
elif layer.no >= 0:        insert on that layer
else:                      warn "unknown layer name", return false     # aborts the structure scope

clearanceClassIndex = clearanceClassName == null
        ? defaultNetClass.defaultItemClearanceClasses[AREA]
        : (matrix.getNo(name) >= 0 ? that : BoardRules.clearanceClassNone() /* 0 */ with a warning)

keepout       → board.insertObstacle(area, layer, cl, SYSTEM_FIXED)
via_keepout   → board.insertViaObstacle(...)
place_keepout → board.insertComponentObstacle(...)
```

The degenerate-keepout tolerance is a **v2 addition** (v19 returned `false`, aborting the read on
KiCad 4.0.7 files).

### 11.3 How the router treats each area type

| Class | `isObstacle(other)` | `isTraceObstacle(net)` | `isDrillable(net)` |
|---|---|---|---|
| `ObstacleArea` (plain keepout) | `!other.sharesNet(this) && (other instanceof Trace \|\| other instanceof Via)` (`ObstacleArea.java:175-180`) | inherited: true unless net shared | inherited |
| `ViaObstacleArea` (via keepout) | `!sharesNet && other instanceof Via` (`ViaObstacleArea.java:92-97`) | **always false** (`:100-102`) — traces pass freely | — |
| `ComponentObstacleArea` (place keepout) | placement-only; not a routing obstacle | — | — |
| `ConductionArea` | `isObstacle ? super.isObstacle(other) : false` (`ConductionArea.java:380-385`) | `isObstacle && !containsNet(net)` (`:397-400`) | `!isObstacle \|\| containsNet(net)` (`:402-405`) |

`ObstacleArea` is constructed with `netNumbers = new int[0]` for keepouts, so `sharesNet` is
always false and every keepout blocks every trace/via. `ViaObstacleArea` likewise passes
`new int[0]` (`ViaObstacleArea.java:63`).

**`isObstacle` on `ConductionArea` is a per-item flag, and every conduction area created by the
DSN importer passes `isObstacle = false`** — `Structure.readScope` plane insertion
(`Structure.java:1108-1114`), `Structure.insertMissingPowerPlanes` (`Structure.java:562-568`), and
`Wiring.readWireScope` (`Wiring.java:484-486`) all pass `false`. Consequence: **imported copper
pours are electrically connectable targets but are not obstacles** — foreign nets route straight
through them. There is a separate global `BoardRules.ignoreConduction` (default `true`,
`BoardRules.java:33`) that the router consults.

### 11.4 `(plane <netName> <shape> (window …)*)`

`Plane.readScope` (`Plane.java:53-83`) only buffers a `PlaneInfo{area, netName}`. Insertion at
the end of `Structure.readScope` (`Structure.java:1068-1122`):
```
netId = Net.Id(netName, 1)                        # planes are ALWAYS subnet 1
if not in netlist: netlist.addNet(netId); board.rules.nets.add(name, 1, containsPlane=true)
net = board.rules.nets.get(netName, 1);  if null: warn and skip this plane
planeArea = transformAreaToBoard(shapeList, transform)
layer = first shape's layer;  if layer.no < 0: warn + return false     # aborts structure scope
clearanceClassIndex = area.clearanceClassName != null
        ? (matrix.getNo(name) >= 0 ? that : 0 with warning)
        : net.getNetClass().defaultItemClearanceClasses[AREA]
board.insertConductionArea(planeArea, layer.no, [net.netNumber], cl, isObstacle=false, SYSTEM_FIXED)
```
Note `board.rules.nets.add(..., containsPlane = true)` is hardcoded `true` here, unlike
`Network.readNetScope` which asks `layerStructure.containsPlane(netName)`.

### 11.5 `insertMissingPowerPlanes` (`Structure.java:526-571`)

For every **non-signal** layer that has no `ConductionArea` and has a non-empty `use_net` list:
take the *first* net name, create the net if needed (subnet 1, `containsPlane = true`), and insert
a conduction area covering the entire `board.boundingBox` with clearance class
`BoardRules.clearanceClassNone()` (= 0) and `SYSTEM_FIXED`.

### 11.6 `adjustPlaneAutorouteSettings` (`DsnFile.java:32-114`)

Runs only when the DSN had **no** `(autoroute_settings …)` scope (`DsnReader.java:131-133`).

```
if layerCount <= 2: return false
if ANY layer is non-signal: return false                 # the file already declares planes
boardArea = Σ area of all convex pieces of every board-outline shape
for each ConductionArea:
    layer = its layer
    if that layer contains any Trace: skip
    if layer is non-signal, or layer == 0, or layer == layerCount-1: skip   # never outer layers
    areaOfThisPour = Σ area of its convex pieces
    if areaOfThisPour < 0.5 * boardArea: skip
    for each of its nets: net.setContainsPlane(true)
    if its fixedState < USER_FIXED: setFixedState(USER_FIXED)
    log "Layer '<name>' has been automatically configured as a dedicated power plane"
```
The `0.5 * boardArea` threshold and the "no traces on that layer" precondition are the whole
heuristic.

---

## 12. SES output format — exactly what is written

`SesWriter.write(board, out, designName)`, `SesWriter.java:55-67`.

* `sessionName = designName.replace(".dsn", ".ses")` (a plain substring replace — a design named
  `my.dsn.board.dsn` becomes `my.ses.board.ses`).
* Identifier quoting for SES uses reserved chars
  `{ "(", ")", " ", ";", "-", "_", "/", "~", "{", "}" }` (`SesWriter.java:62`) — **strictly more
  than the DSN writer's** `{ "(", ")", " ", ";", "-", "_" }` (`WriteScopeParameter.java:35`) and far
  more than v19's SES set `{ "(", ")", " ", "-" }` (v19 `SpecctraSesFileWriter.java:39`). Adding
  `/ ~ { }` in v2 is what makes KiCad net names like `/CLK`, `~{RESET}`, and hierarchical sheet
  paths round-trip.
* The quote character comes from `board.communication.specctraParserInfo.stringQuote` — i.e. it is
  echoed from the input DSN's `(string_quote …)`.
* The stream is **flushed but not closed**.

### 12.1 The full scope tree

```
(session "<name>.ses"
  (base_design "<designName>")
  (placement
    (resolution <unit> <resolution>)
    (component <packageName>
      (place <compName> <x:int> <y:int> front|back <rotation>
        [ (lock_type position) ])
      …)
    …)
  (was_is
    [ (pins <cmpA>-<pinA> <cmpB>-<pinB>) ]* )
  (routes
    (resolution <unit> <resolution>)
    (parser                                        ; "reduced" form
      [(host_cad …)] [(host_version …)] [(constant a b)]* [(write_resolution <c> <n>)])
    (library_out
      (padstack <name>
        (shape (<shape> <layer> …int coords…))*
        [(attach off)])* )
    (network_out
      (net <netName>
        (wire (path <layer> <width:int> x y x y …) [(type fix|protect)])*
        (via <padstackName> <x:int> <y:int> [(type fix|protect)])*
        (wire (<shape> …) (window …)*)*                 ; conduction areas on signal layers
        )* ) ) )
```

`IndentFileWriter` writes `\n` + two spaces per nesting level; `startScope()` emits a newline
first, `startScope(false)` does not; `endScope()` decrements then emits newline + `)`
(`datastructures/IndentFileWriter.java`).

### 12.2 What KiCad's importer actually consumes

KiCad's `SPECCTRA_DB::LoadSESSION` reads only `(routes (network_out (net … (wire (path …)) (via …))))`
plus `(placement …)` for back-annotating component positions. The load-bearing rules:

1. **All SES coordinates and widths must be integers.** Every shape is written with
   `writeScopeInt` (`SesWriter.java:305`, `:544`), which does `(int) Math.round(coor[i])` per
   coordinate. `writePath` writes `int[]` directly (`SesWriter.java:492-512`). Component
   placements round x/y (`SesWriter.java:164-165`) and via coordinates round
   (`SesWriter.java:469-473`).
2. **The layer name written into a `(path …)` must exactly match a DSN layer name.** `writeWire`
   uses `board.layerStructure.layers[layerIndex].name` verbatim
   (`SesWriter.java:379-381`, `:412`).
3. **Via padstacks must be declared in `library_out` before use**, by name, and the name must
   match the DSN's padstack name. `writeLibrary` (`SesWriter.java:235-252`) iterates
   `board.library.getViaPadstack(i)` and **deduplicates by name** with a
   `LinkedHashSet<String>` (v2-only; v19 emitted duplicates when two `ViaInfo`s shared a padstack,
   and some importers choke on that).
4. **`(attach off)` is written only when the padstack disallows attachment**; `(absolute on)` is
   *not* written by the SES writer (only by the DSN writer, `Library.java:94-97`).
5. The `parser` scope is written in *reduced* mode (`Parser.writeScope(..., reduced = true)`,
   `SesWriter.java:229`), which omits `(string_quote …)`, `(space_in_quoted_tokens on)`, and
   `(generated_by_freeroute)`.
6. `net` scopes are emitted only for nets that have at least one non-`SYSTEM_FIXED` wire, via, or
   signal-layer conduction area; the `(net <name>` header is written lazily on the first such item
   (`SesWriter.java:337-357`).

### 12.3 Two v2-only fidelity fixes in the wire writer

**Consecutive-duplicate coordinate elision** (also present in v19): after rounding to integers,
a corner identical to the previous one is dropped, and the array is truncated with
`Arrays.copyOf` (`SesWriter.java:387-411`).

**Endpoint snapping to pad centers** (`SesWriter.snappedEndpoint`, `SesWriter.java:426-453`,
**v2-only**):
```
for the first and last corner of every wire:
  for each contact at that end that is a DrillItem covering this layer:
     padShape = drill.getShape(layer - drill.firstLayer());  if null continue
     center = drill.getCenter().toFloat()
     d = distance(corner, center)
     if d <= 0.5: return null                       # already at the center
     if d <= padShape.borderDistance(center):       # corner is within the pad INRADIUS
         return center                              # snap
  return null
```
Rationale from the source comment: Freerouting approximates pads with octagons; a trace endpoint
can land inside the octagon but outside the importer's real pad polygon, and KiCad then reports
the net as unconnected. Snapping only inside the inradius guarantees the last segment cannot
leave the pad or fold across a neighbor. **This must be reproduced or your SES files will show
spurious DRC unconnecteds in KiCad.**

### 12.4 Placement rotation formatting

`SesWriter.formatPlacementRotation(double)` (`SesWriter.java:259-269`, **v2-only**):
```
rounded = rint(degrees * 1000) / 1000
if |rounded - rint(rounded)| < 1e-9:  return "%.0f"            # "0", "339"
else: s = "%.3f"; strip trailing zeros; strip trailing '.'      # "338.5", not "338.500"
```
Locale is pinned to `Locale.ENGLISH` so the decimal separator is `.` regardless of the JVM locale.
v19 wrote `(int) Math.round(rotation)` (v19 `SpecctraSesFileWriter.java:163-164`), losing all
fractional rotations.

> Note: `Rectangle.writeScope`, `Polygon.writeScope`, `PolygonPath.writeScope` etc. write doubles
> with plain `String.valueOf(double)`, which is **locale-independent** in Java but produces
> Java's shortest-round-trip form (`1.0E-4`, `1.0`). Freerouting's own DSN output therefore
> contains scientific notation for tiny values, and its own lexer's `DecFloatLiteral` rule
> (`Mantissa Exponent?`) accepts it. A Rust writer should match `f64::to_string`-ish behavior or
> use a fixed format both sides agree on.

### 12.5 SES re-import

`SesReader` (`SesReader.java`) walks `(session … (routes … (network_out (net …))))`, skipping
`placement` and `was_is` entirely (`SesReader.java:148-155`). Per net:
```
net = board.rules.nets.get(netName, 1)          # subnet 1 only; missing net ⇒ skip + errorsEncountered++
(wire … (path …))  → PolygonPath; board.insertTrace(polyline, layer, halfWidth,
                                                    [netNumber], cl=[TRACE], USER_FIXED)
       boardCoord = round(sesCoord / (dsnToBoard(1)/resolution))
       halfWidth  = round(path.width / (2 * denominator))
(via <padstack> x y) → padstack.replaceAll("\\.\\d+",""); board.insertVia(..., cl=[VIA],
                                                    USER_FIXED, attachAllowed = true)
a (wire …) with no (path …) (i.e. a conduction area) is silently ignored
```
Everything is imported as `USER_FIXED` and with the **default net class's** item clearance
classes, not the actual net's class (`SesReader.java:316-321`, `:395-400`).

---

## 13. `.rules` sidecar files

`RulesWriter.write` emits `(rules PCB <designName> …)` containing the same
`rule`/`layer`/`padstack`/`via`/`via_rule`/`class`/`snap_angle`/`autoroute_settings` scopes as the
DSN structure/network scopes. `RulesReader.read` (`RulesReader.java:65-170`) applies them onto an
already-loaded board:

* Header must be `( rules pcb <name>`; a design-name mismatch is logged but **not fatal**
  (`RulesReader.java:100-110`).
* `(rule …)` → width/clearance at PCB level; `(layer L (rule …))` → per-layer.
* `(padstack …)` → `Library.readPadstackScope` straight into `board.library.padstacks`.
* `(via …)` → `Network.readViaInfo`, replacing any existing `ViaInfo` with the same name.
* `(via_rule …)` → `Network.addViaRule`.
* `(class …)` → `Network.insertNetClass(..., viaAtSmdAllowed = false)`.
* `(snap_angle …)`, `(autoroute_settings …)` → board rules / caller-supplied `RouterSettings`.

`RulesReader.readRouterSettings(in)` (`:180-236`) is a standalone path that first sniffs the file
for layer names (`discoverLayerStructure`, `:238-274`, falling back to `["F.Cu","B.Cu"]`) and then
extracts only the `autoroute_settings` scope.

---

## 14. What KiCad exports that a minimal parser gets wrong

Ranked by how often it bites.

1. **Numeric pin ids and numeric names.** `(pin Round[A]Pad_600_um 1 -1000 0)` — the pin name is
   the integer `1`. The DFA in `YYINITIAL` would lex it as an `Integer`. Freerouting forces
   `yybegin(NAME)` before *both* the padstack reference and the pin name
   (`Package.java:252`, `:264-265`) and then `toString()`s whatever token arrives
   (`Package.java:254-261`, `:279-286`). Net names, class names, layer names, component names, and
   the pcb design name have the same treatment. **A parser that always lexes numbers as numbers
   loses every numerically-named pin/net.**

2. **`(rotate …)` can appear *before* the pin name.** Two positions are legal inside `(pin …)`:
   immediately after the padstack reference, and after the coordinates
   (`Package.java:267-277` and `:302-324`). Both must be handled.

3. **Oval / rounded-rect pads are exported as `(path …)` shapes inside a padstack.**
   `Shape.readPolygonPathScope` produces a `PolygonPath{layer, width, coords}`, and
   `PolygonPath.transformToBoard` (`PolygonPath.java:59-82`) converts it to geometry:
   ```
   corners = dsnToBoard of each coordinate pair
   offset  = dsnToBoard(width) / 2
   if corners.length <= 2:   result = FloatPoint.boundingOctagon(corners).enlarge(offset)
   else:                     result = PolygonShape(rounded corners).boundingTile().enlarge(offset)
   ```
   So a two-point path (the standard oval-pad encoding) becomes a **bounding octagon enlarged by
   the half-width** — an octagonal approximation of a stadium. A ≥3-point path collapses to its
   *bounding tile* (convex hull in the tree's direction set) enlarged by the offset. A parser
   that treats `(path …)` inside `(shape …)` as a zero-width polyline produces a degenerate
   (dimension < 2) shape, which then hits the `offset(1)` rescue in `Library.java:194-205` and
   yields a 1-unit pad.
   Note `readPolygonPathScope` **rejects paths with fewer than 5 numbers** (width + 2 points),
   logging at debug level and returning `null` (`Shape.java:470-478`).

4. **Quoted strings containing spaces, and `(string_quote ")`.** KiCad emits
   `(string_quote ")` and then quotes net names like `"Net-(U1-Pad3)"`. The `IGNORE_QUOTE` state
   exists solely so the quote character itself can be read as an identifier
   (`SpecctraFileDescription.flex:258-266`). Downstream, `Structure.setClearanceRule` re-parses
   quoted class names by splitting on `stringQuote` (`Structure.java:729-740`), and
   `IdentifierType.write` strips surrounding `"` and *removes* any embedded quote character before
   re-quoting (`datastructures/IdentifierType.java:24-67`). A minimal parser that treats `"` as an
   ordinary character will mis-tokenize every KiCad net name.

5. **`kicad_default` net class.** KiCad renames its `Default` class to `kicad_default` in DSN to
   avoid colliding with Freerouting's reserved `default`. Without the folding in
   `KiCadNetClassNames.isKiCadDefaultNetClassName` (`Network.java:442-445`, `:553`, `:561`) you get
   a *second* net class carrying all the real width/clearance rules while the router keeps using
   the built-in `default` class with `traceHalfWidth = 1500`.

6. **`.N` suffixed padstack names.** KiCad appends `.1`, `.2`, … to disambiguate padstacks that
   differ only in a property Freerouting ignores. All four lookup sites strip `\.\d+`
   (`Library.java:113`, `Library.java:322`, `Network.java:1293`, `Wiring.java:659`,
   `SesReader.java:384`). Miss one and you get "padstack not found", which in
   `Library.readScope` **aborts the entire library read**.

7. **`::N` suffixed footprint names and duplicate `(image …)` blocks.** Handled by the
   dedup/probe loop in `Library.java:409-449` and the `::\d+$`-stripping fallback in
   `Packages.get` (`core/library/Packages.java:40-50`).

8. **Leading `""` in a class's net list.** KiCad 8 emits `(class kicad_default "" NET1 NET2 …)`.
   `nextStringList` explicitly tolerates an empty first element
   (`SpecctraDsnStreamReader.nextStringList`).

9. **Clearance class-pair separators.** `(clearance 200 (type smd_via))` vs
   `(clearance 200 (type smd-via))`. `nextStringList('-')` splits on `-`, producing either one
   token (underscore form) or two (hyphen form). Both are handled, by different branches
   (`Structure.java:719-747`). Multi-underscore names like `smd_via_same_net` are explicitly
   **not** supported and silently skipped.

10. **`(type jumper)` layers** — treated as signal, matched via a raw string comparison
    (`Structure.java:349`).

11. **`(PN "part-number")` in a `(place …)` record** — arrives as a string, not a keyword
    (`Component.java:281-283`).

12. **Degenerate geometry from KiCad 4.0.7.** Zero-area keepout polygons
    (`Structure.java:864-876`) and 3-identical-vertex wires (`Wiring.java:516-545`) must be skipped
    with a warning rather than aborting — the source comments name the version explicitly.

13. **Wire corners outside the board bounding box** — KiCad occasionally exports courtyard-derived
    geometry outside the outline. Freerouting drops the whole wire with a warning
    (`Wiring.java:498-510`). Remember the box was already inflated by 1000 units
    (`Structure.java:1208`).

14. **`(clearance_class …)` vs `(clearance_class …)` casing inside `(place (pin …))`** — the v2
    reader accepts the keyword *or* the raw strings `clearance_class` / `clearanceClass`
    (`Component.java:326-329`).

15. **The Electra `Top`/`Bottom` substring layer fallback** (`LayerStructure.java:37-43`) — needed
    for `.dsn` files whose boundary references `"Top"` when the actual layer is `"F.Cu"`.

---

## 15. v1.9 → v2 (2.3.0) semantic deltas

Beyond renames and reflow, these are real behavior changes. All were verified by reading both trees.

| Area | v19 | v2 (2.3.0) | Impact |
|---|---|---|---|
| Net class default folding | none | `KiCadNetClassNames` folds `default`/`kicad_default` onto the built-in default class (`Network.java:442-445`, `:553`, `:561`) | KiCad width/clearance rules now actually apply |
| Padstack/via/pin name cleanup | none | `.replaceAll("\\.\\d+","")` at 5 lookup sites | KiCad `.N` padstacks resolve |
| Package dedup | plain `packages.add(name, …)` | `::N` probing + `arePackagePinsIdentical` (tol 0.001) (`Library.java:409-449`) | duplicate `(image …)` blocks no longer create bogus packages |
| Component outline | `insert_component_outline(shape, isFront, t, rot, cmpId, fixed)` | adds `isCourtyard` / `isFabrication` / `isClosed`, derived from largest-bbox index, `width == 0.0`, and `width <= 110.0` (`Network.java:1158-1203`) | rendering/classification only |
| Degenerate keepout | `return false` (aborts read) | warn + skip, `return true` (`Structure.java:864-876`) | KiCad 4.0.7 files load |
| Degenerate wire | inserted → infinite normalization | all-corners-identical test, skip + warning (`Wiring.java:516-545`) | hang fix |
| Wire out-of-bounds corner | inserted | dropped with warning (`Wiring.java:498-510`) | |
| Duplicate via in wiring | inserted twice | `viaExists` check, skipped with warning (`Wiring.java:238-255`, `:699-704`) | |
| Wiring via net array index | `curr_index` incremented (v19 `Wiring.java:62-64`) | **not** incremented (`Wiring.java:684-689`) | regression; multi-subnet vias get a wrong net array |
| Trace normalization | inline | deferred to `normalizeAllTraces()` at end of wiring scope, wrapped in try/catch (`Wiring.java:345-352`) | resilience |
| Clearance pair parsing | quote-strip on `pair[1]` + reject multi-underscore `pair[1]` | `classPairs.size()==2` fast path for the `-`-separated form; blanket `replaceAll("[\"]","")` (`Structure.java:719-728`) | handles `nextStringList('-')` output |
| Warnings | logged only | collected into `ReadScopeParameter.warnings`, surfaced in `BoardReadResult` | API |
| Read result | `DsnFile.ReadResult{OK, OUTLINE_MISSING, ERROR}` enum | sealed `BoardReadResult.{Success, OutlineMissing, ParseError, IoError}` carrying board + metadata + warnings | API |
| Read entry point | `DsnFile.read(...)` needing a `BoardHandling` | `DsnReader.readBoard(InputStream, …)` with an internal `MinimalBoardManager` shim; plus `readMetadata` fast path | headless/testable |
| SES reserved chars | `{ "(", ")", " ", "-" }` | `{ "(", ")", " ", ";", "-", "_", "/", "~", "{", "}" }` (`SesWriter.java:62`) | KiCad `/CLK`, `~{RST}` round-trip |
| SES placement rotation | `(int) Math.round(deg)` | `formatPlacementRotation`, 3-decimal minimal form, `Locale.ENGLISH` (`SesWriter.java:259-269`) | fractional rotations preserved |
| SES padstack list | may emit duplicates | `LinkedHashSet<String>` dedup (`SesWriter.java:243-248`) | importer compatibility |
| SES wire endpoints | raw corners | `snappedEndpoint` pad-center snapping (`SesWriter.java:426-453`) | fewer spurious KiCad unconnecteds |
| SES stream lifecycle | `output_file.close()` | flush only, caller owns the stream (`SesWriter.java:66`) | API |
| SES read | lived in GUI `BoardHandling` | standalone `SesReader` returning `SesImportSummary(wires, vias, errors)` | headless |
| `.rules` | `RulesFile.read/write` | `RulesReader`/`RulesWriter` + `readRouterSettings` with layer sniffing | headless |
| `CoordinateTransform` | `designforms.specctra` | `app.freerouting.io`, shared with the KiCad-JSON reader/writer | |
| Item clearance info key | `CLEARANCE_CLASS` keyword only | also accepts raw `"clearance_class"`/`"clearanceClass"` strings (`Component.java:326-329`) | |
| `(PN …)` | not read | `partNumber` on `ComponentLocation` (`Component.java:281-283`) | |
| Hole clearance | absent | `BoardRules.holeClearance` (`BoardRules.java:104-111`) — see `shove.md` §13.2 | |

Unchanged and still present in v2: the `autoroute_settings` `layerStructure == null` guard bug
(§6.6), the `class_class` shared-iterator pairing bug (§7.7), the `separateHoles` last-writer-wins
assignment (§6.3), and the integer-division `scaleFactor` reduction (§3.2).

---

## 16. Constants and thresholds

| Constant | Value | Where |
|---|---|---|
| `Limits.CRIT_INT` | `33_554_432` = 2^25 | `geometry/planar/Limits.java:12` |
| scale-factor headroom multiplier | `5` | `Structure.java:1200` |
| board bounding-box inflation | `1000` board units | `Structure.java:1208` |
| default `unit` / `resolution` | `MIL` / `100` | `ReadScopeParameter.java:87-89` |
| default `stringQuote` | `"` | `ReadScopeParameter.java:67` |
| default `snapAngle` | `FORTYFIVE_DEGREE` | `ReadScopeParameter.java:59` |
| `Unit` µm factors | MIL 25.4, INCH 25400, MM 1000, UM 1 | `board/model/structure/Unit.java:7-10` |
| `ClearanceMatrix.clearance_safety_margin` | `16` | `ClearanceMatrix.java:17` |
| clearance parity | rounded **up to even**, clamped ≥ 0 | `ClearanceMatrix.java:104-113` |
| `BoardRules.defaultClearanceClass()` | `1` (`"default"`) | `BoardRules.java:64-66` |
| `BoardRules.clearanceClassNone()` | `0` (`"null"`) | `BoardRules.java:69-71` |
| default net class trace half width | `1500` | `BoardRules.java:205-207` |
| `minTraceHalfWidth` / `maxTraceHalfWidth` seeds | `100000` / `100` | `BoardRules.java:57-58` |
| `pinEdgeToTurnDist` fallback | `getMinTraceHalfWidth()` when no `smd_to_turn_gap` | `Structure.java:667-669` |
| degenerate-pad rescue offset | `1` board unit | `Library.java:201` |
| package-pin identity tolerance | `0.001` board units (x, y, rotation) | `Library.java:249-256` |
| courtyard/fabrication outline width threshold | `0.0` / `≤ 110.0` | `Network.java:1176-1186` |
| `adjustPlaneAutorouteSettings` area threshold | `0.5 * boardArea` | `DsnFile.java:91` |
| SES endpoint-snap "already centered" epsilon | `0.5` board units | `SesWriter.java:444` |
| SES rotation rounding | `rint(deg*1000)/1000`, integer test `< 1e-9` | `SesWriter.java:260-261` |
| pad trace-exit aspect factor | `1.5`, doubled for packages with `pinCount() <= 3` | `Pin.java:268-277` |
| `Padstack.getDrillRadius` fallback | `smallestRadius * 0.45` | `Padstack.java:109` |
| scanner initial buffer | `16 * 1024 * 1024` chars | `SpecctraDsnStreamReader.java:40` |
| `nextStringList` net-per-row on write | `8` | `Network.java:105` |
| `CLASS_CLEARANCE_SEPARATOR` | `'-'` | `DsnFile.java:20` |
| DSN writer reserved chars | `( ) SPACE ; - _` | `WriteScopeParameter.java:35` |
| SES writer reserved chars | `( ) SPACE ; - _ / ~ { }` | `SesWriter.java:62` |
| `IndentFileWriter` indent | 2 spaces | `IndentFileWriter.java:12` |
| `Nets.max_legal_net_number` | `9_999_999` | `Nets.java:17` |
| `Nets.hidden_net_number` | `10_000_001` | `Nets.java:20` |

---

## 17. Rust design implications

### 17.1 Keep semantically (conformance-critical — bake these into tests)

1. **The scale-factor derivation** (§3.2) verbatim, including the `5 *` headroom and the loop
   condition `5*maxCoor >= 2^25`. Every board coordinate in the router depends on it; get it wrong
   and every downstream cost constant in `maze-engine.md` / `shove.md` is off by a factor.
2. **Clearance values are stored non-negative and even** (`ClearanceMatrix.setValue`). The search
   tree's compensation is `(value + 1) / 2` (`ClearanceMatrix.clearanceCompensationValue`) — see
   `search-tree-obstacles.md` §2.1 — and it silently assumes evenness.
3. **The clearance-class construction order**: `via, smd, pin, area` appended in that order, new
   classes seeded from class 1 (`appendClass`) or from the parent net class
   (`getClearanceClass`), `"wire"` aliasing to class 1. Class *indices* are stored on every item,
   so any reordering changes routing results.
4. **Both trace-half-width rounding formulas** (`round(dsn(w)/2)` at PCB/layer level vs
   `round(dsn(w/2))` at net-class level, §3.4). They differ by ±1 unit and one of them is what
   your reference boards were routed with.
5. **The mirroring math** (§9.2), including the two `flip_style` orders, the exact-vs-approximate
   `% 90 == 0` branch, and the `placedAbsolute` layer-flip exemption. This is the single most
   likely place to introduce a silent 180°/mirror error on back-side components.
6. **`fromLayer() == toLayer()` ⇒ SMD** (clearance class selection and `drillAllowed`).
7. **Conduction areas imported from DSN are `isObstacle = false`.** Ports that "fix" this by
   making pours obstacles will route completely differently.
8. **`FixedState` ordering and the SES writer's `SYSTEM_FIXED` skip** — this is what keeps
   imported keepouts out of the output.
9. **Non-convex pads are convex-hulled** (`Library.java:177-179`). Do not "improve" this without
   also changing the search tree.
10. **Degenerate-geometry tolerances**: zero-area keepouts skipped, all-identical-corner wires
    skipped, out-of-bounds wire corners dropped, sub-2-dimensional pads offset by 1.
11. **The SES endpoint-snapping rule** (§12.3) and integer-only SES coordinates.
12. **The lexer's `NAME`-state discipline** — reproduce the exact set of keywords that
    auto-enter `NAME`/`LAYER_NAME` (§2.1) plus every explicit caller-side `yybegin`.

Bugs I would **not** reproduce, but must be documented as intentional divergences with a
regression test each:
* `Wiring.readViaScope`'s missing `currentIndex++` (§10.2).
* The `class_class` shared-iterator pairing (§7.7) — but note that fixing it *changes the
  clearance matrix*, so gate it behind a compatibility flag if you need bit-identical output.
* The `autoroute_settings` guard (§6.6) — fixing it means DSN-declared router settings suddenly
  take effect on third-party boards.
* `separateHoles`'s assignment-instead-of-OR (§6.3).

### 17.2 Restructure

**Lexer.** Replace the JFlex DFA + raw-buffer bypass with a single hand-written tokenizer over
`&[u8]` (DSN is ASCII-dominant; treat non-ASCII bytes as identifier chars, matching the
`SpecCharANSI` rules). Model the "state" as an argument to `next_token(mode: LexMode)` rather than
scanner-mutable state — the entire `yybegin` dance becomes explicit and testable:

```rust
enum LexMode { Keyword, Name, LayerName, IgnoreQuote, SpecChar }
fn next_token(&mut self, mode: LexMode) -> Option<Token<'src>>
enum Token<'a> { Open, Close, Kw(Keyword), Ident(&'a str), Int(i64), Float(f64) }
```
`Ident` borrows from the mmap'd input — zero allocation for the ~10⁵–10⁶ identifiers in a large
DSN. The Java version allocates a `String` per token (`yytext()` → `new String(zzBuffer, …)`) and
a `StringBuffer`-backed `String` per `nextString`.

**Memory-map the file** instead of the 16 MiB `char[]`. Java doubles the byte count by widening to
UTF-16 and then copies again on every `yytext()`. A 50 MB DSN costs >200 MB of live heap in
Freerouting before a single board item exists.

**Arena the parse tree.** `ReadScopeParameter` currently holds `LinkedList`s of small heap objects
(`PlaneInfo`, `ComponentPlacement`, `Shape`, `Rule`). Use one `Bump`/`typed_arena` per read; all
of it dies at the end of the read anyway.

```rust
struct DsnRead<'a> {
    arena: &'a Bump,
    shapes: Vec<DsnShape<'a>>,      // SoA, indices instead of Box<dyn Shape>
    planes: Vec<PlaneInfo>,
    placements: Vec<ComponentPlacement<'a>>,
    …
}
```

**Replace `Shape` polymorphism with an enum.** `Shape` is an abstract class with five subclasses
and four abstract methods, dispatched virtually per shape. `enum DsnShape { Rect{layer, [f64;4]},
Polygon{layer, coords: &[f64]}, Circle{layer, [f64;3]}, PolygonPath{layer, w, coords: &[f64]},
PolylinePath{layer, w, coords: &[f64]} }` removes the vtable and lets `transform_to_board` be a
single match.

**Clearance matrix layout.** Java stores `Row[classCount]` → `MatrixEntry[classCount]` →
`int[layerCount]` — three pointer hops and `classCount²` separate allocations, and `appendClass`
reallocates *every* row. Use one flat `Vec<i32>` of length `classCount * classCount * layerCount`
with `idx = (j * classCount + i) * layerCount + layer`, plus a `Vec<CompactString>` of names and a
`HashMap<UniCase<String>, u16>` for `getNo` (which is currently a linear case-insensitive scan
run once per class-pair token — O(pairs × classes)). Grow by reallocating the flat buffer with a
stride change; that's one `memcpy`-loop, not `classCount` allocations.

**Padstack shapes.** `ConvexShape[layerCount]` per padstack with mostly-null entries. Store
`(from_layer, to_layer)` plus a contiguous `SmallVec<[ShapeId; 4]>`, and interning identical
shapes — real boards have thousands of pads sharing a handful of distinct geometries.

**Pin shape computation.** `Pin.getShape` computes *all* layers on first access and caches in a
`transient Shape[]` that is invalidated by `clearDerivedData()`. Keep the batch computation (the
comment at `Pin.java:168-170` is right: `fromLayer`/`toLayer` are only correct when computed
together) but move it out of the item: a `PadShapeCache` keyed by
`(padstack_id, pin_rotation, component_rotation, mirrored, flip_style)` deduplicates across the
hundreds of instances of the same footprint. Freerouting recomputes per component instance.

**Net→pin lookup.** `NetList.getNets(componentName, pinName)` (`NetList.java:44-55`) walks
**every net** and does a `TreeSet.contains` per net, and it is called once per pin of every
component (`Network.java:989`). That is O(pins × nets × log pins) — quadratic on real boards. Build
a `HashMap<(CompactString, CompactString), SmallVec<[NetId; 2]>>` once after the network scope.
Same for `Nets.get(name)` / `get(name, subnet)`, which are linear `equalsIgnoreCase` scans
(`Nets.java:41-61`) called from `Wiring` for every wire and via.

**Parallelism.** The read itself is inherently sequential (single token stream, cross-scope state).
But three phases after tokenization are embarrassingly parallel and dominate on big boards:
* padstack shape conversion (`transformToBoardRel` + `splitToConvex` + `convexHull`) — pure per
  padstack;
* package materialization (pin vectors, outlines, keepout areas) — pure per package;
* pin shape precomputation — pure per (component, pin) once the cache key is known.
A two-phase design (`parse → Vec<RawScope>` then `rayon::par_iter` over the geometry conversion)
gets most of the win with no shared mutation. Do **not** parallelize `Wiring` insertion —
`insertTraceWithoutCleaning` mutates the search tree.

**Error model.** Replace the `boolean` return + `FRLogger.warn` convention with
`Result<T, DsnError>` plus a `Vec<Warning>` accumulator that mirrors
`ReadScopeParameter.warnings`. Preserve *which* failures are fatal — the current split is
non-obvious (a missing package padstack aborts the library scope; a missing wiring padstack
aborts the wiring scope; a degenerate keepout does not abort anything).

**Writers.** `IndentFileWriter` extends `OutputStreamWriter` and writes one `\n` plus N × `"  "`
per line, unbuffered per call. Write into a single `String`/`Vec<u8>` with a
`push_str(&INDENT[..2*level])` from a static pad, and use `ryu`/`itoa` instead of
`String.valueOf(double)` — but pin the float format to match Java's shortest-round-trip
representation if you need byte-identical golden files (see the note in §12.4).

### 17.3 Java-isms observed (file:line evidence)

* **Per-token `String` allocation.** `yytext()` (`SpecctraDsnStreamReader.java:800-811`) builds a
  fresh `String` for every identifier; `nextString` builds another via `stringBuffer.toString()`.
  On a 20 MB DSN this is on the order of 10⁶ short-lived `String`s.
* **16 MiB `char[]` up front** (`SpecctraDsnStreamReader.java:40,561`) plus doubling in `zzRefill`
  (`:738-742`) — the file is fully resident, widened 2×, and reallocated with a full copy.
* **`static` mutable scanner state.** `public static String scopeIdentifier` and
  `public static NumberFormat nf` (`SpecctraDsnStreamReader.java:545-546`) — the "current scope"
  used in every error message is a *class* field, so two concurrent reads corrupt each other's
  diagnostics, and `nf` is a lazily-initialized shared `NumberFormat` (not thread safe).
* **`LinkedList` everywhere.** `Structure` (`:943-945`, `:1296-1302`), `Shape.readAreaScope`
  (`Shape.java:171`), `Network.readScope` (`:1208-1211`), `Library` (`:109,266`),
  `Package.readScope` (`:60-71`). Every element is a separately allocated node; iteration is a
  pointer chase. All of these are append-then-iterate — `Vec` in Rust.
* **Boxed numeric tokens.** The scanner returns `Object`; numbers arrive as `Integer`/`Double`
  and every consumer does `instanceof Double d ? d : (instanceof Integer i ? i : fail)`
  (`Shape.java:133-141`, `:270-280`, `:374-384`, `:424-432`, `:485-509`;
  `Package.java:291-299`; `Component.java:200-203`, `:239-248`; `Wiring.java:618-628`).
  That is a heap allocation (outside the `Integer` cache) and two type checks per coordinate.
* **`double[]` churn per point.** `CoordinateTransform.dsnToBoard(double[])` allocates a
  `FloatPoint`, and callers allocate a fresh `double[2]` scratch per point — see
  `Polygon.transformToBoard` (`Polygon.java:29-34`, reuses one array, good) vs
  `Rectangle.transformToBoard` (`Rectangle.java:61-67`, one array, two `FloatPoint`s) vs
  `Wiring.readWireScope` (`Wiring.java:493-497`, one array reused but a `FloatPoint` per corner).
  `board_to_dsn(FloatPoint[])` allocates a `double[2n]` result per call
  (`CoordinateTransform.java:47-55`).
* **Clone-heavy matrix growth.** `ClearanceMatrix.appendClass` (`ClearanceMatrix.java:281-322`)
  allocates a new `Row[]`, a new `Row` per existing class, and a new `MatrixEntry` (holding a new
  `int[layerCount]`) per row, then `System.arraycopy`s the old columns — i.e. O(classes) object
  allocations per appended class, and it is called once per unnamed clearance class encountered.
  On a board with 30 net classes × 5 item classes that is ~150 appends × O(n) allocations.
* **`ClearanceMatrix.getValue` logs on every call.** `FRLogger.trace(...)` at
  `ClearanceMatrix.java:167-198` builds a ~15-fragment concatenated `String` **unconditionally**
  before the trace call can decide to drop it, and allocates `new Point[0]`. `getValue` is on the
  hottest path in the router (see `search-tree-obstacles.md` §2.1). This alone is worth a
  significant fraction of Freerouting's runtime and must not be ported.
* **`Padstack.getDrillRadius` parses the padstack *name* with regex** (`Padstack.java:72-112`) —
  memoized in `cachedDrillRadius`, but the memo is per-`Padstack` and the parse is
  `String.substring` + `replaceAll("[^0-9.]", "")` + `Double.parseDouble`.
* **`Component.getKeepout` is a full board scan** (`Component.java:170-185`) called once per
  package keepout per component while writing DSN — O(components × keepouts × items).
* **`Package.writePlacementScope` scans all board items per component** (`Package.java:358-371`)
  to check "not all items deleted" — O(components × items). `SesWriter.writeComponents` does the
  same (`SesWriter.java:124-136`).
* **`NetList.getNets` is O(nets) per pin** (`NetList.java:44-55`), called from
  `Network.insertComponent` (`Network.java:989`) — the quadratic hot spot of DSN import.
* **`Nets.get(String)` / `get(String,int)` linear `equalsIgnoreCase` scans**
  (`Nets.java:41-61`), called per wire and per via in `Wiring.getSubnets` (`Wiring.java:223-236`).
* **`ClearanceMatrix.getNo` linear `equalsIgnoreCase` scan** (`ClearanceMatrix.java:58-65`),
  called for every clearance-class name in every rule, keepout, pin info, and wire.
* **Exception-driven control flow in the read loop.** `BasicBoard.normalizeAllTraces` catches
  `ConcurrentModificationException` and *restarts the whole collection pass*
  (`BasicBoard.java:807-812`).
* **`instanceof`-chain dispatch instead of an enum.** `Shape` subclass tests in
  `Structure.addBoundaryShape` (`:310-322`), `Structure.createBoard` (`:1212`),
  `CoordinateTransform.boardToDsn` (`io/CoordinateTransform.java:88-104`),
  `Wiring.readWireScope` (`:487,546`), plus `Rule` subclass tests throughout `Structure`/`Network`.
* No `BigInteger` on this path — the BigInt hot spots live in the geometry kernel
  (`Limits.CRIT_INT_BIG`, `geometry/planar/Limits.java:20`) and are covered by the geometry study,
  not here. The DSN reader's only wide-arithmetic concern is the `5 * maxCoor >= CRIT_INT`
  guard that exists precisely to keep the kernel out of BigInteger fallback.

### 17.4 Suggested Rust module cut

```
fr-io/
  specctra/
    lex.rs         Token, LexMode, Lexer<'src>  (borrowed &str, no allocation)
    ast.rs         DsnShape, Rule, NetClassDecl, PackageDecl, PadstackDecl, PlaceRecord …
    read.rs        scope drivers: pcb/parser/resolution/structure/library/placement/network/wiring
    build.rs       ast → fr-core::Board  (scale factor, clearance matrix, padstacks, components)
    write_dsn.rs
    write_ses.rs
    read_ses.rs
    rules.rs       .rules read/write
  transform.rs     CoordinateTransform  (Copy, 3 f64s)
fr-core/
  rules/clearance_matrix.rs   flat Vec<i32>, UniCase name index
  rules/net_class.rs
  library/{padstack,package}.rs
  placement.rs     mirroring math (§9.2) — pure functions, unit-testable in isolation
```

The `read.rs` → `build.rs` split is the one structural change I would insist on: Freerouting
interleaves parsing with board mutation (`Structure.readScope` constructs the board mid-scope and
`Network.readScope` inserts items mid-scope), which is exactly why the state bag in §4 exists and
why the `autoroute_settings` ordering bug is possible. Parse to a complete AST first, then build
once with all information available — the ordering constraints in §1.1 disappear, and the geometry
conversion becomes parallelizable.
