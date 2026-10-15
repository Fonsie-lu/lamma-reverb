//! Headless GUI tests: the editor runs in a real egui context with simulated mouse input against
//! a mock host that applies parameter changes with a delay, like nih-plug's CLAP wrapper does.

use super::*;
use nih_plug::wrapper::state::PluginState;
use nih_plug_egui::egui::{Event, Modifiers, PointerButton, RawInput, Vec2};

const SCREEN: Vec2 = Vec2::new(BW, BH + 20.0);

#[derive(Default)]
struct MockHost {
    /// Values the "plugin" has applied (what the GUI reads back).
    store: PreviewStore,
    /// Values sent by the GUI but not applied yet.
    queue: Mutex<Vec<(String, f32)>>,
    open_gestures: Mutex<HashMap<String, i32>>,
    errors: Mutex<Vec<String>>,
}

impl MockHost {
    fn flush(&self) {
        let mut store = self.store.lock().unwrap();
        for (name, v) in self.queue.lock().unwrap().drain(..) {
            store.insert(name, v);
        }
    }
}

impl GuiContext for MockHost {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }
    fn request_resize(&self) -> bool {
        false
    }
    unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
        let name = param.name().to_owned();
        let mut g = self.open_gestures.lock().unwrap();
        let n = g.entry(name.clone()).or_default();
        if *n != 0 {
            self.errors.lock().unwrap().push(format!("nested gesture: {name}"));
        }
        *n += 1;
    }
    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        let name = param.name().to_owned();
        if self.open_gestures.lock().unwrap().get(&name).copied().unwrap_or(0) != 1 {
            self.errors.lock().unwrap().push(format!("set outside gesture: {name}"));
        }
        self.queue.lock().unwrap().push((name, normalized));
    }
    unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
        let name = param.name().to_owned();
        let mut g = self.open_gestures.lock().unwrap();
        let n = g.entry(name.clone()).or_default();
        *n -= 1;
        if *n != 0 {
            self.errors.lock().unwrap().push(format!("unbalanced end: {name}"));
        }
    }
    fn get_state(&self) -> PluginState {
        unimplemented!()
    }
    fn set_state(&self, _state: PluginState) {}
}

struct Harness {
    ctx: egui::Context,
    params: ReverbParams,
    host: MockHost,
    state: EditorState,
    time: f64,
    frame: u64,
    /// Host applies queued values every `latency` frames.
    latency: u64,
    events: Vec<Event>,
    pointer: Pos2,
}

impl Harness {
    fn new(latency: u64) -> Self {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let host = MockHost::default();
        let state = EditorState::default().with_preview_store(host.store.clone());
        let mut h = Self {
            ctx,
            params: ReverbParams::default(),
            host,
            state,
            time: 0.0,
            frame: 0,
            latency,
            events: Vec::new(),
            pointer: pos2(0.0, 0.0),
        };
        h.run(2);
        h
    }

