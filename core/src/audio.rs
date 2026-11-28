//! What a recording of a machine's sound holds: the sounds in it (when each starts and ends, how loud it is, its
//! pitch) and the recording as a WAV file (SPECIFICATION.md §3.5). Shared by every system's way of capturing sound.

/// Samples are cut into steps this long to find where sounds start and end.
const STEP_S: f64 = 0.010;
/// Two sounds closer than this are one sound (a bell's ring has short dips).
const JOIN_S: f64 = 0.100;
/// Anything shorter than this is a click, not a sound.
const SHORTEST_S: f64 = 0.020;
/// The quietest level that counts, in dB below full scale.
pub const DEFAULT_THRESHOLD_DBFS: f64 = -50.0;
/// Silence is reported as this level rather than minus infinity.
const FLOOR_DBFS: f64 = -120.0;

/// One sound found in a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub start_s: f64,
    pub end_s: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    /// Its main pitch, when it has one (a tone, a bell); `None` for noise.
    pub pitch_hz: Option<f64>,
}

/// What a whole recording holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub duration_s: f64,
    pub peak_dbfs: f64,
    pub sounds: Vec<Sound>,
}

/// A level as dB below full scale.
pub fn dbfs(level: f64) -> f64 {
    if level <= 0.0 {
        FLOOR_DBFS
    } else {
        (20.0 * level.log10()).max(FLOOR_DBFS)
    }
}

/// Interleaved samples to one channel: the channels' mean, frame by frame.
pub fn mono(samples: &[f32], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    samples.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect()
}

/// The sounds in one channel of samples, louder than `threshold_dbfs`.
pub fn analyse(samples: &[f32], rate: u32, threshold_dbfs: f64) -> Heard {
    let rate_f = f64::from(rate.max(1));
    let step = ((STEP_S * rate_f).round() as usize).max(1);
    let loud: Vec<bool> = samples.chunks(step).map(|chunk| dbfs(rms(chunk)) >= threshold_dbfs).collect();

    // Runs of loud steps, joined across short quiet gaps, dropped when too short.
    let join = (JOIN_S / STEP_S).round() as usize;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, &is_loud) in loud.iter().enumerate() {
        if !is_loud {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if index - *end <= join => *end = index + 1,
            _ => runs.push((index, index + 1)),
        }
    }
    let shortest = (SHORTEST_S / STEP_S).round() as usize;
    let sounds = runs
        .into_iter()
        .filter(|(start, end)| end - start >= shortest)
        .map(|(start, end)| {
            let part = &samples[start * step..(end * step).min(samples.len())];
            Sound {
                start_s: (start * step) as f64 / rate_f,
                end_s: ((end * step).min(samples.len())) as f64 / rate_f,
                peak_dbfs: dbfs(peak(part)),
                rms_dbfs: dbfs(rms(part)),
                pitch_hz: pitch(part, rate),
            }
        })
        .collect();
    Heard { duration_s: samples.len() as f64 / rate_f, peak_dbfs: dbfs(peak(samples)), sounds }
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / samples.len() as f64).sqrt()
}

fn peak(samples: &[f32]) -> f64 {
    samples.iter().fold(0.0_f64, |most, &s| most.max(f64::from(s).abs()))
}

