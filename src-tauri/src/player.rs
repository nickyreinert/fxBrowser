use rodio::{Decoder, OutputStream, Sink, Source};
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

enum PlayerCommand {
    Play(String, bool),
    Stop,
}

/// Wraps a sample stream and, every ~30ms of audio, publishes a short-window
/// RMS level into `level` — the "Mäusekino" VU meter reads this. Placed
/// *outside* any looping wrapper so it sees every sample actually reaching
/// the output on every repeat, not just the first pass through a cached loop
/// buffer.
struct LevelTap<S> {
    inner: S,
    level: Arc<AtomicU32>,
    window: Vec<f32>,
    window_size: usize,
}

impl<S> LevelTap<S>
where
    S: Source<Item = f32>,
{
    fn new(inner: S, level: Arc<AtomicU32>) -> Self {
        let window_size = ((inner.sample_rate() as usize) * 30 / 1000).max(64);
        Self {
            inner,
            level,
            window: Vec::with_capacity(window_size),
            window_size,
        }
    }
}

impl<S> Iterator for LevelTap<S>
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
                Some(sample)
            }
            None => {
                self.level.store(0.0f32.to_bits(), Ordering::Relaxed);
                None
            }
        }
    }
}

impl<S> Source for LevelTap<S>
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
}

/// Plays audio natively via `rodio` (ALSA/PipeWire), bypassing WebKitGTK's
/// GStreamer-backed `<audio>` element entirely. The cpal output stream isn't
/// Send/Sync-friendly, so it's owned by a dedicated thread and driven over a
/// channel instead of living in Tauri's managed state directly.
pub struct Player {
    tx: Sender<PlayerCommand>,
    level: Arc<AtomicU32>,
}

impl Player {
    pub fn spawn() -> Self {
        let (tx, rx) = channel::<PlayerCommand>();
        let level = Arc::new(AtomicU32::new(0));
        let level_for_thread = level.clone();

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
                    }
                }
            }
        });

        Self { tx, level }
    }

    fn load_and_play(
        stream_handle: &rodio::OutputStreamHandle,
        path: &str,
        loop_playback: bool,
        level: Arc<AtomicU32>,
    ) -> Result<Sink, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let source = Decoder::new(BufReader::new(file))
            .map_err(|e| e.to_string())?
            .convert_samples::<f32>();
        let sink = Sink::try_new(stream_handle).map_err(|e| e.to_string())?;
        if loop_playback {
            sink.append(LevelTap::new(source.buffered().repeat_infinite(), level));
        } else {
            sink.append(LevelTap::new(source, level));
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

    pub fn current_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}
