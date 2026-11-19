//! fr-gui: Interactive graphical interface for fast-routing built with GPUI.
//!
//! Provides interactive routing canvas, bridge thread runner, parameter panel,
//! live ratsnest incompletes inspector, event logging, layer visibility, and SES export.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, quad, rgb, size, AnyElement, App, Application, Bounds, Context,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, Timer, Window, WindowBounds, WindowOptions,
};

mod bridge;
mod export;
mod mock;

use bridge::{JobBridge, JobState};
use fr_core::snapshot::AreaKind;

/// Sidebar active tab selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideTab {
    Params,
    Incompletes,
    Logs,
    Layers,
}

/// The root GPUI application state and view.
pub struct FrGuiApp {
    pub bridge: JobBridge,
    pub active_tab: SideTab,

    // Layer management
    pub layer_visible: Vec<bool>,
    pub active_layer: usize,
    pub show_side_panel: bool,

    // Viewport coordinates (Board coordinates -> Screen canvas pixels)
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub center: [f64; 2],

    // Mouse interaction
    pub is_dragging: bool,
    pub drag_start_pos: Point<Pixels>,
    pub drag_start_pan: [f64; 2],
    pub mouse_pos_board: Option<[f64; 2]>,

    // Performance tracking
    pub focus_handle: FocusHandle,
    pub frame_times: VecDeque<Instant>,
    pub fps: f64,
}

impl FrGuiApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sample = mock::sample_board();
        let layer_count = sample.layers.len();
        let bbox = sample.bbox;
        let center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        let initial_zoom = (900.0 / bw).min(600.0 / bh).clamp(1e-12, 100.0);

        let mut bridge = JobBridge::new();
        bridge.current_model = Arc::new(sample);

        Self {
            bridge,
            active_tab: SideTab::Layers,
            layer_visible: vec![true; layer_count],
            active_layer: 0,
            show_side_panel: true,
            zoom: initial_zoom,
            pan_x: 0.0,
            pan_y: 0.0,
            center,
            is_dragging: false,
            drag_start_pos: Point::default(),
            drag_start_pan: [0.0, 0.0],
            mouse_pos_board: None,
            focus_handle: cx.focus_handle(),
            frame_times: VecDeque::with_capacity(60),
            fps: 60.0,
        }
    }

    /// Loads a design file into the bridge and updates the view bounds.
    pub fn load_dsn(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        match self.bridge.open_dsn(&path) {
            Ok(()) => {
                let layer_count = self.bridge.current_model.layers.len();
                self.layer_visible = vec![true; layer_count];
                self.active_layer = 0;
                self.fit_view(900.0, 600.0);
                Ok(())
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to open DSN '{}': {}. Falling back to sample board.",
                    path.as_ref().display(),
                    e
                );
                let sample = mock::sample_board();
                let layer_count = sample.layers.len();
                self.layer_visible = vec![true; layer_count];
                self.active_layer = 0;
                self.bridge.current_model = Arc::new(sample);
                self.fit_view(900.0, 600.0);
                Err(e)
            }
        }
    }

    /// Fits the board bounding box to the given viewport width and height.
    pub fn fit_view(&mut self, viewport_width: f64, viewport_height: f64) {
        let bbox = self.bridge.current_model.bbox;
        self.center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
        self.pan_x = 0.0;
        self.pan_y = 0.0;

        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        let margin = 0.88;
        let zx = (viewport_width * margin) / bw;
        let zy = (viewport_height * margin) / bh;
        self.zoom = zx.min(zy).clamp(1e-12, 100.0);
    }

    /// Zooms in by 25%.
    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).clamp(1e-12, 100.0);
    }

    /// Zooms out by 20%.
    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).clamp(1e-12, 100.0);
    }

    /// Zooms keeping the board position under the cursor stationary.
    pub fn zoom_at(
        &mut self,
        mouse_win: Point<Pixels>,
        factor: f64,
        ox: f64,
        oy: f64,
        vw: f64,
        vh: f64,
    ) {
        let cx = vw / 2.0;
        let cy = vh / 2.0;
        let canvas_mx = mouse_win.x.to_f64() - ox;
        let canvas_my = mouse_win.y.to_f64() - oy;

        let bx = self.center[0] + (canvas_mx - cx - self.pan_x) / self.zoom;
        let by = self.center[1] - (canvas_my - cy - self.pan_y) / self.zoom;

        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(1e-12, 100.0);
        self.zoom = new_zoom;

        self.pan_x = canvas_mx - cx - (bx - self.center[0]) * new_zoom;
        self.pan_y = canvas_my - cy + (by - self.center[1]) * new_zoom;
    }

    /// Resets view to center of board bounding box.
    pub fn reset_view(&mut self) {
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        let bbox = self.bridge.current_model.bbox;
        self.center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
        let bw = (bbox[2] - bbox[0]).abs().max(1.0);
        let bh = (bbox[3] - bbox[1]).abs().max(1.0);
        self.zoom = (900.0 / bw).min(600.0 / bh).clamp(1e-12, 100.0);
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
                / now
                    .duration_since(*self.frame_times.front().unwrap())
                    .as_secs_f64()
                    .max(0.001);
        }
    }
}

