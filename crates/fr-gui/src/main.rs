//! fr-gui: Interactive graphical interface for fast-routing built with GPUI.
//!
//! Provides window, menu bar, toolbar, layer management panel, hardware-accelerated
//! interactive canvas with zoom/pan/fit, mouse coordinates, and status bar.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    actions, canvas, div, fill, point, prelude::*, px, quad, rgb, size,
    App, Application, AsyncApp, Bounds, Context, FocusHandle, KeyBinding, KeyDownEvent,
    Menu, MenuItem, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    StyleRefinement, SystemMenuType, WeakEntity, Window, WindowBounds, WindowOptions,
};

mod export;
mod mock;
mod model;

use model::*;

// Define GPUI menu actions
actions!(
    fr_gui,
    [
        OpenDsn,
        SaveSes,
        Quit,
        FitView,
        ZoomIn,
        ZoomOut,
        ResetView,
        ToggleLayerPanel,
        AllLayersOn,
        AllLayersOff,
        RouteStart,
        RoutePause,
        RouteStop,
    ]
);

/// Main view representing the fast-routing GUI.
pub struct FrGuiApp {
    pub model: Arc<RenderModel>,
    pub filename: String,

    // Layer state
    pub layer_visible: Vec<bool>,
    pub active_layer: usize,
    pub show_layer_panel: bool,

    // Viewport transform (Board coordinates -> Screen canvas pixels)
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub center: [f64; 2],

    // Mouse drag state
    pub is_dragging: bool,
    pub drag_start_pos: Point<Pixels>,
    pub drag_start_pan: [f64; 2],

    // Mouse coordinates
    pub mouse_pos_board: Option<[f64; 2]>,

    // Routing status & system logs
    pub route_status: String,
    pub log_message: String,

    // Focus & performance tracking
    pub focus_handle: FocusHandle,
    pub frame_times: VecDeque<Instant>,
    pub fps: f64,
}

impl FrGuiApp {
    pub fn new(model: RenderModel, filename: String, cx: &mut Context<Self>) -> Self {
        let layer_count = model.layers.len();
        let bbox = model.bbox;
        let center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];

        // Initial default zoom based on viewport
        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        let initial_zoom = (900.0 / bw).min(650.0 / bh).max(1e-12);

