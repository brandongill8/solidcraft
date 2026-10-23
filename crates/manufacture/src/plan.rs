//! Toolpath planning: turns one operation, with the outlines it cuts (already in the setup's
//! work coordinates), into moves. 2.5D: every cut is a path at constant Z, passes step down.
//!
//! Conventions: Z is the tool axis, up is positive; the tool is a clockwise-spinning mill, so
//! climb milling keeps the material on the left of the travel. Rapids between features go up to
//! the setup's clearance height first, so no rapid crosses the part.

use std::cell::Cell;

use solidcraft_geom::{Loop2, MAX_BEZIER_CTRL, Plane, Region2, Seg2, Vec2, Vec3};

use crate::error::{ManufactureError, Result, invalid};
use crate::job::{Direction, DrillPoints, Entry, EntryMode, OpKind, Operation, Setup, Side};
use crate::offset::{Clearance, Curve, inset};
use crate::path::{DrillCycle, Move, Toolpath};
use crate::ring::{all_finite, dedupe, dist_to_segment, perimeter, signed_area};
use crate::tool::Tool;

/// Default chord tolerance for curved outlines (mm).
pub const DEFAULT_TOLERANCE: f64 = 0.005;
/// Most moves one operation may produce.
pub const MAX_MOVES: usize = 4_000_000;
/// Most depth passes one operation may have.
pub const MAX_LEVELS: usize = 2000;
/// Most concentric passes one pocket may have.
pub const MAX_STAGES: usize = 3000;

/// A hole to drill: where, and its diameter when it is known (a circle in a sketch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Site {
    pub at: Vec2,
    pub diameter: Option<f64>,
}

/// What an operation cuts, in the setup's work coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub regions: Vec<Region2>,
    pub sites: Vec<Site>,
}

/// A planned operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub toolpath: Toolpath,
    /// Things the user should know (a tool too big for a feature, a hole that needs milling…).
    pub warnings: Vec<String>,
}

/// The centre and radius of a loop that is one full circle.
pub fn circle_of(l: &Loop2) -> Option<(Vec2, f64)> {
    let mut c: Option<(Vec2, f64)> = None;
    let mut sweep = 0.0;
    for s in &l.segs {
        let Seg2::Arc { center, radius, sweep: sw, .. } = *s else { return None };
        match c {
            None => c = Some((center, radius.abs())),
            Some((c0, r0)) => {
                if c0.dist(center) > 1e-6 * r0.max(1.0) || (r0 - radius.abs()).abs() > 1e-6 * r0.max(1.0) {
                    return None;
                }
            }
        }
        sweep += sw.abs();
    }
    let (c, r) = c?;
    ((sweep - std::f64::consts::TAU).abs() < 1e-6 && r > 0.0).then_some((c, r))
}

/// The drilling sites of regions: every region whose outer outline is a circle.
pub fn circle_sites(regions: &[Region2]) -> Vec<Site> {
    regions.iter().filter_map(|r| circle_of(&r.outer)).map(|(c, rad)| Site { at: c, diameter: Some(rad * 2.0) }).collect()
}

/// A region drawn in `plane` (a sketch's plane), in `setup`'s work coordinates. The plane must be
/// parallel to the setup's XY plane: 2.5D cuts live in planes the tool axis is perpendicular to.
/// Also returns the work Z of the plane. A setup whose frame is mirrored against the sketch flips
/// the region; the planner reorients outlines itself.
pub fn region_to_work(region: &Region2, plane: &Plane, setup: &Setup) -> Result<(Region2, f64)> {
    let n = plane.normal();
    let axis = setup.tool_axis();
    let along = n.dot(axis);
    if !(along.abs() >= 1.0 - 1e-6) {
        let tilt = along.abs().clamp(0.0, 1.0).acos().to_degrees();
        return invalid(format!(
            "the sketch is tilted {tilt:.1} degrees to the setup's XY plane: use a setup whose frame lies in the sketch's plane (or a parallel one)"
        ));
    }
    let map = |p: Vec2| {
        let w = setup.to_local(plane.to_world(p));
        Vec2::new(w.x, w.y)
    };
    let (o, ex, ey) = (map(Vec2::ZERO), map(Vec2::X), map(Vec2::Y));
    let flip = (ex - o).cross(ey - o) < 0.0;
    let seg = |s: &Seg2| -> Seg2 {
        match *s {
            Seg2::Line { a, b } => Seg2::Line { a: map(a), b: map(b) },
            Seg2::Arc { center, radius, start, sweep } => {
                let c = map(center);
                let p = map(center + Vec2::from_angle(start) * radius);
                Seg2::Arc { center: c, radius, start: (p - c).angle(), sweep: if flip { -sweep } else { sweep } }
            }
            Seg2::Cubic { p0, p1, p2, p3 } => Seg2::Cubic { p0: map(p0), p1: map(p1), p2: map(p2), p3: map(p3) },
            Seg2::Conic { a, apex, b, w } => Seg2::Conic { a: map(a), apex: map(apex), b: map(b), w },
            Seg2::Bezier { n, p } => {
                let mut q = [Vec2::ZERO; MAX_BEZIER_CTRL];
                for (d, v) in q.iter_mut().zip(p.iter()) {
                    *d = map(*v);
                }
                Seg2::Bezier { n, p: q }
            }
        }
    };
    let lp = |l: &Loop2| Loop2 { segs: l.segs.iter().map(&seg).collect() };
    let z = setup.to_local(plane.origin).z;
    Ok((Region2 { outer: lp(&region.outer), holes: region.holes.iter().map(lp).collect() }, z))
}

