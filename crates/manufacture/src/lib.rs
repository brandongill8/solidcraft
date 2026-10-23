//! Manufacturing for SolidCraft: a tool library, setups, and 2.5D toolpaths (face, contour,
//! pocket, drill) with G-code posts for Grbl, Fanuc and Haas controllers.
//!
//! The crate knows geometry only as 2D regions in a setup's work coordinates; the engine
//! resolves sketches into them. Units are millimetres, Z is the tool axis (up is positive).
//!
//! - [`job`]: what a design saves: tools, setups, operations.
//! - [`plan`]: one operation to a [`path::Toolpath`].
//! - [`offset`]: robust polygon offsetting under it.
//! - [`post`]: toolpaths to G-code.
//! - [`sim`]: material removal and collision checking against the stock and the part.
//!
//! Nothing here panics on hostile input: sizes and work are capped and every number is checked.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod error;
pub mod job;
pub mod offset;
pub mod path;
pub mod plan;
pub mod post;
mod ring;
pub mod sim;
pub mod tool;

pub use error::{ManufactureError, Result};
pub use job::{Direction, DrillPoints, Entry, EntryMode, Job, OpKind, Operation, Setup, Side, Source, Stock};
pub use path::{DrillCycle, Move, Stats, Toolpath};
pub use plan::{DEFAULT_TOLERANCE, Geometry, Plan, Site, circle_of, circle_sites, levels, plan, region_to_work};
pub use post::{Dialect, PostOptions, Step, generate, generate_mapped};
pub use sim::{Collision, CollisionKind, Grid, Part, RunOp, RunSeg, Runner, SimOp, SimOptions, Simulation, Surface, Volumes, simulate};
pub use tool::{Tool, ToolKind};

#[cfg(test)]
mod tests;