    fn run(&mut self, frames: usize) {
        for _ in 0..frames {
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                time: Some(self.time),
                events: std::mem::take(&mut self.events),
                ..Default::default()
            };
            let setter = ParamSetter::new(&self.host);
            let (params, state) = (&self.params, &mut self.state);
            let _ = self.ctx.run(raw, |ctx| root(ctx, params, &setter, state));
            self.time += 1.0 / 60.0;
            self.frame += 1;
            if self.latency > 0 && self.frame.is_multiple_of(self.latency) {
                self.host.flush();
            }
        }
    }

    fn settle(&mut self) {
        self.run(3);
        self.host.flush();
        self.run(2);
    }

    fn move_to(&mut self, p: Pos2) {
        self.pointer = p;
        self.events.push(Event::PointerMoved(p));
    }

    fn button(&mut self, pressed: bool) {
        self.events.push(Event::PointerButton {
            pos: self.pointer,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        });
    }

    fn click(&mut self, p: Pos2) {
        self.move_to(p);
        self.run(1);
        self.button(true);
        self.run(1);
        self.button(false);
        self.run(1);
        // Make sure the next click is not seen as a double click.
        self.time += 0.4;
    }

    fn drag(&mut self, from: Pos2, delta: Vec2, steps: usize) {
        self.move_to(from);
        self.run(1);
        self.button(true);
        self.run(1);
        for k in 1..=steps {
            self.move_to(from + delta * (k as f32 / steps as f32));
            self.run(1);
        }
        self.button(false);
        self.run(1);
        self.move_to(pos2(2.0, 2.0));
        self.run(1);
        self.time += 0.4;
    }

    fn scroll(&mut self, at: Pos2, amount: f32) {
        self.move_to(at);
        self.run(1);
        self.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, amount),
            modifiers: Modifiers::default(),
        });
        self.run(1);
        self.move_to(pos2(2.0, 2.0));
        self.run(1);
    }

    fn control(&self, name: &str) -> ControlInfo {
        self.state
            .controls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("control {name} not found"))
            .clone()
    }

    /// Applied normalized value of a parameter.
    fn value(&self, name: &str) -> f32 {
        if let Some(v) = self.host.store.lock().unwrap().get(name) {
            return *v;
        }
        for (_, ptr, _) in self.params.param_map() {
            unsafe {
                if ptr.name() == name {
                    return ptr.default_normalized_value();
                }
            }
        }
        panic!("param {name} not found")
    }

    fn step_count(&self, name: &str) -> Option<usize> {
        for (_, ptr, _) in self.params.param_map() {
            unsafe {
                if ptr.name() == name {
                    return ptr.step_count();
                }
            }
        }
        panic!("param {name} not found")
    }

    fn assert_clean(&self) {
        let errors = self.host.errors.lock().unwrap();
        assert!(errors.is_empty(), "host protocol errors: {errors:?}");
        let open: Vec<_> = self
            .host
            .open_gestures
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, n)| **n != 0)
            .map(|(k, _)| k.clone())
            .collect();
        assert!(open.is_empty(), "gestures left open: {open:?}");
    }
}

fn canvas_scale(c: &ControlInfo, canvas_width: f32) -> f32 {
    c.rect.width() / canvas_width
}


fn controls_of(h: &Harness, kind: ControlKind) -> Vec<ControlInfo> {
    h.state.controls.iter().filter(|c| c.kind == kind).cloned().collect()
}

#[test]
fn every_control_is_registered() {
    let h = Harness::new(3);
    let count = |kind| h.state.controls.iter().filter(|c| c.kind == kind).count();
    assert_eq!(count(ControlKind::Fader), 20);
    assert_eq!(count(ControlKind::Stepper), 4);
    assert_eq!(count(ControlKind::Selector), 2);
    assert_eq!(count(ControlKind::Handle), 4);
    assert_eq!(count(ControlKind::Keyboard), 1);
    // 6 section / sync switches, 3 preset buttons, 4 theme chips.
    assert_eq!(count(ControlKind::Button), 13);
}

/// No two controls overlap, so every one of them can be reached with the mouse.
#[test]
fn controls_do_not_overlap() {
    let h = Harness::new(2);
    let c = &h.state.controls;
    for i in 0..c.len() {
        for j in i + 1..c.len() {
            let overlap = c[i].rect.intersect(c[j].rect);
            // Graph handles sit on top of each other by design only when values coincide.
            if c[i].kind == ControlKind::Handle && c[j].kind == ControlKind::Handle {
                continue;
            }
            assert!(
                overlap.width() <= 0.5 || overlap.height() <= 0.5,
                "{} and {} overlap",
                c[i].name,
                c[j].name
            );
        }
    }
}

/// Every fader follows a drag by the full distance, even though the host applies values late.
#[test]
fn faders_follow_drag_with_host_latency() {
    let mut h = Harness::new(4);
    for c in controls_of(&h, ControlKind::Fader) {
        let s = canvas_scale(&c, 28.0);
        let travel = 110.0 * s;
        let before = h.value(&c.name);
        // Move away from the nearer end so the change is not clipped.
        let up = before < 0.5;
        let px = 0.4 * travel;
        let delta = vec2(0.0, if up { -px } else { px });
        h.drag(c.rect.center(), delta, 12);
        h.settle();
        let after = h.value(&c.name);
        let change = after - before;
        match h.step_count(&c.name) {
            None => {
                let expected = if up { 0.4 } else { -0.4 };
                assert!(
                    (change - expected).abs() < 0.08,
                    "{}: moved {change:.3}, expected {expected:.3}",
                    c.name
                );
            }
            Some(_) => assert!(
                if up { change > 0.2 } else { change < -0.2 },
                "{}: stepped fader moved {change:.3}",
                c.name
            ),
        }
    }
    h.assert_clean();
}

