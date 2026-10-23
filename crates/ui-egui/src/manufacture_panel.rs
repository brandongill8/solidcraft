//! The Machining Simulation window and its picture in the viewport: the stock being cut away
//! along the job's toolpaths (`manufacture.simulate`), the path the tool has taken, and the
//! collisions found, with a slider to scrub through the program. The toolbar's command
//! (`ui.start` of `manufacture.simulate`) opens it; the picture is drawn over the viewport with
//! the viewport's own camera.

use egui::{Color32, Pos2, RichText, Shape, Stroke};
use serde_json::{Value, json};
use solidcraft_engine::geom::Vec3;

use crate::SolveApp;
use crate::theme::Tokens;

/// How finely the stock is drawn: a name and the most surface cells asked for. More cells look
/// smoother and cost more every time the program is stepped and every frame the view moves.
const DETAILS: [(&str, u64); 4] = [("Draft", 10_000), ("Normal", 40_000), ("Fine", 120_000), ("Smooth", 250_000)];
/// The detail a new window starts with (an index of `DETAILS`).
const DEFAULT_DETAIL: usize = 2;
/// Most path segments drawn in a frame.
const MAX_SEGMENTS: usize = 60_000;
/// Collisions listed in the window.
const LISTED: usize = 8;

/// The stock surface at one point of the program, in work coordinates.
struct Surface {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    z: Vec<f32>,
}

/// One move of the whole program, in work coordinates.
struct Seg {
    from: Vec3,
    to: Vec3,
    rapid: bool,
}

/// What the last full run found.
struct Report {
    reply: Value,
    total: usize,
    /// Work coordinates to model coordinates: origin and the three axes.
    frame: [Vec3; 4],
    stock_max: [f64; 2],
    segs: Vec<Seg>,
}

/// The window's state.
pub struct MfgPanel {
    /// Cell size of the runs (mm).
    pub resolution: f64,
    /// Moves shown so far.
    pub step: usize,
    pub playing: bool,
    /// How fast Play runs: 1 is the whole program in about five seconds.
    pub speed: f64,
    /// Part of a move not yet taken while playing slower than a move a frame.
    carry: f64,
    /// How finely the stock is drawn (an index of `DETAILS`).
    pub detail: usize,
    pub show_stock: bool,
    pub show_path: bool,
    /// The design revision the report is for.
    revision: u64,
    report: Option<Report>,
    /// The surface at `.0` moves.
    surface: Option<(usize, Surface)>,
    /// The detail `surface` was asked for.
    surface_detail: usize,
    /// The last refusal.
    pub error: Option<String>,
}

impl Default for MfgPanel {
    fn default() -> Self {
        MfgPanel {
            resolution: 1.0,
            step: 0,
            playing: false,
            speed: 1.0,
            carry: 0.0,
            detail: DEFAULT_DETAIL,
            show_stock: true,
            show_path: true,
            revision: u64::MAX,
            report: None,
            surface: None,
            surface_detail: DEFAULT_DETAIL,
            error: None,
        }
    }
}

/// Open the window.
pub fn open(app: &mut SolveApp) {
    app.manufacture = Some(MfgPanel::default());
}

fn vec3(v: &Value) -> Vec3 {
    let n = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    Vec3::new(n(0), n(1), n(2))
}

fn to_model(frame: &[Vec3; 4], p: Vec3) -> Vec3 {
    frame[0] + frame[1] * p.x + frame[2] * p.y + frame[3] * p.z
}

