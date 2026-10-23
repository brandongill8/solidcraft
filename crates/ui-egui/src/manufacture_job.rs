//! The Manufacture windows: the job's tools, the setup, the operations and the G-code program,
//! each in a window of its own so any of them can stay open beside the model. Everything here
//! goes through the `manufacture.*` commands, so a click is exactly the JSON call a script would
//! make. The Manufacture workspace's ribbon opens each window.
//!
//! Over the model view: the Setup window draws its stock as a translucent box (live, as the
//! numbers are typed), and the Operations window draws the toolpath of every operation on the
//! part, the one being edited in full colour, with the moves that go down into the material
//! (plunges, ramps, helixes) in the warning colour. No simulation is needed for either.

use egui::{Align2, Pos2, RichText, Shape, Stroke};
use serde_json::{Value, json};
use solidcraft_engine::geom::{Plane, Vec3};

use crate::SolveApp;
use crate::theme::Tokens;

/// The windows, in the order of the ribbon's buttons.
pub const TABS: [&str; 4] = ["Tools", "Setup", "Operations", "G-code"];
const TOOLS: usize = 0;
const SETUP: usize = 1;
const OPERATIONS: usize = 2;
const GCODE: usize = 3;

const KINDS: [&str; 8] = ["Flat end mill", "Ball end mill", "Bull nose end mill", "Drill", "Spot drill", "Reamer", "Shell mill", "Face mill"];
const KIND_IDS: [&str; 8] = ["flat_end_mill", "ball_end_mill", "bull_nose_end_mill", "drill", "spot_drill", "reamer", "shell_mill", "face_mill"];
/// What a new tool of each kind starts with: its flutes and (drills) its point angle.
const KIND_FLUTES: [u32; 8] = [2, 2, 2, 2, 2, 6, 4, 5];
const KIND_TIP: [f64; 8] = [118.0, 118.0, 118.0, 118.0, 90.0, 118.0, 118.0, 118.0];
const BULL_NOSE: usize = 2;
const PLANES: [&str; 3] = ["XY", "XZ", "YZ"];
const OPS: [&str; 4] = ["Face", "Contour", "Pocket", "Drill"];
const SIDES: [&str; 3] = ["Outside", "Inside", "On the line"];
const SIDE_IDS: [&str; 3] = ["outside", "inside", "on"];
const DIRECTIONS: [&str; 2] = ["Climb", "Conventional"];
const DIRECTION_IDS: [&str; 2] = ["climb", "conventional"];
const ENTRIES: [&str; 3] = ["Plunge", "Ramp", "Helical"];
const ENTRY_IDS: [&str; 3] = ["plunge", "ramp", "helical"];
const DIALECTS: [&str; 3] = ["Grbl", "Fanuc", "Haas"];
const DIALECT_IDS: [&str; 3] = ["grbl", "fanuc", "haas"];
/// Longest program text shown in the window (bytes); the rest is still copied and saved.
const SHOWN_BYTES: usize = 60_000;
/// Most toolpath segments drawn in a frame.
const MAX_SEGMENTS: usize = 80_000;
/// Chord error of the toolpaths drawn over the model (mm): coarser than the program's.
const PREVIEW_TOLERANCE: f64 = 0.02;

#[derive(Clone)]
struct ToolDraft {
    id: u32,
    name: String,
    kind: usize,
    diameter: f64,
    flutes: u32,
    corner_radius: f64,
    tip_angle: f64,
    flute_length: f64,
    shank_diameter: f64,
}

impl ToolDraft {
    fn new() -> Self {
        ToolDraft {
            id: 0,
            name: String::new(),
            kind: 0,
            diameter: 6.0,
            flutes: KIND_FLUTES[0],
            corner_radius: 0.0,
            tip_angle: KIND_TIP[0],
            flute_length: 0.0,
            shank_diameter: 0.0,
        }
    }

    /// Switch to another kind of tool, taking what a new tool of that kind starts with.
    fn use_kind(&mut self, kind: usize) {
        let kind = kind.min(KINDS.len() - 1);
        self.kind = kind;
        self.flutes = KIND_FLUTES[kind];
        self.tip_angle = KIND_TIP[kind];
        self.corner_radius = if kind == BULL_NOSE { self.diameter * 0.1 } else { 0.0 };
    }

    /// The tool has a point (a drill or a spot drill).
    fn has_point(&self) -> bool {
        matches!(self.kind, 3 | 4)
    }
}

#[derive(Clone)]
struct SetupDraft {
    id: u32,
    name: String,
    plane: usize,
    /// The X and Y axes of a work frame that is none of the three named planes (kept as it is).
    custom: Option<([f64; 3], [f64; 3])>,
    origin: [f64; 3],
    has_stock: bool,
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
    entry: usize,
    ramp_angle: f64,
    helix_angle: f64,
    helix_diameter: f64,
    top: f64,
    bottom: f64,
    retract: f64,
    stepdown: f64,
    spindle_rpm: f64,
    feed: f64,
    plunge_feed: f64,
    coolant: bool,
}

/// How a toolpath segment is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SegKind {
    Rapid,
    /// A feed move that goes down into the material: a plunge, a ramp or a helix.
    Entry,
    Cut,
}

/// One segment of an operation's toolpath, in model coordinates.
#[derive(Clone, Copy)]
struct Seg {
    a: Vec3,
    b: Vec3,
    kind: SegKind,
}

/// The toolpath of one operation, or why it has none.
struct OpPath {
    id: u32,
    segs: Vec<Seg>,
    note: Option<String>,
}

/// The toolpaths drawn over the model, planned for one design revision.
#[derive(Default)]
struct Paths {
    revision: Option<u64>,
    ops: Vec<OpPath>,
}

pub struct JobWindow {
    /// Which of the four windows are open.
    open: [bool; 4],
    tool: ToolDraft,
    setup: SetupDraft,
    /// The setup window has not shown a setup yet: it starts on the first one there is.
    setup_fresh: bool,
    op: OpDraft,
    dialect: usize,
    program_name: String,
    line_numbers: bool,
    path: String,
    program: Option<(String, Vec<String>)>,
    /// Draw the operations' toolpaths over the model while the Operations window is open.
    show_paths: bool,
    /// The operation being made or edited: its toolpath is drawn in full colour.
    focus_op: Option<u32>,
    paths: Paths,
    /// The tool library file; empty is the one in the SolidCraft data folder.
    library_path: String,
    /// The saved tools, read when the Tools window opens and after each change to the file.
    library: Option<Result<Vec<Value>, String>>,
    /// A refusal or a confirmation of the last click, and the window it belongs to.
    message: Option<(usize, String, bool)>,
}

impl JobWindow {
    pub fn new() -> Self {
        JobWindow {
            open: [false; 4],
            tool: ToolDraft::new(),
            setup: SetupDraft {
                id: 0,
                name: String::new(),
                plane: 0,
                custom: None,
                origin: [0.0; 3],
                has_stock: true,
                stock_min: [0.0, 0.0, -20.0],
                stock_max: [100.0, 100.0, 0.0],
                work_offset: 54,
                clearance: 10.0,
            },
            setup_fresh: true,
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
                entry: 1,
                ramp_angle: 3.0,
                helix_angle: 3.0,
                helix_diameter: 0.0,
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
            show_paths: true,
            focus_op: None,
            paths: Paths::default(),
            library_path: String::new(),
            library: None,
            message: None,
        }
    }
}

