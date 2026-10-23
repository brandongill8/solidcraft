//! The manufacturing job a design carries: a tool library, setups (where the stock sits and
//! which work offset it is) and operations (what to cut, with which tool, how). Saved with the
//! design; a design without a job has none.

use serde::{Deserialize, Serialize};
use solidcraft_geom::{Plane, Vec3};

use crate::error::{Result, finite, invalid};
use crate::tool::{MAX_NAME_BYTES, Tool};

pub const MAX_TOOLS: usize = 256;
pub const MAX_SETUPS: usize = 64;
pub const MAX_OPERATIONS: usize = 1024;
pub const MAX_POINTS: usize = 100_000;
/// Highest spindle speed accepted (rpm).
pub const MAX_RPM: f64 = 100_000.0;
/// Highest feed accepted (mm/min).
pub const MAX_FEED: f64 = 100_000.0;

/// The job: everything saved under a design's `manufacture`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Job {
    pub tools: Vec<Tool>,
    pub setups: Vec<Setup>,
    pub operations: Vec<Operation>,
}

impl Job {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.setups.is_empty() && self.operations.is_empty()
    }
    pub fn tool(&self, id: u32) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }
    pub fn setup(&self, id: u32) -> Option<&Setup> {
        self.setups.iter().find(|s| s.id == id)
    }
    pub fn operation(&self, id: u32) -> Option<&Operation> {
        self.operations.iter().find(|o| o.id == id)
    }

    /// The whole job is consistent: bounded, unique ids, every operation's tool and setup exist
    /// and suit it.
    pub fn validate(&self) -> Result<()> {
        if self.tools.len() > MAX_TOOLS || self.setups.len() > MAX_SETUPS || self.operations.len() > MAX_OPERATIONS {
            return invalid(format!("a job has at most {MAX_TOOLS} tools, {MAX_SETUPS} setups and {MAX_OPERATIONS} operations"));
        }
        unique(self.tools.iter().map(|t| t.id), "tool")?;
        unique(self.setups.iter().map(|s| s.id), "setup")?;
        unique(self.operations.iter().map(|o| o.id), "operation")?;
        for t in &self.tools {
            t.validate()?;
        }
        for s in &self.setups {
            s.validate()?;
        }
        for o in &self.operations {
            let tool = self
                .tool(o.tool)
                .ok_or_else(|| crate::ManufactureError::Invalid(format!("operation {} uses tool {}, which is not in the library", o.id, o.tool)))?;
            let setup = self
                .setup(o.setup)
                .ok_or_else(|| crate::ManufactureError::Invalid(format!("operation {} is in setup {}, which does not exist", o.id, o.setup)))?;
            o.validate(tool, setup)?;
        }
        Ok(())
    }
}

fn unique(ids: impl Iterator<Item = u32>, what: &str) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return invalid(format!("two {what}s have the number {id}"));
        }
    }
    Ok(())
}

/// Stock as a box in the setup's work coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stock {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Stock {
    pub fn validate(&self) -> Result<()> {
        for (lo, hi) in self.min.iter().zip(&self.max) {
            finite(*lo, "a stock limit")?;
            finite(*hi, "a stock limit")?;
            if lo >= hi {
                return invalid("the stock needs a positive size along every axis (min below max)");
            }
        }
        Ok(())
    }
}

fn world_xy() -> Plane {
    Plane::XY
}
fn default_work_offset() -> u32 {
    54
}
fn default_clearance() -> f64 {
    10.0
}

/// A setup: the stock held in the machine, with the work coordinate system the operations'
/// numbers are in. The frame's Z axis is the tool axis, so a frame on a tilted face makes a
/// 3+2 setup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub id: u32,
    pub name: String,
    /// The work coordinate system in model space (origin, X and Y axes; Z = X × Y).
    #[serde(default = "world_xy")]
    pub frame: Plane,
    /// Fixture offset number (54 for G54 … 59 for G59).
    #[serde(default = "default_work_offset")]
    pub work_offset: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<Stock>,
    /// The height (work Z) the tool travels at between features.
    #[serde(default = "default_clearance")]
    pub clearance: f64,
}

