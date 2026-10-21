//! The Manufacture window: the job's tool library, setups and operations, and the G-code
//! program. Everything here goes through the `manufacture.*` commands, so a click is exactly the
//! JSON call a script would make. The Manufacture workspace's ribbon opens it on a tab.

use egui::RichText;
use serde_json::{Value, json};

use crate::SolveApp;
use crate::theme::Tokens;

pub const TABS: [&str; 4] = ["Tools", "Setups", "Operations", "G-code"];
const KINDS: [&str; 4] = ["Flat end mill", "Ball end mill", "Bull nose end mill", "Drill"];
const KIND_IDS: [&str; 4] = ["flat_end_mill", "ball_end_mill", "bull_nose_end_mill", "drill"];
const PLANES: [&str; 3] = ["XY", "XZ", "YZ"];
const OPS: [&str; 4] = ["Face", "Contour", "Pocket", "Drill"];
const SIDES: [&str; 3] = ["Outside", "Inside", "On the line"];
const SIDE_IDS: [&str; 3] = ["outside", "inside", "on"];
const DIRECTIONS: [&str; 2] = ["Climb", "Conventional"];
const DIRECTION_IDS: [&str; 2] = ["climb", "conventional"];
const DIALECTS: [&str; 3] = ["Grbl", "Fanuc", "Haas"];
const DIALECT_IDS: [&str; 3] = ["grbl", "fanuc", "haas"];
/// Longest program text shown in the window (bytes); the rest is still copied and saved.
const SHOWN_BYTES: usize = 60_000;

#[derive(Clone)]
struct ToolDraft {
    id: u32,
    name: String,
    kind: usize,
    diameter: f64,
    flutes: u32,
    corner_radius: f64,
    flute_length: f64,
}

#[derive(Clone)]
struct SetupDraft {
    id: u32,
    name: String,
    plane: usize,
    origin: [f64; 3],
    stock_min: [f64; 3],
    stock_max: [f64; 3],
    work_offset: u32,
    clearance: f64,
}

#[derive(Clone)]
struct OpDraft {
    id: u32,
    name: String,
    setup: u32,
    tool: u32,
    kind: usize,
    sketch: String,
    side: usize,
    direction: usize,
    stepover: f64,
    allowance: f64,
    margin: f64,
    finish: bool,
    top: f64,
    bottom: f64,
    retract: f64,
    stepdown: f64,
    spindle_rpm: f64,
    feed: f64,
    plunge_feed: f64,
    coolant: bool,
}

pub struct JobWindow {
    pub tab: usize,
    tool: ToolDraft,
    setup: SetupDraft,
    op: OpDraft,
    dialect: usize,
    program_name: String,
    line_numbers: bool,
    path: String,
    program: Option<(String, Vec<String>)>,
    /// A refusal or a confirmation of the last click.
    message: Option<(String, bool)>,
}

impl JobWindow {
    pub fn new(tab: usize) -> Self {
        JobWindow {
            tab: tab.min(TABS.len() - 1),
            tool: ToolDraft { id: 0, name: String::new(), kind: 0, diameter: 6.0, flutes: 2, corner_radius: 0.0, flute_length: 0.0 },
            setup: SetupDraft {
                id: 0,
                name: String::new(),
                plane: 0,
                origin: [0.0; 3],
                stock_min: [0.0, 0.0, -20.0],
                stock_max: [100.0, 100.0, 0.0],
                work_offset: 54,
                clearance: 10.0,
            },
            op: OpDraft {
                id: 0,
                name: String::new(),
                setup: 1,
                tool: 1,
                kind: 2,
                sketch: String::new(),
                side: 0,
                direction: 0,
                stepover: 0.4,
                allowance: 0.0,
                margin: 0.0,
                finish: false,
                top: 0.0,
                bottom: -5.0,
                retract: 2.0,
                stepdown: 0.0,
                spindle_rpm: 8000.0,
                feed: 800.0,
                plunge_feed: 0.0,
                coolant: false,
            },
            dialect: 0,
            program_name: String::new(),
            line_numbers: false,
            path: String::new(),
            program: None,
            message: None,
        }
    }
}

/// Open the window on a tab.
pub fn open(app: &mut SolveApp, tab: usize) {
    match &mut app.manufacture_job {
        Some(w) => w.tab = tab.min(TABS.len() - 1),
        None => app.manufacture_job = Some(JobWindow::new(tab)),
    }
}

