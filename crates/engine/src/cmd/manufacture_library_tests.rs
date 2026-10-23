use serde_json::{Value, json};

use crate::Session;

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
}

/// A library file path that does not exist yet, in a folder of its own.
fn library_path(tag: &str) -> String {
    let dir = solidcraft_io::vfs::temp_dir().join(format!("solidcraft-tool-library-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("nested").join("tools.json").to_string_lossy().into_owned()
}

fn clean(path: &str) {
    if let Some(dir) = std::path::Path::new(path).parent().and_then(|d| d.parent()) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn all_eight_tool_kinds_can_be_made_and_edited() {
    let mut s = Session::default();
    let kinds = ["flat_end_mill", "ball_end_mill", "bull_nose_end_mill", "drill", "spot_drill", "reamer", "shell_mill", "face_mill"];
    for (i, kind) in kinds.iter().enumerate() {
        let id = i as u64 + 1;
        let r = run(&mut s, "manufacture.tool.set", json!({"id": id, "kind": kind, "diameter": 6.0 + i as f64, "flutes": 3}));
        assert_eq!(r["tool"]["kind"], *kind);
        assert_eq!(r["created"], true);
    }
    // Editing one changes it and leaves the rest.
    let r = run(&mut s, "manufacture.tool.set", json!({"id": 5, "diameter": 12.0, "tip_angle": 90.0, "name": "Spot 12"}));
    assert_eq!(r["created"], false);
    assert_eq!(r["tool"]["kind"], "spot_drill");
    let job = run(&mut s, "manufacture.job", json!({}));
    assert_eq!(job["tools"].as_array().map(Vec::len), Some(8));
    assert!(s.execute("manufacture.tool.set", &json!({"kind": "laser", "diameter": 3.0})).is_err());
}

#[test]
fn tools_are_saved_to_the_library_and_added_to_another_design() {
    let path = library_path("round-trip");
    let mut a = Session::default();
    run(
        &mut a,
        "manufacture.tool.set",
        json!({"id": 3, "name": "10 mm bull nose", "kind": "bull_nose_end_mill", "diameter": 10.0, "corner_radius": 1.5, "flutes": 4}),
    );
    run(&mut a, "manufacture.tool.set", json!({"id": 4, "name": "Spot", "kind": "spot_drill", "diameter": 8.0, "tip_angle": 90.0}));

    // No file yet: an empty library.
    assert_eq!(run(&mut a, "manufacture.library.list", json!({"path": path}))["tools"].as_array().map(Vec::len), Some(0));
    let saved = run(&mut a, "manufacture.library.save", json!({"id": 3, "path": path}));
    assert_eq!(saved["saved"], "10 mm bull nose");
    run(&mut a, "manufacture.library.save", json!({"id": 4, "name": "Spot drill 8", "path": path}));
    // Saving under a name again replaces it.
    run(&mut a, "manufacture.tool.set", json!({"id": 3, "diameter": 12.0}));
    let again = run(&mut a, "manufacture.library.save", json!({"id": 3, "path": path}));
    assert_eq!(again["count"], 2);

    // A different design adds them.
    let mut b = Session::default();
    run(&mut b, "manufacture.tool.set", json!({"id": 3, "diameter": 5.0}));
    let list = run(&mut b, "manufacture.library.list", json!({"path": path}));
    assert_eq!(list["tools"].as_array().map(Vec::len), Some(2));
    // Tool 3 is taken in this design, so the saved tool takes the next free number.
    let loaded = run(&mut b, "manufacture.library.load", json!({"name": "10 mm bull nose", "path": path}));
    assert_eq!(loaded["created"], true);
    let new_id = loaded["tool"]["id"].as_u64().unwrap();
    assert_ne!(new_id, 3);
    assert_eq!(loaded["tool"]["diameter"], 12.0);
    assert_eq!(loaded["tool"]["corner_radius"], 1.5);
    assert_eq!(loaded["tool"]["kind"], "bull_nose_end_mill");
    // A number can be asked for, which replaces the tool that has it.
    let replaced = run(&mut b, "manufacture.library.load", json!({"name": "Spot drill 8", "id": 3, "path": path}));
    assert_eq!(replaced["created"], false);
    assert_eq!(replaced["tool"]["kind"], "spot_drill");
    // Adding is one undo step.
    run(&mut b, "edit.undo", json!({}));
    let job = run(&mut b, "manufacture.job", json!({}));
    assert_eq!(job["tools"].as_array().unwrap().iter().find(|t| t["id"] == 3).map(|t| t["kind"].clone()), Some(json!("flat_end_mill")));

    // Deleting from the library leaves the designs alone.
    let gone = run(&mut b, "manufacture.library.delete", json!({"name": "Spot drill 8", "path": path}));
    assert_eq!(gone["count"], 1);
    assert!(b.execute("manufacture.library.load", &json!({"name": "Spot drill 8", "path": path})).is_err());
    assert!(b.execute("manufacture.library.delete", &json!({"name": "Spot drill 8", "path": path})).is_err());
    clean(&path);
}

#[test]
fn the_library_does_not_change_the_design_unless_a_tool_is_added() {
    let path = library_path("no-change");
    let mut s = Session::default();
    run(&mut s, "manufacture.tool.set", json!({"id": 1, "diameter": 6.0}));
    let before = s.revision;
    run(&mut s, "manufacture.library.save", json!({"id": 1, "path": path}));
    run(&mut s, "manufacture.library.list", json!({"path": path}));
    run(&mut s, "manufacture.library.delete", json!({"name": "Tool 1", "path": path}));
    assert_eq!(s.revision, before);
    clean(&path);
}

#[test]
fn a_library_file_that_is_wrong_is_refused_not_overwritten() {
    let path = library_path("bad-file");
    let mut s = Session::default();
    run(&mut s, "manufacture.tool.set", json!({"id": 1, "diameter": 6.0}));
    let file = std::path::Path::new(&path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    for text in [
        "not json",
        r#"{"format": "solidcraft-tool-library/9", "tools": []}"#,
        r#"{"format": "solidcraft-tool-library/1", "tools": [], "mystery": 1}"#,
        r#"{"format": "solidcraft-tool-library/1", "tools": [{"id": 1, "name": "x", "diameter": -3.0}]}"#,
    ] {
        std::fs::write(file, text).unwrap();
        for (id, p) in [
            ("manufacture.library.list", json!({"path": path})),
            ("manufacture.library.save", json!({"id": 1, "path": path})),
            ("manufacture.library.load", json!({"name": "x", "path": path})),
        ] {
            assert!(s.execute(id, &p).is_err(), "{id} on {text}");
        }
        assert_eq!(std::fs::read_to_string(file).unwrap(), text, "a library that does not read is left as it was");
    }
    clean(&path);
}

#[test]
fn library_requests_are_checked() {
    let mut s = Session::default();
    run(&mut s, "manufacture.tool.set", json!({"id": 1, "diameter": 6.0}));
    let path = library_path("hostile");
    for (id, p) in [
        ("manufacture.library.list", json!({"path": ""})),
        ("manufacture.library.list", json!({"path": 3})),
        ("manufacture.library.list", json!({"path": "a\nb"})),
        ("manufacture.library.list", json!([])),
        ("manufacture.library.save", json!({"path": path})),
        ("manufacture.library.save", json!({"id": 99, "path": path})),
        ("manufacture.library.save", json!({"id": 0, "path": path})),
        ("manufacture.library.save", json!({"id": 1, "name": "", "path": path})),
        ("manufacture.library.save", json!({"id": 1, "name": "x".repeat(500), "path": path})),
        ("manufacture.library.load", json!({"path": path})),
        ("manufacture.library.load", json!({"name": "nothing here", "path": path})),
        ("manufacture.library.delete", json!({"name": 5, "path": path})),
    ] {
        assert!(s.execute(id, &p).is_err(), "{id} {p}");
    }
    clean(&path);
}

#[test]
fn entry_moves_are_saved_with_the_operation_and_change_the_toolpath() {
    let mut s = Session::default();
    run(&mut s, "sketch.create", json!({"plane": "XY", "name": "Outline"}));
    run(&mut s, "sketch.rectangle.two_point", json!({"p0": [0.0, 0.0], "p1": [40.0, 30.0]}));
    run(&mut s, "sketch.finish", json!({}));
    run(&mut s, "manufacture.tool.set", json!({"id": 1, "diameter": 6.0}));
    run(&mut s, "manufacture.setup.set", json!({"id": 1, "plane": "XY", "stock": {"min": [-5.0, -5.0, -3.0], "max": [45.0, 35.0, 0.0]}}));
    let op = |entry: Value| json!({"id": 1, "setup": 1, "tool": 1, "kind": entry, "top": 0.0, "bottom": -3.0, "feed": 600.0, "spindle_rpm": 8000.0, "stepdown": 3.0});
    let pocket = |extra: Value| {
        let mut k = json!({"type": "pocket", "source": {"sketch": "Outline"}, "stepover": 0.5});
        for (key, v) in extra.as_object().unwrap() {
            k[key] = v.clone();
        }
        k
    };
    let count_slanted = |s: &mut Session| -> (usize, Vec<String>) {
        let r = run(s, "manufacture.toolpath", json!({"operation": 1, "moves": true, "limit": 200_000}));
        let mut at: Option<(f64, f64, f64)> = None;
        let mut n = 0;
        for m in r["toolpath"].as_array().unwrap() {
            let to = (m["to"][0].as_f64().unwrap(), m["to"][1].as_f64().unwrap(), m["to"][2].as_f64().unwrap());
            if m["move"] == "feed"
                && let Some(a) = at
                && a.2 - to.2 > 1e-9
                && ((a.0 - to.0).powi(2) + (a.1 - to.1).powi(2)).sqrt() > 1e-9
            {
                n += 1;
            }
            at = Some(to);
        }
        (n, r["warnings"].as_array().unwrap().iter().filter_map(|w| w.as_str().map(str::to_owned)).collect())
    };
    run(&mut s, "manufacture.operation.set", op(pocket(json!({"entry": "plunge"}))));
    assert_eq!(count_slanted(&mut s).0, 0, "a plunge goes straight down");
    run(&mut s, "manufacture.operation.set", op(pocket(json!({"entry": "ramp", "ramp_angle": 4.0}))));
    let (ramp, _) = count_slanted(&mut s);
    assert!(ramp > 0);
    run(&mut s, "manufacture.operation.set", op(pocket(json!({"entry": "helical", "helix_angle": 5.0, "helix_diameter": 4.0}))));
    let (helix, warnings) = count_slanted(&mut s);
    assert!(helix > ramp, "a helix is many short moves: {helix} against {ramp}");
    assert!(warnings.is_empty(), "{warnings:?}");
    let saved = run(&mut s, "manufacture.job", json!({}));
    assert_eq!(saved["operations"][0]["kind"]["entry"], "helical");
    assert_eq!(saved["operations"][0]["kind"]["helix_angle"], 5.0);
    // Designs from before `entry` existed ramp by their angle, and still read and save.
    run(&mut s, "manufacture.operation.set", op(pocket(json!({"ramp_angle": 3.0}))));
    assert!(count_slanted(&mut s).0 > 0);
    assert!(run(&mut s, "manufacture.job", json!({}))["operations"][0]["kind"].get("entry").is_none());
    // Bad entry settings are refused.
    for extra in [
        json!({"entry": "helical", "helix_angle": 0.0}),
        json!({"entry": "ramp", "ramp_angle": 0.0}),
        json!({"entry": "spiral"}),
        json!({"helix_diameter": -1.0}),
    ] {
        assert!(s.execute("manufacture.operation.set", &op(pocket(extra.clone()))).is_err(), "{extra}");
    }
}

#[test]
fn a_new_tool_starts_with_what_its_kind_usually_has() {
    let mut s = Session::default();
    for (kind, flutes, tip) in [("spot_drill", 2, 90.0), ("reamer", 6, 118.0), ("face_mill", 5, 118.0), ("shell_mill", 4, 118.0)] {
        let out = run(&mut s, "manufacture.tool.set", json!({"kind": kind, "diameter": 10.0}));
        assert_eq!(out["tool"]["flutes"], flutes, "{kind}");
        assert_eq!(out["tool"]["tip_angle"], tip, "{kind}");
    }
    // What the caller gives wins.
    let out = run(&mut s, "manufacture.tool.set", json!({"kind": "reamer", "diameter": 10.0, "flutes": 8}));
    assert_eq!(out["tool"]["flutes"], 8);
}
