# Manufacturing (2.5D CAM core)

An opt-in core for planning 2.5D (three-axis) milling from a design's sketches and posting
G-code. Everything is a command, so scripts, the CLI and the control channel drive it exactly as
the Manufacture ribbon and its windows do.

The job lives in the design (`manufacture`), so it saves, reopens and undoes with it. A design
without CAM saves exactly as before. The planning is the `solidcraft-manufacture` crate (L1,
geometry only); the `manufacture.*` commands resolve sketches into it.

Units are millimetres, Z is the tool axis (up is positive), feeds are mm/min.

## Job

- **Tool** (`manufacture.tool.set`): number, kind, diameter, flutes, corner radius, point angle,
  and optionally `flute_length` and `shank_diameter`. The kinds:

  | Kind | Works | Notes |
  |---|---|---|
  | `flat_end_mill` | sideways and down | |
  | `ball_end_mill` | sideways and down | planned as if its whole diameter cut at every depth |
  | `bull_nose_end_mill` | sideways and down | `corner_radius` |
  | `drill` | along its axis | drill operations; point angle 118 |
  | `spot_drill` | along its axis | drill operations; point angle 90 |
  | `reamer` | along its axis | drill operations; flat end |
  | `shell_mill` | sideways and down | large diameter, flat end |
  | `face_mill` | sideways and down | very large diameter, flat end; for facing |

  A new tool of a kind starts with that kind's usual flutes and point angle. Drills, spot drills
  and reamers cannot mill, and the others cannot drill.
- **Saved tools** (`manufacture.library.list`, `.save`, `.load`, `.delete`): a library file in
  the SolidCraft data folder (`tool-library.json`, or any file with `path`), kept across
  designs. `save` copies a design's tool into it by name (the same name replaces), `load` adds a
  saved tool to the design (the next free tool number unless one is asked for; one undo step),
  `delete` removes it from the file. A file that does not read is refused and never
  overwritten.
- **Setup** (`manufacture.setup.set`): a work frame (`plane` XY/XZ/YZ and `origin`, or a full
  `frame`), work offset G54-G59, optional stock box in work coordinates, clearance height. Sketch
  planes must be parallel to the setup's XY; tilted ones are refused.
- **Operation** (`manufacture.operation.set`): a setup, a tool, a kind, `top`/`bottom` (work Z),
  retract, stepdown, spindle speed, feed, plunge feed, coolant.

| Kind | Cuts |
|---|---|
| `face` | zig-zag over the stock top, with margin and stepover |
| `contour` | around the chosen sketch profiles: `outside`, `inside` or `on`, climb or conventional, with allowance and an entry |
| `pocket` | concentric passes inside the profiles, islands respected, optional wall finish pass, with an entry |
| `drill` | at circle centres of a sketch, or at given points; optional peck and dwell |

`sketch` can be an id or a name. Deleting a tool or setup that an operation uses is refused.

### Entry into the material

A contour or pocket goes down to each depth (`top` to `bottom` in passes of `stepdown`) with its
`entry`:

| `entry` | Moves | Numbers |
|---|---|---|
| `plunge` | straight down at the plunge feed | |
| `ramp` | slanting down along the outline, then a full lap at depth | `ramp_angle` (1 to 45 degrees) |
| `helical` | a circle beside the outline, spiralling down, then a flat lap | `helix_angle` (degrees) and `helix_diameter` (mm of the circle the tool centre follows; 0 is 90% of the tool diameter) |

A helix starts where the circle fits: the planner looks along the outline, nearest to the tool
first (a corner never has room), and tries smaller circles before giving up. A circle that
fits nowhere ramps instead and the plan says so in its warnings. Around a part (`outside`) the
circle is in the air beside it, inside a pocket or an `inside` contour it is in the free space;
a contour on the line itself has no room and ramps. Every point of the circle keeps the tool
radius (plus the allowance) from the walls. The descent is a whole number of turns, so the slope
is never steeper than `helix_angle`.

The feed of a ramped or helical entry is `plunge_feed` when one is set and the cutting `feed`
otherwise; a plunge runs at `plunge_feed`, or half the feed. Without `entry` (older designs) a
`ramp_angle` above 0 ramps and 0 plunges, as before.

## Output

`manufacture.toolpath {operation}` returns move count, length, time estimate, bounds and
warnings (`moves: true` lists the moves). `manufacture.post {dialect}` returns the program, or
writes it to `path` (an existing file is kept unless `overwrite: true`).