impl Render for FrGuiApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_fps();

        // Synchronize layer visibility vector length if model changed
        let model_layers = self.bridge.current_model.layers.len();
        if self.layer_visible.len() != model_layers {
            self.layer_visible = vec![true; model_layers];
            self.active_layer = 0;
        }

        let model = Arc::clone(&self.bridge.current_model);
        let stats = model.stats;
        let layer_visible = self.layer_visible.clone();
        let active_layer = self.active_layer;
        let zoom = self.zoom;
        let pan_x = self.pan_x;
        let pan_y = self.pan_y;
        let center = self.center;
        let selected_inc = self.bridge.selected_incomplete;

        let dsn_title = self
            .bridge
            .params
            .dsn_path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "sample-4layer.dsn".into());

        let job_state = self.bridge.state;
        let active_tab = self.active_tab;
        let show_panel = self.show_side_panel;
        let current_fps = self.fps;
        let mouse_coord = self.mouse_pos_board;
        let res_mm = model.resolution;

        // Theme palette
        let bg_app = rgb(0x13151D);
        let bg_toolbar = rgb(0x1E212B);
        let bg_sidebar = rgb(0x181A22);
        let bg_status = rgb(0x101217);
        let border_col = rgb(0x2D313E);
        let text_main = rgb(0xEDEFEF);
        let text_dim = rgb(0x8C92A4);

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
                        this.fit_view(900.0, 600.0);
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
                        this.show_side_panel = !this.show_side_panel;
                        cx.notify();
                    }
                    "r" => {
                        let _ = this.bridge.start();
                        cx.notify();
                    }
                    "p" => {
                        if this.bridge.state == JobState::Paused {
                            this.bridge.resume();
                        } else {
                            this.bridge.pause();
                        }
                        cx.notify();
                    }
                    "s" => {
                        this.bridge.stop();
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
            // Top Toolbar
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .px_4()
                    .bg(bg_toolbar)
                    .border_b_1()
                    .border_color(border_col)
                    // Left: App title & Design name & State badge
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("fast-routing GUI"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(rgb(0x2B3040))
                                    .text_xs()
                                    .text_color(text_dim)
                                    .child(dsn_title),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .text_xs()
                                    .bg(match job_state {
                                        JobState::Running => rgb(0x22543D),
                                        JobState::Paused => rgb(0x744210),
                                        JobState::Finished => rgb(0x2A4365),
                                        JobState::Stopped => rgb(0x742A2A),
                                        _ => rgb(0x353A48),
                                    })
                                    .text_color(rgb(0xFFFFFF))
                                    .child(job_state.as_str()),
                            ),
                    )
                    // Center: Actions (Start, Pause, Stop, Fit, Save SES)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(make_button("▶ Start", cx, |this, cx| {
                                let _ = this.bridge.start();
                                cx.notify();
                            }))
                            .child(make_button(
                                if job_state == JobState::Paused {
                                    "▶ Resume"
                                } else {
                                    "⏸ Pause"
                                },
                                cx,
                                |this, cx| {
                                    if this.bridge.state == JobState::Paused {
                                        this.bridge.resume();
                                    } else {
                                        this.bridge.pause();
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(make_button("⏹ Stop", cx, |this, cx| {
                                this.bridge.stop();
                                cx.notify();
                            }))
                            .child(make_button("⛶ Fit (F)", cx, |this, cx| {
                                this.fit_view(900.0, 600.0);
                                cx.notify();
                            }))
                            .child(make_button("Zoom +", cx, |this, cx| {
                                this.zoom_in();
                                cx.notify();
                            }))
                            .child(make_button("Zoom -", cx, |this, cx| {
                                this.zoom_out();
                                cx.notify();
                            }))
                            .child(make_button("Reset (0)", cx, |this, cx| {
                                this.reset_view();
                                cx.notify();
                            }))
                            .child(make_button("💾 Save SES", cx, |this, cx| {
                                let default_path =
                                    this.bridge.params.dsn_path.with_extension("ses");
                                let _ = this.bridge.save_ses(default_path);
                                cx.notify();
                            })),
                    )
                    // Right: Side panel toggles
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(make_tab_btn(
                                "Params",
                                active_tab == SideTab::Params,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Params;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(make_tab_btn(
                                &format!("Incompletes ({})", self.bridge.incompletes.len()),
                                active_tab == SideTab::Incompletes,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Incompletes;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(make_tab_btn(
                                "Logs",
                                active_tab == SideTab::Logs,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Logs;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(make_tab_btn(
                                "Layers",
                                active_tab == SideTab::Layers,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Layers;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .bg(if show_panel {
                                        rgb(0x3B4254)
                                    } else {
                                        rgb(0x232733)
                                    })
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(if show_panel { "⇥" } else { "⇤" })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.show_side_panel = !this.show_side_panel;
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    ),
            )
            // Middle Main Region: Sidebar & Canvas
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .size_full()
                    .overflow_hidden()
                    // Sidebar
                    .when(show_panel, |el| {
                        el.child(
                            div()
                                .w(px(320.0))
                                .h_full()
                                .bg(bg_sidebar)
                                .border_r_1()
                                .border_color(border_col)
                                .p_3()
                                .child(match active_tab {
                                    SideTab::Params => render_params_tab(self, cx),
                                    SideTab::Incompletes => render_incompletes_tab(self, cx),
                                    SideTab::Logs => render_logs_tab(self, cx),
                                    SideTab::Layers => render_layers_tab(self, cx),
                                }),
                        )
                    })
                    // Canvas Viewport
                    .child(
                        div()
                            .id("canvas-container")
                            .flex_1()
                            .h_full()
                            .relative()
                            .bg(rgb(0x001023)) // Java Freerouting default dark blue canvas
                            .child(
                                canvas(
                                    move |_bounds, _window, _cx| {},
                                    move |bounds, _, window, _cx| {
                                        let cw = bounds.size.width.to_f64();
                                        let ch = bounds.size.height.to_f64();
                                        let cx_px = bounds.origin.x.to_f64() + cw / 2.0 + pan_x;
                                        let cy_px = bounds.origin.y.to_f64() + ch / 2.0 + pan_y;

                                        let to_screen = |bx: f64, by: f64| -> Point<Pixels> {
                                            let sx = cx_px + (bx - center[0]) * zoom;
                                            let sy = cy_px - (by - center[1]) * zoom; // Y flip
                                            point(px(sx as f32), px(sy as f32))
                                        };

                                        // Viewport culling bounds
                                        let margin_board = 50_000.0;
                                        let min_bx =
                                            center[0] + (-cw / 2.0 - pan_x) / zoom - margin_board;
                                        let max_bx =
                                            center[0] + (cw / 2.0 - pan_x) / zoom + margin_board;
                                        let min_by =
                                            center[1] - (ch / 2.0 - pan_y) / zoom - margin_board;
                                        let max_by =
                                            center[1] - (-ch / 2.0 - pan_y) / zoom + margin_board;
                                        let (cull_min_x, cull_max_x) = if min_bx < max_bx {
                                            (min_bx, max_bx)
                                        } else {
                                            (max_bx, min_bx)
                                        };
                                        let (cull_min_y, cull_max_y) = if min_by < max_by {
                                            (min_by, max_by)
                                        } else {
                                            (max_by, min_by)
                                        };

                                        // 1. Draw Areas (Planes, Outlines, Keepouts)
                                        for area in &model.areas {
                                            let is_vis = layer_visible
                                                .get(area.layer as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            if !is_vis || area.polygon.len() < 2 {
                                                continue;
                                            }

                                            let p0 = area.polygon[0];
                                            if p0[0] < cull_min_x
                                                || p0[0] > cull_max_x
                                                || p0[1] < cull_min_y
                                                || p0[1] > cull_max_y
                                            {
                                                let mut inside = false;
                                                for pt in &area.polygon {
                                                    if pt[0] >= cull_min_x
                                                        && pt[0] <= cull_max_x
                                                        && pt[1] >= cull_min_y
                                                        && pt[1] <= cull_max_y
                                                    {
                                                        inside = true;
                                                        break;
                                                    }
                                                }
                                                if !inside {
                                                    continue;
                                                }
                                            }

                                            let is_stroke = area.kind == AreaKind::Outline;
                                            if is_stroke {
                                                let mut spb = PathBuilder::stroke(px(1.5));
                                                let p0 = to_screen(
                                                    area.polygon[0][0],
                                                    area.polygon[0][1],
                                                );
                                                spb.move_to(p0);
                                                for pt in &area.polygon[1..] {
                                                    spb.line_to(to_screen(pt[0], pt[1]));
                                                }
                                                spb.close();
                                                if let Ok(path) = spb.build() {
                                                    window.paint_path(path, rgb(0x6496FF));
                                                }
                                            } else {
                                                let mut pb = PathBuilder::fill();
                                                let p0 = to_screen(
                                                    area.polygon[0][0],
                                                    area.polygon[0][1],
                                                );
                                                pb.move_to(p0);
                                                for pt in &area.polygon[1..] {
                                                    pb.line_to(to_screen(pt[0], pt[1]));
                                                }
                                                pb.close();
                                                if let Ok(path) = pb.build() {
                                                    match area.kind {
                                                        AreaKind::Keepout => {
                                                            let mut col = rgb(0x1AC4D2);
                                                            col.a = 0.25;
                                                            window.paint_path(path, col);
                                                        }
                                                        AreaKind::Plane => {
                                                            let c = model
                                                                .layers
                                                                .get(area.layer as usize)
                                                                .map(|l| l.color)
                                                                .unwrap_or([0.3, 0.3, 0.3, 0.2]);
                                                            let mut col =
                                                                rgb(((c[0] * 255.0) as u32) << 16
                                                                    | ((c[1] * 255.0) as u32) << 8
                                                                    | ((c[2] * 255.0) as u32));
                                                            col.a = 0.20;
                                                            window.paint_path(path, col);
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }

                                        // 2. Draw Component Pads / Pins (Non-active first, active on top)
                                        for pass in 0..2 {
                                            for pad in &model.pads {
                                                let is_act = pad.layer as usize == active_layer;
                                                if (pass == 0 && is_act) || (pass == 1 && !is_act) {
                                                    continue;
                                                }

                                                let is_vis = layer_visible
                                                    .get(pad.layer as usize)
                                                    .copied()
                                                    .unwrap_or(true);
                                                if !is_vis || pad.polygon.len() < 3 {
                                                    continue;
                                                }

                                                let p0 = pad.polygon[0];
                                                if p0[0] < cull_min_x
                                                    || p0[0] > cull_max_x
                                                    || p0[1] < cull_min_y
                                                    || p0[1] > cull_max_y
                                                {
                                                    continue;
                                                }

                                                let mut pad_col = rgb(0xE3B72E);
                                                if !is_act {
                                                    pad_col.a = 0.65;
                                                }

                                                // Fast path for 4-point rectangular SMD pads
                                                if pad.polygon.len() == 4 {
                                                    let p0 = to_screen(
                                                        pad.polygon[0][0],
                                                        pad.polygon[0][1],
                                                    );
                                                    let p2 = to_screen(
                                                        pad.polygon[2][0],
                                                        pad.polygon[2][1],
                                                    );
                                                    let min_x = p0.x.min(p2.x);
                                                    let max_x = p0.x.max(p2.x);
                                                    let min_y = p0.y.min(p2.y);
                                                    let max_y = p0.y.max(p2.y);
                                                    let pad_bounds = Bounds::from_corners(
                                                        point(min_x, min_y),
                                                        point(max_x, max_y),
                                                    );
                                                    window.paint_quad(fill(pad_bounds, pad_col));
                                                } else {
                                                    let mut pb = PathBuilder::fill();
                                                    let p0 = to_screen(
                                                        pad.polygon[0][0],
                                                        pad.polygon[0][1],
                                                    );
                                                    pb.move_to(p0);
                                                    for pt in &pad.polygon[1..] {
                                                        pb.line_to(to_screen(pt[0], pt[1]));
                                                    }
                                                    pb.close();
                                                    if let Ok(path) = pb.build() {
                                                        window.paint_path(path, pad_col);
                                                    }
                                                }

                                                // Through-hole drill hole
                                                if !pad.is_smd {
                                                    let center_x = (pad.polygon[0][0]
                                                        + pad.polygon[2][0])
                                                        / 2.0;
                                                    let center_y = (pad.polygon[0][1]
                                                        + pad.polygon[2][1])
                                                        / 2.0;
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
                                        }

                                        // 3. Draw Routed Traces (Non-active first, active on top)
                                        for pass in 0..2 {
                                            for trace in &model.traces {
                                                let is_act = trace.layer as usize == active_layer;
                                                if (pass == 0 && is_act) || (pass == 1 && !is_act) {
                                                    continue;
                                                }

                                                let is_vis = layer_visible
                                                    .get(trace.layer as usize)
                                                    .copied()
                                                    .unwrap_or(true);
                                                if !is_vis || trace.points.len() < 2 {
                                                    continue;
                                                }

                                                let p0 = trace.points[0];
                                                if p0[0] < cull_min_x
                                                    || p0[0] > cull_max_x
                                                    || p0[1] < cull_min_y
                                                    || p0[1] > cull_max_y
                                                {
                                                    let last = trace.points.last().unwrap();
                                                    if last[0] < cull_min_x
                                                        || last[0] > cull_max_x
                                                        || last[1] < cull_min_y
                                                        || last[1] > cull_max_y
                                                    {
                                                        continue;
                                                    }
                                                }

                                                let c = model
                                                    .layers
                                                    .get(trace.layer as usize)
                                                    .map(|l| l.color)
                                                    .unwrap_or([0.8, 0.2, 0.2, 1.0]);
                                                let mut trace_col = rgb(((c[0] * 255.0) as u32)
                                                    << 16
                                                    | ((c[1] * 255.0) as u32) << 8
                                                    | ((c[2] * 255.0) as u32));
                                                if !is_act {
                                                    trace_col.a = 0.55;
                                                }

                                                let stroke_w = ((trace.half_width * 2.0 * zoom)
                                                    as f32)
                                                    .max(1.0);
                                                let mut pb = PathBuilder::stroke(px(stroke_w));
                                                let p0 = to_screen(
                                                    trace.points[0][0],
                                                    trace.points[0][1],
                                                );
                                                pb.move_to(p0);
                                                for pt in &trace.points[1..] {
                                                    pb.line_to(to_screen(pt[0], pt[1]));
                                                }
                                                if let Ok(path) = pb.build() {
                                                    window.paint_path(path, trace_col);
                                                }
                                            }
                                        }

                                        // 4. Draw Vias
                                        for via in &model.vias {
                                            let from_vis = layer_visible
                                                .get(via.layer_from as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            let to_vis = layer_visible
                                                .get(via.layer_to as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            if !from_vis && !to_vis {
                                                continue;
                                            }

                                            if via.center[0] < cull_min_x
                                                || via.center[0] > cull_max_x
                                                || via.center[1] < cull_min_y
                                                || via.center[1] > cull_max_y
                                            {
                                                continue;
                                            }

                                            let vcenter = to_screen(via.center[0], via.center[1]);
                                            let r = ((via.radius * zoom) as f32).max(2.0);

                                            window.paint_quad(quad(
                                                Bounds {
                                                    origin: point(
                                                        vcenter.x - px(r),
                                                        vcenter.y - px(r),
                                                    ),
                                                    size: size(px(2.0 * r), px(2.0 * r)),
                                                },
                                                px(r),
                                                rgb(0xE3B72E),
                                                px(0.0),
                                                gpui::transparent_black(),
                                                Default::default(),
                                            ));

                                            let hr = (r * 0.45).max(1.0);
                                            window.paint_quad(quad(
                                                Bounds {
                                                    origin: point(
                                                        vcenter.x - px(hr),
                                                        vcenter.y - px(hr),
                                                    ),
                                                    size: size(px(2.0 * hr), px(2.0 * hr)),
                                                },
                                                px(hr),
                                                rgb(0x001023),
                                                px(0.0),
                                                gpui::transparent_black(),
                                                Default::default(),
                                            ));
                                        }

                                        // 5. Draw Ratsnest Airlines
                                        let rat_col = rgb(0x00E5FF);
                                        let sel_col = rgb(0xFFD700);
                                        for (idx, rat) in model.ratsnest.iter().enumerate() {
                                            let is_selected = selected_inc == Some(idx);
                                            let p1 = to_screen(rat.from[0], rat.from[1]);
                                            let p2 = to_screen(rat.to[0], rat.to[1]);

                                            if is_selected {
                                                // High contrast glowing highlight
                                                let mut pb = PathBuilder::stroke(px(3.5));
                                                pb.move_to(p1);
                                                pb.line_to(p2);
                                                if let Ok(path) = pb.build() {
                                                    window.paint_path(path, sel_col);
                                                }
                                                // Endpoint indicator dots
                                                window.paint_quad(quad(
                                                    Bounds {
                                                        origin: point(
                                                            p1.x - px(5.0),
                                                            p1.y - px(5.0),
                                                        ),
                                                        size: size(px(10.0), px(10.0)),
                                                    },
                                                    px(5.0),
                                                    sel_col,
                                                    px(0.0),
                                                    gpui::transparent_black(),
                                                    Default::default(),
                                                ));
                                                window.paint_quad(quad(
                                                    Bounds {
                                                        origin: point(
                                                            p2.x - px(5.0),
                                                            p2.y - px(5.0),
                                                        ),
                                                        size: size(px(10.0), px(10.0)),
                                                    },
                                                    px(5.0),
                                                    sel_col,
                                                    px(0.0),
                                                    gpui::transparent_black(),
                                                    Default::default(),
                                                ));
                                            } else {
                                                let mut pb = PathBuilder::stroke(px(1.0));
                                                pb = pb.dash_array(&[px(4.0), px(3.0)]);
                                                pb.move_to(p1);
                                                pb.line_to(p2);
                                                if let Ok(path) = pb.build() {
                                                    window.paint_path(path, rat_col);
                                                }
                                            }
                                        }
                                    },
                                )
                                .size_full(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                                    this.is_dragging = true;
                                    this.drag_start_pos = ev.position;
                                    this.drag_start_pan = [this.pan_x, this.pan_y];
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                                if this.is_dragging {
                                    let dx = (ev.position.x - this.drag_start_pos.x).to_f64();
                                    let dy = (ev.position.y - this.drag_start_pos.y).to_f64();
                                    this.pan_x = this.drag_start_pan[0] + dx;
                                    this.pan_y = this.drag_start_pan[1] + dy;
                                }
                                let ox = if this.show_side_panel { 320.0 } else { 0.0 };
                                let oy = 44.0;
                                let vw = 900.0;
                                let vh = 600.0;
                                let canvas_mx = ev.position.x.to_f64() - ox;
                                let canvas_my = ev.position.y.to_f64() - oy;
                                let bx = this.center[0]
                                    + (canvas_mx - vw / 2.0 - this.pan_x) / this.zoom;
                                let by = this.center[1]
                                    - (canvas_my - vh / 2.0 - this.pan_y) / this.zoom;
                                this.mouse_pos_board = Some([bx, by]);
                                cx.notify();
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.is_dragging = false;
                                    cx.notify();
                                }),
                            )
                            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                                let delta = match ev.delta {
                                    gpui::ScrollDelta::Lines(p) => p.y as f64 * 0.1,
                                    gpui::ScrollDelta::Pixels(p) => p.y.to_f64() * 0.005,
                                };
                                let factor = if delta > 0.0 { 1.15 } else { 0.87 };
                                let ox = if this.show_side_panel { 320.0 } else { 0.0 };
                                let oy = 44.0;
                                let vw = 900.0;
                                let vh = 600.0;
                                this.zoom_at(ev.position, factor, ox, oy, vw, vh);
                                cx.notify();
                            })),
                    ),
            )
            // Bottom Status Bar
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(28.0))
                    .px_4()
                    .bg(bg_status)
                    .border_t_1()
                    .border_color(border_col)
                    .text_xs()
                    // Left: Live Board Physical Coordinates (mm)
                    .child(div().flex().gap_3().child(match mouse_coord {
                        Some([bx, by]) => {
                            let mm_x = bx * res_mm;
                            let mm_y = by * res_mm;
                            format!(
                                "X: {:8.3} mm,  Y: {:8.3} mm  (Board: {:>8.0}, {:>8.0})",
                                mm_x, mm_y, bx, by
                            )
                        }
                        None => "X:    0.000 mm,  Y:    0.000 mm".into(),
                    }))
                    // Center: Routing Metrics
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(format!("Pass: {}", stats.pass))
                            .child(format!("Incomplete: {}", stats.incomplete))
                            .child(format!("Violations: {}", stats.violations))
                            .child(format!("Score: {:.2}", stats.score))
                            .child(format!("Elapsed: {:.2}s", stats.elapsed_s))
                            .child(format!("Items: {}", stats.items)),
                    )
                    // Right: Performance & Branding
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(format!("{:.0} FPS", current_fps))
                            .child("freerouting-rs"),
                    ),
            )
    }
}

// ============================================================================
// UI Helper Builders
// ============================================================================

fn make_button(
    text: &str,
    cx: &mut Context<FrGuiApp>,
    on_click: impl Fn(&mut FrGuiApp, &mut Context<FrGuiApp>) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(text.to_string()))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(0x2A2E3D))
        .text_xs()
        .text_color(rgb(0xE2E8F0))
        .hover(|s| s.bg(rgb(0x3B4254)))
        .active(|s| s.bg(rgb(0x1F232E)))
        .child(text.to_string())
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
}

fn make_tab_btn(
    text: &str,
    active: bool,
    cx: &mut Context<FrGuiApp>,
    on_click: impl Fn(&mut FrGuiApp, &mut Context<FrGuiApp>) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(text.to_string()))
        .px_2p5()
        .py_1()
        .rounded_md()
        .bg(if active { rgb(0x3B4254) } else { rgb(0x232733) })
        .text_xs()
        .text_color(if active { rgb(0xFFFFFF) } else { rgb(0x94A3B8) })
        .hover(|s| s.bg(rgb(0x475569)))
        .cursor_pointer()
        .child(text.to_string())
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
}

fn render_params_tab(app: &mut FrGuiApp, cx: &mut Context<FrGuiApp>) -> AnyElement {
    let max_passes = app.bridge.params.max_passes;
    let fanout = app.bridge.params.fanout;
    let det = app.bridge.params.deterministic;
    let opt = app.bridge.params.optimize;
    let t45 = app.bridge.params.tiling45_mode;

    div()
        .flex()
        .flex_col()
        .gap_3()
        .size_full()
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child("Routing Parameters"),
        )
        // Max Passes
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Max Passes (-mp):")
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(make_button("-", cx, |this, cx| {
                            if this.bridge.params.max_passes > 1 {
                                this.bridge.params.max_passes -= 1;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .bg(rgb(0x252834))
                                .rounded_md()
                                .child(format!("{}", max_passes)),
                        )
                        .child(make_button("+", cx, |this, cx| {
                            this.bridge.params.max_passes += 1;
                            cx.notify();
                        })),
                ),
        )
        // Fanout pre-pass
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Fanout Pre-pass:")
                .child(make_button(
                    if fanout { "ON" } else { "OFF" },
                    cx,
                    |this, cx| {
                        this.bridge.params.fanout = !this.bridge.params.fanout;
                        cx.notify();
                    },
                )),
        )
        // Deterministic virtual budget
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Deterministic (--det):")
                .child(make_button(
                    if det { "ON" } else { "OFF" },
                    cx,
                    |this, cx| {
                        this.bridge.params.deterministic = !this.bridge.params.deterministic;
                        cx.notify();
                    },
                )),
        )
        // Optimizer
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Optimizer Stage:")
                .child(make_button(
                    if opt { "ON" } else { "OFF" },
                    cx,
                    |this, cx| {
                        this.bridge.params.optimize = !this.bridge.params.optimize;
                        cx.notify();
                    },
                )),
        )
        // Tiling45 mode
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Tiling45 Mode:")
                .child(make_button(
                    if t45 { "ON" } else { "OFF" },
                    cx,
                    |this, cx| {
                        this.bridge.params.tiling45_mode = !this.bridge.params.tiling45_mode;
                        cx.notify();
                    },
                )),
        )
        .into_any_element()
}

