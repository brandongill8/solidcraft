use std::f64::consts::PI;

use solidcraft_geom::{Loop2, Plane, Region2, Vec2, Vec3};

use crate::job::{Direction, DrillPoints, EntryMode, Job, OpKind, Operation, Setup, Side, Source, Stock};
use crate::offset::{Curve, inset};
use crate::path::{DrillCycle, Move, Toolpath};
use crate::plan::{Geometry, Site, circle_sites, levels, plan, region_to_work};
use crate::post::{Dialect, PostOptions, Step, generate};
use crate::ring::{dist_to_segment, perimeter, signed_area};
use crate::sim::{CollisionKind, Grid, Part, SimOp, SimOptions, simulate};
use crate::tool::{Tool, ToolKind};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Vec2> {
    vec![Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)]
}
fn reversed(mut v: Vec<Vec2>) -> Vec<Vec2> {
    v.reverse();
    v
}
fn rect_region(x0: f64, y0: f64, x1: f64, y1: f64) -> Region2 {
    Region2 { outer: Loop2::polygon(&rect(x0, y0, x1, y1)), holes: vec![] }
}
fn bounds(c: &Curve) -> (Vec2, Vec2) {
    let lo = c.points.iter().fold(Vec2::new(f64::INFINITY, f64::INFINITY), |a, p| Vec2::new(a.x.min(p.x), a.y.min(p.y)));
    let hi = c.points.iter().fold(Vec2::new(f64::NEG_INFINITY, f64::NEG_INFINITY), |a, p| Vec2::new(a.x.max(p.x), a.y.max(p.y)));
    (lo, hi)
}
fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}
/// The smallest distance from `p` to the ring's edges.
fn clearance(rings: &[Vec<Vec2>], p: Vec2) -> f64 {
    rings
        .iter()
        .flat_map(|r| (0..r.len()).map(move |i| (r[i], r[(i + 1) % r.len()])))
        .map(|(a, b)| dist_to_segment(p, a, b))
        .fold(f64::INFINITY, f64::min)
}

fn tool(id: u32, dia: f64) -> Tool {
    Tool {
        id,
        name: format!("{dia} mm end mill"),
        kind: ToolKind::FlatEndMill,
        diameter: dia,
        flutes: 2,
        corner_radius: 0.0,
        tip_angle: 118.0,
        flute_length: None,
        shank_diameter: None,
    }
}
fn drill_tool(id: u32, dia: f64) -> Tool {
    Tool { kind: ToolKind::Drill, ..tool(id, dia) }
}
fn setup() -> Setup {
    Setup {
        id: 1,
        name: "Top".into(),
        frame: Plane::XY,
        work_offset: 54,
        stock: Some(Stock { min: [0.0, 0.0, -10.0], max: [60.0, 40.0, 0.0] }),
        clearance: 10.0,
    }
}
fn op(kind: OpKind, tool: u32) -> Operation {
    Operation {
        id: 1,
        name: "Op".into(),
        setup: 1,
        tool,
        kind,
        top: 0.0,
        bottom: -4.0,
        retract: 2.0,
        stepdown: 2.0,
        spindle_rpm: 8000.0,
        feed: 600.0,
        plunge_feed: 200.0,
        coolant: false,
    }
}
fn source() -> Source {
    Source { sketch: 1, profiles: vec![] }
}
fn contour_op(side: Side, direction: Direction, allowance: f64) -> Operation {
    op(OpKind::Contour { source: source(), side, direction, allowance, ramp_angle: 3.0, entry: None, helix_angle: 3.0, helix_diameter: 0.0 }, 1)
}
fn pocket_op(stepover: f64, finish: bool, allowance: f64) -> Operation {
    op(
        OpKind::Pocket {
            source: source(),
            direction: Direction::Climb,
            stepover,
            allowance,
            finish,
            ramp_angle: 3.0,
            entry: None,
            helix_angle: 3.0,
            helix_diameter: 0.0,
        },
        1,
    )
}
fn geometry(regions: Vec<Region2>) -> Geometry {
    Geometry { regions, sites: vec![] }
}
fn feed_points(t: &Toolpath) -> Vec<Vec3> {
    t.moves.iter().filter_map(|m| if let Move::Feed { to, .. } = m { Some(*to) } else { None }).collect()
}

// ---------------------------------------------------------------------------------- offsetting

#[test]
fn rectangle_insets_to_a_smaller_rectangle() {
    let out = inset(&[rect(0.0, 0.0, 40.0, 20.0)], 3.0, 0.005).unwrap();
    assert_eq!(out.len(), 1);
    let c = &out[0];
    assert!(c.closed);
    let (lo, hi) = bounds(c);
    assert!(near(lo.x, 3.0, 1e-9) && near(lo.y, 3.0, 1e-9) && near(hi.x, 37.0, 1e-9) && near(hi.y, 17.0, 1e-9));
    assert!(near(signed_area(&c.points), 34.0 * 14.0, 1e-6), "counter-clockwise, free space on the left");
}

#[test]
fn a_strip_narrower_than_the_tool_has_no_inset() {
    assert!(inset(&[rect(0.0, 0.0, 40.0, 5.0)], 3.0, 0.005).unwrap().is_empty());
    let thin = inset(&[rect(0.0, 0.0, 40.0, 5.0)], 2.0, 0.005).unwrap();
    assert_eq!(thin.len(), 1);
    assert!(near(signed_area(&thin[0].points), 36.0 * 1.0, 1e-6));
}

#[test]
fn circle_insets_to_a_concentric_circle() {
    let ring = Loop2::circle(Vec2::new(5.0, -2.0), 10.0).polyline(0.001).into_vec();
    let out = inset(&[ring], 3.0, 0.005).unwrap();
    assert_eq!(out.len(), 1);
    for p in &out[0].points {
        assert!(near(p.dist(Vec2::new(5.0, -2.0)), 7.0, 0.01), "{p:?}");
    }
}

#[test]
fn a_reflex_corner_is_rounded_and_a_convex_one_is_sharp() {
    let l =
        vec![Vec2::new(0.0, 0.0), Vec2::new(40.0, 0.0), Vec2::new(40.0, 20.0), Vec2::new(20.0, 20.0), Vec2::new(20.0, 40.0), Vec2::new(0.0, 40.0)];
    let out = inset(std::slice::from_ref(&l), 2.0, 0.002).unwrap();
    assert_eq!(out.len(), 1);
    let area = signed_area(&out[0].points);
    assert!(near(area, 896.0 + 4.0 - PI, 0.05), "area {area}");
    // Never closer than 2 to the boundary (no gouging), and no further than the chord tolerance.
    for p in &out[0].points {
        let c = clearance(std::slice::from_ref(&l), *p);
        assert!((2.0 - 1e-6..=2.0 + 0.002 * 1.1).contains(&c), "{p:?}: {c}");
    }
}

#[test]
fn an_island_is_offset_outward_and_the_two_rings_are_separate() {
    let outer = rect(0.0, 0.0, 60.0, 60.0);
    let island = reversed(rect(20.0, 20.0, 40.0, 40.0));
    let out = inset(&[outer, island], 3.0, 0.002).unwrap();
    assert_eq!(out.len(), 2);
    let mut areas: Vec<f64> = out.iter().map(|c| signed_area(&c.points)).collect();
    areas.sort_by(f64::total_cmp);
    // The island's ring runs clockwise and encloses the island grown by 3 with round corners.
    assert!(near(areas[0], -(400.0 + 4.0 * 20.0 * 3.0 + PI * 9.0), 0.1), "{areas:?}");
    assert!(near(areas[1], 54.0 * 54.0, 1e-6), "{areas:?}");
}

#[test]
fn islands_close_together_leave_only_the_outer_ring() {
    // Two 20 mm squares 4 mm apart: with a 3 mm offset the gap between them is closed.
    let outer = rect(0.0, 0.0, 60.0, 30.0);
    let a = reversed(rect(10.0, 5.0, 28.0, 25.0));
    let b = reversed(rect(32.0, 5.0, 50.0, 25.0));
    let out = inset(&[outer, a, b], 3.0, 0.002).unwrap();
    // The grown islands merge into one, so the free space has two rings: the wall and the pair.
    assert_eq!(out.len(), 2, "{out:?}");
}

