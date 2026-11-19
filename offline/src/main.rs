//! wisperflow-linux: hold a hotkey, speak, release -> text is typed into the focused window.
//! X11 only (polls the keymap), local transcription via whisper.cpp.

use anyhow::{bail, Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

const WHISPER_RATE: u32 = 16_000;
const SOUND_START: &str = "/usr/share/sounds/freedesktop/stereo/audio-volume-change.oga";
const SOUND_STOP: &str = "/usr/share/sounds/freedesktop/stereo/complete.oga";

#[derive(Parser)]
#[command(about = "Wispr Flow-style push-to-talk dictation for Linux (X11)")]
struct Args {
    /// Hotkey to hold, keys joined by '+', e.g. Control_R, Super_L+Control_L, F9, or 0xffe4
    #[arg(long, default_value = "Control_R")]
    key: String,
    /// Path to a ggml whisper model
    #[arg(long, default_value_t = default_model())]
    model: String,
    /// Language code (cs, en, ...) or "auto"
    #[arg(long, default_value = "auto")]
    lang: String,
    /// Optional prompt to bias vocabulary/style (names, jargon)
    #[arg(long, default_value = "")]
    prompt: String,
    /// CPU threads for whisper (default: performance cores only, max 4)
    #[arg(long, default_value_t = 4)]
    threads: i32,
    /// Disable start/stop sounds
    #[arg(long)]
    quiet: bool,
    /// Transcribe a 16 kHz mono f32le raw file and exit (for testing)
    #[arg(long)]
    file: Option<String>,
}

fn default_model() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.local/share/wisperflow-linux/models/ggml-large-v3-turbo-q5_0.bin")
}

fn keysym(name: &str) -> Option<u32> {
    if let Some(hex) = name.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    let sym = match name {
        "Control_L" => 0xffe3, "Control_R" => 0xffe4,
        "Shift_L" => 0xffe1, "Shift_R" => 0xffe2,
        "Alt_L" => 0xffe9, "Alt_R" => 0xffea, "ISO_Level3_Shift" => 0xfe03,
        "Super_L" => 0xffeb, "Super_R" => 0xffec, "Menu" => 0xff67,
        "Caps_Lock" => 0xffe5, "Pause" => 0xff13, "Scroll_Lock" => 0xff14,
        f if f.starts_with('F') => return f[1..].parse::<u32>().ok().filter(|n| (1..=24).contains(n)).map(|n| 0xffbd + n),
        _ => return None,
    };
    Some(sym)
}

/// Resolve "A+B" into X keycodes (every keycode that produces each keysym).
fn hotkey_codes(conn: &impl Connection, spec: &str) -> Result<Vec<Vec<u8>>> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    spec.split('+')
        .map(|name| {
            let sym = keysym(name.trim()).with_context(|| format!("unknown key '{name}'"))?;
            let codes: Vec<u8> = map.keysyms.chunks(per).enumerate()
                .filter(|(_, syms)| syms.contains(&sym))
                .map(|(i, _)| min + i as u8)
                .collect();
            if codes.is_empty() { bail!("key '{name}' is not on this keyboard layout") }
            Ok(codes)
        })
        .collect()
}

/// Returns (hotkey held, some other key also held).
fn hotkey_state(conn: &impl Connection, codes: &[Vec<u8>]) -> Result<(bool, bool)> {
    Ok(key_state(&conn.query_keymap()?.reply()?.keys, codes))
}

fn key_state(keys: &[u8; 32], codes: &[Vec<u8>]) -> (bool, bool) {
    let pressed = |c: u8| keys[c as usize / 8] & (1 << (c % 8)) != 0;
    let held = codes.iter().all(|alts| alts.iter().any(|&c| pressed(c)));
    let other = (8..=255u8).any(|c| pressed(c) && !codes.iter().flatten().any(|&h| h == c));
    (held, other)
}

/// Linear-interpolation resample of mono audio to 16 kHz.
// ponytail: linear resampling, fine for speech; use rubato if accuracy on music/odd rates matters.
fn resample(input: &[f32], from: u32) -> Vec<f32> {
    if from == WHISPER_RATE || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / WHISPER_RATE as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let next = *input.get(idx + 1).unwrap_or(&input[idx]);
            input[idx] * (1.0 - frac) + next * frac
        })
        .collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

struct Recording {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

fn start_recording() -> Result<Recording> {
    let device = cpal::default_host().default_input_device().context("no microphone found")?;
    let supported = device.default_input_config()?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let buf = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
    let sink = buf.clone();
    let push = move |data: &[f32]| {
        let mut b = sink.lock().unwrap();
        b.extend(data.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
    };
    let err = |e| eprintln!("audio error: {e}");
    let config = supported.config();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(config, move |d: &[f32], _: &_| push(d), err, None)?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |d: &[i16], _: &_| push(&d.iter().map(|&s| s as f32 / 32768.0).collect::<Vec<_>>()),
            err,
            None,
        )?,
        other => bail!("unsupported mic sample format {other:?}"),
    };
    stream.play()?;
    Ok(Recording { _stream: stream, buf, rate })
}

