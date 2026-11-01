//! reusemic: denoise a microphone with NVIDIA Real-time RE-USE and publish it as the
//! "ReuseMic" virtual microphone (provided by the ReuseMic.driver HAL plug-in).

mod coreaudio;
mod resample;
mod ring;

use coreaudio::{ensure_float, AudioBuffer, Device, IoProc};
use eyre::{bail, eyre, Result, WrapErr};
use resample::Resampler;
use reuse_rs::{Config, Denoiser};
use ring::Ring;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIC_UID: &str = "ReuseMic_Device_UID";
const FEED_UID: &str = "ReuseMic_Feed_UID";
const MODEL_FILE: &str = "Real-time_RE-USE.safetensors";

struct Args {
    list: bool,
    input: Option<String>,
    channel: usize,
    layers: usize,
    look_ahead: usize,
    rate: u32,
    buffer_ms: f64,
    model: Option<PathBuf>,
    bypass: bool,
    quiet: bool,
    file: Option<(PathBuf, PathBuf)>,
    probe: bool,
    test_tone: bool,
    probe_wav: Option<PathBuf>,
}

const USAGE: &str = "\
usage: reusemic [options]

  --list              list audio devices and exit
  --test-tone         replace the mic signal with a 440 Hz tone (pipeline check)
  --probe             print the level arriving at the ReuseMic input for 5 s and exit
  --probe-wav <path>  same, and save what arrived as a wav
  --input <name>      microphone to enhance (name or UID, substring match).
                      Default: the system default input (unless it is ReuseMic)
  --channel <n>       input channel to use, 0-based (default 0)
  --layers <3-12>     Mamba layers; fewer is faster (default 4)
  --look-ahead <0-2>  future 20 ms frames the model may use (default 0)
  --rate <hz>         model sample rate: 8000 16000 22050 24000 32000 44100 48000
                      (default 16000; higher needs much more CPU)
  --buffer <ms>       output jitter buffer (default 60)
  --model <path>      Real-time_RE-USE model.safetensors
  --bypass            start with enhancement off (press b + Enter to toggle)
  --quiet             no status line
  --file <in> <out>   offline check: run a wav through the same pipeline (48 kHz out)

While running: type `b` + Enter to toggle bypass, `q` + Enter to quit.";

fn parse_args() -> Result<Args> {
    let mut a = Args {
        list: false,
        input: None,
        channel: 0,
        layers: 4,
        look_ahead: 0,
        rate: 16000,
        buffer_ms: 60.0,
        model: None,
        bypass: false,
        quiet: false,
        file: None,
        probe: false,
        test_tone: false,
        probe_wav: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| eyre!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--list" => a.list = true,
            "--probe" => a.probe = true,
            "--probe-wav" => {
                a.probe = true;
                a.probe_wav = Some(val("--probe-wav")?.into());
            }
            "--test-tone" => a.test_tone = true,
            "--input" => a.input = Some(val("--input")?),
            "--channel" => a.channel = val("--channel")?.parse()?,
            "--layers" => a.layers = val("--layers")?.parse()?,
            "--look-ahead" => a.look_ahead = val("--look-ahead")?.parse()?,
            "--rate" => a.rate = val("--rate")?.parse()?,
            "--buffer" => a.buffer_ms = val("--buffer")?.parse()?,
            "--model" => a.model = Some(val("--model")?.into()),
            "--bypass" => a.bypass = true,
            "--quiet" => a.quiet = true,
            "--file" => {
                let i = val("--file")?;
                a.file = Some((i.into(), val("--file")?.into()));
            }
            other => bail!("unknown option {other}\n\n{USAGE}"),
        }
    }
    if !(3..=12).contains(&a.layers) {
        bail!("--layers must be 3-12");
    }
    if a.look_ahead > 2 {
        bail!("--look-ahead must be 0-2");
    }
    if !reuse_rs::audio::SAMPLE_RATES.contains(&a.rate) {
        bail!("--rate must be one of {:?}", reuse_rs::audio::SAMPLE_RATES);
    }
    Ok(a)
}

