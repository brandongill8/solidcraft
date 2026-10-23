//! What the Manufacture tab shows in place of the design's history and browser extras: the job's
//! history along the bottom (the setups and the operations, in the order they run) and a panel on
//! the right with the tools that were made, the setups and the operations. Both read the job
//! through `manufacture.job` and act through the same windows and commands as the toolbar.

use egui::{Rect, RichText, UiBuilder, pos2};
use serde_json::{Value, json};

use crate::SolveApp;
use crate::manufacture_job::{self, Item};
use crate::theme::Tokens;
use crate::ui_kit::{Container, Edge, Surface};

/// Width of the panel on the right.
const PANEL_WIDTH: f32 = 260.0;

/// True while the Manufacture tab is the one shown.
pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == "MANUFACTURE"
}

fn job(app: &mut SolveApp) -> Value {
    app.session.execute("manufacture.job", &json!({})).unwrap_or(Value::Null)
}

fn num(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn op_kind(op: &Value) -> &'static str {
    match op["kind"]["type"].as_str() {
        Some("face") => "Face",
        Some("contour") => "Contour",
        Some("pocket") => "Pocket",
        _ => "Drill",
    }
}

fn tool_kind(tool: &Value) -> &'static str {
    match tool["kind"].as_str() {
        Some("ball_end_mill") => "Ball end mill",
        Some("bull_nose_end_mill") => "Bull nose end mill",
        Some("drill") => "Drill",
        Some("spot_drill") => "Spot drill",
        Some("reamer") => "Reamer",
        Some("shell_mill") => "Shell mill",
        Some("face_mill") => "Face mill",
        _ => "Flat end mill",
    }
}

/// The history along the bottom, in place of the design's: setups first, then the operations in
/// the order the program runs them. Clicking one opens it in its window.
pub fn history(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    if app.ui.layout == crate::shell::LayoutMode::Floating {
        let rect = crate::timeline::floating_rect(ui.available_rect_before_wrap());
        if !rect.is_finite() || rect.width() < 1.0 || rect.height() < 30.0 {
            app.shell.history = None;
            return;
        }
        app.shell.history = Some(rect);
        crate::scenario::publish_rect("history:card", rect);
        let mut child = ui.new_child(UiBuilder::new().id_salt("sc_manufacture_history").max_rect(rect));
        ui.painter().add(Container::new(ui.ctx(), Surface::Panel).frame(ui.ctx(), t.timeline, crate::frame::Side::Left).paint(rect));
        child.set_clip_rect(rect);
        history_contents(app, &mut child);
    } else {
        let response = Container::new(ui.ctx(), Surface::Panel)
            .edge("sc_manufacture_history", Edge::Bottom)
            .exact_size(40.0)
            .frame(Container::new(ui.ctx(), Surface::Panel).edge_frame(t.timeline))
            .show(ui, |ui| {
                history_contents(app, ui);
            });
        crate::scenario::publish_rect("history:strip", response.response.rect);
    }
}

fn history_contents(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let r = ui.max_rect();
    let job = job(app);
    let focus = manufacture_job::focused_operation(app);
    let mut pick: Option<(Item, u32)> = None;
    let label =
        ui.painter().layout_no_wrap(crate::i18n::tr_label("Manufacture history").to_string(), egui::FontId::proportional(t.font_label), t.text);
    let x = r.left() + t.space_10;
    ui.painter().galley(pos2(x, r.center().y - label.size().y / 2.0), label.clone(), t.text);
    let strip = Rect::from_min_max(pos2(x + label.size().x + t.space_12, r.top()), r.right_bottom());
    ui.scope_builder(UiBuilder::new().max_rect(strip), |ui| {
        ui.style_mut().always_scroll_the_only_direction = true;
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                for setup in job["setups"].as_array().into_iter().flatten() {
                    let id = num(setup, "id");
                    let name = text(setup, "name");
                    let resp = crate::ui_kit::button(ui, format!("Setup {id}"), crate::ui_kit::Button::Small);
                    crate::scenario::publish_handle(&format!("mfg_history:setup:{id}"), resp.rect.center());
                    if resp.on_hover_text(name).clicked() {
                        pick = Some((Item::Setup, id));
                    }
                }
                if job["operations"].as_array().is_some_and(|a| !a.is_empty()) && job["setups"].as_array().is_some_and(|a| !a.is_empty()) {
                    ui.separator();
                }
                for (i, op) in job["operations"].as_array().into_iter().flatten().enumerate() {
                    let id = num(op, "id");
                    let name = text(op, "name");
                    let mark = if focus == Some(id) { "\u{25B6} " } else { "" };
                    let label = format!("{mark}{}  {}", i + 1, crate::i18n::tr_label(op_kind(op)));
                    let resp = crate::ui_kit::button(ui, label, crate::ui_kit::Button::Small);
                    crate::scenario::publish_handle(&format!("mfg_history:operation:{id}"), resp.rect.center());
                    let tip = format!("{name}\nT{}, setup {}", num(op, "tool"), num(op, "setup"));
                    if resp.on_hover_text(tip).clicked() {
                        pick = Some((Item::Operation, id));
                    }
                }
                if job["operations"].as_array().is_none_or(Vec::is_empty) && job["setups"].as_array().is_none_or(Vec::is_empty) {
                    ui.label(RichText::new("Nothing yet: make a setup, a tool and an operation").color(t.text_dim));
                }
            });
        });
    });
    if let Some((item, id)) = pick {
        manufacture_job::pick(app, item, id);
    }
}

