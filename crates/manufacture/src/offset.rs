//! Offsetting closed polylines, the heart of pocketing and cutter compensation.
//!
//! [`inset`] takes rings whose free space lies on their **left** (outer boundaries counter-
//! clockwise, islands clockwise; reverse them all to get the outside of a part) and returns the
//! curves of points exactly `d` away from the boundary, inside the free space. The method is the
//! robust one: shift every edge left by `d`, join reflex corners with round arcs, cut the raw curve
//! wherever it crosses itself or another raw curve, and keep only the pieces whose every point is
//! at least `d` from every original edge. Those pieces are the offset; they are chained back into
//! rings. Nothing here trusts its input: sizes and work are capped and every number is checked.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI};

use solidcraft_geom::Vec2;

use crate::error::{ManufactureError, Result, invalid};
use crate::ring::{all_finite, dedupe, dist_to_segment, perimeter, segment_hit, signed_area};

/// Most raw offset vertices one call will build across all rings.
pub const MAX_RAW_POINTS: usize = 600_000;
/// Most curve crossings one call will process.
pub const MAX_CROSSINGS: usize = 3_000_000;

/// One piece of an offset: a closed ring or an open chain, with free space on its left.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub points: Vec<Vec2>,
    pub closed: bool,
}

/// The curves at distance `d` inside the free space left of `rings`. `tol` bounds the chord
/// error of the round joins. An empty result means the free space is narrower than `2d`
/// everywhere.
pub fn inset(rings: &[Vec<Vec2>], d: f64, tol: f64) -> Result<Vec<Curve>> {
    if !(d.is_finite() && d > 0.0) {
        return invalid("the offset distance must be a positive number");
    }
    if !(tol.is_finite() && tol > 0.0) {
        return invalid("the tolerance must be a positive number");
    }
    if rings.iter().any(|r| !all_finite(r)) {
        return invalid("a boundary has a point that is not a finite number");
    }
    let rings: Vec<Vec<Vec2>> = rings.iter().map(|r| dedupe(r, 1e-9)).filter(|r| r.len() >= 3).collect();
    if rings.is_empty() {
        return Ok(Vec::new());
    }

    // The original edges, for the clearance test.
    let mut edges = Vec::new();
    for r in &rings {
        for (i, p) in r.iter().enumerate() {
            if let Some(q) = r.get((i + 1) % r.len()) {
                edges.push((*p, *q));
            }
        }
    }
    let index = EdgeIndex::new(edges, d);
    // Every point of a valid piece is at least `d` from the boundary: round joins are built
    // circumscribed (see `raw_offset`), so even the chords keep that distance.
    let clearance = d - (1e-7 * d).max(1e-7);
    let clear = |p: Vec2| !index.any_closer(p, clearance);

    // Raw offset rings.
    let raws: Vec<Vec<Vec2>> = rings.iter().map(|r| raw_offset(r, d, tol)).filter(|r| r.len() >= 2).collect();
    let total: usize = raws.iter().map(Vec::len).sum();
    if total > MAX_RAW_POINTS {
        return Err(ManufactureError::Limit(format!("the outline needs {total} points to offset (at most {MAX_RAW_POINTS}); raise the tolerance")));
    }

    // Segments of every raw ring, ordered for a sweep along x.
    struct Seg {
        a: Vec2,
        b: Vec2,
        ring: usize,
        local: usize,
        count: usize,
        min_x: f64,
        max_x: f64,
        min_y: f64,
        max_y: f64,
    }
    let mut segs: Vec<Seg> = Vec::with_capacity(total);
    let mut base = Vec::with_capacity(raws.len());
    for (ri, r) in raws.iter().enumerate() {
        base.push(segs.len());
        for (i, p) in r.iter().enumerate() {
            let q = r.get((i + 1) % r.len()).copied().unwrap_or(*p);
            segs.push(Seg {
                a: *p,
                b: q,
                ring: ri,
                local: i,
                count: r.len(),
                min_x: p.x.min(q.x),
                max_x: p.x.max(q.x),
                min_y: p.y.min(q.y),
                max_y: p.y.max(q.y),
            });
        }
    }
    let mut order: Vec<usize> = (0..segs.len()).collect();
    order.sort_by(|&i, &j| match (segs.get(i), segs.get(j)) {
        (Some(a), Some(b)) => a.min_x.total_cmp(&b.min_x),
        _ => std::cmp::Ordering::Equal,
    });

    let mut splits: Vec<Vec<(f64, Vec2)>> = vec![Vec::new(); segs.len()];
    let mut crossings = 0usize;
    for (oi, &i) in order.iter().enumerate() {
        let Some(si) = segs.get(i) else { continue };
        for &j in order.iter().skip(oi + 1) {
            let Some(sj) = segs.get(j) else { continue };
            if sj.min_x > si.max_x + 1e-9 {
                break;
            }
            if sj.min_y > si.max_y + 1e-9 || si.min_y > sj.max_y + 1e-9 {
                continue;
            }
            if si.ring == sj.ring {
                let (lo, hi) = (si.local.min(sj.local), si.local.max(sj.local));
                if hi - lo == 1 || (lo == 0 && hi + 1 == si.count) {
                    continue;
                }
            }
            let Some((t, u)) = segment_hit(si.a, si.b, sj.a, sj.b) else { continue };
            crossings += 1;
            if crossings > MAX_CROSSINGS {
                return Err(ManufactureError::Limit("the offset curves cross too many times; raise the tolerance".into()));
            }
            const E: f64 = 1e-12;
            let p = if t <= E {
                si.a
            } else if t >= 1.0 - E {
                si.b
            } else if u <= E {
                sj.a
            } else if u >= 1.0 - E {
                sj.b
            } else {
                si.a + (si.b - si.a) * t
            };
            if let Some(v) = splits.get_mut(i) {
                v.push((t, p));
            }
            if let Some(v) = splits.get_mut(j) {
                v.push((u, p));
            }
        }
    }

    // Each raw ring with its crossing points inserted and marked.
    let mut pieces: Vec<Vec<Vec2>> = Vec::new();
    let mut closed_rings: Vec<Vec<Vec2>> = Vec::new();
    for (ri, r) in raws.iter().enumerate() {
        let m = r.len();
        let b0 = base.get(ri).copied().unwrap_or(0);
        let mut flags = vec![false; m];
        let mut interior: Vec<Vec<(f64, Vec2)>> = vec![Vec::new(); m];
        for i in 0..m {
            let Some(list) = splits.get(b0 + i) else { continue };
            for &(t, p) in list {
                if t <= 1e-12 {
                    if let Some(f) = flags.get_mut(i) {
                        *f = true;
                    }
                } else if t >= 1.0 - 1e-12 {
                    if let Some(f) = flags.get_mut((i + 1) % m) {
                        *f = true;
                    }
                } else if let Some(v) = interior.get_mut(i) {
                    v.push((t, p));
                }
            }
        }
        let mut entries: Vec<(Vec2, bool)> = Vec::with_capacity(m);
        for i in 0..m {
            if let (Some(p), Some(f)) = (r.get(i), flags.get(i)) {
                entries.push((*p, *f));
            }
            if let Some(v) = interior.get_mut(i) {
                v.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut last: Option<Vec2> = None;
                for &(_, p) in v.iter() {
                    if last.is_none_or(|q| q.dist(p) > 1e-12) {
                        entries.push((p, true));
                        last = Some(p);
                    }
                }
            }
        }
        let nodes: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.1).map(|(i, _)| i).collect();
        let e = entries.len();
        if nodes.is_empty() {
            let pts: Vec<Vec2> = entries.iter().map(|x| x.0).collect();
            if valid(&pts, true, &clear) {
                closed_rings.push(pts);
            }
            continue;
        }
        for (k, &s) in nodes.iter().enumerate() {
            let t = nodes.get((k + 1) % nodes.len()).copied().unwrap_or(s);
            let len = if t > s { t - s } else { t + e - s };
            let len = if nodes.len() == 1 { e } else { len };
            let piece: Vec<Vec2> = (0..=len).filter_map(|o| entries.get((s + o) % e).map(|x| x.0)).collect();
            if valid(&piece, false, &clear) {
                pieces.push(piece);
            }
        }
    }

    let mut out: Vec<Curve> = closed_rings.into_iter().map(|points| Curve { points, closed: true }).collect();
    out.extend(chain(pieces));
    out.retain_mut(|c| {
        c.points = dedupe(&c.points, 1e-9);
        if !c.closed {
            return c.points.len() >= 2;
        }
        // Nearly collinear corners leave sub-micron loops behind; they are not toolpath.
        c.points.len() >= 3 && !(signed_area(&c.points).abs() < 1e-6 && perimeter(&c.points) < 0.01)
    });
    Ok(out)
}

