//! Software renderer for exporting PNG screenshots of the board and GUI shell.
//!
//! Enables automated screenshot generation and headless validation without
//! requiring macOS Screen Recording permissions.

use std::path::Path;

use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform,
};

use crate::model::*;

/// Minimal 5x7 bitmap font for rendering UI labels in exported screenshots.
fn get_glyph_bitmap(ch: char) -> [u8; 5] {
    match ch {
        'A' | 'a' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        'B' | 'b' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        'C' | 'c' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        'D' | 'd' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        'E' | 'e' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        'F' | 'f' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        'G' | 'g' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        'H' | 'h' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'I' | 'i' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        'J' | 'j' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        'K' | 'k' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        'L' | 'l' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        'M' | 'm' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        'N' | 'n' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        'O' | 'o' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        'P' | 'p' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        'Q' | 'q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        'R' | 'r' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        'S' | 's' => [0x46, 0x49, 0x49, 0x49, 0x31],
        'T' | 't' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        'U' | 'u' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        'V' | 'v' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        'W' | 'w' => [0x7F, 0x20, 0x18, 0x20, 0x7F],
        'X' | 'x' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' | 'y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        'Z' | 'z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        '1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        '3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        '9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        '.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        ',' => [0x00, 0x80, 0x60, 0x00, 0x00],
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        ';' => [0x00, 0x56, 0x36, 0x00, 0x00],
        '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '+' => [0x08, 0x08, 0x3E, 0x08, 0x08],
        '=' => [0x14, 0x14, 0x14, 0x14, 0x14],
        '/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        '(' => [0x00, 0x1C, 0x22, 0x41, 0x00],
        ')' => [0x00, 0x41, 0x22, 0x1C, 0x00],
        '[' => [0x00, 0x7F, 0x41, 0x41, 0x00],
        ']' => [0x00, 0x41, 0x41, 0x7F, 0x00],
        '|' => [0x00, 0x00, 0x7F, 0x00, 0x00],
        '<' => [0x08, 0x14, 0x22, 0x41, 0x00],
        '>' => [0x00, 0x41, 0x22, 0x14, 0x08],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

fn draw_text(pixmap: &mut Pixmap, x: f32, y: f32, text: &str, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);

    let mut cur_x = x;
    for ch in text.chars() {
        let bitmap = get_glyph_bitmap(ch);
        for (col_idx, &col) in bitmap.iter().enumerate() {
            for row_idx in 0..7 {
                if (col & (1 << row_idx)) != 0 {
                    let px = cur_x + (col_idx as f32);
                    let py = y + (row_idx as f32);
                    if px >= 0.0 && px < (pixmap.width() as f32) && py >= 0.0 && py < (pixmap.height() as f32) {
                        if let Some(rect) = Rect::from_xywh(px, py, 1.0, 1.0) {
                            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                        }
                    }
                }
            }
        }
        cur_x += 7.0; // 5 width + 2 spacing
    }
}

/// Renders a full GUI shell screenshot (toolbar, layer panel, canvas, status bar).
pub fn render_gui_screenshot(
    model: &RenderModel,
    filename: &str,
    layer_visible: &[bool],
    active_layer: usize,
    width: u32,
    height: u32,
    custom_zoom_factor: Option<f64>,
    output_path: impl AsRef<Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| "Failed to allocate pixmap for screenshot".to_string())?;

    // Window background (dark theme #1E1E24)
    pixmap.fill(Color::from_rgba8(0x18, 0x18, 0x20, 0xFF));

    let toolbar_h = 44.0;
    let statusbar_h = 28.0;
    let layer_panel_w = 220.0;

    let canvas_x = layer_panel_w;
    let canvas_y = toolbar_h;
    let canvas_w = (width as f32) - layer_panel_w;
    let canvas_h = (height as f32) - toolbar_h - statusbar_h;

    // Allocate an isolated canvas pixmap for 100% strict clipping
    let mut canvas_pixmap = Pixmap::new(canvas_w as u32, canvas_h as u32)
        .ok_or_else(|| "Failed to allocate canvas pixmap".to_string())?;
    canvas_pixmap.fill(Color::from_rgba8(0x00, 0x10, 0x23, 0xFF)); // Freerouting default #001023

    // Compute fit zoom & center for the canvas
    let bbox = model.bbox;
    let bw = (bbox[2] - bbox[0]).abs().max(1.0);
    let bh = (bbox[3] - bbox[1]).abs().max(1.0);
    let margin = 0.90;
    let zx = ((canvas_w as f64) * margin) / bw;
    let zy = ((canvas_h as f64) * margin) / bh;
    let mut zoom = zx.min(zy);
    let mut center = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];

    // If custom zoom factor requested (e.g. for detail/zoom screenshot)
    if let Some(factor) = custom_zoom_factor {
        zoom *= factor;
        center[0] += 50_000.0;
        center[1] -= 50_000.0;
    }

    let cx = (canvas_w as f64) / 2.0;
    let cy = (canvas_h as f64) / 2.0;

    let to_canvas_screen = |bx: f64, by: f64| -> (f32, f32) {
        let sx = cx + (bx - center[0]) * zoom;
        let sy = cy - (by - center[1]) * zoom; // Y flip
        (sx as f32, sy as f32)
    };

    // 1. Draw Canvas Elements on canvas_pixmap
    // Outlines & Areas
    let mut outline_paint = Paint::default();
    outline_paint.set_color_rgba8(0x64, 0x96, 0xFF, 0xFF);
    let mut outline_stroke = Stroke::default();
    outline_stroke.width = 1.5;

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

        let mut pb = PathBuilder::new();
        let (p0x, p0y) = to_canvas_screen(area.polygon[0][0], area.polygon[0][1]);
        pb.move_to(p0x, p0y);
        for pt in &area.polygon[1..] {
            let (px, py) = to_canvas_screen(pt[0], pt[1]);
            pb.line_to(px, py);
        }
        pb.close();

        if let Some(path) = pb.finish() {
            match area.kind {
                AreaKind::Outline => {
                    canvas_pixmap.stroke_path(&path, &outline_paint, &outline_stroke, Transform::identity(), None);
                }
                AreaKind::Keepout => {
                    let mut kp_paint = Paint::default();
                    kp_paint.set_color_rgba8(0x1A, 0xC4, 0xD2, 0x40);
                    canvas_pixmap.fill_path(&path, &kp_paint, FillRule::Winding, Transform::identity(), None);
                }
                AreaKind::Plane => {
                    let mut pl_paint = Paint::default();
                    let c = model.layers.get(area.layer as usize).map(|l| l.color).unwrap_or([0.3, 0.3, 0.3, 0.3]);
                    pl_paint.set_color_rgba8(
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8,
                        0x30,
                    );
                    canvas_pixmap.fill_path(&path, &pl_paint, FillRule::Winding, Transform::identity(), None);
                }
                AreaKind::Other => {}
            }
        }
    }

    // Pads
    for pad in &model.pads {
        let is_vis = layer_visible.get(pad.layer as usize).copied().unwrap_or(true);
        if !is_vis || pad.polygon.len() < 3 {
            continue;
        }
        let is_act = pad.layer as usize == active_layer;
        let mut pad_paint = Paint::default();
        if is_act {
            pad_paint.set_color_rgba8(0xE3, 0xB7, 0x2E, 0xFF); // Gold
        } else {
            pad_paint.set_color_rgba8(0xD0, 0xAA, 0x30, 0xCC);
        }

        let mut pb = PathBuilder::new();
        let (p0x, p0y) = to_canvas_screen(pad.polygon[0][0], pad.polygon[0][1]);
        pb.move_to(p0x, p0y);
        for pt in &pad.polygon[1..] {
            let (px, py) = to_canvas_screen(pt[0], pt[1]);
            pb.line_to(px, py);
        }
        pb.close();

        if let Some(path) = pb.finish() {
            canvas_pixmap.fill_path(&path, &pad_paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    // Traces
    for trace in &model.traces {
        let is_vis = layer_visible.get(trace.layer as usize).copied().unwrap_or(true);
        if !is_vis || trace.points.len() < 2 {
            continue;
        }
        let is_act = trace.layer as usize == active_layer;
        let c = model
            .layers
            .get(trace.layer as usize)
            .map(|l| l.color)
            .unwrap_or([0.8, 0.2, 0.2, 1.0]);

        let mut trace_paint = Paint::default();
        let alpha = if is_act { 0xFF } else { 0xA0 };
        trace_paint.set_color_rgba8(
            (c[0] * 255.0) as u8,
            (c[1] * 255.0) as u8,
            (c[2] * 255.0) as u8,
            alpha,
        );

        let stroke_w = ((trace.half_width * 2.0 * zoom) as f32).max(1.0);
        let mut stroke = Stroke::default();
        stroke.width = stroke_w;

        let mut pb = PathBuilder::new();
        let (p0x, p0y) = to_canvas_screen(trace.points[0][0], trace.points[0][1]);
        pb.move_to(p0x, p0y);
        for pt in &trace.points[1..] {
            let (px, py) = to_canvas_screen(pt[0], pt[1]);
            pb.line_to(px, py);
        }

        if let Some(path) = pb.finish() {
            canvas_pixmap.stroke_path(&path, &trace_paint, &stroke, Transform::identity(), None);
        }
    }

    // Vias
    let mut via_paint = Paint::default();
    via_paint.set_color_rgba8(0xE3, 0xB7, 0x2E, 0xFF);
    let mut via_hole_paint = Paint::default();
    via_hole_paint.set_color_rgba8(0x00, 0x10, 0x23, 0xFF);

    for via in &model.vias {
        let from_vis = layer_visible.get(via.layer_from as usize).copied().unwrap_or(true);
        let to_vis = layer_visible.get(via.layer_to as usize).copied().unwrap_or(true);
        if !from_vis && !to_vis {
            continue;
        }

        let (vx, vy) = to_canvas_screen(via.center[0], via.center[1]);
        let r = ((via.radius * zoom) as f32).max(1.8);
        if vx + r < 0.0 || vx - r > canvas_w || vy + r < 0.0 || vy - r > canvas_h {
            continue;
        }

        let mut pb = PathBuilder::new();
        pb.push_circle(vx, vy, r);
        if let Some(path) = pb.finish() {
            canvas_pixmap.fill_path(&path, &via_paint, FillRule::Winding, Transform::identity(), None);
        }

        let hr = (r * 0.5).max(0.8);
        let mut hpb = PathBuilder::new();
        hpb.push_circle(vx, vy, hr);
        if let Some(path) = hpb.finish() {
            canvas_pixmap.fill_path(&path, &via_hole_paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    // Ratsnest
    let mut rat_paint = Paint::default();
    rat_paint.set_color_rgba8(0xFF, 0xFF, 0xFF, 0xC0);
    let mut rat_stroke = Stroke::default();
    rat_stroke.width = 1.0;

    for rat in &model.ratsnest {
        let from_vis = layer_visible.get(rat.layer_from as usize).copied().unwrap_or(true);
        let to_vis = layer_visible.get(rat.layer_to as usize).copied().unwrap_or(true);
        if !from_vis && !to_vis {
            continue;
        }

        let (f_x, f_y) = to_canvas_screen(rat.from[0], rat.from[1]);
        let (t_x, t_y) = to_canvas_screen(rat.to[0], rat.to[1]);
        let mut pb = PathBuilder::new();
        pb.move_to(f_x, f_y);
        pb.line_to(t_x, t_y);
        if let Some(path) = pb.finish() {
            canvas_pixmap.stroke_path(&path, &rat_paint, &rat_stroke, Transform::identity(), None);
        }
    }

    // Blit canvas onto main pixmap
    pixmap.draw_pixmap(
        canvas_x as i32,
        canvas_y as i32,
        canvas_pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );

    // 2. Toolbar at the top
    let mut bar_paint = Paint::default();
    bar_paint.set_color_rgba8(0x24, 0x27, 0x32, 0xFF);
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, width as f32, toolbar_h) {
        pixmap.fill_rect(rect, &bar_paint, Transform::identity(), None);
    }
    let mut border_paint = Paint::default();
    border_paint.set_color_rgba8(0x35, 0x3A, 0x48, 0xFF);
    if let Some(rect) = Rect::from_xywh(0.0, toolbar_h - 1.0, width as f32, 1.0) {
        pixmap.fill_rect(rect, &border_paint, Transform::identity(), None);
    }

    let buttons = [
        ("Open DSN", 0x3A, 0x40, 0x50),
        ("Save SES", 0x3A, 0x40, 0x50),
        ("Fit (F)", 0x2E, 0x5E, 0xAA),
        ("Zoom +", 0x3A, 0x40, 0x50),
        ("Zoom -", 0x3A, 0x40, 0x50),
        ("Reset", 0x3A, 0x40, 0x50),
        ("Route", 0x2A, 0x7E, 0x43),
        ("Pause", 0x7B, 0x68, 0x1E),
        ("Stop", 0x78, 0x28, 0x28),
    ];
    let mut btn_x = 12.0;
    let text_white = Color::from_rgba8(0xE0, 0xE0, 0xE0, 0xFF);
    for (name, r, g, b) in buttons {
        let btn_w = (name.len() as f32) * 7.5 + 16.0;
        let mut btn_paint = Paint::default();
        btn_paint.set_color_rgba8(r, g, b, 0xFF);
        if let Some(rect) = Rect::from_xywh(btn_x, 8.0, btn_w, 26.0) {
            pixmap.fill_rect(rect, &btn_paint, Transform::identity(), None);
        }
        draw_text(&mut pixmap, btn_x + 8.0, 18.0, name, text_white);
        btn_x += btn_w + 8.0;
    }

    let info_text = format!("{} ({} layers, {} items)", filename, model.layers.len(), model.stats.items);
    draw_text(&mut pixmap, (width as f32) - 420.0, 18.0, &info_text, Color::from_rgba8(0x90, 0x94, 0xA0, 0xFF));

    // 3. Layer panel on the left
    let mut panel_paint = Paint::default();
    panel_paint.set_color_rgba8(0x1E, 0x20, 0x28, 0xFF);
    if let Some(rect) = Rect::from_xywh(0.0, toolbar_h, layer_panel_w, canvas_h) {
        pixmap.fill_rect(rect, &panel_paint, Transform::identity(), None);
    }
    if let Some(rect) = Rect::from_xywh(layer_panel_w - 1.0, toolbar_h, 1.0, canvas_h) {
        pixmap.fill_rect(rect, &border_paint, Transform::identity(), None);
    }

    draw_text(&mut pixmap, 12.0, toolbar_h + 12.0, &format!("Layers ({})", model.layers.len()), text_white);
    draw_text(&mut pixmap, 130.0, toolbar_h + 12.0, "[All]", Color::from_rgba8(0x4A, 0x90, 0xE2, 0xFF));
    draw_text(&mut pixmap, 175.0, toolbar_h + 12.0, "[None]", Color::from_rgba8(0xA0, 0xA0, 0xA0, 0xFF));

    let mut row_y = toolbar_h + 32.0;
    for (i, layer) in model.layers.iter().enumerate() {
        let is_active = i == active_layer;
        let is_vis = layer_visible.get(i).copied().unwrap_or(true);

        if is_active {
            let mut active_bg = Paint::default();
            active_bg.set_color_rgba8(0x2D, 0x37, 0x48, 0xFF);
            if let Some(rect) = Rect::from_xywh(4.0, row_y - 2.0, layer_panel_w - 8.0, 24.0) {
                pixmap.fill_rect(rect, &active_bg, Transform::identity(), None);
            }
        }

        let mut cb_paint = Paint::default();
        if is_vis {
            cb_paint.set_color_rgba8(0x4A, 0x90, 0xE2, 0xFF);
        } else {
            cb_paint.set_color_rgba8(0x44, 0x48, 0x55, 0xFF);
        }
        if let Some(rect) = Rect::from_xywh(12.0, row_y + 3.0, 14.0, 14.0) {
            pixmap.fill_rect(rect, &cb_paint, Transform::identity(), None);
        }

        let c = layer.color;
        let mut swatch_paint = Paint::default();
        swatch_paint.set_color_rgba8(
            (c[0] * 255.0) as u8,
            (c[1] * 255.0) as u8,
            (c[2] * 255.0) as u8,
            255,
        );
        if let Some(rect) = Rect::from_xywh(34.0, row_y + 3.0, 14.0, 14.0) {
            pixmap.fill_rect(rect, &swatch_paint, Transform::identity(), None);
        }

        let name_col = if is_vis { text_white } else { Color::from_rgba8(0x70, 0x74, 0x80, 0xFF) };
        draw_text(&mut pixmap, 56.0, row_y + 7.0, &layer.name, name_col);

        let badge_text = if layer.is_signal { "Sig" } else { "Pln" };
        let mut badge_paint = Paint::default();
        badge_paint.set_color_rgba8(if layer.is_signal { 0x23 } else { 0x4A }, if layer.is_signal { 0x4E } else { 0x55 }, if layer.is_signal { 0x52 } else { 0x68 }, 0xFF);
        if let Some(rect) = Rect::from_xywh(175.0, row_y + 3.0, 24.0, 14.0) {
            pixmap.fill_rect(rect, &badge_paint, Transform::identity(), None);
        }
        draw_text(&mut pixmap, 178.0, row_y + 7.0, badge_text, text_white);

        row_y += 28.0;
    }

    // 4. Status bar at the bottom
    let status_y = (height as f32) - statusbar_h;
    let mut status_paint = Paint::default();
    status_paint.set_color_rgba8(0x16, 0x18, 0x20, 0xFF);
    if let Some(rect) = Rect::from_xywh(0.0, status_y, width as f32, statusbar_h) {
        pixmap.fill_rect(rect, &status_paint, Transform::identity(), None);
    }
    if let Some(rect) = Rect::from_xywh(0.0, status_y, width as f32, 1.0) {
        pixmap.fill_rect(rect, &border_paint, Transform::identity(), None);
    }

    draw_text(&mut pixmap, 12.0, status_y + 10.0, "X:   12.345 mm,  Y:  -45.678 mm", text_white);
    draw_text(&mut pixmap, 280.0, status_y + 10.0, "Log: Ready", Color::from_rgba8(0x90, 0x94, 0xA0, 0xFF));
    let stats_text = format!(
        "Pass: {} | Incomplete: {} | Violations: {} | Score: {:.0} | Elapsed: {:.1}s | Items: {}",
        model.stats.pass, model.stats.incomplete, model.stats.violations, model.stats.score, model.stats.elapsed_s, model.stats.items
    );
    draw_text(&mut pixmap, 550.0, status_y + 10.0, &stats_text, text_white);
    draw_text(&mut pixmap, (width as f32) - 80.0, status_y + 10.0, "60 FPS", Color::from_rgba8(0x48, 0xBB, 0x78, 0xFF));

    if let Some(parent) = output_path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    pixmap.save_png(output_path)?;
    Ok(())
}