impl Default for JobWindow {
    fn default() -> Self {
        JobWindow::new()
    }
}

/// Open one of the windows (a tab of [`TABS`]); the others stay as they are.
pub fn open(app: &mut SolveApp, tab: usize) {
    let w = app.manufacture_job.get_or_insert_with(JobWindow::new);
    let tab = tab.min(TABS.len() - 1);
    if !w.open[tab] {
        w.open[tab] = true;
        match tab {
            TOOLS => w.library = None,
            SETUP => w.setup_fresh = true,
            OPERATIONS => w.paths.revision = None,
            _ => {}
        }
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
fn vec3(v: &Value) -> Vec3 {
    let [x, y, z] = triple(v);
    Vec3::new(x, y, z)
}
fn position(ids: &[&str], v: &Value, key: &str) -> usize {
    let name = v.get(key).and_then(Value::as_str).unwrap_or("");
    ids.iter().position(|i| *i == name).unwrap_or(0)
}

/// Run a command and keep its outcome for the window `tab`.
fn run(app: &mut SolveApp, w: &mut JobWindow, tab: usize, id: &str, params: Value, done: &str) -> Option<Value> {
    match app.run(id, params) {
        Ok(v) => {
            w.message = Some((tab, done.to_string(), false));
            Some(v)
        }
        Err(e) => {
            w.message = Some((tab, e, true));
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

fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(Tokens::get().text_dim));
}

// ------------------------------------------------------------------------------------- tools

fn tool_line(tool: &Value) -> String {
    let kind = KINDS.get(position(&KIND_IDS, tool, "kind")).copied().unwrap_or("");
    format!("T{}  {}  {}  \u{2300}{} mm", u(tool, "id"), s(tool, "name"), crate::i18n::tr_label(kind), f(tool, "diameter"))
}

fn load_tool(tool: &Value) -> ToolDraft {
    ToolDraft {
        id: u(tool, "id"),
        name: s(tool, "name"),
        kind: position(&KIND_IDS, tool, "kind"),
        diameter: f(tool, "diameter"),
        flutes: u(tool, "flutes").max(1),
        corner_radius: f(tool, "corner_radius"),
        tip_angle: if tool.get("tip_angle").is_some() { f(tool, "tip_angle") } else { 118.0 },
        flute_length: f(tool, "flute_length"),
        shank_diameter: f(tool, "shank_diameter"),
    }
}

/// The saved tools of the library file, read afresh.
fn read_library(app: &mut SolveApp, w: &mut JobWindow) {
    let params = if w.library_path.trim().is_empty() { json!({}) } else { json!({"path": w.library_path.trim()}) };
    w.library = Some(match app.session.execute("manufacture.library.list", &params) {
        Ok(v) => Ok(v["tools"].as_array().cloned().unwrap_or_default()),
        Err(e) => Err(e.to_string()),
    });
}

fn library_params(w: &JobWindow, mut p: Value) -> Value {
    if !w.library_path.trim().is_empty() {
        p["path"] = json!(w.library_path.trim());
    }
    p
}

fn tools_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    if w.library.is_none() {
        read_library(app, w);
    }
    // 0 edit, 1 delete, 2 save to the library
    let mut act: Option<(u8, u32)> = None;
    for tool in job["tools"].as_array().into_iter().flatten() {
        ui.horizontal(|ui| {
            ui.label(tool_line(tool));
            if crate::ui_kit::button(ui, "Edit", crate::ui_kit::Button::Small).clicked() {
                act = Some((0, u(tool, "id")));
            }
            if crate::ui_kit::button(ui, "Save to library", crate::ui_kit::Button::Small).clicked() {
                act = Some((2, u(tool, "id")));
            }
            if crate::ui_kit::button(ui, "Delete", crate::ui_kit::Button::Small).clicked() {
                act = Some((1, u(tool, "id")));
            }
        });
    }
    if job["tools"].as_array().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No tools yet").color(t.text_dim));
    }
    if let Some((what, id)) = act {
        match what {
            0 => {
                if let Some(tool) = job["tools"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)) {
                    w.tool = load_tool(tool);
                }
            }
            1 => {
                run(app, w, TOOLS, "manufacture.tool.delete", json!({"id": id}), "Tool deleted");
            }
            _ => {
                let p = library_params(w, json!({"id": id}));
                if run(app, w, TOOLS, "manufacture.library.save", p, "Saved to the tool library").is_some() {
                    read_library(app, w);
                }
            }
        }
    }
    ui.separator();
    let d = &mut w.tool;
    ui.label(RichText::new(if d.id == 0 { "New tool" } else { "Edit tool" }).strong());
    field_row(ui, "Name", |ui| crate::ui_kit::field(ui, &mut d.name));
    let before = d.kind;
    field_row(ui, "Type", |ui| crate::ui_kit::dropdown(ui, "mfg_tool_kind", &mut d.kind, &KINDS));
    if d.kind != before {
        let chosen = d.kind;
        d.use_kind(chosen);
    }
    num(ui, "Diameter", &mut d.diameter, 0.05..=500.0, " mm");
    field_row(ui, "Flutes", |ui| crate::ui_kit::number_field(ui, &mut d.flutes, 1..=16, 0.1, ""));
    if d.kind == BULL_NOSE {
        num(ui, "Corner radius", &mut d.corner_radius, 0.0..=250.0, " mm");
    }
    if d.has_point() {
        num(ui, "Point angle", &mut d.tip_angle, 1.0..=179.0, " deg");
    }
    num(ui, "Flute length (0: not checked)", &mut d.flute_length, 0.0..=2000.0, " mm");
    num(ui, "Shank diameter (0: same as the tool)", &mut d.shank_diameter, 0.0..=500.0, " mm");
    let mut save = false;
    ui.horizontal(|ui| {
        save = crate::ui_kit::button(ui, if d.id == 0 { "Add tool" } else { "Save tool" }, crate::ui_kit::Button::Normal).clicked();
        if d.id != 0 && crate::ui_kit::button(ui, "New tool", crate::ui_kit::Button::Normal).clicked() {
            *d = ToolDraft::new();
        }
    });
    if save {
        let d = w.tool.clone();
        let mut p = json!({
            "name": d.name,
            "kind": KIND_IDS[d.kind.min(KIND_IDS.len() - 1)],
            "diameter": d.diameter,
            "flutes": d.flutes,
            "corner_radius": if d.kind == BULL_NOSE { d.corner_radius } else { 0.0 },
            "flute_length": if d.flute_length > 0.0 { json!(d.flute_length) } else { Value::Null },
            "shank_diameter": if d.shank_diameter > 0.0 { json!(d.shank_diameter) } else { Value::Null },
        });
        if d.has_point() {
            p["tip_angle"] = json!(d.tip_angle);
        }
        if d.id != 0 {
            p["id"] = json!(d.id);
        }
        if let Some(v) = run(app, w, TOOLS, "manufacture.tool.set", p, "Tool saved") {
            w.tool.id = u(&v["tool"], "id");
        }
    }
    ui.separator();
    ui.label(RichText::new("Saved tools").strong());
    note(ui, "Kept on this computer for every design. Add one to the design to use it.");
    let mut library_act: Option<(bool, String)> = None;
    match &w.library {
        Some(Ok(tools)) if tools.is_empty() => {
            ui.label(RichText::new("No saved tools yet: use Save to library on a tool above").color(t.text_dim));
        }
        Some(Ok(tools)) => {
            for tool in tools {
                ui.horizontal(|ui| {
                    ui.label(tool_line(tool).split_once("  ").map_or(String::new(), |(_, rest)| rest.to_string()));
                    if crate::ui_kit::button(ui, "Add to design", crate::ui_kit::Button::Small).clicked() {
                        library_act = Some((true, s(tool, "name")));
                    }
                    if crate::ui_kit::button(ui, "Remove", crate::ui_kit::Button::Small).clicked() {
                        library_act = Some((false, s(tool, "name")));
                    }
                });
            }
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(t.error));
        }
        None => {}
    }
    if let Some((add, name)) = library_act {
        let p = library_params(w, json!({"name": name}));
        if add {
            run(app, w, TOOLS, "manufacture.library.load", p, "Tool added to the design");
        } else if run(app, w, TOOLS, "manufacture.library.delete", p, "Removed from the tool library").is_some() {
            read_library(app, w);
        }
    }
}

