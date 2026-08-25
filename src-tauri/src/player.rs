use rodio::source::SeekError;
use rodio::{Decoder, OutputStream, Sink, Source};
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::f32::consts::PI;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

enum PlayerCommand {
    Play(String, bool),
    Stop,
    Seek(f64),
}

/// Loops `inner` by seeking back to the start on end-of-stream instead of
/// rodio's built-in `repeat_infinite()`, which caches the whole source in a
/// `Buffered` wrapper that unconditionally rejects `try_seek` — that would
/// make click-to-seek on the waveform impossible while looping.
struct LoopingSource<S> {
    inner: S,
}

impl<S> Iterator for LoopingSource<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        match self.inner.next() {
            Some(s) => Some(s),
            None => {
                self.inner.try_seek(Duration::ZERO).ok()?;
                self.inner.next()
            }
        }
    }
}

impl<S> Source for LoopingSource<S>
where
    S: Source<Item = f32>,
{
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)
    }
}

/// How many mono samples the spectrum analyzer looks at per FFT — sets the
/// frequency resolution (sample_rate / RING_SIZE per bin) and, at typical
/// sample rates, covers roughly a 40-90ms analysis window.
const RING_SIZE: usize = 2048;

/// Fixed-size circular buffer of the most recent mono samples, shared with
/// the frontend-facing spectrum command so it can FFT on demand without
/// touching the playback thread.
struct RingBuffer {
    buf: Vec<f32>,
    pos: usize,
    filled: bool,
}

impl RingBuffer {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0.0; size],
            pos: 0,
            filled: false,
        }
    }

    fn push(&mut self, sample: f32) {
        self.buf[self.pos] = sample;
        self.pos = (self.pos + 1) % self.buf.len();
        if self.pos == 0 {
            self.filled = true;
        }
    }

    /// Returns the buffer contents in chronological order (oldest first).
    fn snapshot(&self) -> Vec<f32> {
        if !self.filled {
            self.buf[..self.pos].to_vec()
        } else {
            let mut v = Vec::with_capacity(self.buf.len());
            v.extend_from_slice(&self.buf[self.pos..]);
            v.extend_from_slice(&self.buf[..self.pos]);
            v
        }
    }
}

/// Wraps a sample stream and, every ~30ms of audio, publishes a short-window
/// RMS level into `level` (used for the overall Play/Stop-adjacent meter
/// scale), while continuously downmixing to mono and feeding `ring` — the
/// spectrum analyzer reads snapshots of that on demand. Placed *outside* any
/// looping wrapper so it sees every sample actually reaching the output on
/// every repeat, not just the first pass through a cached loop buffer.
struct AnalysisTap<S> {
    inner: S,
    level: Arc<AtomicU32>,
    window: Vec<f32>,
    window_size: usize,
    ring: Arc<Mutex<RingBuffer>>,
    channels: u16,
    chan_accum: f32,
    chan_idx: u16,
}

impl<S> AnalysisTap<S>
where
    S: Source<Item = f32>,
{
    fn new(inner: S, level: Arc<AtomicU32>, ring: Arc<Mutex<RingBuffer>>) -> Self {
        let window_size = ((inner.sample_rate() as usize) * 30 / 1000).max(64);
        let channels = inner.channels();
        Self {
            inner,
            level,
            window: Vec::with_capacity(window_size),
            window_size,
            ring,
            channels,
            chan_accum: 0.0,
            chan_idx: 0,
        }
    }
}

impl<S> Iterator for AnalysisTap<S>
where
    S: Iterator<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        match self.inner.next() {
            Some(sample) => {
                self.window.push(sample);
                if self.window.len() >= self.window_size {
                    let rms = (self.window.iter().map(|s| s * s).sum::<f32>()
                        / self.window.len() as f32)
                        .sqrt();
                    self.level.store(rms.to_bits(), Ordering::Relaxed);
                    self.window.clear();
                }

                self.chan_accum += sample;
                self.chan_idx += 1;
                if self.chan_idx >= self.channels.max(1) {
                    let mono = self.chan_accum / self.chan_idx as f32;
                    if let Ok(mut ring) = self.ring.lock() {
                        ring.push(mono);
                    }
                    self.chan_accum = 0.0;
                    self.chan_idx = 0;
                }

                Some(sample)
            }
            None => {
                self.level.store(0.0f32.to_bits(), Ordering::Relaxed);
                None
            }
        }
    }
}

impl<S> Source for AnalysisTap<S>
where
    S: Source<Item = f32>,
{
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)
    }
}

/// Plays audio natively via `rodio` (ALSA/PipeWire), bypassing WebKitGTK's
/// GStreamer-backed `<audio>` element entirely. The cpal output stream isn't
/// Send/Sync-friendly, so it's owned by a dedicated thread and driven over a
/// channel instead of living in Tauri's managed state directly.
pub struct Player {
    tx: Sender<PlayerCommand>,
    level: Arc<AtomicU32>,
    ring: Arc<Mutex<RingBuffer>>,
    sample_rate: Arc<AtomicU32>,
    // RING_SIZE never changes, so the forward FFT plan (the expensive part of
    // an FFT call — rustfft caches twiddle factors per-planner, which a fresh
    // FftPlanner::new() would throw away) is built once and reused for every
    // spectrum request instead of replanning on every animation frame.
    fft: Arc<dyn Fft<f32>>,
}