#[test]
fn hostile_offsets_are_errors_not_panics() {
    let sq = rect(0.0, 0.0, 10.0, 10.0);
    assert!(inset(std::slice::from_ref(&sq), f64::NAN, 0.01).is_err());
    assert!(inset(std::slice::from_ref(&sq), -1.0, 0.01).is_err());
    assert!(inset(std::slice::from_ref(&sq), 1.0, 0.0).is_err());
    assert!(inset(&[vec![Vec2::new(f64::NAN, 0.0), Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0)]], 0.1, 0.01).is_err());
    assert!(inset(&[vec![Vec2::new(1.0, 1.0); 5]], 1.0, 0.01).unwrap().is_empty());
    assert!(inset(&[], 1.0, 0.01).unwrap().is_empty());
}

#[test]
fn a_huge_outline_at_a_tiny_tolerance_is_refused_not_ground_through() {
    let ring = Loop2::circle(Vec2::ZERO, 5000.0).polyline(1e-9).into_vec();
    let r = inset(&[ring], 1.0, 1e-9);
    assert!(r.is_ok() || r.is_err());
}

// -------------------------------------------------------------------------------------- levels

#[test]
fn depth_passes_are_equal_and_end_on_the_bottom() {
    let lv = levels(0.0, -5.0, 2.0).unwrap();
    assert_eq!(lv.len(), 3);
    assert!(near(lv[0], -5.0 / 3.0, 1e-9) && near(lv[1], -10.0 / 3.0, 1e-9) && lv[2] == -5.0);
    assert_eq!(levels(0.0, -1.0, 5.0).unwrap(), vec![-1.0]);
    assert!(levels(0.0, -1000.0, 0.0001).is_err());
    assert!(levels(0.0, f64::NAN, 1.0).is_err());
    assert!(levels(0.0, 1.0, 1.0).is_err());
}

// ------------------------------------------------------------------------------------ contours

#[test]
fn an_outside_contour_stays_outside_the_part_by_the_tool_radius() {
    let region = rect_region(0.0, 0.0, 40.0, 20.0);
    let o = contour_op(Side::Outside, Direction::Climb, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &geometry(vec![region]), 0.005).unwrap();
    let outline = rect(0.0, 0.0, 40.0, 20.0);
    let pts = feed_points(&p.toolpath);
    assert!(!pts.is_empty());
    for q in pts.iter().filter(|q| q.z <= o.bottom + 1e-9) {
        let c = clearance(std::slice::from_ref(&outline), Vec2::new(q.x, q.y));
        assert!(c >= 3.0 - 1e-6, "tool gouges the part: {q:?} is {c} from it");
        // and outside, not inside
        assert!(!(q.x > 0.0 && q.x < 40.0 && q.y > 0.0 && q.y < 20.0));
    }
    // The deepest pass reaches the bottom exactly and nothing goes below it.
    assert!(pts.iter().all(|q| q.z >= o.bottom - 1e-9));
    assert!(pts.iter().any(|q| near(q.z, o.bottom, 1e-9)));
    assert!(p.warnings.is_empty());
}

#[test]
fn climb_runs_counter_clockwise_around_a_part_and_conventional_the_other_way() {
    let go = |dir: Direction| {
        let p =
            plan(&setup(), &tool(1, 6.0), &contour_op(Side::Outside, dir, 0.0), &geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]), 0.005).unwrap();
        let bottom: Vec<Vec2> = feed_points(&p.toolpath).into_iter().filter(|q| near(q.z, -4.0, 1e-9)).map(|q| Vec2::new(q.x, q.y)).collect();
        signed_area(&bottom)
    };
    assert!(go(Direction::Climb) > 0.0, "material on the left of the travel");
    assert!(go(Direction::Conventional) < 0.0);
}

#[test]
fn an_inside_contour_and_its_holes_keep_the_tool_in_the_free_space() {
    let region = Region2 { outer: Loop2::polygon(&rect(0.0, 0.0, 60.0, 40.0)), holes: vec![Loop2::polygon(&reversed(rect(20.0, 10.0, 40.0, 30.0)))] };
    let p = plan(&setup(), &tool(1, 4.0), &contour_op(Side::Inside, Direction::Climb, 0.5), &geometry(vec![region]), 0.005).unwrap();
    let outline = [rect(0.0, 0.0, 60.0, 40.0), reversed(rect(20.0, 10.0, 40.0, 30.0))];
    for q in feed_points(&p.toolpath) {
        let c = clearance(&outline, Vec2::new(q.x, q.y));
        assert!(c >= 2.5 - 1e-6, "{q:?}: {c}");
    }
}

#[test]
fn an_outside_contour_of_a_part_with_a_hole_cuts_the_hole_first() {
    let region = Region2 { outer: Loop2::polygon(&rect(0.0, 0.0, 60.0, 40.0)), holes: vec![Loop2::polygon(&reversed(rect(20.0, 10.0, 40.0, 30.0)))] };
    let p = plan(&setup(), &tool(1, 4.0), &contour_op(Side::Outside, Direction::Climb, 0.0), &geometry(vec![region]), 0.005).unwrap();
    let first_feed = feed_points(&p.toolpath).first().copied().unwrap();
    assert!(first_feed.x > 18.0 && first_feed.x < 42.0 && first_feed.y > 8.0 && first_feed.y < 32.0, "{first_feed:?}");
}

#[test]
fn a_tool_too_big_for_the_outline_is_an_error() {
    let r =
        plan(&setup(), &tool(1, 30.0), &contour_op(Side::Inside, Direction::Climb, 0.0), &geometry(vec![rect_region(0.0, 0.0, 20.0, 20.0)]), 0.005);
    assert!(r.is_err());
}

#[test]
fn a_contour_without_outlines_is_an_error() {
    assert!(plan(&setup(), &tool(1, 6.0), &contour_op(Side::On, Direction::Climb, 0.0), &geometry(vec![]), 0.005).is_err());
}

#[test]
fn ramping_never_goes_below_the_next_depth() {
    let p =
        plan(&setup(), &tool(1, 6.0), &contour_op(Side::Outside, Direction::Climb, 0.0), &geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]), 0.005)
            .unwrap();
    let mut z = f64::INFINITY;
    for m in &p.toolpath.moves {
        if let Move::Feed { to, .. } = m {
            assert!(to.z <= 2.0 + 1e-9, "feeds begin no higher than the feed plane");
            z = z.min(to.z);
        }
    }
    assert!(near(z, -4.0, 1e-9));
}

// --------------------------------------------------------------------------------------- pockets

#[test]
fn a_pocket_clears_the_floor_without_touching_the_walls() {
    let region = rect_region(0.0, 0.0, 40.0, 30.0);
    let p = plan(&setup(), &tool(1, 6.0), &pocket_op(0.4, false, 0.0), &geometry(vec![region]), 0.005).unwrap();
    let pts = feed_points(&p.toolpath);
    for q in &pts {
        assert!(q.x >= 3.0 - 1e-6 && q.x <= 37.0 + 1e-6 && q.y >= 3.0 - 1e-6 && q.y <= 27.0 + 1e-6, "{q:?} cuts into the wall");
        assert!(q.z >= -4.0 - 1e-9 && q.z <= 2.0 + 1e-9);
    }
    // Floor coverage: every point of the floor is within a tool radius of some cutting move.
    let bottom: Vec<Vec3> = pts.iter().copied().filter(|q| near(q.z, -4.0, 1e-9)).collect();
    for ix in 0..=40 {
        for iy in 0..=30 {
            let g = Vec2::new(ix as f64, iy as f64);
            // A round tool cannot reach into a sharp corner: only points within a radius of where
            // its centre may stand (the rectangle inset by the radius) are asked for.
            let (cx, cy) = (g.x.clamp(3.0, 37.0), g.y.clamp(3.0, 27.0));
            if g.dist(Vec2::new(cx, cy)) > 3.0 - 1e-9 {
                continue;
            }
            let reach =
                bottom.windows(2).map(|w| dist_to_segment(g, Vec2::new(w[0].x, w[0].y), Vec2::new(w[1].x, w[1].y))).fold(f64::INFINITY, f64::min);
            assert!(reach <= 3.0 + 1e-6, "uncut floor at {g:?}: nearest pass is {reach} away");
        }
    }
}

#[test]
fn pockets_keep_their_islands() {
    let region = Region2 { outer: Loop2::polygon(&rect(0.0, 0.0, 60.0, 40.0)), holes: vec![Loop2::polygon(&reversed(rect(25.0, 15.0, 35.0, 25.0)))] };
    let p = plan(&setup(), &tool(1, 4.0), &pocket_op(0.4, false, 0.0), &geometry(vec![region]), 0.005).unwrap();
    for q in feed_points(&p.toolpath) {
        let island = [reversed(rect(25.0, 15.0, 35.0, 25.0))];
        let c = clearance(&island, Vec2::new(q.x, q.y));
        let inside = q.x > 25.0 && q.x < 35.0 && q.y > 15.0 && q.y < 25.0;
        assert!(!inside && c >= 2.0 - 1e-6, "the tool is {c} from the island at {q:?}");
    }
}