fn f(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}
fn u(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}
fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}
fn triple(v: &Value) -> [f64; 3] {
    let n = |i: usize| v.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    [n(0), n(1), n(2)]
}
fn position(ids: &[&str], v: &Value, key: &str) -> usize {
    let name = v.get(key).and_then(Value::as_str).unwrap_or("");
    ids.iter().position(|i| *i == name).unwrap_or(0)
}

/// Run a command and keep its outcome for the window.
fn run(app: &mut SolveApp, w: &mut JobWindow, id: &str, params: Value, done: &str) -> Option<Value> {
    match app.run(id, params) {
        Ok(v) => {
            w.message = Some((done.to_string(), false));
            Some(v)
        }
        Err(e) => {
            w.message = Some((e, true));
            None
        }
    }
}

fn vec3_row(ui: &mut egui::Ui, label: &str, v: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for x in v.iter_mut() {
            crate::ui_kit::number_field(ui, x, -1.0e5..=1.0e5, 0.5, "");
        }
    });
}

fn field_row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.label(label);
        add(ui)
    })
    .inner
}

fn num(ui: &mut egui::Ui, label: &str, v: &mut f64, range: std::ops::RangeInclusive<f64>, suffix: &str) {
    field_row(ui, label, |ui| crate::ui_kit::number_field(ui, v, range, 0.1, suffix));
}

fn tools_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    let mut act: Option<(bool, u32)> = None;
    for tool in job["tools"].as_array().into_iter().flatten() {
        ui.horizontal(|ui| {
            let kind = KINDS.get(position(&KIND_IDS, tool, "kind")).copied().unwrap_or("");
            ui.label(format!("T{}  {}  {} \u{2300}{} mm", u(tool, "id"), s(tool, "name"), crate::i18n::tr_label(kind), f(tool, "diameter")));
            if crate::ui_kit::button(ui, "Edit", crate::ui_kit::Button::Small).clicked() {
                act = Some((true, u(tool, "id")));
            }
            if crate::ui_kit::button(ui, "Delete", crate::ui_kit::Button::Small).clicked() {
                act = Some((false, u(tool, "id")));
            }
        });
    }
    if job["tools"].as_array().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No tools yet").color(t.text_dim));
    }
    if let Some((edit, id)) = act {
        if edit {
            if let Some(tool) = job["tools"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)) {
                w.tool = ToolDraft {
                    id,
                    name: s(tool, "name"),
                    kind: position(&KIND_IDS, tool, "kind"),
                    diameter: f(tool, "diameter"),
                    flutes: u(tool, "flutes").max(1),
                    corner_radius: f(tool, "corner_radius"),
                    flute_length: f(tool, "flute_length"),
                };
            }
        } else {
            run(app, w, "manufacture.tool.delete", json!({"id": id}), "Tool deleted");
        }
    }
    ui.separator();
    let d = &mut w.tool;
    ui.label(RichText::new(if d.id == 0 { "New tool" } else { "Edit tool" }).strong());
    field_row(ui, "Name", |ui| crate::ui_kit::field(ui, &mut d.name));
    field_row(ui, "Type", |ui| crate::ui_kit::dropdown(ui, "mfg_tool_kind", &mut d.kind, &KINDS));
    num(ui, "Diameter", &mut d.diameter, 0.05..=500.0, " mm");
    field_row(ui, "Flutes", |ui| crate::ui_kit::number_field(ui, &mut d.flutes, 1..=16, 0.1, ""));
    if d.kind == 2 {
        num(ui, "Corner radius", &mut d.corner_radius, 0.0..=250.0, " mm");
    }
    num(ui, "Flute length (0: not checked)", &mut d.flute_length, 0.0..=2000.0, " mm");
    let mut save = false;
    ui.horizontal(|ui| {
        save = crate::ui_kit::button(ui, if d.id == 0 { "Add tool" } else { "Save tool" }, crate::ui_kit::Button::Normal).clicked();
        if d.id != 0 && crate::ui_kit::button(ui, "New tool", crate::ui_kit::Button::Normal).clicked() {
            d.id = 0;
        }
    });
    if save {
        let d = w.tool.clone();
        let mut p = json!({"name": d.name, "kind": KIND_IDS[d.kind.min(3)], "diameter": d.diameter, "flutes": d.flutes, "corner_radius": d.corner_radius});
        if d.id != 0 {
            p["id"] = json!(d.id);
        }
        if d.flute_length > 0.0 {
            p["flute_length"] = json!(d.flute_length);
        }
        if let Some(v) = run(app, w, "manufacture.tool.set", p, "Tool saved") {
            w.tool.id = u(&v, "id");
        }
    }
}

