# GUI Parity Comparison Template

This document defines the standardized methodology, view alignment criteria, and test scenarios for validating visual and functional parity between the upstream Java Freerouting GUI (v2.3.0) and fast-routing's Rust GPUI interface (`fr-gui`).

---

## 1. Test Fixtures

Three reference designs span different complexity classes, layer counts, and geometric features:

| Board Fixture | Path | Layer Count | Key Geometric Features | Focus Area |
|---|---|---|---|---|
| **Issue039** | `fixtures/Issue039-bug-design.dsn` | 2 | Dense pin escape, narrow channels | Unrouted ratsnest lines, SMD pad shapes |
| **RelayModule** | `fixtures/Issue110-RelayModule.dsn` | 2 | Large power pours, polygon keepouts | Conduction areas, keepouts, outline |
| **cla45-sensor-l6** | `bench/fixtures-oos/cla45-sensor-l6.dsn` | 6 | High-density multi-layer, 10k+ elements | Multi-layer rendering, zoom/pan 60fps |

---

## 2. Display and Alignment Standards

To guarantee repeatable, pixel-accurate comparison:

### 2.1 Viewport & Coordinate Space
- **Window Geometry**: 1280x800 minimum per window, arranged side-by-side (Java GUI on left, `fr-gui` on right).
- **Coordinate Orientation**: Board units (microns/nm) converted to display space with $Y$-axis inverted ($Y$ increasing downwards on screen, matching KiCad and Java Swing convention).
- **Fit View (`F` / Fit to Screen)**: Both applications zoomed to fit the entire board outline with a 5% margin.
- **Aspect Ratio**: 1:1 square aspect ratio strictly preserved (no non-uniform scaling).

### 2.2 Layer Visibility Matrix
Comparison screenshots must be captured across standard layer visibility permutations:
1. **Composite View (All Layers)**: All signal and plane layers enabled.
2. **Top Layer Solo (F.Cu)**: Only Layer 0 (F.Cu) visible; pads/vias with copper on Layer 0 rendered.
3. **Bottom Layer Solo (B.Cu)**: Only bottom copper visible.
4. **Inner Layer (cla45-sensor-l6 In1.Cu)**: Inner signal layer isolated.

### 2.3 Color Palette & Alpha Blend Targets
Colors must conform to Freerouting 2.3.0 defaults (`ItemColorTableModel` and `OtherColorTableModel`):
- **Background**: `#001023` (Dark Navy: R:0, G:16, B:35)
- **Top Layer (F.Cu) Traces**: `#C83434` (Crimson Red: R:200, G:52, B:52, $\alpha=1.0$)
- **Bottom Layer (B.Cu) Traces**: `#4D7FC4` (Medium Blue: R:77, G:127, B:196, $\alpha=1.0$)
- **Inner Layers**:
  - In1: `#7FC87F` (Light Green: R:127, G:200, B:127)
  - In2: `#CE7D2C` (Amber Orange: R:206, G:125, B:44)
  - In3: `#4FCBCB` (Cyan Teal: R:79, G:203, B:203)
  - In4: `#DB628B` (Pink/Magenta: R:219, G:98, B:139)
- **Pads & Vias**: `#E3B72E` (Warm Gold: R:227, G:183, B:46)
- **Conduction Areas**: Greenish (`#009600`), translucency $\alpha \approx 0.20$
- **Keepout Areas**: Cyan (`#1AC4D2`), translucency $\alpha \approx 0.20$
- **Board Outline**: Light Blue (`#6496FF`)
- **Ratsnest (Air lines)**: Pure White (`#FFFFFF`, $\alpha=1.0$)
- **Clearance Violations**: Magenta (`#FF00FF`)
- **Selection Highlight**: Pure White (`#FFFFFF`, $\alpha=0.80$)

---

## 3. Comparison Scenarios & Naming Conventions

All comparison assets are stored in `docs/gui-parity/`:

| Scenario | Java GUI Capture | fr-gui Live Window Capture | fr-gui Offline Rasterization | Validation Criteria |
|---|---|---|---|---|
| **Initial Load** | `java-<board>-initial.png` | `screencapture -l <wid>` (标为“未验证（显示器锁定）”当锁屏时) | `offline-export-<board>.png` | Board bounds, pads, outline, initial unrouted air lines match |
| **Layer Isolation** | `java-<board>-fcu.png` | `screencapture -l <wid>` | `offline-export-<board>-fcu.png` | Top copper visibility filter matches |
| **In-Flight Routing** | `java-<board>-routing.png` | `screencapture -l <wid>` | `offline-export-<board>-routing.png` | Live trace additions, ripped-up air lines, progress stats update |
| **Completed Route** | `java-<board>-complete.png` | `screencapture -l <wid>` | `offline-export-<board>-complete.png` | Final copper layout, zero or residual air lines |

*Note on Screen Capture Protocol: Live window captures use `screencapture -l <windowid>`. When macOS display sleep / screen lock is active (`CGSSessionScreenIsLocked=Yes`), WindowServer denies frame buffer capture; in this environment, live captures are explicitly documented as '未验证（显示器锁定）' rather than substituting offline exports.*

---

## 4. Execution Workflow

```bash
# 1. Launch side-by-side runner
./bench/tools/gui_parity_side_by_side.sh issue039 --both --arrange

# 2. Capture screenshot
./bench/tools/gui_parity_side_by_side.sh issue039 --capture docs/gui-parity/side-by-side-issue039.png

# 3. Check feature checklist
python3 bench/tools/gui_parity_check.py --report
```