impl Player {
    pub fn spawn() -> Self {
        let (tx, rx) = channel::<PlayerCommand>();
        let level = Arc::new(AtomicU32::new(0));
        let ring = Arc::new(Mutex::new(RingBuffer::new(RING_SIZE)));
        let sample_rate = Arc::new(AtomicU32::new(44100));
        let fft = FftPlanner::new().plan_fft_forward(RING_SIZE);
        let level_for_thread = level.clone();
        let ring_for_thread = ring.clone();
        let sample_rate_for_thread = sample_rate.clone();

        std::thread::spawn(move || {
            let (_stream, stream_handle) = match OutputStream::try_default() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[fxbrowser] failed to open audio output device: {e}");
                    return;
                }
            };
            let mut current_sink: Option<Sink> = None;

            for cmd in rx {
                match cmd {
                    PlayerCommand::Play(path, loop_playback) => {
                        if let Some(sink) = current_sink.take() {
                            sink.stop();
                        }
                        match Self::load_and_play(
                            &stream_handle,
                            &path,
                            loop_playback,
                            level_for_thread.clone(),
                            ring_for_thread.clone(),
                            sample_rate_for_thread.clone(),
                        ) {
                            Ok(sink) => current_sink = Some(sink),
                            Err(e) => eprintln!("[fxbrowser] failed to play {path}: {e}"),
                        }
                    }
                    PlayerCommand::Stop => {
                        if let Some(sink) = current_sink.take() {
                            sink.stop();
                        }
                        level_for_thread.store(0.0f32.to_bits(), Ordering::Relaxed);
                        if let Ok(mut ring) = ring_for_thread.lock() {
                            *ring = RingBuffer::new(RING_SIZE);
                        }
                    }
                    PlayerCommand::Seek(secs) => {
                        if let Some(sink) = &current_sink {
                            let _ = sink.try_seek(Duration::from_secs_f64(secs.max(0.0)));
                        }
                    }
                }
            }
        });

        Self {
            tx,
            level,
            ring,
            sample_rate,
            fft,
        }
    }

    fn load_and_play(
        stream_handle: &rodio::OutputStreamHandle,
        path: &str,
        loop_playback: bool,
        level: Arc<AtomicU32>,
        ring: Arc<Mutex<RingBuffer>>,
        sample_rate: Arc<AtomicU32>,
    ) -> Result<Sink, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let source = Decoder::new(BufReader::new(file))
            .map_err(|e| e.to_string())?
            .convert_samples::<f32>();
        sample_rate.store(source.sample_rate(), Ordering::Relaxed);
        let sink = Sink::try_new(stream_handle).map_err(|e| e.to_string())?;
        if loop_playback {
            sink.append(AnalysisTap::new(
                LoopingSource { inner: source },
                level,
                ring,
            ));
        } else {
            sink.append(AnalysisTap::new(source, level, ring));
        }
        sink.play();
        Ok(sink)
    }

    pub fn play(&self, path: String, loop_playback: bool) {
        let _ = self.tx.send(PlayerCommand::Play(path, loop_playback));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(PlayerCommand::Stop);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(PlayerCommand::Seek(secs));
    }

    pub fn current_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Runs a single windowed FFT over the most recent ~`RING_SIZE` samples
    /// and buckets the magnitude spectrum into `bars` log-spaced frequency
    /// bands (so each bar represents a range of Hz, not a moment in time) —
    /// a classic real-time spectrum analyzer display.
    pub fn current_spectrum(&self, bars: usize) -> Vec<f32> {
        let samples = match self.ring.lock() {
            Ok(ring) => ring.snapshot(),
            Err(_) => return vec![0.0; bars],
        };
        if samples.len() < RING_SIZE {
            return vec![0.0; bars];
        }
        let sample_rate = self.sample_rate.load(Ordering::Relaxed).max(1) as f32;

        let n = samples.len();
        let mut buf: Vec<Complex<f32>> = samples
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (n as f32 - 1.0)).cos();
                Complex::new(s * w, 0.0)
            })
            .collect();

        self.fft.process(&mut buf);

        let half = n / 2;
        let mags: Vec<f32> = buf[..half].iter().map(|c| c.norm()).collect();

        let nyquist = sample_rate / 2.0;
        let min_hz = 40.0_f32.min(nyquist * 0.9);
        let max_hz = nyquist.max(min_hz + 1.0);
        let ratio = max_hz / min_hz;

        let norm_factor = n as f32 * 0.25;

        (0..bars)
            .map(|i| {
                let f_lo = min_hz * ratio.powf(i as f32 / bars as f32);
                let f_hi = min_hz * ratio.powf((i + 1) as f32 / bars as f32);
                let bin_lo = ((f_lo * n as f32 / sample_rate) as usize).clamp(1, half - 1);
                let bin_hi = ((f_hi * n as f32 / sample_rate) as usize).clamp(bin_lo + 1, half);
                let slice = &mags[bin_lo..bin_hi];
                let peak = slice.iter().cloned().fold(0.0_f32, f32::max);
                // Most real-world audio has far more low/mid energy than
                // treble, so a flat dB mapping leaves the right half of the
                // display nearly dead. Apply a mild per-octave tilt (like a
                // hardware analyzer's "slope" control) so activity is
                // visible across the whole width instead of bunched at the
                // low end.
                let f_center = (f_lo * f_hi).sqrt();
                let tilt_db = 3.5 * (f_center / min_hz).max(1.0).log2();
                let db = 20.0 * (peak / norm_factor).max(1e-6).log10() + tilt_db;
                ((db + 55.0) / 55.0).clamp(0.0, 1.0)
            })
            .collect()
    }
}