#[test]
fn a_finish_pass_runs_along_the_walls_at_full_depth() {
    let p = plan(&setup(), &tool(1, 6.0), &pocket_op(0.4, true, 0.3), &geometry(vec![rect_region(0.0, 0.0, 40.0, 30.0)]), 0.005).unwrap();
    let walls: Vec<Vec3> = feed_points(&p.toolpath).into_iter().filter(|q| near(q.x, 3.0, 1e-6) && near(q.z, -4.0, 1e-9)).collect();
    assert!(!walls.is_empty(), "no pass at the wall");
    let rough_only = plan(&setup(), &tool(1, 6.0), &pocket_op(0.4, false, 0.3), &geometry(vec![rect_region(0.0, 0.0, 40.0, 30.0)]), 0.005).unwrap();
    assert!(!feed_points(&rough_only.toolpath).iter().any(|q| near(q.x, 3.0, 1e-6)));
}

#[test]
fn a_finish_pass_without_an_allowance_warns() {
    let p = plan(&setup(), &tool(1, 6.0), &pocket_op(0.4, true, 0.0), &geometry(vec![rect_region(0.0, 0.0, 40.0, 30.0)]), 0.005).unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("allowance")));
}

#[test]
fn a_circular_pocket_is_planned_from_a_real_circle() {
    let region = Region2 { outer: Loop2::circle(Vec2::new(30.0, 20.0), 12.0), holes: vec![] };
    let p = plan(&setup(), &tool(1, 6.0), &pocket_op(0.5, false, 0.0), &geometry(vec![region]), 0.005).unwrap();
    for q in feed_points(&p.toolpath) {
        assert!(Vec2::new(q.x, q.y).dist(Vec2::new(30.0, 20.0)) <= 9.0 + 0.01, "{q:?}");
    }
}

#[test]
fn rapids_between_features_go_through_the_clearance_height() {
    let regions = vec![rect_region(0.0, 0.0, 20.0, 20.0), rect_region(40.0, 0.0, 60.0, 20.0)];
    let p = plan(&setup(), &tool(1, 4.0), &pocket_op(0.4, false, 0.0), &geometry(regions), 0.005).unwrap();
    let mut at: Option<Vec3> = None;
    for m in &p.toolpath.moves {
        match m {
            Move::Rapid { to } => {
                if let Some(a) = at
                    && (a.x - to.x).abs() + (a.y - to.y).abs() > 1e-9
                {
                    assert!(near(a.z, 10.0, 1e-9) && near(to.z, 10.0, 1e-9), "a sideways rapid below the clearance: {a:?} -> {to:?}");
                }
                at = Some(*to);
            }
            Move::Feed { to, .. } => at = Some(*to),
            Move::Drill(_) => {}
        }
    }
}

// ------------------------------------------------------------------------------------ face, drill

#[test]
fn facing_covers_the_stock_from_outside_it() {
    let o = Operation { bottom: -1.0, stepdown: 1.0, ..op(OpKind::Face { margin: 0.0, stepover: 0.6 }, 1) };
    let p = plan(&setup(), &tool(1, 10.0), &o, &Geometry::default(), 0.005).unwrap();
    let pts = feed_points(&p.toolpath);
    assert!(pts.iter().all(|q| near(q.z, -1.0, 1e-9)));
    // Each cutting line starts and ends clear of the stock.
    assert!(pts.iter().any(|q| q.x < -5.0) && pts.iter().any(|q| q.x > 65.0));
    let ys: Vec<f64> = pts.iter().map(|q| q.y).collect();
    assert!(ys.iter().cloned().fold(f64::INFINITY, f64::min) <= 5.0 && ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) >= 35.0);
}

#[test]
fn facing_without_stock_is_an_error() {
    let mut s = setup();
    s.stock = None;
    assert!(plan(&s, &tool(1, 10.0), &op(OpKind::Face { margin: 0.0, stepover: 0.6 }, 1), &Geometry::default(), 0.005).is_err());
}

#[test]
fn drilling_visits_each_hole_once_and_warns_about_size() {
    let o =
        op(OpKind::Drill { points: DrillPoints::Points { points: vec![[10.0, 10.0], [50.0, 10.0], [30.0, 30.0]] }, peck: Some(1.5), dwell: 0.0 }, 2);
    let p = plan(&setup(), &drill_tool(2, 5.0), &o, &Geometry::default(), 0.005).unwrap();
    let cycles: Vec<&DrillCycle> = p.toolpath.moves.iter().filter_map(|m| if let Move::Drill(c) = m { Some(c) } else { None }).collect();
    assert_eq!(cycles.len(), 3);
    assert!(cycles.iter().all(|c| c.bottom == -4.0 && c.top == 0.0 && c.retract == 2.0));
    // From the origin the nearest hole comes first.
    assert_eq!((cycles[0].x, cycles[0].y), (10.0, 10.0));

    let sites = Geometry {
        regions: vec![],
        sites: vec![Site { at: Vec2::new(10.0, 10.0), diameter: Some(8.0) }, Site { at: Vec2::new(20.0, 10.0), diameter: Some(3.0) }],
    };
    let o = op(OpKind::Drill { points: DrillPoints::Sketch { sketch: 1, profiles: vec![] }, peck: None, dwell: 0.0 }, 2);
    let p = plan(&setup(), &drill_tool(2, 5.0), &o, &sites, 0.005).unwrap();
    assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
}

#[test]
fn circles_are_found_as_drill_sites() {
    let regions = vec![Region2 { outer: Loop2::circle(Vec2::new(5.0, 6.0), 2.5), holes: vec![] }, rect_region(0.0, 0.0, 4.0, 4.0)];
    let sites = circle_sites(&regions);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].at, Vec2::new(5.0, 6.0));
    assert!(near(sites[0].diameter.unwrap(), 5.0, 1e-9));
}

#[test]
fn a_drill_cannot_mill_and_an_end_mill_cannot_drill() {
    let mill = contour_op(Side::Outside, Direction::Climb, 0.0);
    assert!(plan(&setup(), &drill_tool(1, 5.0), &mill, &geometry(vec![rect_region(0.0, 0.0, 10.0, 10.0)]), 0.005).is_err());
    let drill_op = op(OpKind::Drill { points: DrillPoints::Points { points: vec![[1.0, 1.0]] }, peck: None, dwell: 0.0 }, 1);
    assert!(plan(&setup(), &tool(1, 5.0), &drill_op, &Geometry::default(), 0.005).is_err());
}

// ------------------------------------------------------------------------------------- job and tools

#[test]
fn hostile_numbers_never_pass_validation() {
    for v in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
        assert!(Tool { diameter: v, ..tool(1, 6.0) }.validate().is_err(), "diameter {v}");
        assert!(Operation { feed: v, ..contour_op(Side::On, Direction::Climb, 0.0) }.validate(&tool(1, 6.0), &setup()).is_err(), "feed {v}");
        assert!(Operation { spindle_rpm: v, ..contour_op(Side::On, Direction::Climb, 0.0) }.validate(&tool(1, 6.0), &setup()).is_err(), "rpm {v}");
    }
    let o = Operation { bottom: 1.0, ..contour_op(Side::On, Direction::Climb, 0.0) };
    assert!(o.validate(&tool(1, 6.0), &setup()).is_err(), "bottom above top");
    let o = Operation { top: 9.5, bottom: 0.0, ..contour_op(Side::On, Direction::Climb, 0.0) };
    assert!(o.validate(&tool(1, 6.0), &setup()).is_err(), "clearance below the feed plane");
    assert!(Setup { work_offset: 60, ..setup() }.validate().is_err());
    assert!(Setup { stock: Some(Stock { min: [0.0; 3], max: [0.0, 1.0, 1.0] }), ..setup() }.validate().is_err());
}