fn setups_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    let mut act: Option<(bool, u32)> = None;
    for setup in job["setups"].as_array().into_iter().flatten() {
        ui.horizontal(|ui| {
            ui.label(format!("{}  {}  G{}", u(setup, "id"), s(setup, "name"), u(setup, "work_offset")));
            if crate::ui_kit::button(ui, "Edit", crate::ui_kit::Button::Small).clicked() {
                act = Some((true, u(setup, "id")));
            }
            if crate::ui_kit::button(ui, "Delete", crate::ui_kit::Button::Small).clicked() {
                act = Some((false, u(setup, "id")));
            }
        });
    }
    if job["setups"].as_array().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No setups yet").color(t.text_dim));
    }
    if let Some((edit, id)) = act {
        if edit {
            if let Some(x) = job["setups"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)) {
                let origin = triple(&x["frame"]["origin"]);
                w.setup = SetupDraft {
                    id,
                    name: s(x, "name"),
                    plane: w.setup.plane,
                    origin,
                    stock_min: triple(&x["stock"]["min"]),
                    stock_max: triple(&x["stock"]["max"]),
                    work_offset: u(x, "work_offset").clamp(54, 59),
                    clearance: f(x, "clearance"),
                };
            }
        } else {
            run(app, w, "manufacture.setup.delete", json!({"id": id}), "Setup deleted");
        }
    }
    ui.separator();
    let d = &mut w.setup;
    ui.label(RichText::new(if d.id == 0 { "New setup" } else { "Edit setup" }).strong());
    field_row(ui, "Name", |ui| crate::ui_kit::field(ui, &mut d.name));
    field_row(ui, "Work plane", |ui| crate::ui_kit::dropdown(ui, "mfg_setup_plane", &mut d.plane, &PLANES));
    vec3_row(ui, "Zero point (model)", &mut d.origin);
    vec3_row(ui, "Stock min", &mut d.stock_min);
    vec3_row(ui, "Stock max", &mut d.stock_max);
    field_row(ui, "Work offset (G54 to G59)", |ui| crate::ui_kit::number_field(ui, &mut d.work_offset, 54..=59, 0.1, ""));
    num(ui, "Clearance height", &mut d.clearance, 0.0..=1.0e4, " mm");
    let mut save = false;
    ui.horizontal(|ui| {
        save = crate::ui_kit::button(ui, if d.id == 0 { "Add setup" } else { "Save setup" }, crate::ui_kit::Button::Normal).clicked();
        if d.id != 0 && crate::ui_kit::button(ui, "New setup", crate::ui_kit::Button::Normal).clicked() {
            d.id = 0;
        }
    });
    if save {
        let d = w.setup.clone();
        let mut p = json!({
            "name": d.name,
            "plane": PLANES[d.plane.min(2)],
            "origin": d.origin,
            "work_offset": d.work_offset,
            "clearance": d.clearance,
            "stock": {"min": d.stock_min, "max": d.stock_max},
        });
        if d.id != 0 {
            p["id"] = json!(d.id);
        }
        if let Some(v) = run(app, w, "manufacture.setup.set", p, "Setup saved") {
            w.setup.id = u(&v, "id");
        }
    }
}

fn sketch_names(app: &SolveApp) -> Vec<String> {
    app.session.model.state().sketches.iter().map(|s| s.name.clone()).collect()
}

