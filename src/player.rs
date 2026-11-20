//! Live preview: the sound card's callback pulls the file through the channel, so a
//! slider move is heard at once. (Stream set-up as in DecAero's `audio.rs`.)

use crate::channel::{Channel, Mode, Params, Tap};
use crate::wav::Audio;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What to play. Changing it (and bumping `Shared::generation`) restarts the channel.
#[derive(Clone)]
pub struct Source {
    pub audio: Arc<Audio>,
    pub mode: Mode,
    pub seed: u64,
}

pub struct Shared {
    pub source: Mutex<Option<Source>>,
    pub generation: AtomicU64,
    pub params: Mutex<Params>,
    pub playing: AtomicBool,
    pub looping: AtomicBool,
    /// Frame being played.
    pub pos: AtomicUsize,
    /// Frame to jump to, or -1.
    pub seek: AtomicI64,
    /// Volume as the bits of an f32.
    pub volume: AtomicU32,
    /// Processed frames (interleaved, file rate) for the spectrogram.
    pub scope: Mutex<VecDeque<f32>>,
    pub taps: Mutex<Vec<Tap>>,
    pub clipped: AtomicU64,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            source: Mutex::new(None),
            generation: AtomicU64::new(0),
            params: Mutex::new(Params::default()),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(true),
            pos: AtomicUsize::new(0),
            seek: AtomicI64::new(-1),
            volume: AtomicU32::new(0.7f32.to_bits()),
            scope: Mutex::new(VecDeque::new()),
            taps: Mutex::new(Vec::new()),
            clipped: AtomicU64::new(0),
        })
    }

    pub fn set_source(&self, s: Option<Source>) {
        *self.source.lock().unwrap() = s;
        self.pos.store(0, Ordering::Relaxed);
        self.seek.store(-1, Ordering::Relaxed);
        self.clipped.store(0, Ordering::Relaxed);
        self.scope.lock().unwrap().clear();
        self.taps.lock().unwrap().clear();
        self.generation.fetch_add(1, Ordering::Release);
    }
}

pub struct Output {
    /// Kept alive: dropping the stream stops the sound.
    _stream: cpal::Stream,
}

impl Output {
    pub fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no sound output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let (rate, channels, format) = (
            config.sample_rate(),
            config.channels() as usize,
            config.sample_format(),
        );
        let config: cpal::StreamConfig = config.into();
        let err = |e| eprintln!("audio output: {e}");
        let stream = match format {
            cpal::SampleFormat::F32 => {
                device.build_output_stream(config, callback::<f32>(shared, channels, rate), err, None)
            }
            cpal::SampleFormat::I16 => {
                device.build_output_stream(config, callback::<i16>(shared, channels, rate), err, None)
            }
            cpal::SampleFormat::U16 => {
                device.build_output_stream(config, callback::<u16>(shared, channels, rate), err, None)
            }
            f => return Err(format!("sound device format {f} is not handled")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream })
    }
}

/// Frames run through the channel at a time.
const BLOCK: usize = 256;

/// The file through the channel, one frame at a time, in the file's own rate.
struct Feed {
    shared: Arc<Shared>,
    generation: u64,
    source: Option<Source>,
    channel: Option<Channel>,
    params: Params,
    input: Vec<f32>,
    block: Vec<f32>,
    at: usize,
}

impl Feed {
    fn refresh(&mut self) {
        let g = self.shared.generation.load(Ordering::Acquire);
        if g != self.generation {
            self.generation = g;
            self.source = self.shared.source.lock().ok().and_then(|s| s.clone());
            self.channel = self.source.as_ref().map(|s| {
                Channel::new(s.audio.fs, s.audio.channels, s.mode, s.seed, s.audio.power)
            });
            self.block.clear();
            self.at = 0;
        }
        if let Ok(p) = self.shared.params.try_lock()
            && *p != self.params
        {
            self.params = p.clone();
        }
    }