// ------------------------------------------------------------------------------------ setup

/// The setup's work frame as the draft describes it: origin, X axis, Y axis, Z axis (model space).
fn draft_frame(d: &SetupDraft) -> [Vec3; 4] {
    let (x, y) = match d.custom {
        Some((x, y)) => (Vec3::new(x[0], x[1], x[2]), Vec3::new(y[0], y[1], y[2])),
        None => {
            let p = Plane::named(PLANES[d.plane.min(PLANES.len() - 1)]).unwrap_or(Plane::XY);
            (p.x, p.y)
        }
    };
    [Vec3::new(d.origin[0], d.origin[1], d.origin[2]), x, y, x.cross(y)]
}

/// A point in work coordinates in model space.
fn to_model(frame: &[Vec3; 4], p: Vec3) -> Vec3 {
    frame[0] + frame[1] * p.x + frame[2] * p.y + frame[3] * p.z
}

fn load_setup(x: &Value) -> SetupDraft {
    let frame = &x["frame"];
    let (xd, yd) = (vec3(&frame["x_dir"]), vec3(&frame["y_dir"]));
    let near = |a: Vec3, b: Vec3| a.dist(b) < 1e-6;
    let plane = PLANES.iter().position(|n| Plane::named(n).is_some_and(|p| near(p.x, xd) && near(p.y, yd)));
    SetupDraft {
        id: u(x, "id"),
        name: s(x, "name"),
        plane: plane.unwrap_or(0),
        custom: if plane.is_none() { Some((triple(&frame["x_dir"]), triple(&frame["y_dir"]))) } else { None },
        origin: triple(&frame["origin"]),
        has_stock: x.get("stock").is_some_and(|s| s.is_object()),
        stock_min: if x.get("stock").is_some() { triple(&x["stock"]["min"]) } else { [0.0, 0.0, -20.0] },
        stock_max: if x.get("stock").is_some() { triple(&x["stock"]["max"]) } else { [100.0, 100.0, 0.0] },
        work_offset: u(x, "work_offset").clamp(54, 59),
        clearance: f(x, "clearance"),
    }
}

fn setups_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    if std::mem::take(&mut w.setup_fresh)
        && w.setup.id == 0
        && let Some(first) = job["setups"].as_array().and_then(|a| a.first())
    {
        w.setup = load_setup(first);
    }
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
                w.setup = load_setup(x);
            }
        } else {
            run(app, w, SETUP, "manufacture.setup.delete", json!({"id": id}), "Setup deleted");
        }
    }
    ui.separator();
    let d = &mut w.setup;
    ui.label(RichText::new(if d.id == 0 { "New setup" } else { "Edit setup" }).strong());
    field_row(ui, "Name", |ui| crate::ui_kit::field(ui, &mut d.name));
    let before = d.plane;
    if d.custom.is_some() {
        note(ui, "This setup has a work frame that is none of XY, XZ or YZ; it is kept as it is.");
    } else {
        field_row(ui, "Work plane", |ui| crate::ui_kit::dropdown(ui, "mfg_setup_plane", &mut d.plane, &PLANES));
    }
    if d.plane != before {
        d.custom = None;
    }
    vec3_row(ui, "Zero point (model)", &mut d.origin);
    crate::ui_kit::checkbox(ui, "Stock", &mut d.has_stock);
    if d.has_stock {
        vec3_row(ui, "Stock min", &mut d.stock_min);
        vec3_row(ui, "Stock max", &mut d.stock_max);
        note(ui, "The stock is drawn in the model view as you type: a box from min to max, measured from the zero point.");
    }
    field_row(ui, "Work offset (G54 to G59)", |ui| crate::ui_kit::number_field(ui, &mut d.work_offset, 54..=59, 0.1, ""));
    num(ui, "Clearance height", &mut d.clearance, 0.0..=1.0e4, " mm");
    let mut save = false;
    ui.horizontal(|ui| {
        save = crate::ui_kit::button(ui, if d.id == 0 { "Add setup" } else { "Save setup" }, crate::ui_kit::Button::Normal).clicked();
        if d.id != 0 && crate::ui_kit::button(ui, "New setup", crate::ui_kit::Button::Normal).clicked() {
            let keep = (d.stock_min, d.stock_max, d.origin);
            *d = JobWindow::new().setup;
            (d.stock_min, d.stock_max, d.origin) = keep;
        }
    });
    if save {
        let d = w.setup.clone();
        let mut p = json!({
            "name": d.name,
            "work_offset": d.work_offset,
            "clearance": d.clearance,
            "stock": if d.has_stock { json!({"min": d.stock_min, "max": d.stock_max}) } else { Value::Null },
        });
        match d.custom {
            Some((x, y)) => p["frame"] = json!({"origin": d.origin, "x_dir": x, "y_dir": y}),
            None => {
                p["plane"] = json!(PLANES[d.plane.min(2)]);
                p["origin"] = json!(d.origin);
            }
        }
        if d.id != 0 {
            p["id"] = json!(d.id);
        }
        if let Some(v) = run(app, w, SETUP, "manufacture.setup.set", p, "Setup saved") {
            w.setup.id = u(&v["setup"], "id");
        }
    }
}

// -------------------------------------------------------------------------------- operations

fn sketch_names(app: &SolveApp) -> Vec<String> {
    app.session.model.state().sketches.iter().map(|s| s.name.clone()).collect()
}