/// Plans `op` with `tool` in `setup` over `geometry`. `tolerance` bounds the chord error of
/// curved outlines and offsets.
pub fn plan(setup: &Setup, tool: &Tool, op: &Operation, geometry: &Geometry, tolerance: f64) -> Result<Plan> {
    op.validate(tool, setup)?;
    tool.validate()?;
    if !(tolerance.is_finite() && (1e-4..=1.0).contains(&tolerance)) {
        return invalid("the tolerance is between 0.0001 and 1 mm");
    }
    let mut pen = Pen::new(setup.clearance);
    let mut warnings = Vec::new();
    let fallbacks = Cell::new(0usize);
    if matches!(tool.kind, crate::tool::ToolKind::BallEndMill | crate::tool::ToolKind::BullNoseEndMill) && !matches!(op.kind, OpKind::Drill { .. }) {
        warnings.push(format!("a {} is planned as if its whole diameter cut at every depth; walls will not be square", tool.kind.label()));
    }
    match &op.kind {
        OpKind::Face { margin, stepover } => face(&mut pen, setup, tool, op, *margin, *stepover)?,
        OpKind::Contour { side, direction, allowance, .. } => {
            let entry = op.kind.entry().unwrap_or(PLUNGE);
            contour(&mut pen, tool, op, geometry, tolerance, *side, *direction, *allowance, entry, &fallbacks, &mut warnings)?
        }
        OpKind::Pocket { direction, stepover, allowance, finish, .. } => {
            let entry = op.kind.entry().unwrap_or(PLUNGE);
            pocket(&mut pen, tool, op, geometry, tolerance, *direction, *stepover, *allowance, *finish, entry, &fallbacks, &mut warnings)?
        }
        OpKind::Drill { points, peck, dwell } => drill(&mut pen, tool, op, geometry, points, *peck, *dwell, &mut warnings)?,
    }
    pen.check()?;
    if fallbacks.get() > 0 {
        warnings.push(format!(
            "a helical entry did not fit in {} place(s) (the outline is too narrow for the helix); a ramp was used there instead",
            fallbacks.get()
        ));
    }
    Ok(Plan { toolpath: Toolpath { moves: pen.moves }, warnings })
}

/// Entry used where an operation has none (facing, drilling): never read.
const PLUNGE: Entry = Entry { mode: EntryMode::Plunge, ramp_angle: 0.0, helix_angle: 3.0, helix_diameter: 0.0 };

/// The depth of each pass, from the first below `top` down to `bottom` (equal steps no deeper
/// than `stepdown`).
pub fn levels(top: f64, bottom: f64, stepdown: f64) -> Result<Vec<f64>> {
    let depth = top - bottom;
    if !(depth > 0.0 && stepdown > 0.0 && depth.is_finite() && stepdown.is_finite()) {
        return invalid("the depths are not usable");
    }
    let n = (depth / stepdown - 1e-9).ceil().max(1.0);
    if n > MAX_LEVELS as f64 {
        return Err(ManufactureError::Limit(format!("{n} depth passes (at most {MAX_LEVELS}); raise the stepdown")));
    }
    let n = n as usize;
    Ok((1..=n).map(|k| if k == n { bottom } else { top - depth * k as f64 / n as f64 }).collect())
}

