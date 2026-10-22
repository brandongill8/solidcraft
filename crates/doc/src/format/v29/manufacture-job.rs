//! A design can carry a manufacturing job under `manufacture`: a tool library, setups (work
//! coordinate frame, stock box, work offset) and operations (face, contour, pocket, drill) that
//! the `manufacture.*` commands edit and post to G-code. Older designs need no transformation:
//! a design without one has an empty job, and an empty job is not saved. The last release asks
//! for an update instead of dropping it on save.
//! A tool may also carry `flute_length` and `shank_diameter` (mm), which the simulation uses to
//! find the shank touching stock or the part; both are optional and not saved when unset.
