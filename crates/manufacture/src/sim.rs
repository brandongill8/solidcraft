//! Material removal and collision checking for 2.5D toolpaths.
//!
//! One setup at a time, in its work coordinates. The stock is a height map (a vertical tool
//! only ever removes material from the top down, so the top surface says everything), the
//! part is a column of solid intervals per cell, found by dropping rays through its triangles.
//! The tool is swept along each move in steps of half a cell; at each step it lowers the stock
//! under it and is checked against the part and the stock:
//!
//! - **Gouge**: the cutting flutes enter the part (feed moves).
//! - **Shank**: the shank above the flutes meets stock or the part (a cut deeper than the
//!   flute length, with a tool that has one).
//! - **Rapid**: a rapid move runs the tool into stock or the part.
//! - **Floor**: the tip goes below the bottom of the stock.
//!
//! Anything smaller than about one cell is not reported: the tool outline is cut to the grid.
//! The `tolerance` (default: one cell, at least 0.05 mm) is how deep an overlap must be to count.

use serde::Serialize;
use solidcraft_geom::Vec3;

use crate::error::{ManufactureError, Result, finite, invalid};
use crate::job::Stock;
use crate::path::{Move, Toolpath};
use crate::tool::Tool;

/// Most cells in a grid; coarser cells are used past it.
pub const MAX_CELLS: usize = 4_000_000;
/// Most ray hits kept while building the part.
pub const MAX_CROSSINGS: usize = 60_000_000;
/// Most grid cells visited in one simulation (tool positions times cells under the tool);
/// about ten seconds of work.
pub const MAX_WORK: f64 = 3.0e9;
/// Most collisions listed (the counts keep going).
pub const MAX_COLLISIONS: usize = 200;
/// Smallest cell (mm).
pub const MIN_CELL: f64 = 0.01;
/// Largest coordinate accepted (mm).
const MAX_COORD: f64 = 1.0e7;

/// The grid both the stock and the part live on: square cells over the stock's footprint.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Grid {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
}

impl Grid {
    /// A grid over `stock`'s footprint, cells of about `cell` mm (larger when that would be
    /// more than [`MAX_CELLS`] cells).
    pub fn new(stock: &Stock, cell: f64) -> Result<Grid> {
        finite(cell, "the resolution")?;
        if cell < MIN_CELL {
            return invalid(format!("the resolution is at least {MIN_CELL} mm"));
        }
        stock.validate()?;
        let (w, h) = (stock.max[0] - stock.min[0], stock.max[1] - stock.min[1]);
        if w > MAX_COORD || h > MAX_COORD {
            return invalid("the stock is too large");
        }
        let mut cell = cell;
        loop {
            let (nx, ny) = ((w / cell).ceil().max(1.0), (h / cell).ceil().max(1.0));
            if nx * ny <= MAX_CELLS as f64 {
                return Ok(Grid { x0: stock.min[0], y0: stock.min[1], cell, nx: nx as usize, ny: ny as usize });
            }
            cell *= 1.25;
        }
    }

    pub fn cells(&self) -> usize {
        self.nx * self.ny
    }

    fn cx(&self, ix: usize) -> f64 {
        self.x0 + (ix as f64 + 0.5) * self.cell
    }
    fn cy(&self, iy: usize) -> f64 {
        self.y0 + (iy as f64 + 0.5) * self.cell
    }
}

/// The part as solid intervals along Z, per cell.
#[derive(Clone, Debug)]
pub struct Part {
    grid: Grid,
    offsets: Vec<u32>,
    spans: Vec<[f32; 2]>,
    /// Cells where the surface did not close (an odd number of hits): the last hit is ignored.
    pub open_cells: usize,
}

