use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use crate::harmonic::NOTE_NAMES;

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmpType {
    #[name = "Tube"]
    Tube,
    #[name = "Tape"]
    Tape,
    #[name = "Fuzz"]
    Fuzz,
    #[name = "Fold"]
    Fold,
}

/// Note values for the tempo synced ping-pong delay.
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Division {
    #[name = "1/32"]
    ThirtySecond,
    #[name = "1/16 T"]
    SixteenthTriplet,
    #[name = "1/16"]
    Sixteenth,
    #[name = "1/16 D"]
    SixteenthDotted,
    #[name = "1/8 T"]
    EighthTriplet,
    #[name = "1/8"]
    Eighth,
    #[name = "1/8 D"]
    EighthDotted,
    #[name = "1/4 T"]
    QuarterTriplet,
    #[name = "1/4"]
    Quarter,
    #[name = "1/4 D"]
    QuarterDotted,
    #[name = "1/2"]
    Half,
    #[name = "1/2 D"]
    HalfDotted,
    #[name = "1/1"]
    Whole,
}

impl Division {
    /// Length in quarter notes.
    pub fn beats(self) -> f32 {
        match self {
            Division::ThirtySecond => 0.125,
            Division::SixteenthTriplet => 0.25 * 2.0 / 3.0,
            Division::Sixteenth => 0.25,
            Division::SixteenthDotted => 0.375,
            Division::EighthTriplet => 0.5 * 2.0 / 3.0,
            Division::Eighth => 0.5,
            Division::EighthDotted => 0.75,
            Division::QuarterTriplet => 2.0 / 3.0,
            Division::Quarter => 1.0,
            Division::QuarterDotted => 1.5,
            Division::Half => 2.0,
            Division::HalfDotted => 3.0,
            Division::Whole => 4.0,
        }
    }
}

/// Where the harmonic EQ takes its root note from.
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarmSource {
    /// Key detected from the MIDI notes.
    #[name = "Key"]
    Key,
    /// Lowest held MIDI note.
    #[name = "Note"]
    Note,
    #[name = "Manual"]
    Manual,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarmMode {
    /// Peaking bands: boost or cut the harmonics.
    #[name = "Peak"]
    Peak,
    /// Bandpasses: only the harmonics pass.
    #[name = "Isolate"]
    Isolate,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarmSelect {
    #[name = "All"]
    All,
    #[name = "Even"]
    Even,
    #[name = "Odd"]
    Odd,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slope {
    #[name = "12 dB"]
    Db12,
    #[name = "24 dB"]
    Db24,
}

#[derive(Params)]
pub struct ReverbParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,
    /// Selected GUI colour scheme (index into `theme::THEMES`).
    #[persist = "colour-theme"]
    pub theme: Arc<AtomicU8>,

    // Amp
    #[id = "amp_on"]
    pub amp_on: BoolParam,
    #[id = "amp_type"]
    pub amp_type: EnumParam<AmpType>,
    #[id = "amp_drive"]
    pub amp_drive: FloatParam,
    #[id = "amp_bias"]
    pub amp_bias: FloatParam,
    #[id = "amp_tone"]
    pub amp_tone: FloatParam,
    #[id = "amp_mix"]
    pub amp_mix: FloatParam,
    #[id = "amp_level"]
    pub amp_level: FloatParam,

    // Reverb
    #[id = "verb_on"]
    pub verb_on: BoolParam,
    #[id = "verb_mix"]
    pub verb_mix: FloatParam,
    #[id = "verb_predelay"]
    pub verb_predelay: FloatParam,
    #[id = "verb_size"]
    pub verb_size: FloatParam,
    #[id = "verb_decay"]
    pub verb_decay: FloatParam,
    #[id = "verb_damp"]
    pub verb_damp: FloatParam,
    #[id = "verb_diffusion"]
    pub verb_diffusion: FloatParam,
    #[id = "verb_mod"]
    pub verb_mod: FloatParam,
    #[id = "verb_width"]
    pub verb_width: FloatParam,

    // Ping-pong delay
    #[id = "pp_on"]
    pub pp_on: BoolParam,
    #[id = "pp_sync"]
    pub pp_sync: BoolParam,
    #[id = "pp_division"]
    pub pp_division: EnumParam<Division>,
    #[id = "pp_time"]
    pub pp_time: FloatParam,
    #[id = "pp_feedback"]
    pub pp_feedback: FloatParam,
    #[id = "pp_level"]
    pub pp_level: FloatParam,
    #[id = "pp_send"]
    pub pp_send: FloatParam,

    // Harmonic EQ
    #[id = "harm_on"]
    pub harm_on: BoolParam,
    #[id = "harm_source"]
    pub harm_source: EnumParam<HarmSource>,
    #[id = "harm_root"]
    pub harm_root: IntParam,
    #[id = "harm_octave"]
    pub harm_octave: IntParam,
    #[id = "harm_mode"]
    pub harm_mode: EnumParam<HarmMode>,
    #[id = "harm_select"]
    pub harm_select: EnumParam<HarmSelect>,
    #[id = "harm_q"]
    pub harm_q: FloatParam,
    #[id = "harm_gain"]
    pub harm_gain: FloatParam,
    #[id = "harm_count"]
    pub harm_count: IntParam,
    #[id = "harm_mix"]
    pub harm_mix: FloatParam,

    // Cut filter
    #[id = "filt_on"]
    pub filt_on: BoolParam,
    #[id = "filt_low"]
    pub filt_low: FloatParam,
    #[id = "filt_high"]
    pub filt_high: FloatParam,
    #[id = "filt_slope"]
    pub filt_slope: EnumParam<Slope>,
    #[id = "filt_reso"]
    pub filt_reso: FloatParam,

    // Output
    #[id = "out_mix"]
    pub out_mix: FloatParam,
    #[id = "out_gain"]
    pub out_gain: FloatParam,
}

fn percent(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_unit(" %")
        .with_value_to_string(formatters::v2s_f32_percentage(0))
        .with_string_to_value(formatters::s2v_f32_percentage())
}

fn decibels(name: &str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min, max })
        .with_unit(" dB")
        .with_step_size(0.1)
}

