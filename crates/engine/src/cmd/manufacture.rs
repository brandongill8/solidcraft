//! Manufacturing: the design's tool library, setups and operations, and the toolpaths and G-code
//! they make (docs/manufacture.md). The job lives in the design (`manufacture`), so it saves,
//! reopens and undoes with it. The planning itself is `solidcraft_manufacture`; these commands
//! resolve sketches into work-coordinate outlines and keep the job consistent.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use solidcraft_doc::{FeatureKind, ProfileSel, select_profiles};
use solidcraft_geom::{Plane, Vec3};
use solidcraft_manufacture as mf;

use super::CommandSpec;
use crate::params::bad;
use crate::{Result, Session};

/// Largest G-code text returned in a reply (bytes); more goes to a file with `path`.
const MAX_INLINE_GCODE: usize = 4 << 20;
/// Most moves listed in a toolpath reply.
const MAX_LISTED_MOVES: usize = 5000;
/// Most moves a `limit` can ask for.
const MAX_LISTED_MOVES_HARD: usize = 200_000;

pub static COMMANDS: &[CommandSpec] = &[
CommandSpec::new("manufacture.job", "Manufacturing Job", job).icon("list").noundo().doc_only()
    .params("No parameters. Returns {tools, setups, operations}: the design's tool library, setups (work frame, stock, work offset, clearance height) and operations. See docs/manufacture.md"),
CommandSpec::new("manufacture.tool.set", "Set Tool", tool_set).icon("hole").doc_only().at("MANUFACTURE", "TOOLS")
    .params("id?: tool number (new when omitted or unused; the controller's T number), name?, kind?: flat_end_mill|ball_end_mill|bull_nose_end_mill|drill|spot_drill|reamer|shell_mill|face_mill, diameter (mm), flutes?: 2, corner_radius?: 0 (bull nose), tip_angle?: 118 (drills; 90 is usual for a spot drill), flute_length? and shank_diameter? (mm; the simulation reports the shank touching stock or the part). Updates only the fields given. Drills, spot drills and reamers work along their axis (drill operations); the others mill. One undo step."),
CommandSpec::new("manufacture.tool.delete", "Delete Tool", tool_delete).icon("delete").doc_only()
    .params("id: tool number. Refused while an operation uses it (they are listed)."),
CommandSpec::new("manufacture.setup.set", "Set Setup", setup_set).icon("gear").doc_only().at("MANUFACTURE", "SETUPS")
    .params("id?: setup number (new when omitted or unused), name?, plane?: XY|XZ|YZ (the work frame's plane), origin?: [x, y, z] (model space), frame?: {origin, x_dir, y_dir} (a full frame; Z = x_dir × y_dir is the tool axis), work_offset?: 54-59 (G54-G59), stock?: {min: [x, y, z], max: [x, y, z]} in work coordinates, clearance?: 10 (work Z the tool travels at between features). Updates only the fields given."),
CommandSpec::new("manufacture.setup.delete", "Delete Setup", setup_delete).icon("delete").doc_only()
    .params("id: setup number. Refused while an operation is in it."),
CommandSpec::new("manufacture.operation.set", "Set Operation", operation_set).icon("extrude").doc_only().at("MANUFACTURE", "OPERATIONS")
    .params("id?: operation number (new when omitted or unused), name?, setup, tool, kind: {type: face, margin?: 0, stepover?: 0.6} | {type: contour, source: {sketch, profiles?: [indices]}, side?: outside|inside|on, direction?: climb|conventional, allowance?: 0, entry?: plunge|ramp|helical, ramp_angle?: 3, helix_angle?: 3, helix_diameter?: 0} | {type: pocket, source, direction?, stepover?: 0.4, allowance?: 0, finish?: false, entry?, ramp_angle?: 3, helix_angle?: 3, helix_diameter?: 0} | {type: drill, points: {from: sketch, sketch, profiles?} | {from: points, points: [[x, y]]}, peck?, dwell?: 0}; top (work Z of the surface, mm), bottom (work Z of the deepest cut), retract?: 2 (feed plane above top), stepdown? (default half the tool diameter), spindle_rpm, feed (mm/min), plunge_feed? (a plunge runs at it, half the feed when omitted; a ramped or helical entry runs at it too, at the cutting feed when omitted), coolant?: false. `entry` is how each depth is entered: plunge straight down; ramp along the outline at `ramp_angle` degrees (1-45); helical in a circle of `helix_diameter` mm (0: 90% of the tool diameter) at `helix_angle` degrees (a circle that does not fit ramps instead and the plan warns). Without `entry`, a ramp_angle above 0 ramps and 0 plunges. `sketch` may be an id or a name. Updates only the fields given. Checked against the tool and setup. See docs/manufacture.md"),
CommandSpec::new("manufacture.operation.delete", "Delete Operation", operation_delete).icon("delete").doc_only()
    .params("id: operation number."),
CommandSpec::new("manufacture.toolpath", "Plan Toolpath", toolpath).icon("measure").noundo().at("MANUFACTURE", "OPERATIONS")
    .params("operation: number, moves?: false (list the moves; `limit`: at most 5000 by default, 200000 at most; `expanded`: drilling cycles as plain moves, as `manufacture.simulate` numbers them), tolerance?: 0.005 (chord error of curves, mm), rapid_rate?: 10000 (mm/min, for the time estimate). Returns {operation, kind, moves, stats: {rapid_length, feed_length, minutes, min, max}, warnings}. Does not change the design."),
CommandSpec::new("manufacture.post", "Post Process", post).icon("export").noundo().at("MANUFACTURE", "OUTPUT")
    .params("dialect: grbl|fanuc|haas, operations?: [numbers] (all, in order), program_name?, program_number?: 1001 (Fanuc/Haas O number), line_numbers?: false, map_steps?: false (also return `step_lines`: the program line each move of the simulation runs from), tolerance?: 0.005, path?: write the program there (an existing file is kept unless overwrite: true), overwrite?: false. Returns {dialect, operations, lines, bytes, warnings} and the program text as `gcode` unless it was written to `path`. Does not change the design."),
CommandSpec::new("manufacture.simulate", "Simulate Machining", simulate).icon("play").noundo().at("MANUFACTURE", "OUTPUT")
    .params("setup?: number (the first setup), operations?: [numbers] (all in the setup, in order), resolution?: 0.5 (cell size, mm), tolerance? (how deep an overlap counts as a collision, mm; default one cell, at least 0.05), until?: move number to stop after, part?: true (check against the model's bodies; false: stock only), bodies?: [names] (default all), surface?: max cells of the stock surface to return for drawing (0/omitted: none; at most 250000), tolerance_chord?: 0.005. Cuts the setup's stock away along the toolpaths (a height map) and reports gouges into the model, shank hits (tool.flute_length), rapids through stock or the model, and cuts below the stock, with volumes removed and left over. Returns {setup, frame, grid, steps, total_steps, operations: [{operation, first_step}], volumes, counts, collisions, open_cells, tolerance, surface?}. Does not change the design. See docs/manufacture.md"),
];

