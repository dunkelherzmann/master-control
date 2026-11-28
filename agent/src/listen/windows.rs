//! Windows: what the default output plays, from WASAPI's loopback of it, which needs no driver. Loopback hands sound
//! over only while something plays, stamped with the performance counter, so the gaps are filled with silence here to
//! keep the recording's times true.

use std::io;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::{Placer, Sink};

/// Silence is added only up to this long ago, so sound that is still on its way lands in its place.
const LATE_S: f64 = 0.2;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

fn failed(what: &str, error: windows::core::Error) -> io::Error {
    io::Error::other(format!("{what}: {error}"))
}

/// The performance counter in 100-nanosecond steps, the unit loopback stamps its sound with.
fn now_100ns() -> u64 {
    let (mut count, mut frequency) = (0i64, 0i64);
    // SAFETY: both write one integer each.
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (count as u128 * 10_000_000 / frequency.max(1) as u128) as u64
}

pub(super) fn capture(sink: &mut Sink) -> io::Result<()> {
    // SAFETY: COM calls with the arguments their documentation gives; the mix format is freed once copied.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|error| failed("the sound devices can't be listed", error))?;
        let output = devices
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|error| failed("there is no sound output to listen to (listening on Windows needs one, even a virtual one)", error))?;
        let client: IAudioClient = output.Activate(CLSCTX_ALL, None).map_err(|error| failed("the sound output can't be opened", error))?;
        let format_pointer = client.GetMixFormat().map_err(|error| failed("the sound output's format can't be read", error))?;
        let format: WAVEFORMATEX = *format_pointer;
        let (tag, bits, rate, channels) = (format.wFormatTag, format.wBitsPerSample, format.nSamplesPerSec, format.nChannels);
        // A shared-mode output mixes in 32-bit float; anything else isn't read here.
        if !matches!(tag, WAVE_FORMAT_IEEE_FLOAT | WAVE_FORMAT_EXTENSIBLE) || bits != 32 || channels == 0 {
            CoTaskMemFree(Some(format_pointer as *const _));
            return Err(io::Error::other(format!("the sound output mixes in a format not read here (tag {tag:#x}, {bits} bits)")));
        }
        let initialised = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 10_000_000, 0, format_pointer, None);
        CoTaskMemFree(Some(format_pointer as *const _));
        initialised.map_err(|error| failed("listening to the sound output couldn't be set up", error))?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|error| failed("the sound output's recorder couldn't be had", error))?;
        let origin = now_100ns();
        client.Start().map_err(|error| failed("listening to the sound output didn't start", error))?;
        sink.started(rate, channels)?;

        let mut placer = Placer::new(channels);
        let frame_at = |stamp: u64| (stamp as f64 - origin as f64) / 10_000_000.0 * f64::from(rate);
        let outcome = loop {
            std::thread::sleep(Duration::from_millis(10));
            let mut problem = None;
            loop {
                match capture.GetNextPacketSize() {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) => {
                        problem = Some(failed("the sound couldn't be read", error));
                        break;
                    }
                }
                let (mut data, mut frames, mut flags, mut stamp) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                if let Err(error) = capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut stamp)) {
                    problem = Some(failed("the sound couldn't be read", error));
                    break;
                }
                let count = frames as usize * usize::from(channels);
                let samples: Vec<f32> = if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    vec![0.0; count]
                } else {
                    std::slice::from_raw_parts(data as *const f32, count).to_vec()
                };
                let _ = capture.ReleaseBuffer(frames);
                placer.place(frame_at(stamp).round() as i64, &samples);
            }
            if let Some(problem) = problem {
                break Err(problem);
            }
            placer.pad_to(frame_at(now_100ns().saturating_sub((LATE_S * 10_000_000.0) as u64)).max(0.0) as u64);
            if let Err(error) = sink.write(&placer.take()) {
                break Err(error);
            }
            if sink.should_stop() {
                break Ok(());
            }
        };
        let _ = client.Stop();
        outcome
    }
}
