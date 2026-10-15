//! Small shared DSP building blocks: noise, saturation helpers, a state variable filter, a
//! fractional delay line and the 4x oversampler used by the amp.

use std::f64::consts::PI as PI64;

/// Cheap xorshift noise generator, deterministic per seed.
#[derive(Clone, Copy)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453) | 1)
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in [0, 1).
    #[inline]
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in [-1, 1).
    #[inline]
    pub fn bipolar(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// Rational tanh approximation, exact at 0 and saturating at +-1 from |x| = 3.
#[inline]
pub fn tanh(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// Dot product written with eight independent accumulators so it vectorises.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0.0f32; 8];
    let mut ca = a.chunks_exact(8);
    let mut cb = b.chunks_exact(8);
    for (x, y) in (&mut ca).zip(&mut cb) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let mut sum: f32 = acc.iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        sum += x * y;
    }
    sum
}

/// One-pole lowpass coefficient for a given cutoff.
#[inline]
pub fn onepole_coef(cutoff_hz: f32, sample_rate: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * cutoff_hz / sample_rate).exp()
}

/// Time-constant based smoothing coefficient.
#[inline]
pub fn smooth_coef(time_s: f32, sample_rate: f32) -> f32 {
    if time_s <= 0.0 {
        1.0
    } else {
        1.0 - (-1.0 / (time_s * sample_rate)).exp()
    }
}

/// Frequency of a MIDI note number (A4 = 440 Hz).
#[inline]
pub fn note_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// Equal power crossfade gains (dry, wet) for a 0..1 mix.
#[inline]
pub fn equal_power(mix: f32) -> (f32, f32) {
    let a = mix.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}

