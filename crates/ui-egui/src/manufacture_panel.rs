//! The Machining Simulation window and its picture in the viewport: the stock cut away as the
//! program runs in real time (or faster), the tool at its real size, the path taken, and the
//! G-code line being run in a window of its own. The toolbar's command (`ui.start` of
//! `manufacture.simulate`) opens it; the picture is drawn over the viewport with the
//! viewport's own camera.
//!
//! The picture is cut on its own grid, chosen by Drawing detail (the cells per detail level and
//! the finest cell are in Preferences, Simulation). The collision check is a separate, optional
//! run of `manufacture.simulate` with its own Check resolution.

use egui::{Color32, Pos2, RichText, Shape, Stroke};
use serde_json::{Value, json};
use solidcraft_engine::geom::Vec3;
use solidcraft_engine::manufacture::{Grid, Stock};
use solidcraft_engine::{WatchRun, watch_run};

use crate::SolveApp;
use crate::theme::Tokens;

const DETAIL_NAMES: [&str; 4] = ["Draft", "Normal", "Fine", "Smooth"];
const DIALECTS: [&str; 3] = ["grbl", "fanuc", "haas"];
const DIALECT_NAMES: [&str; 3] = ["Grbl", "Fanuc", "Haas"];
/// Most path segments drawn in a frame.
const MAX_SEGMENTS: usize = 60_000;
/// Collisions listed in the window.
const LISTED: usize = 8;
/// Buckets of the far-to-near sort of the stock's cells.
const DEPTH_BUCKETS: usize = 4096;
/// G-code lines drawn above the current one when the window follows it.
const LINES_ABOVE: usize = 6;

/// The window's state.
pub struct MfgPanel {
    /// Seconds of the program run so far.
    pub time: f64,
    pub playing: bool,
    /// The speed of Play as a power of ten times real time.
    pub speed_log: f64,
    /// How finely the stock is drawn (an index of `DETAIL_NAMES`).
    pub detail: usize,
    pub show_stock: bool,
    pub show_path: bool,
    pub show_tool: bool,
    /// Whether the collision check is run and shown.
    pub collisions: bool,
    /// Cell size of the collision check (mm).
    pub resolution: f64,
    pub show_gcode: bool,
    /// Index into `DIALECTS` of the program shown.
    pub dialect: usize,
    /// The run being watched.
    pub run: Option<Box<WatchRun>>,
    /// The detail the run was built for, and the design revision.
    built_for: (usize, u64),
    /// The collision check's reply and what it was run for.
    pub check: Option<Value>,
    check_for: (u64, u64),
    /// The program text and the line each move runs from.
    pub program: Option<(Vec<String>, Vec<usize>)>,
    program_for: (u64, usize),
    /// The G-code line last scrolled to.
    followed: Option<usize>,
    /// How far the tool has turned for the picture (radians).
    spin: f64,
    pub error: Option<String>,
}

impl MfgPanel {
    fn new(app: &SolveApp) -> MfgPanel {
        let p = &app.preferences;
        MfgPanel {
            time: 0.0,
            playing: false,
            speed_log: 0.0,
            detail: usize::from(p.sim_detail.min(3)),
            show_stock: p.sim_show_stock,
            show_path: p.sim_show_path,
            show_tool: p.sim_show_tool,
            collisions: p.sim_check_collisions,
            resolution: f64::from(p.sim_check_resolution),
            show_gcode: false,
            dialect: 0,
            run: None,
            built_for: (usize::MAX, u64::MAX),
            check: None,
            check_for: (u64::MAX, 0),
            program: None,
            program_for: (u64::MAX, 0),
            followed: None,
            spin: 0.0,
            error: None,
        }
    }

    /// Times faster than the real cut that Play runs.
    pub fn speed(&self) -> f64 {
        10f64.powf(self.speed_log.clamp(-1.0, 4.0))
    }

    /// Moves done at the current time.
    pub fn step(&self) -> usize {
        self.run.as_ref().map_or(0, |r| r.runner.step())
    }
}