fn model_path(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = arg {
        return Ok(p);
    }
    let mut candidates = Vec::new();
    if let Ok(p) = std::env::var("REUSEMIC_MODEL") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Library/Application Support/reusemic")
                .join(MODEL_FILE),
        );
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(MODEL_FILE));
            candidates.push(dir.join("../share/reusemic").join(MODEL_FILE));
        }
    }
    candidates.push(PathBuf::from("models").join(MODEL_FILE));
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| {
            eyre!(
                "model not found; looked in:\n  {}\nrun `make model` or pass --model",
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            )
        })
}

fn list_devices() -> Result<()> {
    let default = Device::default_input().ok().map(|d| d.id);
    println!("{:>5}  {:>3} {:>3}  {:<40} UID", "id", "in", "out", "name");
    for d in Device::all()? {
        let rate = d
            .sample_rate()
            .map(|r| format!("{r:.0} Hz"))
            .unwrap_or_default();
        let mark = if Some(d.id) == default {
            " (default input)"
        } else {
            ""
        };
        println!(
            "{:>5}  {:>3} {:>3}  {:<40} {}  {rate}{mark}",
            d.id, d.input_channels, d.output_channels, d.name, d.uid
        );
    }
    match Device::by_uid(FEED_UID)? {
        Some(d) => println!("\nReuseMic driver: installed (feed device id {})", d.id),
        None => println!("\nReuseMic driver: NOT installed (run `make install`)"),
    }
    Ok(())
}

fn is_ours(d: &Device) -> bool {
    d.uid == MIC_UID || d.uid == FEED_UID
}

fn pick_input(query: Option<&str>) -> Result<Device> {
    let inputs: Vec<Device> = Device::all()?
        .into_iter()
        .filter(|d| d.input_channels > 0 && !is_ours(d))
        .collect();
    if let Some(q) = query {
        if let Some(d) = inputs.iter().find(|d| d.name == q || d.uid == q) {
            return Ok(d.clone());
        }
        let ql = q.to_lowercase();
        let hits: Vec<&Device> = inputs
            .iter()
            .filter(|d| d.name.to_lowercase().contains(&ql) || d.uid.to_lowercase().contains(&ql))
            .collect();
        return match hits.as_slice() {
            [d] => Ok((*d).clone()),
            [] => bail!("no input device matches {q:?} (see --list)"),
            many => bail!(
                "{q:?} matches several devices: {} (be more specific)",
                many.iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    if let Ok(d) = Device::default_input() {
        if !is_ours(&d) && d.input_channels > 0 {
            return Ok(d);
        }
    }
    // The default input is ReuseMic itself: fall back to the built-in mic, then anything.
    inputs
        .iter()
        .find(|d| d.is_builtin())
        .or(inputs.first())
        .cloned()
        .ok_or_else(|| eyre!("no input devices found"))
}

fn set_realtime_qos() {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relative: i32) -> i32;
    }
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
}

/// Lock-free f32 for the status line.
#[derive(Default)]
struct AtomicF32(AtomicU64);
impl AtomicF32 {
    fn set(&self, v: f32) {
        self.0.store(v.to_bits() as u64, Ordering::Relaxed)
    }
    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed) as u32)
    }
}

#[derive(Default)]
struct Stats {
    underruns: AtomicU64,
    overflows: AtomicU64,
    in_dropped: AtomicU64,
    in_level: AtomicF32,
    out_level: AtomicF32,
    frame_ms: AtomicF32,
    frame_max_ms: AtomicF32,
    fill_ms: AtomicF32,
    adjust: AtomicF32,
}

