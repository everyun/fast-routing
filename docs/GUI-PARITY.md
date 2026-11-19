# GUI-PARITY: Java Freerouting 2.3.0 GUI Baseline Specification & Parity Audit

This document defines the functional baseline of upstream Freerouting 2.3.0 (Java Swing GUI) and serves as the authoritative parity checklist for the `fast-routing` GPU-accelerated graphical interface (`fr-gui`).

---

## 1. Upstream Java GUI Architecture

The Java GUI is implemented in packages `app.freerouting.gui` and `app.freerouting.boardgraphics`.

### 1.1 Core Components
- **`BoardFrame` (`JFrame`)**: Main top-level application window containing the menu bar, toolbar, main canvas panel (`BoardPanel`), and status bar (`ScreenMessages`).
- **`BoardPanel` (`JPanel`)**: Viewport canvas handling coordinate transformations, zoom/pan operations, and dispatching mouse/keyboard events. Delegates rendering to `GuiBoardManager.draw(Graphics)`.
- **`BoardMenuBar` (`JMenuBar`)**: Main application menu hierarchy (`File`, `Display`, `Parameter`, `Rules`, `Info`, `Other`, `Help`).
- **`BoardToolbar` (`JToolBar`)**: Primary action toolbar featuring mode selectors, layer selection combobox, clearance class selector, and autoroute execution controls (Start / Stop / Batch).
- **`ScreenMessages`**: Multi-section status footer below the canvas displaying cursor coordinates, layer status, live autoroute counters, error/warning tallies, and normalized routing score.

---

## 2. Canonical Color Palette & Layer Rendering Defaults

Sourced from `app.freerouting.boardgraphics.ItemColorTableModel`, `OtherColorTableModel`, and `ColorIntensityTable`:

### 2.1 General Elements
| Element | Upstream Constant | RGBA (Float [0.0..1.0]) | Hex Color | Translucency ($\alpha$) |
|---|---|---|---|---|
| **Canvas Background** | `OtherColorTableModel.BACKGROUND` | `[0.000, 0.063, 0.137, 1.0]` | `#001023` | 1.00 (Opaque) |
| **Board Outline** | `OtherColorTableModel.OUTLINE` | `[0.392, 0.588, 1.000, 1.0]` | `#6496FF` | 1.00 |
| **Selection Highlight** | `OtherColorTableModel.HIGHLIGHT` | `[1.000, 1.000, 1.000, 0.8]` | `#FFFFFF` | 0.80 |
| **Ratsnest (Air Lines)** | `OtherColorTableModel.INCOMPLETES` | `[1.000, 1.000, 1.000, 1.0]` | `#FFFFFF` | 1.00 |
| **Design Rule Violations** | `OtherColorTableModel.VIOLATIONS` | `[1.000, 0.000, 1.000, 1.0]` | `#FF00FF` | 1.00 |
| **Pins & Vias** | `ItemColorTableModel.PINS / VIAS` | `[0.890, 0.718, 0.180, 1.0]` | `#E3B72E` | 1.00 |
| **Conduction Areas** | `ItemColorTableModel.CONDUCTION_AREAS` | `[0.000, 0.588, 0.000, 0.2]` | `#009600` | 0.20 |
| **Keepout Areas** | `ItemColorTableModel.KEEPOUTS` | `[0.102, 0.769, 0.824, 0.2]` | `#1AC4D2` | 0.20 |
| **Drill Holes** | `OtherColorTableModel.DRILL_HOLE` | `[0.000, 0.000, 0.000, 1.0]` | `#000000` | 1.00 |

### 2.2 Trace Layer Palette
Signal traces use layer-dependent default colors:
- **Top Layer (0: F.Cu)**: Crimson Red (`#C83434`, `[200, 52, 52]`)
- **Bottom Layer (N-1: B.Cu)**: Medium Blue (`#4D7FC4`, `[77, 127, 196]`)
- **Inner Layers (In1 .. In6 cyclical rotation)**:
  1. `In1`: Light Leaf Green (`#7FC87F`, `[127, 200, 127]`)
  2. `In2`: Amber Orange (`#CE7D2C`, `[206, 125, 44]`)
  3. `In3`: Cyan Teal (`#4FCBCB`, `[79, 203, 203]`)
  4. `In4`: Pink / Rose (`#DB628B`, `[219, 98, 139]`)
  5. `In5`: Soft Lavender (`#A7A5C6`, `[167, 165, 198]`)
  6. `In6`: Electric Turquoise (`#28CCD9`, `[40, 204, 217]`)
