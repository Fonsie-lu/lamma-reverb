//! Opens the editor in a standalone window without a plugin host. Parameter changes go into a
//! local store so every control can be operated and checked. There is no audio output.
//! Usage: cargo run --release --example gui_preview
//! With LEMMA_DEMO=1 a headless engine processes a plucked chord progression (D minor) in a
//! background thread at real-time pace, with matching MIDI notes for the key detector, and feeds
//! the meters, scope and key display. It uses the default settings, not the preview store.
//! LEMMA_THEME=0..3 starts in a given colour theme.
use baseview::{Size, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use nih_plug::prelude::*;
use nih_plug::wrapper::state::PluginState;
use lemma::editor::{self, EditorState, PreviewStore};
use lemma::engine::Engine;
use lemma::params::ReverbParams;
use lemma::settings_from_params;
use lemma::visuals::{ScopeInput, VisualState};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct PreviewContext {
    store: PreviewStore,
}

impl GuiContext for PreviewContext {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }
    fn request_resize(&self) -> bool {
        false
    }
    unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {}
    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        self.store.lock().unwrap().insert(param.name().to_owned(), normalized);
    }
    unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {}
    fn get_state(&self) -> PluginState {
        unimplemented!()
    }
    fn set_state(&self, _state: PluginState) {}
}

fn main() {
    let params = ReverbParams::default();
    if let Some(i) = std::env::var("LEMMA_THEME").ok().and_then(|v| v.parse().ok()) {
        params.theme.store(i, std::sync::atomic::Ordering::Relaxed);
    }
    let store = PreviewStore::default();
    let visuals = Arc::new(VisualState::default());
    let (scope_input, scope_output) = ScopeInput::new();
    if std::env::var("LEMMA_DEMO").is_ok() {
        let visuals = visuals.clone();
        std::thread::spawn(move || demo(visuals, scope_input));
    }
    let settings = WindowOpenOptions {
        title: "Lemma Preview".into(),
        size: Size::new(editor::WIDTH as f64, editor::HEIGHT as f64),
        scale: WindowScalePolicy::ScaleFactor(1.0),
        gl_config: Some(Default::default()),
    };
    let context = PreviewContext { store: store.clone() };
    EguiWindow::open_blocking(
        settings,
        GraphicsConfig::default(),
        EditorState::with_visuals(visuals, Arc::new(Mutex::new(scope_output))).with_preview_store(store),
        |ctx, _queue, _state| editor::setup_style(ctx),
        move |ctx, _queue, state| {
            let setter = ParamSetter::new(&context);
            editor::root(ctx, &params, &setter, state);
        },
    );
}

/// Plucked saw chords: Dm - Bb - Gm - A, one per second.
fn demo(visuals: Arc<VisualState>, mut scope: ScopeInput) {
    const SR: f32 = 48_000.0;
    const BLOCK: usize = 480;
    let params = ReverbParams::default();
    let settings = settings_from_params(&params, 120.0);
    let mut engine = Engine::new(SR);
    engine.init(&settings);
    scope.set_sample_rate(SR);
    let chords: [[u8; 3]; 4] = [[50, 53, 57], [46, 50, 53], [43, 46, 50], [45, 49, 52]];
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let mut phases = [0.0f32; 3];
    let start = Instant::now();
    let mut rendered = 0usize;
    let mut current: Option<usize> = None;
    loop {
        let t = rendered as f32 / SR;
        let index = (t as usize) % chords.len();
        if current != Some(index) {
            if let Some(prev) = current {
                chords[prev].iter().for_each(|n| engine.note_off(*n));
            }
            chords[index].iter().for_each(|n| engine.note_on(*n, 0.8));
            current = Some(index);
        }
        for i in 0..BLOCK {
            let age = (rendered + i) as f32 / SR - t.floor();
            let env = (-age * 3.0).exp() * if age < 0.6 { 1.0 } else { 0.0 };
            let mut x = 0.0;
            for (k, n) in chords[index].iter().enumerate() {
                let hz = 440.0 * ((*n as f32 - 69.0) / 12.0).exp2();
                phases[k] = (phases[k] + hz / SR).fract();
                x += 2.0 * phases[k] - 1.0;
            }
            l[i] = x * env * 0.15;
            r[i] = l[i];
        }
        engine.process(&mut l, &mut r, &settings);
        engine.publish(&visuals);
        for (a, b) in l.iter().zip(&r) {
            scope.push(0.5 * (a + b));
        }
        rendered += BLOCK;
        let due = Duration::from_secs_f32(rendered as f32 / SR);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
}
