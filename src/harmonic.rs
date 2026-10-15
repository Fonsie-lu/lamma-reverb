//! Section 3: key tracking harmonic EQ.
//!
//! [`KeyDetector`] listens to the MIDI notes of the track and estimates the key with the
//! Krumhansl-Kessler key profiles over a decaying pitch class histogram (each note counts by its
//! velocity and by how long it is held). [`HarmonicEq`] then places a band on every harmonic of
//! the key's root (or of the lowest held note, or a manual root): either peaking bands that boost
//! or cut the harmonics, or bandpasses that isolate them. All, even or odd harmonics can be
//! selected; Q is shared by all bands.

use crate::dsp::{note_hz, Svf, SvfCoefs, SvfMode};
use crate::params::{HarmMode, HarmSelect, HarmSource};

pub const MAX_HARMONICS: usize = 32;
/// How long the key detector remembers notes (time constant of the histogram decay).
const KEY_MEMORY_S: f32 = 20.0;
/// A new key must correlate this much better than the current one before it takes over.
const KEY_HYSTERESIS: f32 = 0.04;

const MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// Pitch class of the tonic, 0 = C.
    pub root: u8,
    pub minor: bool,
    /// Correlation with the key profile, -1..1.
    pub score: f32,
}

impl Key {
    pub fn name(&self) -> String {
        format!("{} {}", NOTE_NAMES[self.root as usize], if self.minor { "MINOR" } else { "MAJOR" })
    }
}

fn correlation(hist: &[f32; 12], profile: &[f32; 12], root: usize) -> f32 {
    let mh = hist.iter().sum::<f32>() / 12.0;
    let mp = profile.iter().sum::<f32>() / 12.0;
    let (mut num, mut dh, mut dp) = (0.0, 0.0, 0.0);
    for pc in 0..12 {
        let h = hist[pc] - mh;
        let p = profile[(pc + 12 - root) % 12] - mp;
        num += h * p;
        dh += h * h;
        dp += p * p;
    }
    if dh <= 1e-12 {
        0.0
    } else {
        num / (dh * dp).sqrt()
    }
}

pub struct KeyDetector {
    pub hist: [f32; 12],
    /// Number of note-ons per MIDI note that have not been released yet.
    held: [u8; 128],
    pub key: Option<Key>,
    /// Pitch class of the lowest held note, or of the last one played.
    pub bass: Option<u8>,
}

impl Default for KeyDetector {
    fn default() -> Self {
        Self { hist: [0.0; 12], held: [0; 128], key: None, bass: None }
    }
}

impl KeyDetector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn note_on(&mut self, note: u8, velocity: f32) {
        let n = note.min(127) as usize;
        self.held[n] = self.held[n].saturating_add(1);
        self.hist[n % 12] += 0.5 + velocity;
        self.update_bass();
    }

    pub fn note_off(&mut self, note: u8) {
        let n = note.min(127) as usize;
        self.held[n] = self.held[n].saturating_sub(1);
        self.update_bass();
    }

    pub fn all_notes_off(&mut self) {
        self.held = [0; 128];
    }

    fn update_bass(&mut self) {
        if let Some(n) = self.held.iter().position(|h| *h > 0) {
            self.bass = Some((n % 12) as u8);
        }
    }

    /// Bit mask of held pitch classes (bit 0 = C).
    pub fn held_mask(&self) -> u16 {
        self.held
            .iter()
            .enumerate()
            .filter(|(_, h)| **h > 0)
            .fold(0, |m, (n, _)| m | 1 << (n % 12))
    }

    /// Advances time by `dt` seconds: held notes gain weight, everything decays, and the key
    /// estimate is updated.
    pub fn advance(&mut self, dt: f32) {
        let decay = (-dt / KEY_MEMORY_S).exp();
        for h in self.hist.iter_mut() {
            *h *= decay;
        }
        for (n, h) in self.held.iter().enumerate() {
            if *h > 0 {
                self.hist[n % 12] += dt;
            }
        }
        self.estimate();
    }

    fn estimate(&mut self) {
        let total: f32 = self.hist.iter().sum();
        if total < 0.05 {
            return;
        }
        let mut best = Key { root: 0, minor: false, score: f32::MIN };
        for root in 0..12 {
            for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
                let score = correlation(&self.hist, profile, root);
                if score > best.score {
                    best = Key { root: root as u8, minor, score };
                }
            }
        }
        self.key = match self.key {
            Some(cur) => {
                let profile = if cur.minor { &MINOR } else { &MAJOR };
                let cur_score = correlation(&self.hist, profile, cur.root as usize);
                if best.score > cur_score + KEY_HYSTERESIS {
                    Some(best)
                } else {
                    Some(Key { score: cur_score, ..cur })
                }
            }
            None => Some(best),
        };
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct HarmSettings {
    pub on: bool,
    pub source: HarmSource,
    /// Manual root pitch class.
    pub root: u8,
    /// Octave of the fundamental (C0 = octave 0).
    pub octave: u8,
    pub mode: HarmMode,
    pub select: HarmSelect,
    pub q: f32,
    pub gain_db: f32,
    pub count: usize,
    pub mix: f32,
}