fn operations_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    let mut act: Option<(u8, u32)> = None;
    for op in job["operations"].as_array().into_iter().flatten() {
        ui.horizontal(|ui| {
            let kind = match op["kind"]["type"].as_str() {
                Some("face") => OPS[0],
                Some("contour") => OPS[1],
                Some("pocket") => OPS[2],
                _ => OPS[3],
            };
            ui.label(format!("{}  {}  {}  T{}  setup {}", u(op, "id"), s(op, "name"), crate::i18n::tr_label(kind), u(op, "tool"), u(op, "setup")));
            if crate::ui_kit::button(ui, "Edit", crate::ui_kit::Button::Small).clicked() {
                act = Some((0, u(op, "id")));
            }
            if crate::ui_kit::button(ui, "Check path", crate::ui_kit::Button::Small).clicked() {
                act = Some((1, u(op, "id")));
            }
            if crate::ui_kit::button(ui, "Delete", crate::ui_kit::Button::Small).clicked() {
                act = Some((2, u(op, "id")));
            }
        });
    }
    if job["operations"].as_array().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No operations yet").color(t.text_dim));
    }
    if let Some((what, id)) = act {
        match what {
            0 => {
                if let Some(x) = job["operations"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)) {
                    let k = &x["kind"];
                    let mut d = w.op.clone();
                    d.id = id;
                    d.name = s(x, "name");
                    d.setup = u(x, "setup");
                    d.tool = u(x, "tool");
                    d.kind = ["face", "contour", "pocket", "drill"].iter().position(|n| k["type"] == *n).unwrap_or(2);
                    d.side = position(&SIDE_IDS, k, "side");
                    d.direction = position(&DIRECTION_IDS, k, "direction");
                    if let Some(v) = k.get("stepover").and_then(Value::as_f64) {
                        d.stepover = v;
                    }
                    d.allowance = f(k, "allowance");
                    d.margin = f(k, "margin");
                    d.finish = k["finish"].as_bool().unwrap_or(false);
                    d.top = f(x, "top");
                    d.bottom = f(x, "bottom");
                    d.retract = f(x, "retract");
                    d.stepdown = f(x, "stepdown");
                    d.spindle_rpm = f(x, "spindle_rpm");
                    d.feed = f(x, "feed");
                    d.plunge_feed = f(x, "plunge_feed");
                    d.coolant = x["coolant"].as_bool().unwrap_or(false);
                    w.op = d;
                }
            }
            1 => {
                if let Some(v) = run(app, w, "manufacture.toolpath", json!({"operation": id}), "") {
                    let st = &v["stats"];
                    let mut text = format!(
                        "Operation {id}: {} moves, {:.1} min, cutting {:.0} mm, rapid {:.0} mm",
                        v["moves"],
                        f(st, "minutes"),
                        f(st, "feed_length"),
                        f(st, "rapid_length")
                    );
                    for warning in v["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        text.push_str(&format!("\n{warning}"));
                    }
                    w.message = Some((text, false));
                }
            }
            _ => {
                run(app, w, "manufacture.operation.delete", json!({"id": id}), "Operation deleted");
            }
        }
    }
    ui.separator();
    let sketches = sketch_names(app);
    let d = &mut w.op;
    ui.label(RichText::new(if d.id == 0 { "New operation" } else { "Edit operation" }).strong());
    field_row(ui, "Name", |ui| crate::ui_kit::field(ui, &mut d.name));
    field_row(ui, "Operation", |ui| crate::ui_kit::dropdown(ui, "mfg_op_kind", &mut d.kind, &OPS));
    field_row(ui, "Setup number", |ui| crate::ui_kit::number_field(ui, &mut d.setup, 1..=999, 0.1, ""));
    field_row(ui, "Tool number", |ui| crate::ui_kit::number_field(ui, &mut d.tool, 1..=999, 0.1, ""));
    if matches!(d.kind, 1..=3) {
        let mut at = sketches.iter().position(|n| *n == d.sketch).unwrap_or(0);
        if sketches.is_empty() {
            ui.label(RichText::new("Draw a sketch first: the profiles come from it").color(t.text_dim));
        } else {
            let names: Vec<&str> = sketches.iter().map(String::as_str).collect();
            let before = at;
            field_row(ui, "Sketch", |ui| crate::ui_kit::dropdown(ui, "mfg_op_sketch", &mut at, &names));
            if at != before || d.sketch.is_empty() {
                d.sketch = sketches.get(at).cloned().unwrap_or_default();
            }
        }
    }
    match d.kind {
        0 => {
            num(ui, "Overhang", &mut d.margin, 0.0..=1.0e3, " mm");
            num(ui, "Stepover (fraction of diameter)", &mut d.stepover, 0.05..=1.0, "");
        }
        1 => {
            field_row(ui, "Side", |ui| crate::ui_kit::dropdown(ui, "mfg_op_side", &mut d.side, &SIDES));
            field_row(ui, "Direction", |ui| crate::ui_kit::dropdown(ui, "mfg_op_dir", &mut d.direction, &DIRECTIONS));
            num(ui, "Stock to leave on walls", &mut d.allowance, 0.0..=50.0, " mm");
        }
        2 => {
            field_row(ui, "Direction", |ui| crate::ui_kit::dropdown(ui, "mfg_op_dir", &mut d.direction, &DIRECTIONS));
            num(ui, "Stepover (fraction of diameter)", &mut d.stepover, 0.05..=1.0, "");
            num(ui, "Stock to leave on walls", &mut d.allowance, 0.0..=50.0, " mm");
            crate::ui_kit::checkbox(ui, "Finish pass", &mut d.finish);
        }
        _ => {}
    }
    num(ui, "Top of cut (work Z)", &mut d.top, -1.0e4..=1.0e4, " mm");
    num(ui, "Bottom of cut (work Z)", &mut d.bottom, -1.0e4..=1.0e4, " mm");
    num(ui, "Retract above top", &mut d.retract, 0.0..=1.0e3, " mm");
    if d.kind != 3 {
        num(ui, "Step down (0: half the tool)", &mut d.stepdown, 0.0..=1.0e3, " mm");
    }
    num(ui, "Spindle speed", &mut d.spindle_rpm, 1.0..=1.0e5, " rpm");
    num(ui, "Feed", &mut d.feed, 1.0..=1.0e5, " mm/min");
    num(ui, "Plunge feed (0: half the feed)", &mut d.plunge_feed, 0.0..=1.0e5, " mm/min");
    crate::ui_kit::checkbox(ui, "Coolant", &mut d.coolant);
    let mut save = false;
    ui.horizontal(|ui| {
        save = crate::ui_kit::button(ui, if d.id == 0 { "Add operation" } else { "Save operation" }, crate::ui_kit::Button::Normal).clicked();
        if d.id != 0 && crate::ui_kit::button(ui, "New operation", crate::ui_kit::Button::Normal).clicked() {
            d.id = 0;
        }
    });
    if save {
        let d = w.op.clone();
        let source = json!({"sketch": d.sketch});
        let kind = match d.kind {
            0 => json!({"type": "face", "margin": d.margin, "stepover": d.stepover}),
            1 => json!({"type": "contour", "source": source, "side": SIDE_IDS[d.side.min(2)], "direction": DIRECTION_IDS[d.direction.min(1)], "allowance": d.allowance}),
            2 => json!({"type": "pocket", "source": source, "direction": DIRECTION_IDS[d.direction.min(1)], "stepover": d.stepover, "allowance": d.allowance, "finish": d.finish}),
            _ => json!({"type": "drill", "points": {"from": "sketch", "sketch": d.sketch}}),
        };
        let mut p = json!({
            "name": d.name, "setup": d.setup, "tool": d.tool, "kind": kind,
            "top": d.top, "bottom": d.bottom, "retract": d.retract,
            "spindle_rpm": d.spindle_rpm, "feed": d.feed, "coolant": d.coolant,
        });
        if d.id != 0 {
            p["id"] = json!(d.id);
        }
        if d.stepdown > 0.0 && d.kind != 3 {
            p["stepdown"] = json!(d.stepdown);
        }
        if d.plunge_feed > 0.0 {
            p["plunge_feed"] = json!(d.plunge_feed);
        }
        if let Some(v) = run(app, w, "manufacture.operation.set", p, "Operation saved") {
            w.op.id = u(&v, "id");
        }
    }
}

