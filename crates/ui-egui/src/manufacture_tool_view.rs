//! The cutting tool drawn in the viewport at its real size, for every kind in the Tools window:
//! end mills (flat, ball, bull nose), drills (drill, spot drill), the reamer and the shell and
//! face mills with their inserts. The body is a surface of revolution made from the tool's own
//! numbers (diameter, flute length, shank, corner radius, point angle), the flutes and inserts
//! are drawn on it and turn with the spindle.

use egui::{Color32, Pos2, Shape, Stroke};
use solidcraft_engine::geom::Vec3;
use solidcraft_engine::manufacture::{Tool, ToolKind};

use crate::viewport::Proj;

/// Segments around the axis.
const AROUND: usize = 36;
/// Points along the rounded tip.
const TIP_POINTS: usize = 12;

/// How long the cutting part is drawn when the tool does not say (mm).
fn cutting_length(tool: &Tool) -> f64 {
    if let Some(l) = tool.flute_length {
        return l;
    }
    let d = tool.diameter;
    match tool.kind {
        ToolKind::Drill => (6.0 * d).min(80.0),
        ToolKind::SpotDrill => (2.0 * d).min(30.0),
        ToolKind::Reamer => (4.0 * d).min(70.0),
        ToolKind::ShellMill => (0.4 * d).clamp(8.0, 35.0),
        ToolKind::FaceMill => (0.25 * d).clamp(6.0, 20.0),
        _ => (3.0 * d).min(60.0),
    }
}

/// The shank radius drawn above the cutting part: a shell or face mill sits on an arbor.
fn shank_radius(tool: &Tool) -> f64 {
    match (tool.kind, tool.shank_diameter) {
        (_, Some(d)) => d * 0.5,
        (ToolKind::ShellMill | ToolKind::FaceMill, None) => tool.radius() * 0.4,
        _ => tool.radius(),
    }
}

/// How far a flute winds round the tool per unit of length (radians per mm at radius 1), by kind.
fn twist(tool: &Tool) -> f64 {
    let degrees: f64 = match tool.kind {
        ToolKind::Drill | ToolKind::SpotDrill => 30.0,
        ToolKind::Reamer => 8.0,
        _ => 35.0,
    };
    degrees.to_radians().tan()
}

/// The outline of the tool from the tip up, as (radius, height above the tip).
pub fn profile(tool: &Tool) -> Vec<(f64, f64)> {
    let big = tool.radius();
    let len = cutting_length(tool).max(tool.lift(big) + 0.1);
    let shank = shank_radius(tool);
    let shank_len = (2.0 * tool.diameter).clamp(12.0, 45.0);
    let mut out = Vec::with_capacity(TIP_POINTS + 6);
    for k in 0..=TIP_POINTS {
        let r = big * k as f64 / TIP_POINTS as f64;
        out.push((r, tool.lift(r)));
    }
    out.push((big, len));
    if (shank - big).abs() > 1e-9 {
        out.push((shank, len));
    }
    out.push((shank, len + shank_len));
    out.push((0.0, len + shank_len));
    out
}

struct View<'a> {
    painter: &'a egui::Painter,
    proj: &'a Proj,
    frame: &'a [Vec3; 4],
    tip: Vec3,
    /// Where the eye is looking along, in model space.
    dir: Vec3,
}

impl View<'_> {
    /// A point of the tool (x, y across the axis, z up from the tip) in model space.
    fn model(&self, x: f64, y: f64, z: f64) -> Vec3 {
        let w = self.tip + Vec3::new(x, y, z);
        self.frame[0] + self.frame[1] * w.x + self.frame[2] * w.y + self.frame[3] * w.z
    }
    /// A direction of the tool in model space.
    fn axes(&self, x: f64, y: f64, z: f64) -> Vec3 {
        self.frame[1] * x + self.frame[2] * y + self.frame[3] * z
    }
    fn screen(&self, p: Vec3) -> Option<Pos2> {
        self.proj.to_screen(p)
    }
    /// How squarely a surface with this normal faces the eye (0 to 1), and whether it does.
    fn facing(&self, normal: Vec3) -> (f32, bool) {
        let n = normal.normalized().unwrap_or(Vec3::Z);
        let d = n.dot(-self.dir);
        (d.abs() as f32, d > -0.05)
    }
}

fn steel(lit: f32) -> Color32 {
    let t = crate::theme::Tokens::get();
    let (dark, light) = (t.text_dim.gamma_multiply(0.6), t.text);
    dark.lerp_to_gamma(light, 0.2 + 0.8 * lit)
}