fn db(rms: f32) -> f32 {
    20.0 * rms.max(1e-6).log10()
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    if args.list {
        return list_devices();
    }
    if args.probe {
        return probe(args.probe_wav.as_deref());
    }
    if let Some((i, o)) = &args.file {
        return process_file(&args, i, o);
    }

    // Model threads get the same priority as the processing thread.
    rayon::ThreadPoolBuilder::new()
        .start_handler(|_| set_realtime_qos())
        .build_global()
        .ok();

    let feed = Device::by_uid(FEED_UID)?
        .ok_or_else(|| eyre!("ReuseMic driver is not installed (run `make install`)"))?;
    let input = pick_input(args.input.as_deref())?;
    if args.channel >= input.input_channels as usize {
        bail!(
            "{} has {} input channel(s); --channel {} is out of range",
            input.name,
            input.input_channels,
            args.channel
        );
    }

    let in_rate = input.sample_rate()?;
    let feed_rate = feed.sample_rate()?;
    let in_fmt = input.format(true)?;
    let feed_fmt = feed.format(false)?;
    ensure_float(&in_fmt, &input.name)?;
    ensure_float(&feed_fmt, &feed.name)?;
    let _ = input.set_buffer_frames(256);
    let _ = feed.set_buffer_frames(256);

    let path = model_path(args.model.clone())?;
    let config = Config {
        layers: args.layers,
        sample_rate: args.rate,
        look_ahead: args.look_ahead,
    };
    let mut denoiser = Denoiser::with_config(&path, config)
        .wrap_err_with(|| format!("loading {}", path.display()))?;
    let model_latency_ms = denoiser.latency() as f64 * 1000.0 / args.rate as f64;

    eprintln!(
        "input : {} ({} Hz, {} ch, using ch {})",
        input.name, in_rate, input.input_channels, args.channel
    );
    eprintln!("output: ReuseMic ({} Hz)", feed_rate);
    eprintln!(
        "model : {} | {} layers, look-ahead {}, {} Hz, {:.0} ms algorithmic latency",
        path.display(),
        args.layers,
        args.look_ahead,
        args.rate,
        model_latency_ms
    );

    let mic_ring = Arc::new(Ring::new(in_rate as usize * 2));
    let out_ring = Arc::new(Ring::new(feed_rate as usize * 2));
    let stats = Arc::new(Stats::default());
    let bypass = Arc::new(AtomicBool::new(args.bypass));
    let target = (args.buffer_ms / 1000.0 * feed_rate) as usize;

    // --- Microphone IOProc: pick one channel, push it to mic_ring.
    let channel = args.channel;
    let mic_proc = {
        let ring = mic_ring.clone();
        let stats = stats.clone();
        let mut scratch = vec![0f32; 8192];
        IoProc::start(
            input.id,
            move |inp: &[AudioBuffer], _out: &mut [AudioBuffer]| {
                let mut first = 0usize;
                for b in inp {
                    let nch = b.mNumberChannels.max(1) as usize;
                    if channel < first + nch {
                        let c = channel - first;
                        let frames = b.mDataByteSize as usize / 4 / nch;
                        let data = unsafe {
                            std::slice::from_raw_parts(b.mData as *const f32, frames * nch)
                        };
                        let mut f0 = 0;
                        while f0 < frames {
                            let n = (frames - f0).min(scratch.len());
                            for (i, s) in scratch[..n].iter_mut().enumerate() {
                                *s = data[(f0 + i) * nch + c];
                            }
                            f0 += n;
                            let pushed = ring.push(&scratch[..n]);
                            if pushed < n {
                                stats
                                    .in_dropped
                                    .fetch_add((n - pushed) as u64, Ordering::Relaxed);
                            }
                        }
                        break;
                    }
                    first += nch;
                }
            },
        )
        .wrap_err_with(|| format!("starting {}", input.name))?
    };

    // --- Feed IOProc: pop from out_ring with a priming jitter buffer.
    let feed_proc = {
        let ring = out_ring.clone();
        let stats = stats.clone();
        let mut primed = false;
        let mut mono = vec![0f32; 8192];
        IoProc::start(
            feed.id,
            move |_inp: &[AudioBuffer], out: &mut [AudioBuffer]| {
                let frames = out
                    .first()
                    .map(|b| b.mDataByteSize as usize / 4 / b.mNumberChannels.max(1) as usize)
                    .unwrap_or(0)
                    .min(mono.len());
                let fill = ring.len();
                if fill > target * 3 + 4096 {
                    ring.skip(fill - target);
                    stats.overflows.fetch_add(1, Ordering::Relaxed);
                }
                if !primed && ring.len() >= target + frames {
                    // Start exactly at the target fill, dropping whatever piled up before the first callback.
                    ring.skip(ring.len() - target - frames);
                    primed = true;
                }
                let mut got = 0;
                if primed {
                    got = ring.pop(&mut mono[..frames]);
                    if got < frames {
                        primed = false;
                        stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
                mono[got..frames].fill(0.0);
                for b in out.iter_mut() {
                    let nch = b.mNumberChannels.max(1) as usize;
                    let n = (b.mDataByteSize as usize / 4 / nch).min(frames);
                    let data =
                        unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, n * nch) };
                    for (f, v) in mono[..n].iter().enumerate() {
                        data[f * nch..(f + 1) * nch].fill(*v);
                    }
                }
            },
        )
        .wrap_err("starting ReuseMic feed")?
    };

    // --- Keyboard control.
    let quit = Arc::new(AtomicBool::new(false));
    {
        let bypass = bypass.clone();
        let quit = quit.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                match line.trim() {
                    "b" => {
                        let now = !bypass.load(Ordering::Relaxed);
                        bypass.store(now, Ordering::Relaxed);
                        eprintln!(
                            "\nbypass {}",
                            if now {
                                "ON (raw mic)"
                            } else {
                                "OFF (enhanced)"
                            }
                        );
                    }
                    "q" => quit.store(true, Ordering::Relaxed),
                    _ => {}
                }
            }
        });
    }

    // --- Status line.
    if !args.quiet {
        let stats = stats.clone();
        let bypass = bypass.clone();
        let extra_ms = model_latency_ms;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let s = &stats;
            eprint!(
                "\r{} in {:>6.1} dB  out {:>6.1} dB | model {:>5.1} ms/frame (max {:>5.1}) | buffer {:>3.0} ms | latency ~{:>3.0} ms | drift {:+.3}% | underruns {} \x1b[K",
                if bypass.load(Ordering::Relaxed) { "[BYPASS]" } else { "[RE-USE]" },
                db(s.in_level.get()),
                db(s.out_level.get()),
                s.frame_ms.get(),
                s.frame_max_ms.get(),
                s.fill_ms.get(),
                s.fill_ms.get() as f64 + extra_ms + 2.0 * 256.0 / 48.0,
                s.adjust.get() * 100.0,
                s.underruns.load(Ordering::Relaxed),
            );
            s.frame_max_ms.set(0.0);
        });
    }

    // --- Processing loop: mic rate -> model rate -> enhance -> feed rate (drift-corrected).
    set_realtime_qos();
    let model_rate = args.rate as f64;
    let mut down = Resampler::new(in_rate, model_rate);
    let mut up = Resampler::new(model_rate, feed_rate);
    let mut raw = vec![0f32; in_rate as usize];
    let mut model_in = Vec::new();
    let mut feed_out = Vec::new();
    let mut fill_ema = target as f64;
    let mut last = Instant::now();
    let frame = denoiser.frame();
    let mut tone_phase = 0f64;

    while !quit.load(Ordering::Relaxed) {
        let n = mic_ring.pop(&mut raw);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        if args.test_tone {
            for v in &mut raw[..n] {
                *v = 0.1 * (tone_phase * std::f64::consts::TAU).sin() as f32;
                tone_phase = (tone_phase + 440.0 / in_rate).fract();
            }
        }
        stats.in_level.set(rms(&raw[..n]));

        model_in.clear();
        down.process(&raw[..n], &mut model_in);

        let started = Instant::now();
        let enhanced = denoiser.process(&model_in)?;
        if !enhanced.is_empty() {
            let ms =
                started.elapsed().as_secs_f32() * 1000.0 / (enhanced.len() as f32 / frame as f32);
            stats.frame_ms.set(stats.frame_ms.get() * 0.9 + ms * 0.1);
            stats.frame_max_ms.set(stats.frame_max_ms.get().max(ms));
        }
        // Bypass keeps the model running (so toggling is seamless) but sends the raw input.
        let output: &[f32] = if bypass.load(Ordering::Relaxed) {
            &model_in
        } else {
            &enhanced
        };
        stats.out_level.set(rms(output));

        // Drift control: steer the output resampler so the feed buffer hovers at `target`.
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        let fill = out_ring.len() as f64;
        let a = (dt / 2.0).min(1.0);
        fill_ema += (fill - fill_ema) * a;
        let err_s = (fill_ema - target as f64) / feed_rate;
        let adjust = (err_s * 0.05).clamp(-0.005, 0.005);
        up.set_adjust(adjust);
        stats.adjust.set(adjust as f32);
        stats.fill_ms.set((fill / feed_rate * 1000.0) as f32);

        feed_out.clear();
        up.process(output, &mut feed_out);
        out_ring.push(&feed_out);
    }

    drop(mic_proc);
    drop(feed_proc);
    eprintln!();
    Ok(())
}