fn job(s: &mut Session, _p: &Value) -> Result<Value> {
    serde_json::to_value(&*s.doc.manufacture).map_err(|e| bad("manufacture.job", e.to_string()))
}

// ------------------------------------------------------------------------------------- helpers

pub(super) fn object<'a>(p: &'a Value, cmd: &str) -> Result<&'a Map<String, Value>> {
    p.as_object().ok_or_else(|| bad(cmd, "the parameters must be an object"))
}

pub(super) fn number_id(v: &Value, what: &str, cmd: &str) -> Result<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).ok_or_else(|| bad(cmd, format!("`{what}` must be a whole number of 1 or more")))
}

pub(super) fn required_id(p: &Value, key: &str, cmd: &str) -> Result<u32> {
    number_id(p.get(key).ok_or_else(|| bad(cmd, format!("`{key}` is required")))?, key, cmd)
}

/// One past the largest id; ids come from the design file, so one past the largest a `u64` holds is
/// an error, not an overflow.
fn next_free_id(cmd: &str, ids: impl Iterator<Item = u64>) -> Result<u64> {
    ids.max().unwrap_or(0).checked_add(1).ok_or_else(|| bad(cmd, "this design has no ids left to give"))
}

pub(super) fn new_id(cmd: &str, ids: impl Iterator<Item = u32>) -> Result<u32> {
    u32::try_from(next_free_id(cmd, ids.map(u64::from))?).map_err(|_| bad(cmd, "no numbers left to give"))
}