fn program_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui) {
    let t = Tokens::get();
    field_row(ui, "Controller", |ui| crate::ui_kit::dropdown(ui, "mfg_dialect", &mut w.dialect, &DIALECTS));
    field_row(ui, "Program name", |ui| crate::ui_kit::field(ui, &mut w.program_name));
    crate::ui_kit::checkbox(ui, "Line numbers", &mut w.line_numbers);
    let mut make = false;
    ui.horizontal(|ui| {
        make = crate::ui_kit::button(ui, "Generate G-code", crate::ui_kit::Button::Normal).clicked();
        if crate::ui_kit::button(ui, "Simulate Machining", crate::ui_kit::Button::Normal).clicked() {
            crate::manufacture_panel::open(app);
        }
    });
    if make {
        let mut p = json!({"dialect": DIALECT_IDS[w.dialect.min(2)], "line_numbers": w.line_numbers});
        if !w.program_name.trim().is_empty() {
            p["program_name"] = json!(w.program_name.trim());
        }
        if let Some(v) = run(app, w, "manufacture.post", p, "G-code ready") {
            let warnings: Vec<String> = v["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            w.program = Some((s(&v, "gcode"), warnings));
        } else {
            w.program = None;
        }
    }
    if let Some((text, warnings)) = &w.program {
        for warning in warnings {
            ui.label(RichText::new(warning).color(t.error));
        }
        ui.label(RichText::new(format!("{} lines", text.lines().count())).small().color(t.text_dim));
        let shown = text.char_indices().nth(SHOWN_BYTES).map_or(text.as_str(), |(i, _)| &text[..i]);
        egui::ScrollArea::vertical().max_height(260.0).id_salt("mfg_gcode").show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(shown).monospace().size(t.font_small)).wrap_mode(egui::TextWrapMode::Extend));
        });
        let mut copy = false;
        let mut save = false;
        ui.horizontal(|ui| {
            copy = crate::ui_kit::button(ui, "Copy", crate::ui_kit::Button::Normal).clicked();
            crate::ui_kit::field(ui, &mut w.path);
            save = crate::ui_kit::button(ui, "Save file", crate::ui_kit::Button::Enabled(!w.path.trim().is_empty())).clicked();
        });
        if copy {
            ui.ctx().copy_text(text.clone());
        }
        if save {
            let mut p = json!({"dialect": DIALECT_IDS[w.dialect.min(2)], "line_numbers": w.line_numbers, "path": w.path.trim(), "overwrite": true});
            if !w.program_name.trim().is_empty() {
                p["program_name"] = json!(w.program_name.trim());
            }
            run(app, w, "manufacture.post", p, "Program saved");
        }
    }
}