/// Whether every vertex and every segment midpoint of the piece keeps its distance from the
/// original boundary.
fn valid(pts: &[Vec2], closed: bool, clear: &impl Fn(Vec2) -> bool) -> bool {
    let n = pts.len();
    for (i, p) in pts.iter().enumerate() {
        if !clear(*p) {
            return false;
        }
        let next = if closed { pts.get((i + 1) % n) } else { pts.get(i + 1) };
        if let Some(q) = next
            && !clear((*p + *q) * 0.5)
        {
            return false;
        }
    }
    true
}

/// The shifted edges of one ring, joined by round arcs where the boundary turns away from the
/// free space and by straight connectors (which cross, and are trimmed away) where it turns
/// toward it.
fn raw_offset(ring: &[Vec2], d: f64, tol: f64) -> Vec<Vec2> {
    let n = ring.len();
    let dirs: Vec<Vec2> = (0..n)
        .map(|i| match (ring.get(i), ring.get((i + 1) % n)) {
            (Some(a), Some(b)) => (*b - *a).normalized().unwrap_or(Vec2::ZERO),
            _ => Vec2::ZERO,
        })
        .collect();
    let step = if tol < d { 2.0 * (1.0 - tol / d).clamp(-1.0, 1.0).acos() } else { FRAC_PI_2 };
    let step = step.clamp(1e-3, FRAC_PI_2);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (Some(p0), Some(p1), Some(u), Some(u2)) = (ring.get(i), ring.get((i + 1) % n), dirs.get(i), dirs.get((i + 1) % n)) else { continue };
        let nrm = Vec2::new(-u.y, u.x);
        out.push(*p0 + nrm * d);
        out.push(*p1 + nrm * d);
        let cross = u.cross(*u2);
        let dot = u.dot(*u2);
        let flat = cross.abs() <= 1e-12;
        if cross < -1e-12 || (flat && dot < 0.0) {
            let sweep = if flat { -PI } else { cross.atan2(dot) };
            let steps = ((sweep.abs() / step).ceil() as usize).clamp(1, 4096);
            // The chords are tangent to the circle of radius `d` (vertices at mid angles on the
            // slightly larger circle through the tangent lines' corners), so the offset is never
            // closer to the boundary than `d`: the error leaves stock rather than gouging.
            let half = sweep.abs() / steps as f64 * 0.5;
            let radius = d / half.cos().max(0.1);
            let a0 = nrm.angle();
            for k in 0..steps {
                out.push(*p1 + Vec2::from_angle(a0 + sweep * (k as f64 + 0.5) / steps as f64) * radius);
            }
        }
    }
    dedupe(&out, 1e-9)
}