/// `base` (or nothing) with the fields of `p` laid over it, as a `T`. Unknown fields are
/// refused by `T`, so a misspelt one is an error, not a silent no-op.
fn patched<T: Serialize + DeserializeOwned>(base: Option<&T>, id: u32, p: &Map<String, Value>, cmd: &str) -> Result<T> {
    let mut m = match base {
        Some(b) => match serde_json::to_value(b) {
            Ok(Value::Object(m)) => m,
            _ => return Err(bad(cmd, "the stored item cannot be read")),
        },
        None => Map::new(),
    };
    for (k, v) in p {
        m.insert(k.clone(), v.clone());
    }
    m.insert("id".into(), json!(id));
    serde_json::from_value(Value::Object(m)).map_err(|e| bad(cmd, e.to_string()))
}

/// Checks the job and stores it, unless it is what the design already has.
pub(super) fn store(s: &mut Session, job: mf::Job, cmd: &str) -> Result<()> {
    job.validate().map_err(|e| bad(cmd, e.to_string()))?;
    if *s.doc.manufacture != job {
        s.doc_mut().manufacture = Arc::new(job);
    }
    Ok(())
}

pub(super) fn job_of(s: &Session) -> mf::Job {
    (*s.doc.manufacture).clone()
}

pub(super) fn upsert<T>(list: &mut Vec<T>, item: T, same: impl Fn(&T) -> bool) {
    match list.iter_mut().find(|x| same(x)) {
        Some(slot) => *slot = item,
        None => list.push(item),
    }
}

// -------------------------------------------------------------------------------------- tools

fn tool_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.tool.set";
    let mut m = object(p, cmd)?.clone();
    let mut job = job_of(s);
    let id = match m.get("id") {
        Some(v) => number_id(v, "id", cmd)?,
        None => new_id(cmd, job.tools.iter().map(|t| t.id))?,
    };
    let existing = job.tool(id).cloned();
    if existing.is_none() && !m.contains_key("name") {
        m.insert("name".into(), json!(format!("Tool {id}")));
    }
    // A new tool of a kind starts with what that kind usually has.
    if existing.is_none()
        && let Some(kind) = m.get("kind").and_then(|k| serde_json::from_value::<mf::ToolKind>(k.clone()).ok())
    {
        m.entry("flutes").or_insert_with(|| json!(kind.default_flutes()));
        m.entry("tip_angle").or_insert_with(|| json!(kind.default_tip_angle()));
    }
    let tool: mf::Tool = patched(existing.as_ref(), id, &m, cmd)?;
    tool.validate().map_err(|e| bad(cmd, e.to_string()))?;
    upsert(&mut job.tools, tool.clone(), |t| t.id == id);
    store(s, job, cmd)?;
    Ok(json!({"tool": tool, "created": existing.is_none()}))
}

fn tool_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.tool.delete";
    let id = required_id(p, "id", cmd)?;
    let mut job = job_of(s);
    if job.tool(id).is_none() {
        return Err(bad(cmd, format!("there is no tool {id}")));
    }
    let users: Vec<u32> = job.operations.iter().filter(|o| o.tool == id).map(|o| o.id).collect();
    if !users.is_empty() {
        return Err(bad(cmd, format!("tool {id} is used by operations {users:?}: delete them or give them another tool first")));
    }
    job.tools.retain(|t| t.id != id);
    store(s, job, cmd)?;
    Ok(json!({"deleted": id}))
}

// ------------------------------------------------------------------------------------- setups