impl Part {
    /// The part inside `grid`, from triangles in work coordinates.
    pub fn from_triangles(grid: &Grid, triangles: impl IntoIterator<Item = [Vec3; 3]>) -> Result<Part> {
        // The rays go through points a hair off the cell centres, so none lands on a mesh edge.
        let (jx, jy) = (1.234_567e-6 * grid.cell.max(1e-3), 0.765_432e-6 * grid.cell.max(1e-3));
        let mut hits: Vec<(u32, f32)> = Vec::new();
        for t in triangles {
            if t.iter().any(|p| !p.is_finite() || p.x.abs() > MAX_COORD || p.y.abs() > MAX_COORD || p.z.abs() > MAX_COORD) {
                return invalid("the part has a point that is not finite or is out of range");
            }
            let (a, b, c) = (t[0], t[1], t[2]);
            let det = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
            if det.abs() < 1e-12 {
                continue; // edge-on or degenerate
            }
            let (lo_x, hi_x) = (a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x));
            let (lo_y, hi_y) = (a.y.min(b.y).min(c.y), a.y.max(b.y).max(c.y));
            let ix0 = (((lo_x - grid.x0) / grid.cell - 0.5).floor().max(0.0)) as usize;
            let iy0 = (((lo_y - grid.y0) / grid.cell - 0.5).floor().max(0.0)) as usize;
            let ix1 = ((((hi_x - grid.x0) / grid.cell - 0.5).ceil()).max(-1.0) + 1.0).min(grid.nx as f64) as usize;
            let iy1 = ((((hi_y - grid.y0) / grid.cell - 0.5).ceil()).max(-1.0) + 1.0).min(grid.ny as f64) as usize;
            for iy in iy0..iy1 {
                let py = grid.cy(iy) + jy;
                for ix in ix0..ix1 {
                    let px = grid.cx(ix) + jx;
                    let l1 = ((b.y - c.y) * (px - c.x) + (c.x - b.x) * (py - c.y)) / det;
                    let l2 = ((c.y - a.y) * (px - c.x) + (a.x - c.x) * (py - c.y)) / det;
                    let l3 = 1.0 - l1 - l2;
                    if l1 < 0.0 || l2 < 0.0 || l3 < 0.0 {
                        continue;
                    }
                    if hits.len() >= MAX_CROSSINGS {
                        return Err(ManufactureError::Limit("the part has too many surface layers over the stock; use a coarser resolution".into()));
                    }
                    let z = l1 * a.z + l2 * b.z + l3 * c.z;
                    hits.push(((iy * grid.nx + ix) as u32, z as f32));
                }
            }
        }
        hits.sort_unstable_by(|p, q| p.0.cmp(&q.0).then(p.1.total_cmp(&q.1)));
        let mut offsets = vec![0u32; grid.cells() + 1];
        let mut spans = Vec::with_capacity(hits.len() / 2);
        let (mut open, mut i, mut cell) = (0usize, 0usize, 0usize);
        while cell < grid.cells() {
            let start = i;
            while i < hits.len() && hits.get(i).is_some_and(|h| h.0 as usize == cell) {
                i += 1;
            }
            let run = hits.get(start..i).unwrap_or(&[]);
            for pair in run.chunks_exact(2) {
                if let [p, q] = pair {
                    spans.push([p.1, q.1]);
                }
            }
            if run.len() % 2 == 1 {
                open += 1;
            }
            cell += 1;
            if let Some(o) = offsets.get_mut(cell) {
                *o = spans.len() as u32;
            }
        }
        Ok(Part { grid: *grid, offsets, spans, open_cells: open })
    }

    fn at(&self, cell: usize) -> &[[f32; 2]] {
        let (a, b) = (self.offsets.get(cell).copied().unwrap_or(0) as usize, self.offsets.get(cell + 1).copied().unwrap_or(0) as usize);
        self.spans.get(a..b).unwrap_or(&[])
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }
}

/// What kind of collision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionKind {
    /// The flutes cut into the part.
    Gouge,
    /// The shank above the flutes meets stock or the part.
    Shank,
    /// A rapid move runs into stock or the part.
    Rapid,
    /// The tip goes below the bottom of the stock.
    Floor,
}

impl CollisionKind {
    /// Position in [`Simulation::counts`].
    pub fn index(self) -> usize {
        match self {
            CollisionKind::Gouge => 0,
            CollisionKind::Shank => 1,
            CollisionKind::Rapid => 2,
            CollisionKind::Floor => 3,
        }
    }
}

/// One collision: the first tool position of a move where it happens, and how deep it goes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Collision {
    pub operation: u32,
    /// The move's number in the whole simulation (drilling cycles spelled out).
    pub step: usize,
    pub kind: CollisionKind,
    /// Tool tip, work coordinates.
    pub at: Vec3,
    /// The deepest overlap in the move (mm).
    pub depth: f64,
}

