//! Post-processing: toolpaths to G-code text for a controller dialect.
//!
//! - **Grbl** (hobby routers and small mills): metric, absolute, no canned cycles (drilling is
//!   spelled out), no tool length offsets, and a pause (`M0`) at every tool change.
//! - **Fanuc**: `G90 G94 G17 G21` safe line, `T M6` and `G43 H` tool changes, work offsets,
//!   canned drilling (`G81`, `G82`, `G83` with `G98`/`G80`), `G91 G28 Z0` at the end, program
//!   number `O`, `%` delimiters. Every dimension carries a decimal point.
//! - **Haas**: the same, ending with `G53` moves to the machine home, `O` with five digits.
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
    for m in &path.moves {
        if !matches!(m, Move::Drill(_)) && in_cycle {
            w.line("G80");
            in_cycle = false;
            mode = None;
        }
        // The line this move starts at, once for each step it makes.
        let here = w.count;
        let steps = match m {
            Move::Drill(_) => Toolpath { moves: vec![*m] }.expanded().len(),
            _ => 1,
        };
        step_lines.extend(std::iter::repeat_n(here, steps));
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