impl Setup {
    pub fn validate(&self) -> Result<()> {
        if self.id == 0 {
            return invalid("a setup number is 1 or more");
        }
        if self.name.len() > MAX_NAME_BYTES {
            return invalid(format!("a setup name is at most {MAX_NAME_BYTES} bytes"));
        }
        if !(54..=59).contains(&self.work_offset) {
            return invalid("the work offset is 54 to 59 (G54 to G59)");
        }
        finite(self.clearance, "the clearance height")?;
        if self.clearance.abs() > 1e6 {
            return invalid("the clearance height is within 1,000,000 mm");
        }
        let f = &self.frame;
        if !(f.origin.is_finite() && f.x.is_finite() && f.y.is_finite()) {
            return invalid("the setup frame has a number that is not finite");
        }
        if f.origin.x.abs().max(f.origin.y.abs()).max(f.origin.z.abs()) > 1e6 {
            return invalid("the setup origin is within 1,000,000 mm of the model origin");
        }
        let n = f.normal();
        if !(n.len() > 0.5 && (f.x.dot(f.y)).abs() < 1e-6) {
            return invalid("the setup frame's axes must be perpendicular unit vectors");
        }
        if let Some(s) = &self.stock {
            s.validate()?;
        }
        Ok(())
    }

    /// A model-space point in work coordinates.
    pub fn to_local(&self, p: Vec3) -> Vec3 {
        let d = p - self.frame.origin;
        Vec3::new(d.dot(self.frame.x), d.dot(self.frame.y), d.dot(self.frame.normal()))
    }

    /// A point in work coordinates, in model space.
    pub fn to_model(&self, p: Vec3) -> Vec3 {
        self.frame.origin + self.frame.x * p.x + self.frame.y * p.y + self.frame.normal() * p.z
    }

    /// The setup's Z axis in model space.
    pub fn tool_axis(&self) -> Vec3 {
        self.frame.normal()
    }
}

/// Where an operation takes its outlines: the closed profiles of a sketch (by index; none
/// listed means every profile the sketch draws).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub sketch: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<usize>,
}

/// Which side of the outline the tool stays on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Outside the outer outline and inside its holes: the finished part is the profile.
    #[default]
    Outside,
    /// Inside the outer outline and outside its holes: the profile is a cut-out.
    Inside,
    /// The tool centre follows the outline itself.
    On,
}

/// How the cutter meets the material.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Material on the left of the travel (the usual finish, for a clockwise spindle).
    #[default]
    Climb,
    Conventional,
}

/// The holes of a drill operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case", deny_unknown_fields)]
pub enum DrillPoints {
    /// The centres of the sketch's circular profiles (their diameters are checked against the
    /// drill).
    Sketch {
        sketch: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        profiles: Vec<usize>,
    },
    /// Positions in work coordinates.
    Points { points: Vec<[f64; 2]> },
}

fn default_stepover_pocket() -> f64 {
    0.4
}
fn default_stepover_face() -> f64 {
    0.6
}
fn default_ramp() -> f64 {
    3.0
}
fn default_helix_angle() -> f64 {
    3.0
}

/// How the cutter first goes down into the material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryMode {
    /// Straight down at the plunge feed.
    Plunge,
    /// Slanting down along the outline at the ramp angle.
    Ramp,
    /// Spiralling down in a circle beside the outline at the helix angle.
    Helical,
}

impl EntryMode {
    pub fn label(self) -> &'static str {
        match self {
            EntryMode::Plunge => "plunge",
            EntryMode::Ramp => "ramp",
            EntryMode::Helical => "helical",
        }
    }
}

/// The way a contour or pocket enters each depth, with its numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub mode: EntryMode,
    /// Ramp angle (degrees).
    pub ramp_angle: f64,
    /// Helix angle (degrees).
    pub helix_angle: f64,
    /// Diameter of the circle the tool centre follows going down (mm); 0 is automatic.
    pub helix_diameter: f64,
}