fn render_incompletes_tab(app: &mut FrGuiApp, cx: &mut Context<FrGuiApp>) -> AnyElement {
    let incompletes = app.bridge.incompletes.clone();
    let selected = app.bridge.selected_incomplete;

    div()
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .overflow_hidden()
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("Incompletes ({})", incompletes.len())),
        )
        .child(
            div()
                .id("inc-list-scroll")
                .flex()
                .flex_col()
                .gap_1()
                .size_full()
                .overflow_scroll()
                .children(
                    incompletes
                        .into_iter()
                        .enumerate()
                        .take(100)
                        .map(|(idx, rat)| {
                            let is_sel = selected == Some(idx);
                            let dx = (rat.to[0] - rat.from[0]) * 0.0001;
                            let dy = (rat.to[1] - rat.from[1]) * 0.0001;
                            let len_mm = (dx * dx + dy * dy).sqrt();

                            div()
                                .id(SharedString::from(format!("inc-{}", idx)))
                                .p_2()
                                .rounded_md()
                                .bg(if is_sel { rgb(0x4A3B18) } else { rgb(0x252834) })
                                .hover(|s| s.bg(rgb(0x353A48)))
                                .cursor_pointer()
                                .text_xs()
                                .child(format!("#{idx}: Net {} | {:.2} mm", rat.net, len_mm))
                                .child(div().text_color(rgb(0x94A3B8)).child(format!(
                                    "({:.1}, {:.1}) → ({:.1}, {:.1})",
                                    rat.from[0] * 0.0001,
                                    rat.from[1] * 0.0001,
                                    rat.to[0] * 0.0001,
                                    rat.to[1] * 0.0001
                                )))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.bridge.selected_incomplete = Some(idx);
                                    cx.notify();
                                }))
                        }),
                ),
        )
        .into_any_element()
}