/// Without any host updates during the drag, the fader still ends at the dragged position.
#[test]
fn fader_does_not_depend_on_read_back() {
    let mut h = Harness::new(0);
    let c = h.control("Size");
    let s = canvas_scale(&c, 28.0);
    let before = h.value("Size");
    h.drag(c.rect.center(), vec2(0.0, 0.3 * 110.0 * s), 15);
    h.host.flush();
    let after = h.value("Size");
    assert!((after - (before - 0.3)).abs() < 0.06, "{before} -> {after}");
    h.assert_clean();
}

#[test]
fn double_click_resets_and_scroll_changes() {
    let mut h = Harness::new(2);
    let c = h.control("Reverb Mix");
    let default = h.value("Reverb Mix");
    h.scroll(c.rect.center(), 60.0);
    h.settle();
    let scrolled = h.value("Reverb Mix");
    assert!(scrolled > default + 0.02, "scroll: {default} -> {scrolled}");

    h.move_to(c.rect.center());
    h.run(1);
    for _ in 0..2 {
        h.button(true);
        h.run(1);
        h.button(false);
        h.run(1);
    }
    h.settle();
    assert!((h.value("Reverb Mix") - default).abs() < 1e-4);
    h.assert_clean();
}

/// Scrolling an integer fader moves exactly one step per notch.
#[test]
fn stepped_fader_scrolls_one_step() {
    let mut h = Harness::new(2);
    let c = h.control("Harmonic Count");
    let before = h.value("Harmonic Count");
    h.scroll(c.rect.center(), 1.0);
    h.settle();
    let step = 1.0 / h.step_count("Harmonic Count").unwrap() as f32;
    assert!((h.value("Harmonic Count") - before - step).abs() < 1e-4);
    h.assert_clean();
}

/// Clicking the top and bottom detent of every stepped fader selects the end values.
#[test]
fn steppers_click_positions() {
    let mut h = Harness::new(3);
    for c in controls_of(&h, ControlKind::Stepper) {
        let s = canvas_scale(&c, 54.0);
        let two = h.step_count(&c.name) == Some(1);
        let inset = if two { 41.0 } else { 16.0 };
        let top = pos2(c.rect.center().x, c.rect.top() + inset * s);
        let bottom = pos2(c.rect.center().x, c.rect.bottom() - inset * s);
        h.click(top);
        h.settle();
        let v = h.value(&c.name);
        assert!((v - 1.0).abs() < 1e-4, "{} top: {v}", c.name);
        h.click(bottom);
        h.settle();
        let v = h.value(&c.name);
        assert!(v.abs() < 1e-4, "{} bottom: {v}", c.name);
    }
    h.assert_clean();
}

#[test]
fn stepper_drag_moves_through_positions() {
    let mut h = Harness::new(5);
    let c = h.control("Root Octave");
    let s = canvas_scale(&c, 54.0);
    let bottom = pos2(c.rect.center().x, c.rect.bottom() - 16.0 * s);
    h.click(bottom);
    h.settle();
    assert_eq!(h.value("Root Octave"), 0.0);
    // Two detents up (110 px travel / 5 steps = 22 px per step).
    h.drag(bottom, vec2(0.0, -44.0 * s), 20);
    h.settle();
    assert!((h.value("Root Octave") - 0.4).abs() < 1e-4, "{}", h.value("Root Octave"));
    h.assert_clean();
}