/// Joins directed pieces end to start into rings (or open chains where a joint is missing).
fn chain(pieces: Vec<Vec<Vec2>>) -> Vec<Curve> {
    const EPS: f64 = 1e-6;
    let key = |p: Vec2| ((p.x / EPS).floor() as i64, (p.y / EPS).floor() as i64);
    let mut starts: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, p) in pieces.iter().enumerate() {
        if let Some(s) = p.first() {
            starts.entry(key(*s)).or_default().push(i);
        }
    }
    let mut used = vec![false; pieces.len()];
    let mut out = Vec::new();
    for s in 0..pieces.len() {
        if used.get(s).copied().unwrap_or(true) {
            continue;
        }
        if let Some(u) = used.get_mut(s) {
            *u = true;
        }
        let Some(first_piece) = pieces.get(s) else { continue };
        let mut pts = first_piece.clone();
        let head = pts.first().copied().unwrap_or(Vec2::ZERO);
        let mut closed = false;
        loop {
            let end = pts.last().copied().unwrap_or(head);
            if pts.len() > 2 && end.dist(head) <= EPS {
                pts.pop();
                closed = true;
                break;
            }
            let (kx, ky) = key(end);
            let mut best: Option<(f64, usize)> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(list) = starts.get(&(kx + dx, ky + dy)) else { continue };
                    for &c in list {
                        if used.get(c).copied().unwrap_or(true) {
                            continue;
                        }
                        let Some(st) = pieces.get(c).and_then(|p| p.first()) else { continue };
                        let dist = st.dist(end);
                        if dist <= EPS && best.is_none_or(|(b, _)| dist < b) {
                            best = Some((dist, c));
                        }
                    }
                }
            }
            let Some((_, c)) = best else { break };
            if let Some(u) = used.get_mut(c) {
                *u = true;
            }
            if let Some(p) = pieces.get(c) {
                pts.extend(p.iter().skip(1).copied());
            }
        }
        out.push(Curve { points: pts, closed });
    }
    out
}