impl Default for MfgPanel {
    fn default() -> Self {
        let prefs = crate::prefs::Prefs::default();
        MfgPanel {
            time: 0.0,
            playing: false,
            speed_log: 0.0,
            detail: usize::from(prefs.sim_detail.min(3)),
            show_stock: prefs.sim_show_stock,
            show_path: prefs.sim_show_path,
            show_tool: prefs.sim_show_tool,
            collisions: prefs.sim_check_collisions,
            resolution: f64::from(prefs.sim_check_resolution),
            show_gcode: false,
            dialect: 0,
            run: None,
            built_for: (usize::MAX, u64::MAX),
            check: None,
            check_for: (u64::MAX, 0),
            program: None,
            program_for: (u64::MAX, 0),
            followed: None,
            spin: 0.0,
            error: None,
        }
    }
}

/// Open the window.
pub fn open(app: &mut SolveApp) {
    app.manufacture = Some(MfgPanel::new(app));
}

fn vec3(v: &Value) -> Vec3 {
    let n = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    Vec3::new(n(0), n(1), n(2))
}

fn to_model(frame: &[Vec3; 4], p: Vec3) -> Vec3 {
    frame[0] + frame[1] * p.x + frame[2] * p.y + frame[3] * p.z
}

/// A time as `m:ss.s` (`h:mm:ss` from an hour on).
pub fn clock(seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    let whole = s.floor() as u64;
    let (h, m, sec) = (whole / 3600, whole % 3600 / 60, whole % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{:04.1}", sec as f64 + (s - s.floor())) }
}

/// The cell of the picture for a detail level: the cells asked for in Preferences over the
/// stock's footprint, not finer than the finest cell.
fn cell_for(app: &SolveApp, detail: usize, stock: &Stock) -> f64 {
    let p = &app.preferences;
    let cells = [p.sim_cells_draft, p.sim_cells_normal, p.sim_cells_fine, p.sim_cells_smooth];
    let n = f64::from(cells.get(detail).copied().unwrap_or(p.sim_cells_fine).max(1));
    let area = ((stock.max[0] - stock.min[0]) * (stock.max[1] - stock.min[1])).abs();
    (area / n).sqrt().max(f64::from(p.sim_finest_cell)).max(0.005)
}

/// Builds the run for the current design and detail.
fn make_run(app: &mut SolveApp, d: &mut MfgPanel) {
    d.run = None;
    d.error = None;
    d.check = None;
    d.program = None;
    d.followed = None;
    d.built_for = (d.detail, app.session.revision);
    let rapid = f64::from(app.preferences.sim_rapid_rate).max(1.0);
    // Stock first: its footprint sets the cell, so ask for a coarse one and refine it.
    let probe = match watch_run(&mut app.session, None, 1.0e3, rapid) {
        Ok(r) => r,
        Err(e) => {
            d.error = Some(e.to_string());
            return;
        }
    };
    let mut cell = cell_for(app, d.detail, &probe.stock);
    drop(probe);
    for _ in 0..6 {
        match watch_run(&mut app.session, None, cell, rapid) {
            Ok(run) => {
                let first = d.run.is_none() && d.time == 0.0;
                let duration = run.runner.duration();
                let play_seconds = f64::from(app.preferences.sim_play_seconds).max(1.0);
                if first && duration > 0.0 {
                    d.speed_log = (duration / play_seconds).max(1.0).log10().clamp(0.0, 4.0);
                }
                d.time = d.time.min(duration);
                d.run = Some(Box::new(run));
                return;
            }
            Err(e) => {
                d.error = Some(e.to_string());
                cell *= 1.6;
            }
        }
    }
}

/// The optional collision check: the whole program on the check's own grid.
fn make_check(app: &mut SolveApp, d: &mut MfgPanel) {
    d.check_for = (app.session.revision, d.resolution.to_bits());
    d.check = None;
    match app.session.execute("manufacture.simulate", &json!({"resolution": d.resolution})) {
        Ok(v) => d.check = Some(v),
        Err(e) => d.error = Some(e.to_string()),
    }
}

/// The program text with the line each move of the run is cut by.
fn make_program(app: &mut SolveApp, d: &mut MfgPanel) {
    d.program_for = (app.session.revision, d.dialect);
    d.program = None;
    let Some(run) = &d.run else { return };
    let ops: Vec<u32> = run.runner.operations().iter().map(|o| o.operation).collect();
    let params = json!({"dialect": DIALECTS[d.dialect.min(2)], "operations": ops, "map_steps": true});
    match app.session.execute("manufacture.post", &params) {
        Ok(v) => {
            let text = v["gcode"].as_str().unwrap_or("");
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            let map: Vec<usize> = v["step_lines"].as_array().into_iter().flatten().map(|n| n.as_u64().unwrap_or(0) as usize).collect();
            d.program = Some((lines, map));
        }
        Err(e) => d.error = Some(e.to_string()),
    }
}

/// The G-code line (from 0) the move in progress runs from.
pub fn current_line(d: &MfgPanel) -> Option<usize> {
    let (_, map) = d.program.as_ref()?;
    let index = d.step().min(map.len().checked_sub(1)?);
    map.get(index).copied()
}

/// Where the tool is and which tool it is: the tip in work coordinates, the move it is on.
fn tool_at(run: &WatchRun, time: f64) -> Option<(Vec3, usize)> {
    let r = &run.runner;
    let segs = r.segments();
    let times = r.times();
    let step = r.step();
    let Some(seg) = segs.get(step) else {
        let last = segs.last()?;
        return Some((last.to, last.op));
    };
    let start = step.checked_sub(1).and_then(|i| times.get(i)).copied().unwrap_or(0.0);
    let end = times.get(step).copied().unwrap_or(start);
    let f = if end > start { ((time - start) / (end - start)).clamp(0.0, 1.0) } else { 1.0 };
    Some((seg.from + (seg.to - seg.from) * f, seg.op))
}

fn mm3(v: f64) -> String {
    format!("{:.2} cm³", v / 1000.0)
}

/// Draw the window when it is open. Called once per frame.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut d) = app.manufacture.take() else { return };
    let t = Tokens::get();
    if d.run.is_none() && d.error.is_none()
        || d.built_for != (d.detail, app.session.revision) && (d.error.is_none() || d.built_for.1 != app.session.revision)
    {
        make_run(app, &mut d);
    }
    if d.collisions && d.run.is_some() && d.check_for != (app.session.revision, d.resolution.to_bits()) {
        make_check(app, &mut d);
    }
    if d.show_gcode && d.run.is_some() && d.program_for != (app.session.revision, d.dialect) {
        make_program(app, &mut d);
    }
    let duration = d.run.as_ref().map_or(0.0, |r| r.runner.duration());
    if d.playing {
        if d.time >= duration {
            d.playing = false;
        } else {
            let dt = f64::from(ctx.input(|i| i.stable_dt)).clamp(1.0 / 240.0, 0.1);
            d.time = (d.time + dt * d.speed()).min(duration);
            ctx.request_repaint();
        }
    }
    let rev_per_s = f64::from(app.preferences.sim_spin_cap);
    if let Some(run) = &d.run {
        let rpm = tool_at(run, d.time).and_then(|(_, op)| run.rpm.get(op).copied()).unwrap_or(0.0);
        let rate = (rpm / 60.0).min(rev_per_s).max(0.0);
        d.spin = (d.spin + rate * std::f64::consts::TAU * f64::from(ctx.input(|i| i.stable_dt)).min(0.1)) % std::f64::consts::TAU;
    }
    if let Some(run) = d.run.as_mut() {
        let want = run.runner.step_at(d.time);
        if want != run.runner.step() {
            run.runner.advance(want);
        }
    }
    let (mut play, mut rewind) = (false, false);
    let closed = crate::ui_kit::Window::new(egui::Id::new("sc_machining_simulation"), "gear", crate::i18n::tr_label("Machining Simulation"))
        .cards(2.0)
        .show_body(
        ctx,
        |ui| {
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(t.error));
            }
            if let Some(run) = &d.run {
                let r = &run.runner;
                let where_ = r.operations().iter().enumerate().find(|(_, o)| r.step() <= o.first_step + o.steps).map(|(_, o)| o);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{} of {}", clock(d.time), clock(duration))).strong());
                    ui.separator();
                    ui.label(format!("Removed {}", mm3(r.removed())));
                    if let Some(o) = where_ {
                        ui.separator();
                        ui.label(format!("Operation {}  T{}", o.operation, o.tool.id));
                    }
                });
                let mut time = d.time;
                crate::ui_kit::slider(ui, &mut time, 0.0..=duration.max(0.001), crate::ui_kit::Slider { format: clock, ..Default::default() });
                if (time - d.time).abs() > 1e-9 {
                    d.time = time;
                }
                ui.horizontal(|ui| {
                    play = crate::ui_kit::button(ui, if d.playing { "Pause" } else { "Play" }, crate::ui_kit::Button::Normal).clicked();
                    rewind = crate::ui_kit::button(ui, "Rewind", crate::ui_kit::Button::Normal).clicked();
                });
                ui.horizontal_wrapped(|ui| {
                    crate::ui_kit::checkbox(ui, "Show stock", &mut d.show_stock);
                    crate::ui_kit::checkbox(ui, "Show toolpath", &mut d.show_path);
                    crate::ui_kit::checkbox(ui, "Show tool", &mut d.show_tool);
                    crate::ui_kit::checkbox(ui, "Show G-code", &mut d.show_gcode);
                    crate::ui_kit::checkbox(ui, "Check collisions", &mut d.collisions);
                });
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    crate::ui_kit::slider(
                        ui,
                        &mut d.speed_log,
                        -1.0..=4.0,
                        crate::ui_kit::Slider { format: |v| format!("×{:.1} real time", 10f64.powf(v)), ..Default::default() },
                    );
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Drawing detail");
                crate::ui_kit::segmented(ui, &mut d.detail, &DETAIL_NAMES, crate::ui_kit::Segments::default());
            })
            .response
            .on_hover_text("How finely the stock is drawn: more cells look smoother and cost more as the program runs. The cells for each level are in Preferences, Simulation.");
            if d.collisions {
                ui.horizontal(|ui| {
                    ui.label("Check resolution (mm)");
                    crate::ui_kit::number_field(ui, &mut d.resolution, 0.05..=10.0, 0.05, "");
                })
                .response
                .on_hover_text("The cell size of the collision check only; it does not change how the stock is drawn. Smaller is more exact and slower.");
                if let Some(check) = &d.check {
                    let counts = &check["counts"];
                    let n = |k: &str| counts[k].as_u64().unwrap_or(0);
                    if n("gouge") + n("shank") + n("rapid") + n("floor") > 0 {
                        ui.label(
                            RichText::new(format!(
                                "Collisions: {} gouges, {} shank, {} rapid, {} below stock",
                                n("gouge"),
                                n("shank"),
                                n("rapid"),
                                n("floor")
                            ))
                            .color(t.error),
                        );
                        for c in check["collisions"].as_array().into_iter().flatten().take(LISTED) {
                            ui.label(
                                RichText::new(format!(
                                    "{} in operation {} at move {}, {:.2} mm deep",
                                    c["kind"].as_str().unwrap_or("collision"),
                                    c["operation"],
                                    c["step"],
                                    c["depth"].as_f64().unwrap_or(0.0)
                                ))
                                .small()
                                .color(t.text_dim),
                            );
                        }
                    } else {
                        ui.label(RichText::new("No collisions").color(t.text_dim));
                    }
                }
            }
        },
    );
    if d.show_gcode {
        gcode_window(ctx, &mut d, &t);
    }
    if play {
        d.playing = !d.playing;
        if d.playing && d.time >= duration {
            d.time = 0.0;
        }
    }
    if rewind {
        d.time = 0.0;
        d.playing = false;
    }
    if !closed {
        app.manufacture = Some(d);
    }
}

