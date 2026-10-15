//! Offline test harness: runs a test signal (plucked saw chords in D minor, with matching MIDI
//! notes for the key detector) through every preset, writes one WAV per preset and prints peak,
//! RMS, tail level, NaN check, the detected key and the CPU load.
//! Usage: cargo run --release --example render -- <out dir>
use lemma::engine::{Engine, Settings};
use lemma::params::{AmpType, Division, HarmMode, HarmSelect, HarmSource, ReverbParams, Slope};
use lemma::pingpong::delay_seconds;
use lemma::presets::PRESETS;
use lemma::settings_from_params;
use std::io::Write;
use std::time::Instant;

const SR: f32 = 48_000.0;
const TEMPO: f32 = 120.0;

/// Applies one preset value (plain parameter value) to the settings. Keep in sync with
/// `settings_from_params` when adding parameters.
fn set(s: &mut Settings, sync: &mut (bool, Division, f32), id: &str, v: f32) {
    let db = |v: f32| 10f32.powf(v / 20.0);
    let b = v > 0.5;
    let i = v as usize;
    match id {
        "amp_on" => s.amp.on = b,
        "amp_type" => s.amp.kind = [AmpType::Tube, AmpType::Tape, AmpType::Fuzz, AmpType::Fold][i],
        "amp_drive" => s.amp.drive = db(v),
        "amp_bias" => s.amp.bias = v,
        "amp_tone" => s.amp.tone_hz = v,
        "amp_mix" => s.amp.mix = v,
        "amp_level" => s.amp.level = db(v),
        "verb_on" => s.verb_on = b,
        "verb_mix" => s.verb_mix = v,
        "verb_predelay" => s.reverb.predelay_ms = v,
        "verb_size" => s.reverb.size = v,
        "verb_decay" => s.reverb.decay_s = v,
        "verb_damp" => s.reverb.damp = v,
        "verb_diffusion" => s.reverb.diffusion = v,
        "verb_mod" => s.reverb.modulation = v,
        "verb_width" => s.verb_width = v,
        "pp_on" => s.pp_on = b,
        "pp_sync" => sync.0 = b,
        "pp_division" => {
            use Division::*;
            sync.1 = [
                ThirtySecond, SixteenthTriplet, Sixteenth, SixteenthDotted, EighthTriplet, Eighth,
                EighthDotted, QuarterTriplet, Quarter, QuarterDotted, Half, HalfDotted, Whole,
            ][i]
        }
        "pp_time" => sync.2 = v,
        "pp_feedback" => s.pp.feedback = v,
        "pp_level" => s.pp_level = v,
        "pp_send" => s.pp_send = v,
        "harm_on" => s.harm.on = b,
        "harm_source" => s.harm.source = [HarmSource::Key, HarmSource::Note, HarmSource::Manual][i],
        "harm_root" => s.harm.root = i as u8,
        "harm_octave" => s.harm.octave = i as u8,
        "harm_mode" => s.harm.mode = [HarmMode::Peak, HarmMode::Isolate][i],
        "harm_select" => s.harm.select = [HarmSelect::All, HarmSelect::Even, HarmSelect::Odd][i],
        "harm_q" => s.harm.q = v,
        "harm_gain" => s.harm.gain_db = v,
        "harm_count" => s.harm.count = i,
        "harm_mix" => s.harm.mix = v,
        "filt_on" => s.cut.on = b,
        "filt_low" => s.cut.low_hz = v,
        "filt_high" => s.cut.high_hz = v,
        "filt_slope" => s.cut.slope = [Slope::Db12, Slope::Db24][i],
        "filt_reso" => s.cut.resonance = v,
        "out_mix" => s.out_mix = v,
        "out_gain" => s.out_gain = db(v),
        _ => panic!("render.rs does not know preset parameter {id}"),
    }
}