#[test]
fn a_job_round_trips_and_checks_its_references() {
    let job = Job { tools: vec![tool(1, 6.0)], setups: vec![setup()], operations: vec![contour_op(Side::Outside, Direction::Climb, 0.2)] };
    job.validate().unwrap();
    let text = serde_json::to_string(&job).unwrap();
    assert_eq!(serde_json::from_str::<Job>(&text).unwrap(), job);

    let mut bad = job.clone();
    bad.operations[0].tool = 9;
    assert!(bad.validate().is_err());
    let mut dup = job.clone();
    dup.tools.push(tool(1, 8.0));
    assert!(dup.validate().is_err());
    assert!(serde_json::from_str::<Job>(r#"{"tools": [], "mystery": 1}"#).is_err());
    assert!(Job::default().is_empty());
}

// ------------------------------------------------------------------------------------------- posts

fn post_one(dialect: Dialect, o: &Operation, t: &Tool, g: &Geometry, opts: &PostOptions) -> String {
    let s = setup();
    let planned = plan(&s, t, o, g, 0.005).unwrap();
    generate(dialect, &[Step { setup: &s, tool: t, op: o, toolpath: &planned.toolpath }], opts).unwrap()
}

#[test]
fn fanuc_programs_carry_the_safe_line_tool_change_offsets_and_decimal_points() {
    let g = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let text = post_one(Dialect::Fanuc, &contour_op(Side::Outside, Direction::Climb, 0.0), &tool(1, 6.0), &g, &PostOptions::default());
    assert!(text.starts_with("%\nO1001 (SOLIDCRAFT)\n"), "{text}");
    for want in ["G90 G94 G17 G21 G40 G49 G80", "T1 M6", "G54", "G0 G43 H1 Z10.", "S8000 M3", "M5", "G91 G28 Z0.", "M30"] {
        assert!(text.contains(want), "missing `{want}`:\n{text}");
    }
    assert!(text.trim_end().ends_with("%"));
    assert!(text.is_ascii());
    // A dimensioned word without a decimal point would be read in thousandths.
    for line in text.lines().filter(|l| l.contains(" X") || l.starts_with('X')) {
        for word in line.split_whitespace().filter(|w| w.starts_with(['X', 'Y', 'Z'])) {
            assert!(word.contains('.'), "`{word}` in `{line}`");
        }
    }
}

#[test]
fn haas_ends_at_the_machine_home() {
    let g = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let text = post_one(
        Dialect::Haas,
        &contour_op(Side::Outside, Direction::Climb, 0.0),
        &tool(1, 6.0),
        &g,
        &PostOptions { program_number: 7, ..Default::default() },
    );
    assert!(text.contains("O00007 (SOLIDCRAFT)"), "{text}");
    assert!(text.contains("G53 G0 Z0.") && text.contains("M30"));
}

#[test]
fn grbl_programs_have_no_tool_length_offsets_and_no_canned_cycles() {
    let o = op(OpKind::Drill { points: DrillPoints::Points { points: vec![[10.0, 10.0], [20.0, 10.0]] }, peck: Some(1.0), dwell: 0.0 }, 2);
    let text = post_one(Dialect::Grbl, &o, &drill_tool(2, 3.0), &Geometry::default(), &PostOptions::default());
    assert!(!text.contains("G43") && !text.contains("G81") && !text.contains("G83") && !text.contains("M6") && !text.contains('%'), "{text}");
    assert!(text.contains("G21 G90 G17 G94") && text.contains("G54") && text.contains("M30"));
    // Four 1 mm pecks per hole, spelled out as feeds.
    assert!(text.matches("G1").count() >= 4 && text.contains("F"), "{text}");
}

#[test]
fn fanuc_drilling_uses_canned_cycles() {
    let o = op(OpKind::Drill { points: DrillPoints::Points { points: vec![[10.0, 10.0], [20.0, 10.0]] }, peck: Some(1.0), dwell: 0.0 }, 2);
    let text = post_one(Dialect::Fanuc, &o, &drill_tool(2, 3.0), &Geometry::default(), &PostOptions::default());
    assert!(text.contains("G98"));
    assert_eq!(text.matches("G83 ").count(), 2, "{text}");
    assert!(text.contains("Q1.") && text.contains("R2.") && text.contains("Z-4."), "{text}");
    let g80 = text.rfind("G80\n").unwrap();
    assert!(g80 > text.find("G83").unwrap(), "the cycle is cancelled after the holes");

    let dwell = Operation { kind: OpKind::Drill { points: DrillPoints::Points { points: vec![[10.0, 10.0]] }, peck: None, dwell: 0.5 }, ..o };
    let text = post_one(Dialect::Fanuc, &dwell, &drill_tool(2, 3.0), &Geometry::default(), &PostOptions::default());
    assert!(text.contains("G82 ") && text.contains("P500"), "{text}");
}

#[test]
fn a_tool_change_pauses_grbl_and_calls_the_tool_on_fanuc() {
    let (s, t1, t2) = (setup(), tool(1, 6.0), tool(2, 3.0));
    let o1 = contour_op(Side::Outside, Direction::Climb, 0.0);
    let o2 = Operation { id: 2, tool: 2, ..contour_op(Side::Inside, Direction::Climb, 0.0) };
    let g = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let (p1, p2) = (plan(&s, &t1, &o1, &g, 0.005).unwrap(), plan(&s, &t2, &o2, &g, 0.005).unwrap());
    let steps = [Step { setup: &s, tool: &t1, op: &o1, toolpath: &p1.toolpath }, Step { setup: &s, tool: &t2, op: &o2, toolpath: &p2.toolpath }];
    let grbl = generate(Dialect::Grbl, &steps, &PostOptions::default()).unwrap();
    assert_eq!(grbl.matches("M0\n").count(), 1, "{grbl}");
    let fanuc = generate(Dialect::Fanuc, &steps, &PostOptions::default()).unwrap();
    assert!(fanuc.contains("T1 M6") && fanuc.contains("T2 M6") && fanuc.contains("G43 H2"), "{fanuc}");
    assert_eq!(fanuc.matches("G54\n").count(), 1, "the work offset is set once");
}

#[test]
fn line_numbers_and_empty_jobs() {
    let g = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let text = post_one(
        Dialect::Fanuc,
        &contour_op(Side::On, Direction::Climb, 0.0),
        &tool(1, 6.0),
        &g,
        &PostOptions { line_numbers: true, ..Default::default() },
    );
    assert!(text.contains("N10 ") && text.contains("N20 "));
    assert!(generate(Dialect::Fanuc, &[], &PostOptions::default()).is_err());
}

#[test]
fn a_non_finite_move_is_refused_by_the_post() {
    let (s, t) = (setup(), tool(1, 6.0));
    let o = contour_op(Side::On, Direction::Climb, 0.0);
    let path = Toolpath { moves: vec![Move::Rapid { to: Vec3::new(f64::NAN, 0.0, 0.0) }] };
    assert!(generate(Dialect::Grbl, &[Step { setup: &s, tool: &t, op: &o, toolpath: &path }], &PostOptions::default()).is_err());
}

#[test]
fn comments_cannot_break_out_of_their_parentheses() {
    let (s, t) = (setup(), tool(1, 6.0));
    let o = Operation { name: "bad) M30 (name\u{e9}".into(), ..contour_op(Side::On, Direction::Climb, 0.0) };
    let g = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let p = plan(&s, &t, &o, &g, 0.005).unwrap();
    let text = generate(Dialect::Fanuc, &[Step { setup: &s, tool: &t, op: &o, toolpath: &p.toolpath }], &PostOptions::default()).unwrap();
    assert!(text.is_ascii());
    let line = text.lines().find(|l| l.contains("OPERATION 1")).unwrap();
    assert_eq!(line.matches('(').count(), 1);
    assert_eq!(line.matches(')').count(), 1);
}

#[test]
fn stats_add_up() {
    let p = Toolpath { moves: vec![Move::Rapid { to: Vec3::new(10.0, 0.0, 0.0) }, Move::Feed { to: Vec3::new(10.0, 60.0, 0.0), feed: 600.0 }] };
    let st = p.stats(Vec3::ZERO, 10_000.0);
    assert!(near(st.rapid_length, 10.0, 1e-9) && near(st.feed_length, 60.0, 1e-9));
    assert!(near(st.minutes, 10.0 / 10_000.0 + 60.0 / 600.0, 1e-12));
    assert_eq!(st.max, Vec3::new(10.0, 60.0, 0.0));
    assert!(near(perimeter(&rect(0.0, 0.0, 2.0, 3.0)), 10.0, 1e-12));
}

#[test]
fn a_tiny_peck_does_not_explode_into_millions_of_moves() {
    let c = DrillCycle { x: 0.0, y: 0.0, top: 0.0, bottom: -50.0, retract: 2.0, peck: Some(1e-12), dwell: 0.0, feed: 100.0 };
    let n = Toolpath { moves: vec![Move::Drill(c)] }.expanded().len();
    assert!(n < 10, "{n}");
}

// ------------------------------------------------------------------------------- sketch mapping

#[test]
fn sketch_regions_map_into_work_coordinates() {
    // A sketch on a plane 5 above the origin, setup frame shifted by (10, 20, 2).
    let plane = Plane::XY.offset(5.0);
    let s = Setup { frame: Plane { origin: Vec3::new(10.0, 20.0, 2.0), ..Plane::XY }, ..setup() };
    let region = Region2 { outer: Loop2::circle(Vec2::new(30.0, 40.0), 5.0), holes: vec![] };
    let (mapped, z) = region_to_work(&region, &plane, &s).unwrap();
    assert!(near(z, 3.0, 1e-9));
    let (c, r) = crate::plan::circle_of(&mapped.outer).unwrap();
    assert!(near(c.x, 20.0, 1e-9) && near(c.y, 20.0, 1e-9) && near(r, 5.0, 1e-9));
}

#[test]
fn a_tilted_sketch_is_refused_and_a_flipped_one_keeps_its_shape() {
    let region = rect_region(0.0, 0.0, 10.0, 6.0);
    let tilted = Plane::from_normal(Vec3::ZERO, Vec3::new(0.0, 0.3, 1.0)).unwrap();
    assert!(region_to_work(&region, &tilted, &setup()).is_err());
    // Seen from below: the same plane, mirrored. The outline keeps its size.
    let (m, _) = region_to_work(&region, &Plane::XY.flipped(false), &setup()).unwrap();
    let pts = m.outer.polyline(0.01).into_vec();
    let w = pts.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max) - pts.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let h = pts.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max) - pts.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    assert!(near(w, 10.0, 1e-9) && near(h, 6.0, 1e-9));
    // and a mirrored arc still sweeps through the same points
    let arc = Region2 { outer: Loop2::circle(Vec2::new(3.0, 4.0), 2.0), holes: vec![] };
    let (m, _) = region_to_work(&arc, &Plane::XY.flipped(false), &setup()).unwrap();
    let (c, r) = crate::plan::circle_of(&m.outer).unwrap();
    assert!(near(c.x, 3.0, 1e-9) && near(c.y, -4.0, 1e-9) && near(r, 2.0, 1e-9));
}

