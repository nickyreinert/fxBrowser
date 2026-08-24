//! Decodes an audio file into a small per-channel min/max peak table, for
//! drawing a static waveform overview (mono = one lane, stereo = two lanes).

use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn decode_channels(path: &Path) -> Option<Vec<Vec<f32>>> {
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
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    // Cap at two channels (mono/stereo) — anything wider is rare for SFX and
    // we only render at most two lanes anyway.
    let mut channels: Vec<Vec<f32>> = Vec::new();

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1).min(2);
        if channels.is_empty() {
            channels = vec![Vec::new(); ch];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        let stride = spec.channels.count().max(1);
        for frame in buf.samples().chunks(stride) {
            for (i, out) in channels.iter_mut().enumerate() {
                out.push(frame.get(i).copied().unwrap_or(0.0));
            }
        }
    }

    if channels.iter().all(|c| c.is_empty()) {
        None
    } else {
        Some(channels)
    }
}

/// Returns, per channel, `buckets` (min, max) peak pairs across the whole file.
pub fn compute_peaks(path: &Path, buckets: usize) -> Option<Vec<Vec<(f32, f32)>>> {
    let channels = decode_channels(path)?;
    let buckets = buckets.max(1);

    let mut result = Vec::with_capacity(channels.len());
    for samples in channels {
        if samples.is_empty() {
            continue;
        }
        let win = (samples.len() / buckets).max(1);
        let peaks: Vec<(f32, f32)> = samples
            .chunks(win)
            .map(|w| {
                w.iter()
                    .fold((f32::MAX, f32::MIN), |(mn, mx), &s| (mn.min(s), mx.max(s)))
            })
            .collect();
        result.push(peaks);
    }
    Some(result)
}
