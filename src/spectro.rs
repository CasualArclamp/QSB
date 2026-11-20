//! Spectrogram of what is being played: columns of dB values, newest last.

use crate::channel::Mode;
use num_complex::Complex32 as C32;
use rustfft::{Fft, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;

pub struct Spectrogram {
    pub n: usize,
    hop: usize,
    fs: u32,
    channels: usize,
    /// One complex pane (I/Q, both sides of zero) instead of one real pane a channel.
    complex: bool,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    pending: Vec<f32>,
    /// `panes()` deques of columns; a column is `bins()` dB values, lowest frequency first.
    pub cols: Vec<VecDeque<Vec<f32>>>,
    pub max_cols: usize,
    /// Columns made so far, to tell when the picture needs redrawing.
    pub count: u64,
}

impl Spectrogram {
    pub fn new(fs: u32, channels: usize, mode: Mode, max_cols: usize) -> Self {
        // About 46 ms: 2048 points at 44.1 and 48 kHz, 512 at 12 kHz.
        let n = ((f64::from(fs) * 0.046) as usize).next_power_of_two().clamp(256, 8192);
        let complex = channels == 2 && mode == Mode::Iq;
        let window = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
            .collect();
        Self {
            n,
            hop: n / 2,
            fs,
            channels,
            complex,
            window,
            fft: FftPlanner::new().plan_fft_forward(n),
            pending: Vec::new(),
            cols: vec![VecDeque::new(); if complex { 1 } else { channels }],
            max_cols,
            count: 0,
        }
    }

    pub fn panes(&self) -> usize {
        self.cols.len()
    }
    pub fn is_complex(&self) -> bool {
        self.complex
    }
    pub fn bins(&self) -> usize {
        if self.complex { self.n } else { self.n / 2 }
    }
    pub fn bin_hz(&self) -> f32 {
        self.fs as f32 / self.n as f32
    }
    pub fn seconds_per_col(&self) -> f32 {
        self.hop as f32 / self.fs as f32
    }

    /// Add interleaved frames; makes a column for every half window.
    pub fn feed(&mut self, frames: &[f32]) {
        self.pending.extend_from_slice(frames);
        let ch = self.channels;
        let mut buf = vec![C32::ZERO; self.n];
        let mut used = 0;
        while self.pending.len() - used >= self.n * ch {
            let at = &self.pending[used..used + self.n * ch];
            // 0 dB = a full-scale sine.
            let scale = 2.0 / self.window.iter().sum::<f32>();
            let to_db = |z: &C32| 10.0 * ((z.norm_sqr() * scale * scale).max(1e-14)).log10();
            if self.complex {
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = C32::new(at[2 * i], at[2 * i + 1]) * self.window[i];
                }
                self.fft.process(&mut buf);
                let h = self.n / 2;
                let col = buf[h..].iter().chain(&buf[..h]).map(to_db).collect();
                self.cols[0].push_back(col);
            } else {
                for c in 0..ch {
                    for (i, b) in buf.iter_mut().enumerate() {
                        *b = C32::new(at[ch * i + c] * self.window[i], 0.0);
                    }
                    self.fft.process(&mut buf);
                    self.cols[c].push_back(buf[..self.n / 2].iter().map(to_db).collect());
                }
            }
            self.count += 1;
            used += self.hop * ch;
        }
        self.pending.drain(..used);
        for c in &mut self.cols {
            while c.len() > self.max_cols {
                c.pop_front();
            }
        }
    }
}

/// DecSAT's waterfall colours (by way of DecAero and DecHAMDRM `theme.rs`): black
/// through purple and orange to pale yellow.
pub fn palette() -> Vec<[u8; 3]> {
    const STOPS: [(f32, [u8; 3]); 6] = [
        (0.0, [0, 0, 4]),
        (0.2, [40, 11, 84]),
        (0.4, [101, 21, 110]),
        (0.6, [159, 42, 99]),
        (0.8, [237, 105, 37]),
        (1.0, [252, 255, 164]),
    ];
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            let k = STOPS
                .iter()
                .rposition(|(at, _)| *at <= t)
                .unwrap_or(0)
                .min(STOPS.len() - 2);
            let (a, ca) = STOPS[k];
            let (b, cb) = STOPS[k + 1];
            let u = (t - a) / (b - a);
            std::array::from_fn(|j| (ca[j] as f32 + (cb[j] as f32 - ca[j] as f32) * u) as u8)
        })
        .collect()
}

/// A picture of a whole file's spectrogram (time left to right, panes stacked, low
/// frequencies at the bottom of each), for checking renders without the window.
pub fn save_png(
    audio: &crate::wav::Audio,
    mode: Mode,
    max_hz: f32,
    out: &std::path::Path,
) -> Result<(), String> {
    let mut s = Spectrogram::new(audio.fs, audio.channels, mode, usize::MAX);
    s.feed(&audio.samples);
    let total = s.cols[0].len();
    if total == 0 {
        return Err("too short for a spectrogram".into());
    }
    let bins = if s.is_complex() {
        s.bins()
    } else {
        ((max_hz / s.bin_hz()) as usize).clamp(8, s.bins())
    };
    let width = total.min(1600);
    let top = s
        .cols
        .iter()
        .flatten()
        .flat_map(|c| c[..bins].iter().copied())
        .fold(f32::MIN, f32::max);
    let pal = palette();
    let height = (bins + 2) * s.panes();
    let mut rgb = vec![0u8; width * height * 3];
    for (pane, cols) in s.cols.iter().enumerate() {
        for x in 0..width {
            let col = &cols[x * total / width];
            for (b, &db) in col[..bins].iter().enumerate() {
                let t = ((db - (top - 70.0)) / 70.0).clamp(0.0, 1.0);
                let y = pane * (bins + 2) + (bins - 1 - b);
                let at = (y * width + x) * 3;
                rgb[at..at + 3].copy_from_slice(&pal[(t * 255.0) as usize]);
            }
        }
    }
    image::save_buffer(out, &rgb, width as u32, height as u32, image::ColorType::Rgb8)
        .map_err(|e| e.to_string())
}