fn setup_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.setup.set";
    let mut m = object(p, cmd)?.clone();
    let mut job = job_of(s);
    let id = match m.get("id") {
        Some(v) => number_id(v, "id", cmd)?,
        None => new_id(cmd, job.setups.iter().map(|x| x.id))?,
    };
    let existing = job.setup(id).cloned();
    if existing.is_none() && !m.contains_key("name") {
        m.insert("name".into(), json!(format!("Setup {id}")));
    }
    // `plane` and `origin` are shorthands for the frame.
    let (plane, origin) = (m.remove("plane"), m.remove("origin"));
    if plane.is_some() || origin.is_some() {
        if m.contains_key("frame") {
            return Err(bad(cmd, "give `frame`, or `plane` and `origin`, not both"));
        }
        let mut frame = match &plane {
            Some(Value::String(n)) => Plane::named(n).ok_or_else(|| bad(cmd, "`plane` is XY, XZ or YZ"))?,
            Some(_) => return Err(bad(cmd, "`plane` is XY, XZ or YZ")),
            None => existing.as_ref().map_or(Plane::XY, |e| e.frame),
        };
        if let Some(o) = origin {
            frame.origin = serde_json::from_value::<Vec3>(o).map_err(|_| bad(cmd, "`origin` is [x, y, z]"))?;
        }
        m.insert("frame".into(), serde_json::to_value(frame).map_err(|e| bad(cmd, e.to_string()))?);
    }
    let setup: mf::Setup = patched(existing.as_ref(), id, &m, cmd)?;
    setup.validate().map_err(|e| bad(cmd, e.to_string()))?;
    upsert(&mut job.setups, setup.clone(), |x| x.id == id);
    store(s, job, cmd)?;
    Ok(json!({"setup": setup, "created": existing.is_none()}))
}

fn setup_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.setup.delete";
    let id = required_id(p, "id", cmd)?;
    let mut job = job_of(s);
    if job.setup(id).is_none() {
        return Err(bad(cmd, format!("there is no setup {id}")));
    }
    let users: Vec<u32> = job.operations.iter().filter(|o| o.setup == id).map(|o| o.id).collect();
    if !users.is_empty() {
        return Err(bad(cmd, format!("setup {id} holds operations {users:?}: delete them first")));
    }
    job.setups.retain(|x| x.id != id);
    store(s, job, cmd)?;
    Ok(json!({"deleted": id}))
}

// --------------------------------------------------------------------------------- operations

/// A sketch given by name becomes its id, wherever an operation names one.
fn resolve_sketch_names(s: &Session, m: &mut Map<String, Value>, cmd: &str) -> Result<()> {
    let Some(Value::Object(kind)) = m.get_mut("kind") else { return Ok(()) };
    for key in ["source", "points"] {
        if let Some(Value::Object(inner)) = kind.get_mut(key)
            && let Some(v) = inner.get_mut("sketch")
            && v.is_string()
        {
            let id = super::features::sketch_id(s, Some(v), cmd, "sketch")?;
            *v = json!(id);
        }
    }
    Ok(())
}

fn operation_sketch(op: &mf::Operation) -> Option<u64> {
    match &op.kind {
        mf::OpKind::Contour { source, .. } | mf::OpKind::Pocket { source, .. } => Some(source.sketch),
        mf::OpKind::Drill { points: mf::DrillPoints::Sketch { sketch, .. }, .. } => Some(*sketch),
        _ => None,
    }
}

fn operation_set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.operation.set";
    let mut m = object(p, cmd)?.clone();
    resolve_sketch_names(s, &mut m, cmd)?;
    let mut job = job_of(s);
    let id = match m.get("id") {
        Some(v) => number_id(v, "id", cmd)?,
        None => new_id(cmd, job.operations.iter().map(|x| x.id))?,
    };
    let existing = job.operation(id).cloned();
    if existing.is_none() {
        if !m.contains_key("name") {
            let kind = m.get("kind").and_then(|k| k.get("type")).and_then(Value::as_str).unwrap_or("operation");
            m.insert("name".into(), json!(format!("{kind} {id}")));
        }
        if !m.contains_key("stepdown") {
            let tool = m.get("tool").and_then(Value::as_u64).and_then(|t| u32::try_from(t).ok()).and_then(|t| job.tool(t));
            if let Some(t) = tool {
                m.insert("stepdown".into(), json!((t.diameter * 0.5).max(0.1)));
            }
        }
    }
    let op: mf::Operation = patched(existing.as_ref(), id, &m, cmd)?;
    if let Some(sk) = operation_sketch(&op)
        && !matches!(s.doc.feature(sk).map(|f| &f.kind), Some(FeatureKind::Sketch { .. }))
    {
        return Err(bad(cmd, format!("there is no sketch {sk}")));
    }
    upsert(&mut job.operations, op.clone(), |x| x.id == id);
    store(s, job, cmd)?;
    Ok(json!({"operation": op, "created": existing.is_none()}))
}

fn operation_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.operation.delete";
    let id = required_id(p, "id", cmd)?;
    let mut job = job_of(s);
    if job.operation(id).is_none() {
        return Err(bad(cmd, format!("there is no operation {id}")));
    }
    job.operations.retain(|x| x.id != id);
    store(s, job, cmd)?;
    Ok(json!({"deleted": id}))
}