/// The program in a window of its own, the line being run highlighted.
fn gcode_window(ctx: &egui::Context, d: &mut MfgPanel, t: &Tokens) {
    let current = current_line(d);
    let mut dialect = d.dialect;
    let closed = crate::ui_kit::Window::new(egui::Id::new("sc_simulation_gcode"), "gear", crate::i18n::tr_label("G-code"))
        .cards(2.0)
        .tall(440.0)
        .show_body(ctx, |ui| {
            crate::ui_kit::segmented(ui, &mut dialect, &DIALECT_NAMES, crate::ui_kit::Segments::default());
            let Some((lines, _)) = &d.program else {
                ui.label(RichText::new("No program").color(t.text_dim));
                return;
            };
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y;
            let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(360.0);
            if current != d.followed
                && let Some(c) = current
            {
                area = area.vertical_scroll_offset(c.saturating_sub(LINES_ABOVE) as f32 * row_h);
            }
            area.show_rows(ui, row_h, lines.len(), |ui, range| {
                for i in range {
                    let text = lines.get(i).map_or("", String::as_str);
                    let on = current == Some(i);
                    let colour = if on { t.text } else { t.text_dim };
                    let response = ui.label(RichText::new(format!("{:>5}  {text}", i + 1)).monospace().color(colour));
                    if on {
                        ui.painter().rect_filled(response.rect.expand2(egui::vec2(4.0, 0.0)), 2.0, t.accent.gamma_multiply(0.25));
                    }
                }
            });
        });
    d.dialect = dialect;
    d.followed = current;
    if closed {
        d.show_gcode = false;
    }
}