/// Harmonic number of band `i` for a selection.
#[inline]
pub fn harmonic_number(select: HarmSelect, i: usize) -> usize {
    match select {
        HarmSelect::All => i + 1,
        HarmSelect::Even => 2 * (i + 1),
        HarmSelect::Odd => 2 * i + 1,
    }
}

/// Root pitch class used for the current settings, falling back to the manual root while the
/// detector has nothing yet.
pub fn active_root(s: &HarmSettings, det: &KeyDetector) -> u8 {
    match s.source {
        HarmSource::Key => det.key.map(|k| k.root).unwrap_or(s.root),
        HarmSource::Note => det.bass.unwrap_or(s.root),
        HarmSource::Manual => s.root,
    }
}

/// Fundamental frequency for a root pitch class and octave.
pub fn fundamental_hz(root: u8, octave: u8) -> f32 {
    note_hz(12.0 * (octave as f32 + 1.0) + root as f32)
}

/// Band coefficients for a fundamental. Bands above 0.45 x the sample rate are left out.
pub fn band_coefs(s: &HarmSettings, f0: f32, sr: f32, out: &mut [SvfCoefs; MAX_HARMONICS]) -> usize {
    let mut n = 0;
    for i in 0..s.count.min(MAX_HARMONICS) {
        let hz = f0 * harmonic_number(s.select, i) as f32;
        if hz > sr * 0.45 {
            break;
        }
        out[n] = match s.mode {
            HarmMode::Peak => SvfCoefs::new(SvfMode::Bell, hz as f64, s.q as f64, s.gain_db as f64, sr as f64),
            HarmMode::Isolate => SvfCoefs::new(SvfMode::Bandpass, hz as f64, s.q as f64, 0.0, sr as f64),
        };
        n += 1;
    }
    n
}

/// Response of the whole EQ in dB at `hz` (before MIX), for the GUI.
pub fn response_db(s: &HarmSettings, coefs: &[SvfCoefs], hz: f32, sr: f32) -> f32 {
    let (hz, sr) = (hz as f64, sr as f64);
    let mag = match s.mode {
        HarmMode::Peak => coefs.iter().map(|c| c.magnitude(hz, sr)).product::<f64>(),
        HarmMode::Isolate => {
            let (re, im) = coefs.iter().fold((0.0, 0.0), |(re, im), c| {
                let (a, b) = c.response(hz, sr);
                (re + a, im + b)
            });
            (re * re + im * im).sqrt() * 10f64.powf(s.gain_db as f64 / 20.0)
        }
    };
    20.0 * mag.max(1e-6).log10() as f32
}

pub struct HarmonicEq {
    sr: f32,
    filters: [[Svf; MAX_HARMONICS]; 2],
    coefs: [SvfCoefs; MAX_HARMONICS],
    bands: usize,
    /// Smoothed fundamental, as log2 Hz.
    f0_log: f32,
    q: f32,
    gain_db: f32,
    mix: f32,
    makeup: f32,
    active: f32,
    last: Option<(HarmSettings, f32)>,
    mode: Option<HarmMode>,
}

/// Samples between coefficient updates while something glides.
const CHUNK: usize = 32;

impl HarmonicEq {
    pub fn new() -> Self {
        Self {
            sr: 48_000.0,
            filters: [[Svf::default(); MAX_HARMONICS]; 2],
            coefs: [SvfCoefs::default(); MAX_HARMONICS],
            bands: 0,
            f0_log: 6.0,
            q: 10.0,
            gain_db: 0.0,
            mix: 1.0,
            makeup: 1.0,
            active: 0.0,
            last: None,
            mode: None,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sr = sr;
        self.last = None;
    }

    pub fn init(&mut self, s: &HarmSettings, f0: f32) {
        self.f0_log = f0.log2();
        self.q = s.q;
        self.gain_db = s.gain_db;
        self.mix = s.mix;
        self.active = if s.on { 1.0 } else { 0.0 };
        self.last = None;
    }

    pub fn reset(&mut self) {
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
    }

    /// Fundamental currently used by the filters.
    pub fn f0(&self) -> f32 {
        self.f0_log.exp2()
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], s: &HarmSettings, f0: f32) {
        // Per chunk: smoothing toward the targets (about 40 ms for a key change).
        let k = 1.0 - (-(CHUNK as f32) / (0.04 * self.sr)).exp();
        let target_on = if s.on { 1.0 } else { 0.0 };
        if self.mode != Some(s.mode) {
            // Peak and isolate states mean different things; start clean.
            self.reset();
            self.mode = Some(s.mode);
        }
        let len = left.len();
        let mut pos = 0;
        while pos < len {
            let end = (pos + CHUNK).min(len);
            let glide = |cur: &mut f32, target: f32| {
                *cur += (target - *cur) * k;
                if (target - *cur).abs() < 1e-4 * target.abs().max(1.0) {
                    *cur = target;
                }
            };
            glide(&mut self.f0_log, f0.log2());
            glide(&mut self.q, s.q);
            glide(&mut self.gain_db, s.gain_db);
            glide(&mut self.mix, s.mix);
            glide(&mut self.active, target_on);

            let current = HarmSettings { q: self.q, gain_db: self.gain_db, ..*s };
            let key = (current, self.f0_log);
            if self.last != Some(key) {
                self.bands = band_coefs(&current, self.f0_log.exp2(), self.sr, &mut self.coefs);
                self.makeup = 10f32.powf(self.gain_db / 20.0);
                self.last = Some(key);
            }

            if self.active > 0.0 {
                for c in 0..2 {
                    let buf = if c == 0 { &mut left[pos..end] } else { &mut right[pos..end] };
                    let filters = &mut self.filters[c];
                    for x in buf.iter_mut() {
                        let dry = *x;
                        let wet = match s.mode {
                            HarmMode::Peak => {
                                let mut y = dry;
                                for (f, cf) in filters.iter_mut().zip(&self.coefs[..self.bands]) {
                                    y = f.process(y, cf);
                                }
                                y
                            }
                            HarmMode::Isolate => {
                                let mut y = 0.0;
                                for (f, cf) in filters.iter_mut().zip(&self.coefs[..self.bands]) {
                                    y += f.process(dry, cf);
                                }
                                y * self.makeup
                            }
                        };
                        let out = dry + (wet - dry) * self.mix;
                        *x = dry + (out - dry) * self.active;
                    }
                }
            } else {
                self.reset();
            }
            pos = end;
        }
    }
}