fn load_operation(base: &OpDraft, x: &Value) -> OpDraft {
    let k = &x["kind"];
    let mut d = base.clone();
    d.id = u(x, "id");
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
    // A design from before the entry setting ramps when it has a ramp angle, else plunges.
    let ramp = k.get("ramp_angle").and_then(Value::as_f64).unwrap_or(3.0);
    d.entry = match k.get("entry").and_then(Value::as_str) {
        Some(_) => position(&ENTRY_IDS, k, "entry"),
        None if ramp > 0.0 => 1,
        None => 0,
    };
    d.ramp_angle = if ramp > 0.0 { ramp } else { 3.0 };
    d.helix_angle = k.get("helix_angle").and_then(Value::as_f64).unwrap_or(3.0);
    d.helix_diameter = f(k, "helix_diameter");
    d.top = f(x, "top");
    d.bottom = f(x, "bottom");
    d.retract = f(x, "retract");
    d.stepdown = f(x, "stepdown");
    d.spindle_rpm = f(x, "spindle_rpm");
    d.feed = f(x, "feed");
    d.plunge_feed = f(x, "plunge_feed");
    d.coolant = x["coolant"].as_bool().unwrap_or(false);
    d
}

fn operations_tab(app: &mut SolveApp, w: &mut JobWindow, ui: &mut egui::Ui, job: &Value) {
    let t = Tokens::get();
    crate::ui_kit::checkbox(ui, "Show toolpaths on the part", &mut w.show_paths);
    if w.show_paths {
        note(
            ui,
            "Cutting moves are blue, moves down into the material (plunge, ramp, helix) orange, rapids grey. The operation you are editing is drawn in full colour.",
        );
    }
    let mut act: Option<(u8, u32)> = None;
    for op in job["operations"].as_array().into_iter().flatten() {
        let id = u(op, "id");
        ui.horizontal(|ui| {
            let kind = match op["kind"]["type"].as_str() {
                Some("face") => OPS[0],
                Some("contour") => OPS[1],
                Some("pocket") => OPS[2],
                _ => OPS[3],
            };
            let marker = if w.focus_op == Some(id) { "\u{25B6} " } else { "" };
            ui.label(format!("{marker}{}  {}  {}  T{}  setup {}", id, s(op, "name"), crate::i18n::tr_label(kind), u(op, "tool"), u(op, "setup")));
            if crate::ui_kit::button(ui, "Edit", crate::ui_kit::Button::Small).clicked() {
                act = Some((0, id));
            }
            if crate::ui_kit::button(ui, "Check path", crate::ui_kit::Button::Small).clicked() {
                act = Some((1, id));
            }
            if crate::ui_kit::button(ui, "Delete", crate::ui_kit::Button::Small).clicked() {
                act = Some((2, id));
            }
        });
        if let Some(why) = w.paths.ops.iter().find(|p| p.id == id).and_then(|p| p.note.as_deref()) {
            ui.label(RichText::new(format!("No toolpath: {why}")).small().color(t.error));
        }
    }
    if job["operations"].as_array().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No operations yet").color(t.text_dim));
    }
    if let Some((what, id)) = act {
        match what {
            0 => {
                if let Some(x) = job["operations"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)) {
                    w.op = load_operation(&w.op, x);
                    w.focus_op = Some(id);
                }
            }
            1 => {
                w.focus_op = Some(id);
                if let Some(v) = run(app, w, OPERATIONS, "manufacture.toolpath", json!({"operation": id}), "") {
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
                    w.message = Some((OPERATIONS, text, false));
                }
            }
            _ => {
                if run(app, w, OPERATIONS, "manufacture.operation.delete", json!({"id": id}), "Operation deleted").is_some() && w.focus_op == Some(id)
                {
                    w.focus_op = None;
                }
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
    if matches!(d.kind, 1 | 2) {
        field_row(ui, "Entry into each depth", |ui| crate::ui_kit::dropdown(ui, "mfg_op_entry", &mut d.entry, &ENTRIES));
        match d.entry {
            1 => num(ui, "Ramp angle", &mut d.ramp_angle, 0.1..=45.0, " deg"),
            2 => {
                num(ui, "Helix angle", &mut d.helix_angle, 0.1..=45.0, " deg");
                num(ui, "Helix diameter (0: 90% of the tool)", &mut d.helix_diameter, 0.0..=1.0e3, " mm");
                note(ui, "A helix that does not fit the outline ramps instead; the plan says so.");
            }
            _ => {}
        }
    }
    num(ui, "Top of cut (work Z)", &mut d.top, -1.0e4..=1.0e4, " mm");
    num(ui, "Bottom of cut (work Z)", &mut d.bottom, -1.0e4..=1.0e4, " mm");
    num(ui, "Retract above top", &mut d.retract, 0.0..=1.0e3, " mm");
    if d.kind != 3 {
        num(ui, "Step down (0: half the tool)", &mut d.stepdown, 0.0..=1.0e3, " mm");
    }
    num(ui, "Spindle speed", &mut d.spindle_rpm, 1.0..=1.0e5, " rpm");
    num(ui, "Feed", &mut d.feed, 1.0..=1.0e5, " mm/min");
    num(ui, "Plunge / entry feed (0: automatic)", &mut d.plunge_feed, 0.0..=1.0e5, " mm/min");
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
        let entry =
            json!({"entry": ENTRY_IDS[d.entry.min(2)], "ramp_angle": d.ramp_angle, "helix_angle": d.helix_angle, "helix_diameter": d.helix_diameter});
        let with_entry = |mut kind: Value| {
            if let (Some(k), Some(e)) = (kind.as_object_mut(), entry.as_object()) {
                k.extend(e.iter().map(|(a, b)| (a.clone(), b.clone())));
            }
            kind
        };
        let kind = match d.kind {
            0 => json!({"type": "face", "margin": d.margin, "stepover": d.stepover}),
            1 => with_entry(
                json!({"type": "contour", "source": source, "side": SIDE_IDS[d.side.min(2)], "direction": DIRECTION_IDS[d.direction.min(1)], "allowance": d.allowance}),
            ),
            2 => with_entry(
                json!({"type": "pocket", "source": source, "direction": DIRECTION_IDS[d.direction.min(1)], "stepover": d.stepover, "allowance": d.allowance, "finish": d.finish}),
            ),
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
        p["plunge_feed"] = json!(d.plunge_feed.max(0.0));
        if let Some(v) = run(app, w, OPERATIONS, "manufacture.operation.set", p, "Operation saved") {
            let id = u(&v["operation"], "id");
            w.op.id = id;
            // The new toolpath shows on the part at once.
            w.focus_op = Some(id);
        }
    }
}

/// Plans every operation again for the toolpaths drawn over the model, when the design changed.
fn refresh_paths(app: &mut SolveApp, w: &mut JobWindow, job: &Value) {
    if w.paths.revision == Some(app.session.revision) {
        return;
    }
    let mut ops = Vec::new();
    for op in job["operations"].as_array().into_iter().flatten() {
        let id = u(op, "id");
        let setup = job["setups"].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == u(op, "setup")));
        let Some(setup) = setup else {
            ops.push(OpPath { id, segs: Vec::new(), note: Some("its setup is missing".into()) });
            continue;
        };
        let frame = [vec3(&setup["frame"]["origin"]), vec3(&setup["frame"]["x_dir"]), vec3(&setup["frame"]["y_dir"])];
        let frame = [frame[0], frame[1], frame[2], frame[1].cross(frame[2])];
        let params = json!({"operation": id, "moves": true, "expanded": true, "limit": 200_000, "tolerance": PREVIEW_TOLERANCE});
        match app.session.execute("manufacture.toolpath", &params) {
            Ok(v) => {
                let mut segs = Vec::new();
                let mut at: Option<Vec3> = None;
                for m in v["toolpath"].as_array().into_iter().flatten() {
                    let to = vec3(&m["to"]);
                    let rapid = m["move"] == "rapid";
                    if let Some(a) = at {
                        let kind = if rapid {
                            SegKind::Rapid
                        } else if to.z < a.z - 1e-9 {
                            SegKind::Entry
                        } else {
                            SegKind::Cut
                        };
                        segs.push(Seg { a: to_model(&frame, a), b: to_model(&frame, to), kind });
                    }
                    at = Some(to);
                }
                ops.push(OpPath { id, segs, note: None });
            }
            Err(e) => ops.push(OpPath { id, segs: Vec::new(), note: Some(e.to_string()) }),
        }
    }
    w.paths = Paths { revision: Some(app.session.revision), ops };
}

