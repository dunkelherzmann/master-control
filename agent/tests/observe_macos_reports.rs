//! Observation on macOS from the sandbox's reports alone, as on a machine with no admin rights: the same tests,
//! with the system's own events left aside.
#![cfg(target_os = "macos")]

mod common;

const WITHOUT_ROOT: bool = true;

include!("common/macos_observation.rs");