fn transcribe(ctx: &WhisperContext, args: &Args, audio: &[f32]) -> Result<String> {
    let mut state = ctx.create_state()?;
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    p.set_language(Some(args.lang.as_str()));
    p.set_n_threads(args.threads);
    p.set_no_context(true);
    p.set_suppress_blank(true);
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_special(false);
    p.set_print_timestamps(false);
    if !args.prompt.is_empty() {
        p.set_initial_prompt(&args.prompt);
    }
    // whisper needs >= 1 s of audio; pad short clips with silence
    let mut padded = audio.to_vec();
    padded.resize(padded.len().max(WHISPER_RATE as usize + WHISPER_RATE as usize / 10), 0.0);
    state.full(p, &padded)?;
    let text: String = state.as_iter().map(|s| s.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default()).collect();
    Ok(clean(&text))
}

/// Drop whisper's non-speech markers and known silence hallucinations.
fn clean(text: &str) -> String {
    const HALLUCINATIONS: &[&str] = &["Titulky vytvořil JohnyX", "Děkuji za pozornost.", "Thank you for watching.", "Thanks for watching!"];
    let t = text.trim();
    if t.starts_with('[') && t.ends_with(']') || t.starts_with('(') && t.ends_with(')') || HALLUCINATIONS.contains(&t) {
        return String::new();
    }
    t.to_string()
}

fn type_text(text: &str) -> Result<()> {
    let ok = Command::new("xdotool")
        .args(["type", "--clearmodifiers", "--delay", "4", "--", &format!("{text} ")])
        .status()
        .context("xdotool not installed")?
        .success();
    if !ok { bail!("xdotool type failed") }
    Ok(())
}

fn beep(args: &Args, sound: &str) {
    if !args.quiet {
        let _ = Command::new("paplay").arg(sound).spawn();
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    whisper_rs::install_logging_hooks(); // silence whisper.cpp stdout chatter
    let ctx = WhisperContext::new_with_params(&args.model, WhisperContextParameters::default())
        .with_context(|| format!("cannot load model {} (see README to download)", args.model))?;

    if let Some(path) = &args.file {
        let bytes = std::fs::read(path)?;
        // a trailing partial sample is dropped
        let audio: Vec<f32> = bytes.chunks(4).filter_map(|b| b.try_into().ok().map(f32::from_le_bytes)).collect();
        let t = Instant::now();
        println!("{}", transcribe(&ctx, &args, &audio)?);
        eprintln!("transcribed {:.1}s audio in {:.2}s", audio.len() as f32 / WHISPER_RATE as f32, t.elapsed().as_secs_f32());
        return Ok(());
    }

    transcribe(&ctx, &args, &[])?; // warm-up: GPU shader compile happens here, not on the first dictation
    let (conn, _) = x11rb::connect(None).context("cannot connect to X11 (Wayland is not supported)")?;
    let codes = hotkey_codes(&conn, &args.key)?;
    eprintln!("ready: hold {} to dictate (Ctrl+C to quit)", args.key);

    loop {
        while hotkey_state(&conn, &codes)? != (true, false) {
            std::thread::sleep(Duration::from_millis(15));
        }
        let rec = start_recording()?;
        beep(&args, SOUND_START);
        let started = Instant::now();
        let mut chord = false; // hotkey used as a modifier (e.g. Ctrl+C): cancel dictation
        loop {
            let (held, other) = hotkey_state(&conn, &codes)?;
            chord |= other;
            if !held { break }
            std::thread::sleep(Duration::from_millis(15));
        }
        let rate = rec.rate;
        let raw = std::mem::take(&mut *rec.buf.lock().unwrap());
        drop(rec);
        beep(&args, SOUND_STOP);

        if chord || started.elapsed() < Duration::from_millis(300) || rms(&raw) < 0.002 {
            continue; // accidental tap or silence: whisper would hallucinate
        }
        let t = Instant::now();
        match transcribe(&ctx, &args, &resample(&raw, rate)) {
            Ok(text) if !text.is_empty() => {
                eprintln!("[{:.2}s] {text}", t.elapsed().as_secs_f32());
                if let Err(e) = type_text(&text) { eprintln!("typing failed: {e}") }
            }
            Ok(_) => {}
            Err(e) => eprintln!("transcription failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_length_and_keeps_level() {
        let out = resample(&vec![0.5; 32_000], 32_000);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert_eq!(resample(&[0.1, 0.2], 16_000), vec![0.1, 0.2]);
    }

    #[test]
    fn keysym_parsing() {
        assert_eq!(keysym("Control_R"), Some(0xffe4));
        assert_eq!(keysym("F9"), Some(0xffc6));
        assert_eq!(keysym("0xffeb"), Some(0xffeb));
        assert_eq!(keysym("F99"), None);
        assert_eq!(keysym("Nope"), None);
    }

    #[test]
    fn chord_detection() {
        let mut keys = [0u8; 32];
        let codes = vec![vec![105u8]];
        keys[105 / 8] |= 1 << (105 % 8);
        assert_eq!(key_state(&keys, &codes), (true, false));
        keys[54 / 8] |= 1 << (54 % 8); // 'c'
        assert_eq!(key_state(&keys, &codes), (true, true));
    }

    #[test]
    fn clean_drops_noise() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean("Titulky vytvořil JohnyX"), "");
        assert_eq!(clean(" Ahoj, jak se máš? "), "Ahoj, jak se máš?");
    }
}
