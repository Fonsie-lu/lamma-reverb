//! Host independent signal chain:
//!
//! input -> AMP -> REVERB section (ping-pong delay + reverb, dry/wet) -> HARMONIC EQ -> FILTER
//! -> output (global dry/wet against the latency compensated input, output gain).
//!
//! [`Settings`] is a plain snapshot of all parameters, read once per host block.

use crate::amp::{Amp, AmpSettings};
use crate::dsp::{equal_power, smooth_coef, DelayLine, OS_LATENCY};
use crate::filter::{CutFilter, CutSettings};
use crate::harmonic::{active_root, fundamental_hz, HarmSettings, HarmonicEq, KeyDetector};
use crate::pingpong::{PingPong, PingPongSettings};
use crate::reverb::{Reverb, ReverbSettings};
use crate::visuals::VisualState;
use std::sync::atomic::Ordering;

/// Largest chunk processed at once; also the granularity of the key detector.
pub const MAX_BLOCK: usize = 64;

/// Plugin latency in samples (the amp's oversampling filters).
pub const LATENCY: usize = OS_LATENCY;

#[derive(Clone, Copy)]
pub struct Settings {
    pub amp: AmpSettings,
    pub verb_on: bool,
    pub verb_mix: f32,
    pub verb_width: f32,
    pub reverb: ReverbSettings,
    pub pp_on: bool,
    pub pp: PingPongSettings,
    pub pp_level: f32,
    pub pp_send: f32,
    pub harm: HarmSettings,
    pub cut: CutSettings,
    pub out_mix: f32,
    /// Linear output gain.
    pub out_gain: f32,
}

/// Smoothed scalar controls of the reverb section and the output.
#[derive(Clone, Copy, Default)]
struct Smoothed {
    verb_on: f32,
    pp_on: f32,
    section_on: f32,
    verb_mix: f32,
    width: f32,
    pp_level: f32,
    pp_send: f32,
    out_mix: f32,
    out_gain: f32,
}

impl Smoothed {
    fn targets(s: &Settings) -> Self {
        let b = |v: bool| if v { 1.0 } else { 0.0 };
        Self {
            verb_on: b(s.verb_on),
            pp_on: b(s.pp_on),
            section_on: b(s.verb_on || s.pp_on),
            verb_mix: s.verb_mix,
            width: s.verb_width,
            pp_level: s.pp_level,
            pp_send: s.pp_send,
            out_mix: s.out_mix,
            out_gain: s.out_gain,
        }
    }

    #[inline]
    fn approach(&mut self, t: &Self, k: f32) {
        self.verb_on += (t.verb_on - self.verb_on) * k;
        self.pp_on += (t.pp_on - self.pp_on) * k;
        self.section_on += (t.section_on - self.section_on) * k;
        self.verb_mix += (t.verb_mix - self.verb_mix) * k;
        self.width += (t.width - self.width) * k;
        self.pp_level += (t.pp_level - self.pp_level) * k;
        self.pp_send += (t.pp_send - self.pp_send) * k;
        self.out_mix += (t.out_mix - self.out_mix) * k;
        self.out_gain += (t.out_gain - self.out_gain) * k;
    }
}

pub struct Engine {
    sr: f32,
    amp: Amp,
    pingpong: PingPong,
    reverb: Reverb,
    pub key: KeyDetector,
    harm: HarmonicEq,
    cut: CutFilter,
    dry: [DelayLine; 2],
    sm: Smoothed,
    smooth: f32,
    // Scratch buffers for one chunk.
    in_l: [f32; MAX_BLOCK],
    in_r: [f32; MAX_BLOCK],
    sec_l: [f32; MAX_BLOCK],
    sec_r: [f32; MAX_BLOCK],
    echo_l: [f32; MAX_BLOCK],
    echo_r: [f32; MAX_BLOCK],
    // Meters, read by `publish`.
    peak_in: [f32; 2],
    peak_out: [f32; 2],
    root: u8,
}

