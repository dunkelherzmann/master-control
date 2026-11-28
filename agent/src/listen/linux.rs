//! Linux: what the default output plays, from PipeWire's monitor of it (`pw-record` with `stream.capture.sink`).
//! The monitor carries the sound before the output's volume and mute, so a muted machine is heard too, and PipeWire
//! keeps a stand-in output when there is no sound card.

use std::io::{self, Read};
use std::process::{Command, Stdio};

use super::Sink;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

pub(super) fn capture(sink: &mut Sink) -> io::Result<()> {
    let mut recorder = Command::new("pw-record")
        .args(["--raw", "-P", "stream.capture.sink=true", "--rate", &RATE.to_string(), "--channels", &CHANNELS.to_string(), "--format", "f32", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => io::Error::other("pw-record isn't installed: listening on Linux needs PipeWire and its tools (Ubuntu: pipewire-bin)"),
            _ => io::Error::other(format!("pw-record couldn't be started: {error}")),
        })?;
    let mut output = recorder.stdout.take().expect("piped");
    let mut buffer = vec![0u8; 16 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut said_started = false;
    let outcome = loop {
        if sink.should_stop() {
            break Ok(());
        }
        let read = match output.read(&mut buffer) {
            Ok(0) => {
                let mut why = String::new();
                if let Some(mut errors) = recorder.stderr.take() {
                    let _ = errors.read_to_string(&mut why);
                }
                break Err(io::Error::other(format!("pw-record stopped: {}", why.trim())));
            }
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => break Err(error),
        };
        if !said_started {
            sink.started(RATE, CHANNELS)?;
            said_started = true;
        }
        carry.extend_from_slice(&buffer[..read]);
        let whole = carry.len() - carry.len() % (4 * usize::from(CHANNELS));
        let samples = mc_core::audio::from_raw(&carry[..whole]);
        carry.drain(..whole);
        sink.write(&samples)?;
    };
    let _ = recorder.kill();
    let _ = recorder.wait();
    outcome
}