/// Whether a tool centre may stand at a point: inside the free space of `rings` (free space on
/// their left, so an even-odd count) and no closer than `d` to any edge.
pub(crate) struct Clearance {
    rings: Vec<Vec<Vec2>>,
    index: EdgeIndex,
    d: f64,
    /// The free space is the outside of the rings (an even count) rather than their inside.
    outside: bool,
}

impl Clearance {
    pub(crate) fn new(rings: &[Vec<Vec2>], d: f64) -> Clearance {
        let mut edges = Vec::new();
        for r in rings {
            for (i, p) in r.iter().enumerate() {
                if let Some(q) = r.get((i + 1) % r.len()) {
                    edges.push((*p, *q));
                }
            }
        }
        Clearance { rings: rings.to_vec(), index: EdgeIndex::new(edges, d.max(1e-6)), d, outside: false }
    }

    /// The same rings with the free space on the other side: around a part rather than inside it.
    pub(crate) fn complement(mut self) -> Clearance {
        self.outside = !self.outside;
        self
    }

    pub(crate) fn ok(&self, p: Vec2) -> bool {
        let crossings = self.rings.iter().filter(|r| crate::ring::contains(r, p)).count();
        (crossings % 2 == 1) != self.outside && !self.index.any_closer(p, self.d - (1e-7 * self.d).max(1e-7))
    }
}

/// A uniform grid over the original edges: is anything closer than `r` to a point?
struct EdgeIndex {
    edges: Vec<(Vec2, Vec2)>,
    min: Vec2,
    cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl EdgeIndex {
    fn new(edges: Vec<(Vec2, Vec2)>, hint: f64) -> EdgeIndex {
        let (mut lo, mut hi) = (Vec2::new(f64::INFINITY, f64::INFINITY), Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY));
        for (a, b) in &edges {
            lo = Vec2::new(lo.x.min(a.x).min(b.x), lo.y.min(a.y).min(b.y));
            hi = Vec2::new(hi.x.max(a.x).max(b.x), hi.y.max(a.y).max(b.y));
        }
        if edges.is_empty() {
            lo = Vec2::ZERO;
            hi = Vec2::ZERO;
        }
        let extent = (hi.x - lo.x).max(hi.y - lo.y).max(1e-9);
        let cell = hint.max(extent / 128.0).max(1e-9);
        let nx = (((hi.x - lo.x) / cell).floor() as usize + 1).min(256);
        let ny = (((hi.y - lo.y) / cell).floor() as usize + 1).min(256);
        let mut idx = EdgeIndex { edges, min: lo, cell, nx, ny, cells: vec![Vec::new(); nx * ny] };
        for e in 0..idx.edges.len() {
            let Some((a, b)) = idx.edges.get(e).copied() else { continue };
            let (x0, x1) = idx.span_x(a.x.min(b.x), a.x.max(b.x));
            let (y0, y1) = idx.span_y(a.y.min(b.y), a.y.max(b.y));
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    if let Some(c) = idx.cells.get_mut(cy * idx.nx + cx) {
                        c.push(e as u32);
                    }
                }
            }
        }
        idx
    }

    fn span_x(&self, lo: f64, hi: f64) -> (usize, usize) {
        let f = |v: f64| (((v - self.min.x) / self.cell).floor().max(0.0) as usize).min(self.nx - 1);
        (f(lo), f(hi))
    }
    fn span_y(&self, lo: f64, hi: f64) -> (usize, usize) {
        let f = |v: f64| (((v - self.min.y) / self.cell).floor().max(0.0) as usize).min(self.ny - 1);
        (f(lo), f(hi))
    }

    fn any_closer(&self, q: Vec2, r: f64) -> bool {
        let (x0, x1) = self.span_x(q.x - r, q.x + r);
        let (y0, y1) = self.span_y(q.y - r, q.y + r);
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let Some(c) = self.cells.get(cy * self.nx + cx) else { continue };
                for &e in c {
                    if let Some((a, b)) = self.edges.get(e as usize)
                        && dist_to_segment(q, *a, *b) < r
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}