fn render_logs_tab(app: &mut FrGuiApp, _cx: &mut Context<FrGuiApp>) -> AnyElement {
    let logs = app.bridge.logs.clone();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .overflow_hidden()
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child("Routing Engine Logs"),
        )
        .child(
            div()
                .id("logs-list-scroll")
                .flex()
                .flex_col()
                .gap_1()
                .size_full()
                .overflow_scroll()
                .children(logs.into_iter().rev().take(100).map(|msg| {
                    div()
                        .p_1p5()
                        .bg(if msg.is_warning {
                            rgb(0x3B2D18)
                        } else {
                            rgb(0x1B1E28)
                        })
                        .rounded_sm()
                        .text_xs()
                        .text_color(if msg.is_warning {
                            rgb(0xF6AD55)
                        } else {
                            rgb(0xCBD5E1)
                        })
                        .child(format!("[{:.2}s] {}", msg.timestamp_s, msg.message))
                })),
        )
        .into_any_element()
}

fn render_layers_tab(app: &mut FrGuiApp, cx: &mut Context<FrGuiApp>) -> AnyElement {
    let layers = app.bridge.current_model.layers.clone();
    let active = app.active_layer;

    div()
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .overflow_hidden()
        // Header with All / None buttons
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .p_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_sm()
                        .child(format!("Layers ({})", layers.len())),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(make_button("All", cx, |this, cx| {
                            for v in &mut this.layer_visible {
                                *v = true;
                            }
                            cx.notify();
                        }))
                        .child(make_button("None", cx, |this, cx| {
                            for v in &mut this.layer_visible {
                                *v = false;
                            }
                            if this.active_layer < this.layer_visible.len() {
                                this.layer_visible[this.active_layer] = true;
                            }
                            cx.notify();
                        })),
                ),
        )
        // Scrollable layer list
        .child(
            div()
                .id("layers-list-scroll")
                .flex()
                .flex_col()
                .gap_1()
                .size_full()
                .overflow_scroll()
                .children(layers.iter().enumerate().map(|(idx, layer)| {
                    let is_vis = app.layer_visible.get(idx).copied().unwrap_or(true);
                    let is_act = active == idx;
                    let c = layer.color;
                    let swatch_col = rgb(((c[0] * 255.0) as u32) << 16
                        | ((c[1] * 255.0) as u32) << 8
                        | ((c[2] * 255.0) as u32));

                    div()
                        .id(SharedString::from(format!("layer-{}", idx)))
                        .flex()
                        .items_center()
                        .justify_between()
                        .p_2()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(if is_act { rgb(0x2D3748) } else { rgb(0x222530) })
                        .hover(|s| s.bg(if is_act { rgb(0x3A475C) } else { rgb(0x282C37) }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_layer = idx;
                            if idx < this.layer_visible.len() {
                                this.layer_visible[idx] = true;
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                // Checkbox for visibility
                                .child(
                                    div()
                                        .w(px(14.0))
                                        .h(px(14.0))
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(rgb(0x4A5568))
                                        .bg(if is_vis { rgb(0x4A90E2) } else { rgb(0x2D3139) })
                                        .cursor_pointer()
                                        .id(SharedString::from(format!("layer-cb-{}", idx)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if idx < this.layer_visible.len() {
                                                this.layer_visible[idx] = !this.layer_visible[idx];
                                                cx.notify();
                                            }
                                        })),
                                )
                                // Swatch
                                .child(div().w(px(12.0)).h(px(12.0)).rounded_sm().bg(swatch_col))
                                // Layer name
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if is_vis {
                                            rgb(0xE2E8F0)
                                        } else {
                                            rgb(0x718096)
                                        })
                                        .child(layer.name.clone()),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(if layer.is_signal {
                                            rgb(0x234E52)
                                        } else {
                                            rgb(0x4A5568)
                                        })
                                        .text_xs()
                                        .child(if layer.is_signal { "Sig" } else { "Pln" }),
                                )
                                .when(is_act, |s| {
                                    s.child(div().size_2().rounded_full().bg(rgb(0x48BB78)))
                                }),
                        )
                })),
        )
        .into_any_element()
}