/// Runs the whole program and reads the paths.
fn load(app: &mut SolveApp, d: &mut MfgPanel) {
    d.report = None;
    d.surface = None;
    d.error = None;
    let reply = match app.session.execute("manufacture.simulate", &json!({"resolution": d.resolution})) {
        Ok(v) => v,
        Err(e) => {
            d.error = Some(e.to_string());
            return;
        }
    };
    let total = reply["total_steps"].as_u64().unwrap_or(0) as usize;
    let frame = [vec3(&reply["frame"]["origin"]), vec3(&reply["frame"]["x"]), vec3(&reply["frame"]["y"]), vec3(&reply["frame"]["z"])];
    let stock_max = [reply["stock"]["max"][0].as_f64().unwrap_or(0.0), reply["stock"]["max"][1].as_f64().unwrap_or(0.0)];
    let mut segs = Vec::new();
    for op in reply["operations"].as_array().into_iter().flatten() {
        let Some(id) = op["operation"].as_u64() else { continue };
        let params = json!({"operation": id, "moves": true, "expanded": true, "limit": 200_000});
        let Ok(path) = app.session.execute("manufacture.toolpath", &params) else { continue };
        // Each operation starts at the clearance height above its first move.
        let mut at: Option<Vec3> = None;
        for m in path["toolpath"].as_array().into_iter().flatten() {
            let to = vec3(&m["to"]);
            let rapid = m["move"] == "rapid";
            let from = at.unwrap_or(Vec3::new(to.x, to.y, to.z.max(10.0)));
            segs.push(Seg { from, to, rapid });
            at = Some(to);
        }
    }
    d.step = d.step.min(total);
    d.revision = app.session.revision;
    d.report = Some(Report { reply, total, frame, stock_max, segs });
}

/// The stock after `d.step` moves.
fn load_surface(app: &mut SolveApp, d: &mut MfgPanel) {
    d.surface_detail = d.detail;
    let params = json!({"resolution": d.resolution, "until": d.step, "part": false, "surface": DETAILS[d.detail.min(DETAILS.len() - 1)].1});
    match app.session.execute("manufacture.simulate", &params) {
        Ok(v) => {
            let s = &v["surface"];
            let z: Vec<f32> = s["z"].as_array().into_iter().flatten().map(|n| n.as_f64().unwrap_or(0.0) as f32).collect();
            let (nx, ny) = (s["nx"].as_u64().unwrap_or(0) as usize, s["ny"].as_u64().unwrap_or(0) as usize);
            if nx > 0 && ny > 0 && z.len() == nx * ny {
                d.surface = Some((
                    d.step,
                    Surface {
                        x0: s["x0"].as_f64().unwrap_or(0.0),
                        y0: s["y0"].as_f64().unwrap_or(0.0),
                        cell: s["cell"].as_f64().unwrap_or(1.0),
                        nx,
                        ny,
                        z,
                    },
                ));
            }
        }
        Err(e) => d.error = Some(e.to_string()),
    }
}

fn mm3(v: &Value) -> String {
    format!("{:.1} cm³", v.as_f64().unwrap_or(0.0) / 1000.0)
}