// --------------------------------------------------------------------------------------- G-code

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
        if let Some(v) = run(app, w, GCODE, "manufacture.post", p, "G-code ready") {
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
            run(app, w, GCODE, "manufacture.post", p, "Program saved");
        }
    }
}

// -------------------------------------------------------------------------------------- windows

/// Draw the windows that are open. Called once per frame.
/// What the history strip and the side panel hand to the windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Tool,
    Setup,
    Operation,
}

/// The operation shown in full colour on the part, if any.
pub fn focused_operation(app: &SolveApp) -> Option<u32> {
    app.manufacture_job.as_ref().and_then(|w| w.focus_op)
}

/// Open the window of `item` with the entry `id` loaded for editing; with id 0 a new one.
pub fn pick(app: &mut SolveApp, item: Item, id: u32) {
    let tab = match item {
        Item::Tool => TOOLS,
        Item::Setup => SETUP,
        Item::Operation => OPERATIONS,
    };
    open(app, tab);
    let job = app.session.execute("manufacture.job", &json!({})).unwrap_or(Value::Null);
    let find = |key: &str| job[key].as_array().and_then(|a| a.iter().find(|x| u(x, "id") == id)).cloned();
    let Some(w) = app.manufacture_job.as_mut() else { return };
    match item {
        Item::Tool => w.tool = find("tools").map_or_else(ToolDraft::new, |x| load_tool(&x)),
        Item::Setup => {
            w.setup_fresh = false;
            w.setup = find("setups").map_or_else(|| JobWindow::new().setup, |x| load_setup(&x));
        }
        Item::Operation => {
            match find("operations") {
                Some(x) => w.op = load_operation(&w.op, &x),
                None => w.op.id = 0,
            }
            w.focus_op = (id != 0).then_some(id);
            w.paths.revision = None;
        }
    }
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut w) = app.manufacture_job.take() else { return };
    if w.open.iter().any(|o| *o) {
        let t = Tokens::get();
        let job = app.session.execute("manufacture.job", &json!({})).unwrap_or(Value::Null);
        if w.open[OPERATIONS] && w.show_paths {
            refresh_paths(app, &mut w, &job);
        }
        #[allow(clippy::needless_range_loop)] // the index is also the tab id
        for tab in 0..TABS.len() {
            if !w.open[tab] {
                continue;
            }
            let mut open = true;
            // Side by side from the left, so opening a second window does not hide the first.
            let screen = ctx.content_rect();
            let at = screen.left_top() + egui::vec2(120.0 + 40.0 * tab as f32, 80.0 + 24.0 * tab as f32);
            let room = (screen.height() - 160.0 - 24.0 * tab as f32).clamp(200.0, 900.0);
            let closed = crate::ui_kit::Window::new(egui::Id::new(("sc_manufacture_window", tab)), "gear", crate::i18n::tr_label(TABS[tab]))
                .cards(2.0)
                .tall(room)
                .placed(egui::Align2::LEFT_TOP, at)
                .show_body(ctx, |ui| {
                    if let Some((from, m, error)) = &w.message
                        && *from == tab
                        && !m.is_empty()
                    {
                        ui.label(RichText::new(m).color(if *error { t.error } else { t.text_dim }));
                    }
                    // The window opens tall, and its own body scrolls when a tab is taller than the screen.
                    match tab {
                        TOOLS => tools_tab(app, &mut w, ui, &job),
                        SETUP => setups_tab(app, &mut w, ui, &job),
                        OPERATIONS => operations_tab(app, &mut w, ui, &job),
                        _ => program_tab(app, &mut w, ui),
                    }
                });
            if closed {
                open = false;
            }
            if !open {
                w.open[tab] = false;
            }
        }
    }
    app.manufacture_job = Some(w);
}

// ------------------------------------------------------------------------------ over the model

/// The stock box of the Setup window and the toolpaths of the Operations window, drawn over the
/// viewport with the viewport's own camera.
pub fn paint(app: &SolveApp, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let Some(w) = &app.manufacture_job else { return };
    if w.open[SETUP] && w.setup.has_stock {
        paint_stock_box(&w.setup, painter, proj);
    }
    // The simulation window draws the path itself.
    if w.open[OPERATIONS] && w.show_paths && app.manufacture.is_none() {
        paint_paths(w, painter, proj);
    }
}

/// The eight corners of the stock box in model space, numbered by bits: 1 for the larger X,
/// 2 for the larger Y, 4 for the larger Z. None while the numbers are not finite.
fn stock_corners(d: &SetupDraft) -> Option<[Vec3; 8]> {
    let frame = draft_frame(d);
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for i in 0..3 {
        let (a, b) = (d.stock_min[i], d.stock_max[i]);
        if !(a.is_finite() && b.is_finite() && d.origin[i].is_finite()) {
            return None;
        }
        // A box typed with min above max is still a box, drawn between the two.
        (lo[i], hi[i]) = (a.min(b), a.max(b));
    }
    let corner = |i: usize| {
        let pick = |bit: usize, k: usize| if i & bit == 0 { lo[k] } else { hi[k] };
        to_model(&frame, Vec3::new(pick(1, 0), pick(2, 1), pick(4, 2)))
    };
    Some(std::array::from_fn(corner))
}