/// The panel on the right: the tools that were built, the setups and the operations in order.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let floating = app.ui.layout == crate::shell::LayoutMode::Floating;
    let job = job(app);
    let focus = manufacture_job::focused_operation(app);
    let mut pick: Option<(Item, u32)> = None;
    let mut remove: Option<(Item, u32)> = None;
    let _ = floating;
    let response = Container::new(ui.ctx(), Surface::Panel)
        .edge("sc_manufacture_panel", Edge::Right)
        .exact_size(PANEL_WIDTH)
        .resizable(false)
        .frame(Container::new(ui.ctx(), Surface::Panel).edge_frame(t.panel).inner_margin(6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                section(ui, "Tools", "New tool", Item::Tool, &mut pick, &mut remove, |ui, pick, remove| {
                    for tool in job["tools"].as_array().into_iter().flatten() {
                        let id = num(tool, "id");
                        let diameter = tool.get("diameter").and_then(Value::as_f64).unwrap_or(0.0);
                        let label = format!("T{id}  {}  \u{2300}{diameter} mm", text(tool, "name"));
                        row(ui, &label, crate::i18n::tr_label(tool_kind(tool)), false, (Item::Tool, id), pick, remove);
                    }
                    if job["tools"].as_array().is_none_or(Vec::is_empty) {
                        ui.label(RichText::new("No tools yet").color(t.text_dim));
                    }
                });
                section(ui, "Setups", "New setup", Item::Setup, &mut pick, &mut remove, |ui, pick, remove| {
                    for setup in job["setups"].as_array().into_iter().flatten() {
                        let id = num(setup, "id");
                        row(ui, &format!("{id}  {}", text(setup, "name")), "", false, (Item::Setup, id), pick, remove);
                    }
                    if job["setups"].as_array().is_none_or(Vec::is_empty) {
                        ui.label(RichText::new("No setups yet").color(t.text_dim));
                    }
                });
                section(ui, "Operations", "New operation", Item::Operation, &mut pick, &mut remove, |ui, pick, remove| {
                    for (i, op) in job["operations"].as_array().into_iter().flatten().enumerate() {
                        let id = num(op, "id");
                        let label = format!("{}  {}  {}", i + 1, text(op, "name"), crate::i18n::tr_label(op_kind(op)));
                        let hint = format!("T{}, setup {}", num(op, "tool"), num(op, "setup"));
                        row(ui, &label, &hint, focus == Some(id), (Item::Operation, id), pick, remove);
                    }
                    if job["operations"].as_array().is_none_or(Vec::is_empty) {
                        ui.label(RichText::new("No operations yet").color(t.text_dim));
                    }
                });
            });
        });
    crate::scenario::publish_rect("manufacture:panel", response.response.rect);
    if let Some((item, id)) = remove {
        let (command, key) = match item {
            Item::Tool => ("manufacture.tool.delete", "id"),
            Item::Setup => ("manufacture.setup.delete", "id"),
            Item::Operation => ("manufacture.operation.delete", "id"),
        };
        if let Err(e) = app.run(command, json!({ key: id })) {
            app.set_status(e, true);
        }
    }
    if let Some((item, id)) = pick {
        manufacture_job::pick(app, item, id);
    }
}

fn section(
    ui: &mut egui::Ui,
    title: &str,
    new_label: &str,
    item: Item,
    pick: &mut Option<(Item, u32)>,
    remove: &mut Option<(Item, u32)>,
    body: impl FnOnce(&mut egui::Ui, &mut Option<(Item, u32)>, &mut Option<(Item, u32)>),
) {
    let t = Tokens::get();
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr_label(title)).strong().size(t.font_button));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let resp = crate::ui_kit::button(ui, new_label, crate::ui_kit::Button::Small);
            crate::scenario::publish_handle(&format!("manufacture:new:{item:?}"), resp.rect.center());
            if resp.clicked() {
                *pick = Some((item, 0));
            }
        });
    });
    body(ui, pick, remove);
    ui.add_space(t.space_6);
}