// ------------------------------------------------------------------------------- toolpath, post

/// What an operation cuts, from its sketch, in the setup's work coordinates.
fn geometry(s: &Session, setup: &mf::Setup, op: &mf::Operation, cmd: &str) -> Result<mf::Geometry> {
    let (sketch, profiles, drilling) = match &op.kind {
        mf::OpKind::Contour { source, .. } | mf::OpKind::Pocket { source, .. } => (source.sketch, &source.profiles, false),
        mf::OpKind::Drill { points: mf::DrillPoints::Sketch { sketch, profiles }, .. } => (*sketch, profiles, true),
        _ => return Ok(mf::Geometry::default()),
    };
    let st = s.model.state();
    let solved = st.sketch(sketch).ok_or_else(|| bad(cmd, format!("sketch {sketch} is not evaluated (is it after the timeline marker?)")))?;
    let selection = if profiles.is_empty() { ProfileSel::All } else { ProfileSel::Indices { indices: profiles.clone() } };
    let regions = select_profiles(solved, &selection).map_err(|e| bad(cmd, format!("operation {}: {e}", op.id)))?;
    let mut mapped = Vec::with_capacity(regions.len());
    for r in &regions {
        mapped.push(mf::region_to_work(r, &solved.plane, setup).map_err(|e| bad(cmd, format!("operation {}: {e}", op.id)))?.0);
    }
    if drilling {
        let sites = mf::circle_sites(&mapped);
        if sites.is_empty() {
            return Err(bad(cmd, format!("operation {}: the sketch has no circular profile to drill (draw circles, or give points)", op.id)));
        }
        return Ok(mf::Geometry { regions: Vec::new(), sites });
    }
    Ok(mf::Geometry { regions: mapped, sites: Vec::new() })
}

fn tolerance(p: &Value, cmd: &str) -> Result<f64> {
    match p.get("tolerance") {
        None => Ok(mf::DEFAULT_TOLERANCE),
        Some(v) => v.as_f64().filter(|t| t.is_finite() && (1e-4..=1.0).contains(t)).ok_or_else(|| bad(cmd, "`tolerance` is between 0.0001 and 1 mm")),
    }
}

fn plan_operation(s: &Session, job: &mf::Job, op: &mf::Operation, tol: f64, cmd: &str) -> Result<mf::Plan> {
    let tool = job.tool(op.tool).ok_or_else(|| bad(cmd, format!("operation {} uses tool {}, which is not in the library", op.id, op.tool)))?;
    let setup = job.setup(op.setup).ok_or_else(|| bad(cmd, format!("operation {} is in setup {}, which does not exist", op.id, op.setup)))?;
    let geometry = geometry(s, setup, op, cmd)?;
    mf::plan(setup, tool, op, &geometry, tol).map_err(|e| bad(cmd, format!("operation {}: {e}", op.id)))
}

fn toolpath(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.toolpath";
    let id = required_id(p, "operation", cmd)?;
    let tol = tolerance(p, cmd)?;
    let job = s.doc.manufacture.clone();
    let op = job.operation(id).ok_or_else(|| bad(cmd, format!("there is no operation {id}")))?;
    let planned = plan_operation(s, &job, op, tol, cmd)?;
    let setup = job.setup(op.setup).ok_or_else(|| bad(cmd, "the operation's setup is missing"))?;
    let rapid = p.get("rapid_rate").and_then(Value::as_f64).filter(|r| r.is_finite() && *r > 0.0).unwrap_or(10_000.0);
    let stats = planned.toolpath.stats(Vec3::new(0.0, 0.0, setup.clearance), rapid);
    let mut out = json!({
        "operation": id,
        "kind": op.kind.label(),
        "moves": planned.toolpath.moves.len(),
        "stats": stats,
        "warnings": planned.warnings,
    });
    if p.get("moves").and_then(Value::as_bool).unwrap_or(false) {
        let limit = match p.get("limit") {
            None => MAX_LISTED_MOVES,
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_LISTED_MOVES_HARD).contains(n))
                .ok_or_else(|| bad(cmd, format!("`limit` is 1 to {MAX_LISTED_MOVES_HARD}")))?,
        };
        // Drilling cycles spelled out, as the simulation numbers its moves.
        let moves: Vec<mf::Move> =
            if p.get("expanded").and_then(Value::as_bool).unwrap_or(false) { planned.toolpath.expanded() } else { planned.toolpath.moves.clone() };
        let listed: Vec<_> = moves.iter().take(limit).collect();
        out["toolpath"] = json!(listed);
        out["toolpath_truncated"] = json!(moves.len() > listed.len());
    }
    Ok(out)
}