/// The stock, the path so far, the tool and the collisions, drawn over the viewport.
pub fn paint(app: &SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let Some(d) = &app.manufacture else { return };
    let Some(run) = &d.run else { return };
    let t = Tokens::get();
    let frame = &run.frame;
    let r = &run.runner;
    if d.show_stock {
        paint_stock(painter, proj, frame, r.grid(), r.heights(), &run.stock, (t.body, t.body_edge));
    }
    if d.show_path {
        let segs = r.segments();
        let upto = r.step().min(segs.len());
        let stride = (upto / MAX_SEGMENTS).max(1);
        for (i, seg) in segs.iter().take(upto).enumerate() {
            let last = i + 1 == upto;
            if i % stride != 0 && !last {
                continue;
            }
            let (Some(a), Some(b)) = (proj.to_screen(to_model(frame, seg.from)), proj.to_screen(to_model(frame, seg.to))) else { continue };
            let stroke = if seg.rapid { Stroke::new(0.8, t.text_dim.gamma_multiply(0.6)) } else { Stroke::new(1.4, t.accent) };
            painter.line_segment([a, b], stroke);
        }
    }
    if let Some((tip, op)) = tool_at(run, d.time) {
        if d.show_tool
            && let Some(o) = r.operations().get(op)
        {
            crate::manufacture_tool_view::paint(painter, proj, frame, &o.tool, tip, d.spin);
        } else if let Some(p) = proj.to_screen(to_model(frame, tip)) {
            painter.circle(p, 5.0, t.accent, Stroke::new(1.5, t.text));
        }
    }
    if d.collisions
        && let Some(check) = &d.check
    {
        for c in check["collisions"].as_array().into_iter().flatten() {
            if c["step"].as_u64().is_none_or(|s| s as usize >= r.step()) {
                continue;
            }
            if let Some(p) = proj.to_screen(to_model(frame, vec3(&c["at"]))) {
                let k = 6.0;
                let stroke = Stroke::new(2.0, t.error);
                painter.line_segment([p + egui::vec2(-k, -k), p + egui::vec2(k, k)], stroke);
                painter.line_segment([p + egui::vec2(-k, k), p + egui::vec2(k, -k)], stroke);
            }
        }
    }
}