impl Engine {
    /// Allocates all delay memory for `sr`. Call outside the audio thread.
    pub fn new(sr: f32) -> Self {
        let mut amp = Amp::new();
        amp.set_sample_rate(sr);
        let mut harm = HarmonicEq::new();
        harm.set_sample_rate(sr);
        let mut cut = CutFilter::new();
        cut.set_sample_rate(sr);
        Self {
            sr,
            amp,
            pingpong: PingPong::new(sr),
            reverb: Reverb::new(sr),
            key: KeyDetector::default(),
            harm,
            cut,
            dry: [DelayLine::new(LATENCY + 1), DelayLine::new(LATENCY + 1)],
            sm: Smoothed::default(),
            smooth: smooth_coef(0.03, sr),
            in_l: [0.0; MAX_BLOCK],
            in_r: [0.0; MAX_BLOCK],
            sec_l: [0.0; MAX_BLOCK],
            sec_r: [0.0; MAX_BLOCK],
            echo_l: [0.0; MAX_BLOCK],
            echo_r: [0.0; MAX_BLOCK],
            peak_in: [0.0; 2],
            peak_out: [0.0; 2],
            root: 0,
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    /// Jump all smoothed values to the settings.
    pub fn init(&mut self, s: &Settings) {
        self.amp.init(&s.amp);
        self.reverb.init(&s.reverb);
        self.pingpong.init(&s.pp);
        let root = active_root(&s.harm, &self.key);
        self.harm.init(&s.harm, fundamental_hz(root, s.harm.octave));
        self.cut.init(&s.cut);
        self.sm = Smoothed::targets(s);
    }

    pub fn reset(&mut self) {
        self.amp.reset();
        self.pingpong.reset();
        self.reverb.reset();
        self.harm.reset();
        self.cut.reset();
        self.dry.iter_mut().for_each(DelayLine::reset);
    }

    pub fn note_on(&mut self, note: u8, velocity: f32) {
        self.key.note_on(note, velocity);
    }

    pub fn note_off(&mut self, note: u8) {
        self.key.note_off(note);
    }

    pub fn all_notes_off(&mut self) {
        self.key.all_notes_off();
    }

    /// Processes any number of samples in place.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &Settings) {
        let mut pos = 0;
        while pos < left.len() {
            let end = (pos + MAX_BLOCK).min(left.len());
            self.process_chunk(&mut left[pos..end], &mut right[pos..end], s);
            pos = end;
        }
    }

    fn process_chunk(&mut self, left: &mut [f32], right: &mut [f32], s: &Settings) {
        let n = left.len();
        let targets = Smoothed::targets(s);

        // Latency compensated copy of the input for the global dry/wet.
        for i in 0..n {
            self.peak_in[0] = self.peak_in[0].max(left[i].abs());
            self.peak_in[1] = self.peak_in[1].max(right[i].abs());
            self.dry[0].push(left[i]);
            self.dry[1].push(right[i]);
            self.in_l[i] = self.dry[0].read_int(LATENCY);
            self.in_r[i] = self.dry[1].read_int(LATENCY);
        }

        // 1. Amp
        self.amp.process(left, right, &s.amp);

        // 2. Ping-pong delay and reverb. The reverb input is the section input plus some echo.
        // Each pass below replays the same smoothing trajectory from the chunk start.
        let start = self.sm;
        let mut sm = start;
        for i in 0..n {
            sm.approach(&targets, self.smooth);
            let mono = 0.5 * (left[i] + right[i]);
            let (el, er) = self.pingpong.tick(mono * sm.pp_on, &s.pp);
            self.echo_l[i] = el * sm.pp_on;
            self.echo_r[i] = er * sm.pp_on;
            self.sec_l[i] = (left[i] + self.echo_l[i] * sm.pp_send) * sm.verb_on;
            self.sec_r[i] = (right[i] + self.echo_r[i] * sm.pp_send) * sm.verb_on;
        }
        self.reverb.process(&mut self.sec_l[..n], &mut self.sec_r[..n], &s.reverb);
        let mut sm = start;
        for i in 0..n {
            sm.approach(&targets, self.smooth);
            let wl = self.sec_l[i] * sm.verb_on + self.echo_l[i] * sm.pp_level;
            let wr = self.sec_r[i] * sm.verb_on + self.echo_r[i] * sm.pp_level;
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * sm.width;
            let (gd, gw) = equal_power(sm.verb_mix);
            let (dl, dr) = (left[i], right[i]);
            left[i] = dl + (dl * gd + (mid + side) * gw - dl) * sm.section_on;
            right[i] = dr + (dr * gd + (mid - side) * gw - dr) * sm.section_on;
        }

        // 3. Harmonic EQ on the root from the key detector, the lowest note or the manual root.
        self.key.advance(n as f32 / self.sr);
        self.root = active_root(&s.harm, &self.key);
        let f0 = fundamental_hz(self.root, s.harm.octave);
        self.harm.process(left, right, &s.harm, f0);

        // 4. Low / high cut
        self.cut.process(left, right, &s.cut);

        // 5. Output
        let mut sm = start;
        for i in 0..n {
            sm.approach(&targets, self.smooth);
            let (gd, gw) = equal_power(sm.out_mix);
            left[i] = (self.in_l[i] * gd + left[i] * gw) * sm.out_gain;
            right[i] = (self.in_r[i] * gd + right[i] * gw) * sm.out_gain;
            self.peak_out[0] = self.peak_out[0].max(left[i].abs());
            self.peak_out[1] = self.peak_out[1].max(right[i].abs());
        }
        self.sm = sm;
    }

    /// Publishes meters and the key detector state for the editor, then restarts the meters.
    pub fn publish(&mut self, v: &VisualState) {
        let r = Ordering::Relaxed;
        v.store_peaks(&v.in_l, &v.in_r, self.peak_in);
        v.store_peaks(&v.out_l, &v.out_r, self.peak_out);
        v.drive.store((v.drive.load(r) * 0.9).max(self.amp.drive_peak), r);
        self.peak_in = [0.0; 2];
        self.peak_out = [0.0; 2];
        self.amp.drive_peak = 0.0;

        let sum: f32 = self.key.hist.iter().sum::<f32>().max(1e-6);
        for (slot, h) in v.hist.iter().zip(self.key.hist.iter()) {
            slot.store(h / sum, r);
        }
        match self.key.key {
            Some(k) => {
                v.key_root.store(k.root as i8, r);
                v.key_minor.store(k.minor, r);
                v.key_score.store(k.score, r);
            }
            None => v.key_root.store(-1, r),
        }
        v.held.store(self.key.held_mask(), r);
        v.root.store(self.root, r);
        v.f0.store(self.harm.f0(), r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ReverbParams;

    #[test]
    fn bypassed_chain_is_a_pure_delay() {
        let params = ReverbParams::default();
        let mut s = crate::settings_from_params(&params, 120.0);
        s.amp.on = false;
        s.verb_on = false;
        s.pp_on = false;
        s.harm.on = false;
        s.cut.on = false;
        let mut e = Engine::new(48_000.0);
        e.init(&s);
        let mut l: Vec<f32> = (0..1000).map(|i| ((i * 7919) % 101) as f32 / 101.0 - 0.5).collect();
        let input = l.clone();
        let mut r = l.clone();
        e.process(&mut l, &mut r, &s);
        for i in LATENCY..1000 {
            assert!((l[i] - input[i - LATENCY]).abs() < 1e-6, "sample {i}");
        }
    }

    #[test]
    fn default_settings_stay_finite_and_bounded() {
        let params = ReverbParams::default();
        let s = crate::settings_from_params(&params, 120.0);
        let mut e = Engine::new(48_000.0);
        e.init(&s);
        e.note_on(57, 0.9);
        let n = 48_000 * 3;
        let mut l: Vec<f32> = (0..n)
            .map(|i| if i < 24_000 { (i as f32 * 0.0577).sin() * 0.5 } else { 0.0 })
            .collect();
        let mut r = l.clone();
        e.process(&mut l, &mut r, &s);
        let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(peak < 4.0 && peak > 0.05, "peak {peak}");
        // The reverb tail is still there after the input stopped.
        let tail = l[30_000..40_000].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(tail > 1e-3, "tail {tail}");
    }
}