/// An operation to simulate.
pub struct SimOp<'a> {
    pub operation: u32,
    pub tool: &'a Tool,
    pub toolpath: &'a Toolpath,
}

/// Settings of a run.
#[derive(Clone, Copy, Debug)]
pub struct SimOptions {
    /// Cell size (mm).
    pub cell: f64,
    /// How deep an overlap counts as a collision (mm); `None`: one cell, at least 0.05.
    pub tolerance: Option<f64>,
    /// Stop after this many moves (the preview's scrubber); `None` runs everything.
    pub until: Option<usize>,
    /// Work Z the tool starts each operation at (the setup's clearance height).
    pub clearance: f64,
}

/// Volumes (mm³) after the run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Volumes {
    pub stock: f64,
    pub removed: f64,
    pub remaining: f64,
    /// Part material (inside the stock).
    pub part: f64,
    /// Stock that is not part and still stands: what is left to cut away.
    pub leftover: f64,
    /// Part material that has been cut away.
    pub gouged: f64,
}

/// The state after a run.
#[derive(Clone, Debug)]
pub struct Simulation {
    pub grid: Grid,
    /// Stock top per cell, row by row from the low corner.
    pub heights: Vec<f32>,
    pub floor: f64,
    pub top: f64,
    pub collisions: Vec<Collision>,
    /// How many collisions of each kind (gouge, shank, rapid, floor), also past the listed ones.
    pub counts: [usize; 4],
    /// Moves simulated, and moves in all.
    pub steps: usize,
    pub total_steps: usize,
    /// First move number of each operation, in the order given.
    pub starts: Vec<(u32, usize)>,
    pub volumes: Volumes,
    pub tolerance: f64,
    pub open_cells: usize,
}

struct Collector {
    list: Vec<Collision>,
    counts: [usize; 4],
}

impl Collector {
    fn note(&mut self, c: Collision) {
        if let Some(last) = self.list.iter_mut().rev().take(4).find(|l| l.operation == c.operation && l.step == c.step && l.kind == c.kind) {
            last.depth = last.depth.max(c.depth);
            return;
        }
        if let Some(n) = self.counts.get_mut(c.kind.index()) {
            *n += 1;
        }
        if self.list.len() < MAX_COLLISIONS {
            self.list.push(c);
        }
    }
}

