use nih_plug::prelude::*;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

pub mod amp;
pub mod dsp;
pub mod editor;
pub mod engine;
pub mod filter;
pub mod harmonic;
pub mod params;
pub mod pingpong;
pub mod presets;
pub mod reverb;
pub mod theme;
pub mod visuals;

use amp::AmpSettings;
use engine::{Engine, Settings, LATENCY};
use filter::CutSettings;
use harmonic::HarmSettings;
use params::ReverbParams;
use pingpong::{delay_seconds, PingPongSettings};
use reverb::ReverbSettings;
use visuals::{ScopeInput, ScopeOutput, VisualState};

pub struct Lemma {
    params: Arc<ReverbParams>,
    engine: Engine,
    /// Audio-to-editor link: meters and key detector state, plus the scope.
    visuals: Arc<VisualState>,
    scope: ScopeInput,
    scope_output: Arc<Mutex<ScopeOutput>>,
}

impl Default for Lemma {
    fn default() -> Self {
        let (scope, scope_output) = ScopeInput::new();
        Self {
            params: Arc::new(ReverbParams::default()),
            engine: Engine::new(48_000.0),
            visuals: Arc::new(VisualState::default()),
            scope,
            scope_output: Arc::new(Mutex::new(scope_output)),
        }
    }
}

/// Snapshot of all parameters for one block. `tempo` is the host tempo in BPM.
pub fn settings_from_params(p: &ReverbParams, tempo: f32) -> Settings {
    Settings {
        amp: AmpSettings {
            on: p.amp_on.value(),
            kind: p.amp_type.value(),
            drive: util::db_to_gain(p.amp_drive.value()),
            bias: p.amp_bias.value(),
            tone_hz: p.amp_tone.value(),
            mix: p.amp_mix.value(),
            level: util::db_to_gain(p.amp_level.value()),
        },
        verb_on: p.verb_on.value(),
        verb_mix: p.verb_mix.value(),
        verb_width: p.verb_width.value(),
        reverb: ReverbSettings {
            predelay_ms: p.verb_predelay.value(),
            size: p.verb_size.value(),
            decay_s: p.verb_decay.value(),
            damp: p.verb_damp.value(),
            diffusion: p.verb_diffusion.value(),
            modulation: p.verb_mod.value(),
        },
        pp_on: p.pp_on.value(),
        pp: PingPongSettings {
            seconds: delay_seconds(p.pp_sync.value(), p.pp_division.value(), p.pp_time.value(), tempo),
            feedback: p.pp_feedback.value(),
        },
        pp_level: p.pp_level.value(),
        pp_send: p.pp_send.value(),
        harm: HarmSettings {
            on: p.harm_on.value(),
            source: p.harm_source.value(),
            root: p.harm_root.value() as u8,
            octave: p.harm_octave.value() as u8,
            mode: p.harm_mode.value(),
            select: p.harm_select.value(),
            q: p.harm_q.value(),
            gain_db: p.harm_gain.value(),
            count: p.harm_count.value() as usize,
            mix: p.harm_mix.value(),
        },
        cut: CutSettings {
            on: p.filt_on.value(),
            low_hz: p.filt_low.value(),
            high_hz: p.filt_high.value(),
            slope: p.filt_slope.value(),
            resonance: p.filt_reso.value(),
        },
        out_mix: p.out_mix.value(),
        out_gain: util::db_to_gain(p.out_gain.value()),
    }
}

impl Plugin for Lemma {
    const NAME: &'static str = "Lemma";
    const VENDOR: &'static str = "Fonsie Audio";
    const URL: &'static str = "https://example.com/lemma";
    const EMAIL: &'static str = "beat.weber@bithawk.ch";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    /// Notes feed the key detector of the harmonic EQ.
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.visuals.clone(), self.scope_output.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        let sr = buffer_config.sample_rate;
        // The delay memory depends on the sample rate; allocating here is allowed.
        if self.engine.sample_rate() != sr {
            self.engine = Engine::new(sr);
        }
        self.scope.set_sample_rate(sr);
        let tempo = self.visuals.tempo.load(Ordering::Relaxed);
        self.engine.init(&settings_from_params(&self.params, tempo));
        context.set_latency_samples(LATENCY as u32);
        true
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.scope.reset();
        self.visuals.clear();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        while let Some(event) = context.next_event() {
            match event {
                NoteEvent::NoteOn { note, velocity, .. } => self.engine.note_on(note, velocity),
                NoteEvent::NoteOff { note, .. } => self.engine.note_off(note),
                NoteEvent::Choke { note, .. } => self.engine.note_off(note),
                NoteEvent::MidiCC { cc: 120 | 123, .. } => self.engine.all_notes_off(),
                _ => {}
            }
        }

        let tempo = context.transport().tempo.unwrap_or(120.0) as f32;
        self.visuals.tempo.store(tempo, Ordering::Relaxed);
        let settings = settings_from_params(&self.params, tempo);

        let out = buffer.as_slice();
        let (left, right) = out.split_at_mut(1);
        let (left, right) = (&mut *left[0], &mut *right[0]);
        self.engine.process(left, right, &settings);

        // Only pay for visualisation when someone is looking.
        if self.params.editor_state.is_open() {
            self.engine.publish(&self.visuals);
            for (l, r) in left.iter().zip(right.iter()) {
                self.scope.push(0.5 * (l + r));
            }
        }

        // The reverb and the echoes ring on without input.
        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Lemma {
    const CLAP_ID: &'static str = "ch.fonsie.lemma";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Harmonic amp, reverb with ping-pong delay, key tracking harmonic EQ and filter");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Reverb,
        ClapFeature::Delay,
        ClapFeature::Distortion,
        ClapFeature::Equalizer,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for Lemma {
    const VST3_CLASS_ID: [u8; 16] = *b"FonsieAudioLemma";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[
        Vst3SubCategory::Fx,
        Vst3SubCategory::Reverb,
        Vst3SubCategory::Delay,
        Vst3SubCategory::Stereo,
    ];
}

nih_export_clap!(Lemma);
nih_export_vst3!(Lemma);
