//! A job's tools may be a `spot_drill`, `reamer`, `shell_mill` or `face_mill` (besides the flat,
//! ball, bull nose and drill kinds), and a contour or pocket operation may carry how it enters
//! each depth: `entry` (`plunge`, `ramp` or `helical`), `helix_angle` (degrees) and
//! `helix_diameter` (mm, 0 for automatic). All of them are optional and an operation without
//! `entry` keeps its meaning (it ramps when `ramp_angle` is above 0, else plunges), so older
//! designs need no transformation. The last release refuses a design with the newer format
//! number rather than dropping the new kinds or settings on save.