/// Runs `ops` (all in one setup) over `stock`, checking against `part` when there is one.
pub fn simulate(stock: &Stock, part: Option<&Part>, ops: &[SimOp], opts: &SimOptions) -> Result<Simulation> {
    let grid = Grid::new(stock, opts.cell)?;
    if let Some(p) = part
        && p.grid != grid
    {
        return invalid("the part was sampled on another grid than the stock");
    }
    finite(opts.clearance, "the clearance height")?;
    let tol = match opts.tolerance {
        Some(t) => {
            finite(t, "the tolerance")?;
            if !(1e-6..=10.0).contains(&t) {
                return invalid("the tolerance is between 0.000001 and 10 mm");
            }
            t
        }
        None => grid.cell.max(0.05),
    };
    let (floor, top) = (stock.min[2], stock.max[2]);
    let mut heights = vec![top as f32; grid.cells()];
    let mut coll = Collector { list: Vec::new(), counts: [0; 4] };
    let expanded: Vec<Vec<Move>> = ops.iter().map(|o| o.toolpath.expanded()).collect();
    let total: usize = expanded.iter().map(Vec::len).sum();
    let limit = opts.until.unwrap_or(total).min(total);
    let (mut step, mut work) = (0usize, 0.0f64);
    let mut starts = Vec::with_capacity(ops.len());
    'ops: for (op, moves) in ops.iter().zip(&expanded) {
        starts.push((op.operation, step));
        op.tool.validate()?;
        let cutter = Cutter::new(op.tool, grid.cell);
        let footprint = {
            let side = 2.0 * cutter.r.max(cutter.shank) / grid.cell + 1.0;
            side * side
        };
        let Some(first) = moves.iter().find_map(|m| match m {
            Move::Rapid { to } | Move::Feed { to, .. } => Some(*to),
            Move::Drill(_) => None,
        }) else {
            continue;
        };
        let mut at = Vec3::new(first.x, first.y, opts.clearance.max(first.z));
        for m in moves {
            if step >= limit {
                break 'ops;
            }
            let (to, rapid) = match m {
                Move::Rapid { to } => (*to, true),
                Move::Feed { to, .. } => (*to, false),
                Move::Drill(_) => continue,
            };
            if !to.is_finite() || to.x.abs() > MAX_COORD || to.y.abs() > MAX_COORD || to.z.abs() > MAX_COORD {
                return invalid("a toolpath move has a number that is not finite or is out of range");
            }
            let len = at.dist(to);
            let n = (len / (grid.cell * 0.5)).ceil().max(1.0);
            work += n * footprint * if rapid { 2.0 } else { 1.0 };
            if work > MAX_WORK {
                return Err(ManufactureError::Limit("the toolpaths are too long for this resolution; use a coarser one".into()));
            }
            let n = n as usize;
            let passes: &[Mode] = if rapid { &[Mode::Check, Mode::Cut] } else { &[Mode::Both] };
            for &mode in passes {
                for k in 1..=n {
                    let p = at + (to - at) * (k as f64 / n as f64);
                    stamp(&grid, &mut heights, floor, part, &cutter, p, rapid, mode, tol, (op.operation, step), &mut coll);
                }
            }
            at = to;
            step += 1;
        }
    }
    let volumes = volumes(&grid, &heights, floor, top, part);
    Ok(Simulation {
        grid,
        heights,
        floor,
        top,
        collisions: coll.list,
        counts: coll.counts,
        steps: step,
        total_steps: total,
        starts,
        volumes,
        tolerance: tol,
        open_cells: part.map_or(0, |p| p.open_cells),
    })
}

/// One move of a run, in work coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunSeg {
    pub from: Vec3,
    pub to: Vec3,
    pub rapid: bool,
    /// Feed (mm/min) of a cutting move; 0 for a rapid.
    pub feed: f64,
    /// Index into [`Runner::operations`].
    pub op: usize,
}

/// An operation of a run.
#[derive(Clone, Debug)]
pub struct RunOp {
    pub operation: u32,
    pub tool: Tool,
    /// First move number of the operation and how many moves it has.
    pub first_step: usize,
    pub steps: usize,
}

struct Checkpoint {
    step: usize,
    heights: Vec<f32>,
}

/// Memory kept for saved points of a run (bytes).
const CHECKPOINT_BYTES: usize = 96_000_000;

/// A stock-removal run to watch: it moves forward move by move and goes back to a saved point,
/// instead of starting over for every position of a scrubber. It only cuts (no part, no
/// collision checks), so it can run on a grid as fine as the picture needs.
pub struct Runner {
    grid: Grid,
    floor: f64,
    top: f64,
    ops: Vec<RunOp>,
    segs: Vec<RunSeg>,
    /// Seconds from the start to the end of each move.
    times: Vec<f64>,
    heights: Vec<f32>,
    step: usize,
    checkpoints: Vec<Checkpoint>,
    every: usize,
    max_checkpoints: usize,
}

