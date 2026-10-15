//! Section 1: amp / distortion. Four waveshapers run at 4x oversampling. BIAS shifts the
//! operating point so the curve becomes asymmetric, which adds even harmonics; the symmetric
//! curves alone only make odd ones.

use crate::dsp::{onepole_coef, tanh, smooth_coef, DcBlock, DelayLine, Oversampler, OS, OS_LATENCY};
use crate::params::AmpType;

/// Shaper curve without bias. Used by the GUI for the transfer curve and harmonic preview.
#[inline]
pub fn curve(kind: AmpType, x: f32) -> f32 {
    match kind {
        // Triode-like: soft on the positive side, harder on the negative side.
        AmpType::Tube => {
            if x >= 0.0 {
                tanh(x)
            } else {
                tanh(1.6 * x) / 1.6
            }
        }
        // Tape: smooth symmetric saturation with a gentle knee.
        AmpType::Tape => x / (1.0 + x * x).sqrt(),
        // Fuzz: near hard clipping with a short knee.
        AmpType::Fuzz => {
            let x2 = x * x;
            x / (1.0 + x2 * x2).sqrt().sqrt()
        }
        // Sine wave folder: folds back above full scale.
        AmpType::Fold => (x * std::f32::consts::FRAC_PI_2).sin(),
    }
}

/// Shaper with drive, bias and output compensation as the audio path applies it.
#[inline]
pub fn shape(kind: AmpType, x: f32, drive: f32, bias: f32, comp: f32) -> f32 {
    shape_with_offset(kind, x, drive, bias * 0.8, curve(kind, bias * 0.8), comp)
}

/// [`shape`] with the bias offset `b` and its curve value precomputed. The offset is removed
/// again after the curve, so only the asymmetry remains.
#[inline]
fn shape_with_offset(kind: AmpType, x: f32, drive: f32, b: f32, b_out: f32, comp: f32) -> f32 {
    (curve(kind, x * drive + b) - b_out) * comp
}

/// Output compensation for a drive gain: a -12 dB sine comes out at roughly the same level.
pub fn compensation(kind: AmpType, drive: f32) -> f32 {
    if kind == AmpType::Fold {
        // The folder's output never grows past full scale, so plain inverse drive is too much.
        return 1.0 / drive.sqrt().max(1.0);
    }
    let r = 0.25;
    let y = curve(kind, r * drive).abs().max(1e-4);
    (r / y).clamp(0.05, 1.5)
}

#[derive(Clone, Copy)]
pub struct AmpSettings {
    pub on: bool,
    pub kind: AmpType,
    /// Linear gain into the shaper.
    pub drive: f32,
    /// -1..1
    pub bias: f32,
    pub tone_hz: f32,
    pub mix: f32,
    /// Linear output gain.
    pub level: f32,
}

struct Channel {
    os: Oversampler,
    bypass: DelayLine,
    tone: f32,
    dc: DcBlock,
}

pub struct Amp {
    ch: [Channel; 2],
    sample_rate: f32,
    smooth: f32,
    drive: f32,
    bias: f32,
    mix: f32,
    level: f32,
    comp: f32,
    /// 1 = amp in circuit, 0 = bypassed (crossfades on switching).
    active: f32,
    /// Peak of the shaper input, for the GUI drive LED.
    pub drive_peak: f32,
}

