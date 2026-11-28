//! Listening: the agent records what the machine plays while the test plays a faint tone, and says what it heard.
//! The tone is 880 Hz, 0.3 s, at -40 dBFS: quiet, in case the speakers are on. On the Mac the tone plays only with
//! MC_MAC_AUDIO_TESTS=1 (Mac tests must not really act; see CLAUDE.md).

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use common::Test;
use serde_json::json;

const HZ: f64 = 880.0;
const AMPLITUDE: f32 = 0.01;

fn listening() -> Test {
    // The agent records with a helper: its own program, which inside a test is elsewhere.
    std::env::set_var("MC_AGENT_PROGRAM", env!("CARGO_BIN_EXE_mc-agent"));
    Test::new()
}

fn tone_file(folder: &Path) -> std::path::PathBuf {
    let rate = 48_000u32;
    let silence = vec![0.0f32; rate as usize / 20];
    let tone: Vec<f32> = (0..(rate as f64 * 0.3) as usize).map(|i| AMPLITUDE * (2.0 * std::f64::consts::PI * HZ * i as f64 / f64::from(rate)).sin() as f32).collect();
    let samples = [silence.clone(), tone, silence].concat();
    let path = folder.join("tone.wav");
    std::fs::write(&path, mc_core::audio::wav(&samples, rate, 1)).unwrap();
    path
}

/// Plays a sound file the system's own way, to its default output, and waits until it has played.
fn play(path: &Path) {
    let status = if cfg!(target_os = "linux") {
        Command::new("pw-play").arg(path).status()
    } else if cfg!(windows) {
        Command::new("powershell").args(["-NoProfile", "-Command", &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", path.display())]).status()
    } else {
        Command::new("afplay").arg(path).status()
    };
    assert!(status.is_ok_and(|status| status.success()), "the tone didn't play");
}

/// Whether this machine can play and record here: PipeWire's tools on Linux; on the Mac, only when asked.
fn can_play() -> bool {
    if cfg!(target_os = "linux") {
        Command::new("pw-record").arg("--version").output().is_ok_and(|output| output.status.success())
    } else if cfg!(target_os = "macos") {
        std::env::var_os("MC_MAC_AUDIO_TESTS").is_some()
    } else {
        cfg!(windows)
    }
}

#[test]
fn a_tone_played_is_heard_with_its_time_level_and_pitch() {
    if !can_play() {
        eprintln!("skipped: nothing to play and record with here (on the Mac, set MC_MAC_AUDIO_TESTS=1)");
        return;
    }
    let _turn = common::desktop_turn();
    let test = listening();
    let tone = tone_file(&test.home);
    let started = test.tool("listen_start", json!({ "limit_s": 30 }));
    let id = started["listen"].as_u64().expect("a listening number");
    assert!(started["rate"].as_u64().unwrap() > 0, "{started}");
    std::thread::sleep(Duration::from_millis(500));
    play(&tone);
    let heard = test.tool("listen_stop", json!({ "listen": id, "wait_for_sound_s": 10, "threshold_dbfs": -60 }));
    assert_eq!(heard["heard"], true, "{heard}");
    assert_eq!(heard["timed_out"], false, "{heard}");
    let sounds = heard["sounds"].as_array().unwrap();
    assert_eq!(sounds.len(), 1, "{heard}");
    let sound = &sounds[0];
    let pitch = sound["pitch_hz"].as_f64().expect("a tone has a pitch");
    assert!((pitch - HZ).abs() < 20.0, "{heard}");
    let length = sound["end_s"].as_f64().unwrap() - sound["start_s"].as_f64().unwrap();
    assert!((0.2..0.5).contains(&length), "{heard}");
    assert!(sound["start_s"].as_f64().unwrap() >= 0.3, "{heard}");
    // Played at -40 dBFS; the output's own volume doesn't change what is recorded.
    let peak = sound["peak_dbfs"].as_f64().unwrap();
    assert!((-50.0..=-30.0).contains(&peak), "{heard}");
    let wav = std::fs::read(heard["wav"].as_str().unwrap()).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(wav.len() as u64, heard["wav_bytes"].as_u64().unwrap());
    // Asked again, the same answer.
    assert_eq!(test.tool("listen_stop", json!({ "listen": id })), heard);
}

#[test]
fn silence_is_heard_as_nothing_and_waiting_for_a_sound_times_out() {
    if !can_play() {
        eprintln!("skipped: nothing to record with here (on the Mac, set MC_MAC_AUDIO_TESTS=1)");
        return;
    }
    let _turn = common::desktop_turn();
    let test = listening();
    let id = test.tool("listen_start", json!({}))["listen"].as_u64().unwrap();
    // A threshold above anything a quiet machine plays.
    let heard = test.tool("listen_stop", json!({ "listen": id, "wait_for_sound_s": 1, "threshold_dbfs": -1 }));
    assert_eq!(heard["heard"], false, "{heard}");
    assert_eq!(heard["timed_out"], true, "{heard}");
    assert!(heard["duration_s"].as_f64().unwrap() >= 0.8, "{heard}");
}

#[test]
fn what_is_refused() {
    let test = listening();
    assert_eq!(test.refused("listen_stop", json!({ "listen": 999 })), (404, "not_found".to_string()));
    assert_eq!(test.refused("listen_stop", json!({})).1, "bad_input");
    assert_eq!(test.refused("listen_start", json!({ "limit_s": 0 })).1, "bad_input");
    assert_eq!(test.refused("listen_start", json!({ "limit_s": 601 })).1, "bad_input");
}
