//! Listening: a recording of what the machine plays, and what was heard in it (specs.md §3.5). The recording is
//! made by a helper process (`mc-agent listen-run DIR`, the same program started again) so it outlives the call
//! that started it, also when the master runs one process per call. The helper and the tools share only the
//! listening's folder:
//!
//! - `request.json` the tool writes: the longest it may record;
//! - `format.json` the helper writes once sound arrives: rate and channels;
//! - `audio.f32` the helper appends to: interleaved little-endian 32-bit float samples, gaps filled with silence;
//! - `error.txt` the helper writes when it can't record; `ended` when it has stopped;
//! - `stop` the tool makes to stop it; `pid` the helper's process id; `result.json` the answer, once stopped.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::cell::Cell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mc_core::audio;
use serde_json::{json, Value};

use crate::tools::{number, text, ToolError};
use crate::Agent;

const DEFAULT_LIMIT_S: u64 = 60;
const LONGEST_LIMIT_S: u64 = 600;
const LONGEST_WAIT_S: u64 = 60;
/// A sound counts as over once this much quiet follows it (longer than the gap that joins two parts of a sound).
const OVER_AFTER_S: f64 = 0.15;

/// Whether this system can listen.
pub const BUILT_IN: bool = cfg!(any(target_os = "linux", target_os = "macos", windows));

fn folder(agent: &Agent) -> PathBuf {
    agent.home.join("listen")
}