impl Amp {
    pub fn new() -> Self {
        let ch = || Channel {
            os: Oversampler::new(),
            bypass: DelayLine::new(OS_LATENCY + 1),
            tone: 0.0,
            dc: DcBlock::default(),
        };
        Self {
            ch: [ch(), ch()],
            sample_rate: 48_000.0,
            smooth: 0.01,
            drive: 1.0,
            bias: 0.0,
            mix: 1.0,
            level: 1.0,
            comp: 1.0,
            active: 1.0,
            drive_peak: 0.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.smooth = smooth_coef(0.02, sr);
    }

    /// Jump to the settings without smoothing.
    pub fn init(&mut self, s: &AmpSettings) {
        self.drive = s.drive;
        self.bias = s.bias;
        self.mix = s.mix;
        self.level = s.level;
        self.comp = compensation(s.kind, s.drive);
        self.active = if s.on { 1.0 } else { 0.0 };
    }

    pub fn reset(&mut self) {
        for c in &mut self.ch {
            c.os.reset();
            c.bypass.reset();
            c.tone = 0.0;
            c.dc.reset();
        }
        self.drive_peak = 0.0;
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &AmpSettings) {
        let target_comp = compensation(s.kind, s.drive);
        let target_on = if s.on { 1.0 } else { 0.0 };
        let tone_coef = onepole_coef(s.tone_hz, self.sample_rate);
        let dc_r = 1.0 - 2.0 * std::f32::consts::PI * 8.0 / self.sample_rate;
        let k = self.smooth;
        let mut up = [0.0; OS];
        for i in 0..left.len() {
            self.drive += (s.drive - self.drive) * k;
            self.bias += (s.bias - self.bias) * k;
            self.mix += (s.mix - self.mix) * k;
            self.level += (s.level - self.level) * k;
            self.comp += (target_comp - self.comp) * k;
            self.active += (target_on - self.active) * k;
            // Exactly settled values let the bypass skip the oversampler entirely.
            if (self.active - target_on).abs() < 1e-4 {
                self.active = target_on;
            }
            let b = self.bias * 0.8;
            let b_out = curve(s.kind, b);

            for (c, sample) in [(0, &mut left[i]), (1, &mut right[i])] {
                let ch = &mut self.ch[c];
                let x = *sample;
                ch.bypass.push(x);
                let dry = ch.bypass.read_int(OS_LATENCY);
                if self.active == 0.0 {
                    *sample = dry;
                    continue;
                }
                ch.os.up(x, &mut up);
                for v in up.iter_mut() {
                    self.drive_peak = self.drive_peak.max((*v * self.drive).abs());
                    let y = shape_with_offset(s.kind, *v, self.drive, b, b_out, self.comp);
                    *v = *v + (y - *v) * self.mix;
                }
                let y = ch.os.down(&up);
                ch.tone += (y - ch.tone) * tone_coef;
                let wet = ch.dc.process(ch.tone, dc_r) * self.level;
                *sample = dry + (wet - dry) * self.active;
            }
        }
    }
}

impl Default for Amp {
    fn default() -> Self {
        Self::new()
    }
}

/// Levels of harmonics 1..=N (relative to the fundamental's input level) of a full scale sine
/// through the shaper, by DFT. Used for the harmonic bars in the GUI.
pub fn harmonic_spectrum<const N: usize>(kind: AmpType, drive: f32, bias: f32, level: f32) -> [f32; N] {
    const M: usize = 256;
    let comp = compensation(kind, drive);
    let ys: Vec<f32> = (0..M)
        .map(|i| {
            let x = level * (2.0 * std::f32::consts::PI * i as f32 / M as f32).sin();
            shape(kind, x, drive, bias, comp)
        })
        .collect();
    let mut out = [0.0; N];
    for (h, o) in out.iter_mut().enumerate() {
        let n = (h + 1) as f32;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, y) in ys.iter().enumerate() {
            let a = 2.0 * std::f32::consts::PI * n * i as f32 / M as f32;
            re += y * a.cos();
            im += y * a.sin();
        }
        *o = 2.0 * (re * re + im * im).sqrt() / M as f32 / level;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bias_adds_even_harmonics() {
        let sym = harmonic_spectrum::<4>(AmpType::Tape, 4.0, 0.0, 0.5);
        let asym = harmonic_spectrum::<4>(AmpType::Tape, 4.0, 0.6, 0.5);
        assert!(sym[1] < 1e-3, "symmetric shaper made 2nd harmonic {}", sym[1]);
        assert!(sym[2] > 0.01, "no 3rd harmonic");
        assert!(asym[1] > 0.02, "bias made no 2nd harmonic {}", asym[1]);
    }

    #[test]
    fn bypass_is_latency_matched() {
        let mut amp = Amp::new();
        amp.set_sample_rate(48_000.0);
        let s = AmpSettings {
            on: false,
            kind: AmpType::Tube,
            drive: 4.0,
            bias: 0.0,
            tone_hz: 20_000.0,
            mix: 1.0,
            level: 1.0,
        };
        amp.init(&s);
        let mut l: Vec<f32> = (0..256).map(|i| if i == 10 { 1.0 } else { 0.0 }).collect();
        let mut r = l.clone();
        amp.process(&mut l, &mut r, &s);
        assert_eq!(l[10 + OS_LATENCY], 1.0);
    }
}
