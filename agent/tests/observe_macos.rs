//! Observation on macOS with whatever this machine gives: the system's own events when this user may become root
//! unasked, the sandbox's reports otherwise. Sessions follow a small target program which does exactly what the test
//! tells it to, so every expected event is known.
#![cfg(target_os = "macos")]

mod common;

const WITHOUT_ROOT: bool = false;

include!("common/macos_observation.rs");
