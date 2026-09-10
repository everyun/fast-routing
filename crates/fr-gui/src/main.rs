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
    canvas, div, point, px, quad, rgb, size, AnyElement, App, Application, Bounds, Context,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, Timer, Window, WindowBounds, WindowOptions,
};

mod bridge;
mod export;

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

    // Viewport coordinates
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
        Self {
            bridge: JobBridge::new(),
            active_tab: SideTab::Params,
            layer_visible: Vec::new(),
            active_layer: 0,
            show_side_panel: true,
            zoom: 0.05,
            pan_x: 0.0,
            pan_y: 0.0,
            center: [0.0, 0.0],
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
        self.bridge.open_dsn(path)?;
        let layer_count = self.bridge.current_model.layers.len();
        self.layer_visible = vec![true; layer_count];
        self.active_layer = 0;
        self.fit_view(900.0, 600.0);
        Ok(())
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

        let model = Arc::clone(&self.bridge.current_model);
        if self.layer_visible.len() != model.layers.len() {
            self.layer_visible = vec![true; model.layers.len()];
        }

        let layer_visible = self.layer_visible.clone();
        let active_tab = self.active_tab;
        let zoom = self.zoom;
        let center = self.center;
        let pan_x = self.pan_x;
        let pan_y = self.pan_y;
        let selected_inc = self.bridge.selected_incomplete;

        let dsn_title = if self.bridge.current_design_name.is_empty() {
            "No DSN loaded".to_string()
        } else {
            self.bridge.current_design_name.clone()
        };

        let job_state = self.bridge.state;
        let stats = self.bridge.stats;
        let show_side = self.show_side_panel;
        let incomplete_count = self.bridge.incompletes.len();
        let log_count = self.bridge.logs.len();
        let layer_count = model.layers.len();
        let current_fps = self.fps;

        // Theme colors
        let bg_app = rgb(0x181820);
        let bg_toolbar = rgb(0x222530);
        let bg_panel = rgb(0x1E202A);
        let bg_status = rgb(0x14161C);
        let border_col = rgb(0x323644);
        let text_light = rgb(0xE2E4EB);
        let text_dim = rgb(0x8A8E9E);

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg_app)
            .text_color(text_light)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.to_lowercase();
                match key.as_str() {
                    "f" => {
                        this.fit_view(900.0, 600.0);
                        cx.notify();
                    }
                    "+" | "=" => {
                        this.zoom *= 1.25;
                        cx.notify();
                    }
                    "-" | "_" => {
                        this.zoom /= 1.25;
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
                    .h(px(46.0))
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
                                &format!("Incompletes ({})", incomplete_count),
                                active_tab == SideTab::Incompletes,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Incompletes;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(make_tab_btn(
                                &format!("Logs ({})", log_count),
                                active_tab == SideTab::Logs,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Logs;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            ))
                            .child(make_tab_btn(
                                &format!("Layers ({})", layer_count),
                                active_tab == SideTab::Layers,
                                cx,
                                |this, cx| {
                                    this.active_tab = SideTab::Layers;
                                    this.show_side_panel = true;
                                    cx.notify();
                                },
                            )),
                    ),
            )
            // Main Body: Side Panel + Interactive Canvas
            .child(
                div()
                    .flex()
                    .flex_1()
                    .size_full()
                    .overflow_hidden()
                    // Optional Side Drawer Panel
                    .when(show_side, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .w(px(320.0))
                                .h_full()
                                .bg(bg_panel)
                                .border_r_1()
                                .border_color(border_col)
                                .p_3()
                                .child(match active_tab {
                                    SideTab::Params => render_params_tab(self, cx),
                                    SideTab::Incompletes => render_incompletes_tab(self, cx),
                                    SideTab::Logs => render_logs_tab(self),
                                    SideTab::Layers => render_layers_tab(self, cx),
                                }),
                        )
                    })
                    // Canvas Viewport
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .relative()
                            .bg(rgb(0x040814))
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
                                            let sy = cy_px - (by - center[1]) * zoom;
                                            point(px(sx as f32), px(sy as f32))
                                        };

                                        // 1. Draw Areas (Planes, Outlines, Keepouts)
                                        for area in &model.areas {
                                            let is_vis = layer_visible
                                                .get(area.layer as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            if !is_vis || area.polygon.len() < 3 {
                                                continue;
                                            }
                                            let (color, is_stroke) = match area.kind {
                                                AreaKind::Outline => (rgb(0xE2E8F0), true),
                                                AreaKind::Keepout => (rgb(0xE53E3E), false),
                                                AreaKind::Plane => (rgb(0x2D3748), false),
                                                AreaKind::Other => (rgb(0x4A5568), false),
                                            };

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
                                                    window.paint_path(path, color);
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
                                                    window.paint_path(path, color);
                                                }
                                            }
                                        }

                                        // 2. Draw Component Pads / Pins
                                        for pad in &model.pads {
                                            let is_vis = layer_visible
                                                .get(pad.layer as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            if !is_vis || pad.polygon.len() < 3 {
                                                continue;
                                            }
                                            let pad_col = if pad.is_smd {
                                                rgb(0xED8936)
                                            } else {
                                                rgb(0x48BB78)
                                            };

                                            let mut pb = PathBuilder::fill();
                                            let p0 =
                                                to_screen(pad.polygon[0][0], pad.polygon[0][1]);
                                            pb.move_to(p0);
                                            for pt in &pad.polygon[1..] {
                                                pb.line_to(to_screen(pt[0], pt[1]));
                                            }
                                            pb.close();
                                            if let Ok(path) = pb.build() {
                                                window.paint_path(path, pad_col);
                                            }
                                        }

                                        // 3. Draw Routed Traces
                                        for trace in &model.traces {
                                            let is_vis = layer_visible
                                                .get(trace.layer as usize)
                                                .copied()
                                                .unwrap_or(true);
                                            if !is_vis || trace.points.len() < 2 {
                                                continue;
                                            }
                                            let c = model
                                                .layers
                                                .get(trace.layer as usize)
                                                .map(|l| l.color)
                                                .unwrap_or([0.8, 0.2, 0.2, 1.0]);
                                            let trace_col = rgb(
                                                ((c[0] * 255.0) as u32) << 16
                                                    | ((c[1] * 255.0) as u32) << 8
                                                    | ((c[2] * 255.0) as u32),
                                            );

                                            let stroke_w =
                                                ((trace.half_width * 2.0 * zoom) as f32).max(1.0);
                                            let mut pb = PathBuilder::stroke(px(stroke_w));
                                            let p0 =
                                                to_screen(trace.points[0][0], trace.points[0][1]);
                                            pb.move_to(p0);
                                            for pt in &trace.points[1..] {
                                                pb.line_to(to_screen(pt[0], pt[1]));
                                            }
                                            if let Ok(path) = pb.build() {
                                                window.paint_path(path, trace_col);
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
                                                rgb(0xE2B714),
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
                                                rgb(0x040814),
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
                                                        origin: point(p1.x - px(5.0), p1.y - px(5.0)),
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
                                                        origin: point(p2.x - px(5.0), p2.y - px(5.0)),
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
                                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                                    this.is_dragging = true;
                                    this.drag_start_pos = ev.position;
                                    this.drag_start_pan = [this.pan_x, this.pan_y];
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                                if this.is_dragging {
                                    let dx = (ev.position.x - this.drag_start_pos.x).to_f64();
                                    let dy = (ev.position.y - this.drag_start_pos.y).to_f64();
                                    this.pan_x = this.drag_start_pan[0] + dx;
                                    this.pan_y = this.drag_start_pan[1] + dy;
                                    cx.notify();
                                }
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| {
                                    this.is_dragging = false;
                                }),
                            )
                            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                                let delta = match ev.delta {
                                    gpui::ScrollDelta::Lines(p) => p.y as f64 * 0.1,
                                    gpui::ScrollDelta::Pixels(p) => p.y.to_f64() * 0.005,
                                };
                                let factor = if delta > 0.0 { 1.15 } else { 0.87 };
                                this.zoom = (this.zoom * factor).clamp(1e-12, 100.0);
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

// Helper UI builders for buttons, tabs, and panels

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
    is_active: bool,
    cx: &mut Context<FrGuiApp>,
    on_click: impl Fn(&mut FrGuiApp, &mut Context<FrGuiApp>) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("tab-{}", text)))
        .px_2p5()
        .py_1()
        .rounded_md()
        .bg(if is_active {
            rgb(0x3B82F6)
        } else {
            rgb(0x222530)
        })
        .text_xs()
        .text_color(if is_active {
            rgb(0xFFFFFF)
        } else {
            rgb(0x9CA3AF)
        })
        .hover(|s| s.bg(rgb(0x2D3748)))
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
                .child("Router Parameters"),
        )
        // Max passes
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
                            this.bridge.params.max_passes =
                                (this.bridge.params.max_passes - 1).max(1);
                            cx.notify();
                        }))
                        .child(div().px_2().child(format!("{max_passes}")))
                        .child(make_button("+", cx, |this, cx| {
                            this.bridge.params.max_passes += 1;
                            cx.notify();
                        })),
                ),
        )
        // Fanout toggle
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Fanout Pre-pass:")
                .child(make_button(if fanout { "ON" } else { "OFF" }, cx, |this, cx| {
                    this.bridge.params.fanout = !this.bridge.params.fanout;
                    cx.notify();
                })),
        )
        // Deterministic virtual budget
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Deterministic (--det):")
                .child(make_button(if det { "ON" } else { "OFF" }, cx, |this, cx| {
                    this.bridge.params.deterministic = !this.bridge.params.deterministic;
                    cx.notify();
                })),
        )
        // Optimizer
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Optimizer Stage:")
                .child(make_button(if opt { "ON" } else { "OFF" }, cx, |this, cx| {
                    this.bridge.params.optimize = !this.bridge.params.optimize;
                    cx.notify();
                })),
        )
        // Tiling45 mode
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Tiling45 Mode:")
                .child(make_button(if t45 { "ON" } else { "OFF" }, cx, |this, cx| {
                    this.bridge.params.tiling45_mode = !this.bridge.params.tiling45_mode;
                    cx.notify();
                })),
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
                .children(incompletes.into_iter().enumerate().take(100).map(|(idx, rat)| {
                    let is_sel = selected == Some(idx);
                    let dx = (rat.to[0] - rat.from[0]) * 0.0001;
                    let dy = (rat.to[1] - rat.from[1]) * 0.0001;
                    let len_mm = (dx * dx + dy * dy).sqrt();

                    div()
                        .id(SharedString::from(format!("inc-{}", idx)))
                        .p_2()
                        .rounded_md()
                        .bg(if is_sel {
                            rgb(0x4A3B18)
                        } else {
                            rgb(0x252834)
                        })
                        .hover(|s| s.bg(rgb(0x353A48)))
                        .cursor_pointer()
                        .text_xs()
                        .child(format!("#{idx}: Net {} | {:.2} mm", rat.net, len_mm))
                        .child(
                            div().text_color(rgb(0x94A3B8)).child(format!(
                                "({:.1}, {:.1}) → ({:.1}, {:.1})",
                                rat.from[0] * 0.0001,
                                rat.from[1] * 0.0001,
                                rat.to[0] * 0.0001,
                                rat.to[1] * 0.0001
                            )),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.bridge.selected_incomplete = Some(idx);
                            cx.notify();
                        }))
                })),
        )
        .into_any_element()
}

