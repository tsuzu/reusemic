# reusemic

[![CI](https://github.com/tsuzu/reusemic/actions/workflows/ci.yml/badge.svg)](https://github.com/tsuzu/reusemic/actions/workflows/ci.yml)

Real-time noise suppression for your microphone with NVIDIA's RE-USE speech enhancement models, published on macOS as a virtual microphone called **ReuseMic**. Pick "ReuseMic" as the input device in Zoom, Meet, Discord, OBS or any other app.

- No third-party tools such as BlackHole: the virtual device comes from the bundled HAL plug-in.
- No Python: the driver is written in C and the app in Rust. Inference runs on the CPU (via Accelerate) through [reuse-rs](https://github.com/altunenes/re-use-rs).
- At runtime you only need the app binary, the driver and the model weights.

## The model

[nvidia/RE-USE](https://huggingface.co/nvidia/RE-USE) is an offline model built on bidirectional Mamba, and it expects several seconds of audio at a time. For a live microphone, reusemic uses the streaming model from the same family, [nvidia/Real-time_RE-USE](https://huggingface.co/nvidia/Real-time_RE-USE), which is causal and works in 20 ms frames.

> The weights are licensed under the NVIDIA One-Way Noncommercial License (research and non-commercial use only).

## How it works

```
physical mic ──IOProc──▶ [reusemic]
                           resample 48k→16k → Real-time RE-USE → resample 16k→48k (drift-corrected)
                                     │
                                     ▼ IOProc (output)
                 ┌──────────── ReuseMic.driver (HAL plug-in) ────────────┐
                 │  "ReuseMic Feed" (hidden output) ─ring buffer─▶ "ReuseMic" (input) │
                 └────────────────────────────────────────────────────────┘
                                     │
                                     ▼
                            Zoom / Meet / Discord …
```

- The two devices share one clock and one ring buffer. Audio written to the feed at sample time T is read back by the mic at sample time T.
- While reusemic is not running, ReuseMic outputs silence.
- Clock drift between the physical mic and the virtual device is absorbed by nudging the output resampler's ratio slightly (at most ±0.5%).

## Requirements

- macOS 11 or later (Apple Silicon recommended)
- Xcode Command Line Tools (`clang`, `codesign`)
- Rust (`cargo`)

## Build and install

```sh
make            # build the driver and the app
make model      # download the weights (~54 MB) into models/
make test       # driver and unit tests
make install    # copy the driver to /Library/Audio/Plug-Ins/HAL and restart coreaudiod (sudo)
```

## Usage

```sh
./target/release/reusemic --list                 # list audio devices
./target/release/reusemic                        # enhance the default input device into ReuseMic
./target/release/reusemic --input "MacBook Pro"  # pick a specific microphone
```

Then select **ReuseMic** as the input device in your call app. The first run asks for microphone permission for your terminal.

### Which microphone is used

By default reusemic captures the system default input device (System Settings → Sound → Input). If the default input is ReuseMic itself, it falls back to the built-in microphone, then to the first input device it finds. Use `--input` with part of a device name or UID (case-insensitive) to choose another one. The `input : ...` line printed at startup shows the device in use.

For the cleanest setup, leave the system default input on your physical mic and choose ReuseMic only in the apps that should get the enhanced audio.

### Keys while running

- `b` + Enter toggles bypass, to compare against the raw mic.
- `q` + Enter quits.

### Options

| Option | Default | Description |
|---|---|---|
| `--input name` | system default | Microphone to enhance (name or UID, substring match) |
| `--layers 3-12` | 4 | Number of Mamba layers: more is higher quality but heavier |
| `--look-ahead 0-2` | 0 | Future frames the model may use; each one adds 20 ms of latency |
| `--rate` | 16000 | Sample rate the model runs at; 48000 needs about 3× the CPU |
| `--buffer ms` | 60 | Output jitter buffer; raise it if you see underruns |
| `--channel n` | 0 | Channel to use on multi-channel microphones (0-based) |
| `--model path` | auto | Path to the weights file |
| `--bypass` | | Start with enhancement off |
| `--quiet` | | No status line |
| `--file in.wav out.wav` | | Run a wav through the same pipeline as the live path |
| `--probe` | | Print the level arriving at the ReuseMic input |
| `--probe-wav out.wav` | | Same, and save what arrived as a wav |
| `--test-tone` | | Replace the mic with a 440 Hz tone to check the path (the model removes it, so combine with `--bypass`) |

When `model xx ms/frame` in the status line stays under 20 ms, processing keeps up in real time. Latency is roughly buffer + 20 ms + look-ahead + device buffers, about 90 ms with the defaults.

## Uninstall

```sh
make uninstall
```

## License

The code in this repository is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

The model weights ([nvidia/Real-time_RE-USE](https://huggingface.co/nvidia/Real-time_RE-USE)) are not included. `make model` downloads them from NVIDIA's distribution, and they are used under the **NVIDIA One-Way Noncommercial License (NSCLv1)**, which allows research and non-commercial use only. Keep that license in mind for anything you produce with the weights.