/// What an operation does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpKind {
    /// Skim the stock's top over its full extent (needs the setup's stock).
    Face {
        /// Overhang beyond the stock edges (mm).
        #[serde(default)]
        margin: f64,
        /// Step between passes as a fraction of the tool diameter.
        #[serde(default = "default_stepover_face")]
        stepover: f64,
    },
    /// Follow an outline.
    Contour {
        source: Source,
        #[serde(default)]
        side: Side,
        #[serde(default)]
        direction: Direction,
        /// Stock left on the walls (mm).
        #[serde(default)]
        allowance: f64,
        /// Ramp angle into each depth (degrees); 0 plunges.
        #[serde(default = "default_ramp")]
        ramp_angle: f64,
        /// How each depth is entered; none (older designs) ramps when `ramp_angle` is above 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<EntryMode>,
        /// Angle of a helical entry (degrees).
        #[serde(default = "default_helix_angle")]
        helix_angle: f64,
        /// Diameter of a helical entry's circle (mm); 0 takes 90% of the tool diameter.
        #[serde(default)]
        helix_diameter: f64,
    },
    /// Clear the inside of an outline in concentric passes, around islands.
    Pocket {
        source: Source,
        #[serde(default)]
        direction: Direction,
        #[serde(default = "default_stepover_pocket")]
        stepover: f64,
        #[serde(default)]
        allowance: f64,
        /// A last full-depth pass along the walls (needs an allowance).
        #[serde(default)]
        finish: bool,
        #[serde(default = "default_ramp")]
        ramp_angle: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<EntryMode>,
        #[serde(default = "default_helix_angle")]
        helix_angle: f64,
        #[serde(default)]
        helix_diameter: f64,
    },
    /// Drill holes.
    Drill {
        points: DrillPoints,
        /// Peck depth; none drills each hole in one go.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peck: Option<f64>,
        /// Dwell at the bottom (seconds).
        #[serde(default)]
        dwell: f64,
    },
}

impl OpKind {
    pub fn label(&self) -> &'static str {
        match self {
            OpKind::Face { .. } => "face",
            OpKind::Contour { .. } => "contour",
            OpKind::Pocket { .. } => "pocket",
            OpKind::Drill { .. } => "drill",
        }
    }

    /// How a contour or pocket enters each depth (none for facing and drilling).
    pub fn entry(&self) -> Option<Entry> {
        match self {
            OpKind::Contour { ramp_angle, entry, helix_angle, helix_diameter, .. }
            | OpKind::Pocket { ramp_angle, entry, helix_angle, helix_diameter, .. } => {
                let mode = entry.unwrap_or(if *ramp_angle > 0.0 { EntryMode::Ramp } else { EntryMode::Plunge });
                Some(Entry { mode, ramp_angle: *ramp_angle, helix_angle: *helix_angle, helix_diameter: *helix_diameter })
            }
            _ => None,
        }
    }
}

fn default_retract() -> f64 {
    2.0
}
fn default_stepdown() -> f64 {
    1.0
}

/// One operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: u32,
    pub name: String,
    pub setup: u32,
    pub tool: u32,
    pub kind: OpKind,
    /// Work Z of the surface the cut starts from.
    pub top: f64,
    /// Work Z of the deepest cut.
    pub bottom: f64,
    /// Height above `top` where feed moves begin (mm).
    #[serde(default = "default_retract")]
    pub retract: f64,
    /// Depth of each pass (mm); the last pass ends exactly at `bottom`.
    #[serde(default = "default_stepdown")]
    pub stepdown: f64,
    pub spindle_rpm: f64,
    /// Cutting feed (mm/min).
    pub feed: f64,
    /// Plunge feed (mm/min); 0 takes half the cutting feed.
    #[serde(default)]
    pub plunge_feed: f64,
    #[serde(default)]
    pub coolant: bool,
}

impl Operation {
    pub fn plunge(&self) -> f64 {
        if self.plunge_feed > 0.0 { self.plunge_feed } else { self.feed * 0.5 }
    }

    /// The feed of a ramped or helical entry: the plunge feed when one is set, otherwise the
    /// cutting feed (the tool is cutting sideways as well as down).
    pub fn entry_feed(&self) -> f64 {
        if self.plunge_feed > 0.0 { self.plunge_feed } else { self.feed }
    }