/// Runs a wav through the same resample -> enhance -> resample chain as the live path,
/// in 10 ms chunks, and writes 48 kHz output.
fn process_file(args: &Args, input: &std::path::Path, output: &std::path::Path) -> Result<()> {
    let (wav, in_rate) = reuse_rs::audio::load_wav(&input.to_string_lossy())?;
    let path = model_path(args.model.clone())?;
    let config = Config {
        layers: args.layers,
        sample_rate: args.rate,
        look_ahead: args.look_ahead,
    };
    let mut denoiser = Denoiser::with_config(&path, config)?;
    let out_rate = 48000.0;
    let mut down = Resampler::new(in_rate as f64, args.rate as f64);
    let mut up = Resampler::new(args.rate as f64, out_rate);
    let mut out = Vec::new();
    let mut model_in = Vec::new();
    let started = Instant::now();
    for chunk in wav.chunks(in_rate as usize / 100) {
        model_in.clear();
        down.process(chunk, &mut model_in);
        let enhanced = if args.bypass {
            model_in.clone()
        } else {
            denoiser.process(&model_in)?
        };
        up.process(&enhanced, &mut out);
    }
    let secs = wav.len() as f64 / in_rate as f64;
    eprintln!(
        "{:.1} s of audio in {:.1} s ({:.2}x real time)",
        secs,
        started.elapsed().as_secs_f64(),
        secs / started.elapsed().as_secs_f64()
    );
    reuse_rs::audio::save_wav(&output.to_string_lossy(), &out, out_rate as u32)?;
    eprintln!("wrote {}", output.display());
    Ok(())
}