impl Runner {
    /// A run of `ops` (one setup) over `stock` on cells of about `cell` mm. `rapid_rate` is the
    /// machine's rapid speed (mm/min) for the times.
    pub fn new(stock: &Stock, ops: &[SimOp], cell: f64, clearance: f64, rapid_rate: f64) -> Result<Runner> {
        let grid = Grid::new(stock, cell)?;
        finite(clearance, "the clearance height")?;
        let rate = if rapid_rate.is_finite() && rapid_rate > 0.0 { rapid_rate } else { 5000.0 };
        let (floor, top) = (stock.min[2], stock.max[2]);
        let mut segs: Vec<RunSeg> = Vec::new();
        let mut run_ops = Vec::with_capacity(ops.len());
        let mut times: Vec<f64> = Vec::new();
        let (mut clock, mut work) = (0.0f64, 0.0f64);
        for (index, op) in ops.iter().enumerate() {
            op.tool.validate()?;
            let moves = op.toolpath.expanded();
            let first_step = segs.len();
            let Some(first) = moves.iter().find_map(|m| match m {
                Move::Rapid { to } | Move::Feed { to, .. } => Some(*to),
                Move::Drill(_) => None,
            }) else {
                run_ops.push(RunOp { operation: op.operation, tool: op.tool.clone(), first_step, steps: 0 });
                continue;
            };
            let footprint = {
                let side = 2.0 * op.tool.radius().max(op.tool.shank_radius()) / grid.cell + 1.0;
                side * side
            };
            let mut at = Vec3::new(first.x, first.y, clearance.max(first.z));
            for m in &moves {
                let (to, rapid, feed) = match m {
                    Move::Rapid { to } => (*to, true, 0.0),
                    Move::Feed { to, feed } => (*to, false, *feed),
                    Move::Drill(_) => continue,
                };
                if !to.is_finite() || to.x.abs() > MAX_COORD || to.y.abs() > MAX_COORD || to.z.abs() > MAX_COORD {
                    return invalid("a toolpath move has a number that is not finite or is out of range");
                }
                let len = at.dist(to);
                if !(at.z >= top && to.z >= top) {
                    work += (len / (grid.cell * 0.5)).ceil().max(1.0) * footprint;
                }
                if work > MAX_WORK {
                    return Err(ManufactureError::Limit("the toolpaths are too long for this resolution; use a coarser one".into()));
                }
                let speed = if rapid { rate } else { feed };
                if speed > 0.0 {
                    clock += len / (speed / 60.0);
                }
                times.push(clock);
                segs.push(RunSeg { from: at, to, rapid, feed, op: index });
                at = to;
            }
            run_ops.push(RunOp { operation: op.operation, tool: op.tool.clone(), first_step, steps: segs.len() - first_step });
        }
        let cells = grid.cells();
        let max_checkpoints = (CHECKPOINT_BYTES / (cells.max(1) * 4)).clamp(2, 40);
        let every = (segs.len() / max_checkpoints).max(1);
        Ok(Runner {
            heights: vec![top as f32; cells],
            grid,
            floor,
            top,
            ops: run_ops,
            segs,
            times,
            step: 0,
            checkpoints: Vec::new(),
            every,
            max_checkpoints,
        })
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }
    /// The stock top per cell after [`Runner::step`] moves, row by row from the low corner.
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }
    pub fn step(&self) -> usize {
        self.step
    }
    pub fn total(&self) -> usize {
        self.segs.len()
    }
    pub fn operations(&self) -> &[RunOp] {
        &self.ops
    }
    pub fn segments(&self) -> &[RunSeg] {
        &self.segs
    }
    /// Seconds from the start to the end of each move.
    pub fn times(&self) -> &[f64] {
        &self.times
    }
    /// Seconds of the whole run.
    pub fn duration(&self) -> f64 {
        self.times.last().copied().unwrap_or(0.0)
    }
    /// How many moves are done at `seconds` (the move being run counts when it is under way).
    pub fn step_at(&self, seconds: f64) -> usize {
        self.times.partition_point(|t| *t <= seconds)
    }
    /// The volume cut away so far (mm³).
    pub fn removed(&self) -> f64 {
        let a = self.grid.cell * self.grid.cell;
        self.heights.iter().map(|h| (self.top - f64::from(*h).clamp(self.floor, self.top)) * a).sum()
    }

    /// Runs (or goes back) to `until` moves done.
    pub fn advance(&mut self, until: usize) {
        let until = until.min(self.segs.len());
        if until < self.step {
            match self.checkpoints.iter().rev().find(|c| c.step <= until) {
                Some(c) => {
                    self.heights.clone_from(&c.heights);
                    self.step = c.step;
                }
                None => {
                    self.heights.fill(self.top as f32);
                    self.step = 0;
                }
            }
        }
        let mut coll = Collector { list: Vec::new(), counts: [0; 4] };
        let mut cutter: Option<(usize, Cutter)> = None;
        while self.step < until {
            let Some(seg) = self.segs.get(self.step).copied() else { break };
            if !(seg.from.z >= self.top && seg.to.z >= self.top) {
                if cutter.as_ref().is_none_or(|(o, _)| *o != seg.op) {
                    cutter = self.ops.get(seg.op).map(|o| (seg.op, Cutter::new(&o.tool, self.grid.cell)));
                }
                if let Some((_, c)) = &cutter {
                    let n = (seg.from.dist(seg.to) / (self.grid.cell * 0.5)).ceil().max(1.0);
                    let count = n as usize;
                    for k in 1..=count {
                        let p = seg.from + (seg.to - seg.from) * (k as f64 / n);
                        stamp(&self.grid, &mut self.heights, self.floor, None, c, p, seg.rapid, Mode::Cut, 0.0, (0, self.step), &mut coll);
                    }
                }
            }
            self.step += 1;
            if self.step.is_multiple_of(self.every)
                && self.checkpoints.len() < self.max_checkpoints
                && self.checkpoints.last().is_none_or(|c| c.step < self.step)
            {
                self.checkpoints.push(Checkpoint { step: self.step, heights: self.heights.clone() });
            }
        }
    }
}