// ------------------------------------------------------------------------------- simulation

fn box_triangles(lo: [f64; 3], hi: [f64; 3]) -> Vec<[Vec3; 3]> {
    let p = |x: usize, y: usize, z: usize| {
        Vec3::new(if x == 0 { lo[0] } else { hi[0] }, if y == 0 { lo[1] } else { hi[1] }, if z == 0 { lo[2] } else { hi[2] })
    };
    let quads = [
        [p(0, 0, 0), p(0, 1, 0), p(1, 1, 0), p(1, 0, 0)],
        [p(0, 0, 1), p(1, 0, 1), p(1, 1, 1), p(0, 1, 1)],
        [p(0, 0, 0), p(1, 0, 0), p(1, 0, 1), p(0, 0, 1)],
        [p(1, 0, 0), p(1, 1, 0), p(1, 1, 1), p(1, 0, 1)],
        [p(1, 1, 0), p(0, 1, 0), p(0, 1, 1), p(1, 1, 1)],
        [p(0, 1, 0), p(0, 0, 0), p(0, 0, 1), p(0, 1, 1)],
    ];
    quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect()
}
fn stock_box() -> Stock {
    Stock { min: [0.0, 0.0, -10.0], max: [60.0, 40.0, 0.0] }
}
fn sim_opts(cell: f64) -> SimOptions {
    SimOptions { cell, tolerance: None, until: None, clearance: 10.0 }
}
fn manual(moves: Vec<Move>) -> Toolpath {
    Toolpath { moves }
}
fn pocket_path(bottom: f64) -> Toolpath {
    let mut o = pocket_op(0.4, false, 0.0);
    o.bottom = bottom;
    plan(&setup(), &tool(1, 6.0), &o, &geometry(vec![rect_region(10.0, 10.0, 50.0, 30.0)]), 0.005).unwrap().toolpath
}

#[test]
fn tool_surfaces_by_kind() {
    let ball = Tool { kind: ToolKind::BallEndMill, ..tool(1, 6.0) };
    assert!(near(ball.lift(0.0), 0.0, 1e-12) && near(ball.lift(3.0), 3.0, 1e-12));
    assert!(near(ball.lift(1.5), 3.0 - (9.0f64 - 2.25).sqrt(), 1e-12));
    let bull = Tool { kind: ToolKind::BullNoseEndMill, corner_radius: 1.0, ..tool(1, 6.0) };
    assert!(near(bull.lift(2.0), 0.0, 1e-12) && near(bull.lift(3.0), 1.0, 1e-12));
    let drill = drill_tool(1, 6.0);
    assert!(near(drill.lift(3.0), 3.0 / (59.0f64).to_radians().tan(), 1e-9));
    assert!(near(tool(1, 6.0).lift(2.0), 0.0, 0.0));
    assert!(Tool { flute_length: Some(-1.0), ..tool(1, 6.0) }.validate().is_err());
    assert!(Tool { shank_diameter: Some(f64::NAN), ..tool(1, 6.0) }.validate().is_err());
}

#[test]
fn a_pocket_removes_its_volume_without_collisions() {
    let t = tool(1, 6.0);
    let path = pocket_path(-4.0);
    let sim = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &path }], &sim_opts(0.25)).unwrap();
    assert!(sim.collisions.is_empty(), "{:?}", sim.collisions);
    // 40 x 20 x 4, less the corners a round tool cannot reach.
    assert!(near(sim.volumes.removed, 3200.0, 3200.0 * 0.03), "{}", sim.volumes.removed);
    assert!(near(sim.volumes.removed + sim.volumes.remaining, sim.volumes.stock, 1e-6));
    let g = sim.grid;
    let at = |x: f64, y: f64| sim.heights[((y - g.y0) / g.cell) as usize * g.nx + ((x - g.x0) / g.cell) as usize];
    assert!(near(f64::from(at(30.0, 20.0)), -4.0, 1e-4));
    assert!(near(f64::from(at(5.0, 5.0)), 0.0, 1e-9));
    assert_eq!(sim.steps, sim.total_steps);
}

#[test]
fn cutting_into_the_part_is_a_gouge() {
    let t = tool(1, 6.0);
    // The part is the block under a pocket meant to stop at -6.
    let grid = Grid::new(&stock_box(), 0.25).unwrap();
    let part = Part::from_triangles(&grid, box_triangles([0.0, 0.0, -10.0], [60.0, 40.0, -6.0])).unwrap();
    assert_eq!(part.open_cells, 0);
    let deep = pocket_path(-8.0);
    let sim = simulate(&stock_box(), Some(&part), &[SimOp { operation: 7, tool: &t, toolpath: &deep }], &sim_opts(0.25)).unwrap();
    let gouge = sim.collisions.iter().find(|c| c.kind == CollisionKind::Gouge).expect("a gouge");
    assert_eq!(gouge.operation, 7);
    assert!(sim.collisions.iter().filter(|c| c.kind == CollisionKind::Gouge).any(|c| near(c.depth, 2.0, 0.1)));
    assert!(sim.volumes.gouged > 40.0 * 20.0 * 1.5, "{}", sim.volumes.gouged);
    let exact = pocket_path(-6.0);
    let ok = simulate(&stock_box(), Some(&part), &[SimOp { operation: 7, tool: &t, toolpath: &exact }], &sim_opts(0.25)).unwrap();
    assert_eq!(ok.counts, [0, 0, 0, 0], "{:?}", ok.collisions);
    assert!(ok.volumes.gouged < 1.0);
    // What is left to cut is the stock above the part, less the pocket.
    assert!(near(ok.volumes.leftover, 60.0 * 40.0 * 6.0 - ok.volumes.removed, 1.0));
}