/// Draws `tool` with its tip at `tip` (work coordinates) and its flutes turned by `angle`
/// radians. `frame` takes work coordinates to model coordinates: origin and the X, Y, Z axes.
pub fn paint(painter: &egui::Painter, proj: &Proj, frame: &[Vec3; 4], tool: &Tool, tip: Vec3, angle: f64) {
    let (_, dir) = proj.ray(proj.rect.center());
    let v = View { painter, proj, frame, tip, dir };
    body(&v, tool);
    match tool.kind {
        ToolKind::ShellMill | ToolKind::FaceMill => inserts(&v, tool, angle),
        _ => flutes(&v, tool, angle),
    }
}

/// The turned surface: quads round the profile, far ones first.
fn body(v: &View, tool: &Tool) {
    let prof = profile(tool);
    // A normal for each profile point from the slope around it.
    let normals: Vec<(f64, f64)> = (0..prof.len())
        .map(|i| {
            let a = prof.get(i.saturating_sub(1)).copied().unwrap_or((0.0, 0.0));
            let b = prof.get((i + 1).min(prof.len() - 1)).copied().unwrap_or(a);
            let (dr, dz) = (b.0 - a.0, b.1 - a.1);
            let n = (dr * dr + dz * dz).sqrt().max(1e-12);
            (dz / n, -dr / n)
        })
        .collect();
    let mut quads: Vec<(f64, [Pos2; 4], Color32)> = Vec::with_capacity(prof.len() * AROUND);
    let (origin, _) = v.proj.ray(v.proj.rect.center());
    for i in 0..prof.len().saturating_sub(1) {
        let ((r0, z0), (r1, z1)) = (prof[i], prof[i + 1]);
        if (r1 - r0).abs() < 1e-12 && (z1 - z0).abs() < 1e-12 {
            continue;
        }
        let (n0, n1) = (normals[i], normals[i + 1]);
        for j in 0..AROUND {
            let (a0, a1) = (std::f64::consts::TAU * j as f64 / AROUND as f64, std::f64::consts::TAU * (j + 1) as f64 / AROUND as f64);
            let corners = [(r0, z0, a0), (r1, z1, a0), (r1, z1, a1), (r0, z0, a1)];
            let pts = corners.map(|(r, z, a)| v.model(r * a.cos(), r * a.sin(), z));
            let mid = (a0 + a1) * 0.5;
            let n = (n0.0 + n1.0, n0.1 + n1.1);
            let normal = v.axes(n.0 * mid.cos(), n.0 * mid.sin(), n.1);
            let (lit, front) = v.facing(normal);
            if !front {
                continue;
            }
            let mut screen = [Pos2::ZERO; 4];
            let mut ok = true;
            for (k, p) in pts.iter().enumerate() {
                match v.screen(*p) {
                    Some(sp) => screen[k] = sp,
                    None => ok = false,
                }
            }
            if ok {
                let centre = (pts[0] + pts[2]) * 0.5;
                quads.push(((centre - origin).dot(v.dir), screen, steel(lit)));
            }
        }
    }
    quads.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut mesh = egui::Mesh::default();
    for (_, q, colour) in &quads {
        let base = mesh.vertices.len() as u32;
        for p in q {
            mesh.colored_vertex(*p, *colour);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    v.painter.add(Shape::mesh(mesh));
}

/// The flutes of an end mill, drill or reamer: helical lines on the side, on the facing half.
fn flutes(v: &View, tool: &Tool, angle: f64) {
    let big = tool.radius();
    let len = cutting_length(tool);
    let start = tool.lift(big);
    let n = tool.flutes.clamp(1, 16) as usize;
    let wind = twist(tool) / big.max(1e-6);
    let steps = 28usize;
    for k in 0..n {
        let phase = angle + std::f64::consts::TAU * k as f64 / n as f64;
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() > 1 {
                v.painter.add(Shape::line(std::mem::take(run), Stroke::new(1.6, crate::theme::Tokens::get().body_edge)));
            }
            run.clear();
        };
        for s in 0..=steps {
            let z = start + (len - start) * s as f64 / steps as f64;
            let a = phase + wind * (z - start);
            let outward = v.axes(a.cos(), a.sin(), 0.0);
            let (_, front) = v.facing(outward);
            match (front, v.screen(v.model(big * a.cos(), big * a.sin(), z))) {
                (true, Some(p)) => run.push(p),
                _ => flush(&mut run),
            }
        }
        flush(&mut run);
    }
}

/// The inserts of a shell or face mill: a block on the rim for each tooth, turning with the
/// spindle.
fn inserts(v: &View, tool: &Tool, angle: f64) {
    let big = tool.radius();
    let n = tool.flutes.clamp(1, 16) as usize;
    let height = (cutting_length(tool) * 0.7).max(1.0);
    let width = (big * 0.16).clamp(2.0, 14.0);
    let half = (width / (2.0 * big)).clamp(0.01, 0.5).asin();
    let t = crate::theme::Tokens::get();
    let gold = t.warning;
    for k in 0..n {
        let centre = angle + std::f64::consts::TAU * k as f64 / n as f64;
        let outward = v.axes(centre.cos(), centre.sin(), 0.0);
        let (lit, front) = v.facing(outward);
        if !front {
            continue;
        }
        let corners = [(-half, 0.0), (half, 0.0), (half, height), (-half, height)];
        let pts: Vec<Pos2> =
            corners.iter().filter_map(|(da, z)| v.screen(v.model(big * (centre + da).cos(), big * (centre + da).sin(), *z))).collect();
        if pts.len() == 4 {
            let fill = gold.gamma_multiply(0.55).lerp_to_gamma(gold, 0.3 + 0.7 * lit);
            v.painter.add(Shape::convex_polygon(pts, fill, Stroke::new(1.0, t.body_edge)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(kind: ToolKind, diameter: f64) -> Tool {
        Tool {
            id: 1,
            name: "t".into(),
            kind,
            diameter,
            flutes: kind.default_flutes(),
            corner_radius: if kind == ToolKind::BullNoseEndMill { diameter * 0.2 } else { 0.0 },
            tip_angle: kind.default_tip_angle(),
            flute_length: None,
            shank_diameter: None,
        }
    }

    #[test]
    fn every_kind_is_drawn_at_its_own_size() {
        for kind in ToolKind::ALL {
            let t = tool(kind, 10.0);
            let p = profile(&t);
            assert!(p.len() > 4, "{kind:?}");
            let widest = p.iter().map(|(r, _)| *r).fold(0.0, f64::max);
            assert!((widest - 5.0).abs() < 1e-9, "{kind:?} is 10 mm across");
            assert!(p.iter().all(|(r, z)| r.is_finite() && z.is_finite() && *r >= 0.0 && *z >= 0.0));
            // Height only goes up from the tip.
            assert!(p.windows(2).skip(TIP_POINTS).all(|w| w[1].1 >= w[0].1 - 1e-9), "{kind:?}");
        }
    }

    #[test]
    fn the_tip_follows_the_kind() {
        let height = |kind: ToolKind| profile(&tool(kind, 10.0)).first().map(|p| p.1).unwrap_or(f64::NAN);
        let tip_edge = |kind: ToolKind| profile(&tool(kind, 10.0)).get(TIP_POINTS).map(|p| p.1).unwrap_or(f64::NAN);
        assert_eq!(height(ToolKind::FlatEndMill), 0.0);
        assert_eq!(tip_edge(ToolKind::FlatEndMill), 0.0);
        // A ball mill's edge is a radius up from its tip; a drill's follows the point angle.
        assert!((tip_edge(ToolKind::BallEndMill) - 5.0).abs() < 1e-9);
        assert!((tip_edge(ToolKind::Drill) - 5.0 / (59.0f64.to_radians()).tan()).abs() < 1e-6);
        assert!(tip_edge(ToolKind::SpotDrill) > tip_edge(ToolKind::Drill), "a 90 degree point is sharper, so taller, than 118");
        let bull = profile(&tool(ToolKind::BullNoseEndMill, 10.0));
        assert!(bull.iter().take(TIP_POINTS + 1).any(|(_, z)| *z == 0.0) && bull[TIP_POINTS].1 > 0.0, "flat, then a corner radius");
    }

    #[test]
    fn the_tool_uses_the_flute_and_shank_it_was_given() {
        let mut t = tool(ToolKind::FlatEndMill, 6.0);
        t.flute_length = Some(18.0);
        t.shank_diameter = Some(8.0);
        let p = profile(&t);
        assert!(p.iter().any(|(r, z)| (*r - 3.0).abs() < 1e-9 && (*z - 18.0).abs() < 1e-9), "the flutes end at 18 mm");
        assert!(p.iter().any(|(r, _)| (*r - 4.0).abs() < 1e-9), "the shank is 8 mm across");
    }
}
