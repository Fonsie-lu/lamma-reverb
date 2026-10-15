# Lemma: project notes for Claude

A reverb effect plugin for **Bitwig Studio on Linux**. It has four sections: an amp that adds
harmonics, a reverb with a tempo-synced ping-pong delay, a key-tracking harmonic EQ and a low /
high cut filter. The GUI copies the look and the code structure of `~/tools/juno-clone`; read
that project's CLAUDE.md for the background of the shared patterns.

## Stack and commands

The stack and the build machine are the same as juno-clone: Rust, nih-plug (git, rev pinned in
`Cargo.lock`) and `nih_plug_egui` (egui 0.31). The plugin is exported as CLAP and VST3, and the
user should use the CLAP version in Bitwig.

```sh
./install.sh                                   # bundles, symlinked into ~/.clap and ~/.vst3
cargo test --release --lib                     # DSP unit tests + headless GUI tests
cargo run --release --example render -- <dir>  # every preset to WAV; peak, RMS, tail, NaN, key, CPU
cargo run --release --example gui_preview      # GUI without host; LEMMA_DEMO=1, LEMMA_THEME=0..3
```

For screenshots, use the same workflow as juno-clone, with the window title
"Lemma Preview". To check the WAV output, run
`ffmpeg -i x.wav -lavfi showspectrumpic=s=900x380:scale=log out.png` and then Read the PNG.

## Layout

| File | Role |
|------|------|
| `src/lib.rs` | `Plugin` impl, `settings_from_params()`, note events into the key detector, reports latency |
| `src/params.rs` | `ReverbParams` (40 params) and the enums `AmpType`, `Division`, `HarmSource`, `HarmMode`, `HarmSelect`, `Slope` |
| `src/engine.rs` | Host-independent chain, `Settings` snapshot, 64-sample chunks, section crossfades, global dry/wet against the latency-delayed input, `publish()` |
| `src/amp.rs` | Waveshapers (`curve`, `shape`, `compensation`) at 4x oversampling; bias for even harmonics; `harmonic_spectrum` for the GUI |
| `src/reverb.rs` | 8-line FDN (Hadamard), pre-delay, 4 allpass diffusers per channel, damping, modulation |
| `src/pingpong.rs` | Cross-fed stereo delay; `delay_seconds()` handles sync and free time |
| `src/harmonic.rs` | `KeyDetector` (Krumhansl-Kessler, decaying histogram, hysteresis) and `HarmonicEq` (bell or bandpass bank, up to 32 bands, f64 SVFs) |
| `src/filter.rs` | Butterworth HP + LP, 12 / 24 dB, resonance on the last stage |
| `src/dsp.rs` | `Svf`/`SvfCoefs` (Simper, f64, exact `response()`), `DelayLine`, `Oversampler` (129-tap polyphase, latency `OS_LATENCY` = 32), `dot`, `tanh` |
| `src/editor.rs` + `src/editor_tests.rs` | GUI and headless tests (harness copied from juno-clone) |
| `src/theme.rs` | The same 4 themes as juno-clone, with `Section` = Amp, Reverb, PingPong, Harmonic, Filter, Output |
| `src/presets.rs` | Factory presets as `(param_id, plain value)` |
| `examples/render.rs` | Its `set()` maps preset IDs to `Settings` fields by hand; update it when adding params |

## Decisions

- **Latency:** the amp always runs its oversampler, except when it is fully bypassed. Then a
  32-sample delay keeps the latency constant, so toggling the amp never shifts timing.
- **Section switches:** they are smoothed gains (crossfades), not hard bypasses.
- **Reverb section:** the reverb input is the section input plus the echoes times INTO REVERB.
  The wet signal is the reverb plus the echoes times ECHO LEVEL, followed by width and an
  equal-power mix.
- **Harmonic EQ root:**
  - The fundamental is `note_hz(12*(octave+1) + pc)`.
  - In KEY mode the root is the detected tonic. The manual root is the fallback until the first
    notes arrive.
  - NOTE mode uses the lowest held note and keeps the last one after release.
- **Harmonic EQ updates:** coefficients are recomputed per 32-sample chunk while f0, Q or gain
  glide (40 ms). Switching between PEAK and ISO resets the filter states.
- **GUI rules:** the same as juno-clone.
  - Read values only through `G::shown`, and write them only through `G::send` / `G::change`.
  - Never hard-code colours.
  - The PING-PONG TIME fader shows `pp_division` while SYNC is on and `pp_time` while it is off.
  - The filter handles use `Axis::LogHz`, so they stay under the pointer on the log axis.
  - The keyboard in the key panel sets `harm_root`.
- Plugin name "Lemma", vendor "Fonsie Audio". The plugin was renamed from "Overtone Reverb" before
  any release, which changed the IDs once.
- **Never rename** parameter IDs, `CLAP_ID` (`ch.fonsie.lemma`) or `VST3_CLASS_ID` (`FonsieAudioLemma`).

## Status

- 33 tests pass.
- Measured CPU, single instance, 48 kHz, on the Ryzen 3500U: about 2-4 % of a core with
  everything on. The amp's oversampling costs the most.
- Not yet checked in Bitwig itself: GUI, state save / restore, and how notes reach the plugin.
  clap-validator has not been run.
- Ideas:
  - The key detector's memory as a parameter.
  - Per-harmonic gain tilt.
  - A freeze function.