/// A tool as the sweep sees it.
struct Cutter<'a> {
    tool: &'a Tool,
    /// Radius of the cutting part, and of the part that is checked (a cell and a half smaller
    /// at most, so a tool riding a wall is not reported for the grid's roughness).
    r: f64,
    r_check: f64,
    shank: f64,
    shank_check: f64,
    flute: Option<f64>,
}

impl<'a> Cutter<'a> {
    fn new(tool: &'a Tool, cell: f64) -> Self {
        let margin = (cell * 0.75).min(tool.radius() * 0.5);
        let shank = tool.shank_radius();
        Cutter {
            tool,
            r: tool.radius(),
            r_check: tool.radius() - margin,
            shank,
            shank_check: shank - (cell * 0.75).min(shank * 0.5),
            flute: tool.flute_length,
        }
    }
}

/// What a pass over a tool position does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Looks for collisions only (a rapid is checked whole before it cuts anything).
    Check,
    /// Cuts only.
    Cut,
    /// Checks, then cuts (feed moves).
    Both,
}

#[allow(clippy::too_many_arguments)]
fn stamp(
    grid: &Grid,
    heights: &mut [f32],
    floor: f64,
    part: Option<&Part>,
    c: &Cutter,
    p: Vec3,
    rapid: bool,
    mode: Mode,
    tol: f64,
    (operation, step): (u32, usize),
    coll: &mut Collector,
) {
    let (check, cut) = (mode != Mode::Cut, mode != Mode::Check);
    let reach = c.r.max(c.shank);
    let ix0 = (((p.x - reach - grid.x0) / grid.cell).floor().max(0.0)) as usize;
    let iy0 = (((p.y - reach - grid.y0) / grid.cell).floor().max(0.0)) as usize;
    let ix1 = ((((p.x + reach - grid.x0) / grid.cell).floor() + 1.0).max(0.0)).min(grid.nx as f64) as usize;
    let iy1 = ((((p.y + reach - grid.y0) / grid.cell).floor() + 1.0).max(0.0)).min(grid.ny as f64) as usize;
    let flute_top = c.flute.map(|l| p.z + l);
    let (mut gouge, mut shank, mut rapid_hit) = (0.0f64, 0.0f64, 0.0f64);
    for iy in iy0..iy1 {
        let dy = grid.cy(iy) - p.y;
        for ix in ix0..ix1 {
            let dx = grid.cx(ix) - p.x;
            let r = (dx * dx + dy * dy).sqrt();
            let i = iy * grid.nx + ix;
            let Some(h) = heights.get_mut(i) else { continue };
            let top = f64::from(*h);
            if check {
                if r <= c.r_check
                    && let Some(part) = part
                {
                    let surf = p.z + c.tool.lift(r);
                    // Rapids and the flute-less tool crash through everything above the tip.
                    let upper = if rapid { f64::INFINITY } else { flute_top.unwrap_or(f64::INFINITY) };
                    for s in part.at(i) {
                        let overlap = f64::from(s[1]).min(upper) - f64::from(s[0]).max(surf);
                        if overlap > tol {
                            if rapid {
                                rapid_hit = rapid_hit.max(overlap);
                            } else {
                                gouge = gouge.max(overlap);
                            }
                        }
                    }
                }
                // The stock is exact, so it is compared at the tool's full size: a cell is
                // checked the first time the tool reaches it, before it has been cut.
                if rapid && r <= c.r {
                    let d = top - (p.z + c.tool.lift(r)).max(floor);
                    if d > tol {
                        rapid_hit = rapid_hit.max(d);
                    }
                }
                if let Some(zlow) = flute_top
                    && r <= c.shank
                    && !rapid
                {
                    // The shank is where the flutes end: stock still standing above that is hit by it.
                    let d = top - zlow;
                    if d > tol {
                        shank = shank.max(d);
                    }
                    if r <= c.shank_check
                        && let Some(part) = part
                    {
                        for s in part.at(i) {
                            let overlap = f64::from(s[1]) - f64::from(s[0]).max(zlow);
                            if overlap > tol {
                                shank = shank.max(overlap);
                            }
                        }
                    }
                }
            }
            if cut && r <= c.r {
                let surf = p.z + c.tool.lift(r);
                if surf < top {
                    *h = surf.max(floor) as f32;
                }
            }
        }
    }
    for (kind, depth) in [(CollisionKind::Gouge, gouge), (CollisionKind::Shank, shank), (CollisionKind::Rapid, rapid_hit)] {
        if depth > 0.0 {
            coll.note(Collision { operation, step, kind, at: p, depth });
        }
    }
    let inside = p.x >= grid.x0 && p.y >= grid.y0 && p.x < grid.x0 + grid.nx as f64 * grid.cell && p.y < grid.y0 + grid.ny as f64 * grid.cell;
    if check && inside && floor - p.z > tol {
        coll.note(Collision { operation, step, kind: CollisionKind::Floor, at: p, depth: floor - p.z });
    }
}