/// Records from the ReuseMic input device (as any app would) and prints its level.
fn probe(save: Option<&std::path::Path>) -> Result<()> {
    let mic = Device::by_uid(MIC_UID)?.ok_or_else(|| eyre!("ReuseMic driver is not installed"))?;
    let level = Arc::new(AtomicF32::default());
    let frames = Arc::new(AtomicU64::new(0));
    let rate = mic.sample_rate()?;
    let rec = Arc::new(Ring::new(rate as usize * 6));
    let proc_ = {
        let level = level.clone();
        let frames = frames.clone();
        let rec = rec.clone();
        IoProc::start(
            mic.id,
            move |inp: &[AudioBuffer], _out: &mut [AudioBuffer]| {
                if let Some(b) = inp.first() {
                    let data = unsafe {
                        std::slice::from_raw_parts(
                            b.mData as *const f32,
                            b.mDataByteSize as usize / 4,
                        )
                    };
                    level.set(level.get().max(rms(data)));
                    rec.push(data);
                    frames.fetch_add(
                        data.len() as u64 / b.mNumberChannels.max(1) as u64,
                        Ordering::Relaxed,
                    );
                }
            },
        )?
    };
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(500));
        println!(
            "ReuseMic input: {:>6.1} dB peak-rms, {} frames",
            db(level.get()),
            frames.load(Ordering::Relaxed)
        );
        level.set(0.0);
    }
    drop(proc_);
    if let Some(path) = save {
        let mut buf = vec![0f32; rec.len()];
        rec.pop(&mut buf);
        reuse_rs::audio::save_wav(&path.to_string_lossy(), &buf, rate as u32)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