fn write_wav(path: &std::path::Path, l: &[f32], r: &[f32]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (l.len() * 2 * 4) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&(SR as u32).to_le_bytes())?;
    f.write_all(&(SR as u32 * 8).to_le_bytes())?;
    f.write_all(&8u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for (a, b) in l.iter().zip(r) {
        f.write_all(&a.to_le_bytes())?;
        f.write_all(&b.to_le_bytes())?;
    }
    Ok(())
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "render".into()));
    std::fs::create_dir_all(&dir).unwrap();
    let chords: [[u8; 3]; 4] = [[50, 53, 57], [46, 50, 53], [43, 46, 50], [45, 49, 52]];
    let seconds = 10.0;
    let n = (SR * seconds) as usize;
    let play = (SR * 4.0) as usize;

    // Dry test signal: plucked saw chords, one per second, then silence for the tail.
    let mut dry = vec![0.0f32; n];
    let mut phases = [0.0f32; 3];
    for (i, x) in dry.iter_mut().enumerate().take(play) {
        let chord = chords[i / SR as usize % 4];
        let age = (i % SR as usize) as f32 / SR;
        let env = (-age * 3.0).exp() * if age < 0.6 { 1.0 } else { 0.0 };
        let mut v = 0.0;
        for (k, note) in chord.iter().enumerate() {
            let hz = 440.0 * ((*note as f32 - 69.0) / 12.0).exp2();
            phases[k] = (phases[k] + hz / SR).fract();
            v += 2.0 * phases[k] - 1.0;
        }
        *x = v * env * 0.15;
    }
    write_wav(&dir.join("dry.wav"), &dry, &dry).unwrap();

    println!("{:<18} {:>7} {:>7} {:>9} {:>5} {:>13} {:>6}", "preset", "peak", "rms", "tail dB", "nan", "key", "cpu %");
    for (pi, preset) in PRESETS.iter().enumerate() {
        let params = ReverbParams::default();
        let mut s = settings_from_params(&params, TEMPO);
        let mut sync = (params.pp_sync.value(), params.pp_division.value(), params.pp_time.value());
        for (id, v) in preset.values {
            set(&mut s, &mut sync, id, *v);
        }
        s.pp.seconds = delay_seconds(sync.0, sync.1, sync.2, TEMPO);

        let mut engine = Engine::new(SR);
        engine.init(&s);
        let (mut l, mut r) = (dry.clone(), dry.clone());
        let block = 256;
        let started = Instant::now();
        let mut current = usize::MAX;
        for pos in (0..n).step_by(block) {
            let end = (pos + block).min(n);
            let index = pos / SR as usize;
            if pos < play && index != current {
                if current != usize::MAX {
                    chords[current % 4].iter().for_each(|x| engine.note_off(*x));
                }
                chords[index % 4].iter().for_each(|x| engine.note_on(*x, 0.8));
                current = index;
            }
            if pos >= play && current != usize::MAX {
                chords[current % 4].iter().for_each(|x| engine.note_off(*x));
                current = usize::MAX;
            }
            engine.process(&mut l[pos..end], &mut r[pos..end], &s);
        }
        let cpu = started.elapsed().as_secs_f32() / seconds * 100.0;

        let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
        let rms = (l.iter().chain(&r).map(|v| v * v).sum::<f32>() / (2 * n) as f32).sqrt();
        let tail = &l[(SR * 5.0) as usize..(SR * 6.0) as usize];
        let tail_rms = (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        let nan = l.iter().chain(&r).any(|v| !v.is_finite());
        let key = engine.key.key.map(|k| k.name()).unwrap_or_else(|| "--".into());
        println!(
            "{:<18} {:>7.3} {:>7.4} {:>9.1} {:>5} {:>13} {:>6.2}",
            preset.name,
            peak,
            rms,
            20.0 * tail_rms.max(1e-9).log10(),
            nan,
            key,
            cpu
        );
        let name = format!("{:02}_{}.wav", pi + 1, preset.name.replace(' ', "_").to_lowercase());
        write_wav(&dir.join(name), &l, &r).unwrap();
    }
}