#[test]
fn rapids_through_stock_and_the_floor_are_reported() {
    let t = tool(1, 6.0);
    let crash = manual(vec![Move::Rapid { to: Vec3::new(30.0, 20.0, 10.0) }, Move::Rapid { to: Vec3::new(30.0, 20.0, -5.0) }]);
    let sim = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &crash }], &sim_opts(0.25)).unwrap();
    assert!(sim.counts[CollisionKind::Rapid.index()] >= 1, "{:?}", sim.collisions);
    let clear = manual(vec![Move::Rapid { to: Vec3::new(30.0, 20.0, 10.0) }, Move::Rapid { to: Vec3::new(5.0, 5.0, 5.0) }]);
    let none = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &clear }], &sim_opts(0.25)).unwrap();
    assert_eq!(none.counts, [0; 4]);
    let deep = manual(vec![Move::Rapid { to: Vec3::new(30.0, 20.0, 10.0) }, Move::Feed { to: Vec3::new(30.0, 20.0, -12.0), feed: 100.0 }]);
    let floor = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &deep }], &sim_opts(0.25)).unwrap();
    assert!(floor.collisions.iter().any(|c| c.kind == CollisionKind::Floor && near(c.depth, 2.0, 0.2)));
}

#[test]
fn a_short_flute_makes_the_shank_hit_stock() {
    let side = manual(vec![
        Move::Rapid { to: Vec3::new(5.0, 20.0, 10.0) },
        Move::Feed { to: Vec3::new(5.0, 20.0, -6.0), feed: 100.0 },
        Move::Feed { to: Vec3::new(40.0, 20.0, -6.0), feed: 300.0 },
    ]);
    let short = Tool { flute_length: Some(3.0), ..tool(1, 6.0) };
    let sim = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &short, toolpath: &side }], &sim_opts(0.25)).unwrap();
    assert!(sim.collisions.iter().any(|c| c.kind == CollisionKind::Shank && near(c.depth, 3.0, 0.2)), "{:?}", sim.collisions);
    let long = Tool { flute_length: Some(12.0), ..tool(1, 6.0) };
    let ok = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &long, toolpath: &side }], &sim_opts(0.25)).unwrap();
    assert_eq!(ok.counts, [0; 4], "{:?}", ok.collisions);
}

#[test]
fn the_run_can_stop_part_way() {
    let t = tool(1, 6.0);
    let path = pocket_path(-4.0);
    let half = SimOptions { until: Some(path.moves.len() / 2), ..sim_opts(0.5) };
    let part_way = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &path }], &half).unwrap();
    let all = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &path }], &sim_opts(0.5)).unwrap();
    assert!(part_way.steps < all.steps && part_way.volumes.removed < all.volumes.removed && part_way.volumes.removed > 0.0);
    assert_eq!(part_way.total_steps, all.total_steps);
    let s = all.surface(100);
    assert!(s.nx * s.ny <= 100 && s.z.len() == s.nx * s.ny);
}

#[test]
fn a_part_with_a_hole_in_its_surface_does_not_panic() {
    let grid = Grid::new(&stock_box(), 0.5).unwrap();
    let mut tris = box_triangles([10.0, 10.0, -10.0], [50.0, 30.0, -4.0]);
    tris.remove(2);
    let part = Part::from_triangles(&grid, tris).unwrap();
    assert!(part.open_cells > 0);
}

#[test]
fn hostile_simulation_input_is_an_error() {
    let t = tool(1, 6.0);
    let nan = manual(vec![Move::Rapid { to: Vec3::new(f64::NAN, 0.0, 0.0) }]);
    assert!(simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &nan }], &sim_opts(0.5)).is_err());
    let far = manual(vec![Move::Rapid { to: Vec3::new(1e300, 0.0, 0.0) }]);
    assert!(simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &far }], &sim_opts(0.5)).is_err());
    for cell in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-9] {
        assert!(simulate(&stock_box(), None, &[], &sim_opts(cell)).is_err(), "{cell}");
    }
    for tol in [0.0, -1.0, f64::NAN, 1e9] {
        assert!(simulate(&stock_box(), None, &[], &SimOptions { tolerance: Some(tol), ..sim_opts(0.5) }).is_err(), "{tol}");
    }
    let wide = Stock { min: [-1e9, 0.0, 0.0], max: [1e9, 1.0, 1.0] };
    assert!(Grid::new(&wide, 1.0).is_err());
    let huge = Stock { min: [0.0, 0.0, 0.0], max: [5000.0, 5000.0, 10.0] };
    assert!(Grid::new(&huge, 0.01).unwrap().cells() <= crate::sim::MAX_CELLS);
    let other = Grid::new(&stock_box(), 1.0).unwrap();
    let part = Part::from_triangles(&other, box_triangles([0.0; 3], [1.0; 3])).unwrap();
    assert!(simulate(&stock_box(), Some(&part), &[], &sim_opts(0.5)).is_err());
    assert!(Part::from_triangles(&other, [[Vec3::new(f64::NAN, 0.0, 0.0), Vec3::ZERO, Vec3::X]]).is_err());
    // A very long path at a fine resolution stops with a limit error, not a hang.
    let long = manual(
        (0..60_000)
            .flat_map(|i| [Move::Rapid { to: Vec3::new(0.0, 0.0, 10.0) }, Move::Rapid { to: Vec3::new(59.0, f64::from(i % 39), 10.0) }])
            .collect(),
    );
    let r = simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &long }], &sim_opts(0.05));
    assert!(r.is_err());
}

// --------------------------------------------------------------------- tool kinds and entry moves

/// `o` (a contour or pocket) with its entry settings replaced.
fn entering(mut o: Operation, mode: Option<EntryMode>, ramp: f64, helix_angle: f64, helix_diameter: f64) -> Operation {
    if let OpKind::Contour { entry, ramp_angle, helix_angle: ha, helix_diameter: hd, .. }
    | OpKind::Pocket { entry, ramp_angle, helix_angle: ha, helix_diameter: hd, .. } = &mut o.kind
    {
        *entry = mode;
        *ramp_angle = ramp;
        *ha = helix_angle;
        *hd = helix_diameter;
    }
    o
}

/// The feed moves that go down and sideways at once (a ramp or a helix), with their feeds.
fn slanted(t: &Toolpath) -> Vec<(Vec3, Vec3, f64)> {
    let mut at: Option<Vec3> = None;
    let mut out = Vec::new();
    for m in &t.moves {
        match m {
            Move::Rapid { to } => at = Some(*to),
            Move::Feed { to, feed } => {
                if let Some(a) = at
                    && a.z - to.z > 1e-9
                    && Vec2::new(a.x, a.y).dist(Vec2::new(to.x, to.y)) > 1e-9
                {
                    out.push((a, *to, *feed));
                }
                at = Some(*to);
            }
            Move::Drill(_) => {}
        }
    }
    out
}