/// Clicking each icon of the rotary selectors selects that position.
#[test]
fn selectors_click_icons() {
    let mut h = Harness::new(3);
    for c in controls_of(&h, ControlKind::Selector) {
        let s = canvas_scale(&c, 92.0);
        let count = h.step_count(&c.name).unwrap() + 1;
        let angles: &[f32] = match count {
            3 => &[-100.0, 0.0, 100.0],
            _ => &[-125.0, -45.0, 45.0, 125.0],
        };
        let last = (count - 1) as f32;
        for (i, deg) in angles.iter().enumerate().rev() {
            let a = (deg - 90.0).to_radians();
            let p = c.rect.center() + vec2(a.cos(), a.sin()) * 39.0 * s;
            h.click(p);
            h.settle();
            let v = h.value(&c.name);
            assert!((v - i as f32 / last).abs() < 1e-4, "{} icon {i}: {v}", c.name);
        }
        // Clicking the knob itself advances one position.
        h.click(c.rect.center());
        h.settle();
        assert!((h.value(&c.name) - 1.0 / last).abs() < 1e-4, "{} knob click", c.name);
    }
    h.assert_clean();
}

#[test]
fn switches_toggle_their_parameter() {
    let mut h = Harness::new(3);
    for name in ["Amp", "Reverb", "Ping-Pong", "Tempo Sync", "Harmonic EQ", "Filter"] {
        let before = h.value(name);
        let c = h.control(name);
        assert_eq!(c.kind, ControlKind::Button);
        h.click(c.rect.center());
        h.settle();
        assert_eq!(h.value(name), 1.0 - before, "{name}");
        h.click(c.rect.center());
        h.settle();
        assert_eq!(h.value(name), before, "{name}");
    }
    h.assert_clean();
}

/// Rapid toggling before the host applies the first change must still alternate.
#[test]
fn toggle_twice_before_host_applies() {
    let mut h = Harness::new(0);
    let c = h.control("Ping-Pong");
    h.click(c.rect.center());
    h.click(c.rect.center());
    h.host.flush();
    h.run(2);
    assert_eq!(h.value("Ping-Pong"), 0.0);
    h.assert_clean();
}

/// The TIME fader edits the note value while synced and milliseconds otherwise.
#[test]
fn time_fader_follows_sync() {
    let mut h = Harness::new(2);
    assert!(h.state.controls.iter().any(|c| c.name == "Delay Division"));
    let sync = h.control("Tempo Sync");
    h.click(sync.rect.center());
    h.settle();
    assert!(h.state.controls.iter().any(|c| c.name == "Delay Time"));
    assert!(!h.state.controls.iter().any(|c| c.name == "Delay Division"));
    h.assert_clean();
}

#[test]
fn preset_buttons_load_presets() {
    let mut h = Harness::new(3);
    let next = h.control("next");
    h.click(next.rect.center()); // -> Init (01)
    h.click(next.rect.center()); // -> Warm Room (02)
    h.settle();
    assert_eq!(h.state.preset, Some(1));
    assert_eq!(h.value("Harmonic EQ"), 0.0);

    let prev = h.control("prev");
    h.click(prev.rect.center());
    h.settle();
    assert_eq!(h.state.preset, Some(0));
    assert_eq!(h.value("Harmonic EQ"), 1.0);
    h.assert_clean();
}

/// The GUI keeps showing a value it sent while the host has not applied it yet.
#[test]
fn pending_value_is_displayed_until_applied() {
    let mut h = Harness::new(0);
    let c = h.control("Size");
    let s = canvas_scale(&c, 28.0);
    h.drag(c.rect.center(), vec2(0.0, -20.0 * s), 6);
    let pending = h.state.pending.get("Size").map(|p| p.value);
    assert!(pending.is_some(), "no pending value recorded");
    assert!(pending.unwrap() > h.value("Size"));
    h.host.flush();
    h.run(2);
    assert!(!h.state.pending.contains_key("Size"), "pending not cleared after apply");
}