fn paint_stock_box(d: &SetupDraft, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let t = Tokens::get();
    let Some(corners) = stock_corners(d) else { return };
    let screen: Vec<Option<Pos2>> = corners.iter().map(|c| proj.to_screen(*c)).collect();
    const FACES: [[usize; 4]; 6] = [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]];
    let (eye, dir) = proj.ray(proj.rect.center());
    let mut faces: Vec<(f64, [Pos2; 4])> = Vec::new();
    for face in FACES {
        let pts: Vec<Pos2> = face.iter().filter_map(|i| screen.get(*i).copied().flatten()).collect();
        if let Ok(pts) = <[Pos2; 4]>::try_from(pts) {
            let centre = face.iter().filter_map(|i| corners.get(*i).copied()).fold(Vec3::ZERO, |a, c| a + c) * 0.25;
            faces.push(((centre - eye).dot(dir), pts));
        }
    }
    // Far faces first, so the near ones tint what is behind them.
    faces.sort_by(|a, b| b.0.total_cmp(&a.0));
    let fill = t.accent.gamma_multiply(0.14);
    for (_, pts) in &faces {
        painter.add(Shape::convex_polygon(pts.to_vec(), fill, Stroke::NONE));
    }
    let edge = Stroke::new(1.5, t.accent);
    for i in 0..8usize {
        for bit in [1usize, 2, 4] {
            if i & bit == 0
                && let (Some(Some(a)), Some(Some(b))) = (screen.get(i), screen.get(i | bit))
            {
                painter.line_segment([*a, *b], edge);
            }
        }
    }
    // The zero point the numbers are measured from, with the work axes.
    let frame = draft_frame(d);
    let span = (0..3).map(|k| (d.stock_max[k] - d.stock_min[k]).abs()).fold(0.0, f64::max).max(10.0) * 0.25;
    if let Some(o) = proj.to_screen(frame[0]) {
        for (axis, colour) in [(frame[1], t.axis_x), (frame[2], t.axis_y), (frame[3], t.axis_z)] {
            if let Some(p) = proj.to_screen(frame[0] + axis * span) {
                painter.line_segment([o, p], Stroke::new(2.0, colour));
            }
        }
    }
    let size = |k: usize| (d.stock_max[k] - d.stock_min[k]).abs();
    if let Some(Some(p)) = screen.get(7) {
        painter.text(
            *p + egui::vec2(8.0, -6.0),
            Align2::LEFT_BOTTOM,
            format!("Stock {:.1} \u{D7} {:.1} \u{D7} {:.1} mm", size(0), size(1), size(2)),
            egui::FontId::proportional(t.font_small),
            t.text,
        );
    }
}

