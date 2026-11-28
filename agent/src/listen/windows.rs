//! Windows: placeholder until WASAPI loopback is written.
use std::io;

use super::Sink;

pub(super) fn capture(_sink: &mut Sink) -> io::Result<()> {
    Err(io::Error::other("listening on Windows isn't built yet"))
}