/// Draw the window when it is open. Called once per frame.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut d) = app.manufacture.take() else { return };
    let t = Tokens::get();
    if d.report.is_none() && d.error.is_none() || d.revision != app.session.revision && d.error.is_none() {
        load(app, &mut d);
    }
    if d.report.is_some() && (d.surface.as_ref().is_none_or(|(s, _)| *s != d.step) || d.surface_detail != d.detail) {
        load_surface(app, &mut d);
    }
    let total = d.report.as_ref().map_or(0, |r| r.total);
    if d.playing {
        if d.step >= total {
            d.playing = false;
        } else {
            // A move a frame at least when the speed is 1; slower speeds carry the rest over.
            d.carry += (total as f64 / 300.0).max(1.0) * d.speed.clamp(0.05, 50.0);
            let whole = d.carry.floor();
            d.carry -= whole;
            d.step = (d.step + whole as usize).min(total);
            ctx.request_repaint();
        }
    }
    let mut open = true;
    let (mut rerun, mut play, mut rewind) = (false, false, false);
    let title = RichText::new(crate::i18n::tr_label("Machining Simulation")).strong().size(t.font_button);
    crate::frame::window(ctx, title, crate::frame::Width::Wide).id(egui::Id::new("sc_machining_simulation")).open(&mut open).resizable(true).show(
        ctx,
        |ui| {
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(t.error));
            }
            if let Some(r) = &d.report {
                let v = &r.reply["volumes"];
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Removed {}", mm3(&v["removed"])));
                    ui.separator();
                    ui.label(format!("Left to cut {}", mm3(&v["leftover"])));
                    ui.separator();
                    let gouged = v["gouged"].as_f64().unwrap_or(0.0);
                    ui.label(RichText::new(format!("Cut from the part {}", mm3(&v["gouged"]))).color(if gouged > 1.0 {
                        t.error
                    } else {
                        t.text_dim
                    }));
                });
                ui.horizontal(|ui| {
                    crate::ui_kit::slider(ui, &mut d.step, 0..=r.total, crate::ui_kit::Slider { integer: true, ..Default::default() });
                });
                ui.horizontal(|ui| {
                    play = crate::ui_kit::button(ui, if d.playing { "Pause" } else { "Play" }, crate::ui_kit::Button::Normal).clicked();
                    rewind = crate::ui_kit::button(ui, "Rewind", crate::ui_kit::Button::Normal).clicked();
                    crate::ui_kit::checkbox(ui, "Show stock", &mut d.show_stock);
                    crate::ui_kit::checkbox(ui, "Show toolpath", &mut d.show_path);
                });
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    crate::ui_kit::slider(
                        ui,
                        &mut d.speed,
                        0.05..=20.0,
                        crate::ui_kit::Slider { format: |v| format!("{v:.2}x"), ..Default::default() },
                    );
                });
                let counts = &r.reply["counts"];
                let n = |k: &str| counts[k].as_u64().unwrap_or(0);
                let any = n("gouge") + n("shank") + n("rapid") + n("floor") > 0;
                if any {
                    ui.separator();
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
                    for c in r.reply["collisions"].as_array().into_iter().flatten().take(LISTED) {
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
                let tolerance = r.reply["tolerance"].as_f64().unwrap_or(0.0);
                ui.label(RichText::new(format!("Overlaps under {tolerance:.2} mm are not reported.")).small().color(t.text_dim));
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Drawing detail");
                for (i, (name, _)) in DETAILS.iter().enumerate() {
                    if ui.selectable_label(d.detail == i, *name).clicked() {
                        d.detail = i;
                    }
                }
            })
            .response
            .on_hover_text("How finely the stock is drawn. Smoother looks better; Draft steps through the program faster.");
            ui.horizontal(|ui| {
                ui.label("Check resolution (mm)");
                rerun = crate::ui_kit::number_field(ui, &mut d.resolution, 0.05..=10.0, 0.05, "").changed();
            })
            .response
            .on_hover_text("The cell size of the machining check that finds gouges and measures what is removed. Smaller is more exact and slower. It does not change how the stock is drawn: that is Drawing detail.");
        },
    );
    if play {
        d.playing = !d.playing;
        if d.playing && d.step >= total {
            d.step = 0;
        }
    }
    if rewind {
        d.step = 0;
        d.playing = false;
    }
    if rerun {
        d.revision = u64::MAX;
    }
    if open {
        app.manufacture = Some(d);
    }
}

/// The stock, the path so far and the collisions, drawn over the viewport.
pub fn paint(app: &SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let Some(d) = &app.manufacture else { return };
    let Some(report) = &d.report else { return };
    let t = Tokens::get();
    let frame = &report.frame;
    if d.show_stock
        && let Some((_, s)) = &d.surface
    {
        paint_stock(painter, proj, frame, s, report.stock_max, (t.body, t.body_edge));
    }
    if d.show_path {
        let upto = d.step.min(report.segs.len());
        let stride = (upto / MAX_SEGMENTS).max(1);
        for (i, seg) in report.segs.iter().take(upto).enumerate() {
            let last = i + 1 == upto;
            if i % stride != 0 && !last {
                continue;
            }
            let (Some(a), Some(b)) = (proj.to_screen(to_model(frame, seg.from)), proj.to_screen(to_model(frame, seg.to))) else { continue };
            let stroke = if seg.rapid { Stroke::new(0.8, t.text_dim.gamma_multiply(0.6)) } else { Stroke::new(1.4, t.accent) };
            painter.line_segment([a, b], stroke);
        }
        if let Some(seg) = d.step.checked_sub(1).and_then(|i| report.segs.get(i))
            && let Some(p) = proj.to_screen(to_model(frame, seg.to))
        {
            painter.circle(p, 5.0, t.accent, Stroke::new(1.5, t.text));
        }
    }
    for c in report.reply["collisions"].as_array().into_iter().flatten() {
        if c["step"].as_u64().is_none_or(|s| s as usize >= d.step) {
            continue;
        }
        let at = vec3(&c["at"]);
        if let Some(p) = proj.to_screen(to_model(frame, at)) {
            let r = 6.0;
            let stroke = Stroke::new(2.0, t.error);
            painter.line_segment([p + egui::vec2(-r, -r), p + egui::vec2(r, r)], stroke);
            painter.line_segment([p + egui::vec2(-r, r), p + egui::vec2(r, -r)], stroke);
        }
    }
}