    fn file_rate(&self) -> Option<(u32, usize)> {
        self.source.as_ref().map(|s| (s.audio.fs, s.audio.channels))
    }

    /// The next processed frame, or `None` when stopped or at the end.
    fn next(&mut self) -> Option<[f32; 2]> {
        let (src, chan) = (self.source.as_ref()?, self.channel.as_mut()?);
        let ch = src.audio.channels;
        if self.at >= self.block.len() {
            let sh = &self.shared;
            if !sh.playing.load(Ordering::Relaxed) {
                return None;
            }
            let frames = src.audio.frames();
            let seek = sh.seek.swap(-1, Ordering::Relaxed);
            let mut pos = sh.pos.load(Ordering::Relaxed);
            if seek >= 0 {
                pos = (seek as usize).min(frames.saturating_sub(1));
            }
            if pos >= frames {
                if sh.looping.load(Ordering::Relaxed) {
                    pos = 0;
                } else {
                    sh.playing.store(false, Ordering::Relaxed);
                    sh.pos.store(0, Ordering::Relaxed);
                    return None;
                }
            }
            let n = BLOCK.min(frames - pos);
            self.input.clear();
            self.input
                .extend_from_slice(&src.audio.samples[pos * ch..(pos + n) * ch]);
            self.block.resize(n * ch, 0.0);
            chan.process(&self.params, &self.input, &mut self.block);
            self.at = 0;
            sh.pos.store(pos + n, Ordering::Relaxed);
            sh.clipped.store(chan.clipped, Ordering::Relaxed);
            if let Ok(mut s) = sh.scope.try_lock() {
                s.extend(self.block.iter());
                // Never more than a few seconds waiting for the window.
                let cap = src.audio.fs as usize * ch * 4;
                if s.len() > cap {
                    let extra = s.len() - cap;
                    s.drain(..extra);
                }
            }
            if let Ok(mut t) = sh.taps.try_lock() {
                t.clear();
                if !self.params.bypass {
                    t.extend_from_slice(&chan.taps);
                }
            }
        }
        let f = &self.block[self.at..self.at + ch];
        self.at += ch;
        Some([f[0], f[ch - 1]])
    }
}

/// The card's callback: frames from [`Feed`] brought to the card's rate by cubic
/// interpolation (good enough for a preview; the rendered file is not resampled).
fn callback<T>(
    shared: Arc<Shared>,
    channels: usize,
    rate: u32,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let mut feed = Feed {
        shared: shared.clone(),
        generation: u64::MAX,
        source: None,
        channel: None,
        params: Params::default(),
        input: Vec::new(),
        block: Vec::new(),
        at: 0,
    };
    let mut hist = [[0f32; 2]; 4];
    let mut frac = 0f64;
    move |data: &mut [T], _| {
        feed.refresh();
        let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed)).powi(2);
        let step = feed
            .file_rate()
            .map_or(1.0, |(fs, _)| f64::from(fs) / f64::from(rate));
        for frame in data.chunks_mut(channels.max(1)) {
            frac += step;
            while frac >= 1.0 {
                frac -= 1.0;
                hist.copy_within(1.., 0);
                hist[3] = feed.next().unwrap_or([0.0; 2]);
            }
            let t = frac as f32;
            let mut out = [0f32; 2];
            for (c, o) in out.iter_mut().enumerate() {
                let [p0, p1, p2, p3] = hist.map(|h| h[c]);
                let a = (p3 - p0) * 0.5 + (p1 - p2) * 1.5;
                let b = p0 - p1 * 2.5 + p2 * 2.0 - p3 * 0.5;
                let cc = (p2 - p0) * 0.5;
                *o = ((((a * t + b) * t + cc) * t + p1) * volume).clamp(-1.0, 1.0);
            }
            for (c, s) in frame.iter_mut().enumerate() {
                *s = T::from_sample(if c < 2 { out[c] } else { 0.0 });
            }
        }
    }
}