/// Position-tracking move builder.
struct Pen {
    moves: Vec<Move>,
    at: Option<Vec3>,
    clearance: f64,
    over: bool,
}

impl Pen {
    fn new(clearance: f64) -> Pen {
        Pen { moves: Vec::new(), at: None, clearance, over: false }
    }
    fn push(&mut self, m: Move, to: Vec3) {
        if self.moves.len() >= MAX_MOVES {
            self.over = true;
            return;
        }
        self.moves.push(m);
        self.at = Some(to);
    }
    fn rapid(&mut self, to: Vec3) {
        self.push(Move::Rapid { to }, to);
    }
    fn feed(&mut self, to: Vec3, feed: f64) {
        self.push(Move::Feed { to, feed }, to);
    }
    fn xy(&self) -> Option<Vec2> {
        self.at.map(|p| Vec2::new(p.x, p.y))
    }
    /// Up to the clearance height, across, and down to `safe_z` above `p`.
    fn travel(&mut self, p: Vec2, safe_z: f64) {
        match self.at {
            Some(a) => {
                if a.z < self.clearance - 1e-9 {
                    self.rapid(Vec3::new(a.x, a.y, self.clearance));
                }
                if Vec2::new(a.x, a.y).dist(p) > 1e-9 {
                    self.rapid(Vec3::new(p.x, p.y, self.clearance));
                }
            }
            None => self.rapid(Vec3::new(p.x, p.y, self.clearance)),
        }
        if safe_z < self.clearance - 1e-9 {
            self.rapid(Vec3::new(p.x, p.y, safe_z));
        }
    }
    fn check(&self) -> Result<()> {
        if self.over {
            return Err(ManufactureError::Limit(format!("the operation needs more than {MAX_MOVES} moves; raise the tolerance or the stepover")));
        }
        Ok(())
    }
}

/// A region's outlines as polylines: the outer ring counter-clockwise, holes clockwise.
fn region_rings(r: &Region2, tol: f64) -> Result<(Vec<Vec2>, Vec<Vec<Vec2>>)> {
    let orient = |mut v: Vec<Vec2>, ccw: bool| {
        if (signed_area(&v) >= 0.0) != ccw {
            v.reverse();
        }
        v
    };
    let outer = dedupe(&r.outer.polyline(tol).into_vec(), 1e-9);
    if outer.len() < 3 || !all_finite(&outer) {
        return invalid("an outline is degenerate or has a number that is not finite");
    }
    let mut holes = Vec::new();
    for h in &r.holes {
        let p = dedupe(&h.polyline(tol).into_vec(), 1e-9);
        if p.len() >= 3 && all_finite(&p) {
            holes.push(orient(p, false));
        }
    }
    Ok((orient(outer, true), holes))
}