/// The main pitch of a sound from 50 Hz to 4 kHz, by autocorrelation over its loudest 50 ms; `None` when no period
/// stands out (noise) or the sound is too short to have one.
pub fn pitch(samples: &[f32], rate: u32) -> Option<f64> {
    let rate_f = f64::from(rate);
    let window = ((0.050 * rate_f) as usize).min(samples.len());
    let shortest_lag = ((rate_f / 4000.0) as usize).max(1);
    // A period must fit twice into the stretch looked at.
    let longest_lag = ((rate_f / 50.0) as usize).min(window / 2);
    if longest_lag <= shortest_lag + 1 {
        return None;
    }
    // The loudest stretch, looked for in quarter-window steps: a sound's start may be a click or its end a fade.
    let start = (0..=samples.len() - window).step_by((window / 4).max(1)).max_by(|&a, &b| rms(&samples[a..a + window]).total_cmp(&rms(&samples[b..b + window])))?;
    let part = &samples[start..start + window];
    // values[lag] is how alike the stretch is to itself moved by `lag` samples, from -1 to 1: the products over
    // where the two overlap, divided by both overlapping parts' energy, so a period peaks at 1 however long it is.
    let mut squares = Vec::with_capacity(window + 1);
    squares.push(0.0f64);
    for &sample in part {
        squares.push(squares.last().copied().unwrap_or(0.0) + f64::from(sample) * f64::from(sample));
    }
    let values: Vec<f64> = (0..=longest_lag)
        .map(|lag| {
            let product: f64 = part[..window - lag].iter().zip(&part[lag..]).map(|(&a, &b)| f64::from(a) * f64::from(b)).sum();
            let both = (squares[window - lag] * (squares[window] - squares[lag])).sqrt();
            if both > 0.0 { product / both } else { 0.0 }
        })
        .collect();
    // Past the first dip below zero (half a period on), the next peak is a period.
    let dip = values.iter().position(|&value| value < 0.0)?;
    let first = dip.max(shortest_lag);
    let best = values[first..].iter().copied().fold(f64::MIN, f64::max);
    if best < 0.5 {
        return None;
    }
    // The first peak that comes close to the best: the period itself, not a multiple of it.
    let at = (first.max(1)..longest_lag).find(|&i| values[i] >= 0.9 * best && values[i] >= values[i - 1] && values[i] >= values[i + 1])?;
    // A parabola through the peak and its neighbours places it between whole samples.
    let (before, here, after) = (values[at - 1], values[at], values[at + 1]);
    let bend = before - 2.0 * here + after;
    let shift = if bend == 0.0 { 0.0 } else { 0.5 * (before - after) / bend };
    Some(rate_f / (at as f64 + shift))
}

/// Interleaved samples as a 16-bit PCM WAV file.
pub fn wav(samples: &[f32], rate: u32, channels: u16) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    out
}