fn post(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.post";
    let dialect =
        p.get("dialect").and_then(Value::as_str).and_then(mf::Dialect::parse).ok_or_else(|| bad(cmd, "`dialect` is grbl, fanuc or haas"))?;
    let tol = tolerance(p, cmd)?;
    let job = s.doc.manufacture.clone();
    let ids: Vec<u32> = match p.get("operations") {
        None => job.operations.iter().map(|o| o.id).collect(),
        Some(Value::Array(a)) => a.iter().map(|v| number_id(v, "operations", cmd)).collect::<Result<_>>()?,
        Some(_) => return Err(bad(cmd, "`operations` is a list of operation numbers")),
    };
    if ids.is_empty() {
        return Err(bad(cmd, "the job has no operations to post"));
    }
    let mut planned = Vec::with_capacity(ids.len());
    let mut warnings = Vec::new();
    for id in &ids {
        let op = job.operation(*id).ok_or_else(|| bad(cmd, format!("there is no operation {id}")))?;
        let plan = plan_operation(s, &job, op, tol, cmd)?;
        warnings.extend(plan.warnings.iter().map(|w| format!("operation {id}: {w}")));
        planned.push((op, plan));
    }
    let mut steps = Vec::with_capacity(planned.len());
    for (op, plan) in &planned {
        let tool = job.tool(op.tool).ok_or_else(|| bad(cmd, "a tool is missing"))?;
        let setup = job.setup(op.setup).ok_or_else(|| bad(cmd, "a setup is missing"))?;
        steps.push(mf::Step { setup, tool, op, toolpath: &plan.toolpath });
    }
    let mut opts = mf::PostOptions::default();
    if let Some(n) = p.get("program_name") {
        opts.program_name = n.as_str().filter(|n| n.len() <= 128).ok_or_else(|| bad(cmd, "`program_name` is text of at most 128 bytes"))?.to_string();
    } else {
        opts.program_name = s.doc.name.chars().take(64).collect();
    }
    if let Some(n) = p.get("program_number") {
        opts.program_number =
            n.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n <= 99_999).ok_or_else(|| bad(cmd, "`program_number` is 0 to 99999"))?;
    }
    opts.line_numbers = p.get("line_numbers").and_then(Value::as_bool).unwrap_or(false);
    let (text, step_lines) = mf::generate_mapped(dialect, &steps, &opts).map_err(|e| bad(cmd, e.to_string()))?;
    let mut out = json!({
        "dialect": dialect,
        "operations": ids,
        "lines": text.lines().count(),
        "bytes": text.len(),
        "warnings": warnings,
    });
    // For following the program along a simulation: the line each move of it is run from.
    if p.get("map_steps").and_then(Value::as_bool).unwrap_or(false) {
        out["step_lines"] = json!(step_lines);
    }
    match p.get("path") {
        Some(Value::String(path)) => {
            if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
                return Err(bad(cmd, "`path` must be a file path of at most 4096 characters"));
            }
            solidcraft_io::vfs::check(path).map_err(|e| bad(cmd, e.to_string()))?;
            let target = std::path::Path::new(path);
            let overwrite = p.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
            let written = if overwrite {
                solidcraft_io::write_atomic(target, text.as_bytes(), false)
            } else {
                solidcraft_io::safe_write::create_atomic(target, text.as_bytes())
            };
            written.map_err(|e| bad(cmd, format!("could not write the program: {e}")))?;
            out["path"] = json!(path);
        }
        Some(_) => return Err(bad(cmd, "`path` must be text")),
        None => {
            if text.len() > MAX_INLINE_GCODE {
                return Err(bad(cmd, format!("the program is {} bytes: give a `path` to write it to a file", text.len())));
            }
            out["gcode"] = json!(text);
        }
    }
    Ok(out)
}