fn render_logs_tab(app: &FrGuiApp) -> AnyElement {
    let logs = app.bridge.logs.clone();
    div()
        .id("logs-list-scroll")
        .flex()
        .flex_col()
        .gap_1()
        .size_full()
        .overflow_scroll()
        .children(logs.iter().rev().take(100).map(|entry| {
            div()
                .text_xs()
                .text_color(if entry.is_warning {
                    rgb(0xFC8181)
                } else {
                    rgb(0xCBD5E1)
                })
                .child(format!("[{:.2}s] {}", entry.timestamp_s, entry.message))
        }))
        .into_any_element()
}

fn render_layers_tab(app: &mut FrGuiApp, cx: &mut Context<FrGuiApp>) -> AnyElement {
    let layers = app.bridge.current_model.layers.clone();
    let active = app.active_layer;

    div()
        .id("layers-list-scroll")
        .flex()
        .flex_col()
        .gap_1p5()
        .size_full()
        .overflow_scroll()
        .children(layers.iter().enumerate().map(|(idx, layer)| {
            let is_vis = app.layer_visible.get(idx).copied().unwrap_or(true);
            let is_act = active == idx;
            let c = layer.color;
            let swatch_col = rgb(
                ((c[0] * 255.0) as u32) << 16
                    | ((c[1] * 255.0) as u32) << 8
                    | ((c[2] * 255.0) as u32),
            );

            div()
                .id(SharedString::from(format!("layer-{}", idx)))
                .flex()
                .items_center()
                .justify_between()
                .p_1p5()
                .rounded_md()
                .bg(if is_act {
                    rgb(0x2D3748)
                } else {
                    rgb(0x222530)
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(12.0))
                                .h(px(12.0))
                                .rounded_sm()
                                .bg(swatch_col),
                        )
                        .child(div().text_xs().child(layer.name.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(make_button(
                            if is_vis { "👁" } else { "✕" },
                            cx,
                            move |this, cx| {
                                if idx < this.layer_visible.len() {
                                    this.layer_visible[idx] = !this.layer_visible[idx];
                                }
                                cx.notify();
                            },
                        ))
                        .child(make_button("Act", cx, move |this, cx| {
                            this.active_layer = idx;
                            cx.notify();
                        })),
                )
        }))
        .into_any_element()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dsn_to_open: Option<PathBuf> = None;
    let mut autostart = false;
    let mut screenshot_out: Option<PathBuf> = None;

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
            _ => {
                if !args[i].starts_with('-') && dsn_to_open.is_none() {
                    dsn_to_open = Some(PathBuf::from(&args[i]));
                }
            }
        }
        i += 1;
    }

    // Headless screenshot export mode (for verification and CI parity snapshots)
    if let Some(out_path) = screenshot_out {
        if let Some(dsn) = &dsn_to_open {
            println!(
                "Exporting screenshot for {} -> {}",
                dsn.display(),
                out_path.display()
            );
            let job =
                fr_core::RoutingJob::from_path(dsn).expect("Failed to load DSN for screenshot");
            let design = job.import().expect("Import failed");
            let initial_stats = fr_core::snapshot::Stats {
                items: design.board.store().len() as u32,
                ..Default::default()
            };
            let model = fr_core::snapshot::snapshot(&design.board, initial_stats);
            let layer_vis = vec![true; model.layers.len()];
            export::render_gui_screenshot(
                &model, &layer_vis, 0, 1280, 800, None, None, &out_path,
            )
            .expect("Failed to export screenshot");
            println!("Screenshot written successfully: {}", out_path.display());
            return;
        }
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