fn paint_stock(
    painter: &egui::Painter,
    proj: &crate::viewport::Proj,
    frame: &[Vec3; 4],
    s: &Surface,
    stock_max: [f64; 2],
    (base, shade): (Color32, Color32),
) {
    let (cx, cy) = (s.nx + 1, s.ny + 1);
    // Height at each cell corner: the mean of the cells that meet there.
    let mut sum = vec![0.0f32; cx * cy];
    let mut count = vec![0u8; cx * cy];
    for j in 0..s.ny {
        for i in 0..s.nx {
            let z = s.z.get(j * s.nx + i).copied().unwrap_or(0.0);
            for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let k = (j + dj) * cx + i + di;
                if let (Some(a), Some(n)) = (sum.get_mut(k), count.get_mut(k)) {
                    *a += z;
                    *n += 1;
                }
            }
        }
    }
    let height: Vec<f32> = sum.iter().zip(&count).map(|(a, n)| a / f32::from((*n).max(1))).collect();
    let h = |i: usize, j: usize| -> f64 { f64::from(height.get(j.min(cy - 1) * cx + i.min(cx - 1)).copied().unwrap_or(0.0)) };
    // Where the eye looks along, in model space.
    let (origin, dir) = proj.ray(proj.rect.center());
    let light = (-dir).normalized().unwrap_or(Vec3::Z);
    // One vertex per corner with a normal from the slope around it, so the colour blends across
    // cells instead of showing each one as a flat tile.
    let mut mesh = egui::Mesh::default();
    mesh.vertices.reserve(cx * cy);
    let mut at: Vec<Option<Vec3>> = Vec::with_capacity(cx * cy);
    let mut on_screen: Vec<bool> = Vec::with_capacity(cx * cy);
    for j in 0..cy {
        for i in 0..cx {
            let x = (s.x0 + i as f64 * s.cell).min(stock_max[0].max(s.x0));
            let y = (s.y0 + j as f64 * s.cell).min(stock_max[1].max(s.y0));
            let p = to_model(frame, Vec3::new(x, y, h(i, j)));
            let dzdx = (h(i + 1, j) - h(i.saturating_sub(1), j)) / (s.cell * if i == 0 || i + 1 >= cx { 1.0 } else { 2.0 });
            let dzdy = (h(i, j + 1) - h(i, j.saturating_sub(1))) / (s.cell * if j == 0 || j + 1 >= cy { 1.0 } else { 2.0 });
            let local = Vec3::new(-dzdx, -dzdy, 1.0);
            let n = (frame[1] * local.x + frame[2] * local.y + frame[3] * local.z).normalized().unwrap_or(Vec3::Z);
            let lit = n.dot(light).abs() as f32;
            let colour = shade.lerp_to_gamma(base, 0.25 + 0.75 * lit);
            match proj.to_screen(p) {
                Some(sp) => {
                    mesh.colored_vertex(sp, colour);
                    on_screen.push(true);
                }
                None => {
                    mesh.colored_vertex(Pos2::ZERO, colour);
                    on_screen.push(false);
                }
            }
            at.push(Some(p));
        }
    }
    let mut cells: Vec<(f64, [u32; 4])> = Vec::with_capacity(s.nx * s.ny);
    for j in 0..s.ny {
        for i in 0..s.nx {
            let k = [j * cx + i, j * cx + i + 1, (j + 1) * cx + i + 1, (j + 1) * cx + i];
            if k.iter().any(|v| !on_screen.get(*v).copied().unwrap_or(false)) {
                continue;
            }
            let (Some(Some(a)), Some(Some(c))) = (at.get(k[0]), at.get(k[2])) else { continue };
            let centre = (*a + *c) * 0.5;
            cells.push(((centre - origin).dot(dir), k.map(|v| v as u32)));
        }
    }
    // Far cells first, so near ones cover them.
    cells.sort_by(|a, b| b.0.total_cmp(&a.0));
    mesh.indices.reserve(cells.len() * 6);
    for (_, [a, b, c, d]) in &cells {
        mesh.add_triangle(*a, *b, *c);
        mesh.add_triangle(*a, *c, *d);
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

    #[test]
    fn the_window_simulates_the_job_and_scrubs_through_it() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        h.step(&json!({"start": "manufacture.simulate"})).unwrap();
        assert!(h.app.manufacture.is_some(), "ui.start opens the window");
        h.frames(3);
        let d = h.app.manufacture.as_ref().unwrap();
        let report = d.report.as_ref().expect("the full run");
        assert!(report.total > 10 && !report.segs.is_empty());
        assert!(report.reply["counts"]["gouge"].as_u64().unwrap() >= 1, "the pocket is cut out of the solid block");
        assert!(d.surface.is_some(), "the stock is drawn");
        assert!(h.widget("Play").is_some() && h.widget("Rewind").is_some());
        h.step(&json!({"widget": "Play"})).unwrap();
        h.frames(4);
        assert!(h.app.manufacture.as_ref().unwrap().step > 0, "playing advances the program");
        h.step(&json!({"widget": "Rewind"})).unwrap();
        h.frames(2);
        assert_eq!(h.app.manufacture.as_ref().unwrap().step, 0);
    }

    #[test]
    fn the_speed_and_the_drawing_detail_are_adjustable() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        h.step(&json!({"start": "manufacture.simulate"})).unwrap();
        h.frames(3);
        for label in ["Speed", "Drawing detail", "Check resolution (mm)"] {
            assert!(h.widget(label).is_some(), "{label}");
        }
        let cells = |h: &Harness| h.app.manufacture.as_ref().unwrap().surface.as_ref().map(|(_, s)| s.nx * s.ny).unwrap_or(0);
        // The window opens on Fine; Draft draws fewer cells and Smooth more.
        h.app.manufacture.as_mut().unwrap().detail = 0;
        h.frames(3);
        let draft = cells(&h);
        h.app.manufacture.as_mut().unwrap().detail = 3;
        h.app.manufacture.as_mut().unwrap().resolution = 0.1;
        h.app.manufacture.as_mut().unwrap().revision = u64::MAX;
        h.frames(4);
        let smooth = cells(&h);
        assert!(draft > 0 && draft <= 10_000, "{draft}");
        assert!(smooth > draft, "smooth {smooth} draft {draft}");
        // Slower is fewer moves a frame; faster, more.
        let moves = |h: &mut Harness, speed: f64| {
            let d = h.app.manufacture.as_mut().unwrap();
            d.speed = speed;
            d.step = 0;
            d.playing = true;
            h.frames(10);
            h.app.manufacture.as_ref().unwrap().step
        };
        let slow = moves(&mut h, 0.25);
        let fast = moves(&mut h, 8.0);
        assert!(fast > slow, "fast {fast} slow {slow}");
    }

    #[test]
    fn a_job_without_stock_says_why_instead_of_drawing() {
        let mut h = Harness::new();
        h.step(&json!({"start": "manufacture.simulate"})).unwrap();
        h.frames(2);
        let d = h.app.manufacture.as_ref().unwrap();
        assert!(d.report.is_none() && d.error.is_some());
    }
}
