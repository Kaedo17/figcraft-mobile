//! A finger on the canvas, driven through egui headless the way Android reports it: a drag on the
//! canvas background pans the view (a touch screen has no scroll wheel and the Move tool's drag
//! there selected nothing), while a press on a layer stays the tool's, a tap still clicks and a
//! still hold is egui's long-press menu. A mouse press is never arbitrated, so the desktop keeps
//! every drag it has.

use egui::{Event, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, vec2};
use figcraft_engine::Session;
use serde_json::json;

use crate::layers_drag_tests::Ui;

fn touch(phase: TouchPhase, p: Pos2) -> Event {
    Event::Touch { device_id: TouchDeviceId(1), id: TouchId(1), phase, pos: p, force: Some(0.5) }
}

fn button(p: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

/// A finger goes down at `p` (the touch plus the pointer events egui-winit sends with it).
fn touch_down(u: &mut Ui, p: Pos2) {
    u.frame(vec![touch(TouchPhase::Start, p), Event::PointerMoved(p), button(p, true)]);
}

/// The finger moves to `p` while held.
fn touch_move(u: &mut Ui, p: Pos2) {
    u.frame(vec![touch(TouchPhase::Move, p), Event::PointerMoved(p)]);
}

/// The finger lifts at `p`.
fn touch_up(u: &mut Ui, p: Pos2) {
    u.frame(vec![touch(TouchPhase::End, p), button(p, false), Event::PointerGone]);
}

/// A point on the canvas background: far from `around`, inside the canvas.
fn background(u: &Ui, around: Pos2) -> Pos2 {
    let rect = u.app.canvas.rect;
    let mut p = Pos2::new(rect.center().x, rect.center().y);
    if p.distance(around) < 150.0 {
        p += vec2(0.0, 200.0);
    }
    assert!(rect.contains(p), "the point is on the canvas: {p:?} in {rect:?}");
    p
}

/// A document with one rectangle, selected, drawn on the canvas.
fn selected_rect() -> (Ui, Pos2) {
    let mut s = Session::new();
    let r = s.execute("shape.rectangle", &json!({"x": 100, "y": 100, "width": 200, "height": 120})).unwrap();
    let r = figcraft_engine::params::id_of(&r["id"]).unwrap();
    s.execute("select.set", &json!({"ids": [r]})).unwrap();
    let mut u = Ui::new(s);
    u.frame(vec![]);
    let c = u.app.session.doc().unwrap().world_bounds(r).unwrap().center();
    let at = u.app.to_screen(c.x, c.y);
    (u, at)
}

#[test]
fn a_finger_drag_on_the_canvas_background_pans_without_touching_the_selection() {
    let (mut u, on_rect) = selected_rect();
    let start = background(&u, on_rect);
    let cx = u.app.session.ds().unwrap().view.cx;
    let sel = u.app.session.selection();
    touch_down(&mut u, start);
    for k in 1..=6 {
        touch_move(&mut u, start + vec2(25.0 * k as f32, 0.0));
    }
    let zoom = u.app.session.ds().unwrap().view.zoom;
    let moved = u.app.session.ds().unwrap().view.cx;
    assert!((moved - cx + 150.0 / zoom).abs() < 1.0, "the view panned under the finger: {cx} -> {moved}");
    assert_eq!(u.app.session.selection(), sel, "panning never clears or changes the selection");
    assert!(u.app.session.tool.overlay.marquee.is_none(), "no marquee is drawn");
    touch_up(&mut u, start + vec2(150.0, 0.0));
    assert_eq!(u.app.session.selection(), sel, "the lift ends the pan, nothing else");
}

#[test]
fn a_finger_drag_on_a_layer_still_moves_it_and_never_pans() {
    let (mut u, on_rect) = selected_rect();
    let cx = u.app.session.ds().unwrap().view.cx;
    let before = figcraft_geom::translation(u.app.session.doc().unwrap().world_transform(u.app.session.selection()[0]));
    touch_down(&mut u, on_rect);
    for k in 1..=6 {
        touch_move(&mut u, on_rect + vec2(10.0 * k as f32, 0.0));
    }
    touch_up(&mut u, on_rect + vec2(60.0, 0.0));
    let after = figcraft_geom::translation(u.app.session.doc().unwrap().world_transform(u.app.session.selection()[0]));
    assert!((after.x - before.x) > 30.0, "the layer moved with the finger: {:?} -> {:?}", before, after);
    assert_eq!(u.app.session.ds().unwrap().view.cx, cx, "the view never panned");
}

#[test]
fn a_finger_tap_on_the_canvas_background_still_deselects() {
    let (mut u, on_rect) = selected_rect();
    let p = background(&u, on_rect);
    touch_down(&mut u, p);
    touch_up(&mut u, p);
    u.frame(vec![]);
    assert!(u.app.session.selection().is_empty(), "a tap on empty canvas deselects");
}

#[test]
fn a_still_finger_hold_opens_the_menu_and_keeps_the_selection() {
    let (mut u, on_rect) = selected_rect();
    let p = background(&u, on_rect);
    let sel = u.app.session.selection();
    touch_down(&mut u, p);
    // Past egui's long-press window (0.8 s at the harness's 60 fps).
    for _ in 0..60 {
        u.frame(vec![]);
    }
    assert!(u.app.ui.context_menu.is_some(), "a still hold opens the canvas menu");
    assert_eq!(u.app.session.selection(), sel, "the hold never clears the selection first");
    touch_up(&mut u, p);
}

#[test]
fn a_mouse_drag_on_the_canvas_background_still_marquees() {
    let (mut u, on_rect) = selected_rect();
    u.app.session.execute("select.none", &json!({})).unwrap();
    let start = background(&u, on_rect);
    let cx = u.app.session.ds().unwrap().view.cx;
    u.frame(vec![Event::PointerMoved(start)]);
    u.frame(vec![button(start, true)]);
    for k in 1..=8 {
        u.frame(vec![Event::PointerMoved(start + vec2(20.0 * k as f32, 0.0))]);
    }
    // Across the rectangle, so the marquee reaches it.
    let over = on_rect;
    u.frame(vec![Event::PointerMoved(over)]);
    u.frame(vec![button(over, false)]);
    assert!(!u.app.session.selection().is_empty(), "the mouse marquee still selects");
    assert_eq!(u.app.session.ds().unwrap().view.cx, cx, "a mouse drag never pans");
}