fn paint_paths(w: &JobWindow, painter: &egui::Painter, proj: &crate::viewport::Proj) {
    let t = Tokens::get();
    let total: usize = w.paths.ops.iter().map(|o| o.segs.len()).sum();
    let stride = (total / MAX_SEGMENTS).max(1);
    // The operation being edited last, so it lies over the others.
    let (others, focused): (Vec<&OpPath>, Vec<&OpPath>) = w.paths.ops.iter().partition(|o| w.focus_op.is_some_and(|f| f != o.id));
    for (dim, group) in [(true, others), (false, focused)] {
        let fade = if dim { 0.4 } else { 1.0 };
        for op in group {
            for (i, seg) in op.segs.iter().enumerate() {
                // Thinning keeps every move that goes down: they are few and the point of the picture.
                if i % stride != 0 && seg.kind != SegKind::Entry {
                    continue;
                }
                let (Some(a), Some(b)) = (proj.to_screen(seg.a), proj.to_screen(seg.b)) else { continue };
                let stroke = match seg.kind {
                    SegKind::Rapid => Stroke::new(0.8, t.text_dim.gamma_multiply(0.6 * fade)),
                    SegKind::Entry => Stroke::new(1.8, t.warning.gamma_multiply(fade)),
                    SegKind::Cut => Stroke::new(1.4, t.accent.gamma_multiply(fade)),
                };
                painter.line_segment([a, b], stroke);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::scenario::Harness;

    fn run_cmd(h: &mut Harness, id: &str, p: serde_json::Value) -> serde_json::Value {
        h.app.run(id, p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    fn window(h: &Harness) -> &JobWindow {
        h.app.manufacture_job.as_ref().expect("the windows' state")
    }

    /// A block with a pocket sketch drawn on it, a tool and a setup with stock.
    fn block_with_pocket(h: &mut Harness) {
        run_cmd(h, "sketch.create", json!({"plane": "XY", "name": "Plate"}));
        run_cmd(h, "sketch.rectangle.two_point", json!({"p0": [0.0, 0.0], "p1": [40.0, 30.0]}));
        run_cmd(h, "sketch.finish", json!({}));
        run_cmd(h, "solid.extrude", json!({"distance": 10.0}));
        run_cmd(h, "sketch.create", json!({"plane": "XY", "name": "Pocket"}));
        run_cmd(h, "sketch.rectangle.two_point", json!({"p0": [10.0, 8.0], "p1": [30.0, 22.0]}));
        run_cmd(h, "sketch.finish", json!({}));
        run_cmd(h, "manufacture.tool.set", json!({"id": 1, "diameter": 4.0}));
        run_cmd(
            h,
            "manufacture.setup.set",
            json!({"id": 1, "plane": "XY", "origin": [0.0, 0.0, 10.0], "stock": {"min": [-5.0, -5.0, -10.0], "max": [45.0, 35.0, 3.0]}}),
        );
    }

    fn pocket_op(entry: serde_json::Value) -> serde_json::Value {
        let mut kind = json!({"type": "pocket", "source": {"sketch": "Pocket"}, "stepover": 0.5});
        for (k, v) in entry.as_object().unwrap() {
            kind[k] = v.clone();
        }
        json!({"setup": 1, "tool": 1, "kind": kind, "top": 0.0, "bottom": -4.0, "feed": 500.0, "spindle_rpm": 8000.0})
    }

    #[test]
    fn the_window_adds_a_tool_through_the_command() {
        let mut h = Harness::new();
        super::open(&mut h.app, TOOLS);
        h.frames(2);
        assert!(h.widget("Add tool").is_some());
        h.step(&json!({"widget": "Add tool"})).unwrap();
        h.frames(2);
        let job = h.app.session.doc.manufacture.clone();
        assert_eq!(job.tools.len(), 1, "the click ran manufacture.tool.set");
    }

    #[test]
    fn a_refused_operation_keeps_the_reason_for_the_window() {
        let mut h = Harness::new();
        super::open(&mut h.app, OPERATIONS);
        let mut w = h.app.manufacture_job.take().unwrap();
        let p = json!({"setup": 1, "tool": 1, "kind": {"type": "face"}, "top": 0.0, "bottom": -1.0, "feed": 100.0, "spindle_rpm": 1000.0});
        let out = super::run(&mut h.app, &mut w, OPERATIONS, "manufacture.operation.set", p, "Operation saved");
        assert!(out.is_none(), "no tool or setup yet");
        assert!(matches!(&w.message, Some((OPERATIONS, m, true)) if !m.is_empty()), "{:?}", w.message);
    }

    #[test]
    fn each_tab_has_a_window_of_its_own() {
        let mut h = Harness::new();
        super::open(&mut h.app, TOOLS);
        super::open(&mut h.app, OPERATIONS);
        h.frames(3);
        assert_eq!(window(&h).open, [true, false, true, false]);
        // Two windows at once, each with its own controls and none of the others'.
        assert!(h.widget("Add tool").is_some() && h.widget("Add operation").is_some());
        assert!(h.widget("Add setup").is_none() && h.widget("Generate G-code").is_none());
        super::open(&mut h.app, GCODE);
        super::open(&mut h.app, SETUP);
        h.frames(3);
        assert_eq!(window(&h).open, [true, true, true, true]);
        for label in ["Add tool", "Add setup", "Add operation", "Generate G-code"] {
            assert!(h.widget(label).is_some(), "{label}");
        }
        // Opening one again leaves the others as they are.
        super::open(&mut h.app, TOOLS);
        assert_eq!(window(&h).open, [true; 4]);
    }

    #[test]
    fn the_tool_types_offer_what_each_one_needs() {
        let mut d = ToolDraft::new();
        d.diameter = 10.0;
        d.use_kind(4);
        assert_eq!((KIND_IDS[d.kind], d.flutes, d.tip_angle), ("spot_drill", 2, 90.0));
        assert!(d.has_point());
        d.use_kind(2);
        assert!(!d.has_point() && d.corner_radius > 0.0 && d.corner_radius <= 5.0);
        d.use_kind(7);
        assert_eq!((KIND_IDS[d.kind], d.flutes, d.corner_radius), ("face_mill", 5, 0.0));
        d.use_kind(5);
        assert_eq!((d.flutes, d.has_point()), (6, false));
        assert_eq!(KINDS.len(), KIND_IDS.len());
        assert_eq!(KIND_FLUTES.len(), KINDS.len());
        assert_eq!(KIND_TIP.len(), KINDS.len());
    }

    #[test]
    fn every_tool_type_can_be_saved_from_the_window_and_edited() {
        let mut h = Harness::new();
        // Tall enough that the whole window is on the screen: a click cannot reach a button below it.
        h.call("ui.resize", json!({"width": 1600, "height": 3000}));
        super::open(&mut h.app, TOOLS);
        h.frames(2);
        for kind in 0..KINDS.len() {
            let w = h.app.manufacture_job.as_mut().unwrap();
            w.tool = ToolDraft::new();
            w.tool.use_kind(kind);
            w.tool.name = format!("Tool of kind {kind}");
            w.tool.diameter = 8.0;
            if kind == 2 {
                w.tool.corner_radius = 1.0;
            }
            h.frames(6);
            h.step(&json!({"widget": "Add tool"})).unwrap();
            h.frames(6);
        }
        let tools = &h.app.session.doc.manufacture.tools;
        assert_eq!(tools.len(), 8);
        let job = run_cmd(&mut h, "manufacture.job", json!({}));
        let kinds: Vec<&str> = job["tools"].as_array().unwrap().iter().map(|t| t["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, KIND_IDS);
        // Editing a tool through the window changes that tool.
        let w = h.app.manufacture_job.as_mut().unwrap();
        w.tool = load_tool(&job["tools"][4]);
        assert_eq!((w.tool.id, w.tool.tip_angle), (5, 90.0));
        w.tool.diameter = 12.0;
        w.tool.shank_diameter = 10.0;
        h.frames(2);
        h.step(&json!({"widget": "Save tool"})).unwrap();
        h.frames(2);
        let job = run_cmd(&mut h, "manufacture.job", json!({}));
        assert_eq!(job["tools"][4]["diameter"], 12.0);
        assert_eq!(job["tools"][4]["shank_diameter"], 10.0);
        assert_eq!(job["tools"].as_array().unwrap().len(), 8);
        // Clearing the shank diameter takes it off the tool.
        let w = h.app.manufacture_job.as_mut().unwrap();
        w.tool.shank_diameter = 0.0;
        h.frames(2);
        h.step(&json!({"widget": "Save tool"})).unwrap();
        h.frames(2);
        let job = run_cmd(&mut h, "manufacture.job", json!({}));
        assert!(job["tools"][4].get("shank_diameter").is_none());
    }

    #[test]
    fn tools_are_saved_to_the_library_from_the_window_and_added_back() {
        let dir = std::env::temp_dir().join(format!("solidcraft-ui-tool-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("tools.json").to_string_lossy().into_owned();
        let mut h = Harness::new();
        // Tall enough that the whole window is on the screen: a click cannot reach a button below it.
        h.call("ui.resize", json!({"width": 1600, "height": 3000}));
        run_cmd(&mut h, "manufacture.tool.set", json!({"id": 2, "name": "My ball mill", "kind": "ball_end_mill", "diameter": 6.0}));
        super::open(&mut h.app, TOOLS);
        h.app.manufacture_job.as_mut().unwrap().library_path = file.clone();
        h.frames(3);
        assert!(h.widget("Save to library").is_some());
        h.step(&json!({"widget": "Save to library"})).unwrap();
        h.frames(3);
        assert!(matches!(&window(&h).library, Some(Ok(t)) if t.len() == 1), "{:?}", window(&h).message);
        assert!(h.widget("Add to design").is_some(), "the saved tool is listed");
        // A new design starts without it and takes it from the library.
        run_cmd(&mut h, "manufacture.tool.delete", json!({"id": 2}));
        assert!(h.app.session.doc.manufacture.tools.is_empty());
        h.step(&json!({"widget": "Add to design"})).unwrap();
        h.frames(2);
        let tools = &h.app.session.doc.manufacture.tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "My ball mill");
        h.step(&json!({"widget": "Remove"})).unwrap();
        h.frames(2);
        assert!(matches!(&window(&h).library, Some(Ok(t)) if t.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_that_cannot_be_read_says_so() {
        let mut h = Harness::new();
        super::open(&mut h.app, TOOLS);
        let dir = std::env::temp_dir().join(format!("solidcraft-ui-bad-library-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tools.json");
        std::fs::write(&file, "not a library").unwrap();
        h.app.manufacture_job.as_mut().unwrap().library_path = file.to_string_lossy().into_owned();
        h.frames(3);
        assert!(matches!(&window(&h).library, Some(Err(e)) if !e.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_setup_window_draws_the_stock_where_the_numbers_put_it() {
        let mut d = JobWindow::new().setup;
        d.origin = [10.0, 20.0, 30.0];
        d.stock_min = [0.0, 0.0, -5.0];
        d.stock_max = [40.0, 30.0, 2.0];
        // Work plane XY: work coordinates are model coordinates shifted by the zero point.
        let c = stock_corners(&d).unwrap();
        assert!(c[0].dist(Vec3::new(10.0, 20.0, 25.0)) < 1e-9);
        assert!(c[7].dist(Vec3::new(50.0, 50.0, 32.0)) < 1e-9);
        assert!(
            c[1].dist(Vec3::new(50.0, 20.0, 25.0)) < 1e-9
                && c[2].dist(Vec3::new(10.0, 50.0, 25.0)) < 1e-9
                && c[4].dist(Vec3::new(10.0, 20.0, 32.0)) < 1e-9
        );
        // Front plane: work Z is model +Y, work Y is model -Z.
        d.plane = 1;
        let c = stock_corners(&d).unwrap();
        assert!(c[0].dist(Vec3::new(10.0, 20.0 - 5.0, 30.0)) < 1e-9, "{:?}", c[0]);
        assert!(c[2].dist(Vec3::new(10.0, 15.0, 0.0 + 30.0 - 30.0)) < 1e-9, "{:?}", c[2]);
        // Min above max still makes a box; a number that is not finite makes none.
        d.plane = 0;
        d.stock_min = [40.0, 30.0, 2.0];
        d.stock_max = [0.0, 0.0, -5.0];
        assert!(stock_corners(&d).is_some());
        d.stock_max[0] = f64::NAN;
        assert!(stock_corners(&d).is_none());
    }

    #[test]
    fn the_setup_window_starts_on_the_setup_the_design_has() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        super::open(&mut h.app, SETUP);
        h.frames(3);
        let d = &window(&h).setup;
        assert_eq!(d.id, 1);
        assert_eq!(d.origin, [0.0, 0.0, 10.0]);
        assert_eq!((d.stock_min, d.stock_max), ([-5.0, -5.0, -10.0], [45.0, 35.0, 3.0]));
        assert!(d.custom.is_none() && d.has_stock);
        // Editing the numbers moves the drawn box before anything is saved.
        let w = h.app.manufacture_job.as_mut().unwrap();
        w.setup.stock_max = [100.0, 50.0, 3.0];
        let c = stock_corners(&w.setup).unwrap();
        assert!(c[7].dist(Vec3::new(100.0, 50.0, 13.0)) < 1e-9);
        assert!(h.app.session.doc.manufacture.setups[0].stock.unwrap().max == [45.0, 35.0, 3.0]);
        // A setup with no stock is saved without it.
        let w = h.app.manufacture_job.as_mut().unwrap();
        w.setup.has_stock = false;
        h.frames(2);
        h.step(&json!({"widget": "Save setup"})).unwrap();
        h.frames(2);
        assert!(h.app.session.doc.manufacture.setups[0].stock.is_none());
        let w = h.app.manufacture_job.as_ref().unwrap();
        assert!(w.open[SETUP]);
    }

    #[test]
    fn an_operation_made_in_the_window_shows_its_toolpath_without_simulating() {
        let mut h = Harness::new();
        h.call("ui.resize", json!({"width": 1600, "height": 3000}));
        block_with_pocket(&mut h);
        super::open(&mut h.app, OPERATIONS);
        h.app.manufacture_job.as_mut().unwrap().op = {
            let mut d = JobWindow::new().op;
            d.kind = 2;
            d.sketch = "Pocket".into();
            d.tool = 1;
            d.setup = 1;
            d.top = 0.0;
            d.bottom = -4.0;
            d.entry = 2;
            d.helix_angle = 4.0;
            d
        };
        h.frames(8);
        assert!(window(&h).paths.ops.is_empty());
        h.step(&json!({"widget": "Add operation"})).unwrap();
        h.frames(8);
        assert_eq!(h.app.session.doc.manufacture.operations.len(), 1, "{:?}", window(&h).message);
        let w = window(&h);
        assert_eq!(w.focus_op, Some(1), "the new operation is the one in full colour");
        let path = w.paths.ops.iter().find(|p| p.id == 1).expect("its toolpath is planned");
        assert!(path.note.is_none(), "{:?}", path.note);
        assert!(path.segs.iter().any(|s| s.kind == SegKind::Cut) && path.segs.iter().any(|s| s.kind == SegKind::Rapid));
        assert!(path.segs.iter().filter(|s| s.kind == SegKind::Entry).count() > 20, "the helix is drawn as the moves that go down");
        assert!(h.app.manufacture.is_none(), "no simulation is running");
        // The path is in model space: the pocket is at work Z 0 to -4, which is model Z 10 to 6.
        let (lo, hi) = path.segs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), s| (lo.min(s.b.z), hi.max(s.b.z)));
        assert!(lo >= 6.0 - 1e-6 && hi >= 10.0, "{lo} {hi}");
        // It saved the helical entry.
        let k = &run_cmd(&mut h, "manufacture.job", json!({}))["operations"][0]["kind"];
        assert_eq!((k["entry"].as_str(), k["helix_angle"].as_f64()), (Some("helical"), Some(4.0)));
        // Changing the operation plans its path again; hiding the paths stops the work.
        run_cmd(&mut h, "manufacture.operation.set", json!({"id": 1, "kind": pocket_op(json!({"entry": "plunge"}))["kind"]}));
        h.frames(3);
        let entries = |h: &Harness| window(h).paths.ops[0].segs.iter().filter(|s| s.kind == SegKind::Entry).count();
        let plunge = entries(&h);
        assert!(plunge < 20, "a plunge goes down in a few straight moves: {plunge}");
        h.app.manufacture_job.as_mut().unwrap().show_paths = false;
        run_cmd(&mut h, "manufacture.operation.set", json!({"id": 1, "kind": pocket_op(json!({"entry": "helical"}))["kind"]}));
        h.frames(3);
        assert_eq!(entries(&h), plunge, "paths that are not shown are not planned");
    }

    #[test]
    fn an_operation_that_cannot_be_planned_says_why_in_the_list() {
        let mut h = Harness::new();
        block_with_pocket(&mut h);
        run_cmd(&mut h, "manufacture.operation.set", pocket_op(json!({})));
        // A tool wider than the pocket leaves nothing to cut.
        run_cmd(&mut h, "manufacture.tool.set", json!({"id": 1, "diameter": 40.0}));
        super::open(&mut h.app, OPERATIONS);
        h.frames(3);
        let path = window(&h).paths.ops.iter().find(|p| p.id == 1).unwrap();
        assert!(path.segs.is_empty() && path.note.as_deref().is_some_and(|n| !n.is_empty()), "{:?}", path.note);
    }

    #[test]
    fn a_setup_with_a_tilted_frame_keeps_it_when_edited() {
        let mut h = Harness::new();
        run_cmd(
            &mut h,
            "manufacture.setup.set",
            json!({"id": 1, "frame": {"origin": [0.0, 0.0, 0.0], "x_dir": [0.0, 1.0, 0.0], "y_dir": [1.0, 0.0, 0.0]}, "stock": {"min": [0.0, 0.0, -5.0], "max": [10.0, 10.0, 0.0]}}),
        );
        super::open(&mut h.app, SETUP);
        h.frames(3);
        assert!(window(&h).setup.custom.is_some(), "a frame that is none of the three planes is kept");
        h.app.manufacture_job.as_mut().unwrap().setup.name = "Renamed".into();
        h.step(&json!({"widget": "Save setup"})).unwrap();
        h.frames(2);
        let setup = &h.app.session.doc.manufacture.setups[0];
        assert_eq!(setup.name, "Renamed");
        assert!(setup.frame.x.dist(Vec3::new(0.0, 1.0, 0.0)) < 1e-9 && setup.frame.y.dist(Vec3::new(1.0, 0.0, 0.0)) < 1e-9);
    }
}