    /// The operation is usable with this tool and setup.
    pub fn validate(&self, tool: &Tool, setup: &Setup) -> Result<()> {
        if self.id == 0 {
            return invalid("an operation number is 1 or more");
        }
        if self.name.len() > MAX_NAME_BYTES {
            return invalid(format!("an operation name is at most {MAX_NAME_BYTES} bytes"));
        }
        for (v, what) in [
            (self.top, "top"),
            (self.bottom, "bottom"),
            (self.retract, "retract"),
            (self.stepdown, "stepdown"),
            (self.spindle_rpm, "spindle speed"),
            (self.feed, "feed"),
            (self.plunge_feed, "plunge feed"),
        ] {
            finite(v, what)?;
        }
        if self.bottom >= self.top {
            return invalid("the bottom must be below the top");
        }
        if self.retract < 0.0 {
            return invalid("the retract height cannot be negative");
        }
        if !(self.stepdown > 0.0) {
            return invalid("the stepdown must be above 0");
        }
        if !(self.spindle_rpm > 0.0 && self.spindle_rpm <= MAX_RPM) {
            return invalid(format!("the spindle speed is above 0 and at most {MAX_RPM} rpm"));
        }
        if !(self.feed > 0.0 && self.feed <= MAX_FEED) || self.plunge_feed < 0.0 || self.plunge_feed > MAX_FEED {
            return invalid(format!("feeds are above 0 and at most {MAX_FEED} mm/min"));
        }
        if setup.clearance < self.top + self.retract {
            return invalid(format!(
                "the setup's clearance height ({}) is below the top plus the retract height ({}): raise the clearance",
                setup.clearance,
                self.top + self.retract
            ));
        }
        match (&self.kind, tool.kind.is_drill()) {
            (OpKind::Drill { .. }, false) => return invalid("a drill operation needs a drill in the library"),
            (OpKind::Drill { .. }, true) => {}
            (_, true) => return invalid("a drill cannot mill: pick an end mill for this operation"),
            _ => {}
        }
        let ratio = |v: f64, what: &str| -> Result<()> {
            finite(v, what)?;
            if !(v > 0.0 && v <= 1.0) {
                return invalid(format!("{what} is a fraction of the tool diameter above 0 and at most 1"));
            }
            Ok(())
        };
        let allowance = |v: f64| -> Result<()> {
            finite(v, "the allowance")?;
            if v < 0.0 || v > tool.diameter {
                return invalid("the allowance is between 0 and the tool diameter");
            }
            Ok(())
        };
        let ramp = |v: f64| -> Result<()> {
            finite(v, "the ramp angle")?;
            if !(0.0..=45.0).contains(&v) {
                return invalid("the ramp angle is between 0 and 45 degrees");
            }
            Ok(())
        };
        let entry = |e: Entry| -> Result<()> {
            ramp(e.ramp_angle)?;
            finite(e.helix_angle, "the helix angle")?;
            if !(e.helix_angle > 0.0 && e.helix_angle <= 45.0) {
                return invalid("the helix angle is above 0 and at most 45 degrees");
            }
            finite(e.helix_diameter, "the helix diameter")?;
            if !(0.0..=1000.0).contains(&e.helix_diameter) {
                return invalid("the helix diameter is between 0 (automatic) and 1000 mm");
            }
            if e.mode == EntryMode::Ramp && e.ramp_angle <= 0.0 {
                return invalid("a ramped entry needs a ramp angle above 0");
            }
            Ok(())
        };
        match &self.kind {
            OpKind::Face { margin, stepover } => {
                ratio(*stepover, "the stepover")?;
                finite(*margin, "the margin")?;
                if *margin < 0.0 || *margin > 1000.0 {
                    return invalid("the margin is between 0 and 1000 mm");
                }
                if setup.stock.is_none() {
                    return invalid("facing needs the setup's stock: give the setup a stock box");
                }
            }
            OpKind::Contour { allowance: a, .. } => {
                allowance(*a)?;
                if let Some(e) = self.kind.entry() {
                    entry(e)?;
                }
            }
            OpKind::Pocket { stepover, allowance: a, .. } => {
                ratio(*stepover, "the stepover")?;
                allowance(*a)?;
                if let Some(e) = self.kind.entry() {
                    entry(e)?;
                }
            }
            OpKind::Drill { points, peck, dwell } => {
                if let Some(p) = peck {
                    finite(*p, "the peck depth")?;
                    if !(*p > 0.0) {
                        return invalid("the peck depth must be above 0");
                    }
                }
                finite(*dwell, "the dwell")?;
                if !(0.0..=60.0).contains(dwell) {
                    return invalid("the dwell is between 0 and 60 seconds");
                }
                if let DrillPoints::Points { points } = points {
                    if points.is_empty() || points.len() > MAX_POINTS {
                        return invalid(format!("a drill operation has 1 to {MAX_POINTS} points"));
                    }
                    if points.iter().any(|p| !(p[0].is_finite() && p[1].is_finite())) {
                        return invalid("a drill point has a number that is not finite");
                    }
                }
            }
        }
        Ok(())
    }
}
