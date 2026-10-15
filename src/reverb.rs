//! Section 2a: the reverb. Pre-delay, an allpass input diffuser per channel and an 8 line
//! feedback delay network with a Hadamard mixing matrix, per line damping and slow delay
//! modulation. SIZE scales all line lengths, DECAY sets the RT60.

use crate::dsp::{onepole_coef, smooth_coef, DelayLine};

const LINES: usize = 8;
/// Line lengths at SIZE = 0.5 (scale 1), in ms. Mutually prime-ish in samples.
const BASE_MS: [f32; LINES] = [29.3, 34.7, 39.1, 43.9, 49.3, 55.1, 61.7, 68.3];
const DIFF_MS: [[f32; 4]; 2] = [[4.771, 3.595, 12.73, 9.307], [4.913, 3.343, 12.11, 9.817]];
const LFO_HZ: [f32; LINES] = [0.51, 0.63, 0.77, 0.89, 0.97, 1.09, 1.21, 1.33];
pub const MAX_PREDELAY_MS: f32 = 250.0;

/// Delay scale for a SIZE value 0..1 (0.3x .. 2x of `BASE_MS`).
#[inline]
pub fn size_scale(size: f32) -> f32 {
    0.3 + 1.7 * size * size
}

/// Loop filter cutoff for a DAMP value 0..1.
#[inline]
pub fn damp_hz(damp: f32) -> f32 {
    800.0 * ((1.0 - damp).clamp(0.0, 1.0) * 4.7).exp2()
}

/// Parabolic sine approximation for a phase in 0..1; plenty for delay modulation.
#[inline]
fn lfo_sine(phase: f32) -> f32 {
    let x = 2.0 * phase - 1.0;
    -4.0 * x * (1.0 - x.abs())
}

#[derive(Clone, Copy)]
pub struct ReverbSettings {
    pub predelay_ms: f32,
    pub size: f32,
    pub decay_s: f32,
    pub damp: f32,
    pub diffusion: f32,
    pub modulation: f32,
}

struct Allpass {
    line: DelayLine,
    len: usize,
}

impl Allpass {
    #[inline]
    fn process(&mut self, x: f32, g: f32) -> f32 {
        let d = self.line.read_int(self.len - 1);
        let w = x + g * d;
        self.line.push(w);
        d - g * w
    }
}

pub struct Reverb {
    sr: f32,
    predelay: [DelayLine; 2],
    diffusers: [[Allpass; 4]; 2],
    lines: [DelayLine; LINES],
    /// Current (smoothed) line lengths in samples.
    len: [f32; LINES],
    damp: [f32; LINES],
    phase: [f32; LINES],
    predelay_s: f32,
    len_smooth: f32,
    param_smooth: f32,
    diffusion: f32,
    modulation: f32,
}

#[inline]
fn hadamard(x: &mut [f32; LINES]) {
    let mut h = 1;
    while h < LINES {
        for i in (0..LINES).step_by(h * 2) {
            for j in i..i + h {
                let (a, b) = (x[j], x[j + h]);
                x[j] = a + b;
                x[j + h] = a - b;
            }
        }
        h *= 2;
    }
    let norm = 1.0 / (LINES as f32).sqrt();
    x.iter_mut().for_each(|v| *v *= norm);
}

impl Reverb {
    /// Allocates all buffers for `sr`. Call outside the audio thread.
    pub fn new(sr: f32) -> Self {
        let ms = |m: f32| (m * 0.001 * sr) as usize;
        let max_line = ms(BASE_MS[LINES - 1] * size_scale(1.0) + 3.0);
        let diffusers = |c: usize| {
            std::array::from_fn(|k| {
                let len = ms(DIFF_MS[c][k]).max(1);
                Allpass { line: DelayLine::new(len + 1), len }
            })
        };
        let mut r = Self {
            sr,
            predelay: [DelayLine::new(ms(MAX_PREDELAY_MS) + 2), DelayLine::new(ms(MAX_PREDELAY_MS) + 2)],
            diffusers: [diffusers(0), diffusers(1)],
            lines: std::array::from_fn(|_| DelayLine::new(max_line)),
            len: [0.0; LINES],
            damp: [0.0; LINES],
            phase: std::array::from_fn(|i| i as f32 / LINES as f32),
            predelay_s: 0.0,
            len_smooth: smooth_coef(0.25, sr),
            param_smooth: smooth_coef(0.03, sr),
            diffusion: 0.5,
            modulation: 0.0,
        };
        r.set_lengths(0.5);
        r
    }