fn hertz(name: &str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Skewed { min, max, factor: FloatRange::skew_factor(-2.0) })
        .with_value_to_string(formatters::v2s_f32_hz_then_khz(1))
        .with_string_to_value(formatters::s2v_f32_hz_then_khz())
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(crate::editor::WIDTH, crate::editor::HEIGHT),
            theme: Arc::new(AtomicU8::new(crate::theme::DEFAULT_THEME)),

            amp_on: BoolParam::new("Amp", true),
            amp_type: EnumParam::new("Amp Type", AmpType::Tube),
            amp_drive: decibels("Drive", 9.0, 0.0, 36.0),
            amp_bias: FloatParam::new("Bias", 0.25, FloatRange::Linear { min: -1.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            amp_tone: hertz("Tone", 9000.0, 800.0, 20_000.0),
            amp_mix: percent("Amp Mix", 1.0),
            amp_level: decibels("Amp Level", 0.0, -24.0, 12.0),

            verb_on: BoolParam::new("Reverb", true),
            verb_mix: percent("Reverb Mix", 0.35),
            verb_predelay: FloatParam::new(
                "Pre-Delay",
                20.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: crate::reverb::MAX_PREDELAY_MS,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_unit(" ms")
            .with_step_size(0.1),
            verb_size: percent("Size", 0.6),
            verb_decay: FloatParam::new(
                "Decay",
                2.5,
                FloatRange::Skewed { min: 0.2, max: 20.0, factor: FloatRange::skew_factor(-2.0) },
            )
            .with_unit(" s")
            .with_step_size(0.01),
            verb_damp: percent("Damping", 0.4),
            verb_diffusion: percent("Diffusion", 0.7),
            verb_mod: percent("Modulation", 0.3),
            verb_width: percent("Width", 1.0),

            pp_on: BoolParam::new("Ping-Pong", false),
            pp_sync: BoolParam::new("Tempo Sync", true),
            pp_division: EnumParam::new("Delay Division", Division::EighthDotted),
            pp_time: FloatParam::new(
                "Delay Time",
                375.0,
                FloatRange::Skewed { min: 10.0, max: 2000.0, factor: FloatRange::skew_factor(-1.5) },
            )
            .with_unit(" ms")
            .with_step_size(0.1),
            pp_feedback: FloatParam::new("Feedback", 0.45, FloatRange::Linear { min: 0.0, max: 0.95 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            pp_level: percent("Echo Level", 0.5),
            pp_send: percent("Echo to Reverb", 0.5),

            harm_on: BoolParam::new("Harmonic EQ", true),
            harm_source: EnumParam::new("Root Source", HarmSource::Key),
            harm_root: IntParam::new("Manual Root", 9, IntRange::Linear { min: 0, max: 11 })
                .with_value_to_string(Arc::new(|v| NOTE_NAMES[v.clamp(0, 11) as usize].to_owned()))
                .with_string_to_value(Arc::new(|s| {
                    let s = s.trim().to_uppercase();
                    NOTE_NAMES.iter().position(|n| *n == s).map(|i| i as i32)
                })),
            harm_octave: IntParam::new("Root Octave", 2, IntRange::Linear { min: 0, max: 5 }),
            harm_mode: EnumParam::new("Harmonic Mode", HarmMode::Peak),
            harm_select: EnumParam::new("Harmonics", HarmSelect::All),
            harm_q: FloatParam::new(
                "Harmonic Q",
                24.0,
                FloatRange::Skewed { min: 1.0, max: 200.0, factor: FloatRange::skew_factor(-2.0) },
            )
            .with_step_size(0.1),
            harm_gain: decibels("Harmonic Gain", 6.0, -30.0, 18.0),
            harm_count: IntParam::new("Harmonic Count", 12, IntRange::Linear { min: 1, max: 32 }),
            harm_mix: percent("Harmonic Mix", 1.0),

            filt_on: BoolParam::new("Filter", true),
            filt_low: hertz("Low Cut", 40.0, 20.0, 2000.0),
            filt_high: hertz("High Cut", 16_000.0, 1000.0, 20_000.0),
            filt_slope: EnumParam::new("Filter Slope", Slope::Db12),
            filt_reso: percent("Filter Resonance", 0.0),

            out_mix: percent("Dry/Wet", 1.0),
            out_gain: decibels("Output", 0.0, -24.0, 12.0),
        }
    }
}
