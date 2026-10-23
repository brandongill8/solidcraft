//! Post-processing: toolpaths to G-code text for a controller dialect.
//!
//! - **Grbl** (hobby routers and small mills): metric, absolute, no canned cycles (drilling is
//!   spelled out), no tool length offsets, and a pause (`M0`) at every tool change.
//! - **Fanuc**: `G90 G94 G17 G21` safe line, `T M6` and `G43 H` tool changes, work offsets,
//!   canned drilling (`G81`, `G82`, `G83` with `G98`/`G80`), `G91 G28 Z0` at the end, program
//!   number `O`, `%` delimiters. Every dimension carries a decimal point.
//! - **Haas**: the same, ending with `G53` moves to the machine home, `O` with five digits.
//!
//! Straight runs stay `G1`; a run of short feed moves along one circle (or helix) is written as
//! a single `G2`/`G3` with `I` and `J`, so round pockets, bores and fillets take a few lines
//! instead of hundreds. Every dialect uses `G17`, `G90`, `G94` and `G21`; spindle `S M3`/`M5`,
//! coolant `M8`/`M9` and the end `M30`.
//!
//! The text is all ASCII, comments are in parentheses, and numbers have three decimals.

use serde::{Deserialize, Serialize};
use solidcraft_geom::Vec3;

use crate::error::{Result, invalid};
use crate::job::{Operation, Setup};
use crate::path::{DrillCycle, Move, Toolpath};
use crate::tool::Tool;

/// The controller family to write for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Grbl,
    Fanuc,
    Haas,
}

impl Dialect {
    pub fn label(self) -> &'static str {
        match self {
            Dialect::Grbl => "Grbl",
            Dialect::Fanuc => "Fanuc",
            Dialect::Haas => "Haas",
        }
    }
    pub fn parse(s: &str) -> Option<Dialect> {
        match s.to_ascii_lowercase().as_str() {
            "grbl" => Some(Dialect::Grbl),
            "fanuc" => Some(Dialect::Fanuc),
            "haas" => Some(Dialect::Haas),
            _ => None,
        }
    }
    fn decimal_point(self) -> bool {
        !matches!(self, Dialect::Grbl)
    }
}

/// Options of a post run.
#[derive(Clone, Debug, PartialEq)]
pub struct PostOptions {
    /// Shown in the program's first comment.
    pub program_name: String,
    /// The `O` number of Fanuc and Haas programs.
    pub program_number: u32,
    /// Number every line (`N10`, `N20`…) in Fanuc and Haas programs.
    pub line_numbers: bool,
}

impl Default for PostOptions {
    fn default() -> Self {
        PostOptions { program_name: "SOLIDCRAFT".into(), program_number: 1001, line_numbers: false }
    }
}

/// One operation ready to post.
pub struct Step<'a> {
    pub setup: &'a Setup,
    pub tool: &'a Tool,
    pub op: &'a Operation,
    pub toolpath: &'a Toolpath,
}

/// Comment text the controllers accept: printable ASCII without parentheses, upper case for
/// the Japanese-style controllers.
fn comment_text(s: &str, upper: bool) -> String {
    let t: String = s.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' }).filter(|c| *c != '(' && *c != ')').collect();
    let t = t.trim().to_string();
    if upper { t.to_ascii_uppercase() } else { t }
}

/// A number with at most three decimals; Fanuc-style dialects keep the decimal point on whole
/// numbers (`10.`), because `X10` would be ten thousandths there.
fn num(v: f64, point: bool) -> String {
    let mut s = format!("{v:.3}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        if point {
            return if s == "-0." { "0.".into() } else { s };
        }
        s.pop();
    }
    if s == "-0" { "0".into() } else { s }
}

struct Writer {
    dialect: Dialect,
    out: String,
    n: u32,
    numbered: bool,
    /// Lines written so far.
    count: usize,
}

impl Writer {
    fn line(&mut self, text: &str) {
        self.count += 1;
        if self.numbered && !text.is_empty() && !text.starts_with('%') && !text.starts_with('O') {
            self.n += 10;
            self.out.push_str(&format!("N{} ", self.n));
        }
        self.out.push_str(text);
        self.out.push('\n');
    }
    fn comment(&mut self, text: &str) {
        let t = comment_text(text, self.dialect != Dialect::Grbl);
        self.line(&format!("({t})"));
    }
    fn n(&self, v: f64) -> String {
        num(v, self.dialect.decimal_point())
    }
}