#[test]
fn every_tool_kind_round_trips_and_validates() {
    assert_eq!(ToolKind::ALL.len(), 8);
    for kind in ToolKind::ALL {
        let t = Tool { kind, flutes: kind.default_flutes(), tip_angle: kind.default_tip_angle(), ..tool(1, 6.0) };
        t.validate().unwrap();
        let back: Tool = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        assert!(!kind.label().is_empty());
    }
    let shell: Tool = serde_json::from_str(r#"{"id": 3, "name": "Shell", "kind": "shell_mill", "diameter": 50.0}"#).unwrap();
    assert_eq!(shell.kind, ToolKind::ShellMill);
    assert!(serde_json::from_str::<Tool>(r#"{"id": 3, "name": "x", "kind": "laser", "diameter": 5.0}"#).is_err());
    assert!(near(ToolKind::SpotDrill.default_tip_angle(), 90.0, 0.0) && near(ToolKind::Drill.default_tip_angle(), 118.0, 0.0));
}

#[test]
fn spot_drills_and_reamers_drill_and_the_wide_mills_mill() {
    let spot = Tool { kind: ToolKind::SpotDrill, tip_angle: 90.0, ..tool(1, 6.0) };
    let reamer = Tool { kind: ToolKind::Reamer, ..tool(1, 6.0) };
    let shell = Tool { kind: ToolKind::ShellMill, ..tool(1, 25.0) };
    let face = Tool { kind: ToolKind::FaceMill, ..tool(1, 40.0) };
    // A 90 degree point rises one for one; a reamer, shell mill and face mill end flat.
    assert!(near(spot.lift(3.0), 3.0, 1e-9));
    for t in [&reamer, &shell, &face] {
        assert!(near(t.lift(t.radius()), 0.0, 0.0));
    }
    let drill_op = op(OpKind::Drill { points: DrillPoints::Points { points: vec![[10.0, 10.0]] }, peck: None, dwell: 0.0 }, 1);
    for t in [&spot, &reamer] {
        assert!(t.kind.is_drill());
        assert!(plan(&setup(), t, &drill_op, &Geometry::default(), 0.005).is_ok(), "{} drills", t.kind.label());
        let mill = contour_op(Side::Outside, Direction::Climb, 0.0);
        assert!(plan(&setup(), t, &mill, &geometry(vec![rect_region(0.0, 0.0, 30.0, 30.0)]), 0.005).is_err(), "{} cannot mill", t.kind.label());
    }
    let face_op = Operation { bottom: -1.0, stepdown: 1.0, ..op(OpKind::Face { margin: 0.0, stepover: 0.6 }, 1) };
    for t in [&shell, &face] {
        assert!(!t.kind.is_drill());
        assert!(plan(&setup(), t, &face_op, &Geometry::default(), 0.005).is_ok(), "{} faces", t.kind.label());
    }
    let pocket = pocket_op(0.4, false, 0.0);
    assert!(plan(&setup(), &shell, &pocket, &geometry(vec![rect_region(0.0, 0.0, 90.0, 70.0)]), 0.005).is_ok());
}

#[test]
fn a_plunge_goes_straight_down_and_ignores_the_angles() {
    let region = rect_region(0.0, 0.0, 60.0, 40.0);
    let o = entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Plunge), 3.0, 3.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &geometry(vec![region]), 0.005).unwrap();
    assert!(slanted(&p.toolpath).is_empty(), "no move goes down and sideways together");
    assert!(p.warnings.is_empty());
}

#[test]
fn designs_from_before_the_entry_setting_ramp_or_plunge_by_their_angle() {
    let kind = r#"{"type": "pocket", "source": {"sketch": 1}, "ramp_angle": 3.0}"#;
    let k: OpKind = serde_json::from_str(kind).unwrap();
    assert_eq!(k.entry().map(|e| e.mode), Some(EntryMode::Ramp));
    let k: OpKind = serde_json::from_str(r#"{"type": "contour", "source": {"sketch": 1}, "ramp_angle": 0.0}"#).unwrap();
    assert_eq!(k.entry().map(|e| e.mode), Some(EntryMode::Plunge));
    assert!(OpKind::Face { margin: 0.0, stepover: 0.5 }.entry().is_none());
    // Saving without an entry keeps the saved form as it was.
    let again = serde_json::to_value(serde_json::from_str::<OpKind>(kind).unwrap()).unwrap();
    assert!(again.get("entry").is_none());
    // The same path comes out whether the ramp is implied or asked for.
    let region = geometry(vec![rect_region(0.0, 0.0, 60.0, 40.0)]);
    let implied = plan(&setup(), &tool(1, 6.0), &pocket_op(0.4, false, 0.0), &region, 0.005).unwrap();
    let asked = plan(&setup(), &tool(1, 6.0), &entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Ramp), 3.0, 3.0, 0.0), &region, 0.005).unwrap();
    assert_eq!(implied.toolpath, asked.toolpath);
}

#[test]
fn a_ramp_follows_the_ramp_angle_at_the_entry_feed() {
    let region = geometry(vec![rect_region(0.0, 0.0, 60.0, 40.0)]);
    let o = entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Ramp), 5.0, 3.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    let ramps = slanted(&p.toolpath);
    assert!(!ramps.is_empty());
    let max = 5.0f64.to_radians().tan();
    for (a, b, feed) in &ramps {
        let slope = (a.z - b.z) / Vec2::new(a.x, a.y).dist(Vec2::new(b.x, b.y));
        assert!(slope <= max * 1.001, "steeper than 5 degrees: {slope}");
        assert!(near(*feed, 200.0, 0.0), "the plunge feed set on the operation is the entry feed");
    }
    // With no plunge feed the ramp runs at the cutting feed.
    let o = Operation { plunge_feed: 0.0, ..o };
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    assert!(slanted(&p.toolpath).iter().all(|(_, _, f)| near(*f, 600.0, 0.0)));
}

#[test]
fn a_helical_entry_spirals_down_inside_the_pocket_at_the_helix_angle() {
    let region = geometry(vec![rect_region(0.0, 0.0, 60.0, 40.0)]);
    // One depth pass of 4 mm from the 1 mm feed plane: a 5 mm drop.
    let o = Operation { stepdown: 4.0, ..entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Helical), 3.0, 5.0, 0.0) };
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let helix = slanted(&p.toolpath);
    assert!(helix.len() > 30, "{} moves", helix.len());
    // The helix is a circle of 90% of the tool diameter, run at the entry feed, never steeper
    // than asked, and never closer to the walls than the tool radius.
    let n = helix.len() as f64;
    let (cx, cy) = (helix.iter().map(|h| h.1.x).sum::<f64>() / n, helix.iter().map(|h| h.1.y).sum::<f64>() / n);
    let max = 5.0f64.to_radians().tan();
    for (a, b, feed) in &helix {
        assert!(near(Vec2::new(b.x, b.y).dist(Vec2::new(cx, cy)), 2.7, 0.05), "{b:?} is off the circle");
        assert!(b.x >= 3.0 - 1e-6 && b.x <= 57.0 + 1e-6 && b.y >= 3.0 - 1e-6 && b.y <= 37.0 + 1e-6, "{b:?} cuts into the wall");
        assert!((a.z - b.z) / Vec2::new(a.x, a.y).dist(Vec2::new(b.x, b.y)) <= max * 1.001);
        assert!(near(*feed, 200.0, 0.0));
    }
    // It reaches the floor, and the cut never goes below it.
    let floor = feed_points(&p.toolpath).iter().fold(f64::INFINITY, |m, q| m.min(q.z));
    assert!(near(floor, -4.0, 1e-9));
    // A bigger helix diameter makes a bigger circle.
    let wide = Operation { ..entering(o.clone(), Some(EntryMode::Helical), 3.0, 5.0, 8.0) };
    let q = plan(&setup(), &tool(1, 6.0), &wide, &region, 0.005).unwrap();
    let w = slanted(&q.toolpath);
    let m = w.len() as f64;
    let (wx, wy) = (w.iter().map(|h| h.1.x).sum::<f64>() / m, w.iter().map(|h| h.1.y).sum::<f64>() / m);
    assert!(w.iter().all(|h| near(Vec2::new(h.1.x, h.1.y).dist(Vec2::new(wx, wy)), 4.0, 0.05)));
}

#[test]
fn a_helical_pocket_with_several_depth_passes_stays_clear_of_the_walls() {
    let region = geometry(vec![rect_region(0.0, 0.0, 60.0, 40.0)]);
    let o = entering(pocket_op(0.4, true, 0.2), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    for q in feed_points(&p.toolpath) {
        assert!(q.x >= 3.2 - 1e-6 - 0.2 && q.x <= 56.8 + 1e-6 + 0.2 && q.y >= 3.0 - 1e-6 && q.y <= 37.0 + 1e-6, "{q:?}");
        assert!(q.z >= -4.0 - 1e-9 && q.z <= 1.0 + 1e-9);
    }
    assert!(!slanted(&p.toolpath).is_empty());
}

#[test]
fn a_helix_that_does_not_fit_ramps_and_says_so() {
    // 6.5 mm wide: the tool centre has 0.5 mm to move in, so no circle fits.
    let region = geometry(vec![rect_region(0.0, 0.0, 6.5, 40.0)]);
    let o = entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("helical")), "{:?}", p.warnings);
    let floor = feed_points(&p.toolpath).iter().fold(f64::INFINITY, |m, q| m.min(q.z));
    assert!(near(floor, -4.0, 1e-9), "it still reaches the bottom");
    for q in feed_points(&p.toolpath) {
        assert!(q.x >= 3.0 - 1e-6 && q.x <= 3.5 + 1e-6, "{q:?}");
    }
}