/// A run of the job's operations to watch: the stock being cut away move by move.
pub struct WatchRun {
    pub runner: mf::Runner,
    /// Work coordinates to model coordinates: origin and the X, Y and Z axes.
    pub frame: [solidcraft_geom::Vec3; 4],
    pub stock: mf::Stock,
    pub setup: u32,
    /// Spindle speed of each operation of the runner (rpm).
    pub rpm: Vec<f64>,
}

/// Plans the operations of a setup (the first one when `setup` is `None`) and sets up the stock
/// to be cut on cells of about `cell` mm. Does not change the design.
pub fn watch_run(s: &mut Session, setup: Option<u32>, cell: f64, rapid_rate: f64) -> Result<WatchRun> {
    let cmd = "manufacture.simulate";
    let job = s.doc.manufacture.clone();
    let setup_id = match setup {
        Some(id) => id,
        None => job.setups.first().map(|x| x.id).ok_or_else(|| bad(cmd, "the job has no setup"))?,
    };
    let setup = job.setup(setup_id).ok_or_else(|| bad(cmd, format!("there is no setup {setup_id}")))?;
    let stock = setup.stock.ok_or_else(|| bad(cmd, format!("setup {setup_id} has no stock: give it `stock` {{min, max}} to simulate")))?;
    let ids: Vec<u32> = job.operations.iter().filter(|o| o.setup == setup_id).map(|o| o.id).collect();
    if ids.is_empty() {
        return Err(bad(cmd, format!("setup {setup_id} has no operations to simulate")));
    }
    let mut planned = Vec::with_capacity(ids.len());
    for id in &ids {
        let op = job.operation(*id).ok_or_else(|| bad(cmd, format!("there is no operation {id}")))?;
        let tool = job.tool(op.tool).ok_or_else(|| bad(cmd, format!("operation {id} uses tool {}, which is not in the library", op.tool)))?;
        planned.push((op, tool, plan_operation(s, &job, op, mf::DEFAULT_TOLERANCE, cmd)?));
    }
    let ops: Vec<mf::SimOp> = planned.iter().map(|(op, tool, plan)| mf::SimOp { operation: op.id, tool, toolpath: &plan.toolpath }).collect();
    let runner = mf::Runner::new(&stock, &ops, cell, setup.clearance, rapid_rate).map_err(|e| bad(cmd, e.to_string()))?;
    Ok(WatchRun {
        runner,
        frame: [setup.frame.origin, setup.frame.x, setup.frame.y, setup.tool_axis()],
        stock,
        setup: setup_id,
        rpm: planned.iter().map(|(op, _, _)| op.spindle_rpm).collect(),
    })
}

/// Most triangles taken from the model for the part.
const MAX_PART_TRIANGLES: usize = 6_000_000;