/// Raw little-endian 32-bit float samples, as the capture helpers write them, back to samples. A partly written last
/// sample is left out.
pub fn from_raw(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(hz: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
        (0..(seconds * f64::from(RATE)) as usize).map(|i| amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(RATE)).sin() as f32).collect()
    }

    fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (seconds * f64::from(RATE)) as usize]
    }

    #[test]
    fn silence_holds_no_sound() {
        let heard = analyse(&silence(1.0), RATE, DEFAULT_THRESHOLD_DBFS);
        assert!(heard.sounds.is_empty());
        assert_eq!(heard.peak_dbfs, -120.0);
        assert!((heard.duration_s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_tone_is_found_with_its_times_level_and_pitch() {
        let samples = [silence(0.5), tone(880.0, 0.3, 0.1), silence(0.5)].concat();
        let heard = analyse(&samples, RATE, DEFAULT_THRESHOLD_DBFS);
        assert_eq!(heard.sounds.len(), 1, "{heard:?}");
        let sound = &heard.sounds[0];
        // The tone starts and ends on a 10 ms step, so its times are exact.
        assert!((sound.start_s - 0.5).abs() < 1e-9, "{sound:?}");
        assert!((sound.end_s - 0.8).abs() < 1e-9, "{sound:?}");
        assert!((sound.peak_dbfs - -20.0).abs() < 0.1, "{sound:?}");
        // A sine's RMS is 3 dB under its peak.
        assert!((sound.rms_dbfs - -23.0).abs() < 0.2, "{sound:?}");
        let hz = sound.pitch_hz.expect("a tone has a pitch");
        // Within a hertz: the period falls between whole samples (54.5 at 48 kHz).
        assert!((hz - 880.0).abs() < 1.0, "{hz}");
        assert!((heard.peak_dbfs - -20.0).abs() < 0.1);
    }

    #[test]
    fn two_tones_apart_are_two_sounds_and_a_short_dip_is_not_a_gap() {
        let apart = [tone(440.0, 0.2, 0.1), silence(0.3), tone(660.0, 0.2, 0.1)].concat();
        let heard = analyse(&apart, RATE, DEFAULT_THRESHOLD_DBFS);
        assert_eq!(heard.sounds.len(), 2, "{heard:?}");
        assert!((heard.sounds[0].pitch_hz.unwrap() - 440.0).abs() < 3.0);
        assert!((heard.sounds[1].pitch_hz.unwrap() - 660.0).abs() < 4.0);
        let dipped = [tone(440.0, 0.2, 0.1), silence(0.05), tone(440.0, 0.2, 0.1)].concat();
        assert_eq!(analyse(&dipped, RATE, DEFAULT_THRESHOLD_DBFS).sounds.len(), 1);
    }

    #[test]
    fn the_threshold_decides_what_counts_and_a_click_does_not() {
        let faint = [silence(0.2), tone(880.0, 0.3, 0.001), silence(0.2)].concat(); // -60 dBFS
        assert!(analyse(&faint, RATE, DEFAULT_THRESHOLD_DBFS).sounds.is_empty());
        assert_eq!(analyse(&faint, RATE, -70.0).sounds.len(), 1);
        let click = [silence(0.2), tone(880.0, 0.01, 0.5), silence(0.2)].concat();
        assert!(analyse(&click, RATE, DEFAULT_THRESHOLD_DBFS).sounds.is_empty());
    }

    #[test]
    fn pitch_across_the_range_and_from_the_loudest_stretch() {
        for hz in [60.0, 100.0, 440.0, 1234.0, 3000.0] {
            let found = pitch(&tone(hz, 0.2, 0.2), RATE).unwrap_or_else(|| panic!("no pitch for {hz}"));
            assert!((found - hz).abs() < hz * 0.005, "{hz}: {found}");
        }
        // Too low or too high for the range.
        assert_eq!(pitch(&tone(30.0, 0.2, 0.2), RATE), None);
        assert!(pitch(&tone(6000.0, 0.2, 0.2), RATE).is_none_or(|found| (found - 6000.0).abs() > 100.0));
        // A loud tone after a quieter one of another pitch: the loud one is the sound's pitch.
        let both = [tone(300.0, 0.1, 0.02), tone(700.0, 0.1, 0.3)].concat();
        assert!((pitch(&both, RATE).unwrap() - 700.0).abs() < 3.5);
        let both = [tone(700.0, 0.1, 0.3), tone(300.0, 0.1, 0.02)].concat();
        assert!((pitch(&both, RATE).unwrap() - 700.0).abs() < 3.5);
        // A period must fit twice: 5 ms of 100 Hz is too short to tell.
        assert_eq!(pitch(&tone(100.0, 0.005, 0.2), RATE), None);
        assert_eq!(pitch(&[], RATE), None);
    }

    #[test]
    fn noise_has_no_pitch() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..RATE / 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) as f32 * 0.2
            })
            .collect();
        assert_eq!(pitch(&noise, RATE), None);
        assert_eq!(pitch(&silence(0.1), RATE), None);
        assert_eq!(pitch(&[0.1; 10], RATE), None);
    }

    #[test]
    fn levels_and_channels() {
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(0.1) - -20.0).abs() < 1e-9);
        assert_eq!(dbfs(0.0), -120.0);
        assert_eq!(dbfs(1e-9), -120.0);
        assert_eq!(mono(&[0.2, 0.4, -0.2, 0.0], 2), vec![0.3, -0.1]);
        assert_eq!(mono(&[0.5, 0.25], 1), vec![0.5, 0.25]);
        assert_eq!(mono(&[0.5, 0.25], 0), vec![0.5, 0.25]);
    }

    #[test]
    fn a_wav_file_has_the_header_and_samples() {
        let file = wav(&[0.0, 1.0, -1.0, 2.0], 48_000, 2);
        assert_eq!(&file[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&file[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(file[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(file[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(file[28..32].try_into().unwrap()), 48_000 * 4);
        assert_eq!(u16::from_le_bytes(file[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(file[34..36].try_into().unwrap()), 16);
        assert_eq!(u32::from_le_bytes(file[40..44].try_into().unwrap()), 8);
        let mono = wav(&[0.5], 8_000, 1);
        assert_eq!(u32::from_le_bytes(mono[28..32].try_into().unwrap()), 8_000 * 2);
        assert_eq!(u16::from_le_bytes(mono[32..34].try_into().unwrap()), 2);
        assert_eq!(mono.len(), 46);
        let samples: Vec<i16> = file[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(samples, vec![0, 32767, -32767, 32767]);
    }

    #[test]
    fn raw_bytes_back_to_samples() {
        let bytes: Vec<u8> = [0.5f32, -0.25].iter().flat_map(|s| s.to_le_bytes()).chain([1, 2]).collect();
        assert_eq!(from_raw(&bytes), vec![0.5, -0.25]);
    }
}