fn volumes(grid: &Grid, heights: &[f32], floor: f64, top: f64, part: Option<&Part>) -> Volumes {
    let a = grid.cell * grid.cell;
    let (mut remaining, mut part_v, mut leftover, mut gouged) = (0.0, 0.0, 0.0, 0.0);
    for (i, h) in heights.iter().enumerate() {
        let h = f64::from(*h).clamp(floor, top);
        remaining += h - floor;
        let mut inside_stock = 0.0; // part inside the stock column
        let mut part_standing = 0.0; // of it, below the current top
        if let Some(part) = part {
            for s in part.at(i) {
                let (lo, hi) = (f64::from(s[0]).max(floor), f64::from(s[1]).min(top));
                if hi > lo {
                    inside_stock += hi - lo;
                    part_standing += (hi.min(h) - lo).max(0.0);
                }
            }
        }
        part_v += inside_stock;
        leftover += (h - floor) - part_standing;
        gouged += inside_stock - part_standing;
    }
    let stock = (top - floor) * a * heights.len() as f64;
    Volumes { stock, removed: stock - remaining * a, remaining: remaining * a, part: part_v * a, leftover: leftover * a, gouged: gouged * a }
}

/// The stock surface at a coarser grid, for drawing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Surface {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    /// Heights row by row, rounded to 0.01 mm.
    pub z: Vec<f32>,
}

impl Simulation {
    /// The stock top averaged into blocks, at most `max_cells` of them.
    pub fn surface(&self, max_cells: usize) -> Surface {
        let g = &self.grid;
        let mut f = 1usize;
        while g.nx.div_ceil(f) * g.ny.div_ceil(f) > max_cells.max(4) {
            f += 1;
        }
        let (nx, ny) = (g.nx.div_ceil(f), g.ny.div_ceil(f));
        let mut z = Vec::with_capacity(nx * ny);
        for by in 0..ny {
            for bx in 0..nx {
                let (mut sum, mut n) = (0.0f64, 0u32);
                for iy in by * f..((by + 1) * f).min(g.ny) {
                    for ix in bx * f..((bx + 1) * f).min(g.nx) {
                        sum += f64::from(self.heights.get(iy * g.nx + ix).copied().unwrap_or(0.0));
                        n += 1;
                    }
                }
                z.push(((sum / f64::from(n.max(1))) * 100.0).round() as f32 / 100.0);
            }
        }
        Surface { x0: g.x0, y0: g.y0, cell: g.cell * f as f64, nx, ny, z }
    }
}
