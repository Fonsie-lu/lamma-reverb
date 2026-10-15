//! Section 2b: ping-pong delay inside the reverb section. The mono input enters the left line,
//! each line feeds the other, so echoes alternate left / right. The feedback path is band
//! limited and softly saturated, like a tape echo, so high feedback stays musical.

use crate::dsp::{onepole_coef, smooth_coef, DelayLine};
use crate::params::Division;

pub const MAX_DELAY_S: f32 = 4.0;

/// Delay time in seconds for the current settings.
pub fn delay_seconds(sync: bool, division: Division, time_ms: f32, tempo_bpm: f32) -> f32 {
    let s = if sync {
        division.beats() * 60.0 / tempo_bpm.clamp(20.0, 999.0)
    } else {
        time_ms * 0.001
    };
    s.clamp(0.001, MAX_DELAY_S)
}

#[derive(Clone, Copy)]
pub struct PingPongSettings {
    pub seconds: f32,
    pub feedback: f32,
}

#[inline]
fn soft(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

pub struct PingPong {
    sr: f32,
    lines: [DelayLine; 2],
    time: f32,
    smooth: f32,
    lp: [f32; 2],
    hp: [f32; 2],
    lp_coef: f32,
    hp_coef: f32,
}

impl PingPong {
    /// Allocates the delay lines for `sr`. Call outside the audio thread.
    pub fn new(sr: f32) -> Self {
        let n = (MAX_DELAY_S * sr) as usize + 4;
        Self {
            sr,
            lines: [DelayLine::new(n), DelayLine::new(n)],
            time: 0.25 * sr,
            smooth: smooth_coef(0.08, sr),
            lp: [0.0; 2],
            hp: [0.0; 2],
            lp_coef: onepole_coef(6500.0, sr),
            hp_coef: onepole_coef(90.0, sr),
        }
    }

    pub fn init(&mut self, s: &PingPongSettings) {
        self.time = s.seconds * self.sr;
    }

    pub fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::reset);
        self.lp = [0.0; 2];
        self.hp = [0.0; 2];
    }

    /// One sample: mono input in, (left, right) echoes out.
    #[inline]
    pub fn tick(&mut self, input: f32, s: &PingPongSettings) -> (f32, f32) {
        let target = (s.seconds * self.sr).min(self.lines[0].capacity() - 2.0);
        self.time += (target - self.time) * self.smooth;
        let a = self.lines[0].read(self.time - 1.0);
        let b = self.lines[1].read(self.time - 1.0);
        let mut fb = [b, a];
        for (c, v) in fb.iter_mut().enumerate() {
            self.lp[c] += (*v - self.lp[c]) * self.lp_coef;
            self.hp[c] += (self.lp[c] - self.hp[c]) * self.hp_coef;
            *v = soft((self.lp[c] - self.hp[c]) * s.feedback);
        }
        // Left line: input plus the right echo. Right line: the left echo.
        self.lines[0].push(input + fb[0]);
        self.lines[1].push(fb[1]);
        (a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_alternate_sides() {
        let sr = 48_000.0;
        let mut pp = PingPong::new(sr);
        let s = PingPongSettings { seconds: 0.1, feedback: 0.5 };
        pp.init(&s);
        let mut out = Vec::new();
        for i in 0..20_000 {
            out.push(pp.tick(if i == 0 { 1.0 } else { 0.0 }, &s));
        }
        let peak = |from: usize, to: usize, right: bool| {
            out[from..to]
                .iter()
                .map(|(l, r)| if right { r.abs() } else { l.abs() })
                .fold(0.0f32, f32::max)
        };
        // First echo left at 100 ms, second right at 200 ms.
        assert!(peak(4700, 5100, false) > 0.5);
        assert!(peak(4700, 5100, true) < 1e-3);
        assert!(peak(9500, 9900, true) > 0.2);
        assert!(peak(9500, 9900, false) < 1e-3);
    }

    #[test]
    fn synced_time_follows_tempo() {
        let s = delay_seconds(true, Division::Eighth, 0.0, 120.0);
        assert!((s - 0.25).abs() < 1e-6);
        let d = delay_seconds(true, Division::QuarterDotted, 0.0, 120.0);
        assert!((d - 0.75).abs() < 1e-6);
    }
}