/// The stock top as a mesh. Cells still at the top of the stock and away from any cut are one
/// flat polygon; the cells near cuts share vertices, each with a normal from the slope around
/// it, so the colour blends across cells instead of showing tiles.
fn paint_stock(
    painter: &egui::Painter,
    proj: &crate::viewport::Proj,
    frame: &[Vec3; 4],
    grid: &Grid,
    z: &[f32],
    stock: &Stock,
    (base, shade): (Color32, Color32),
) {
    let (nx, ny) = (grid.nx, grid.ny);
    if nx == 0 || ny == 0 || z.len() != nx * ny {
        return;
    }
    let top = stock.max[2] as f32;
    let (origin, dir) = proj.ray(proj.rect.center());
    let light = (-dir).normalized().unwrap_or(Vec3::Z);
    let lit = |n: Vec3| -> f32 { n.dot(light).abs() as f32 };
    let colour = |n: Vec3| shade.lerp_to_gamma(base, 0.25 + 0.75 * lit(n));
    let (xmax, ymax) = (stock.max[0], stock.max[1]);
    // The flat top.
    let corners = [(grid.x0, grid.y0), (xmax, grid.y0), (xmax, ymax), (grid.x0, ymax)];
    let flat: Vec<Pos2> = corners.iter().filter_map(|(x, y)| proj.to_screen(to_model(frame, Vec3::new(*x, *y, f64::from(top))))).collect();
    if flat.len() == 4 {
        painter.add(Shape::convex_polygon(flat, colour(frame[3]), Stroke::NONE));
    }
    // Cells that are cut, and the ones next to them.
    let cut: Vec<bool> = z.iter().map(|h| *h < top - 1e-4).collect();
    let mut near = cut.clone();
    for j in 0..ny {
        for i in 0..nx {
            if cut.get(j * nx + i).copied().unwrap_or(false) {
                for (di, dj) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (a, b) = (i as i64 + di, j as i64 + dj);
                    if a >= 0
                        && b >= 0
                        && (a as usize) < nx
                        && (b as usize) < ny
                        && let Some(c) = near.get_mut(b as usize * nx + a as usize)
                    {
                        *c = true;
                    }
                }
            }
        }
    }
    let (cx, cy) = (nx + 1, ny + 1);
    let cell_z = |i: i64, j: i64| -> f64 {
        let (i, j) = (i.clamp(0, nx as i64 - 1) as usize, j.clamp(0, ny as i64 - 1) as usize);
        f64::from(z.get(j * nx + i).copied().unwrap_or(top))
    };
    // Height at a corner: the mean of the cells that meet there.
    let corner_h = |i: usize, j: usize| -> f64 {
        let (i, j) = (i as i64, j as i64);
        (cell_z(i - 1, j - 1) + cell_z(i, j - 1) + cell_z(i - 1, j) + cell_z(i, j)) / 4.0
    };
    let mut index = vec![u32::MAX; cx * cy];
    let mut mesh = egui::Mesh::default();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut vertex = |i: usize, j: usize, mesh: &mut egui::Mesh, positions: &mut Vec<Vec3>| -> Option<u32> {
        let slot = index.get_mut(j * cx + i)?;
        if *slot != u32::MAX {
            return Some(*slot);
        }
        let x = (grid.x0 + i as f64 * grid.cell).min(xmax.max(grid.x0));
        let y = (grid.y0 + j as f64 * grid.cell).min(ymax.max(grid.y0));
        let h = corner_h(i, j);
        let p = to_model(frame, Vec3::new(x, y, h));
        let sp = proj.to_screen(p)?;
        let dzdx = (corner_h(i + 1, j) - corner_h(i.saturating_sub(1), j)) / (grid.cell * 2.0);
        let dzdy = (corner_h(i, j + 1) - corner_h(i, j.saturating_sub(1))) / (grid.cell * 2.0);
        let local = Vec3::new(-dzdx, -dzdy, 1.0);
        let n = (frame[1] * local.x + frame[2] * local.y + frame[3] * local.z).normalized().unwrap_or(Vec3::Z);
        let id = mesh.vertices.len() as u32;
        mesh.colored_vertex(sp, colour(n));
        positions.push(p);
        *slot = id;
        Some(id)
    };
    let mut cells: Vec<(f64, [u32; 4])> = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            if !near.get(j * nx + i).copied().unwrap_or(false) {
                continue;
            }
            let (Some(a), Some(b), Some(c), Some(e)) = (
                vertex(i, j, &mut mesh, &mut positions),
                vertex(i + 1, j, &mut mesh, &mut positions),
                vertex(i + 1, j + 1, &mut mesh, &mut positions),
                vertex(i, j + 1, &mut mesh, &mut positions),
            ) else {
                continue;
            };
            let (Some(pa), Some(pc)) = (positions.get(a as usize), positions.get(c as usize)) else { continue };
            cells.push((((*pa + *pc) * 0.5 - origin).dot(dir), [a, b, c, e]));
        }
    }
    // Far cells first, so near ones cover them: a counting sort on the depth.
    let (lo, hi) = cells.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (d, _)| (lo.min(*d), hi.max(*d)));
    let span = (hi - lo).max(1e-9);
    let bucket = |d: f64| ((hi - d) / span * (DEPTH_BUCKETS - 1) as f64) as usize;
    let mut start = vec![0usize; DEPTH_BUCKETS + 1];
    for (d, _) in &cells {
        if let Some(s) = start.get_mut(bucket(*d).min(DEPTH_BUCKETS - 1) + 1) {
            *s += 1;
        }
    }
    for k in 1..start.len() {
        start[k] += start[k - 1];
    }
    let mut order = vec![0usize; cells.len()];
    for (n, (d, _)) in cells.iter().enumerate() {
        let b = bucket(*d).min(DEPTH_BUCKETS - 1);
        if let Some(s) = start.get_mut(b)
            && let Some(o) = order.get_mut(*s)
        {
            *o = n;
            *s += 1;
        }
    }
    mesh.indices.reserve(cells.len() * 6);
    for n in order {
        if let Some((_, [a, b, c, e])) = cells.get(n) {
            mesh.add_triangle(*a, *b, *c);
            mesh.add_triangle(*a, *c, *e);
        }
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::scenario::Harness;

    fn block_with_pocket(h: &mut Harness) {
        let run = |h: &mut Harness, id: &str, p: serde_json::Value| h.app.run(id, p).unwrap_or_else(|e| panic!("{id}: {e}"));
        run(h, "sketch.create", json!({"plane": "XY", "name": "Plate"}));
        run(h, "sketch.rectangle.two_point", json!({"p0": [0.0, 0.0], "p1": [40.0, 30.0]}));
        run(h, "sketch.finish", json!({}));
        run(h, "solid.extrude", json!({"distance": 10.0}));
        run(h, "sketch.create", json!({"plane": "XY", "name": "Pocket"}));
        run(h, "sketch.rectangle.two_point", json!({"p0": [10.0, 8.0], "p1": [30.0, 22.0]}));
        run(h, "sketch.finish", json!({}));
        run(h, "manufacture.tool.set", json!({"id": 1, "diameter": 4.0}));
        run(
            h,
            "manufacture.setup.set",
            json!({"id": 1, "plane": "XY", "origin": [0.0, 0.0, 10.0], "stock": {"min": [-5.0, -5.0, -10.0], "max": [45.0, 35.0, 3.0]}}),
        );
        let op = json!({"setup": 1, "tool": 1, "kind": {"type": "pocket", "source": {"sketch": "Pocket"}, "stepover": 0.5}, "top": 0.0, "bottom": -4.0, "feed": 500.0, "spindle_rpm": 8000.0});
        run(h, "manufacture.operation.set", op);
    }

    fn open_window(h: &mut Harness) {
        h.step(&json!({"start": "manufacture.simulate"})).unwrap();
        h.frames(3);
    }

    #[test]
    fn the_window_cuts_the_stock_as_the_time_goes_on() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        open_window(&mut h);
        let d = h.app.manufacture.as_ref().unwrap();
        let run = d.run.as_ref().expect("the run");
        assert!(run.runner.total() > 10 && run.runner.duration() > 1.0);
        assert!(d.step() <= 1, "only a move that takes no time is done at the start");
        assert!(h.widget("Play").is_some() && h.widget("Rewind").is_some());
        // Half way through the time, part of the program is done and some stock is gone.
        let half = run.runner.duration() / 2.0;
        h.app.manufacture.as_mut().unwrap().time = half;
        h.frames(2);
        let d = h.app.manufacture.as_ref().unwrap();
        assert!(d.step() > 0 && d.run.as_ref().unwrap().runner.removed() > 0.0);
        h.step(&json!({"widget": "Play"})).unwrap();
        h.frames(4);
        assert!(h.app.manufacture.as_ref().unwrap().time > half, "playing moves the clock on");
        h.step(&json!({"widget": "Rewind"})).unwrap();
        h.frames(2);
        let d = h.app.manufacture.as_ref().unwrap();
        assert!(d.step() <= 1 && d.time == 0.0);
        assert!(d.run.as_ref().unwrap().runner.removed() < 1e-9, "rewinding puts the stock back");
    }

    #[test]
    fn the_drawing_detail_sets_how_fine_the_stock_is_and_the_check_does_not() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        open_window(&mut h);
        let cells = |h: &Harness| h.app.manufacture.as_ref().unwrap().run.as_ref().map(|r| r.runner.grid().nx * r.runner.grid().ny).unwrap_or(0);
        h.app.manufacture.as_mut().unwrap().detail = 0;
        h.frames(3);
        let draft = cells(&h);
        h.app.manufacture.as_mut().unwrap().detail = 3;
        h.frames(3);
        let smooth = cells(&h);
        assert!(draft > 0 && smooth > draft * 3, "draft {draft} smooth {smooth}");
        // The collision check's cell is its own business.
        h.app.manufacture.as_mut().unwrap().resolution = 0.05;
        h.frames(3);
        assert_eq!(cells(&h), smooth);
    }

    #[test]
    fn collisions_are_checked_only_when_asked_for() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        open_window(&mut h);
        assert!(!h.app.manufacture.as_ref().unwrap().collisions && h.app.manufacture.as_ref().unwrap().check.is_none());
        h.app.manufacture.as_mut().unwrap().collisions = true;
        h.frames(3);
        let d = h.app.manufacture.as_ref().unwrap();
        assert!(d.check.as_ref().is_some_and(|c| c["counts"]["gouge"].as_u64().unwrap_or(0) >= 1), "the pocket is cut out of the solid block");
    }

    #[test]
    fn the_time_is_a_clock_and_the_speed_a_multiple_of_real_time() {
        assert_eq!(super::clock(75.5), "1:15.5");
        assert_eq!(super::clock(0.0), "0:00.0");
        assert_eq!(super::clock(3725.0), "1:02:05");
        assert_eq!(super::clock(f64::NAN), "0:00.0");
        let d = super::MfgPanel { speed_log: 2.0, ..Default::default() };
        assert!((d.speed() - 100.0).abs() < 1e-9);
        let d = super::MfgPanel { speed_log: 99.0, ..Default::default() };
        assert!((d.speed() - 10_000.0).abs() < 1e-6, "clamped");
    }

    #[test]
    fn the_program_follows_the_cut_in_a_window_of_its_own() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        open_window(&mut h);
        h.app.manufacture.as_mut().unwrap().show_gcode = true;
        h.frames(3);
        let d = h.app.manufacture.as_ref().unwrap();
        let (lines, map) = d.program.as_ref().expect("the program");
        assert!(lines.len() > 10 && map.len() == d.run.as_ref().unwrap().runner.total());
        let end = d.run.as_ref().unwrap().runner.duration();
        h.app.manufacture.as_mut().unwrap().time = end * 0.6;
        h.frames(2);
        let d = h.app.manufacture.as_ref().unwrap();
        let line = super::current_line(d).expect("a current line");
        let (lines, _) = d.program.as_ref().unwrap();
        let text = lines.get(line).map(String::as_str).unwrap_or("");
        assert!(text.starts_with('G') || text.starts_with('X') || text.starts_with('Y') || text.starts_with('Z'), "line {line}: {text}");
        // Another dialect is another text for the same moves.
        h.app.manufacture.as_mut().unwrap().dialect = 1;
        h.frames(3);
        assert!(h.app.manufacture.as_ref().unwrap().program.as_ref().unwrap().0.iter().any(|l| l.contains("M6")));
    }

    #[test]
    fn a_job_without_stock_says_why_instead_of_drawing() {
        let mut h = Harness::new();
        open_window(&mut h);
        let d = h.app.manufacture.as_ref().unwrap();
        assert!(d.run.is_none() && d.error.is_some());
    }
}
