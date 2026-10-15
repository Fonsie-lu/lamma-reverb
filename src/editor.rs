//! Hardware style editor in the look of the Alpha Juno panel: coloured section bars, faders with
//! scales, rotary selectors, steppers and an OLED, plus graphs: the amp's transfer curve and
//! harmonics, a draggable reverb decay display, the ping-pong echo pattern, the detected key on a
//! keyboard, the harmonic EQ response and a draggable low / high cut response. The whole panel is
//! drawn on a fixed canvas and scales with the window (drag the bottom right corner).

use crate::amp::{curve, harmonic_spectrum, shape};
use crate::dsp::SvfCoefs;
use crate::filter::{self as cut, CutSettings};
use crate::harmonic::{self, HarmSettings, MAX_HARMONICS, NOTE_NAMES};
use crate::params::{AmpType, HarmMode, HarmSource, ReverbParams};
use crate::pingpong::delay_seconds;
use crate::presets::PRESETS;
use crate::reverb::size_scale;
use crate::theme::{self, Section, Theme, THEMES};
use crate::visuals::{ScopeInput, ScopeOutput, VisualState, SCOPE_COLUMNS};
use nih_plug::prelude::*;
use nih_plug_egui::create_egui_editor;
use nih_plug_egui::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Id, Painter, Pos2, Rect,
    Sense, Shape, Stroke, StrokeKind, TextureHandle, Ui, Vec2,
};
use nih_plug_egui::resizable_window::ResizableWindow;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// Canvas size in logical pixels. Everything is drawn on this canvas and scaled to the window.
const BW: f32 = 1664.0;
const BH: f32 = 500.0;
/// Right edge of the section bars.
const RIGHT: f32 = BW - 16.0;
/// Default window size: the canvas at 110 %.
pub const WIDTH: u32 = (BW * 1.1) as u32;
pub const HEIGHT: u32 = (BH * 1.1) as u32;

// Row geometry
const BAR1_Y: f32 = 50.0;
const LABEL1_Y: f32 = 96.0;
const SLOT1: (f32, f32) = (116.0, 226.0);
const BAR2_Y: f32 = 258.0;
const LABEL2_Y: f32 = 304.0;
const SLOT2: (f32, f32) = (324.0, 434.0);

/// Frequency axis of the EQ and filter graphs.
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
/// Sample rate the graphs are computed for.
const GRAPH_SR: f32 = 48_000.0;

struct Focus {
    name: String,
    value: String,
}

/// How long a value set by the GUI is shown before trusting the parameter again. nih-plug only
/// applies GUI changes asynchronously (CLAP queues them until the next process call or flush), so
/// reading the parameter right after setting it returns the old value.
const PENDING_TIMEOUT: f64 = 1.0;

struct Pending {
    value: f32,
    time: f64,
}

/// Shared parameter store used by the standalone preview, which has no plugin wrapper that could
/// apply parameter changes. Keyed by parameter name, normalized values.
pub type PreviewStore = Arc<Mutex<HashMap<String, f32>>>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlKind {
    Fader,
    Stepper,
    Selector,
    Button,
    /// Graph handle, registered under its horizontal parameter.
    Handle,
    /// Keyboard of the key display, registered under the manual root parameter.
    Keyboard,
}

/// Screen rectangle of an interactive control, recorded every frame (used by tests).
#[derive(Clone, Debug)]
pub struct ControlInfo {
    pub name: String,
    pub kind: ControlKind,
    pub rect: Rect,
}

pub struct EditorState {
    visuals: Arc<VisualState>,
    scope: Arc<Mutex<ScopeOutput>>,
    preset: Option<usize>,
    focus: Option<Focus>,
    /// Panel texture and the theme index it was made for.
    texture: Option<(u8, TextureHandle)>,
    /// Values sent to the host that may not have been applied to the parameters yet.
    pending: HashMap<String, Pending>,
    preview_store: Option<PreviewStore>,
    pub controls: Vec<ControlInfo>,
}

impl Default for EditorState {
    /// Idle visuals and an empty scope, for the preview and the tests.
    fn default() -> Self {
        let (_input, output) = ScopeInput::new();
        Self::with_visuals(Arc::new(VisualState::default()), Arc::new(Mutex::new(output)))
    }
}

impl EditorState {
    pub fn with_visuals(visuals: Arc<VisualState>, scope: Arc<Mutex<ScopeOutput>>) -> Self {
        Self {
            visuals,
            scope,
            preset: None,
            focus: None,
            texture: None,
            pending: HashMap::new(),
            preview_store: None,
            controls: Vec::new(),
        }
    }

    /// Editor state for the standalone preview: parameter values are read from `store`.
    pub fn with_preview_store(mut self, store: PreviewStore) -> Self {
        self.preview_store = Some(store);
        self
    }
}

pub fn create(
    params: Arc<ReverbParams>,
    visuals: Arc<VisualState>,
    scope: Arc<Mutex<ScopeOutput>>,
) -> Option<Box<dyn Editor>> {
    create_egui_editor(
        params.editor_state.clone(),
        EditorState::with_visuals(visuals, scope),
        |ctx, _| setup_style(ctx),
        move |ctx, setter, state| root(ctx, &params, setter, state),
    )
}

pub fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "panel".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSansCondensed-Bold.ttf"
        ))),
    );
    fonts.font_data.insert(
        "oled".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSansMono-Bold.ttf"
        ))),
    );
    fonts
        .families
        .insert(FontFamily::Name("panel".into()), vec!["panel".to_owned()]);
    fonts
        .families
        .insert(FontFamily::Name("oled".into()), vec!["oled".to_owned()]);
    ctx.set_fonts(fonts);
}

pub fn root(ctx: &egui::Context, params: &ReverbParams, setter: &ParamSetter, state: &mut EditorState) {
    // Meters, scope and the key display animate without any input.
    ctx.request_repaint();
    ResizableWindow::new("lemma")
        .min_size(vec2(BW * 0.5, BH * 0.5))
        .show(ctx, params.editor_state.as_ref(), |ui| draw(ui, params, setter, state));
}

/// Maps canvas coordinates to screen coordinates.
#[derive(Clone, Copy)]
struct Cv {
    o: Pos2,
    s: f32,
}

impl Cv {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.o.x + x * self.s, self.o.y + y * self.s)
    }
    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), vec2(w * self.s, h * self.s))
    }
    fn rc(&self, cx: f32, cy: f32, w: f32, h: f32) -> Rect {
        self.r(cx - w * 0.5, cy - h * 0.5, w, h)
    }
    fn font(&self, size: f32) -> FontId {
        FontId::new(size * self.s, FontFamily::Name("panel".into()))
    }
    fn mono(&self, size: f32) -> FontId {
        FontId::new(size * self.s, FontFamily::Name("oled".into()))
    }
    fn stroke(&self, w: f32, c: Color32) -> Stroke {
        Stroke::new(w * self.s, c)
    }
}

