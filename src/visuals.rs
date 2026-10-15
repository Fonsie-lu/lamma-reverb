//! Lock-free state shared from the audio thread to the editor.
//!
//! Scalars go through relaxed atomics; tearing between them is invisible at 60-odd frames a
//! second. The scope frame goes through a triple buffer, because a half-updated waveform reads
//! as a glitch and because the GUI only ever wants the newest complete frame.
//!
//! Nothing here ever locks or allocates on the audio thread.

use atomic_float::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU16, AtomicU8, Ordering};

/// Horizontal resolution of the oscilloscope.
pub const SCOPE_COLUMNS: usize = 256;
/// Visible time window.
pub const SCOPE_WINDOW_S: f32 = 0.021;

const RELAXED: Ordering = Ordering::Relaxed;

pub struct VisualState {
    pub in_l: AtomicF32,
    pub in_r: AtomicF32,
    pub out_l: AtomicF32,
    pub out_r: AtomicF32,
    /// Peak level at the amp's shaper input (after drive), decaying.
    pub drive: AtomicF32,

    /// Key detector pitch class histogram, normalised to sum 1.
    pub hist: [AtomicF32; 12],
    /// Detected key root (pitch class), -1 while nothing was detected.
    pub key_root: AtomicI8,
    pub key_minor: AtomicBool,
    pub key_score: AtomicF32,
    /// Held pitch classes, bit 0 = C.
    pub held: AtomicU16,
    /// Root actually used by the harmonic EQ and its fundamental.
    pub root: AtomicU8,
    pub f0: AtomicF32,
    /// Host tempo, for the delay display.
    pub tempo: AtomicF32,
}

impl Default for VisualState {
    fn default() -> Self {
        Self {
            in_l: AtomicF32::new(0.0),
            in_r: AtomicF32::new(0.0),
            out_l: AtomicF32::new(0.0),
            out_r: AtomicF32::new(0.0),
            drive: AtomicF32::new(0.0),
            hist: std::array::from_fn(|_| AtomicF32::new(0.0)),
            key_root: AtomicI8::new(-1),
            key_minor: AtomicBool::new(false),
            key_score: AtomicF32::new(0.0),
            held: AtomicU16::new(0),
            root: AtomicU8::new(9),
            f0: AtomicF32::new(110.0),
            tempo: AtomicF32::new(120.0),
        }
    }
}

impl VisualState {
    /// Zero the meters, so a stopped transport does not leave them frozen.
    pub fn clear(&self) {
        for m in [&self.in_l, &self.in_r, &self.out_l, &self.out_r, &self.drive] {
            m.store(0.0, RELAXED);
        }
        self.held.store(0, RELAXED);
    }

    #[inline]
    pub fn store_peaks(&self, l: &AtomicF32, r: &AtomicF32, peaks: [f32; 2]) {
        // Decay on the audio thread so the meter falls smoothly regardless of how often the
        // editor happens to redraw.
        let decay = 0.94;
        l.store((l.load(RELAXED) * decay).max(peaks[0]), RELAXED);
        r.store((r.load(RELAXED) * decay).max(peaks[1]), RELAXED);
    }
}

/// One complete scope frame. A min/max envelope rather than raw samples, so
/// the display stays honest under heavy decimation instead of aliasing.
#[derive(Clone, Copy)]
pub struct ScopeFrame {
    pub min: [f32; SCOPE_COLUMNS],
    pub max: [f32; SCOPE_COLUMNS],
}

impl Default for ScopeFrame {
    fn default() -> Self {
        Self {
            min: [0.0; SCOPE_COLUMNS],
            max: [0.0; SCOPE_COLUMNS],
        }
    }
}

pub type ScopeOutput = triple_buffer::Output<ScopeFrame>;

/// Audio-thread half of the scope. Lives in the plugin struct and is never
/// shared, so it needs no synchronisation of its own.
pub struct ScopeInput {
    buffer: triple_buffer::Input<ScopeFrame>,
    samples_per_column: u32,
    column: usize,
    in_column: u32,
    cur_min: f32,
    cur_max: f32,
    prev: f32,
    /// Waiting for a rising zero crossing before starting the next frame,
    /// which keeps the displayed waveform from sliding sideways.
    armed: bool,
    /// Give up waiting for a trigger after this many samples so quiet or
    /// DC-free material still refreshes.
    since_trigger: u32,
    trigger_timeout: u32,
}

impl ScopeInput {
    pub fn new() -> (Self, ScopeOutput) {
        let (buffer, output) = triple_buffer::TripleBuffer::new(&ScopeFrame::default()).split();
        let mut input = Self {
            buffer,
            samples_per_column: 2,
            column: 0,
            in_column: 0,
            cur_min: f32::INFINITY,
            cur_max: f32::NEG_INFINITY,
            prev: 0.0,
            armed: true,
            since_trigger: 0,
            trigger_timeout: 4_800,
        };
        input.set_sample_rate(48_000.0);
        (input, output)
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.samples_per_column =
            ((sample_rate * SCOPE_WINDOW_S) / SCOPE_COLUMNS as f32).round().max(1.0) as u32;
        self.trigger_timeout = (sample_rate / 10.0) as u32;
    }

    pub fn reset(&mut self) {
        self.column = 0;
        self.in_column = 0;
        self.cur_min = f32::INFINITY;
        self.cur_max = f32::NEG_INFINITY;
        self.prev = 0.0;
        self.armed = true;
        self.since_trigger = 0;
    }

    #[inline(always)]
    pub fn push(&mut self, sample: f32) {
        if self.armed {
            self.since_trigger += 1;
            let rising = self.prev <= 0.0 && sample > 0.0;
            self.prev = sample;
            if !rising && self.since_trigger < self.trigger_timeout {
                return;
            }
            self.armed = false;
            self.since_trigger = 0;
        }
        self.prev = sample;

        self.cur_min = self.cur_min.min(sample);
        self.cur_max = self.cur_max.max(sample);
        self.in_column += 1;
        if self.in_column < self.samples_per_column {
            return;
        }

        // `input_buffer()` hands back the write side directly, so there is no
        // intermediate copy. Safe here only because every column is written
        // before each publish.
        let frame = self.buffer.input_buffer();
        frame.min[self.column] = self.cur_min;
        frame.max[self.column] = self.cur_max;

        self.cur_min = f32::INFINITY;
        self.cur_max = f32::NEG_INFINITY;
        self.in_column = 0;
        self.column += 1;

        if self.column >= SCOPE_COLUMNS {
            self.buffer.publish();
            self.column = 0;
            self.armed = true;
        }
    }
}