/// Every theme chip switches the scheme, and every scheme renders all controls.
#[test]
fn theme_selector_switches_and_all_themes_render() {
    let mut h = Harness::new(2);
    let controls_before = h.state.controls.len();
    for (i, theme) in THEMES.iter().enumerate() {
        let chip = h.control(&format!("theme:{}", theme.name));
        h.click(chip.rect.center());
        h.run(2);
        assert_eq!(h.params.theme.load(Ordering::Relaxed) as usize, i, "{}", theme.name);
        assert_eq!(h.state.texture.as_ref().map(|(t, _)| *t as usize), Some(i));
        assert_eq!(h.state.controls.len(), controls_before);
    }
    h.assert_clean();
}

/// The DECAY handle moves decay sideways and damping up / down; PRE-DELAY moves sideways.
#[test]
fn reverb_handles_drag_decay_damping_and_predelay() {
    let mut h = Harness::new(3);
    let d = h.control("Decay");
    assert_eq!(d.kind, ControlKind::Handle);
    let s = d.rect.width() / 18.0;
    let inner_w = (230.0 - 16.0) * s;
    let dec_zone = 0.78 * inner_w;
    let height = (130.0 - 20.0) * s;
    let (d0, m0) = (h.value("Decay"), h.value("Damping"));
    h.drag(d.rect.center(), vec2(0.2 * dec_zone, -0.2 * height), 10);
    h.settle();
    assert!((h.value("Decay") - d0 - 0.2).abs() < 0.03, "decay {d0} -> {}", h.value("Decay"));
    assert!((h.value("Damping") - m0 - 0.2).abs() < 0.03, "damp {m0} -> {}", h.value("Damping"));

    let pre = h.control("Pre-Delay");
    let p0 = h.value("Pre-Delay");
    h.drag(pre.rect.center(), vec2(0.3 * 0.22 * inner_w, 0.0), 10);
    h.settle();
    assert!((h.value("Pre-Delay") - p0 - 0.3).abs() < 0.03, "pre {p0} -> {}", h.value("Pre-Delay"));
    h.assert_clean();
}

/// Filter handles follow the pointer on the logarithmic axis: one octave of movement doubles
/// the cutoff. Vertical movement sets the resonance.
#[test]
fn filter_handles_follow_log_axis() {
    let mut h = Harness::new(3);
    let low = h.control("Low Cut");
    let s = low.rect.width() / 18.0;
    let width = (206.0 - 12.0) * s;
    let octave = width / (1000.0f32).log2();
    let hz = |h: &Harness, name: &str| {
        for (_, ptr, _) in h.params.param_map() {
            unsafe {
                if ptr.name() == name {
                    return ptr.preview_plain(h.value(name));
                }
            }
        }
        unreachable!()
    };
    let f0 = hz(&h, "Low Cut");
    h.drag(low.rect.center(), vec2(2.0 * octave, 0.0), 10);
    h.settle();
    let f1 = hz(&h, "Low Cut");
    assert!((f1 / f0 - 4.0).abs() < 0.3, "low cut {f0} -> {f1}");

    let high = h.control("High Cut");
    let r0 = h.value("Filter Resonance");
    h.drag(high.rect.center(), vec2(0.0, -0.3 * 0.6 * (130.0 - 16.0) * s), 10);
    h.settle();
    assert!((h.value("Filter Resonance") - r0 - 0.3).abs() < 0.03);
    h.assert_clean();
}

/// Clicking a key on the keyboard sets the manual root.
#[test]
fn keyboard_click_sets_manual_root() {
    let mut h = Harness::new(3);
    let kb = h.control("Manual Root");
    assert_eq!(kb.kind, ControlKind::Keyboard);
    let white_w = kb.rect.width() / 7.0;
    // E: third white key, clicked low so the black keys are not hit.
    let e = pos2(kb.rect.left() + 2.5 * white_w, kb.rect.bottom() - 4.0);
    h.click(e);
    h.settle();
    assert!((h.value("Manual Root") - 4.0 / 11.0).abs() < 1e-4, "{}", h.value("Manual Root"));
    // F#: black key between the 4th and 5th white key, clicked high.
    let fs = pos2(kb.rect.left() + 4.0 * white_w, kb.rect.top() + 4.0);
    h.click(fs);
    h.settle();
    assert!((h.value("Manual Root") - 6.0 / 11.0).abs() < 1e-4, "{}", h.value("Manual Root"));
    h.assert_clean();
}