/// Scale legends next to a fader.
#[derive(Clone, Copy)]
enum Nums {
    None,
    Left(&'static str, &'static str, &'static str),
    Right(&'static str, &'static str, &'static str),
}

const N10L: Nums = Nums::Left("10", "5", "0");
const N10R: Nums = Nums::Right("10", "5", "0");

/// Icons around a rotary selector.
#[derive(Clone, Copy)]
enum RotIcons {
    /// Transfer curve of each amp type.
    Amp,
    Text(&'static [&'static str]),
}

/// Horizontal mapping of a graph handle.
#[derive(Clone, Copy)]
enum Axis {
    /// One full parameter range per this many screen pixels.
    Linear(f32),
    /// Logarithmic frequency axis `width` screen pixels wide from `F_MIN` to `F_MAX`; the
    /// parameter's plain value is in Hz.
    LogHz(f32),
}

struct G<'a, 'b> {
    ui: &'a Ui,
    pt: Painter,
    cv: Cv,
    set: &'a ParamSetter<'b>,
    st: &'a mut EditorState,
    th: &'static Theme,
    /// Accent of the section currently being drawn.
    accent: Color32,
}

fn draw(ui: &mut Ui, p: &ReverbParams, set: &ParamSetter, st: &mut EditorState) {
    let full = ui.max_rect();
    let s = (full.width() / BW).min(full.height() / BH);
    let o = pos2(full.center().x - BW * s * 0.5, full.center().y - BH * s * 0.5);
    let cv = Cv { o, s };

    let theme_index = p.theme.load(Ordering::Relaxed);
    let th = theme::get(theme_index);
    if st.texture.as_ref().map(|(i, _)| *i) != Some(theme_index) {
        let tex = ui.ctx().load_texture(
            "panel-texture",
            panel_texture(th),
            egui::TextureOptions::LINEAR_REPEAT,
        );
        st.texture = Some((theme_index, tex));
    }
    if ui.style().visuals.panel_fill != th.outer {
        ui.ctx().style_mut(|s| s.visuals.panel_fill = th.outer);
    }
    st.focus = None;
    st.controls.clear();

    let pt = ui.painter().clone();
    let mut g = G { ui, pt, cv, set, st, th, accent: th.accents[0] };

    g.panel();
    g.header(p);

    g.bar(
        BAR1_Y,
        &[
            (16.0, 640.0, "AMP", Section::Amp),
            (640.0, 1180.0, "REVERB", Section::Reverb),
            (1180.0, RIGHT, "PING-PONG", Section::PingPong),
        ],
    );
    g.bar(
        BAR2_Y,
        &[
            (16.0, 930.0, "HARMONIC EQ", Section::Harmonic),
            (930.0, 1250.0, "FILTER", Section::Filter),
            (1250.0, RIGHT, "OUTPUT", Section::Output),
        ],
    );

    // ---------------------------------------------------------------- AMP
    g.accent = th.accents[Section::Amp as usize];
    let (l, sl) = (LABEL1_Y, SLOT1);
    g.switch(&p.amp_on, 44.0, l, "ON/OFF");
    let drive = g.st.visuals.drive.load(Ordering::Relaxed);
    g.label(44.0, 196.0, "CLIP");
    g.led_level(cv.p(44.0, 206.0), ((drive - 0.7) / 0.6).clamp(0.0, 1.0));
    g.rotary(&p.amp_type, 4, 112.0, 186.0, l, "TYPE", RotIcons::Amp);
    g.amp_graph(p, cv.r(166.0, l + 10.0, 216.0, 130.0));
    g.fader(&p.amp_drive, 422.0, l, sl, "DRIVE", Nums::Left("36", "18", "0"));
    g.fader(&p.amp_bias, 468.0, l, sl, "BIAS", Nums::None);
    g.fader(&p.amp_tone, 514.0, l, sl, "TONE", Nums::None);
    g.fader(&p.amp_mix, 560.0, l, sl, "MIX", Nums::None);
    g.fader(&p.amp_level, 606.0, l, sl, "LEVEL", Nums::Right("+12", "-6", "-24"));

    // ---------------------------------------------------------------- REVERB
    g.accent = th.accents[Section::Reverb as usize];
    g.switch(&p.verb_on, 668.0, l, "ON/OFF");
    g.reverb_graph(p, 696.0, 230.0);
    g.fader(&p.verb_mix, 958.0, l, sl, "MIX", N10L);
    g.fader(&p.verb_size, 1004.0, l, sl, "SIZE", Nums::None);
    g.fader(&p.verb_diffusion, 1050.0, l, sl, "DIFFU-\nSION", Nums::None);
    g.fader(&p.verb_mod, 1096.0, l, sl, "MODU-\nLATION", Nums::None);
    g.fader(&p.verb_width, 1142.0, l, sl, "WIDTH", N10R);

    // ---------------------------------------------------------------- PING-PONG
    g.accent = th.accents[Section::PingPong as usize];
    g.switch(&p.pp_on, 1206.0, l, "ON/OFF");
    g.switch(&p.pp_sync, 1250.0, l, "SYNC");
    g.echo_graph(p, 1278.0, 180.0);
    // One TIME fader: note value while synced, milliseconds otherwise.
    if g.shown(&p.pp_sync) > 0.5 {
        g.fader(&p.pp_division, 1490.0, l, sl, "TIME", Nums::None);
    } else {
        g.fader(&p.pp_time, 1490.0, l, sl, "TIME", Nums::None);
    }
    g.fader(&p.pp_feedback, 1536.0, l, sl, "FEED-\nBACK", Nums::None);
    g.fader(&p.pp_level, 1582.0, l, sl, "ECHO\nLEVEL", Nums::None);
    g.fader(&p.pp_send, 1626.0, l, sl, "INTO\nREVERB", Nums::None);

    // ---------------------------------------------------------------- HARMONIC EQ
    g.accent = th.accents[Section::Harmonic as usize];
    let (l, sl) = (LABEL2_Y, SLOT2);
    g.switch(&p.harm_on, 44.0, l, "ON/OFF");
    g.rotary(&p.harm_source, 3, 112.0, 394.0, l, "ROOT FROM", RotIcons::Text(&["KEY", "NOTE", "MAN"]));
    g.key_panel(p, cv.r(166.0, l + 10.0, 164.0, 130.0));
    g.harm_graph(p, 342.0, 204.0);
    g.stepper(&p.harm_octave, 6, 600.0, l, sl, "ROOT\nOCTAVE", &["C0", "C1", "C2", "C3", "C4", "C5"]);
    g.stepper(&p.harm_mode, 2, 664.0, l, sl, "MODE", &["PEAK", "ISO"]);
    g.stepper(&p.harm_select, 3, 728.0, l, sl, "HARMO-\nNICS", &["ALL", "EVEN", "ODD"]);
    g.fader(&p.harm_q, 772.0, l, sl, "Q", Nums::None);
    g.fader(&p.harm_gain, 818.0, l, sl, "GAIN", Nums::None);
    g.fader(&p.harm_count, 864.0, l, sl, "COUNT", Nums::None);
    g.fader(&p.harm_mix, 910.0, l, sl, "MIX", Nums::None);

    // ---------------------------------------------------------------- FILTER
    g.accent = th.accents[Section::Filter as usize];
    g.switch(&p.filt_on, 956.0, l, "ON/OFF");
    g.cut_graph(p, 984.0, 206.0);
    g.stepper(&p.filt_slope, 2, 1228.0, l, sl, "SLOPE", &["12", "24"]);

    // ---------------------------------------------------------------- OUTPUT
    g.accent = th.accents[Section::Output as usize];
    g.preset_buttons(p);
    g.meters();
    g.scope(cv.r(1462.0, 290.0, 92.0, 150.0));
    g.fader(&p.out_mix, 1588.0, l, sl, "DRY/\nWET", Nums::None);
    g.fader(&p.out_gain, 1630.0, l, sl, "OUTPUT", Nums::None);

    // The OLED is drawn last so it can show this frame's focus.
    g.oled(p);
}

/// Procedural powder coat: fine grain plus sparse lighter flecks.
fn panel_texture(th: &Theme) -> egui::ColorImage {
    let n = 256;
    let mut rng = crate::dsp::Rng::new(77);
    let mut px = Vec::with_capacity(n * n);
    for _ in 0..n * n {
        let grain = rng.bipolar() * th.grain;
        let fleck = if rng.unit() > 0.985 { th.fleck } else { 0.0 };
        let v = |c: u8| (c as f32 + grain + fleck).clamp(0.0, 255.0) as u8;
        px.push(Color32::from_rgb(v(th.panel.r()), v(th.panel.g()), v(th.panel.b())));
    }
    egui::ColorImage { size: [n, n], pixels: px }
}

/// Horizontal position 0..1 of a frequency on the graph axis.
fn freq_t(hz: f32) -> f32 {
    (hz / F_MIN).log2() / (F_MAX / F_MIN).log2()
}

fn t_freq(t: f32) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(t)
}

impl G<'_, '_> {
    fn text(&self, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) {
        self.pt.text(pos, align, text, self.cv.font(size), color);
    }