        Self {
            model: Arc::new(model),
            filename,
            layer_visible: vec![true; layer_count],
            active_layer: 0,
            show_layer_panel: true,
            zoom: initial_zoom,
            pan_x: 0.0,
            pan_y: 0.0,
            center,
            is_dragging: false,
            drag_start_pos: Point::default(),
            drag_start_pan: [0.0, 0.0],
            mouse_pos_board: None,
            route_status: "Idle".into(),
            log_message: "Ready".into(),
            focus_handle: cx.focus_handle(),
            frame_times: VecDeque::with_capacity(60),
            fps: 60.0,
        }
    }

    /// Fits the board bounding box to the current canvas dimensions.
    pub fn fit_view(&mut self, viewport_width: f64, viewport_height: f64) {
        let bbox = self.model.bbox;
        self.center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
        self.pan_x = 0.0;
        self.pan_y = 0.0;

        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        let margin = 0.90;
        let zx = (viewport_width * margin) / bw;
        let zy = (viewport_height * margin) / bh;
        self.zoom = zx.min(zy).max(1e-12);
        self.log_message = format!("Fit view: zoom {:.6} px/unit", self.zoom);
    }

    /// Zooms in by 25%.
    pub fn zoom_in(&mut self) {
        self.zoom *= 1.25;
        self.log_message = format!("Zoom: {:.6} px/unit", self.zoom);
    }

    /// Zooms out by 20%.
    pub fn zoom_out(&mut self) {
        self.zoom /= 1.25;
        self.log_message = format!("Zoom: {:.6} px/unit", self.zoom);
    }

    /// Zooms keeping the board position under the cursor stationary.
    pub fn zoom_at(&mut self, mouse_win: Point<Pixels>, factor: f64, ox: f64, oy: f64, vw: f64, vh: f64) {
        let cx = vw / 2.0;
        let cy = vh / 2.0;
        let canvas_mx = f64::from(mouse_win.x) - ox;
        let canvas_my = f64::from(mouse_win.y) - oy;

        let bx = self.center[0] + (canvas_mx - cx - self.pan_x) / self.zoom;
        let by = self.center[1] - (canvas_my - cy - self.pan_y) / self.zoom;

        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(1e-12, 1e4);
        self.zoom = new_zoom;

        self.pan_x = canvas_mx - cx - (bx - self.center[0]) * new_zoom;
        self.pan_y = canvas_my - cy + (by - self.center[1]) * new_zoom;
    }

    pub fn reset_view(&mut self) {
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        let bbox = self.model.bbox;
        self.center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        self.zoom = (900.0 / bw).min(650.0 / bh).max(1e-12);
        self.log_message = "Reset view to center".into();
    }

    pub fn toggle_layer_visibility(&mut self, idx: usize) {
        if idx < self.layer_visible.len() {
            self.layer_visible[idx] = !self.layer_visible[idx];
            let name = &self.model.layers[idx].name;
            let state = if self.layer_visible[idx] { "visible" } else { "hidden" };
            self.log_message = format!("Layer '{}' is now {}", name, state);
        }
    }

    pub fn set_active_layer(&mut self, idx: usize) {
        if idx < self.model.layers.len() {
            self.active_layer = idx;
            // Also ensure active layer is visible
            if idx < self.layer_visible.len() {
                self.layer_visible[idx] = true;
            }
            self.log_message = format!("Active layer set to '{}'", self.model.layers[idx].name);
        }
    }

    pub fn all_layers_on(&mut self) {
        for v in &mut self.layer_visible {
            *v = true;
        }
        self.log_message = "All layers visible".into();
    }

    pub fn all_layers_off(&mut self) {
        for v in &mut self.layer_visible {
            *v = false;
        }
        // Keep active layer visible
        if self.active_layer < self.layer_visible.len() {
            self.layer_visible[self.active_layer] = true;
        }
        self.log_message = "All layers hidden except active layer".into();
    }

    pub fn toggle_layer_panel(&mut self) {
        self.show_layer_panel = !self.show_layer_panel;
        self.log_message = if self.show_layer_panel {
            "Layer panel shown".into()
        } else {
            "Layer panel hidden".into()
        };
    }

    pub fn route_start(&mut self) {
        self.route_status = "Running".into();
        self.log_message = "Route: Started autorouting (engine bridge placeholder)".into();
    }

    pub fn route_pause(&mut self) {
        self.route_status = "Paused".into();
        self.log_message = "Route: Paused autorouting".into();
    }

    pub fn route_stop(&mut self) {
        self.route_status = "Stopped".into();
        self.log_message = "Route: Stopped autorouting".into();
    }

    pub fn load_file(&mut self, path: &Path) {
        match mock::load(path) {
            Ok(new_model) => {
                let layer_count = new_model.layers.len();
                self.filename = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unnamed.dsn".into());
                self.layer_visible = vec![true; layer_count];
                self.active_layer = 0;
                self.model = Arc::new(new_model);
                self.reset_view();
                self.log_message = format!(
                    "Loaded {} ({} layers, {} items)",
                    self.filename, layer_count, self.model.stats.items
                );
            }
            Err(e) => {
                self.log_message = format!("Failed to load {}: {}", path.display(), e);
            }
        }
    }

    fn update_fps(&mut self) {
        let now = Instant::now();
        self.frame_times.push_back(now);
        while let Some(&front) = self.frame_times.front() {
            if now.duration_since(front).as_secs_f64() > 1.0 {
                self.frame_times.pop_front();
            } else {
                break;
            }
        }
        if self.frame_times.len() >= 2 {
            self.fps = (self.frame_times.len() as f64)
                / now.duration_since(*self.frame_times.front().unwrap()).as_secs_f64().max(0.001);
        }
    }
}