/// Draw the window when it is open. Called once per frame.
pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut w) = app.manufacture_job.take() else { return };
    let t = Tokens::get();
    let job = app.session.execute("manufacture.job", &json!({})).unwrap_or(Value::Null);
    let mut open = true;
    let title = RichText::new(crate::i18n::tr_label("Manufacture")).strong().size(t.font_button);
    crate::frame::window(ctx, title, crate::frame::Width::Wide).id(egui::Id::new("sc_manufacture_job")).open(&mut open).resizable(true).show(ctx, |ui| {
        let mut tab = w.tab;
        crate::ui_kit::segmented(ui, &mut tab, &TABS, crate::ui_kit::Segments::default());
        w.tab = tab;
        if let Some((m, error)) = &w.message
            && !m.is_empty()
        {
            ui.label(RichText::new(m).color(if *error { t.error } else { t.text_dim }));
        }
        egui::ScrollArea::vertical().id_salt("mfg_job_scroll").show(ui, |ui| match w.tab {
            0 => tools_tab(app, &mut w, ui, &job),
            1 => setups_tab(app, &mut w, ui, &job),
            2 => operations_tab(app, &mut w, ui, &job),
            _ => program_tab(app, &mut w, ui),
        });
    });
    if open {
        app.manufacture_job = Some(w);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::scenario::Harness;

    #[test]
    fn the_window_adds_a_tool_through_the_command() {
        let mut h = Harness::new();
        super::open(&mut h.app, 0);
        h.frames(2);
        assert!(h.widget("Add tool").is_some());
        h.step(&json!({"widget": "Add tool"})).unwrap();
        h.frames(2);
        let job = h.app.session.doc.manufacture.clone();
        assert_eq!(job.tools.len(), 1, "the click ran manufacture.tool.set");
    }

    #[test]
    fn a_refused_operation_shows_why() {
        let mut h = Harness::new();
        super::open(&mut h.app, 2);
        h.frames(2);
        h.step(&json!({"widget": "Add operation"})).unwrap();
        h.frames(2);
        let w = h.app.manufacture_job.as_ref().unwrap();
        assert!(matches!(&w.message, Some((_, true))), "no tool or setup yet: the refusal is shown");
    }
}