    /// Multi-line label, bottom line sitting on `y`.
    fn label(&self, x: f32, y: f32, text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let n = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let yy = y - (n - 1 - i) as f32 * 11.0;
            self.text(self.cv.p(x, yy), Align2::CENTER_BOTTOM, line, 10.0, self.th.text);
        }
    }

    fn panel(&self) {
        let cv = self.cv;
        let rect = cv.r(0.0, 0.0, BW, BH);
        let tex = &self.st.texture.as_ref().unwrap().1;
        let uv = Rect::from_min_max(
            pos2(0.0, 0.0),
            pos2(rect.width() / (256.0 * cv.s), rect.height() / (256.0 * cv.s)),
        );
        self.pt.image(tex.id(), rect, uv, Color32::WHITE);
        self.pt
            .line_segment([cv.p(2.0, 1.0), cv.p(BW - 2.0, 1.0)], cv.stroke(1.5, self.th.bevel));
        self.pt.line_segment(
            [cv.p(2.0, BH - 1.0), cv.p(BW - 2.0, BH - 1.0)],
            cv.stroke(2.0, Color32::from_black_alpha(120)),
        );
    }

    fn header(&mut self, p: &ReverbParams) {
        let cv = self.cv;
        for (x, t) in [(260.0, "AUDIO IN · OUT"), (520.0, "MIDI IN · KEY DETECTION")] {
            self.text(cv.p(x, 14.0), Align2::CENTER_CENTER, t, 8.0, self.th.header);
        }
        self.text(
            cv.p(18.0, 30.0),
            Align2::LEFT_CENTER,
            "HARMONIC AMP · REVERB · PING-PONG DELAY · KEY TRACKING HARMONIC EQ",
            9.0,
            self.th.header,
        );
        self.note_leds();
        let tempo = self.st.visuals.tempo.load(Ordering::Relaxed);
        self.text(
            cv.p(1080.0, 22.0),
            Align2::LEFT_CENTER,
            &format!("TEMPO {tempo:.1} BPM"),
            8.5,
            self.th.header,
        );
        self.theme_selector(p);
    }

    /// One LED per pitch class, lit while a MIDI note of that class is held.
    fn note_leds(&self) {
        let cv = self.cv;
        let held = self.st.visuals.held.load(Ordering::Relaxed);
        self.text(cv.p(752.0, 22.0), Align2::RIGHT_CENTER, "MIDI NOTES", 8.5, self.th.header);
        let round = CornerRadius::same((1.5 * cv.s) as u8);
        for (pc, name) in NOTE_NAMES.iter().enumerate() {
            let x = 760.0 + pc as f32 * 22.0;
            let rect = cv.r(x, 17.0, 18.0, 10.0);
            let on = held & (1 << pc) != 0;
            self.pt
                .rect_filled(rect, round, if on { self.th.led } else { self.th.led_off });
            let ink = if on { self.th.bar_text } else { self.th.header };
            self.text(rect.center(), Align2::CENTER_CENTER, name, 6.5, ink);
        }
    }

    /// Row of chips in the header that switches the colour scheme.
    fn theme_selector(&mut self, p: &ReverbParams) {
        let cv = self.cv;
        let current = p.theme.load(Ordering::Relaxed) as usize;
        let chip_w = |name: &str| 14.0 + name.len() as f32 * 5.6;
        let total: f32 = THEMES.iter().map(|t| chip_w(t.name) + 5.0).sum::<f32>() - 5.0;
        let mut x = RIGHT - total;
        self.text(cv.p(x - 10.0, 22.0), Align2::RIGHT_CENTER, "THEME", 8.5, self.th.header);
        for (i, theme) in THEMES.iter().enumerate() {
            let w = chip_w(theme.name);
            let rect = cv.r(x, 15.0, w, 14.0);
            let key = format!("theme:{}", theme.name);
            let resp = self.ui.interact(rect, Id::new(&key), Sense::click());
            self.register(&key, ControlKind::Button, rect);
            let (fill, ink) = if i == current {
                (self.th.sublabel, self.th.bar_text)
            } else if resp.hovered() {
                (self.th.button_hover, self.th.text)
            } else {
                (self.th.button, self.th.text)
            };
            self.pt.rect_filled(rect, CornerRadius::same((3.0 * cv.s) as u8), fill);
            self.text(rect.center(), Align2::CENTER_CENTER, theme.name, 8.0, ink);
            if resp.clicked() {
                p.theme.store(i as u8, Ordering::Relaxed);
            }
            if resp.hovered() {
                self.set_focus("COLOUR THEME", theme.name.to_owned());
            }
            x += w + 5.0;
        }
    }

    fn bar(&self, y: f32, sections: &[(f32, f32, &str, Section)]) {
        let cv = self.cv;
        for (a, b, title, section) in sections {
            let rect = cv.r(a + 1.5, y, b - a - 3.0, 22.0);
            self.pt.rect_filled(
                rect,
                CornerRadius::same((3.0 * cv.s) as u8),
                self.th.bars[*section as usize],
            );
            self.text(cv.p((a + b) * 0.5, y + 11.5), Align2::CENTER_CENTER, title, 15.0, self.th.bar_text);
        }
    }

    fn register(&mut self, name: &str, kind: ControlKind, rect: Rect) {
        self.st.controls.push(ControlInfo { name: name.to_owned(), kind, rect });
    }

    fn now(&self) -> f64 {
        self.ui.input(|i| i.time)
    }

    /// The parameter's actual normalized value (from the preview store in the standalone preview).
    fn actual<P: Param>(&self, param: &P) -> f32 {
        if let Some(store) = &self.st.preview_store {
            if let Some(v) = store.lock().unwrap().get(param.name()) {
                return *v;
            }
        }
        param.unmodulated_normalized_value()
    }

    /// The value to display: a value the GUI just sent wins until the host has applied it.
    fn shown<P: Param>(&mut self, param: &P) -> f32 {
        let actual = self.actual(param);
        let now = self.now();
        match self.st.pending.get(param.name()) {
            Some(p) if (p.value - actual).abs() > 1e-5 && now - p.time < PENDING_TIMEOUT => p.value,
            Some(_) => {
                self.st.pending.remove(param.name());
                actual
            }
            None => actual,
        }
    }

    /// Shown value as the parameter's plain type.
    fn plain<P: Param>(&mut self, param: &P) -> P::Plain {
        let v = self.shown(param);
        param.preview_plain(v)
    }

    /// Send a normalized value (snapped to the parameter's steps) and remember it as pending.
    /// Must be called between `begin_set_parameter` and `end_set_parameter`.
    fn send<P: Param>(&mut self, param: &P, normalized: f32) {
        let v = param.preview_normalized(param.preview_plain(normalized.clamp(0.0, 1.0)));
        if (v - self.shown(param)).abs() < 1e-6 {
            return;
        }
        self.set.set_parameter_normalized(param, v);
        let time = self.now();
        self.st.pending.insert(param.name().to_owned(), Pending { value: v, time });
    }

    /// A complete single-step change (begin, set, end).
    fn change<P: Param>(&mut self, param: &P, normalized: f32) {
        self.set.begin_set_parameter(param);
        self.send(param, normalized);
        self.set.end_set_parameter(param);
    }

    fn value_text<P: Param>(&mut self, param: &P) -> String {
        let v = self.shown(param);
        param.normalized_value_to_string(v, true)
    }

    fn set_focus(&mut self, name: &str, value: String) {
        self.st.focus = Some(Focus { name: name.to_uppercase(), value });
    }

    fn scale_ticks(&self, x: f32, slot: (f32, f32), nums: Nums) {
        let cv = self.cv;
        let (y0, y1) = slot;
        for i in 0..11 {
            let y = y1 - i as f32 / 10.0 * (y1 - y0);
            let major = i % 5 == 0;
            let w = if major { 15.0 } else { 12.0 };
            self.pt.line_segment(
                [cv.p(x - w, y), cv.p(x + w, y)],
                cv.stroke(if major { 1.2 } else { 0.9 }, self.th.scale),
            );
        }
        let (side, legends) = match nums {
            Nums::None => return,
            Nums::Left(a, b, c) => (-1.0, [a, b, c]),
            Nums::Right(a, b, c) => (1.0, [a, b, c]),
        };
        let align = if side < 0.0 { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
        for (k, text) in legends.iter().enumerate() {
            let y = y0 + k as f32 * 0.5 * (y1 - y0);
            self.text(cv.p(x + side * 18.0, y), align, text, 9.0, self.th.legend);
        }
    }

    fn slot(&self, x: f32, slot: (f32, f32)) {
        let cv = self.cv;
        self.pt.rect_filled(
            cv.r(x - 2.5, slot.0 - 8.0, 5.0, slot.1 - slot.0 + 16.0),
            CornerRadius::same((2.0 * cv.s) as u8),
            self.th.slot,
        );
    }

    fn cap(&self, x: f32, y: f32, active: bool) {
        let cv = self.cv;
        let body = cv.rc(x, y, 20.0, 30.0);
        let round = CornerRadius::same((2.0 * cv.s) as u8);
        self.pt.rect_filled(
            body.translate(vec2(1.5 * cv.s, 3.0 * cv.s)),
            round,
            Color32::from_black_alpha(140),
        );
        self.pt.rect_filled(body, round, self.th.cap);
        // Sloped faces above and below the index line
        self.pt
            .rect_filled(cv.rc(x, y - 8.0, 18.0, 10.0), CornerRadius::same(cv.s as u8), self.th.cap_top);
        self.pt.rect_filled(
            cv.rc(x, y + 8.0, 18.0, 10.0),
            CornerRadius::same(cv.s as u8),
            self.th.cap_bottom,
        );
        self.pt.line_segment(
            [cv.p(x - 9.0, y), cv.p(x + 9.0, y)],
            cv.stroke(2.2, if active { self.th.drag_line } else { self.accent }),
        );
    }

    /// Continuous fader for any parameter (stepped parameters snap).
    fn fader<P: Param>(&mut self, param: &P, x: f32, label_y: f32, slot: (f32, f32), label: &str, nums: Nums) {
        let cv = self.cv;
        let (y0, y1) = slot;
        let rect = cv.r(x - 14.0, y0 - 16.0, 28.0, y1 - y0 + 32.0);
        let id = Id::new(("fader", param.name()));
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());
        self.register(param.name(), ControlKind::Fader, rect);

        // The drag position is tracked locally (start value + accumulated movement) because the
        // parameter itself only updates asynchronously.
        if resp.drag_started() {
            let start = self.shown(param);
            self.ui.data_mut(|d| d.insert_temp(id, start));
            self.set.begin_set_parameter(param);
        }
        if resp.dragged() {
            let fine = self.ui.input(|i| i.modifiers.shift);
            let travel = (y1 - y0) * cv.s;
            let delta = -resp.drag_delta().y / travel * if fine { 0.15 } else { 1.0 };
            let start = self.shown(param);
            let pos: f32 = self.ui.data(|d| d.get_temp(id)).unwrap_or(start);
            let pos = (pos + delta).clamp(0.0, 1.0);
            self.ui.data_mut(|d| d.insert_temp(id, pos));
            self.send(param, pos);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(param);
        }
        if resp.double_clicked() {
            self.change(param, param.default_normalized_value());
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // Stepped parameters move one step per notch.
                let v = match param.step_count() {
                    Some(n) => self.shown(param) + scroll.signum() / n as f32,
                    None => self.shown(param) + scroll * 0.0015,
                };
                self.change(param, v);
            }
        }
        if resp.hovered() || resp.dragged() {
            let text = self.value_text(param);
            self.set_focus(param.name(), text);
        }

        self.label(x, label_y, label);
        self.scale_ticks(x, slot, nums);
        self.slot(x, slot);
        let v = self.shown(param);
        self.cap(x, y1 - v * (y1 - y0), resp.dragged());
    }

    /// Fader with detents; positions are labelled with text on the left. `count` must match the
    /// parameter's number of steps.
    #[allow(clippy::too_many_arguments)]
    fn stepper<P: Param>(
        &mut self,
        param: &P,
        count: usize,
        x: f32,
        label_y: f32,
        slot: (f32, f32),
        label: &str,
        steps: &[&str],
    ) {
        let cv = self.cv;
        let (y0, y1) = slot;
        let last = (count - 1) as f32;
        // Two position steppers use the middle of the slot so they read as a switch.
        let (y0, y1) = if count == 2 { (y0 + 25.0, y1 - 25.0) } else { (y0, y1) };
        let pos_y = |i: usize| y1 - i as f32 / last * (y1 - y0);
        let rect = cv.r(x - 40.0, slot.0 - 16.0, 54.0, slot.1 - slot.0 + 32.0);
        let id = Id::new(("step", param.name()));
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());
        self.register(param.name(), ControlKind::Stepper, rect);

        let current = (self.shown(param) * last).round() as usize;
        let mut target = None;

        if resp.drag_started() {
            self.ui.data_mut(|d| d.insert_temp(id, current as f32));
            self.set.begin_set_parameter(param);
        }
        if resp.dragged() {
            let step_px = ((y1 - y0) / last).max(20.0) * cv.s;
            let mut acc: f32 = self.ui.data(|d| d.get_temp(id)).unwrap_or(current as f32);
            acc = (acc - resp.drag_delta().y / step_px).clamp(0.0, last);
            self.ui.data_mut(|d| d.insert_temp(id, acc));
            self.send(param, acc.round() / last);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(param);
        }
        if resp.clicked() {
            if let Some(ptr) = resp.interact_pointer_pos() {
                let local_y = (ptr.y - cv.o.y) / cv.s;
                target = (0..count)
                    .min_by(|a, b| (pos_y(*a) - local_y).abs().total_cmp(&(pos_y(*b) - local_y).abs()));
            }
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll > 0.0 && current + 1 < count {
                target = Some(current + 1);
            } else if scroll < 0.0 && current > 0 {
                target = Some(current - 1);
            }
        }
        if let Some(i) = target {
            self.change(param, i as f32 / last);
        }
        if resp.hovered() || resp.dragged() {
            let text = self.value_text(param);
            self.set_focus(param.name(), text);
        }

        let shown = (self.shown(param) * last).round() as usize;
        self.label(x, label_y, label);
        for (i, step) in steps.iter().enumerate().take(count) {
            let y = pos_y(i);
            self.pt
                .line_segment([cv.p(x - 9.0, y), cv.p(x + 12.0, y)], cv.stroke(1.2, self.th.scale));
            let color = if i == shown { self.accent } else { self.th.step_off };
            self.text(cv.p(x - 14.0, y), Align2::RIGHT_CENTER, step, 9.0, color);
        }
        self.pt.rect_filled(
            cv.r(x - 2.5, y0 - 8.0, 5.0, y1 - y0 + 16.0),
            CornerRadius::same((2.0 * cv.s) as u8),
            self.th.slot,
        );
        self.cap(x, pos_y(shown), resp.dragged());
    }

    /// Rotary switch with 2 to 4 positions and icons or text around it.
    #[allow(clippy::too_many_arguments)]
    fn rotary<P: Param>(
        &mut self,
        param: &P,
        count: usize,
        cx: f32,
        cy: f32,
        label_y: f32,
        title: &str,
        icons: RotIcons,
    ) {
        let cv = self.cv;
        let last = (count - 1) as f32;
        let angles: &[f32] = match count {
            2 => &[-60.0, 60.0],
            3 => &[-100.0, 0.0, 100.0],
            _ => &[-125.0, -45.0, 45.0, 125.0],
        };
        let current = (self.shown(param) * last).round() as usize;
        let dir = |deg: f32| {
            let a = (deg - 90.0).to_radians();
            vec2(a.cos(), a.sin())
        };
        let center = cv.p(cx, cy);
        let rect = cv.rc(cx, cy, 92.0, 92.0);
        let id = Id::new(("sel", param.name()));
        let resp = self.ui.interact(rect, id, Sense::click_and_drag());
        self.register(param.name(), ControlKind::Selector, rect);
        let mut result = None;

        if resp.drag_started() {
            self.ui.data_mut(|d| d.insert_temp(id, current as f32));
            self.set.begin_set_parameter(param);
        }
        if resp.dragged() {
            let mut acc: f32 = self.ui.data(|d| d.get_temp(id)).unwrap_or(current as f32);
            let d = resp.drag_delta();
            acc = (acc + (d.x - d.y) / (24.0 * cv.s)).clamp(0.0, last);
            self.ui.data_mut(|dm| dm.insert_temp(id, acc));
            self.send(param, acc.round() / last);
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(param);
        }
        if resp.clicked() {
            if let Some(ptr) = resp.interact_pointer_pos() {
                if (ptr - center).length() > 24.0 * cv.s {
                    // Clicked an icon: pick the nearest position.
                    let icon_dist = |i: usize| (center + dir(angles[i]) * 38.0 * cv.s).distance(ptr);
                    let i = (0..count).min_by(|a, b| icon_dist(*a).total_cmp(&icon_dist(*b))).unwrap();
                    result = Some(i);
                } else {
                    result = Some((current + 1) % count);
                }
            }
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll > 0.0 && current + 1 < count {
                result = Some(current + 1);
            } else if scroll < 0.0 && current > 0 {
                result = Some(current - 1);
            }
        }
        if let Some(i) = result {
            self.change(param, i as f32 / last);
        }
        let shown = (self.shown(param) * last).round() as usize;
        if resp.hovered() || resp.dragged() {
            let text = self.value_text(param).to_uppercase();
            self.set_focus(param.name(), text);
        }

        self.text(cv.p(cx, label_y), Align2::CENTER_BOTTOM, title, 10.0, self.th.text);

        // Petal outline around the icons
        let ring: Vec<Pos2> = (0..=40)
            .map(|k| center + dir(-160.0 + 320.0 * k as f32 / 40.0) * 28.0 * cv.s)
            .collect();
        self.pt.add(Shape::line(ring, cv.stroke(1.2, self.th.scale)));
        for deg in angles {
            self.pt.line_segment(
                [center + dir(*deg) * 23.0 * cv.s, center + dir(*deg) * 28.0 * cv.s],
                cv.stroke(1.6, self.th.ring_tick),
            );
        }
        for (i, deg) in angles.iter().enumerate() {
            let c = center + dir(*deg) * 39.0 * cv.s;
            let color = if i == shown { self.accent } else { self.th.step_off };
            match icons {
                RotIcons::Amp => {
                    let kind = AmpType::from_index(i);
                    let pts: Vec<Pos2> = (0..=20)
                        .map(|k| {
                            let x = k as f32 / 10.0 - 1.0;
                            let y = curve(kind, x * 2.2) / curve(kind, 2.2).abs().max(0.3);
                            c + vec2(x * 7.0, -y.clamp(-1.2, 1.2) * 5.0) * cv.s
                        })
                        .collect();
                    self.pt.add(Shape::line(pts, Stroke::new(1.3 * cv.s, color)));
                }
                RotIcons::Text(names) => {
                    self.text(c, Align2::CENTER_CENTER, names[i], 8.0, color);
                }
            }
        }

        // Knob: ribbed skirt with a light cap
        let r = 19.0 * cv.s;
        self.pt.circle_filled(center + vec2(1.5, 3.0) * cv.s, r, Color32::from_black_alpha(150));
        self.pt.circle_filled(center, r, self.th.cap);
        for k in 0..24 {
            let a = k as f32 / 24.0 * 2.0 * PI;
            let d = vec2(a.cos(), a.sin());
            self.pt
                .line_segment([center + d * r * 0.8, center + d * r], Stroke::new(cv.s, self.th.knob_rib));
        }
        let cap_r = 11.5 * cv.s;
        self.pt.circle_filled(center, cap_r, self.th.knob_cap);
        self.pt.circle_stroke(center, cap_r, Stroke::new(cv.s, self.th.knob_cap_edge));
        let d = dir(angles[shown.min(count - 1)]);
        self.pt.line_segment(
            [center + d * 4.0 * cv.s, center + d * (r - 1.0)],
            Stroke::new(2.4 * cv.s, self.th.knob_pointer),
        );
    }

    /// LED lit to `level` in 0..1.
    fn led_level(&self, c: Pos2, level: f32) {
        let s = self.cv.s;
        if level > 0.0 {
            self.pt.circle_filled(c, 6.0 * s, self.th.led.gamma_multiply(0.25 * level));
        }
        self.pt.circle_filled(c, 3.2 * s, self.th.led_off);
        if level > 0.0 {
            self.pt.circle_filled(c, 3.2 * s, self.th.led.gamma_multiply(level));
        }
        self.pt.circle_stroke(c, 3.2 * s, Stroke::new(0.8 * s, self.th.slot));
    }

    fn led(&self, c: Pos2, on: bool) {
        self.led_level(c, if on { 1.0 } else { 0.0 });
    }

    /// Rectangular push button with an optional legend above and a coloured one below.
    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, key: &str, x: f32, y: f32, w: f32, top: &str, bottom: &str, help: &str, value: String) -> bool {
        let cv = self.cv;
        let rect = cv.r(x, y, w, 16.0);
        let resp = self.ui.interact(rect, Id::new(("btn", key)), Sense::click());
        self.register(key, ControlKind::Button, rect);
        let down = resp.is_pointer_button_down_on();
        let face = if down {
            self.th.button_down
        } else if resp.hovered() {
            self.th.button_hover
        } else {
            self.th.button
        };
        let round = CornerRadius::same((2.0 * cv.s) as u8);
        let rect = if down { rect.translate(vec2(0.0, cv.s)) } else { rect };
        self.pt
            .rect_filled(rect.translate(vec2(0.0, 2.0 * cv.s)), round, Color32::from_black_alpha(160));
        self.pt.rect_filled(rect, round, face);
        self.pt.line_segment(
            [rect.left_top() + vec2(2.0, 1.0) * cv.s, rect.right_top() + vec2(-2.0, 1.0) * cv.s],
            cv.stroke(1.0, Color32::from_white_alpha(25)),
        );
        if !top.is_empty() {
            self.text(cv.p(x + w * 0.5, y - 3.0), Align2::CENTER_BOTTOM, top, 8.5, self.th.text);
        }
        if !bottom.is_empty() {
            self.text(cv.p(x + w * 0.5, y + 19.0), Align2::CENTER_TOP, bottom, 7.5, self.th.sublabel);
        }
        if resp.hovered() {
            self.set_focus(help, value);
        }
        resp.clicked()
    }

    /// LED and push button that toggle a boolean parameter.
    fn switch(&mut self, param: &BoolParam, x: f32, label_y: f32, label: &str) {
        let on = self.shown(param) > 0.5;
        self.label(x, label_y, label);
        self.led(self.cv.p(x, label_y + 14.0), on);
        let value = if on { "ON" } else { "OFF" }.to_owned();
        if self.button(param.name(), x - 12.0, label_y + 26.0, 24.0, "", "", param.name(), value) {
            self.change(param, if on { 0.0 } else { 1.0 });
        }
    }

    // ------------------------------------------------------------ OUTPUT

    fn preset_buttons(&mut self, p: &ReverbParams) {
        let count = PRESETS.len();
        let y = 384.0;
        let name = |i: usize| PRESETS[i].name.to_uppercase();
        let prev = (self.st.preset.unwrap_or(0) + count - 1) % count;
        if self.button("prev", 1266.0, y, 36.0, "", "PREV", "PREVIOUS PRESET", name(prev)) {
            self.load(p, prev);
        }
        let next = self.st.preset.map(|i| (i + 1) % count).unwrap_or(0);
        if self.button("next", 1308.0, y, 36.0, "", "NEXT", "NEXT PRESET", name(next)) {
            self.load(p, next);
        }
        if self.button("init", 1350.0, y, 36.0, "", "INIT", "INITIALIZE", name(0)) {
            self.load(p, 0);
        }
        let cv = self.cv;
        for (x, flip) in [(1284.0, -1.0), (1326.0, 1.0)] {
            let yy = y - 5.0;
            self.pt.add(Shape::convex_polygon(
                vec![cv.p(x + 4.0 * flip, yy), cv.p(x - 4.0 * flip, yy - 3.5), cv.p(x - 4.0 * flip, yy + 3.5)],
                self.th.sublabel,
                Stroke::NONE,
            ));
        }
        self.text(cv.p(1446.0, y + 8.0), Align2::RIGHT_CENTER, "PRESET", 9.0, self.th.legend);
    }

    fn load(&mut self, p: &ReverbParams, index: usize) {
        let now = self.now();
        for (name, value) in apply_preset(p, self.set, index) {
            self.st.pending.insert(name, Pending { value, time: now });
        }
        self.st.preset = Some(index);
    }

    /// Input and output peak meters under the preset buttons.
    fn meters(&self) {
        let cv = self.cv;
        let v = &self.st.visuals;
        let rows = [
            ("IN", v.in_l.load(Ordering::Relaxed), v.in_r.load(Ordering::Relaxed)),
            ("OUT", v.out_l.load(Ordering::Relaxed), v.out_r.load(Ordering::Relaxed)),
        ];
        for (k, (name, l, r)) in rows.iter().enumerate() {
            let y = 420.0 + k as f32 * 16.0;
            self.text(cv.p(1290.0, y + 3.5), Align2::RIGHT_CENTER, name, 8.0, self.th.legend);
            for (c, gain) in [*l, *r].into_iter().enumerate() {
                let rect = cv.r(1296.0, y + c as f32 * 4.5, 150.0, 3.0);
                self.pt.rect_filled(rect, CornerRadius::ZERO, self.th.slot);
                let db = 20.0 * gain.max(1.0e-6).log10();
                let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let color = if gain > 1.0 { self.th.sublabel } else { self.accent };
                let lit = Rect::from_min_size(rect.min, vec2(rect.width() * t, rect.height()));
                self.pt.rect_filled(lit, CornerRadius::ZERO, color);
            }
        }
        // 0 dB, -12 dB, -24 dB marks
        for db in [0.0f32, -12.0, -24.0, -48.0] {
            let x = 1296.0 + 150.0 * (db + 60.0) / 60.0;
            self.pt
                .line_segment([cv.p(x, 417.0), cv.p(x, 446.0)], cv.stroke(0.6, self.th.scale));
        }
    }

    /// Min/max envelope of the output, triggered on rising zero crossings.
    fn scope(&self, rect: Rect) {
        let s = self.cv.s;
        let round = CornerRadius::same((3.0 * s) as u8);
        self.pt.rect_filled(rect.expand(3.0 * s), round, self.th.oled_frame);
        self.pt.rect_filled(rect, CornerRadius::ZERO, self.th.oled_bg);
        let ink = self.th.oled_ink;
        let mid = rect.center().y;
        self.pt.line_segment(
            [pos2(rect.left(), mid), pos2(rect.right(), mid)],
            Stroke::new(0.6 * s, self.th.oled_dim),
        );
        self.pt.text(
            rect.left_top() + vec2(5.0, 4.0) * s,
            Align2::LEFT_TOP,
            "OUTPUT",
            self.cv.mono(7.5),
            self.th.oled_dim,
        );
        let Ok(mut output) = self.st.scope.lock() else {
            return;
        };
        let frame = output.read();
        let half = rect.height() * 0.5 - 12.0 * s;
        let x_of = |i: usize| rect.left() + i as f32 * rect.width() / (SCOPE_COLUMNS - 1) as f32;
        let y_of = |v: f32| mid - v.clamp(-1.0, 1.0) * half;
        let mut mesh = egui::Mesh::default();
        let fill = ink.gamma_multiply(0.22);
        for i in 0..SCOPE_COLUMNS - 1 {
            let k = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x_of(i), y_of(frame.max[i])), fill);
            mesh.colored_vertex(pos2(x_of(i + 1), y_of(frame.max[i + 1])), fill);
            mesh.colored_vertex(pos2(x_of(i + 1), y_of(frame.min[i + 1])), fill);
            mesh.colored_vertex(pos2(x_of(i), y_of(frame.min[i])), fill);
            mesh.add_triangle(k, k + 1, k + 2);
            mesh.add_triangle(k, k + 2, k + 3);
        }
        self.pt.add(Shape::mesh(mesh));
        let trace: Vec<Pos2> = (0..SCOPE_COLUMNS).map(|i| pos2(x_of(i), y_of(frame.max[i]))).collect();
        self.pt.add(Shape::line(trace, Stroke::new(1.0 * s, ink)));
    }

    fn oled(&mut self, p: &ReverbParams) {
        let cv = self.cv;
        let th = self.th;
        let rect = cv.r(1266.0, 288.0, 180.0, 70.0);
        self.pt.rect_filled(rect.expand(3.0 * cv.s), CornerRadius::same((3.0 * cv.s) as u8), th.oled_frame);
        self.pt.rect_filled(rect, CornerRadius::ZERO, th.oled_bg);
        let tl = |x: f32, y: f32| cv.p(1272.0 + x, 293.0 + y);
        let on = |g: &mut Self, param: &BoolParam| g.shown(param) > 0.5;
        let sections = [
            ("AMP", on(self, &p.amp_on)),
            ("VRB", on(self, &p.verb_on)),
            ("PP", on(self, &p.pp_on)),
            ("EQ", on(self, &p.harm_on)),
            ("FLT", on(self, &p.filt_on)),
        ];
        let txt = |pos: Pos2, text: String, size: f32, color: Color32| {
            self.pt.text(pos, Align2::LEFT_TOP, text, cv.mono(size), color);
        };

        if let Some(f) = &self.st.focus {
            txt(tl(0.0, 0.0), f.name.clone(), 8.5, th.oled_dim);
            txt(tl(0.0, 15.0), f.value.clone(), 17.0, th.oled_ink);
        } else {
            let head = match self.st.preset {
                Some(i) => format!("PRESET {:02}", i + 1),
                None => "PRESET --".to_owned(),
            };
            let name = self.st.preset.map(|i| PRESETS[i].name).unwrap_or("USER");
            txt(tl(0.0, 0.0), head, 8.5, th.oled_dim);
            txt(tl(0.0, 14.0), name.to_uppercase(), 12.0, th.oled_ink);
            let v = &self.st.visuals;
            let key = v.key_root.load(Ordering::Relaxed);
            let key = if key >= 0 {
                let minor = v.key_minor.load(Ordering::Relaxed);
                format!("KEY {} {}", NOTE_NAMES[key as usize], if minor { "MIN" } else { "MAJ" })
            } else {
                "KEY --".to_owned()
            };
            txt(tl(0.0, 32.0), key, 8.0, th.oled_dim);
        }

        // Footer: which sections are in circuit.
        let footer_y = 51.0;
        self.pt.line_segment(
            [tl(0.0, footer_y - 2.0), tl(168.0, footer_y - 2.0)],
            Stroke::new(0.6 * cv.s, th.oled_dim),
        );
        let mut x = 0.0;
        for (name, active) in sections {
            let color = if active { th.oled_ink } else { th.oled_dim.gamma_multiply(0.6) };
            txt(tl(x, footer_y), name.to_owned(), 7.5, color);
            x += name.len() as f32 * 4.6 + 9.0;
        }
    }

    // ------------------------------------------------------------ graphs

    /// Recessed background behind the panel graphs.
    fn graph_bg(&self, rect: Rect) {
        self.pt.rect_filled(
            rect,
            CornerRadius::same((3.0 * self.cv.s) as u8),
            self.th.slot.gamma_multiply(0.55),
        );
    }

    /// Fill between a polyline and a horizontal baseline, as a strip of quads (the area is
    /// usually not convex).
    fn fill_under(&self, pts: &[Pos2], base: f32, color: Color32) {
        let mut mesh = egui::Mesh::default();
        for w in pts.windows(2) {
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(w[0], color);
            mesh.colored_vertex(w[1], color);
            mesh.colored_vertex(pos2(w[1].x, base), color);
            mesh.colored_vertex(pos2(w[0].x, base), color);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
        self.pt.add(Shape::mesh(mesh));
    }

    /// Small legend in the top left corner of a graph.
    fn graph_legend(&self, rect: Rect, x: f32, text: &str, color: Color32) {
        self.pt.text(
            rect.left_top() + vec2(x, 4.0) * self.cv.s,
            Align2::LEFT_TOP,
            text,
            self.cv.font(8.0),
            color,
        );
    }

    /// Square drag handle, highlighted while hovered or dragged.
    fn handle_square(&self, c: Pos2, active: bool) {
        let s = self.cv.s;
        let r = Rect::from_center_size(c, vec2(9.0, 9.0) * s);
        let round = CornerRadius::same((1.5 * s) as u8);
        let fill = if active { self.th.drag_line } else { self.th.text };
        self.pt.rect_filled(r, round, fill);
        self.pt.rect_stroke(r, round, Stroke::new(1.2 * s, self.th.slot), StrokeKind::Outside);
    }

    /// Pointer movement since the button went down, scaled down with Shift. Graph handles use
    /// this rather than per-frame deltas so the movement before egui recognises the drag is not
    /// lost.
    fn drag_offset(&self) -> Vec2 {
        self.ui.input(|i| {
            let fine = if i.modifiers.shift { 0.15 } else { 1.0 };
            match (i.pointer.press_origin(), i.pointer.interact_pos()) {
                (Some(a), Some(b)) => (b - a) * fine,
                _ => Vec2::ZERO,
            }
        })
    }

    /// Drag handle for one or two parameters on a graph: horizontal movement changes `x` along
    /// `axis`, vertical movement changes `y` by one full range per `y_span` screen pixels.
    /// Double-click resets, the wheel moves `x`. Returns whether it is hovered or dragged.
    #[allow(clippy::too_many_arguments)]
    fn xy_handle(
        &mut self,
        key: &str,
        hit: Rect,
        x: &FloatParam,
        y: Option<&FloatParam>,
        axis: Axis,
        y_span: f32,
    ) -> bool {
        let id = Id::new(("xy", key));
        let resp = self.ui.interact(hit, id, Sense::click_and_drag());
        self.register(x.name(), ControlKind::Handle, hit);

        if resp.drag_started() {
            let start = (self.shown(x), y.map(|y| self.shown(y)).unwrap_or(0.0));
            self.ui.data_mut(|d| d.insert_temp(id, start));
            self.set.begin_set_parameter(x);
            if let Some(y) = y {
                self.set.begin_set_parameter(y);
            }
        }
        if resp.dragged() {
            let d = self.drag_offset();
            let current = (self.shown(x), y.map(|y| self.shown(y)).unwrap_or(0.0));
            let (x0, y0): (f32, f32) = self.ui.data(|dm| dm.get_temp(id)).unwrap_or(current);
            let nx = match axis {
                Axis::Linear(span) => x0 + d.x / span,
                Axis::LogHz(width) => {
                    let t = freq_t(x.preview_plain(x0)) + d.x / width;
                    x.preview_normalized(t_freq(t))
                }
            };
            self.send(x, nx);
            if let Some(y) = y {
                self.send(y, y0 - d.y / y_span);
            }
        }
        if resp.drag_stopped() {
            self.set.end_set_parameter(x);
            if let Some(y) = y {
                self.set.end_set_parameter(y);
            }
        }
        if resp.double_clicked() {
            self.change(x, x.default_normalized_value());
            if let Some(y) = y {
                self.change(y, y.default_normalized_value());
            }
        }
        if resp.hovered() && !resp.dragged() {
            let scroll = self.ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let v = self.shown(x) + scroll * 0.0015;
                self.change(x, v);
            }
        }
        let active = resp.hovered() || resp.dragged();
        if active {
            let (name, value) = match y {
                Some(y) => (
                    format!("{} / {}", x.name(), y.name()),
                    format!("{} / {}", self.value_text(x), self.value_text(y)),
                ),
                None => (x.name().to_owned(), self.value_text(x)),
            };
            self.set_focus(&name, value);
        }
        active
    }

    /// Vertical grid lines of the frequency axis.
    fn freq_grid(&self, inner: Rect) {
        let s = self.cv.s;
        for hz in [50.0_f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
            let x = inner.left() + inner.width() * freq_t(hz);
            let decade = hz == 100.0 || hz == 1000.0 || hz == 10_000.0;
            self.pt.line_segment(
                [pos2(x, inner.top()), pos2(x, inner.bottom())],
                Stroke::new((if decade { 1.0 } else { 0.6 }) * s, self.th.scale),
            );
        }
    }

    /// Transfer curve (left) and the harmonics a -6 dB sine gets (right).
    fn amp_graph(&mut self, p: &ReverbParams, rect: Rect) {
        let s = self.cv.s;
        let kind = self.plain(&p.amp_type);
        let drive = util::db_to_gain(self.plain(&p.amp_drive));
        let bias = self.plain(&p.amp_bias);
        let active = self.shown(&p.amp_on) > 0.5;
        let ink = if active { self.accent } else { self.th.step_off };
        self.graph_bg(rect);

        // Transfer curve, input -1..1 across, output -1.2..1.2 up.
        let side = rect.height() - 26.0 * s;
        let curve_rect = Rect::from_min_size(rect.left_top() + vec2(8.0, 18.0) * s, vec2(side, side));
        let c = curve_rect.center();
        let half = side * 0.5;
        for (a, b) in [
            (pos2(curve_rect.left(), c.y), pos2(curve_rect.right(), c.y)),
            (pos2(c.x, curve_rect.top()), pos2(c.x, curve_rect.bottom())),
        ] {
            self.pt.line_segment([a, b], Stroke::new(0.8 * s, self.th.scale));
        }
        self.pt.line_segment(
            [pos2(curve_rect.left(), curve_rect.bottom()), pos2(curve_rect.right(), curve_rect.top())],
            Stroke::new(0.8 * s, self.th.step_off.gamma_multiply(0.5)),
        );
        let comp = crate::amp::compensation(kind, drive);
        let pts: Vec<Pos2> = (0..=80)
            .map(|k| {
                let x = k as f32 / 40.0 - 1.0;
                let y = shape(kind, x, drive, bias, comp) / 1.2;
                pos2(c.x + x * half, c.y - y.clamp(-1.0, 1.0) * half)
            })
            .collect();
        self.pt.add(Shape::line(pts, Stroke::new(1.6 * s, ink)));
        self.graph_legend(rect, 8.0, "CURVE", self.th.step_off);

        // Harmonic bars: odd harmonics in the section colour, even ones highlighted.
        const H: usize = 10;
        let spec = harmonic_spectrum::<H>(kind, drive, bias, 0.5);
        let bars = Rect::from_min_max(
            pos2(curve_rect.right() + 12.0 * s, curve_rect.top()),
            pos2(rect.right() - 8.0 * s, curve_rect.bottom()),
        );
        self.graph_legend(rect, side + 20.0, "HARMONICS", self.th.step_off);
        let bw = bars.width() / H as f32;
        let fund = spec[0].max(1e-6);
        for (h, level) in spec.iter().enumerate() {
            let db = 20.0 * (level / fund).max(1e-6).log10();
            let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let x = bars.left() + bw * h as f32;
            let bar = Rect::from_min_max(
                pos2(x + bw * 0.18, bars.bottom() - t * (bars.height() - 10.0 * s)),
                pos2(x + bw * 0.82, bars.bottom() - 10.0 * s),
            );
            let even = (h + 1) % 2 == 0;
            let color = if !active {
                self.th.step_off
            } else if even {
                self.th.sublabel
            } else {
                self.accent
            };
            self.pt.rect_filled(bar, CornerRadius::ZERO, color.gamma_multiply(0.85));
            self.pt.text(
                pos2(x + bw * 0.5, bars.bottom()),
                Align2::CENTER_BOTTOM,
                format!("{}", h + 1),
                self.cv.font(6.5),
                self.th.legend,
            );
        }
    }

    /// Decay display: pre-delay gap, early reflections spaced by SIZE, the full band decay
    /// (filled) and the faster high band decay from DAMP. Handles: PRE-DELAY (sideways) and
    /// DECAY (sideways) / DAMPING (up and down).
    fn reverb_graph(&mut self, p: &ReverbParams, x0: f32, w: f32) {
        let cv = self.cv;
        let s = cv.s;
        let pre_text = format!("PRE {}", self.value_text(&p.verb_predelay));
        let damp_text = format!("DAMP {}", self.value_text(&p.verb_damp));
        let dec_text = format!("DECAY {}", self.value_text(&p.verb_decay));
        self.text(cv.p(x0, LABEL1_Y), Align2::LEFT_BOTTOM, &pre_text, 9.5, self.th.text);
        self.text(cv.p(x0 + w * 0.5, LABEL1_Y), Align2::CENTER_BOTTOM, &damp_text, 9.5, self.th.text);
        self.text(cv.p(x0 + w, LABEL1_Y), Align2::RIGHT_BOTTOM, &dec_text, 9.5, self.th.text);

        let rect = cv.r(x0, LABEL1_Y + 10.0, w, 130.0);
        self.graph_bg(rect);
        let inner = rect.shrink2(vec2(8.0, 10.0) * s);
        let zone = 0.22 * inner.width();
        let dec_zone = inner.width() - zone;
        let hit = |c: Pos2| Rect::from_center_size(c, vec2(18.0, 18.0) * s);

        let pre = self.shown(&p.verb_predelay);
        let dec = self.shown(&p.verb_decay);
        let damp = self.shown(&p.verb_damp);
        let pre_x = inner.left() + pre * zone;
        let dec_x = inner.left() + zone + dec * dec_zone;
        let a_dec = self.xy_handle(
            "decay",
            hit(pos2(dec_x, inner.bottom() - damp * inner.height())),
            &p.verb_decay,
            Some(&p.verb_damp),
            Axis::Linear(dec_zone),
            inner.height(),
        );
        let a_pre = self.xy_handle("pre", hit(pos2(pre_x, inner.top())), &p.verb_predelay, None, Axis::Linear(zone), 1.0);

        // Re-read after the handles may have changed values this frame.
        let pre = self.shown(&p.verb_predelay);
        let dec = self.shown(&p.verb_decay);
        let damp = self.shown(&p.verb_damp);
        let size = self.plain(&p.verb_size);
        let active = self.shown(&p.verb_on) > 0.5;
        let ink = if active { self.accent } else { self.th.step_off };
        let pre_x = inner.left() + pre * zone;
        let dec_x = inner.left() + zone + dec * dec_zone;
        let damp_y = inner.bottom() - damp * inner.height();

        // Level axis: 0 dB at the top, -60 dB at the bottom.
        let y_of = |db: f32| inner.top() + (-db / 60.0).clamp(0.0, 1.0) * inner.height();
        let curve = |end_x: f32| -> Vec<Pos2> {
            (0..=48)
                .map(|k| {
                    let u = k as f32 / 48.0;
                    let x = pre_x + (end_x - pre_x).max(1.0) * u;
                    pos2(x, y_of(-60.0 * u))
                })
                .collect()
        };
        let full = curve(dec_x);
        self.fill_under(&full, inner.bottom(), ink.gamma_multiply(0.3));
        self.pt.add(Shape::line(full, Stroke::new(1.6 * s, ink)));
        let hf_end = pre_x + (dec_x - pre_x) * (1.0 - 0.85 * damp);
        self.pt.add(Shape::line(
            curve(hf_end),
            Stroke::new(1.0 * s, self.th.drag_line.gamma_multiply(0.6)),
        ));
        // Early reflections spaced by the room size.
        let spacing = 5.0 * size_scale(size) * s;
        for k in 0..7 {
            let x = pre_x + spacing * (k as f32 + 0.6);
            let h = (0.95 - 0.11 * k as f32) * inner.height();
            if x < dec_x {
                self.pt.line_segment(
                    [pos2(x, inner.bottom()), pos2(x, inner.bottom() - h)],
                    Stroke::new(1.0 * s, ink.gamma_multiply(0.7)),
                );
            }
        }
        // Pre-delay gap and guides
        self.pt.line_segment(
            [pos2(pre_x, inner.top()), pos2(pre_x, inner.bottom())],
            Stroke::new(1.0 * s, ink.gamma_multiply(0.6)),
        );
        self.pt.line_segment(
            [pos2(dec_x, damp_y), pos2(dec_x, inner.bottom())],
            Stroke::new(1.0 * s, self.th.drag_line.gamma_multiply(0.4)),
        );
        self.handle_square(pos2(pre_x, inner.top()), a_pre);
        self.handle_square(pos2(dec_x, damp_y), a_dec);
    }

    /// Echo pattern of the ping-pong delay: left echoes above the line, right ones below,
    /// each scaled by the feedback.
    fn echo_graph(&mut self, p: &ReverbParams, x0: f32, w: f32) {
        let cv = self.cv;
        let s = cv.s;
        let sync = self.shown(&p.pp_sync) > 0.5;
        let division = self.plain(&p.pp_division);
        let time = self.plain(&p.pp_time);
        let tempo = self.st.visuals.tempo.load(Ordering::Relaxed);
        let secs = delay_seconds(sync, division, time, tempo);
        let fb = self.plain(&p.pp_feedback);
        let level = self.plain(&p.pp_level);
        let send = self.plain(&p.pp_send);
        let active = self.shown(&p.pp_on) > 0.5;
        let ink = if active { self.accent } else { self.th.step_off };

        let left = if sync { self.value_text(&p.pp_division) } else { "FREE".to_owned() };
        self.text(cv.p(x0, LABEL1_Y), Align2::LEFT_BOTTOM, &left, 9.5, self.th.text);
        self.text(
            cv.p(x0 + w, LABEL1_Y),
            Align2::RIGHT_BOTTOM,
            &format!("{:.0} ms", secs * 1000.0),
            9.5,
            self.th.text,
        );

        let rect = cv.r(x0, LABEL1_Y + 10.0, w, 130.0);
        self.graph_bg(rect);
        let inner = rect.shrink2(vec2(8.0, 14.0) * s);
        let mid = inner.center().y;
        let half = inner.height() * 0.5;
        self.pt.line_segment(
            [pos2(inner.left(), mid), pos2(inner.right(), mid)],
            Stroke::new(0.8 * s, self.th.scale),
        );
        self.pt.text(pos2(inner.left(), inner.top()), Align2::LEFT_TOP, "L", cv.font(8.0), self.th.step_off);
        self.pt.text(pos2(inner.left(), inner.bottom()), Align2::LEFT_BOTTOM, "R", cv.font(8.0), self.th.step_off);

        const ECHOES: usize = 8;
        let px_per_echo = (inner.width() - 12.0 * s) / ECHOES as f32;
        // Input pulse
        self.pt.line_segment(
            [pos2(inner.left() + 8.0 * s, mid - half * 0.5), pos2(inner.left() + 8.0 * s, mid + half * 0.5)],
            Stroke::new(2.0 * s, self.th.step_off),
        );
        // Reverb smear fed by the echoes
        if send > 0.01 {
            let pts: Vec<Pos2> = (0..=40)
                .map(|k| {
                    let u = k as f32 / 40.0;
                    let x = inner.left() + 8.0 * s + px_per_echo + u * (inner.width() - 8.0 * s - px_per_echo);
                    pos2(x, mid - half * send * 0.5 * (-3.0 * u).exp())
                })
                .collect();
            let mirrored: Vec<Pos2> = pts.iter().map(|q| pos2(q.x, 2.0 * mid - q.y)).collect();
            self.fill_under(&pts, mid, ink.gamma_multiply(0.12));
            self.fill_under(&mirrored, mid, ink.gamma_multiply(0.12));
        }
        let mut amp = level;
        for n in 0..ECHOES {
            let x = inner.left() + 8.0 * s + px_per_echo * (n + 1) as f32;
            let h = half * amp.clamp(0.0, 1.0);
            let (a, b) = if n % 2 == 0 { (mid - h, mid) } else { (mid, mid + h) };
            self.pt.line_segment([pos2(x, a), pos2(x, b)], Stroke::new(3.0 * s, ink));
            amp *= fb;
        }
    }

    /// Detected key, the pitch class histogram and a keyboard. Click a key to set the manual
    /// root; the root in use is marked.
    fn key_panel(&mut self, p: &ReverbParams, rect: Rect) {
        let cv = self.cv;
        let s = cv.s;
        let th = self.th;
        self.pt.rect_filled(rect.expand(3.0 * s), CornerRadius::same((3.0 * s) as u8), th.oled_frame);
        self.pt.rect_filled(rect, CornerRadius::ZERO, th.oled_bg);
        let v = &self.st.visuals;
        let key_root = v.key_root.load(Ordering::Relaxed);
        let minor = v.key_minor.load(Ordering::Relaxed);
        let score = v.key_score.load(Ordering::Relaxed);
        let held = v.held.load(Ordering::Relaxed);
        let hist: [f32; 12] = std::array::from_fn(|i| v.hist[i].load(Ordering::Relaxed));
        let source = self.plain(&p.harm_source);
        let manual = self.plain(&p.harm_root) as usize;
        let octave = self.plain(&p.harm_octave);
        let used = match source {
            HarmSource::Manual => manual,
            _ => self.st.visuals.root.load(Ordering::Relaxed) as usize % 12,
        };
        let f0 = harmonic::fundamental_hz(used as u8, octave as u8);

        let at = |x: f32, y: f32| rect.left_top() + vec2(x, y) * s;
        let txt = |pos: Pos2, text: &str, size: f32, color: Color32| {
            self.pt.text(pos, Align2::LEFT_TOP, text, cv.mono(size), color);
        };
        txt(at(6.0, 5.0), "DETECTED KEY", 7.0, th.oled_dim);
        let key_text = if key_root >= 0 {
            harmonic::Key { root: key_root as u8, minor, score }.name()
        } else {
            "--".to_owned()
        };
        txt(at(6.0, 15.0), &key_text, 13.0, th.oled_ink);
        if key_root >= 0 {
            txt(at(118.0, 5.0), &format!("{:>3.0}%", (score.max(0.0) * 100.0)), 7.0, th.oled_dim);
        }
        let root_text = format!("ROOT {}{}  {:.1} HZ", NOTE_NAMES[used], octave, f0);
        txt(at(6.0, 33.0), &root_text, 7.5, th.oled_ink.gamma_multiply(0.8));

        // Keyboard: 7 white keys across, 5 black keys on top.
        let kb = Rect::from_min_max(at(6.0, 84.0), at(rect.width() / s - 6.0, 124.0));
        let white_w = kb.width() / 7.0;
        const WHITE: [usize; 7] = [0, 2, 4, 5, 7, 9, 11];
        const BLACK: [(usize, f32); 5] = [(1, 1.0), (3, 2.0), (6, 4.0), (8, 5.0), (10, 6.0)];
        let key_rect = |pc: usize| -> Rect {
            if let Some(i) = WHITE.iter().position(|w| *w == pc) {
                Rect::from_min_size(pos2(kb.left() + i as f32 * white_w, kb.top()), vec2(white_w, kb.height()))
            } else {
                let (_, b) = BLACK.iter().find(|(p, _)| *p == pc).unwrap();
                let w = white_w * 0.62;
                Rect::from_min_size(
                    pos2(kb.left() + b * white_w - w * 0.5, kb.top()),
                    vec2(w, kb.height() * 0.6),
                )
            }
        };
        let id = Id::new("keyboard");
        let resp = self.ui.interact(kb, id, Sense::click());
        self.register(p.harm_root.name(), ControlKind::Keyboard, kb);
        let under = resp.hover_pos().map(|ptr| {
            BLACK
                .iter()
                .map(|(pc, _)| *pc)
                .find(|pc| key_rect(*pc).contains(ptr))
                .or_else(|| WHITE.iter().copied().find(|pc| key_rect(*pc).contains(ptr)))
                .unwrap_or(0)
        });
        if resp.clicked() {
            if let Some(pc) = resp.interact_pointer_pos().map(|ptr| {
                BLACK
                    .iter()
                    .map(|(pc, _)| *pc)
                    .find(|pc| key_rect(*pc).contains(ptr))
                    .or_else(|| WHITE.iter().copied().find(|pc| key_rect(*pc).contains(ptr)))
                    .unwrap_or(0)
            }) {
                self.change(&p.harm_root, pc as f32 / 11.0);
            }
        }
        if let Some(pc) = under {
            self.set_focus("MANUAL ROOT (CLICK)", NOTE_NAMES[pc].to_owned());
        }
        let manual = self.plain(&p.harm_root) as usize;

        // Histogram bars above the keys
        let max = hist.iter().fold(1e-6f32, |m, v| m.max(*v));
        for pc in 0..12 {
            let kr = key_rect(pc);
            let h = hist[pc] / max * 30.0 * s;
            let bar = Rect::from_min_max(pos2(kr.center().x - 2.5 * s, kb.top() - 3.0 * s - h), pos2(kr.center().x + 2.5 * s, kb.top() - 3.0 * s));
            self.pt.rect_filled(bar, CornerRadius::ZERO, th.oled_ink.gamma_multiply(0.7));
        }
        let draw_key = |g: &Self, pc: usize, black: bool| {
            let kr = key_rect(pc);
            let lit = held & (1 << pc) != 0;
            let fill = if lit {
                g.accent
            } else if black {
                th.oled_bg
            } else {
                th.oled_dim.gamma_multiply(0.35)
            };
            g.pt.rect_filled(kr.shrink(0.5 * s), CornerRadius::same((1.0 * s) as u8), fill);
            g.pt.rect_stroke(kr.shrink(0.5 * s), CornerRadius::same((1.0 * s) as u8), Stroke::new(0.8 * s, th.oled_dim), StrokeKind::Inside);
            if under == Some(pc) {
                g.pt.rect_stroke(kr, CornerRadius::ZERO, Stroke::new(1.2 * s, th.drag_line), StrokeKind::Inside);
            }
            let marker_y = if black { kr.bottom() - 5.0 * s } else { kr.bottom() - 6.0 * s };
            if pc == used {
                g.pt.circle_filled(pos2(kr.center().x, marker_y), 3.0 * s, th.sublabel);
            } else if pc == manual && source == HarmSource::Manual {
                g.pt.circle_stroke(pos2(kr.center().x, marker_y), 3.0 * s, Stroke::new(1.0 * s, th.sublabel));
            }
        };
        for pc in WHITE {
            draw_key(self, pc, false);
        }
        for (pc, _) in BLACK {
            draw_key(self, pc, true);
        }
    }

    /// Settings of the harmonic EQ as currently displayed, with the root in use.
    fn harm_settings(&mut self, p: &ReverbParams) -> (HarmSettings, f32) {
        let source = self.plain(&p.harm_source);
        let manual = self.plain(&p.harm_root) as u8;
        let root = match source {
            HarmSource::Manual => manual,
            _ => self.st.visuals.root.load(Ordering::Relaxed) % 12,
        };
        let s = HarmSettings {
            on: self.shown(&p.harm_on) > 0.5,
            source,
            root: manual,
            octave: self.plain(&p.harm_octave) as u8,
            mode: self.plain(&p.harm_mode),
            select: self.plain(&p.harm_select),
            q: self.plain(&p.harm_q),
            gain_db: self.plain(&p.harm_gain),
            count: self.plain(&p.harm_count) as usize,
            mix: self.plain(&p.harm_mix),
        };
        let f0 = harmonic::fundamental_hz(root, s.octave);
        (s, f0)
    }

    /// Response of the harmonic EQ, with a tick at every band.
    fn harm_graph(&mut self, p: &ReverbParams, x0: f32, w: f32) {
        let cv = self.cv;
        let s = cv.s;
        let (hs, f0) = self.harm_settings(p);
        let mode = if hs.mode == HarmMode::Peak { "PEAK" } else { "ISOLATE" };
        self.text(cv.p(x0, LABEL2_Y), Align2::LEFT_BOTTOM, &format!("F0 {f0:.1} Hz"), 9.5, self.th.text);
        self.text(cv.p(x0 + w, LABEL2_Y), Align2::RIGHT_BOTTOM, mode, 9.5, self.th.text);

        let rect = cv.r(x0, LABEL2_Y + 10.0, w, 130.0);
        self.graph_bg(rect);
        let inner = rect.shrink2(vec2(6.0, 8.0) * s);
        self.freq_grid(inner);
        // dB axis: +20 at the top, -40 at the bottom.
        let y_of = |db: f32| inner.top() + ((20.0 - db) / 60.0).clamp(0.0, 1.0) * inner.height();
        self.pt.line_segment(
            [pos2(inner.left(), y_of(0.0)), pos2(inner.right(), y_of(0.0))],
            Stroke::new(0.8 * s, self.th.step_off.gamma_multiply(0.6)),
        );

        let mut coefs = [SvfCoefs::default(); MAX_HARMONICS];
        let n = harmonic::band_coefs(&hs, f0, GRAPH_SR, &mut coefs);
        let ink = if hs.on { self.accent } else { self.th.step_off };
        // A log grid plus the exact band centres, so narrow peaks are never missed.
        let mut freqs: Vec<f32> = (0..=240).map(|i| t_freq(i as f32 / 240.0)).collect();
        for i in 0..n {
            let hz = f0 * harmonic::harmonic_number(hs.select, i) as f32;
            freqs.push(hz);
            self.pt.line_segment(
                [pos2(inner.left() + inner.width() * freq_t(hz), inner.bottom() - 4.0 * s), pos2(inner.left() + inner.width() * freq_t(hz), inner.bottom())],
                Stroke::new(1.0 * s, ink),
            );
        }
        freqs.retain(|f| (F_MIN..=F_MAX).contains(f));
        freqs.sort_by(f32::total_cmp);
        let pts: Vec<Pos2> = freqs
            .iter()
            .map(|hz| {
                let db = if hs.on { harmonic::response_db(&hs, &coefs[..n], *hz, GRAPH_SR) } else { 0.0 };
                pos2(inner.left() + inner.width() * freq_t(*hz), y_of(db))
            })
            .collect();
        self.fill_under(&pts, y_of(if hs.mode == HarmMode::Peak { 0.0 } else { -40.0 }), ink.gamma_multiply(0.25));
        self.pt.add(Shape::line(pts, Stroke::new(1.4 * s, ink)));
        let q = self.value_text(&p.harm_q);
        let gain = self.value_text(&p.harm_gain);
        self.graph_legend(rect, 6.0, &format!("Q {q} · {gain}"), self.th.step_off);
    }

    /// Low / high cut response with a handle at each cutoff (sideways) that also sets the
    /// shared resonance (up and down).
    fn cut_graph(&mut self, p: &ReverbParams, x0: f32, w: f32) {
        let cv = self.cv;
        let s = cv.s;
        let low_text = format!("LOW {}", self.value_text(&p.filt_low));
        let high_text = format!("HIGH {}", self.value_text(&p.filt_high));
        self.text(cv.p(x0, LABEL2_Y), Align2::LEFT_BOTTOM, &low_text, 9.5, self.th.text);
        self.text(cv.p(x0 + w, LABEL2_Y), Align2::RIGHT_BOTTOM, &high_text, 9.5, self.th.text);

        let rect = cv.r(x0, LABEL2_Y + 10.0, w, 130.0);
        let inner = rect.shrink2(vec2(6.0, 8.0) * s);
        let hit = |c: Pos2| Rect::from_center_size(c, vec2(18.0, 18.0) * s);
        let x_of = |hz: f32| inner.left() + inner.width() * freq_t(hz);
        let reso = self.shown(&p.filt_reso);
        let handle_y = inner.bottom() - (0.25 + 0.6 * reso) * inner.height();
        let low = self.plain(&p.filt_low);
        let high = self.plain(&p.filt_high);
        let a_high = self.xy_handle(
            "high",
            hit(pos2(x_of(high), handle_y)),
            &p.filt_high,
            Some(&p.filt_reso),
            Axis::LogHz(inner.width()),
            0.6 * inner.height(),
        );
        let a_low = self.xy_handle(
            "low",
            hit(pos2(x_of(low), handle_y)),
            &p.filt_low,
            Some(&p.filt_reso),
            Axis::LogHz(inner.width()),
            0.6 * inner.height(),
        );

        let settings = CutSettings {
            on: self.shown(&p.filt_on) > 0.5,
            low_hz: self.plain(&p.filt_low),
            high_hz: self.plain(&p.filt_high),
            slope: self.plain(&p.filt_slope),
            resonance: self.plain(&p.filt_reso),
        };
        self.graph_bg(rect);
        self.freq_grid(inner);
        // dB axis: +18 at the top, -48 at the bottom.
        let y_of = |db: f32| inner.top() + ((18.0 - db) / 66.0).clamp(0.0, 1.0) * inner.height();
        self.pt.line_segment(
            [pos2(inner.left(), y_of(0.0)), pos2(inner.right(), y_of(0.0))],
            Stroke::new(0.8 * s, self.th.step_off.gamma_multiply(0.6)),
        );
        let ink = if settings.on { self.accent } else { self.th.step_off };
        let pts: Vec<Pos2> = (0..=160)
            .map(|i| {
                let t = i as f32 / 160.0;
                let db = if settings.on { cut::response_db(&settings, t_freq(t), GRAPH_SR) } else { 0.0 };
                pos2(inner.left() + inner.width() * t, y_of(db))
            })
            .collect();
        self.fill_under(&pts, inner.bottom(), ink.gamma_multiply(0.28));
        self.pt.add(Shape::line(pts, Stroke::new(1.8 * s, ink)));
        let (lx, hx) = (x_of(settings.low_hz), x_of(settings.high_hz));
        let hy = inner.bottom() - (0.25 + 0.6 * settings.resonance) * inner.height();
        for x in [lx, hx] {
            self.pt.line_segment(
                [pos2(x, inner.top()), pos2(x, inner.bottom())],
                Stroke::new(1.0 * s, ink.gamma_multiply(0.6)),
            );
        }
        self.handle_square(pos2(lx, hy), a_low);
        self.handle_square(pos2(hx, hy), a_high);
    }
}

/// Sends all values of a preset. Returns (parameter name, normalized value) for every change.
fn apply_preset(p: &ReverbParams, s: &ParamSetter, index: usize) -> Vec<(String, f32)> {
    let preset = &PRESETS[index];
    let mut sent = Vec::new();
    for (id, ptr, _) in p.param_map() {
        let value = preset.values.iter().find(|(pid, _)| *pid == id).map(|(_, v)| *v);
        // Leave the output level alone unless the preset sets it.
        if value.is_none() && id == "out_gain" {
            continue;
        }
        // SAFETY: the pointers come from our own params object, which outlives the editor.
        unsafe {
            let normalized = match value {
                Some(v) => ptr.preview_normalized(v),
                None => ptr.default_normalized_value(),
            };
            s.raw_context.raw_begin_set_parameter(ptr);
            s.raw_context.raw_set_parameter_normalized(ptr, normalized);
            s.raw_context.raw_end_set_parameter(ptr);
            sent.push((ptr.name().to_owned(), normalized));
        }
    }
    sent
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
