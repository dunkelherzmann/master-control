//! Shared by the desktop tests: an agent on its own port and home folder, and a way to call its tools.
#![allow(dead_code)]

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use mc_agent::{Config, Running};
use serde_json::Value;

static NEXT: AtomicU32 = AtomicU32::new(0);
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// There is one pointer, one keyboard and one focus: tests that use them take turns.
pub fn desktop_turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct Test {
    running: Option<Running>,
    pub home: PathBuf,
    pub port: u16,
    pub token: String,
}

impl Test {
    pub fn new() -> Test {
        let home = std::env::temp_dir().join(format!("mc-agent-desktop-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&home);
        let running = mc_agent::start(Config { home: home.clone(), port: 0, worker: None }).expect("the agent starts");
        Test { port: running.port, token: running.token().to_string(), running: Some(running), home }
    }

    pub fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let request = ureq::post(&format!("http://127.0.0.1:{}{path}", self.port))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_string(&body.to_string());
        let response = match request {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("the request didn't reach the agent: {error}"),
        };
        let status = response.status();
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Calls a tool that is expected to work.
    pub fn tool(&self, name: &str, args: Value) -> Value {
        let (status, answer) = self.post(&format!("/v1/{name}"), &args);
        assert_eq!(status, 200, "{name} failed: {answer}");
        answer
    }

    /// Calls a tool that is expected to be refused; returns the status and the error code.
    pub fn refused(&self, name: &str, args: Value) -> (u16, String) {
        let (status, answer) = self.post(&format!("/v1/{name}"), &args);
        (status, answer["error"]["code"].as_str().unwrap_or_default().to_string())
    }
}

impl Drop for Test {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The colour of one pixel of a PNG, and the picture's size.
pub fn png_pixel(png: &[u8], x: u32, y: u32) -> ([u8; 3], (u32, u32)) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let at = ((y * info.width + x) * 3) as usize;
    ([pixels[at], pixels[at + 1], pixels[at + 2]], (info.width, info.height))
}

pub fn decode_base64(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text).unwrap()
}