    fn target_lengths(&self, size: f32) -> [f32; LINES] {
        std::array::from_fn(|i| BASE_MS[i] * size_scale(size) * 0.001 * self.sr)
    }

    fn set_lengths(&mut self, size: f32) {
        self.len = self.target_lengths(size);
    }

    pub fn init(&mut self, s: &ReverbSettings) {
        self.set_lengths(s.size);
        self.predelay_s = s.predelay_ms * 0.001 * self.sr;
        self.diffusion = s.diffusion;
        self.modulation = s.modulation;
    }

    pub fn reset(&mut self) {
        self.predelay.iter_mut().for_each(DelayLine::reset);
        for d in self.diffusers.iter_mut().flatten() {
            d.line.reset();
        }
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.damp = [0.0; LINES];
    }

    /// Renders the wet signal of one block in place: the slices hold the reverb input on entry
    /// and the 100 % wet output on return.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &ReverbSettings) {
        let target_len = self.target_lengths(s.size);
        let target_pre = (s.predelay_ms * 0.001 * self.sr).min(self.predelay[0].capacity());
        let damp_coef = onepole_coef(damp_hz(s.damp), self.sr);
        let decay = s.decay_s.max(0.05) * self.sr;
        let lfo_inc: [f32; LINES] = std::array::from_fn(|i| LFO_HZ[i] / self.sr);
        let (ks, kp) = (self.len_smooth, self.param_smooth);
        // Gain per line for the RT60 (-60 dB after `decay` samples). Lengths glide slowly, so
        // once per block is enough.
        let gain: [f32; LINES] = std::array::from_fn(|i| (-6.907_755 * self.len[i] / decay).exp());

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.predelay_s += (target_pre - self.predelay_s) * kp;
            self.diffusion += (s.diffusion - self.diffusion) * kp;
            self.modulation += (s.modulation - self.modulation) * kp;
            let depth = self.modulation * 0.0006 * self.sr;
            let g_ap = 0.72 * self.diffusion;

            let mut input = [*l, *r];
            for c in 0..2 {
                self.predelay[c].push(input[c]);
                let mut x = self.predelay[c].read(self.predelay_s);
                for ap in self.diffusers[c].iter_mut() {
                    x = ap.process(x, g_ap);
                }
                input[c] = x;
            }

            let mut o = [0.0f32; LINES];
            for i in 0..LINES {
                self.len[i] += (target_len[i] - self.len[i]) * ks;
                self.phase[i] += lfo_inc[i];
                if self.phase[i] >= 1.0 {
                    self.phase[i] -= 1.0;
                }
                let m = lfo_sine(self.phase[i]) * depth;
                o[i] = self.lines[i].read(self.len[i] - 1.0 + m - depth.min(self.len[i] * 0.5));
            }
            let (mut out_l, mut out_r) = (0.0, 0.0);
            for i in 0..LINES {
                let sign = if i & 2 == 0 { 1.0 } else { -1.0 };
                if i & 1 == 0 {
                    out_l += o[i] * sign;
                } else {
                    out_r += o[i] * sign;
                }
            }

            let mut f = [0.0f32; LINES];
            for i in 0..LINES {
                self.damp[i] += (o[i] - self.damp[i]) * damp_coef;
                f[i] = self.damp[i] * gain[i];
            }
            hadamard(&mut f);
            for i in 0..LINES {
                let sign = if i & 2 == 0 { 1.0 } else { -1.0 };
                let x = input[i & 1] * sign * 0.5;
                self.lines[i].push(f[i] + x);
            }

            *l = out_l * 0.5;
            *r = out_r * 0.5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn decay_follows_rt60() {
        let sr = 48_000.0;
        let s = ReverbSettings {
            predelay_ms: 0.0,
            size: 0.5,
            decay_s: 1.0,
            damp: 0.0,
            diffusion: 0.7,
            modulation: 0.0,
        };
        let mut rv = Reverb::new(sr);
        rv.init(&s);
        let n = (sr * 2.0) as usize;
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        for i in 0..480 {
            l[i] = 0.5;
            r[i] = -0.3;
        }
        rv.process(&mut l, &mut r, &s);
        let a = rms(&l[(0.2 * sr) as usize..(0.3 * sr) as usize]);
        let b = rms(&l[(0.7 * sr) as usize..(0.8 * sr) as usize]);
        let db = 20.0 * (b / a).log10();
        // 0.5 s later with RT60 = 1 s: about -30 dB.
        assert!((-36.0..-24.0).contains(&db), "decay {db} dB over 0.5 s");
        assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()));
    }
}
