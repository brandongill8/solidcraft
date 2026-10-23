//! Cutting tools.

use serde::{Deserialize, Serialize};

use crate::error::{Result, finite, invalid};

/// Largest tool diameter the library accepts (mm).
pub const MAX_TOOL_DIAMETER: f64 = 500.0;
/// Longest tool name (bytes).
pub const MAX_NAME_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    FlatEndMill,
    BallEndMill,
    BullNoseEndMill,
    Drill,
    /// A short, stiff drill with a wide point that starts a hole (default point angle 90).
    SpotDrill,
    /// Finishes a drilled hole to size; it cuts along its axis only, with a flat end.
    Reamer,
    /// A large-diameter mill with a flat end, for wide shallow cuts and shoulders.
    ShellMill,
    /// A very large-diameter flat-ended mill with inserts, for skimming surfaces.
    FaceMill,
}

impl ToolKind {
    /// Every kind, in the order a tool list offers them.
    pub const ALL: [ToolKind; 8] = [
        ToolKind::FlatEndMill,
        ToolKind::BallEndMill,
        ToolKind::BullNoseEndMill,
        ToolKind::Drill,
        ToolKind::SpotDrill,
        ToolKind::Reamer,
        ToolKind::ShellMill,
        ToolKind::FaceMill,
    ];

    /// A tool that works along its axis only (drilling operations), never sideways.
    pub fn is_drill(self) -> bool {
        matches!(self, ToolKind::Drill | ToolKind::SpotDrill | ToolKind::Reamer)
    }
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::FlatEndMill => "flat end mill",
            ToolKind::BallEndMill => "ball end mill",
            ToolKind::BullNoseEndMill => "bull nose end mill",
            ToolKind::Drill => "drill",
            ToolKind::SpotDrill => "spot drill",
            ToolKind::Reamer => "reamer",
            ToolKind::ShellMill => "shell mill",
            ToolKind::FaceMill => "face mill",
        }
    }
    /// The number of cutting edges a new tool of this kind starts with.
    pub fn default_flutes(self) -> u32 {
        match self {
            ToolKind::FlatEndMill | ToolKind::BallEndMill | ToolKind::BullNoseEndMill | ToolKind::Drill | ToolKind::SpotDrill => 2,
            ToolKind::Reamer => 6,
            ToolKind::ShellMill => 4,
            ToolKind::FaceMill => 5,
        }
    }
    /// The point angle a new tool of this kind starts with (degrees; used by drills only).
    pub fn default_tip_angle(self) -> f64 {
        if self == ToolKind::SpotDrill { 90.0 } else { 118.0 }
    }
}

/// A tool in the library. `id` is the tool number the controller calls (`T<id>`, length offset
/// `H<id>`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub kind: ToolKind,
    /// Cutting diameter (mm).
    pub diameter: f64,
    #[serde(default = "default_flutes")]
    pub flutes: u32,
    /// Corner radius of a bull nose mill (mm).
    #[serde(default)]
    pub corner_radius: f64,
    /// Point angle of a drill (degrees).
    #[serde(default = "default_tip_angle")]
    pub tip_angle: f64,
    /// Length of the cutting flutes from the tip (mm). Above it is the shank, which does not
    /// cut: the simulation reports it touching stock or the part. `None` means the whole tool
    /// can cut (nothing to collide with above the tip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flute_length: Option<f64>,
    /// Shank diameter (mm); the cutting diameter when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shank_diameter: Option<f64>,
}

fn default_flutes() -> u32 {
    2
}
fn default_tip_angle() -> f64 {
    118.0
}

impl Tool {
    pub fn radius(&self) -> f64 {
        self.diameter * 0.5
    }

    pub fn validate(&self) -> Result<()> {
        if self.id == 0 {
            return invalid("a tool number is 1 or more");
        }
        if self.name.len() > MAX_NAME_BYTES {
            return invalid(format!("a tool name is at most {MAX_NAME_BYTES} bytes"));
        }
        finite(self.diameter, "the tool diameter")?;
        if !(self.diameter > 0.0 && self.diameter <= MAX_TOOL_DIAMETER) {
            return invalid(format!("the tool diameter must be above 0 and at most {MAX_TOOL_DIAMETER} mm"));
        }
        if !(1..=16).contains(&self.flutes) {
            return invalid("a tool has 1 to 16 flutes");
        }
        finite(self.corner_radius, "the corner radius")?;
        if self.corner_radius < 0.0 || self.corner_radius > self.radius() {
            return invalid("the corner radius is between 0 and half the diameter");
        }
        finite(self.tip_angle, "the point angle")?;
        if !(self.tip_angle > 0.0 && self.tip_angle < 180.0) {
            return invalid("the point angle is between 0 and 180 degrees");
        }
        if let Some(l) = self.flute_length {
            finite(l, "the flute length")?;
            if !(l > 0.0 && l <= 2000.0) {
                return invalid("the flute length is above 0 and at most 2000 mm");
            }
        }
        if let Some(d) = self.shank_diameter {
            finite(d, "the shank diameter")?;
            if !(d > 0.0 && d <= MAX_TOOL_DIAMETER) {
                return invalid(format!("the shank diameter must be above 0 and at most {MAX_TOOL_DIAMETER} mm"));
            }
        }
        Ok(())
    }

    /// Radius of the shank (the cutting radius unless a shank diameter is set).
    pub fn shank_radius(&self) -> f64 {
        self.shank_diameter.map_or(self.radius(), |d| d * 0.5)
    }

    /// How far the cutting surface is above the tip at distance `r` from the axis (0 for a flat
    /// mill's bottom, a sphere for a ball mill, a corner radius for a bull nose, a cone for a drill).
    pub fn lift(&self, r: f64) -> f64 {
        let big = self.radius();
        let r = r.abs().min(big);
        match self.kind {
            // A reamer, shell mill and face mill end flat, like a flat end mill.
            ToolKind::FlatEndMill | ToolKind::Reamer | ToolKind::ShellMill | ToolKind::FaceMill => 0.0,
            ToolKind::BallEndMill => big - (big * big - r * r).max(0.0).sqrt(),
            ToolKind::BullNoseEndMill => {
                let rc = self.corner_radius;
                let flat = big - rc;
                if rc <= 1e-9 || r <= flat {
                    0.0
                } else {
                    let d = r - flat;
                    rc - (rc * rc - d * d).max(0.0).sqrt()
                }
            }
            ToolKind::Drill | ToolKind::SpotDrill => r / (self.tip_angle.to_radians() * 0.5).tan().max(1e-3),
        }
    }
}