/// First order DC blocker.
#[derive(Clone, Copy, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    pub fn process(&mut self, x: f32, r: f32) -> f32 {
        let y = x - self.x1 + r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Response types of [`Svf`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SvfMode {
    Lowpass,
    Highpass,
    /// Bandpass with 0 dB gain at the centre.
    Bandpass,
    /// Peaking EQ; the gain is set through [`SvfCoefs::new`].
    Bell,
}

/// Coefficients of the trapezoidal (Simper) state variable filter, in f64 so narrow bands at low
/// frequencies stay clean.
#[derive(Clone, Copy, Debug)]
pub struct SvfCoefs {
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

impl Default for SvfCoefs {
    /// A wire: passes the input unchanged.
    fn default() -> Self {
        Self { a1: 1.0, a2: 0.0, a3: 0.0, m0: 1.0, m1: 0.0, m2: 0.0 }
    }
}

impl SvfCoefs {
    pub fn new(mode: SvfMode, hz: f64, q: f64, gain_db: f64, sample_rate: f64) -> Self {
        let hz = hz.clamp(1.0, sample_rate * 0.49);
        let g = (PI64 * hz / sample_rate).tan();
        let a = 10f64.powf(gain_db / 40.0);
        let k = match mode {
            SvfMode::Bell => 1.0 / (q * a),
            _ => 1.0 / q,
        };
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let (m0, m1, m2) = match mode {
            SvfMode::Lowpass => (0.0, 0.0, 1.0),
            SvfMode::Highpass => (1.0, -k, -1.0),
            SvfMode::Bandpass => (0.0, k, 0.0),
            SvfMode::Bell => (1.0, k * (a * a - 1.0), 0.0),
        };
        Self { a1, a2, a3, m0, m1, m2 }
    }

    /// Complex response (re, im) at `hz`, exact for the digital filter.
    pub fn response(&self, hz: f64, sample_rate: f64) -> (f64, f64) {
        // Recover g and k from the coefficients and evaluate the analog prototype at the
        // pre-warped frequency, which equals the digital response.
        let g = self.a2 / self.a1;
        if g <= 0.0 {
            return (self.m0, 0.0);
        }
        let k = (1.0 / self.a1 - 1.0) / g - g;
        let w = (PI64 * hz.min(sample_rate * 0.4999) / sample_rate).tan() / g;
        // H(s) = (m0 (s^2 + k s + 1) + m1 s + m2) / (s^2 + k s + 1) with s = jw
        let (dr, di) = (1.0 - w * w, k * w);
        let (nr, ni) = (self.m0 * dr + self.m2, self.m0 * di + self.m1 * w);
        let d = dr * dr + di * di;
        ((nr * dr + ni * di) / d, (ni * dr - nr * di) / d)
    }

    /// Magnitude response at `hz`.
    pub fn magnitude(&self, hz: f64, sample_rate: f64) -> f64 {
        let (re, im) = self.response(hz, sample_rate);
        (re * re + im * im).sqrt()
    }
}

#[derive(Clone, Copy, Default)]
pub struct Svf {
    ic1: f64,
    ic2: f64,
}

impl Svf {
    #[inline]
    pub fn process(&mut self, x: f32, c: &SvfCoefs) -> f32 {
        let v0 = x as f64;
        let v3 = v0 - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (c.m0 * v0 + c.m1 * v1 + c.m2 * v2) as f32
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Ring buffer delay with a power of two length and fractional (linear) reads.
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl DelayLine {
    /// A delay that holds at least `max_samples` samples. Allocates, so call it outside `process`.
    pub fn new(max_samples: usize) -> Self {
        let len = (max_samples + 4).next_power_of_two();
        Self { buf: vec![0.0; len], mask: len - 1, pos: 0 }
    }

    /// Longest delay that can be read.
    pub fn capacity(&self) -> f32 {
        (self.buf.len() - 3) as f32
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.pos = (self.pos + 1) & self.mask;
        self.buf[self.pos] = x;
    }

    /// Sample written `delay` samples ago (1.0 = the previous push).
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let d = delay.clamp(0.0, self.capacity());
        let i = d as usize;
        let f = d - i as f32;
        let a = self.buf[(self.pos.wrapping_sub(i)) & self.mask];
        let b = self.buf[(self.pos.wrapping_sub(i + 1)) & self.mask];
        a + (b - a) * f
    }

    #[inline]
    pub fn read_int(&self, delay: usize) -> f32 {
        self.buf[(self.pos.wrapping_sub(delay)) & self.mask]
    }

    pub fn reset(&mut self) {
        self.buf.iter_mut().for_each(|x| *x = 0.0);
    }
}

/// Oversampling factor of [`Oversampler`].
pub const OS: usize = 4;
const OS_TAPS: usize = 129;
/// Latency of an up/down round trip in base rate samples (two linear phase FIRs of 64 taps of
/// delay each at the 4x rate).
pub const OS_LATENCY: usize = (OS_TAPS - 1) / OS;

/// Windowed sinc lowpass at 0.45 of the base rate Nyquist band, designed at the 4x rate.
fn os_kernel() -> [f64; OS_TAPS] {
    let fc = 0.5 / OS as f64 * 0.9;
    let m = (OS_TAPS - 1) as f64 / 2.0;
    let mut h = [0.0; OS_TAPS];
    let mut sum = 0.0;
    for (n, tap) in h.iter_mut().enumerate() {
        let x = n as f64 - m;
        let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * PI64 * fc * x).sin() / (PI64 * x) };
        // Blackman-Harris window
        let a = 2.0 * PI64 * n as f64 / (OS_TAPS - 1) as f64;
        let w = 0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos();
        *tap = sinc * w;
        sum += *tap;
    }
    h.iter_mut().for_each(|t| *t /= sum);
    h
}

/// 4x polyphase up / down sampler for one channel. `up` turns one base rate sample into four,
/// `down` turns four back into one. The round trip delays by exactly [`OS_LATENCY`] samples.
pub struct Oversampler {
    /// Polyphase branches of the interpolator, gain of 4 folded in.
    up_taps: [[f32; OS_TAPS / OS + 1]; OS],
    down_taps: [f32; OS_TAPS],
    /// Base rate history, mirrored so a contiguous window is always available.
    up_hist: [f32; 2 * (OS_TAPS / OS + 1)],
    up_pos: usize,
    down_hist: [f32; 2 * OS_TAPS],
    down_pos: usize,
}

impl Oversampler {
    pub fn new() -> Self {
        let h = os_kernel();
        let mut up_taps = [[0.0; OS_TAPS / OS + 1]; OS];
        for (p, branch) in up_taps.iter_mut().enumerate() {
            for (j, tap) in branch.iter_mut().enumerate() {
                let k = p + OS * j;
                if k < OS_TAPS {
                    *tap = (h[k] * OS as f64) as f32;
                }
            }
        }
        let mut down_taps = [0.0; OS_TAPS];
        for (t, v) in down_taps.iter_mut().zip(h.iter()) {
            *t = *v as f32;
        }
        Self {
            up_taps,
            down_taps,
            up_hist: [0.0; 2 * (OS_TAPS / OS + 1)],
            up_pos: 0,
            down_hist: [0.0; 2 * OS_TAPS],
            down_pos: 0,
        }
    }

    pub fn reset(&mut self) {
        self.up_hist = [0.0; 2 * (OS_TAPS / OS + 1)];
        self.down_hist = [0.0; 2 * OS_TAPS];
        self.up_pos = 0;
        self.down_pos = 0;
    }

    #[inline]
    pub fn up(&mut self, x: f32, out: &mut [f32; OS]) {
        const N: usize = OS_TAPS / OS + 1;
        // Newest sample first: the window starting at `up_pos` runs x[n], x[n-1], ...
        self.up_pos = if self.up_pos == 0 { N - 1 } else { self.up_pos - 1 };
        self.up_hist[self.up_pos] = x;
        self.up_hist[self.up_pos + N] = x;
        let window = &self.up_hist[self.up_pos..self.up_pos + N];
        for (o, branch) in out.iter_mut().zip(self.up_taps.iter()) {
            *o = dot(window, branch);
        }
    }

    #[inline]
    pub fn down(&mut self, x: &[f32; OS]) -> f32 {
        let mut y = 0.0;
        for (k, v) in x.iter().enumerate() {
            self.down_pos = if self.down_pos == 0 { OS_TAPS - 1 } else { self.down_pos - 1 };
            self.down_hist[self.down_pos] = *v;
            self.down_hist[self.down_pos + OS_TAPS] = *v;
            // Decimate on phase 0 so the round trip delay is a whole number of base samples.
            if k == 0 {
                let window = &self.down_hist[self.down_pos..self.down_pos + OS_TAPS];
                y = dot(window, &self.down_taps);
            }
        }
        y
    }
}

impl Default for Oversampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversampler_round_trip_is_a_pure_delay() {
        let mut os = Oversampler::new();
        let mut frame = [0.0; OS];
        let n = 400;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut out = Vec::new();
        for x in &input {
            os.up(*x, &mut frame);
            out.push(os.down(&frame));
        }
        for i in 100..n {
            let err = (out[i] - input[i - OS_LATENCY]).abs();
            assert!(err < 2e-3, "sample {i}: {} vs {}", out[i], input[i - OS_LATENCY]);
        }
    }

    #[test]
    fn svf_bell_has_requested_gain() {
        let fs = 48_000.0;
        let c = SvfCoefs::new(SvfMode::Bell, 1000.0, 4.0, 12.0, fs);
        let g = c.magnitude(1000.0, fs);
        assert!((20.0 * g.log10() - 12.0).abs() < 0.05, "{g}");
        assert!((c.magnitude(20.0, fs) - 1.0).abs() < 0.01);
        let bp = SvfCoefs::new(SvfMode::Bandpass, 500.0, 30.0, 0.0, fs);
        assert!((bp.magnitude(500.0, fs) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn svf_magnitude_matches_measurement() {
        let fs = 48_000.0;
        let c = SvfCoefs::new(SvfMode::Bell, 300.0, 10.0, -12.0, fs);
        for hz in [100.0f64, 280.0, 300.0, 1000.0] {
            let mut f = Svf::default();
            let mut peak = 0.0f32;
            for i in 0..48_000 {
                let x = (2.0 * PI64 * hz * i as f64 / fs).sin() as f32;
                let y = f.process(x, &c);
                if i > 24_000 {
                    peak = peak.max(y.abs());
                }
            }
            let expect = c.magnitude(hz, fs) as f32;
            assert!((peak - expect).abs() < 0.01, "{hz} Hz: {peak} vs {expect}");
        }
    }
}