pub fn listen_start(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    if !BUILT_IN {
        return Err(ToolError { code: "not_supported", message: "listening isn't built for this system".into() });
    }
    let limit_s = number(args, "limit_s")?.unwrap_or(DEFAULT_LIMIT_S);
    if limit_s == 0 || limit_s > LONGEST_LIMIT_S {
        return Err(ToolError::bad(format!("limit_s is 1 to {LONGEST_LIMIT_S} seconds")));
    }
    let failed = |what: &str, error: io::Error| ToolError::failed(format!("{what}: {error}"));
    let root = folder(agent);
    fs::create_dir_all(&root).map_err(|error| failed("the listening folder couldn't be made", error))?;
    // Numbers come from the folders already there, so a master running one process per call counts on.
    let (id, directory) = loop {
        let next = fs::read_dir(&root)
            .map_err(|error| failed("the listening folder couldn't be read", error))?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let directory = root.join(next.to_string());
        match fs::create_dir(&directory) {
            Ok(()) => break (next, directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(failed("the listening's folder couldn't be made", error)),
        }
    };
    fs::write(directory.join("request.json"), json!({ "limit_s": limit_s }).to_string()).map_err(|error| failed("the request couldn't be written", error))?;
    let program = std::env::var_os("MC_AGENT_PROGRAM").map(PathBuf::from).map_or_else(std::env::current_exe, Ok).map_err(|error| failed("the agent's own program wasn't found", error))?;
    let log = fs::File::create(directory.join("helper.log")).map_err(|error| failed("the helper's log couldn't be made", error))?;
    let mut helper = Command::new(program);
    // In a group of its own, so stopping it also stops what it started (pw-record on Linux).
    crate::platform::own_group(&mut helper);
    let child = helper
        .arg("listen-run")
        .arg(&directory)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| failed("the helper's log couldn't be shared", error))?)
        .stderr(log)
        .spawn()
        .map_err(|error| failed("the listening helper couldn't be started", error))?;
    let _ = fs::write(directory.join("pid"), child.id().to_string());
    // The helper is left to run on its own; listen_stop ends it.
    drop(child);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(error) = fs::read_to_string(directory.join("error.txt")) {
            return Err(ToolError { code: "failed", message: error.trim().to_string() });
        }
        if let Some((rate, channels)) = read_format(&directory) {
            return Ok(json!({ "listen": id, "rate": rate, "channels": channels, "limit_s": limit_s }));
        }
        if directory.join("ended").exists() || Instant::now() > deadline {
            return Err(ToolError::failed("the listening helper didn't start recording in time; see helper.log in its folder"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn listen_stop(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let id = number(args, "listen")?.ok_or_else(|| ToolError::bad("listen is required: the number listen_start gave"))?;
    let directory = folder(agent).join(id.to_string());
    if !directory.is_dir() {
        return Err(ToolError::not_found(format!("there is no listening {id}")));
    }
    if let Ok(answer) = fs::read(directory.join("result.json")) {
        return serde_json::from_slice(&answer).map_err(|error| ToolError::failed(format!("the earlier answer couldn't be read: {error}")));
    }
    let threshold = match &args["threshold_dbfs"] {
        Value::Null => audio::DEFAULT_THRESHOLD_DBFS,
        value => value.as_f64().filter(|level| (-120.0..=0.0).contains(level)).ok_or_else(|| ToolError::bad("threshold_dbfs is a level from -120 to 0"))?,
    };
    let wait_s = number(args, "wait_for_sound_s")?.unwrap_or(0);
    if wait_s > LONGEST_WAIT_S {
        return Err(ToolError::bad(format!("wait_for_sound_s is at most {LONGEST_WAIT_S}")));
    }
    let save_to = text(args, "save_to")?.map(crate::state::resolve);

    // Waiting: until a sound has come and gone, the time is up or the helper stopped by itself.
    let mut waited_out = false;
    if wait_s > 0 {
        let deadline = Instant::now() + Duration::from_secs(wait_s);
        loop {
            if let Some(heard) = hear(&directory, threshold) {
                if heard.sounds.last().is_some_and(|sound| sound.end_s + OVER_AFTER_S <= heard.duration_s) {
                    break;
                }
            }
            if directory.join("ended").exists() {
                break;
            }
            if Instant::now() > deadline {
                waited_out = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let _ = fs::write(directory.join("stop"), b"");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !directory.join("ended").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !directory.join("ended").exists() {
        if let Some(pid) = fs::read_to_string(directory.join("pid")).ok().and_then(|text| text.trim().parse::<u32>().ok()) {
            crate::platform::kill_tree(pid);
        }
    }
    let error = fs::read_to_string(directory.join("error.txt")).ok().map(|text| text.trim().to_string());
    let (rate, channels) = read_format(&directory).ok_or_else(|| ToolError::failed(error.clone().unwrap_or_else(|| "nothing was recorded".into())))?;
    let samples = audio::from_raw(&fs::read(directory.join("audio.f32")).unwrap_or_default());
    let samples = &samples[..samples.len() - samples.len() % usize::from(channels.max(1))];
    let heard = audio::analyse(&audio::mono(samples, usize::from(channels)), rate, threshold);
    let wav_path = save_to.unwrap_or_else(|| directory.join("listen.wav"));
    let wav = audio::wav(samples, rate, channels);
    fs::write(&wav_path, &wav).map_err(|error| ToolError::failed(format!("the recording couldn't be saved to {}: {error}", wav_path.display())))?;
    let _ = fs::remove_file(directory.join("audio.f32"));

    let round = |value: f64, places: i32| (value * 10f64.powi(places)).round() / 10f64.powi(places);
    let sounds: Vec<Value> = heard
        .sounds
        .iter()
        .map(|sound| {
            json!({
                "start_s": round(sound.start_s, 3), "end_s": round(sound.end_s, 3),
                "peak_dbfs": round(sound.peak_dbfs, 1), "rms_dbfs": round(sound.rms_dbfs, 1),
                "pitch_hz": sound.pitch_hz.map(|hz| round(hz, 0)),
            })
        })
        .collect();
    let mut answer = json!({
        "listen": id,
        "heard": !sounds.is_empty(),
        "sounds": sounds,
        "duration_s": round(heard.duration_s, 3),
        "peak_dbfs": round(heard.peak_dbfs, 1),
        "threshold_dbfs": threshold,
        "rate": rate,
        "channels": channels,
        "wav": wav_path.display().to_string(),
        "wav_bytes": wav.len(),
    });
    if wait_s > 0 {
        answer["timed_out"] = json!(waited_out);
    }
    if let Some(error) = error {
        answer["error"] = json!(error);
    }
    let _ = fs::write(directory.join("result.json"), answer.to_string());
    Ok(answer)
}

/// What the recording so far holds, while the helper still records.
fn hear(directory: &Path, threshold: f64) -> Option<audio::Heard> {
    let (rate, channels) = read_format(directory)?;
    let samples = audio::from_raw(&fs::read(directory.join("audio.f32")).ok()?);
    let samples = &samples[..samples.len() - samples.len() % usize::from(channels.max(1))];
    Some(audio::analyse(&audio::mono(samples, usize::from(channels)), rate, threshold))
}

fn read_format(directory: &Path) -> Option<(u32, u16)> {
    let format: Value = serde_json::from_slice(&fs::read(directory.join("format.json")).ok()?).ok()?;
    Some((u32::try_from(format["rate"].as_u64()?).ok()?, u16::try_from(format["channels"].as_u64()?).ok()?))
}

/// What a system's capture is handed: where to put the sound, and when to stop.
pub(crate) struct Sink<'a> {
    started: &'a mut dyn FnMut(u32, u16) -> io::Result<()>,
    write: &'a mut dyn FnMut(&[f32]) -> io::Result<()>,
    stop: &'a dyn Fn() -> bool,
}

impl Sink<'_> {
    /// Sound will come at this rate and number of channels (said once, before the first samples).
    pub(crate) fn started(&mut self, rate: u32, channels: u16) -> io::Result<()> {
        (self.started)(rate, channels)
    }

    /// Interleaved samples, in order, with any gap already filled with silence.
    pub(crate) fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        (self.write)(samples)
    }

    pub(crate) fn should_stop(&self) -> bool {
        (self.stop)()
    }
}

/// The helper process: records into `directory` until told to stop or the limit is reached.
pub fn run(directory: &Path) -> io::Result<()> {
    let request: Value = serde_json::from_slice(&fs::read(directory.join("request.json"))?).map_err(io::Error::other)?;
    let limit_s = request["limit_s"].as_u64().unwrap_or(DEFAULT_LIMIT_S);
    let outcome = record_into(directory, limit_s);
    if let Err(error) = &outcome {
        let _ = fs::write(directory.join("error.txt"), error.to_string());
    }
    let _ = fs::write(directory.join("ended"), b"");
    outcome
}

fn record_into(directory: &Path, limit_s: u64) -> io::Result<()> {
    let mut file = io::BufWriter::new(fs::File::create(directory.join("audio.f32"))?);
    let frames = Cell::new(0u64);
    let format = Cell::new((0u32, 0u16));
    let stop_file = directory.join("stop");
    let format_file = directory.join("format.json");
    let mut started = |rate: u32, channels: u16| {
        format.set((rate, channels));
        fs::write(&format_file, json!({ "rate": rate, "channels": channels }).to_string())
    };
    let mut write = |samples: &[f32]| {
        let channels = u64::from(format.get().1.max(1));
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        file.write_all(&bytes)?;
        // Readers see the sound as it comes.
        file.flush()?;
        frames.set(frames.get() + samples.len() as u64 / channels);
        Ok(())
    };
    let stop = || {
        let (rate, _) = format.get();
        stop_file.exists() || (rate > 0 && frames.get() >= limit_s * u64::from(rate))
    };
    let mut sink = Sink { started: &mut started, write: &mut write, stop: &stop };
    capture(&mut sink)
}

#[cfg(target_os = "linux")]
use linux::capture;
#[cfg(target_os = "macos")]
use macos::capture;
#[cfg(windows)]
use windows::capture;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn capture(_sink: &mut Sink) -> io::Result<()> {
    Err(io::Error::other("listening isn't built for this system"))
}
