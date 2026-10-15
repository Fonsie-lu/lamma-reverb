# Lemma (Fonsie Audio)

A reverb effect for Linux (CLAP + VST3), built for Bitwig Studio with
[nih-plug](https://github.com/robbert-vdh/nih-plug) and an egui editor. The panel has the same look as
the Alpha Juno plugin (`~/tools/juno-clone`): coloured section bars, faders, rotary selectors, an
OLED and graphs, with the same four colour themes.

## Build and install

Requirements: Rust (stable), X11 and OpenGL development libraries.

```sh
./install.sh               # build, symlink into ~/.clap and ~/.vst3; later rebuilds need no reinstall
./install.sh --copy        # copy instead of symlink
./install.sh --system      # copy to /usr/lib/clap and /usr/lib/vst3, via sudo
./install.sh --uninstall   # remove (combine with --system for the system folders)
```

Then rescan plugins in Bitwig Studio (Settings > Locations > Plug-ins). Use the CLAP version in
Bitwig.

## Signal flow

```
audio in -> AMP -> REVERB (+ PING-PONG) -> HARMONIC EQ -> FILTER -> OUTPUT (dry/wet, level)
MIDI in  -> key detector ---------------------^
```

Every section has an ON/OFF switch. Switching crossfades, so it does not click. The amp's
oversampling adds a fixed latency of 32 samples, which the plugin reports to the host. The global
DRY/WET mixes against a latency-matched copy of the input.

### 1. Amp (harmonics)

The amp has four waveshapers at 4x oversampling:

- **Tube**: asymmetric soft clip.
- **Tape**: smooth symmetric saturation.
- **Fuzz**: near hard clip.
- **Fold**: sine wavefolder.

The other controls:

- **DRIVE** sets the gain into the shaper, from 0 to 36 dB. The output level is compensated.
- **BIAS** shifts the operating point. The curve becomes asymmetric and adds even harmonics.
- **TONE** is a lowpass after the shaper.
- **MIX** blends the dry and shaped signal. **LEVEL** sets the output level.

The graph shows the transfer curve and the harmonics 1 to 10 that a sine gets. Odd harmonics are
in the section colour, even harmonics are highlighted. The CLIP LED lights when the shaper is
driven hard.

### 2. Reverb with ping-pong delay

The reverb is an 8-line feedback delay network with a Hadamard matrix, input diffusion, per-line
damping and slow delay modulation.

- **Graph**: drag the top-left handle for PRE-DELAY (0 to 250 ms). Drag the right handle
  sideways for DECAY (0.2 to 20 s) and up / down for DAMPING.
- **SIZE** scales the room, **DIFFUSION** sets how dense the onset is, **MODULATION** sets the
  chorus in the tail, and **WIDTH** sets the stereo width.

The **PING-PONG** block is part of the reverb section. Echoes alternate left / right. The feedback
path is band limited and soft-saturated, like a tape echo.

- **SYNC on**: TIME selects a note value from 1/32 to 1/1, including triplet (T) and dotted (D)
  values. It follows the host tempo.
- **SYNC off**: TIME is free, from 10 ms to 2 s.
- **FEEDBACK** sets the repeats, **ECHO LEVEL** sets the echo level in the output, and
  **INTO REVERB** sets how much of the echoes feeds the reverb (echoes that dissolve into the
  hall).

### 3. Harmonic EQ (key tracking)

This section places a narrow band on every harmonic `n x f0` of a root note.

**ROOT FROM** sets where the root note comes from:

- **KEY**: the key detected from the track's MIDI notes. The detector uses Krumhansl-Kessler key
  profiles over a pitch class histogram that slowly decays (about 20 s memory). Each note counts
  by its velocity and by how long it is held.
- **NOTE**: the lowest held MIDI note.
- **MAN**: the manual root. Click a key on the keyboard display to set it.

The **ROOT OCTAVE** (C0 to C5) puts the fundamental in an octave. For example, A in octave 2 is
110 Hz.

The other controls:

- **MODE PEAK**: peaking bands. A positive GAIN boosts the harmonics, a negative GAIN cuts them
  (notches on the key).
- **MODE ISO** (isolate): bandpasses, so only the harmonics pass. GAIN is then the makeup level.
- **HARMONICS**: ALL, EVEN (2, 4, 6, ...) or ODD (1, 3, 5, ...).
- **Q** sets the width of the bands (1 to 200). **COUNT** sets the number of bands (1 to 32).
  **MIX** blends the dry and EQ signal.

The display shows the detected key, the match score, the root in use and its frequency. It also
shows the histogram over a keyboard, where held notes light up and the root in use is marked
with a dot.

### 4. Filter

The filter is a classic low cut / high cut: Butterworth, 12 or 24 dB/oct (SLOPE). Drag the
handles in the graph sideways for the cutoffs and up / down for resonance at both edges.

### Output

The output section has these parts:

- **OLED**: shows the value of the control under the mouse.
- **Preset buttons**: PREV / NEXT / INIT.
- **Meters**: input and output levels.
- **Scope**: the output signal.
- **Faders**: DRY/WET and OUTPUT.

## MIDI in Bitwig

The key detector needs the notes of the track. Put the plugin on an instrument track after the
instrument; Bitwig passes the track's notes to plugins that accept MIDI. On an audio or FX
track, route notes to the plugin with a Note Receiver or with the plugin device's note input.
Without notes, KEY falls back to the manual root.

## Controls

- Faders and graph handles: drag (Shift for fine), mouse wheel, double-click resets to default.
- Steppers and rotary selectors: drag, click a position or icon, or use the mouse wheel.
- Drag the bottom-right corner to scale the panel.
- THEME chips: Tokyo Night (default), Catppuccin, Dracula, Gruvbox. The project saves the
  choice.

## Development

```sh
cargo test --release --lib                           # DSP and headless GUI tests
cargo run --release --example render -- <dir>        # renders every preset to WAV + stats
cargo run --release --example gui_preview            # GUI in a window, no host
LEMMA_DEMO=1 cargo run --release --example gui_preview   # with a chord progression feeding meters / key display
```

Panel fonts: DejaVu Sans (see `assets/fonts/LICENSE-DejaVu`).