impl Render for FrGuiApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_fps();

        let model = Arc::clone(&self.model);
        let layer_visible = self.layer_visible.clone();
        let active_layer = self.active_layer;
        let zoom = self.zoom;
        let pan_x = self.pan_x;
        let pan_y = self.pan_y;
        let center = self.center;

        let filename = self.filename.clone();
        let show_panel = self.show_layer_panel;
        let log_msg = self.log_message.clone();
        let route_status = self.route_status.clone();
        let stats = model.stats;
        let res_mm = model.resolution;
        let mouse_coord = self.mouse_pos_board;

        // Colors
        let bg_app = rgb(0x181820);
        let bg_toolbar = rgb(0x242732);
        let bg_panel = rgb(0x1E2028);
        let bg_statusbar = rgb(0x161820);
        let border_col = rgb(0x353A48);
        let text_main = rgb(0xE0E0E0);
        let text_dim = rgb(0x9094A0);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg_app)
            .text_color(text_main)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.to_lowercase();
                match key.as_str() {
                    "f" => {
                        this.fit_view(1000.0, 650.0);
                        cx.notify();
                    }
                    "+" | "=" => {
                        this.zoom_in();
                        cx.notify();
                    }
                    "-" | "_" => {
                        this.zoom_out();
                        cx.notify();
                    }
                    "0" => {
                        this.reset_view();
                        cx.notify();
                    }
                    "l" => {
                        this.toggle_layer_panel();
                        cx.notify();
                    }
                    "r" => {
                        this.route_start();
                        cx.notify();
                    }
                    "p" => {
                        this.route_pause();
                        cx.notify();
                    }
                    "s" => {
                        this.route_stop();
                        cx.notify();
                    }
                    "arrowup" => {
                        this.pan_y += 50.0;
                        cx.notify();
                    }
                    "arrowdown" => {
                        this.pan_y -= 50.0;
                        cx.notify();
                    }
                    "arrowleft" => {
                        this.pan_x += 50.0;
                        cx.notify();
                    }
                    "arrowright" => {
                        this.pan_x -= 50.0;
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            // 1. Top Toolbar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .h(px(40.0))
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(bg_toolbar)
                    .border_b_1()
                    .border_color(border_col)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            // File group
                            .child(toolbar_button(
                                "Open DSN",
                                "Cmd+O",
                                rgb(0x3B4252),
                                cx.listener(|this, _, window, cx| {
                                    prompt_open_dsn(this, window, cx);
                                }),
                            ))
                            .child(toolbar_button(
                                "Save SES",
                                "Cmd+S",
                                rgb(0x3B4252),
                                cx.listener(|this, _, _window, cx| {
                                    this.log_message = "SES export bridge placeholder".into();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_separator(border_col))
                            // View group
                            .child(toolbar_button(
                                "Fit (F)",
                                "F",
                                rgb(0x2E5EAA),
                                cx.listener(|this, _, _window, cx| {
                                    this.fit_view(1000.0, 650.0);
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "Zoom +",
                                "+",
                                rgb(0x3B4252),
                                cx.listener(|this, _, _window, cx| {
                                    this.zoom_in();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "Zoom -",
                                "-",
                                rgb(0x3B4252),
                                cx.listener(|this, _, _window, cx| {
                                    this.zoom_out();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "Reset",
                                "0",
                                rgb(0x3B4252),
                                cx.listener(|this, _, _window, cx| {
                                    this.reset_view();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "Layers",
                                "L",
                                if show_panel { rgb(0x4A5568) } else { rgb(0x2D3748) },
                                cx.listener(|this, _, _window, cx| {
                                    this.toggle_layer_panel();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_separator(border_col))
                            // Route control group
                            .child(toolbar_button(
                                "▶ Route",
                                "R",
                                rgb(0x285E3E),
                                cx.listener(|this, _, _window, cx| {
                                    this.route_start();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "⏸ Pause",
                                "P",
                                rgb(0x7B681E),
                                cx.listener(|this, _, _window, cx| {
                                    this.route_pause();
                                    cx.notify();
                                }),
                            ))
                            .child(toolbar_button(
                                "⏹ Stop",
                                "S",
                                rgb(0x782828),
                                cx.listener(|this, _, _window, cx| {
                                    this.route_stop();
                                    cx.notify();
                                }),
                            )),
                    )
                    // Design file info pill
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x1E2028))
                            .border_1()
                            .border_color(border_col)
                            .text_xs()
                            .child(
                                div()
                                    .size_2()
                                    .rounded_full()
                                    .bg(match route_status.as_str() {
                                        "Running" => rgb(0x48BB78),
                                        "Paused" => rgb(0xECC94B),
                                        "Stopped" => rgb(0xF56565),
                                        _ => rgb(0xA0AEC0),
                                    }),
                            )
                            .child(format!("{}: {}", filename, route_status))
                            .child(
                                div()
                                    .text_color(text_dim)
                                    .child(format!("({} layers, {} items)", model.layers.len(), stats.items)),
                            ),
                    ),
            )
            // 2. Middle Body: Left Layer Panel + Center Canvas
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .size_full()
                    .overflow_hidden()
                    // Layer Panel (collapsible)
                    .when(show_panel, |el| {
                        el.child(render_layer_panel(
                            &model.layers,
                            &layer_visible,
                            active_layer,
                            bg_panel,
                            border_col,
                            text_main,
                            text_dim,
                            cx,
                        ))
                    })
                    // Main Canvas
                    .child(
                        div()
                            .id("canvas-container")
                            .flex_1()
                            .size_full()
                            .bg(rgb(0x001023)) // Java Freerouting default dark blue canvas
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                    this.is_dragging = true;
                                    this.drag_start_pos = event.position;
                                    this.drag_start_pan = [this.pan_x, this.pan_y];
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                                    this.is_dragging = false;
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                                if this.is_dragging {
                                    let dx = f64::from(event.position.x - this.drag_start_pos.x);
                                    let dy = f64::from(event.position.y - this.drag_start_pos.y);
                                    this.pan_x = this.drag_start_pan[0] + dx;
                                    this.pan_y = this.drag_start_pan[1] + dy;
                                }
                                let ox = if this.show_layer_panel { 220.0 } else { 0.0 };
                                let oy = 40.0;
                                let vw = 1000.0;
                                let vh = 650.0;
                                let canvas_mx = f64::from(event.position.x) - ox;
                                let canvas_my = f64::from(event.position.y) - oy;
                                let bx = this.center[0] + (canvas_mx - vw / 2.0 - this.pan_x) / this.zoom;
                                let by = this.center[1] - (canvas_my - vh / 2.0 - this.pan_y) / this.zoom;
                                this.mouse_pos_board = Some([bx, by]);
                                cx.notify();
                            }))
                            .on_scroll_wheel(cx.listener(
                                |this, event: &ScrollWheelEvent, _window, cx| {
                                    let delta_y = match event.delta {
                                        ScrollDelta::Pixels(p) => f64::from(p.y),
                                        ScrollDelta::Lines(l) => (l.y * 20.0) as f64,
                                    };
                                    let factor = if delta_y > 0.0 { 1.15 } else { 0.87 };
                                    let ox = if this.show_layer_panel { 220.0 } else { 0.0 };
                                    let oy = 40.0;
                                    let vw = 1000.0;
                                    let vh = 650.0;
                                    this.zoom_at(event.position, factor, ox, oy, vw, vh);
                                    cx.notify();
                                },
                            ))
                            .child(
                                canvas(
                                    move |_bounds, _window, _cx| {},
                                    move |bounds, _, window, _cx| {
                                        paint_pcb(
                                            window,
                                            bounds,
                                            &model,
                                            &layer_visible,
                                            active_layer,
                                            zoom,
                                            pan_x,
                                            pan_y,
                                            center,
                                        );
                                    },
                                )
                                .size_full(),
                            ),
                    ),
            )
            // 3. Bottom Status Bar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .h(px(26.0))
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(bg_statusbar)
                    .border_t_1()
                    .border_color(border_col)
                    .text_xs()
                    // Left: Mouse Coordinates (mm)
                    .child(
                        div().flex().flex_row().gap_3().child(match mouse_coord {
                            Some([bx, by]) => {
                                let mm_x = bx * res_mm;
                                let mm_y = by * res_mm;
                                format!("X: {:8.3} mm,  Y: {:8.3} mm  (Board: {:>8.0}, {:>8.0})", mm_x, mm_y, bx, by)
                            }
                            None => "X:    0.000 mm,  Y:    0.000 mm".into(),
                        }),
                    )
                    // Middle: Log Message
                    .child(
                        div()
                            .text_color(text_dim)
                            .child(format!("Log: {}", log_msg)),
                    )
                    // Right: Routing Stats & Performance
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .child(format!(
                                "Pass: {} | Incomplete: {} | Violations: {} | Score: {:.1} | Elapsed: {:.1}s | Items: {}",
                                stats.pass,
                                stats.incomplete,
                                stats.violations,
                                stats.score,
                                stats.elapsed_s,
                                stats.items
                            ))
                            .child(
                                div()
                                    .text_color(rgb(0x48BB78))
                                    .child(format!("{:.0} FPS", self.fps)),
                            ),
                    ),
            )
    }
}

// ============================================================================
// Painting Functions
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn paint_pcb(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    model: &RenderModel,
    layer_visible: &[bool],
    active_layer: usize,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    center: [f64; 2],
) {
    // Fill canvas background (#001023)
    window.paint_quad(fill(bounds, rgb(0x001023)));

    let ox = f64::from(bounds.origin.x);
    let oy = f64::from(bounds.origin.y);
    let vw = f64::from(bounds.size.width);
    let vh = f64::from(bounds.size.height);
    let cx = vw / 2.0;
    let cy = vh / 2.0;

    let to_screen = |bx: f64, by: f64| -> Point<Pixels> {
        let sx = ox + cx + (bx - center[0]) * zoom + pan_x;
        let sy = oy + cy - (by - center[1]) * zoom + pan_y; // Y flip
        point(px(sx as f32), px(sy as f32))
    };

    // Viewport board bounds for fast culling
    let margin_board = 50_000.0;
    let min_bx = center[0] + (-cx - pan_x) / zoom - margin_board;
    let max_bx = center[0] + (vw - cx - pan_x) / zoom + margin_board;
    let min_by = center[1] - (vh - cy - pan_y) / zoom - margin_board;
    let max_by = center[1] - (-cy - pan_y) / zoom + margin_board;
    let (cull_min_x, cull_max_x) = if min_bx < max_bx { (min_bx, max_bx) } else { (max_bx, min_bx) };
    let (cull_min_y, cull_max_y) = if min_by < max_by { (min_by, max_by) } else { (max_by, min_by) };

    // 1. Draw Areas (Outline, Keepouts, Planes)
    for area in &model.areas {
        if area.polygon.len() < 2 {
            continue;
        }
        if area.kind != AreaKind::Outline {
            let is_vis = layer_visible.get(area.layer as usize).copied().unwrap_or(true);
            if !is_vis {
                continue;
            }
        }

        // Viewport bounds check
        let first = area.polygon[0];
        if first[0] < cull_min_x || first[0] > cull_max_x || first[1] < cull_min_y || first[1] > cull_max_y {
            let mut inside = false;
            for p in &area.polygon {
                if p[0] >= cull_min_x && p[0] <= cull_max_x && p[1] >= cull_min_y && p[1] <= cull_max_y {
                    inside = true;
                    break;
                }
            }
            if !inside {
                continue;
            }
        }

        let mut builder = if area.kind == AreaKind::Outline {
            PathBuilder::stroke(px(1.5))
        } else {
            PathBuilder::fill()
        };

        for (i, p) in area.polygon.iter().enumerate() {
            let sp = to_screen(p[0], p[1]);
            if i == 0 {
                builder.move_to(sp);
            } else {
                builder.line_to(sp);
            }
        }
        builder.close();

        if let Ok(path) = builder.build() {
            match area.kind {
                AreaKind::Outline => {
                    window.paint_path(path, rgb(0x6496FF));
                }
                AreaKind::Keepout => {
                    let mut color = rgb(0x1AC4D2);
                    color.a = 0.25;
                    window.paint_path(path, color);
                }
                AreaKind::Plane => {
                    let c = model
                        .layers
                        .get(area.layer as usize)
                        .map(|l| l.color)
                        .unwrap_or([0.3, 0.3, 0.3, 0.2]);
                    let mut color = rgb(
                        ((c[0] * 255.0) as u32) << 16
                            | ((c[1] * 255.0) as u32) << 8
                            | ((c[2] * 255.0) as u32),
                    );
                    color.a = 0.20;
                    window.paint_path(path, color);
                }
                AreaKind::Other => {}
            }
        }
    }

    // 2. Draw Pads
    for pad in &model.pads {
        let is_vis = layer_visible.get(pad.layer as usize).copied().unwrap_or(true);
        if !is_vis || pad.polygon.len() < 3 {
            continue;
        }

        let p0 = pad.polygon[0];
        if p0[0] < cull_min_x || p0[0] > cull_max_x || p0[1] < cull_min_y || p0[1] > cull_max_y {
            continue;
        }

        let is_act = pad.layer as usize == active_layer;
        let mut color = rgb(0xE3B72E);
        if !is_act {
            color.a = 0.70;
        }

        // Fast path for rectangular SMD pads
        if pad.polygon.len() == 4 {
            let p0 = to_screen(pad.polygon[0][0], pad.polygon[0][1]);
            let p2 = to_screen(pad.polygon[2][0], pad.polygon[2][1]);
            let min_x = p0.x.min(p2.x);
            let max_x = p0.x.max(p2.x);
            let min_y = p0.y.min(p2.y);
            let max_y = p0.y.max(p2.y);
            let pad_bounds = Bounds::from_corners(point(min_x, min_y), point(max_x, max_y));
            window.paint_quad(fill(pad_bounds, color));
        } else {
            let mut builder = PathBuilder::fill();
            for (i, p) in pad.polygon.iter().enumerate() {
                let sp = to_screen(p[0], p[1]);
                if i == 0 {
                    builder.move_to(sp);
                } else {
                    builder.line_to(sp);
                }
            }
            builder.close();
            if let Ok(path) = builder.build() {
                window.paint_path(path, color);
            }
        }

        // Through-hole drill hole
        if !pad.is_smd {
            let center_x = (pad.polygon[0][0] + pad.polygon[2][0]) / 2.0;
            let center_y = (pad.polygon[0][1] + pad.polygon[2][1]) / 2.0;
            let sp = to_screen(center_x, center_y);
            let hr = px(2.5);
            let hole_bounds = Bounds::new(
                point(sp.x - hr, sp.y - hr),
                size(px(5.0), px(5.0)),
            );
            window.paint_quad(quad(
                hole_bounds,
                hr,
                rgb(0x001023),
                px(0.),
                gpui::transparent_black(),
                Default::default(),
            ));
        }
    }

    // 3. Draw Traces
    for trace in &model.traces {
        let is_vis = layer_visible.get(trace.layer as usize).copied().unwrap_or(true);
        if !is_vis || trace.points.len() < 2 {
            continue;
        }

        let p0 = trace.points[0];
        if p0[0] < cull_min_x || p0[0] > cull_max_x || p0[1] < cull_min_y || p0[1] > cull_max_y {
            let last = trace.points.last().unwrap();
            if last[0] < cull_min_x || last[0] > cull_max_x || last[1] < cull_min_y || last[1] > cull_max_y {
                continue;
            }
        }

        let is_act = trace.layer as usize == active_layer;
        let c = model
            .layers
            .get(trace.layer as usize)
            .map(|l| l.color)
            .unwrap_or([0.8, 0.2, 0.2, 1.0]);
        let mut color = rgb(
            ((c[0] * 255.0) as u32) << 16
                | ((c[1] * 255.0) as u32) << 8
                | ((c[2] * 255.0) as u32),
        );
        if !is_act {
            color.a = 0.60;
        }

        let width = ((trace.half_width * 2.0 * zoom) as f32).max(1.0);
        let mut builder = PathBuilder::stroke(px(width));
        for (i, p) in trace.points.iter().enumerate() {
            let sp = to_screen(p[0], p[1]);
            if i == 0 {
                builder.move_to(sp);
            } else {
                builder.line_to(sp);
            }
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    // 4. Draw Vias
    for via in &model.vias {
        let from_vis = layer_visible.get(via.layer_from as usize).copied().unwrap_or(true);
        let to_vis = layer_visible.get(via.layer_to as usize).copied().unwrap_or(true);
        if !from_vis && !to_vis {
            continue;
        }

        if via.center[0] < cull_min_x || via.center[0] > cull_max_x || via.center[1] < cull_min_y || via.center[1] > cull_max_y {
            continue;
        }

        let sp = to_screen(via.center[0], via.center[1]);
        let r = ((via.radius * zoom) as f32).max(2.0);
        let vbounds = Bounds::new(point(sp.x - px(r), sp.y - px(r)), size(px(r * 2.0), px(r * 2.0)));
        window.paint_quad(quad(
            vbounds,
            px(r),
            rgb(0xE3B72E),
            px(0.),
            gpui::transparent_black(),
            Default::default(),
        ));

        let hr = (r * 0.5).max(1.0);
        let hbounds = Bounds::new(point(sp.x - px(hr), sp.y - px(hr)), size(px(hr * 2.0), px(hr * 2.0)));
        window.paint_quad(quad(
            hbounds,
            px(hr),
            rgb(0x001023),
            px(0.),
            gpui::transparent_black(),
            Default::default(),
        ));
    }

    // 5. Draw Ratsnest (Incomplete flylines)
    for rat in &model.ratsnest {
        let sp1 = to_screen(rat.from[0], rat.from[1]);
        let sp2 = to_screen(rat.to[0], rat.to[1]);
        let mut builder = PathBuilder::stroke(px(1.0));
        builder = builder.dash_array(&[px(4.0), px(3.0)]);
        builder.move_to(sp1);
        builder.line_to(sp2);
        if let Ok(path) = builder.build() {
            let mut white = rgb(0xFFFFFF);
            white.a = 0.85;
            window.paint_path(path, white);
        }
    }
}

// ============================================================================
// UI Components & Helpers
// ============================================================================

fn toolbar_button(
    label: &'static str,
    _shortcut: &'static str,
    bg_color: gpui::Rgba,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(bg_color)
        .border_1()
        .border_color(rgb(0x4A5568))
        .text_xs()
        .cursor_pointer()
        .hover(|mut style: StyleRefinement| {
            style.opacity = Some(0.85);
            style
        })
        .child(label)
        .on_mouse_down(MouseButton::Left, on_click)
}

fn toolbar_separator(border_col: gpui::Rgba) -> impl IntoElement {
    div().w(px(1.0)).h(px(20.0)).bg(border_col).mx_1()
}

#[allow(clippy::too_many_arguments)]
fn render_layer_panel(
    layers: &[LayerInfo],
    layer_visible: &[bool],
    active_layer: usize,
    bg_panel: gpui::Rgba,
    border_col: gpui::Rgba,
    text_main: gpui::Rgba,
    text_dim: gpui::Rgba,
    cx: &mut Context<FrGuiApp>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px(220.0))
        .bg(bg_panel)
        .border_r_1()
        .border_color(border_col)
        .child(
            // Panel Header
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(border_col)
                .child(
                    div()
                        .text_sm()
                        .child(format!("Layers ({})", layers.len())),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(rgb(0x2D3748))
                                .text_xs()
                                .cursor_pointer()
                                .child("All")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _window, cx| {
                                        this.all_layers_on();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(rgb(0x2D3748))
                                .text_xs()
                                .cursor_pointer()
                                .child("None")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _window, cx| {
                                        this.all_layers_off();
                                        cx.notify();
                                    }),
                                ),
                        ),
                ),
        )
        // Scrollable Layer List
        .child(
            div()
                .id("layer-list")
                .flex()
                .flex_col()
                .overflow_scroll()
                .children(layers.iter().enumerate().map(|(i, layer)| {
                    let is_vis = layer_visible.get(i).copied().unwrap_or(true);
                    let is_act = i == active_layer;
                    let c = layer.color;
                    let swatch_col = rgb(
                        ((c[0] * 255.0) as u32) << 16
                            | ((c[1] * 255.0) as u32) << 8
                            | ((c[2] * 255.0) as u32),
                    );

                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .bg(if is_act { rgb(0x2D3748) } else { rgb(0x1E2028) })
                        .hover(|mut style: StyleRefinement| {
                            style.background = Some(rgb(0x3A475C).into());
                            style
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _window, cx| {
                                this.set_active_layer(i);
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                // Visibility checkbox
                                .child(
                                    div()
                                        .size_4()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(border_col)
                                        .bg(if is_vis { rgb(0x4A90E2) } else { rgb(0x2D3139) })
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _window, cx| {
                                                this.toggle_layer_visibility(i);
                                                cx.notify();
                                            }),
                                        ),
                                )
                                // Color swatch
                                .child(div().size_3().rounded_sm().bg(swatch_col))
                                // Layer name
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if is_vis { text_main } else { text_dim })
                                        .child(layer.name.clone()),
                                ),
                        )
                        // Layer badge: Sig or Pln, plus active marker
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(if layer.is_signal { rgb(0x234E52) } else { rgb(0x4A5568) })
                                        .text_xs()
                                        .child(if layer.is_signal { "Sig" } else { "Pln" }),
                                )
                                .when(is_act, |s| {
                                    s.child(div().size_2().rounded_full().bg(rgb(0x48BB78)))
                                }),
                        )
                })),
        )
}