fn row(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    selected: bool,
    what: (Item, u32),
    pick: &mut Option<(Item, u32)>,
    remove: &mut Option<(Item, u32)>,
) {
    let t = Tokens::get();
    let del_width = t.control_height + t.space_6;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - del_width - ui.spacing().item_spacing.x).max(40.0);
        let resp = ui
            .allocate_ui(egui::vec2(width, t.control_height), |ui| {
                crate::ui_kit::list_picker(ui, &format!("manufacture:{:?}:{}", what.0, what.1), label, selected)
            })
            .inner;
        crate::scenario::publish_handle(&format!("manufacture:{:?}:{}", what.0, what.1), resp.rect.center());
        let clicked = if hint.is_empty() { resp.clicked() } else { resp.on_hover_text(hint).clicked() };
        if clicked {
            *pick = Some(what);
        }
        if crate::ui_kit::button(ui, "\u{00D7}", crate::ui_kit::Button::Small).on_hover_text("Delete").clicked() {
            *remove = Some(what);
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::scenario::Harness;

    fn job(h: &mut Harness) {
        let run = |h: &mut Harness, id: &str, p: serde_json::Value| h.app.run(id, p).unwrap_or_else(|e| panic!("{id}: {e}"));
        run(h, "sketch.create", json!({"plane": "XY", "name": "Pocket"}));
        run(h, "sketch.rectangle.two_point", json!({"p0": [10.0, 8.0], "p1": [30.0, 22.0]}));
        run(h, "sketch.finish", json!({}));
        run(h, "manufacture.tool.set", json!({"id": 1, "name": "6 mm flat", "diameter": 6.0}));
        run(
            h,
            "manufacture.setup.set",
            json!({"id": 1, "plane": "XY", "origin": [0.0, 0.0, 10.0], "stock": {"min": [-5.0, -5.0, -10.0], "max": [45.0, 35.0, 3.0]}}),
        );
        for name in ["First pocket", "Second pocket"] {
            let op = json!({"name": name, "setup": 1, "tool": 1, "kind": {"type": "pocket", "source": {"sketch": "Pocket"}}, "top": 0.0, "bottom": -4.0, "feed": 500.0, "spindle_rpm": 8000.0});
            run(h, "manufacture.operation.set", op);
        }
    }

    #[test]
    fn the_workspace_follows_the_manufacture_tab() {
        let mut h = Harness::new();
        let out = h.call("ui.workspace", json!({"workspace": "manufacture"}));
        assert_eq!(out["result"]["workspace"], "manufacture", "{out}");
        assert_eq!(h.app.ui.tab, "MANUFACTURE");
        let out = h.call("ui.workspace", json!({"workspace": "design"}));
        assert_eq!(out["result"]["workspace"], "design", "{out}");
        assert_ne!(h.app.ui.tab, "MANUFACTURE");
    }

    #[test]
    fn the_manufacture_tab_has_its_own_history_and_a_panel_for_what_was_built() {
        let mut h = Harness::new();
        h.call("ui.resize", json!({"width": 1600, "height": 900}));
        job(&mut h);
        h.frames(2);
        // The design's history is there until the Manufacture tab opens.
        assert!(h.app.ui.show_timeline);
        h.call("ui.workspace", json!({"workspace": "manufacture"}));
        h.frames(4);
        // Setups and operations in order along the bottom; tools, setups and operations on the right.
        assert!(h.widget("Setup 1").is_some());
        assert!(h.widget("1  Pocket").is_some() && h.widget("2  Pocket").is_some(), "the operations are numbered in order");
        assert!(h.widget("T1  6 mm flat  \u{2300}6 mm").is_some(), "the tool built is listed");
        assert!(h.widget("1  First pocket  Pocket").is_some() && h.widget("2  Second pocket  Pocket").is_some());
        // Clicking an operation of the history opens it in its window.
        h.step(&json!({"widget": "2  Pocket"})).unwrap();
        h.frames(3);
        assert_eq!(super::manufacture_job::focused_operation(&h.app), Some(2));
        // Clicking a tool opens the Tools window.
        h.step(&json!({"widget": "T1  6 mm flat  \u{2300}6 mm"})).unwrap();
        h.frames(3);
        assert!(h.widget("Save tool").is_some(), "the tool is loaded for editing");
        // Deleting an operation from the panel removes it from both lists.
        let n = h.app.session.doc.manufacture.operations.len();
        assert_eq!(n, 2);
        h.app.run("manufacture.operation.delete", json!({"id": 2})).unwrap();
        h.frames(3);
        assert!(h.widget("2  Pocket").is_none());
    }
}