- **Non-signal Layers (Internal Planes)**: Solid Black / Dark Slate (`#000000`)

---

## 3. Scope Categorization: Must / Should / Later

The feature inventory is split into three priority tiers:

### 3.1 Must-Have (Core Batch Autorouting GUI)
Essential for viewing boards, running autoroute sessions, tracking progress, and exporting results:
1. **Board Canvas Vector Rendering**: Correctly scales and renders pads, vias, multi-width traces, copper polygons, keepouts, and board outlines.
2. **Dynamic Ratsnest**: Displays unrouted flight lines connecting disconnected contact groups; lines update dynamically as routing progresses.
3. **Coordinate Transformation & View Controls**: Inverted $Y$-axis (matching EDA convention); smooth zooming centered at cursor; panning by dragging; one-touch Fit-to-screen (`F`).
4. **Per-Layer Visibility & Layer Selection**: Toggle checkboxes to show/hide layers; select current routing layer.
5. **Design Input & Session Export**: Load `.dsn` designs via CLI argument (`--open <dsn>`) and file menu; export completed board to Specctra `.ses`.
6. **Autoroute Engine Control**: Start, Stop (clean atomic cancellation without state corruption), and Pause batch autoroute.
7. **Live Status & Statistics**: Real-time pass count, remaining incomplete connections, clearance violations, elapsed time, and normalized routing score.
8. **Incompletes Window / Inspection**: Listing remaining unrouted connections with click-to-highlight/focus capability.

### 3.2 Should-Have (Convenience & Advanced Diagnostics)
1. **Object Class Visibility Toggles**: Globally toggle visibility of traces, vias, pads, areas, or ratsnest across all layers.
2. **Autoroute Parameter Configuration**: Dialog window to adjust maximum passes (`-mp`), thread pool, job timeout, tiling45 mode, and ripup costs.
3. **Log & Event Console**: Integrated scrollable console pane streaming router events, phase switches, and engine warnings.
4. **Clearance Violation Inspector**: Tabular list of clearance violations linking to violation coordinates on canvas.
5. **Color Customization**: Palette editor dialog to modify layer and element colors.
6. **Live Mouse Coordinates**: Cursor position tracking in status bar formatted in board units (mm/mil).

### 3.3 Later (Manual Interactive Routing - Deferred)
Advanced manual CAD editing capabilities deferred to future milestones:
1. Interactive manual trace push-and-shove routing (`RouteState.java`).
2. Interactive manual trace dragging and vertex editing (`MoveItemState.java`).
3. Interactive rip-up tool for individual traces/subnets (`DeleteTraceState.java`).
4. Distance measuring ruler tool (`InteractiveState.java`).
5. Pin and gate swapping tools (`PinSwapState.java`).

---

## 4. In-Flight Routing Behavior Comparison

During batch autorouting, the upstream Java GUI and fast-routing's `fr-gui` operate under distinct threading architectures:

| Property | Upstream Java Swing (v2.3.0) | fast-routing `fr-gui` (GPUI) | Advantage |
|---|---|---|---|
| **Thread Model** | Background worker thread (`AutorouterThread`) updates AWT event queue via `SwingUtilities.invokeLater` | Background engine thread (`std::thread`) sends non-blocking `RouterEvent` messages over `mpsc` channel | Zero risk of UI lockup; engine decoupled from UI thread |
| **Repaint Frequency** | Triggers `boardManager.repaint()` after every single routed/ripped connection, often flooding AWT event loop on fast CPU | Throttled snapshot timer (default 500 ms) guarantees UI maintains 60 fps during intensive routing passes | High-framerate responsiveness on 15k+ item designs |
| **Cancellation** | Sets `Thread.interrupt()` / volatile flag checked between passes | `Arc<AtomicBool>` cancel flag checked at every expansion corridor and pass boundary | Immediate stop (< 5 ms) without thread termination hazards |
| **Deterministic Output** | SES export from GUI can diverge if manual settings differ from defaults | SES export with identical CLI flags matches bit-for-bit with CLI (`--deterministic`) | Exact referee reproducibility |

---

## 5. Visual Reference Captures

Reference captures comparing Java Freerouting 2.3.0 and `fr-gui` are stored in `docs/gui-parity/`:
- `java-Issue039-initial.png` vs `ours-Issue039.png`
- `java-RelayModule-initial.png` vs `ours-RelayModule.png`
- `java-cla45-sensor-l6-initial.png` vs `ours-cla45-sensor-l6.png`
