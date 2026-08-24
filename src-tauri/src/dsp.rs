//! Cheap, deterministic signal-processing tags: no ML, no model weights.
//!
//! Decodes at most the first ~8 seconds of a file, computes a handful of
//! classic DSP features (RMS envelope shape, zero-crossing rate, spectral
//! centroid/flatness via a single windowed FFT), and maps them through a
//! fixed rule set into a small tag vocabulary. This is a best-effort
//! approximation, not authoritative classification — it exists to make a
//! huge, unlabeled library a bit more filterable out of the box.

use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const ANALYSIS_SECONDS: usize = 8;
const ENVELOPE_WINDOWS: usize = 50;

struct Features {
    attack_time_frac: f32,
    decay_ratio: f32,
    spectral_centroid_hz: f32,
    spectral_flatness: f32,
}

/// Returns best-effort classification tags for the audio file at `path`, or
/// an empty vec if it couldn't be decoded (unsupported/corrupt file — not a
/// hard error, indexing continues without DSP tags for that file).
pub fn classify_file(path: &Path, duration_secs: f64) -> Vec<&'static str> {
    let Some((samples, sample_rate)) = decode_mono(path) else {
        return Vec::new();
    };
    if samples.len() < 64 {
        return Vec::new();
    }
    let features = compute_features(&samples, sample_rate);
    classify(&features, duration_secs)
}

fn decode_mono(path: &Path) -> Option<(Vec<f32>, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    let max_samples = sample_rate as usize * ANALYSIS_SECONDS;
    let mut samples: Vec<f32> = Vec::with_capacity(max_samples.min(1_000_000));

    while samples.len() < max_samples {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);

        for frame in buf.samples().chunks(channels) {
            let sum: f32 = frame.iter().sum();
            samples.push(sum / channels as f32);
            if samples.len() >= max_samples {
                break;
            }
        }
    }

    if samples.is_empty() {
        None
    } else {
        Some((samples, sample_rate))
    }
}

fn compute_features(samples: &[f32], sample_rate: u32) -> Features {
    // RMS envelope, split into equal-length windows across the analyzed audio.
    let win_len = (samples.len() / ENVELOPE_WINDOWS).max(1);
    let envelope: Vec<f32> = samples
        .chunks(win_len)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .collect();

    let (peak_idx, &peak_val) = envelope
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap_or((0, &0.0));
    let attack_time_frac = peak_idx as f32 / envelope.len().max(1) as f32;
    let end_val = *envelope.last().unwrap_or(&0.0);
    let decay_ratio = end_val / peak_val.max(1e-9);

    // Single windowed FFT over the analyzed audio for spectral shape.
    let fft_len = samples
        .len()
        .min(sample_rate as usize)
        .next_power_of_two()
        .clamp(256, 65536);
    let mut buf: Vec<Complex<f32>> = samples
        .iter()
        .take(fft_len)
        .enumerate()
        .map(|(i, &s)| {
            // Hann window to reduce spectral leakage.
            let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (fft_len as f32 - 1.0)).cos();
            Complex::new(s * w, 0.0)
        })
        .collect();
    buf.resize(fft_len, Complex::new(0.0, 0.0));

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_len);
    fft.process(&mut buf);

    let half = fft_len / 2;
    let mags: Vec<f32> = buf[..half].iter().map(|c| c.norm()).collect();
    let mag_sum: f32 = mags.iter().sum::<f32>().max(1e-9);

    let spectral_centroid_hz = mags
        .iter()
        .enumerate()
        .map(|(i, &m)| m * (i as f32 * sample_rate as f32 / fft_len as f32))
        .sum::<f32>()
        / mag_sum;

    let eps = 1e-9;
    let log_sum: f32 = mags.iter().map(|&m| (m + eps).ln()).sum();
    let geo_mean = (log_sum / mags.len() as f32).exp();
    let arith_mean = mag_sum / mags.len() as f32;
    let spectral_flatness = (geo_mean / arith_mean.max(eps)).clamp(0.0, 1.0);

    Features {
        attack_time_frac,
        decay_ratio,
        spectral_centroid_hz,
        spectral_flatness,
    }
}

fn classify(f: &Features, duration_secs: f64) -> Vec<&'static str> {
    let mut tags = Vec::new();

    let percussive = f.attack_time_frac < 0.15 && f.decay_ratio < 0.3;
    let sustained = f.decay_ratio > 0.6;
    let tonal = f.spectral_flatness < 0.15;
    let noisy = f.spectral_flatness > 0.5;

    if percussive && duration_secs < 1.5 {
        tags.push("impact");
    } else if noisy && !sustained && duration_secs < 4.0 {
        tags.push("whoosh");
    } else if sustained && duration_secs > 3.0 {
        tags.push(if tonal { "drone" } else { "ambience" });
    }

    if tonal {
        tags.push("tonal");
    }
    if noisy {
        tags.push("noisy");
    }
    if f.spectral_centroid_hz > 3000.0 {
        tags.push("bright");
    } else if f.spectral_centroid_hz < 500.0 {
        tags.push("dark");
    }

    tags
}