fn prompt_open_dsn(_this: &mut FrGuiApp, _window: &mut Window, cx: &mut Context<FrGuiApp>) {
    let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Select Specctra DSN Design File".into()),
    });

    cx.spawn(async move |this: WeakEntity<FrGuiApp>, cx: &mut AsyncApp| {
        if let Ok(Ok(Some(paths))) = receiver.await {
            if let Some(path) = paths.into_iter().next() {
                let _ = this.update(cx, |this, cx| {
                    this.load_file(&path);
                    cx.notify();
                });
            }
        }
    })
    .detach();
}

// ============================================================================
// Entrypoint & Command Line Runner
// ============================================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut open_path: Option<String> = None;
    let mut screenshot_path: Option<String> = None;
    let mut zoom_factor: Option<f64> = None;
    let mut layers_filter: Option<Vec<bool>> = None;
    let mut active_layer_cli: usize = 0;
    let mut fps_bench = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--open" | "-o" => {
                if i + 1 < args.len() {
                    open_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--screenshot" => {
                if i + 1 < args.len() {
                    screenshot_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--zoom-factor" => {
                if i + 1 < args.len() {
                    zoom_factor = args[i + 1].parse().ok();
                    i += 1;
                }
            }
            "--active-layer" => {
                if i + 1 < args.len() {
                    active_layer_cli = args[i + 1].parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--layers-vis" => {
                if i + 1 < args.len() {
                    let bools: Vec<bool> = args[i + 1]
                        .split(',')
                        .map(|s| s.trim() == "1" || s.trim().eq_ignore_ascii_case("true"))
                        .collect();
                    layers_filter = Some(bools);
                    i += 1;
                }
            }
            "--fps-bench" => {
                fps_bench = true;
            }
            "--help" | "-h" => {
                println!("fr-gui: GPUI interactive router interface");
                println!("Usage: fr-gui [OPTIONS]");
                println!();
                println!("Options:");
                println!("  --open <file.dsn>          Open specific DSN file on launch");
                println!("  --screenshot <out.png>     Export high-resolution screenshot and exit");
                println!("  --zoom-factor <float>      Zoom factor for screenshot (e.g. 3.0 for detail zoom)");
                println!("  --active-layer <index>     Active layer index for screenshot (0-based)");
                println!("  --layers-vis <1,0,1...>    Comma-separated layer visibility for screenshot");
                println!("  --fps-bench                Run 100 frames FPS performance benchmark");
                println!("  --help                     Print this help");
                return;
            }
            _ => {
                if args[i].ends_with(".dsn") {
                    open_path = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    // Determine initial design model to load
    let (model, filename) = match open_path {
        Some(ref path_str) => match mock::load(path_str) {
            Ok(m) => (m, Path::new(path_str).file_name().unwrap().to_string_lossy().to_string()),
            Err(e) => {
                eprintln!("Warning: Failed to load '{}': {}. Falling back to sample board.", path_str, e);
                (mock::sample_board(), "sample-4layer.dsn".to_string())
            }
        },
        None => {
            let default_fixture = "bench/fixtures-oos/cla45-compute-hdi6cs.dsn";
            if Path::new(default_fixture).exists() {
                match mock::load(default_fixture) {
                    Ok(m) => (m, "cla45-compute-hdi6cs.dsn".to_string()),
                    Err(_) => (mock::sample_board(), "sample-4layer.dsn".to_string()),
                }
            } else {
                (mock::sample_board(), "sample-4layer.dsn".to_string())
            }
        }
    };

    // If --fps-bench is requested, run 100 rendering cycles and report FPS
    if fps_bench {
        println!("=== Running fr-gui FPS benchmark on '{}' ===", filename);
        println!("Total items: {}", model.stats.items);
        println!("Layers: {}", model.layers.len());
        let layers_vis = vec![true; model.layers.len()];
        let start = Instant::now();
        let frames = 100;
        for _ in 0..frames {
            let _ = export::render_gui_screenshot(&model, &filename, &layers_vis, 0, 1280, 800, None, "/tmp/bench_frame.png");
        }
        let elapsed = start.elapsed().as_secs_f64();
        let fps = (frames as f64) / elapsed;
        println!("Rendered {} frames in {:.3} s: {:.1} FPS (software rasterizer)", frames, elapsed, fps);
        println!("Note: GPUI Metal hardware-accelerated pipeline runs at 60-120 FPS vsync.");
        return;
    }

    // If --screenshot is requested, export and exit
    if let Some(out_path) = screenshot_path {
        let layers_vis = match layers_filter {
            Some(f) => f,
            None => vec![true; model.layers.len()],
        };
        println!("Generating screenshot to '{}' for '{}'...", out_path, filename);
        if let Err(e) = export::render_gui_screenshot(&model, &filename, &layers_vis, active_layer_cli, 1280, 800, zoom_factor, &out_path) {
            eprintln!("Screenshot failed: {}", e);
            std::process::exit(1);
        }
        println!("Screenshot successfully saved to '{}'", out_path);
        return;
    }

    // Launch interactive GPUI application
    Application::new().run(move |cx: &mut App| {
        cx.activate(true);

        // Register application menus
        cx.set_menus(vec![
            Menu {
                name: "fr-gui".into(),
                items: vec![
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit fr-gui", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open DSN…", OpenDsn),
                    MenuItem::action("Save SES…", SaveSes),
                    MenuItem::separator(),
                    MenuItem::action("Quit", Quit),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Fit to Window", FitView),
                    MenuItem::action("Zoom In", ZoomIn),
                    MenuItem::action("Zoom Out", ZoomOut),
                    MenuItem::action("Reset View", ResetView),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Layer Panel", ToggleLayerPanel),
                    MenuItem::action("All Layers Visible", AllLayersOn),
                    MenuItem::action("Hide Other Layers", AllLayersOff),
                ],
            },
            Menu {
                name: "Route".into(),
                items: vec![
                    MenuItem::action("Start Autorouting", RouteStart),
                    MenuItem::action("Pause Autorouting", RoutePause),
                    MenuItem::action("Stop Autorouting", RouteStop),
                ],
            },
        ]);

        // Keybindings
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("f", FitView, None),
            KeyBinding::new("=", ZoomIn, None),
            KeyBinding::new("+", ZoomIn, None),
            KeyBinding::new("-", ZoomOut, None),
            KeyBinding::new("0", ResetView, None),
            KeyBinding::new("l", ToggleLayerPanel, None),
            KeyBinding::new("r", RouteStart, None),
            KeyBinding::new("p", RoutePause, None),
            KeyBinding::new("s", RouteStop, None),
        ]);

        // Global action handlers
        cx.on_action(|_: &Quit, cx| cx.quit());

        // Window configuration
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let win_title = format!("fast-routing - [{}]", filename);

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(win_title.into()),
                        appears_transparent: false,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| FrGuiApp::new(model, filename, cx)),
            )
            .expect("Failed to open main window");

        // Focus window
        window
            .update(cx, |view, window, _cx| {
                window.focus(&view.focus_handle);
            })
            .ok();
    });
}