/// The ring rotated to start at the point of it nearest `p` (a vertex is reused, otherwise the
/// point is inserted).
fn rotate_at(ring: &[Vec2], p: Vec2) -> Vec<Vec2> {
    let n = ring.len();
    let mut best = (f64::INFINITY, 0usize);
    for i in 0..n {
        if let (Some(a), Some(b)) = (ring.get(i), ring.get((i + 1) % n)) {
            let d = dist_to_segment(p, *a, *b);
            if d < best.0 {
                best = (d, i);
            }
        }
    }
    let i = best.1;
    let (Some(a), Some(b)) = (ring.get(i), ring.get((i + 1) % n)) else { return ring.to_vec() };
    let ab = *b - *a;
    let l2 = ab.len2();
    let t = if l2 > 0.0 { ((p - *a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let q = *a + ab * t;
    let mut out = Vec::with_capacity(n + 1);
    if q.dist(*a) <= 1e-9 {
        out.extend((0..n).filter_map(|k| ring.get((i + k) % n).copied()));
    } else if q.dist(*b) <= 1e-9 {
        out.extend((0..n).filter_map(|k| ring.get((i + 1 + k) % n).copied()));
    } else {
        out.push(q);
        out.extend((1..=n).filter_map(|k| ring.get((i + k) % n).copied()));
    }
    out
}

/// One lap of the closed ring at depth `z`, from its first point back to it. The tool is at
/// the first point already, at `z`.
fn lap(pen: &mut Pen, ring: &[Vec2], z: f64, feed: f64) {
    for p in ring.iter().skip(1) {
        pen.feed(p.extend(z), feed);
    }
    if let Some(s) = ring.first() {
        pen.feed(s.extend(z), feed);
    }
}

/// How the tool goes down into the material, and what it needs to do so.
struct Dive<'a> {
    entry: Entry,
    /// Cutting feed (mm/min).
    feed: f64,
    /// Feed of a straight plunge.
    plunge: f64,
    /// Feed of a ramped or helical entry.
    entry_feed: f64,
    /// Where the tool centre may stand, which a helix has to stay inside; none when the tool
    /// centre follows the outline itself.
    clear: Option<&'a Clearance>,
    tool_radius: f64,
    tol: f64,
    /// How many helical entries did not fit and ramped instead.
    fallbacks: &'a Cell<usize>,
}

impl<'a> Dive<'a> {
    fn new(op: &Operation, tool: &Tool, entry: Entry, tol: f64, fallbacks: &'a Cell<usize>) -> Dive<'a> {
        Dive { entry, feed: op.feed, plunge: op.plunge(), entry_feed: op.entry_feed(), clear: None, tool_radius: tool.radius(), tol, fallbacks }
    }
    fn with_clearance(&self, clear: &'a Clearance) -> Dive<'a> {
        Dive { clear: Some(clear), ..*self }
    }
    /// The slope of a ramped entry (degrees); 0 plunges.
    fn ramp_angle(&self) -> f64 {
        match self.entry.mode {
            EntryMode::Plunge => 0.0,
            EntryMode::Ramp => self.entry.ramp_angle,
            // A helix that does not fit ramps along the outline at the helix's own slope.
            EntryMode::Helical => self.entry.helix_angle,
        }
    }
}

/// A circle the tool centre spirals down along.
struct Helix {
    centre: Vec2,
    radius: f64,
    /// +1 counter-clockwise, -1 clockwise.
    dir: f64,
}

/// Most turns one helical entry may take.
const MAX_HELIX_TURNS: f64 = 400.0;

/// The helix that starts at the ring's first point, tangent to the ring, on the side with room
/// for it; none when the outline is too narrow (or the tool centre follows the line itself).
fn helix_for(ring: &[Vec2], drop: f64, dive: &Dive) -> Option<Helix> {
    let s = *ring.first()?;
    let t = (*ring.get(1)? - s).normalized()?;
    helix_at(s, t, drop, dive)
}

/// The helix that starts at `s` going along `t`, tangent to the path, on the side with room for it.
fn helix_at(s: Vec2, t: Vec2, drop: f64, dive: &Dive) -> Option<Helix> {
    let clear = dive.clear?;
    let left = t.perp();
    let wanted = if dive.entry.helix_diameter > 0.0 { dive.entry.helix_diameter * 0.5 } else { dive.tool_radius * 0.9 };
    let slope = dive.entry.helix_angle.to_radians().tan();
    for shrink in [1.0, 0.8, 0.6, 0.45, 0.3] {
        let radius = wanted * shrink;
        if !(radius > 1e-3 && slope > 0.0) || drop / (std::f64::consts::TAU * radius * slope) > MAX_HELIX_TURNS {
            continue;
        }
        for side in [1.0, -1.0] {
            let centre = s + left * (side * radius);
            let fits = (0..36).all(|k| clear.ok(centre + Vec2::from_angle(std::f64::consts::TAU * f64::from(k) / 36.0) * radius));
            if !fits {
                continue;
            }
            // Turn the way the ring goes on from the helix's first point.
            let radial = (s - centre) * (1.0 / radius);
            let dir = if radial.perp().dot(t) >= 0.0 { 1.0 } else { -1.0 };
            return Some(Helix { centre, radius, dir });
        }
    }
    None
}

/// The ring turned to start where a helix fits, nearest `here` first (a corner never has room:
/// a circle touching one wall there cuts into the next). None when a helical entry is not asked
/// for, or no helix fits anywhere on the ring.
fn helical_start(ring: &[Vec2], here: Vec2, drop: f64, dive: &Dive) -> Option<Vec<Vec2>> {
    if dive.entry.mode != EntryMode::Helical || dive.clear.is_none() || drop <= 1e-9 {
        return None;
    }
    let n = ring.len();
    // The vertices and the middles of the edges, evenly thinned on a ring of many points.
    let stride = (n / 24).max(1);
    let mut candidates: Vec<(f64, Vec2)> = Vec::new();
    for i in (0..n).step_by(stride) {
        let (Some(&a), Some(&b)) = (ring.get(i), ring.get((i + 1) % n)) else { continue };
        let mid = a.lerp(b, 0.5);
        candidates.push((a.dist(here), a));
        candidates.push((mid.dist(here), mid));
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    candidates.into_iter().map(|(_, p)| rotate_at(ring, p)).find(|r| helix_for(r, drop, dive).is_some())
}

/// Spirals from the ring's first point at `z_from` down to `z`, ending above the same point,
/// then one flat circle at depth to true the floor of the hole.
fn helix_down(pen: &mut Pen, start: Vec2, h: &Helix, z_from: f64, z: f64, dive: &Dive) {
    let drop = z_from - z;
    let pitch = std::f64::consts::TAU * h.radius * dive.entry.helix_angle.to_radians().tan();
    let turns = (drop / pitch).ceil().clamp(1.0, MAX_HELIX_TURNS);
    let chord = (1.0 - dive.tol / h.radius).clamp(-1.0, 1.0).acos();
    let per_turn = if chord > 1e-6 { (std::f64::consts::TAU / chord).ceil().clamp(16.0, 180.0) } else { 180.0 };
    let steps = (turns * per_turn) as usize;
    let a0 = (start - h.centre).angle();
    for k in 1..=steps {
        if pen.over {
            return;
        }
        let f = k as f64 / steps as f64;
        let at = if k == steps { start } else { h.centre + Vec2::from_angle(a0 + h.dir * std::f64::consts::TAU * turns * f) * h.radius };
        pen.feed(at.extend(z_from - drop * f), dive.entry_feed);
    }
    let flat = per_turn as usize;
    for k in 1..=flat {
        let at = if k == flat { start } else { h.centre + Vec2::from_angle(a0 + h.dir * std::f64::consts::TAU * k as f64 / flat as f64) * h.radius };
        pen.feed(at.extend(z), dive.feed);
    }
}

/// Cuts one closed ring from `z_from` (where the tool stands above its first point) down to
/// `z`: a plunge, a ramp along the ring and then a full lap at depth, or a helix beside the ring
/// (a lap after it). Returns where the lap ended.
fn cut_ring(pen: &mut Pen, ring: &[Vec2], z_from: f64, z: f64, dive: &Dive) -> Vec2 {
    let n = ring.len();
    let Some(&start) = ring.first() else { return Vec2::ZERO };
    let drop = z_from - z;
    if dive.entry.mode == EntryMode::Helical && drop > 1e-9 {
        if let Some(h) = helix_for(ring, drop, dive) {
            helix_down(pen, start, &h, z_from, z, dive);
            lap(pen, ring, z, dive.feed);
            return start;
        }
        dive.fallbacks.set(dive.fallbacks.get() + 1);
    }
    let perim = perimeter(ring);
    let ramp_angle = dive.ramp_angle();
    let ramp_len = if ramp_angle > 0.0 && drop > 1e-9 { (drop / ramp_angle.to_radians().tan()).min(perim * 20.0) } else { 0.0 };
    if ramp_len <= 1e-9 || n < 3 {
        pen.feed(start.extend(z), dive.plunge);
        lap(pen, ring, z, dive.feed);
        return start;
    }
    let mut acc = 0.0;
    let mut ended: Option<(usize, Vec2)> = None;
    'laps: for _ in 0..=20 {
        for i in 0..n {
            let (Some(&a), Some(&b)) = (ring.get(i), ring.get((i + 1) % n)) else { continue };
            let len = a.dist(b);
            if len <= 0.0 {
                continue;
            }
            if acc + len < ramp_len - 1e-12 {
                acc += len;
                pen.feed(b.extend(z_from - drop * acc / ramp_len), dive.entry_feed);
            } else {
                let p = a + (b - a) * ((ramp_len - acc) / len);
                pen.feed(p.extend(z), dive.entry_feed);
                ended = Some((i, p));
                break 'laps;
            }
        }
    }
    let (i, p) = ended.unwrap_or((0, start));
    if let Some(&b) = ring.get((i + 1) % n) {
        if b.dist(p) > 1e-9 {
            pen.feed(b.extend(z), dive.feed);
        }
        for j in 2..=n {
            if let Some(v) = ring.get((i + j) % n) {
                pen.feed(v.extend(z), dive.feed);
            }
        }
    }
    pen.feed(p.extend(z), dive.feed);
    p
}

/// Skims the stock's top in parallel passes along X.
fn face(pen: &mut Pen, setup: &Setup, tool: &Tool, op: &Operation, margin: f64, stepover: f64) -> Result<()> {
    let Some(stock) = setup.stock else { return invalid("facing needs the setup's stock") };
    let r = tool.radius();
    let (x0, x1) = (stock.min[0] - margin, stock.max[0] + margin);
    let (y0, y1) = (stock.min[1] - margin, stock.max[1] + margin);
    let (xa, xb) = (x0 - r - 1.0, x1 + r + 1.0);
    let (first, last) = (y0 + r * 0.5, y1 - r * 0.5);
    let ys: Vec<f64> = if last <= first {
        vec![(y0 + y1) * 0.5]
    } else {
        let n = ((last - first) / (stepover * tool.diameter) - 1e-9).ceil().max(1.0);
        if n > 100_000.0 {
            return Err(ManufactureError::Limit("too many facing passes; raise the stepover".into()));
        }
        let n = n as usize;
        (0..=n).map(|k| first + (last - first) * k as f64 / n as f64).collect()
    };
    let safe = op.top + op.retract;
    for z in levels(op.top, op.bottom, op.stepdown)? {
        let mut forward = true;
        for (k, y) in ys.iter().enumerate() {
            let (from, to) = if forward { (xa, xb) } else { (xb, xa) };
            if k == 0 {
                pen.travel(Vec2::new(from, *y), safe);
                // Outside the stock there is nothing to hit: straight down.
                pen.rapid(Vec3::new(from, *y, z));
            } else {
                pen.feed(Vec3::new(from, *y, z), op.feed);
            }
            pen.feed(Vec3::new(to, *y, z), op.feed);
            forward = !forward;
        }
        pen.check()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn contour(
    pen: &mut Pen,
    tool: &Tool,
    op: &Operation,
    geometry: &Geometry,
    tol: f64,
    side: Side,
    direction: Direction,
    allowance: f64,
    entry: Entry,
    fallbacks: &Cell<usize>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if geometry.regions.is_empty() {
        return invalid("the contour has no closed outline to follow");
    }
    let d = tool.radius() + allowance;
    let dive = Dive::new(op, tool, entry, tol, fallbacks);
    // Where the tool centre may stand, for each region whose outline is offset (a helix has to
    // stay inside it).
    let mut clears: Vec<Clearance> = Vec::new();
    let mut curves: Vec<(Curve, Option<usize>)> = Vec::new();
    for region in &geometry.regions {
        let (outer, holes) = region_rings(region, tol)?;
        let mut clear_at: Option<usize> = None;
        let found = match side {
            Side::On => {
                let mut v = vec![Curve { points: outer, closed: true }];
                v.extend(holes.into_iter().map(|h| Curve { points: h, closed: true }));
                // Outer counter-clockwise and holes clockwise keep the material on the left.
                if direction == Direction::Conventional {
                    for c in &mut v {
                        c.points.reverse();
                    }
                }
                v
            }
            Side::Outside | Side::Inside => {
                let rings: Vec<Vec<Vec2>> = if side == Side::Outside {
                    // The free space is outside the part and inside its holes.
                    std::iter::once(outer)
                        .chain(holes)
                        .map(|mut r| {
                            r.reverse();
                            r
                        })
                        .collect()
                } else {
                    std::iter::once(outer).chain(holes).collect()
                };
                let mut v = inset(&rings, d, tol)?;
                // Around the part the free space is outside the rings; inside, within them.
                let room = Clearance::new(&rings, d);
                clears.push(if side == Side::Outside { room.complement() } else { room });
                clear_at = Some(clears.len() - 1);
                // The offset keeps free space on the left; climb wants the material there.
                if direction == Direction::Climb {
                    for c in &mut v {
                        c.points.reverse();
                    }
                }
                v
            }
        };
        if found.is_empty() {
            warnings.push(format!("the {} mm tool does not fit an outline of the contour (it is narrower than the tool)", tool.diameter));
        }
        curves.extend(found.into_iter().map(|c| (c, clear_at)));
    }
    if curves.is_empty() {
        return invalid("no contour toolpath: the tool does not fit the outline");
    }
    // Inner outlines first, so the part stays attached to the stock for as long as possible.
    curves.sort_by(|a, b| perimeter(&a.0.points).total_cmp(&perimeter(&b.0.points)));
    let safe = op.top + op.retract;
    let feed_plane = op.top + op.retract.min(1.0);
    let lv = levels(op.top, op.bottom, op.stepdown)?;
    for (curve, clear_at) in curves {
        if curve.points.len() < 2 {
            continue;
        }
        let dive = match clear_at.and_then(|i| clears.get(i)) {
            Some(c) => dive.with_clearance(c),
            None => Dive::new(op, tool, entry, tol, fallbacks),
        };
        let from = pen.xy().unwrap_or(Vec2::ZERO);
        if curve.closed {
            let mut ring = curve.points;
            ring = rotate_at(&ring, from);
            if let Some(&first_depth) = lv.first() {
                ring = helical_start(&ring, from, feed_plane - first_depth, &dive).unwrap_or(ring);
            }
            let Some(&start) = ring.first() else { continue };
            pen.travel(start, safe);
            pen.rapid(start.extend(feed_plane));
            let mut z_prev = feed_plane;
            for z in &lv {
                let end = cut_ring(pen, &ring, z_prev, *z, &dive);
                ring = rotate_at(&ring, end);
                z_prev = *z;
            }
        } else {
            // An open chain: cut it level by level, always in the same direction.
            let Some(&start) = curve.points.first() else { continue };
            for z in &lv {
                pen.travel(start, safe);
                pen.rapid(start.extend(feed_plane));
                pen.feed(start.extend(*z), op.plunge());
                for p in curve.points.iter().skip(1) {
                    pen.feed(p.extend(*z), op.feed);
                }
            }
        }
        pen.check()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pocket(
    pen: &mut Pen,
    tool: &Tool,
    op: &Operation,
    geometry: &Geometry,
    tol: f64,
    direction: Direction,
    stepover: f64,
    allowance: f64,
    finish: bool,
    entry: Entry,
    fallbacks: &Cell<usize>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if geometry.regions.is_empty() {
        return invalid("the pocket has no closed outline to clear");
    }
    let r = tool.radius();
    let d0 = r + allowance;
    let step = stepover * tool.diameter;
    let safe = op.top + op.retract;
    let feed_plane = op.top + op.retract.min(1.0);
    let lv = levels(op.top, op.bottom, op.stepdown)?;
    let mut any = false;
    if finish && allowance <= 0.0 {
        warnings.push("a finish pass needs an allowance on the walls; none is left, so none is cut".into());
    }
    for region in &geometry.regions {
        let (outer, holes) = region_rings(region, tol)?;
        let rings: Vec<Vec<Vec2>> = std::iter::once(outer).chain(holes).collect();
        // The concentric passes, outermost first.
        let mut stages: Vec<Vec<Curve>> = Vec::new();
        for k in 0..MAX_STAGES {
            let found = inset(&rings, d0 + step * k as f64, tol)?;
            if found.is_empty() {
                break;
            }
            if k + 1 == MAX_STAGES {
                return Err(ManufactureError::Limit(format!("a pocket of more than {MAX_STAGES} passes; raise the stepover")));
            }
            stages.push(found);
        }
        if stages.is_empty() {
            warnings.push(format!("the {} mm tool does not fit a pocket outline", tool.diameter));
            continue;
        }
        any = true;
        if direction == Direction::Climb {
            for c in stages.iter_mut().flatten() {
                c.points.reverse();
            }
        }
        let clear = Clearance::new(&rings, d0);
        let dive = Dive::new(op, tool, entry, tol, fallbacks).with_clearance(&clear);
        let link_ok = |a: Vec2, b: Vec2| -> bool {
            let len = a.dist(b);
            if len > step * 2.5 + 1e-9 {
                return false;
            }
            let n = ((len / (step * 0.5).max(1e-3)).ceil() as usize).clamp(1, 64);
            (0..=n).all(|i| clear.ok(a + (b - a) * (i as f64 / n as f64)))
        };
        for (li, z) in lv.iter().enumerate() {
            let z_prev = if li == 0 { feed_plane } else { lv.get(li - 1).copied().unwrap_or(feed_plane) };
            let mut last_end: Option<Vec2> = None;
            for stage in &stages {
                let mut remaining: Vec<&Curve> = stage.iter().filter(|c| c.closed && c.points.len() >= 3).collect();
                while !remaining.is_empty() {
                    let here = last_end.or_else(|| pen.xy()).unwrap_or(Vec2::ZERO);
                    let pick = remaining
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (i, c.points.iter().map(|p| p.dist(here)).fold(f64::INFINITY, f64::min)))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let curve = remaining.swap_remove(pick);
                    let ring = rotate_at(&curve.points, here);
                    let Some(&start) = ring.first() else { continue };
                    let end = if last_end.is_some_and(|e| link_ok(e, start)) {
                        // Stay down: the strip between the two passes is already cut.
                        pen.feed(start.extend(*z), op.feed);
                        lap(pen, &ring, *z, op.feed);
                        start
                    } else {
                        let ring = helical_start(&ring, here, z_prev - *z, &dive).unwrap_or(ring);
                        let start = ring.first().copied().unwrap_or(start);
                        pen.travel(start, safe);
                        pen.rapid(start.extend(z_prev));
                        cut_ring(pen, &ring, z_prev, *z, &dive)
                    };
                    last_end = Some(end);
                    pen.check()?;
                }
            }
        }
        if finish && allowance > 0.0 {
            let mut walls = inset(&rings, r, tol)?;
            if direction == Direction::Climb {
                for c in &mut walls {
                    c.points.reverse();
                }
            }
            // The finish ring is a tool radius from the wall, closer than the passes before it.
            let wall_clear = Clearance::new(&rings, r);
            let wall_dive = Dive::new(op, tool, entry, tol, fallbacks).with_clearance(&wall_clear);
            for c in walls.into_iter().filter(|c| c.closed && c.points.len() >= 3) {
                let from = pen.xy().unwrap_or(Vec2::ZERO);
                let ring = rotate_at(&c.points, from);
                let ring = helical_start(&ring, from, feed_plane - op.bottom, &wall_dive).unwrap_or(ring);
                let Some(&start) = ring.first() else { continue };
                pen.travel(start, safe);
                pen.rapid(start.extend(feed_plane));
                cut_ring(pen, &ring, feed_plane, op.bottom, &wall_dive);
                pen.check()?;
            }
        }
    }
    if !any {
        return invalid("no pocket toolpath: the tool does not fit any outline");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn drill(
    pen: &mut Pen,
    tool: &Tool,
    op: &Operation,
    geometry: &Geometry,
    points: &DrillPoints,
    peck: Option<f64>,
    dwell: f64,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut sites: Vec<Site> = match points {
        DrillPoints::Sketch { .. } => geometry.sites.clone(),
        DrillPoints::Points { points } => points.iter().map(|p| Site { at: Vec2::new(p[0], p[1]), diameter: None }).collect(),
    };
    sites.retain(|s| s.at.is_finite());
    if sites.is_empty() {
        return invalid("the drill operation has no holes: the sketch needs circles, or give points");
    }
    for s in &sites {
        if let Some(dia) = s.diameter {
            if dia > tool.diameter * 1.05 {
                warnings.push(format!("a {dia:.3} mm hole is larger than the {} mm drill: it needs milling (a pocket or a contour)", tool.diameter));
            } else if dia < tool.diameter * 0.95 {
                warnings.push(format!("a {dia:.3} mm hole is smaller than the {} mm drill", tool.diameter));
            }
        }
    }
    warnings.dedup();
    let retract = op.top + op.retract;
    // Nearest next hole, starting from the work origin.
    let mut at = Vec2::ZERO;
    let mut left = sites;
    while !left.is_empty() {
        let i = left.iter().enumerate().min_by(|a, b| a.1.at.dist(at).total_cmp(&b.1.at.dist(at))).map(|(i, _)| i).unwrap_or(0);
        let s = left.swap_remove(i);
        if pen.at.is_none() {
            pen.rapid(Vec3::new(s.at.x, s.at.y, pen.clearance));
        }
        pen.push(
            Move::Drill(DrillCycle { x: s.at.x, y: s.at.y, top: op.top, bottom: op.bottom, retract, peck, dwell, feed: op.feed }),
            Vec3::new(s.at.x, s.at.y, retract),
        );
        at = s.at;
        pen.check()?;
    }
    Ok(())
}