/// The G-code for `steps`, in order.
pub fn generate(dialect: Dialect, steps: &[Step<'_>], opts: &PostOptions) -> Result<String> {
    generate_mapped(dialect, steps, opts).map(|(text, _)| text)
}

/// The program, and for every move of the toolpaths (drilling cycles spelled out, all steps in
/// order: the numbering of [`crate::sim::Runner`]) the number of the program line (from 0) that
/// does it.
pub fn generate_mapped(dialect: Dialect, steps: &[Step<'_>], opts: &PostOptions) -> Result<(String, Vec<usize>)> {
    if steps.is_empty() {
        return invalid("there is nothing to post: the job has no operations");
    }
    for s in steps {
        for m in &s.toolpath.moves {
            let ok = match m {
                Move::Rapid { to } | Move::Feed { to, .. } => to.is_finite(),
                Move::Drill(c) => [c.x, c.y, c.top, c.bottom, c.retract, c.feed].iter().all(|v| v.is_finite()),
            };
            if !ok {
                return invalid(format!("operation {} has a move with a number that is not finite", s.op.id));
            }
        }
    }
    let mut w = Writer { dialect, out: String::new(), n: 0, numbered: opts.line_numbers && dialect != Dialect::Grbl, count: 0 };
    let mut step_lines: Vec<usize> = Vec::new();
    match dialect {
        Dialect::Grbl => {
            w.comment(&opts.program_name);
            w.comment("Generated by SolidCraft for Grbl");
            w.line("G21 G90 G17 G94");
        }
        Dialect::Fanuc | Dialect::Haas => {
            w.line("%");
            let digits = if dialect == Dialect::Haas { 5 } else { 4 };
            w.line(&format!("O{:0digits$} ({})", opts.program_number.min(99_999), comment_text(&opts.program_name, true)));
            w.comment("Generated by SolidCraft");
            w.line("G90 G94 G17 G21 G40 G49 G80");
        }
    }
    let mut current_tool: Option<u32> = None;
    let mut current_offset: Option<u32> = None;
    let mut spindle: Option<f64> = None;
    let mut last_clearance = 0.0;
    for (i, s) in steps.iter().enumerate() {
        let (op, tool, setup) = (s.op, s.tool, s.setup);
        w.comment(&format!("Operation {}: {} ({})", op.id, op.name, op.kind.label()));
        if current_tool != Some(tool.id) {
            let note = format!("Tool {}: {} {:.3} mm", tool.id, tool.kind.label(), tool.diameter);
            if current_tool.is_some() || dialect != Dialect::Grbl {
                w.line("M5");
                spindle = None;
            }
            match dialect {
                Dialect::Grbl => {
                    w.comment(&note);
                    if current_tool.is_some() {
                        // Grbl does not change tools: stop and wait for the operator.
                        w.line(&format!("(MSG, Change to tool {} and press cycle start)", tool.id));
                        w.line("M0");
                    }
                }
                _ => {
                    w.comment(&note);
                    // The tool changer is reached with the head up at the machine's own Z home.
                    if dialect == Dialect::Haas {
                        w.line("G53 G0 Z0.");
                    } else {
                        w.line("G91 G28 Z0.");
                        w.line("G90");
                    }
                    w.line(&format!("T{} M6", tool.id));
                }
            }
            current_tool = Some(tool.id);
        }
        if current_offset != Some(setup.work_offset) {
            w.line(&format!("G{}", setup.work_offset));
            current_offset = Some(setup.work_offset);
        }
        let clearance = setup.clearance;
        last_clearance = clearance;
        match dialect {
            Dialect::Grbl => w.line(&format!("G0 Z{}", w.n(clearance))),
            _ => w.line(&format!("G0 G43 H{} Z{}", tool.id, w.n(clearance))),
        }
        if spindle != Some(op.spindle_rpm) {
            w.line(&format!("S{} M3", num(op.spindle_rpm.round(), false)));
            spindle = Some(op.spindle_rpm);
        }
        if op.coolant {
            w.line("M8");
        }
        write_moves(&mut w, s.toolpath, dialect, &mut step_lines);
        if op.coolant {
            w.line("M9");
        }
        if i + 1 == steps.len() {
            w.line("M5");
        }
    }
    match dialect {
        Dialect::Grbl => {
            w.line(&format!("G0 Z{}", w.n(last_clearance)));
            w.line("M30");
        }
        Dialect::Fanuc => {
            w.line("G91 G28 Z0.");
            w.line("G90");
            w.line("M30");
            w.line("%");
        }
        Dialect::Haas => {
            w.line("G53 G0 Z0.");
            w.line("G53 G0 Y0.");
            w.line("M30");
            w.line("%");
        }
    }
    Ok((w.out, step_lines))
}

/// Modal output of one toolpath: G words and feeds only when they change, axes only when they
/// move. `step_lines` gets the line each step starts at.
fn write_moves(w: &mut Writer, path: &Toolpath, dialect: Dialect, step_lines: &mut Vec<usize>) {
    let canned = !matches!(dialect, Dialect::Grbl);
    let mut at: Option<Vec3> = None;
    let mut mode: Option<&str> = None;
    let mut feed: Option<f64> = None;
    let mut in_cycle = false;
    let mut index = 0;
    while let Some(m) = path.moves.get(index) {
        index += 1;
        if !matches!(m, Move::Drill(_)) && in_cycle {
            w.line("G80");
            in_cycle = false;
            mode = None;
        }
        // The line this move starts at, once for each step it makes.
        let here = w.count;
        // A run of short feed moves along one circle is a single G2 or G3.
        let arc = match (m, at) {
            (Move::Feed { .. }, Some(start)) => find_arc(start, &path.moves, index - 1),
            _ => None,
        };
        let steps = match m {
            Move::Drill(_) => Toolpath { moves: vec![*m] }.expanded().len(),
            _ => arc.as_ref().map_or(1, |a| a.segments),
        };
        step_lines.extend(std::iter::repeat_n(here, steps));
        if let Some(a) = arc {
            emit_arc(w, &a, &mut at, &mut mode, &mut feed);
            index = index - 1 + a.segments;
            continue;
        }
        match m {
            Move::Rapid { to } => {
                emit(w, "G0", None, *to, &mut at, &mut mode, &mut feed);
            }
            Move::Feed { to, feed: f } => {
                emit(w, "G1", Some(*f), *to, &mut at, &mut mode, &mut feed);
            }
            Move::Drill(c) if canned => {
                cycle(w, c, &mut in_cycle);
                at = Some(Vec3::new(c.x, c.y, c.retract));
                mode = None;
            }
            Move::Drill(_) => {
                let one = Toolpath { moves: vec![*m] };
                for x in one.expanded() {
                    match x {
                        Move::Rapid { to } => emit(w, "G0", None, to, &mut at, &mut mode, &mut feed),
                        Move::Feed { to, feed: f } => emit(w, "G1", Some(f), to, &mut at, &mut mode, &mut feed),
                        Move::Drill(_) => {}
                    }
                }
            }
        }
    }
    if in_cycle {
        w.line("G80");
    }
}

/// Largest distance (mm) of a point of a run from the circle it is written as.
const ARC_TOLERANCE: f64 = 0.001;
/// Longest chord turn (radians) and sweep of one arc line; a corner is never an arc.
const ARC_MAX_STEP: f64 = std::f64::consts::FRAC_PI_3;
const ARC_MAX_SWEEP: f64 = 300.0 * std::f64::consts::PI / 180.0;
/// Most moves looked at for one arc.
const ARC_MAX_MOVES: usize = 720;

/// A circular (or helical) move written as `G2`/`G3`.
struct Arc {
    /// How many linear moves it replaces.
    segments: usize,
    end: Vec3,
    /// Centre minus start, in X and Y.
    i: f64,
    j: f64,
    ccw: bool,
    feed: f64,
}

fn wrap(a: f64) -> f64 {
    let t = std::f64::consts::TAU;
    let mut r = a % t;
    if r > std::f64::consts::PI {
        r -= t;
    } else if r < -std::f64::consts::PI {
        r += t;
    }
    r
}

/// The longest run of feed moves from `moves[from]` (starting at `start`) that lie on one circle
/// about a vertical axis, with Z changing evenly along it (a helix) or not at all. Needs at
/// least three moves, so two chords of a corner never become an arc.
fn find_arc(start: Vec3, moves: &[Move], from: usize) -> Option<Arc> {
    let Some(Move::Feed { feed, .. }) = moves.get(from) else { return None };
    let mut pts = vec![start];
    for m in moves.iter().skip(from).take(ARC_MAX_MOVES) {
        match m {
            Move::Feed { to, feed: f } if (f - feed).abs() < 1e-9 => pts.push(*to),
            _ => break,
        }
    }
    if pts.len() < 4 {
        return None;
    }
    // The circle through the first three points.
    let (a, b, c) = (pts[0], pts[1], pts[2]);
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-9 {
        return None;
    }
    let (a2, b2, c2) = (a.x * a.x + a.y * a.y, b.x * b.x + b.y * b.y, c.x * c.x + c.y * c.y);
    let cx = (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d;
    let cy = (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d;
    let r = ((a.x - cx).powi(2) + (a.y - cy).powi(2)).sqrt();
    if !(r.is_finite() && (0.01..=1.0e5).contains(&r) && cx.is_finite() && cy.is_finite()) {
        return None;
    }
    let angle = |p: Vec3| (p.y - cy).atan2(p.x - cx);
    let first_turn = wrap(angle(b) - angle(a));
    if first_turn.abs() < 1e-6 || first_turn.abs() > ARC_MAX_STEP {
        return None;
    }
    let sign = first_turn.signum();
    let slope = (b.z - a.z) / first_turn;
    let mut sweep = 0.0;
    let mut good = 0;
    for k in 1..pts.len() {
        let (p, q) = (pts[k - 1], pts[k]);
        if ((q.x - cx).hypot(q.y - cy) - r).abs() > ARC_TOLERANCE {
            break;
        }
        let turn = wrap(angle(q) - angle(p));
        if turn * sign <= 1e-6 || turn.abs() > ARC_MAX_STEP || (sweep + turn).abs() > ARC_MAX_SWEEP {
            break;
        }
        sweep += turn;
        if (q.z - a.z - slope * sweep).abs() > 2.0 * ARC_TOLERANCE {
            break;
        }
        good = k;
    }
    if good < 3 {
        return None;
    }
    let end = pts[good];
    Some(Arc { segments: good, end, i: cx - a.x, j: cy - a.y, ccw: sign > 0.0, feed: *feed })
}

fn emit_arc(w: &mut Writer, arc: &Arc, at: &mut Option<Vec3>, mode: &mut Option<&str>, feed: &mut Option<f64>) {
    let g = if arc.ccw { "G3" } else { "G2" };
    let mut words: Vec<String> = Vec::new();
    if *mode != Some(g) {
        words.push(g.to_string());
        *mode = Some(g);
    }
    words.push(format!("X{}", w.n(arc.end.x)));
    words.push(format!("Y{}", w.n(arc.end.y)));
    if at.is_none_or(|p| (p.z - arc.end.z).abs() >= 5e-4) {
        words.push(format!("Z{}", w.n(arc.end.z)));
    }
    words.push(format!("I{}", w.n(arc.i)));
    words.push(format!("J{}", w.n(arc.j)));
    if feed.is_none_or(|old| (old - arc.feed).abs() > 1e-9) {
        words.push(format!("F{}", num(arc.feed, false)));
        *feed = Some(arc.feed);
    }
    *at = Some(arc.end);
    w.line(&words.join(" "));
}

fn emit(w: &mut Writer, g: &'static str, f: Option<f64>, to: Vec3, at: &mut Option<Vec3>, mode: &mut Option<&str>, feed: &mut Option<f64>) {
    let mut words: Vec<String> = Vec::new();
    if *mode != Some(g) {
        words.push(g.to_string());
        *mode = Some(g);
    }
    let same = |a: f64, b: f64| (a - b).abs() < 5e-4;
    match *at {
        Some(p) => {
            if !same(p.x, to.x) {
                words.push(format!("X{}", w.n(to.x)));
            }
            if !same(p.y, to.y) {
                words.push(format!("Y{}", w.n(to.y)));
            }
            if !same(p.z, to.z) {
                words.push(format!("Z{}", w.n(to.z)));
            }
        }
        None => {
            words.push(format!("X{}", w.n(to.x)));
            words.push(format!("Y{}", w.n(to.y)));
            words.push(format!("Z{}", w.n(to.z)));
        }
    }
    if let Some(f) = f
        && feed.is_none_or(|old| (old - f).abs() > 1e-9)
    {
        words.push(format!("F{}", num(f, false)));
        *feed = Some(f);
    }
    *at = Some(to);
    if !words.is_empty() {
        w.line(&words.join(" "));
    }
}

/// One hole as a canned cycle: `G98` returns to the initial height between holes.
fn cycle(w: &mut Writer, c: &DrillCycle, in_cycle: &mut bool) {
    if !*in_cycle {
        w.line("G98");
        *in_cycle = true;
    }
    let (x, y, z, r) = (w.n(c.x), w.n(c.y), w.n(c.bottom), w.n(c.retract));
    let f = num(c.feed, false);
    let depth = c.top - c.bottom;
    match c.peck {
        Some(p) if p > 0.0 && p < depth => w.line(&format!("G83 X{x} Y{y} Z{z} R{r} Q{} F{f}", w.n(p))),
        _ if c.dwell > 0.0 => w.line(&format!("G82 X{x} Y{y} Z{z} R{r} P{} F{f}", (c.dwell * 1000.0).round() as u64)),
        _ => w.line(&format!("G81 X{x} Y{y} Z{z} R{r} F{f}")),
    }
}