impl Default for HarmonicEq {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(det: &mut KeyDetector, notes: &[u8], seconds: f32) {
        for n in notes {
            det.note_on(*n, 0.8);
        }
        for _ in 0..(seconds * 100.0) as usize {
            det.advance(0.01);
        }
        for n in notes {
            det.note_off(*n);
        }
    }

    #[test]
    fn detects_c_major_and_a_minor() {
        let mut det = KeyDetector::default();
        // I - IV - V - I in C major.
        for chord in [[48, 52, 55], [53, 57, 60], [55, 59, 62], [48, 52, 55]] {
            play(&mut det, &chord, 1.0);
        }
        let k = det.key.unwrap();
        assert_eq!((k.root, k.minor), (0, false), "{}", k.name());

        let mut det = KeyDetector::default();
        // i - iv - V - i in A minor (harmonic: G#).
        for chord in [[57, 60, 64], [62, 65, 69], [64, 68, 71], [57, 60, 64], [57, 60, 64]] {
            play(&mut det, &chord, 1.0);
        }
        let k = det.key.unwrap();
        assert_eq!((k.root, k.minor), (9, true), "{}", k.name());
    }

    #[test]
    fn lowest_held_note_is_bass() {
        let mut det = KeyDetector::default();
        det.note_on(64, 1.0);
        det.note_on(50, 1.0);
        assert_eq!(det.bass, Some(2));
        det.note_off(50);
        assert_eq!(det.bass, Some(4));
        det.note_off(64);
        // Kept after release.
        assert_eq!(det.bass, Some(4));
    }

    fn settings(mode: HarmMode, select: HarmSelect) -> HarmSettings {
        HarmSettings {
            on: true,
            source: HarmSource::Manual,
            root: 9,
            octave: 2,
            mode,
            select,
            q: 30.0,
            gain_db: 12.0,
            count: 8,
            mix: 1.0,
        }
    }

    #[test]
    fn even_selection_boosts_only_even_harmonics() {
        let sr = 48_000.0;
        let s = settings(HarmMode::Peak, HarmSelect::Even);
        let f0 = fundamental_hz(9, 2);
        assert!((f0 - 110.0).abs() < 0.01);
        let mut coefs = [SvfCoefs::default(); MAX_HARMONICS];
        let n = band_coefs(&s, f0, sr, &mut coefs);
        let db = |hz: f32| response_db(&s, &coefs[..n], hz, sr);
        assert!(db(220.0) > 11.0, "2nd {}", db(220.0));
        assert!(db(440.0) > 11.0);
        assert!(db(110.0).abs() < 1.0, "1st {}", db(110.0));
        assert!(db(330.0).abs() < 1.0, "3rd {}", db(330.0));
    }

    #[test]
    fn isolate_passes_harmonics_and_rejects_between() {
        let sr = 48_000.0;
        let s = HarmSettings { gain_db: 0.0, ..settings(HarmMode::Isolate, HarmSelect::All) };
        let mut eq = HarmonicEq::new();
        eq.set_sample_rate(sr);
        let f0 = fundamental_hz(9, 2);
        eq.init(&s, f0);
        let level = |eq: &mut HarmonicEq, hz: f32| {
            eq.reset();
            let n = 24_000;
            let mut l: Vec<f32> = (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sr).sin()).collect();
            let mut r = l.clone();
            eq.process(&mut l, &mut r, &s, f0);
            l[n / 2..].iter().fold(0.0f32, |m, v| m.max(v.abs()))
        };
        assert!(level(&mut eq, 330.0) > 0.8);
        assert!(level(&mut eq, 385.0) < 0.15);
    }
}