Dialects: `grbl` (G21 G90, M0 pause at tool changes, drilling expanded to moves) and
`fanuc` / `haas` (O-number, `T M6`, `G43`, G81/G82/G83 canned cycles, controller-specific end
of program).

## Simulation and collision checking

`manufacture.simulate` cuts the setup's stock away along the operations' toolpaths and checks the
tool against the model. The stock must be set on the setup. It runs one setup at a time, from
the setup's original stock; operations in other setups do not affect it.

- The stock is a height map (a vertical tool only removes material from the top down) and the
  model is a column of solid intervals per cell, found from its triangles. `resolution` is the
  cell size (default 0.5 mm; coarser cells are used when the stock would need more than 4
  million).
- Collisions: **gouge** (the flutes cut into the model), **shank** (the shank above
  `flute_length` meets stock still standing, or the model), **rapid** (a rapid move runs into
  stock or the model) and **floor** (the tip goes below the stock). Each is listed once per
  move with the first position and the deepest overlap.
- Overlaps under `tolerance` (default one cell, at least 0.05 mm) are not reported, and the
  tool outline is cut to the grid, so a cut that rides a wall or a sloped face by less than
  about a cell is not a gouge.
- The reply has the volumes (stock, removed, remaining, part, left to cut, cut from the part),
  the counts per kind, the collisions, and with `surface: <cells>` the stock top for drawing.
  `until: <move>` stops after that move, for stepping through a program.
- `part: false` ignores the model (stock removal only); `bodies` limits which bodies count.

Tools take optional `flute_length` and `shank_diameter`; without a flute length, nothing above
the tip is checked as a shank.

## The windows

The Manufacture ribbon opens one window per tab, so any of them can stay open beside the model:
**Tools** (the design's tools, the editor for the eight kinds, and the saved tools to add from),
**Setup**, **Operations** and **G-code**. Each is a plain front end of the commands above.

- While **Setup** is open the stock is drawn in the model view as a translucent box from the
  stock min to max, measured from the zero point, with the work axes at the zero point and the
  size beside it. It follows the numbers as they are typed, before anything is saved.
- While **Operations** is open (and "Show toolpaths on the part" is ticked) every operation's
  toolpath is drawn on the part without running a simulation: cutting moves in the accent
  colour, moves down into the material (plunge, ramp, helix) in the warning colour, rapids
  grey. The operation being made or edited is drawn in full colour, the others dimmed; a new
  operation appears as soon as it is added. An operation that cannot be planned says why in the
  list.

In the app, `ui.start` of `manufacture.simulate` opens the Machining Simulation window: the
stock is drawn over the viewport with its camera, with the path so far, a slider and Play to
step through the program, and the collisions marked with red crosses.

## Limits

The planner works in short straight moves (chord error 0.005 mm by default, `tolerance`); the post
turns every run of at least three feed moves along one circle or helix about the tool axis into a
single `G2`/`G3` with `I`/`J` (and `Z` for a helix), so round pockets, bores and fillets take a few
lines. The runner and the simulation still walk the straight moves. No 4/5-axis or 3D surfacing. The simulation does not model undercuts, tool holders or fixtures, and checks
the tool only against the stock and the model. Sizes and work are capped, and bad numbers are
errors, never panics.

## History, side panel and simulation controls

In the Manufacture workspace the bottom strip is the **Manufacture history**: the setups and then the operations in the order the program runs them; a click opens the entry in its window. The panel on the right lists the tools that were built, the setups and the operations, each with a New button, a click to edit and a delete button. The workspace switcher shows Manufacture while this tab is open.

The Machining Simulation window plays the program in real cutting time (feeds as set, rapids at the
rapid speed in Preferences). The slider and the clock above it show minutes and seconds of the cut,
and **Speed** is a multiple of real time (a new window starts at the speed that plays the whole
program in the seconds set in Preferences). The stock is cut on its own fine grid: **Drawing detail**
(Draft to Smooth) sets the cells it is drawn with, and the stock is shaded smoothly across cells.
**Show stock**, **Show toolpath**, **Show tool** (the cutter drawn at its real size from the Tools
window: end mills, ball and bull nose, drills, spot drills, reamers, shell and face mills, turning
with the spindle) and **Show G-code** (a window listing the program in Grbl, Fanuc or Haas form with
the line being cut highlighted) are checkboxes. **Check collisions** is off until asked for; it runs
the gouge and volume check on its own **Check resolution** grid and marks the finds with red
crosses.

Preferences, **Simulation** sets the cells for each Drawing detail, the finest cell, the detail and
checkboxes a new window starts with, the seconds to play a program, the rapid speed used for the
time, the fastest tool turn in the picture and the check's cell.