// ============================================================================
// Application Entrypoint
// ============================================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dsn_to_open: Option<PathBuf> = None;
    let mut autostart = false;
    let mut screenshot_out: Option<PathBuf> = None;
    let mut zoom_factor: Option<f64> = None;
    let mut active_layer_cli: usize = 0;
    let mut screen_w: u32 = 960;
    let mut screen_h: u32 = 600;
    let mut fps_bench = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--open" | "-de" | "-open" => {
                if i + 1 < args.len() {
                    dsn_to_open = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "--autostart" => {
                autostart = true;
            }
            "--screenshot" => {
                if i + 1 < args.len() {
                    screenshot_out = Some(PathBuf::from(&args[i + 1]));
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
            "--width" => {
                if i + 1 < args.len() {
                    screen_w = args[i + 1].parse().unwrap_or(960);
                    i += 1;
                }
            }
            "--height" => {
                if i + 1 < args.len() {
                    screen_h = args[i + 1].parse().unwrap_or(600);
                    i += 1;
                }
            }
            "--fps-bench" => {
                fps_bench = true;
            }
            _ => {
                if !args[i].starts_with('-') && dsn_to_open.is_none() {
                    dsn_to_open = Some(PathBuf::from(&args[i]));
                }
            }
        }
        i += 1;
    }

    // Benchmark mode
    if fps_bench {
        let model = match dsn_to_open.as_ref() {
            Some(path) => match mock::load(path) {
                Ok(m) => m,
                Err(_) => mock::sample_board(),
            },
            None => {
                let default_fix = "bench/fixtures-oos/cla45-compute-hdi6cs-stage2.dsn";
                if Path::new(default_fix).exists() {
                    mock::load(default_fix).unwrap_or_else(|_| mock::sample_board())
                } else {
                    mock::sample_board()
                }
            }
        };

        println!("=== Running fr-gui FPS benchmark ===");
        println!("Total items: {}", model.stats.items);
        println!("Layers: {}", model.layers.len());
        let layers_vis = vec![true; model.layers.len()];
        let start = Instant::now();
        let frames = 100;
        for _ in 0..frames {
            let _ = export::render_gui_screenshot(
                "fps-bench",
                &model,
                &layers_vis,
                0,
                960,
                600,
                None,
                None,
                "/tmp/bench_frame.png",
            );
        }
        let elapsed = start.elapsed().as_secs_f64();
        let fps = (frames as f64) / elapsed;
        println!(
            "Rendered {} frames in {:.3} s: {:.1} FPS (software rasterizer)",
            frames, elapsed, fps
        );
        println!("Note: GPUI Metal hardware-accelerated pipeline runs at 60-120 FPS vsync.");
        return;
    }

    // Headless screenshot export mode (for verification and CI parity snapshots)
    if let Some(out_path) = screenshot_out {
        let model = match dsn_to_open.as_ref() {
            Some(dsn) => match fr_core::RoutingJob::from_path(dsn) {
                Ok(job) => match job.import() {
                    Ok(design) => {
                        let initial_stats = fr_core::snapshot::Stats {
                            items: design.board.store().len() as u32,
                            ..Default::default()
                        };
                        fr_core::snapshot::snapshot(&design.board, initial_stats)
                    }
                    Err(_) => mock::load(dsn).unwrap_or_else(|_| mock::sample_board()),
                },
                Err(_) => mock::load(dsn).unwrap_or_else(|_| mock::sample_board()),
            },
            None => mock::sample_board(),
        };

        let layer_vis = vec![true; model.layers.len()];
        let title = dsn_to_open
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("sample_board");
        export::render_gui_screenshot(
            title,
            &model,
            &layer_vis,
            active_layer_cli,
            screen_w,
            screen_h,
            zoom_factor,
            None,
            &out_path,
        )
        .expect("Failed to export screenshot");
        println!("Screenshot written successfully: {}", out_path.display());
        return;
    }

    // Start interactive GPUI application
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let dsn_path_opt = dsn_to_open.clone();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Fast-Routing GUI".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| {
                    let mut app = FrGuiApp::new(cx);
                    if let Some(path) = &dsn_path_opt {
                        if let Err(e) = app.load_dsn(path) {
                            eprintln!("Failed to load DSN {}: {}", path.display(), e);
                        } else if autostart {
                            let _ = app.bridge.start();
                        }
                    }
                    app
                });

                // Periodic polling timer in background to sync bridge events into GPUI view
                window
                    .spawn(cx, {
                        let view_handle = view.clone();
                        async move |cx| loop {
                            Timer::after(std::time::Duration::from_millis(30)).await;
                            let res = cx.update(|_, cx| {
                                view_handle.update(cx, |this, cx| {
                                    if this.bridge.poll_events() {
                                        cx.notify();
                                    }
                                });
                            });
                            if res.is_err() {
                                break;
                            }
                        }
                    })
                    .detach();

                view
            },
        )
        .expect("Failed to open GPUI window");
    });
}