fn simulate(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.simulate";
    object(p, cmd)?;
    let job = s.doc.manufacture.clone();
    let setup_id = match p.get("setup") {
        Some(v) => number_id(v, "setup", cmd)?,
        None => job.setups.first().map(|x| x.id).ok_or_else(|| bad(cmd, "the job has no setup"))?,
    };
    let setup = job.setup(setup_id).ok_or_else(|| bad(cmd, format!("there is no setup {setup_id}")))?;
    let stock = setup.stock.ok_or_else(|| bad(cmd, format!("setup {setup_id} has no stock: give it `stock` {{min, max}} to simulate")))?;
    let ids: Vec<u32> = match p.get("operations") {
        None => job.operations.iter().filter(|o| o.setup == setup_id).map(|o| o.id).collect(),
        Some(Value::Array(a)) => a.iter().map(|v| number_id(v, "operations", cmd)).collect::<Result<_>>()?,
        Some(_) => return Err(bad(cmd, "`operations` is a list of operation numbers")),
    };
    if ids.is_empty() {
        return Err(bad(cmd, format!("setup {setup_id} has no operations to simulate")));
    }
    let chord = match p.get("tolerance_chord") {
        None => mf::DEFAULT_TOLERANCE,
        Some(v) => v
            .as_f64()
            .filter(|t| t.is_finite() && (1e-4..=1.0).contains(t))
            .ok_or_else(|| bad(cmd, "`tolerance_chord` is between 0.0001 and 1 mm"))?,
    };
    let mut planned = Vec::with_capacity(ids.len());
    for id in &ids {
        let op = job.operation(*id).ok_or_else(|| bad(cmd, format!("there is no operation {id}")))?;
        if op.setup != setup_id {
            return Err(bad(cmd, format!("operation {id} is in setup {}, not {setup_id}", op.setup)));
        }
        let tool = job.tool(op.tool).ok_or_else(|| bad(cmd, format!("operation {id} uses tool {}, which is not in the library", op.tool)))?;
        planned.push((op, tool, plan_operation(s, &job, op, chord, cmd)?));
    }
    let cell = match p.get("resolution") {
        None => 0.5,
        Some(v) => v
            .as_f64()
            .filter(|r| r.is_finite() && (mf::sim::MIN_CELL..=100.0).contains(r))
            .ok_or_else(|| bad(cmd, "`resolution` is between 0.01 and 100 mm"))?,
    };
    let tolerance = match p.get("tolerance") {
        None => None,
        Some(v) => Some(v.as_f64().filter(|t| t.is_finite()).ok_or_else(|| bad(cmd, "`tolerance` must be a number"))?),
    };
    let until = match p.get("until") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(cmd, "`until` is a move number"))?),
    };
    let grid = mf::Grid::new(&stock, cell).map_err(|e| bad(cmd, e.to_string()))?;
    let part = if p.get("part").and_then(Value::as_bool).unwrap_or(true) {
        let names: Option<Vec<String>> = match p.get("bodies") {
            None => None,
            Some(Value::Array(a)) => {
                Some(a.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| bad(cmd, "`bodies` is a list of names"))).collect::<Result<_>>()?)
            }
            Some(_) => return Err(bad(cmd, "`bodies` is a list of names")),
        };
        let world = s.world_state();
        let mut triangles: Vec<[solidcraft_geom::Vec3; 3]> = Vec::new();
        for body in &world.bodies {
            if names.as_ref().is_some_and(|n| !n.contains(&body.name)) {
                continue;
            }
            let mesh = body.mesh();
            if triangles.len() + mesh.triangles.len() > MAX_PART_TRIANGLES {
                return Err(bad(cmd, "the model has too many triangles to simulate against; pick `bodies` or use `part: false`"));
            }
            for t in &mesh.triangles {
                let at = |i: u32| mesh.positions.get(i as usize).map(|v| setup.to_local(*v));
                if let [Some(a), Some(b), Some(c)] = t.map(at) {
                    triangles.push([a, b, c]);
                }
            }
        }
        Some(mf::Part::from_triangles(&grid, triangles).map_err(|e| bad(cmd, e.to_string()))?)
    } else {
        None
    };
    let ops: Vec<mf::SimOp> = planned.iter().map(|(op, tool, plan)| mf::SimOp { operation: op.id, tool, toolpath: &plan.toolpath }).collect();
    let sim = mf::simulate(&stock, part.as_ref(), &ops, &mf::SimOptions { cell, tolerance, until, clearance: setup.clearance })
        .map_err(|e| bad(cmd, e.to_string()))?;
    let at = |v: solidcraft_geom::Vec3| json!([v.x, v.y, v.z]);
    let collisions: Vec<Value> =
        sim.collisions.iter().map(|c| json!({"operation": c.operation, "step": c.step, "kind": c.kind, "at": at(c.at), "depth": c.depth})).collect();
    let mut out = json!({
        "setup": setup_id,
        "frame": {"origin": at(setup.frame.origin), "x": at(setup.frame.x), "y": at(setup.frame.y), "z": at(setup.tool_axis())},
        "stock": {"min": stock.min, "max": stock.max},
        "grid": sim.grid,
        "steps": sim.steps,
        "total_steps": sim.total_steps,
        "operations": sim.starts.iter().map(|(o, st)| json!({"operation": o, "first_step": st})).collect::<Vec<_>>(),
        "volumes": sim.volumes,
        "counts": {"gouge": sim.counts[0], "shank": sim.counts[1], "rapid": sim.counts[2], "floor": sim.counts[3]},
        "collisions": collisions,
        "open_cells": sim.open_cells,
        "tolerance": sim.tolerance,
    });
    if let Some(n) = p.get("surface").and_then(Value::as_u64).filter(|n| *n > 0) {
        out["surface"] = json!(sim.surface(usize::try_from(n).unwrap_or(usize::MAX).min(250_000)));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "manufacture_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