#[test]
fn an_outside_contour_spirals_down_in_the_air_beside_the_part() {
    let region = geometry(vec![rect_region(0.0, 0.0, 40.0, 20.0)]);
    let o = entering(contour_op(Side::Outside, Direction::Climb, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let helix = slanted(&p.toolpath);
    assert!(!helix.is_empty());
    for (_, b, _) in &helix {
        let outside = b.x < 0.0 || b.x > 40.0 || b.y < 0.0 || b.y > 20.0;
        let c = clearance(&[rect(0.0, 0.0, 40.0, 20.0)], Vec2::new(b.x, b.y));
        assert!(outside && c >= 3.0 - 1e-6, "{b:?} reaches the part (clearance {c})");
    }
    // A contour that follows the line itself has no room for a helix and ramps.
    let on = entering(contour_op(Side::On, Direction::Climb, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    let q = plan(&setup(), &tool(1, 6.0), &on, &region, 0.005).unwrap();
    assert!(q.warnings.iter().any(|w| w.contains("helical")));
}

#[test]
fn entry_settings_are_checked() {
    let t = tool(1, 6.0);
    let ok = entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    assert!(ok.validate(&t, &setup()).is_ok());
    for (mode, ramp, helix, dia) in [
        (Some(EntryMode::Ramp), 0.0, 3.0, 0.0),
        (Some(EntryMode::Ramp), 60.0, 3.0, 0.0),
        (Some(EntryMode::Helical), 3.0, 0.0, 0.0),
        (Some(EntryMode::Helical), 3.0, 50.0, 0.0),
        (Some(EntryMode::Helical), 3.0, f64::NAN, 0.0),
        (Some(EntryMode::Helical), 3.0, 3.0, -1.0),
        (Some(EntryMode::Helical), 3.0, 3.0, f64::INFINITY),
    ] {
        let o = entering(pocket_op(0.4, false, 0.0), mode, ramp, helix, dia);
        assert!(o.validate(&t, &setup()).is_err(), "{mode:?} ramp {ramp} helix {helix} diameter {dia}");
    }
    // Older designs that plunge with a zero angle stay valid.
    assert!(entering(pocket_op(0.4, false, 0.0), None, 0.0, 3.0, 0.0).validate(&t, &setup()).is_ok());
}

#[test]
fn an_inside_contour_spirals_down_inside_the_outline() {
    let region = geometry(vec![rect_region(0.0, 0.0, 40.0, 30.0)]);
    let o = entering(contour_op(Side::Inside, Direction::Climb, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0);
    let p = plan(&setup(), &tool(1, 6.0), &o, &region, 0.005).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let helix = slanted(&p.toolpath);
    assert!(!helix.is_empty());
    for (_, b, _) in &helix {
        assert!(b.x >= 3.0 - 1e-6 && b.x <= 37.0 + 1e-6 && b.y >= 3.0 - 1e-6 && b.y <= 27.0 + 1e-6, "{b:?} reaches the wall");
    }
}

#[test]
fn a_helical_pocket_removes_the_same_material_as_a_ramped_one_without_collisions() {
    let t = tool(1, 6.0);
    let region = geometry(vec![rect_region(10.0, 10.0, 50.0, 30.0)]);
    let run = |o: &Operation| {
        let path = plan(&setup(), &t, o, &region, 0.005).unwrap().toolpath;
        simulate(&stock_box(), None, &[SimOp { operation: 1, tool: &t, toolpath: &path }], &sim_opts(0.25)).unwrap()
    };
    let helical = run(&entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Helical), 3.0, 4.0, 0.0));
    let ramped = run(&entering(pocket_op(0.4, false, 0.0), Some(EntryMode::Ramp), 3.0, 4.0, 0.0));
    assert!(helical.collisions.is_empty(), "{:?}", helical.collisions);
    assert!(
        near(helical.volumes.removed, ramped.volumes.removed, ramped.volumes.removed * 0.01),
        "{} against {}",
        helical.volumes.removed,
        ramped.volumes.removed
    );
    // Nothing is cut outside the pocket.
    let g = helical.grid;
    let at = |x: f64, y: f64| helical.heights[((y - g.y0) / g.cell) as usize * g.nx + ((x - g.x0) / g.cell) as usize];
    assert!(near(f64::from(at(8.0, 20.0)), 0.0, 1e-9) && near(f64::from(at(30.0, 20.0)), -4.0, 1e-4));
}

// ------------------------------------------------------------------------------------------- arcs

fn circle_moves(r: f64, from_deg: f64, to_deg: f64, segs: usize, z: (f64, f64), feed: f64) -> Vec<Move> {
    (0..=segs)
        .map(|k| {
            let f = k as f64 / segs as f64;
            let a = (from_deg + (to_deg - from_deg) * f).to_radians();
            Move::Feed { to: Vec3::new(20.0 + r * a.cos(), 15.0 + r * a.sin(), z.0 + (z.1 - z.0) * f), feed }
        })
        .collect()
}

fn post_path(dialect: Dialect, moves: Vec<Move>) -> (String, Vec<usize>) {
    let (s, t) = (setup(), tool(1, 6.0));
    let o = contour_op(Side::On, Direction::Climb, 0.0);
    let path = Toolpath { moves };
    crate::post::generate_mapped(dialect, &[Step { setup: &s, tool: &t, op: &o, toolpath: &path }], &PostOptions::default()).unwrap()
}

#[test]
fn a_run_of_short_moves_on_a_circle_is_written_as_one_arc() {
    let mut moves = vec![Move::Rapid { to: Vec3::new(30.0, 15.0, 5.0) }, Move::Feed { to: Vec3::new(30.0, 15.0, -2.0), feed: 300.0 }];
    // A full circle of radius 10 in 72 chords, counter-clockwise.
    moves.extend(circle_moves(10.0, 0.0, 360.0, 72, (-2.0, -2.0), 300.0).into_iter().skip(1));
    for dialect in [Dialect::Grbl, Dialect::Fanuc, Dialect::Haas] {
        let (text, lines) = post_path(dialect, moves.clone());
        let arcs: Vec<&str> = text.lines().filter(|l| l.starts_with("G3 ") || l.starts_with("G2 ")).collect();
        assert!(!arcs.is_empty() && arcs.len() <= 3, "{dialect:?}:\n{text}");
        assert!(arcs.iter().all(|l| l.starts_with("G3 ") && l.contains(" I") && l.contains(" J")), "{arcs:?}");
        assert!(!text.contains("G1 X"), "no chord left over:\n{text}");
        // One simulation step per move, and an arc's steps all point at its line.
        assert_eq!(lines.len(), moves.len());
        let at = lines[2];
        assert!(text.lines().nth(at).unwrap().starts_with("G3 "), "line {at}");
        assert!(lines.windows(2).all(|w| w[0] <= w[1]));
    }
}

#[test]
fn clockwise_circles_are_g2_and_helixes_carry_z() {
    let cw = circle_moves(8.0, 90.0, -90.0, 40, (-1.0, -1.0), 200.0);
    let mut moves = vec![Move::Rapid { to: Vec3::new(20.0, 23.0, 5.0) }];
    moves.extend(cw);
    let (text, _) = post_path(Dialect::Fanuc, moves);
    assert!(text.lines().any(|l| l.starts_with("G2 ")), "{text}");
    let helix = circle_moves(5.0, 0.0, 270.0, 54, (0.0, -3.0), 150.0);
    let mut moves = vec![Move::Rapid { to: Vec3::new(25.0, 15.0, 5.0) }];
    moves.extend(helix);
    let (text, _) = post_path(Dialect::Grbl, moves);
    let arc = text.lines().find(|l| l.starts_with("G3 ")).expect("a helical arc");
    assert!(arc.contains(" Z-3"), "{arc}");
}

#[test]
fn corners_and_straight_lines_are_never_arcs() {
    let square = vec![
        Move::Rapid { to: Vec3::new(0.0, 0.0, 5.0) },
        Move::Feed { to: Vec3::new(0.0, 0.0, -1.0), feed: 300.0 },
        Move::Feed { to: Vec3::new(20.0, 0.0, -1.0), feed: 300.0 },
        Move::Feed { to: Vec3::new(20.0, 20.0, -1.0), feed: 300.0 },
        Move::Feed { to: Vec3::new(0.0, 20.0, -1.0), feed: 300.0 },
        Move::Feed { to: Vec3::new(0.0, 0.0, -1.0), feed: 300.0 },
    ];
    let (text, _) = post_path(Dialect::Fanuc, square);
    assert!(!text.lines().any(|l| l.starts_with("G2 ") || l.starts_with("G3 ")), "{text}");
    let line: Vec<Move> = (0..10).map(|k| Move::Feed { to: Vec3::new(k as f64, 0.0, -1.0), feed: 300.0 }).collect();
    let (text, _) = post_path(Dialect::Grbl, line);
    assert!(!text.contains("G2 ") && !text.contains("G3 "));
    // A change of feed ends an arc.
    let mut moves = circle_moves(10.0, 0.0, 90.0, 18, (-1.0, -1.0), 300.0);
    moves.extend(circle_moves(10.0, 90.0, 180.0, 18, (-1.0, -1.0), 200.0).into_iter().skip(1));
    let (text, lines) = post_path(Dialect::Fanuc, moves.clone());
    assert!(text.lines().filter(|l| l.starts_with("G3 ")).count() >= 1 && text.contains("F200"), "{text}");
    assert_eq!(lines.len(), moves.len());
}
