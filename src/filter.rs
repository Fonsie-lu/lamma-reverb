//! Section 4: classic low cut / high cut filter. Butterworth highpass and lowpass, 12 or
//! 24 dB/oct, with optional resonance at both edges.

use crate::dsp::{smooth_coef, Svf, SvfCoefs, SvfMode};
use crate::params::Slope;

#[derive(Clone, Copy, PartialEq)]
pub struct CutSettings {
    pub on: bool,
    pub low_hz: f32,
    pub high_hz: f32,
    pub slope: Slope,
    /// 0..1
    pub resonance: f32,
}

/// Butterworth Q values of the cascaded stages for a slope.
pub fn stage_qs(slope: Slope) -> &'static [f32] {
    match slope {
        Slope::Db12 => &[std::f32::consts::FRAC_1_SQRT_2],
        Slope::Db24 => &[0.541_196, 1.306_563],
    }
}

/// Resonance only raises the last (highest Q) stage.
#[inline]
pub fn resonant_q(q: f32, last: bool, resonance: f32) -> f32 {
    if last {
        q * (1.0 + 5.0 * resonance * resonance)
    } else {
        q
    }
}

/// Coefficients of all stages: (highpass stages, lowpass stages, count).
pub fn coefs(s: &CutSettings, low: f32, high: f32, sr: f32) -> ([SvfCoefs; 2], [SvfCoefs; 2], usize) {
    let qs = stage_qs(s.slope);
    let mut hp = [SvfCoefs::default(); 2];
    let mut lp = [SvfCoefs::default(); 2];
    for (i, q) in qs.iter().enumerate() {
        let q = resonant_q(*q, i + 1 == qs.len(), s.resonance) as f64;
        hp[i] = SvfCoefs::new(SvfMode::Highpass, low as f64, q, 0.0, sr as f64);
        lp[i] = SvfCoefs::new(SvfMode::Lowpass, high as f64, q, 0.0, sr as f64);
    }
    (hp, lp, qs.len())
}

/// Magnitude in dB of the whole filter at `hz`, for the GUI.
pub fn response_db(s: &CutSettings, hz: f32, sr: f32) -> f32 {
    let (hp, lp, n) = coefs(s, s.low_hz, s.high_hz, sr);
    let mag: f64 = hp[..n]
        .iter()
        .chain(lp[..n].iter())
        .map(|c| c.magnitude(hz as f64, sr as f64))
        .product();
    20.0 * mag.max(1e-6).log10() as f32
}

pub struct CutFilter {
    sr: f32,
    hp: [[Svf; 2]; 2],
    lp: [[Svf; 2]; 2],
    /// Smoothed cutoffs as log2 Hz.
    low: f32,
    high: f32,
    active: f32,
    smooth: f32,
}

const CHUNK: usize = 16;

impl CutFilter {
    pub fn new() -> Self {
        Self {
            sr: 48_000.0,
            hp: Default::default(),
            lp: Default::default(),
            low: 5.0,
            high: 14.0,
            active: 1.0,
            smooth: 0.1,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.smooth = smooth_coef(0.03, sr / CHUNK as f32);
    }

    pub fn init(&mut self, s: &CutSettings) {
        self.low = s.low_hz.log2();
        self.high = s.high_hz.log2();
        self.active = if s.on { 1.0 } else { 0.0 };
    }

    pub fn reset(&mut self) {
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()).flatten() {
            f.reset();
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &CutSettings) {
        let target_on = if s.on { 1.0 } else { 0.0 };
        let mut pos = 0;
        while pos < left.len() {
            let end = (pos + CHUNK).min(left.len());
            let k = self.smooth;
            self.low += (s.low_hz.log2() - self.low) * k;
            self.high += (s.high_hz.log2() - self.high) * k;
            self.active += (target_on - self.active) * k;
            if self.active < 1e-4 {
                self.active = 0.0;
                self.reset();
                pos = end;
                continue;
            }
            let (hp, lp, n) = coefs(s, self.low.exp2(), self.high.exp2(), self.sr);
            for c in 0..2 {
                let buf = if c == 0 { &mut left[pos..end] } else { &mut right[pos..end] };
                for x in buf.iter_mut() {
                    let mut y = *x;
                    for i in 0..n {
                        y = self.hp[c][i].process(y, &hp[i]);
                        y = self.lp[c][i].process(y, &lp[i]);
                    }
                    *x += (y - *x) * self.active;
                }
            }
            pos = end;
        }
    }
}

impl Default for CutFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passband_is_flat_and_edges_cut() {
        let s = CutSettings { on: true, low_hz: 100.0, high_hz: 5000.0, slope: Slope::Db24, resonance: 0.0 };
        let sr = 48_000.0;
        assert!(response_db(&s, 700.0, sr).abs() < 0.2);
        assert!((response_db(&s, 100.0, sr) + 3.0).abs() < 0.3);
        assert!(response_db(&s, 25.0, sr) < -40.0);
        assert!(response_db(&s, 20_000.0, sr) < -40.0);
    }
}
