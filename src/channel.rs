//! The multipath channel: a tapped delay line in the style of the Watterson HF model
//! (C. C. Watterson, J. R. Juroshek, W. D. Bensema, "Experimental confirmation of an HF
//! channel model", IEEE Trans. Commun. Technol., Dec 1970; the model ITU-R F.1487
//! Annex 1 uses).
//!
//! Each path has a delay, a gain, a Doppler shift and a Doppler spread. A path with a
//! spread fades (Rayleigh, Gaussian Doppler spectrum); one without is steady. Two steady
//! paths with different shifts give notches that sweep through the band at
//! `shift difference / delay difference` Hz per second, which is the diagonal striping
//! seen on real recordings.
//!
//! The channel works on a complex signal. A real audio channel is first made analytic
//! (Hilbert transformer) so a Doppler shift moves every frequency the same way, as it
//! does at RF for a single-sideband signal.

use num_complex::Complex32 as C32;
use std::f64::consts::TAU;

/// How the file's channels map to the radio signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Each channel is single-sideband audio; both get the same fading.
    Audio,
    /// Left is I, right is Q.
    Iq,
    /// Left is the lower sideband, right the upper, of one carrier: they fade as one
    /// RF channel, so the notches run in opposite directions in the two.
    Isb,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Audio, Mode::Iq, Mode::Isb];
    pub fn label(self) -> &'static str {
        match self {
            Mode::Audio => "Audio (SSB)",
            Mode::Iq => "I/Q (L = I, R = Q)",
            Mode::Isb => "ISB (L = LSB, R = USB)",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Mode::Audio => "audio",
            Mode::Iq => "iq",
            Mode::Isb => "isb",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Path {
    pub on: bool,
    pub delay_ms: f32,
    pub gain_db: f32,
    /// Doppler shift: the whole path moved in frequency.
    pub shift_hz: f32,
    /// Doppler spread, twice the standard deviation of the Gaussian Doppler spectrum
    /// (the ITU-R F.1487 meaning). 0 = no fading on this path.
    pub spread_hz: f32,
}

impl Path {
    pub const fn new(delay_ms: f32, gain_db: f32, shift_hz: f32, spread_hz: f32) -> Self {
        Self {
            on: true,
            delay_ms,
            gain_db,
            shift_hz,
            spread_hz,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Params {
    pub paths: Vec<Path>,
    pub noise: bool,
    /// Signal to noise ratio with the noise measured in 3 kHz.
    pub snr_db: f32,
    pub out_gain_db: f32,
    /// Scale so the average level stays that of the input however many paths there are.
    pub keep_level: bool,
    pub bypass: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            paths: vec![Path::new(0.0, 0.0, 0.0, 0.0)],
            noise: false,
            snr_db: 20.0,
            out_gain_db: -3.0,
            keep_level: true,
            bypass: false,
        }
    }
}

pub const MAX_DELAY_MS: f32 = 40.0;
/// Bandwidth the SNR figure refers to.
const NOISE_BW_HZ: f32 = 3000.0;

// ---------------------------------------------------------------- random numbers

/// xorshift64* with a splitmix64 start.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    /// Complex Gaussian with E|z|^2 = 1 (Box-Muller).
    pub fn gauss(&mut self) -> C32 {
        let r = (-self.unit().ln()).sqrt();
        let a = TAU * self.unit();
        C32::new((r * a.cos()) as f32, (r * a.sin()) as f32)
    }
}

// ---------------------------------------------------------------- fading

/// Samples of the fading process per unit of normalised time. In normalised time the
/// Doppler spectrum has a standard deviation of 1 Hz, so this puts the fold-over point
/// at 6 sigma (-78 dB).
const FADE_OS: f64 = 12.0;
const FADE_HALF: usize = 8;
const FADE_TAPS: usize = 2 * FADE_HALF + 1;

/// A unit-power complex Gaussian process with a Gaussian Doppler spectrum.
///
/// It is made once in normalised time (white noise through a Gaussian FIR) and read at
/// a speed set by the spread, so the spread can be changed while it runs without a jump.
struct Fader {
    rng: Rng,
    fir: [f32; FADE_TAPS],
    noise: [C32; FADE_TAPS],
    /// Four consecutive samples of the process, for cubic interpolation.
    pts: [C32; 4],
    frac: f64,
}

impl Fader {
    fn new(seed: u64) -> Self {
        // Power spectrum exp(-f^2 / (2 sigma^2)) needs the amplitude response
        // exp(-f^2 / (4 sigma^2)), whose impulse response is exp(-4 pi^2 sigma^2 t^2).
        let mut fir = [0f32; FADE_TAPS];
        for (i, c) in fir.iter_mut().enumerate() {
            let t = (i as f64 - FADE_HALF as f64) / FADE_OS;
            *c = (-4.0 * std::f64::consts::PI.powi(2) * t * t).exp() as f32;
        }
        let norm = fir.iter().map(|c| c * c).sum::<f32>().sqrt();
        fir.iter_mut().for_each(|c| *c /= norm);
        let mut f = Self {
            rng: Rng::new(seed),
            fir,
            noise: [C32::ZERO; FADE_TAPS],
            pts: [C32::ZERO; 4],
            frac: 0.0,
        };
        for _ in 0..FADE_TAPS + 4 {
            f.advance();
        }
        f
    }

    fn advance(&mut self) {
        self.noise.copy_within(1.., 0);
        self.noise[FADE_TAPS - 1] = self.rng.gauss();
        let v: C32 = self.noise.iter().zip(&self.fir).map(|(n, c)| n * c).sum();
        self.pts.copy_within(1.., 0);
        self.pts[3] = v;
    }

    /// The next value, one audio sample later.
    fn step(&mut self, spread_hz: f32, fs: f64) -> C32 {
        self.frac += FADE_OS * f64::from(spread_hz) * 0.5 / fs;
        while self.frac >= 1.0 {
            self.frac -= 1.0;
            self.advance();
        }
        // Catmull-Rom between pts[1] and pts[2].
        let t = self.frac as f32;
        let [p0, p1, p2, p3] = self.pts;
        let a = (p3 - p0) * 0.5 + (p1 - p2) * 1.5;
        let b = p0 - p1 * 2.5 + p2 * 2.0 - p3 * 0.5;
        let c = (p2 - p0) * 0.5;
        ((a * t + b) * t + c) * t + p1
    }
}

// ---------------------------------------------------------------- Hilbert transformer

/// FIR Hilbert transformer (Blackman window). `push` gives the input delayed by
/// [`Hilbert::delay`] samples and its Hilbert transform at that same instant.
pub struct Hilbert {
    m: usize,
    /// Coefficients of the odd offsets 1, 3, 5, ... (the even ones are zero).
    coef: Vec<f32>,
    /// Twice the filter length, each sample written twice, so the newest `2m+1`
    /// samples are always one slice.
    buf: Vec<f32>,
    pos: usize,
}

impl Hilbert {
    /// About 11.6 ms each side: clean above roughly 120 Hz.
    pub fn for_rate(fs: f64) -> Self {
        Self::new(((fs * 0.0116) as usize).clamp(31, 2047))
    }

    pub fn new(m: usize) -> Self {
        let coef = (1..=m)
            .step_by(2)
            .map(|k| {
                let x = std::f64::consts::PI * k as f64 / (m + 1) as f64;
                let w = 0.42 + 0.5 * x.cos() + 0.08 * (2.0 * x).cos();
                (2.0 / (std::f64::consts::PI * k as f64) * w) as f32
            })
            .collect();
        Self {
            m,
            coef,
            buf: vec![0.0; 2 * (2 * m + 1)],
            pos: 0,
        }
    }

    pub fn delay(&self) -> usize {
        self.m
    }

    pub fn push(&mut self, x: f32) -> (f32, f32) {
        let n = 2 * self.m + 1;
        self.buf[self.pos] = x;
        self.buf[self.pos + n] = x;
        self.pos = (self.pos + 1) % n;
        // Oldest first; the centre (index m) is the output instant.
        let w = &self.buf[self.pos..self.pos + n];
        let m = self.m;
        let mut acc = 0f32;
        for (i, c) in self.coef.iter().enumerate() {
            let k = 2 * i + 1;
            acc += c * (w[m - k] - w[m + k]);
        }
        (w[m], acc)
    }
}

// ---------------------------------------------------------------- delay line

struct DelayLine {
    buf: Vec<C32>,
    mask: usize,
    pos: usize,
}

impl DelayLine {
    fn new(max_delay: usize) -> Self {
        let len = (max_delay + 8).next_power_of_two();
        Self {
            buf: vec![C32::ZERO; len],
            mask: len - 1,
            pos: 0,
        }
    }
    fn push(&mut self, z: C32) {
        self.pos = (self.pos + 1) & self.mask;
        self.buf[self.pos] = z;
    }
    /// The signal `delay` samples ago (`delay >= 1`), by 4-point Lagrange interpolation.
    fn read(&self, delay: f32) -> C32 {
        let i = delay as usize;
        let u = delay - i as f32;
        let at = |d: usize| self.buf[(self.pos + self.buf.len() - d) & self.mask];
        let (ym, y0, y1, y2) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let cm = -u * (u - 1.0) * (u - 2.0) / 6.0;
        let c0 = (u + 1.0) * (u - 1.0) * (u - 2.0) / 2.0;
        let c1 = -(u + 1.0) * u * (u - 2.0) / 2.0;
        let c2 = (u + 1.0) * u * (u - 1.0) / 6.0;
        ym * cm + y0 * c0 + y1 * c1 + y2 * c2
    }
}

/// Every path sits this many samples back so the interpolator has a sample each side.
const BASE_DELAY: usize = 2;

// ---------------------------------------------------------------- the channel

struct PathState {
    fader: Fader,
    phase: f64,
    /// Smoothed so a slider move does not click.
    amp: f32,
    delay: f32,
    /// 0 = steady path, 1 = fading path.
    fade_mix: f32,
    started: bool,
}

/// One path's present gain, for the response display.
#[derive(Clone, Copy, Debug)]
pub struct Tap {
    pub delay_s: f32,
    pub gain: C32,
}

pub struct Channel {
    fs: f64,
    mode: Mode,
    channels: usize,
    seed: u64,
    /// Mean square of one channel of the input, for the noise level.
    sig_power: f32,
    hil_in: [Hilbert; 2],
    hil_out: Hilbert,
    /// The real part waiting for the output Hilbert transformer (ISB).
    re_wait: Vec<f32>,
    re_pos: usize,
    lines: [DelayLine; 2],
    paths: Vec<PathState>,
    noise: Rng,
    norm: f32,
    pub taps: Vec<Tap>,
    /// Output samples beyond full scale so far.
    pub clipped: u64,
}

impl Channel {
    /// `channels` is 1 or 2; [`Mode::Iq`] and [`Mode::Isb`] need 2.
    pub fn new(fs: u32, channels: usize, mode: Mode, seed: u64, sig_power: f32) -> Self {
        let fs = f64::from(fs);
        let mode = if channels < 2 { Mode::Audio } else { mode };
        let max_delay = (f64::from(MAX_DELAY_MS) * fs / 1000.0) as usize + BASE_DELAY + 4;
        let hil_out = Hilbert::for_rate(fs);
        Self {
            fs,
            mode,
            channels,
            seed,
            sig_power,
            hil_in: [Hilbert::for_rate(fs), Hilbert::for_rate(fs)],
            re_wait: vec![0.0; hil_out.delay().max(1)],
            re_pos: 0,
            hil_out,
            lines: [DelayLine::new(max_delay), DelayLine::new(max_delay)],
            paths: Vec::new(),
            noise: Rng::new(seed ^ 0x006E_6F69_7365),
            norm: 1.0,
            taps: Vec::new(),
            clipped: 0,
        }
    }

    /// Samples by which the output lags the input (a path of 0 ms).
    pub fn latency(&self) -> usize {
        BASE_DELAY
            + match self.mode {
                Mode::Audio => self.hil_in[0].delay(),
                Mode::Iq => 0,
                Mode::Isb => self.hil_in[0].delay() + self.hil_out.delay(),
            }
    }

    /// `input` and `output` are interleaved frames of the file's channel count.
    pub fn process(&mut self, p: &Params, input: &[f32], output: &mut [f32]) {
        let ch = self.channels;
        debug_assert_eq!(input.len(), output.len());
        if p.bypass {
            output.copy_from_slice(input);
            return;
        }
        while self.paths.len() < p.paths.len() {
            let n = self.paths.len() as u64;
            self.paths.push(PathState {
                fader: Fader::new(self.seed.wrapping_add(n.wrapping_mul(0x51_7C_C1_B7_27_22_0A_95))),
                phase: 0.0,
                amp: 0.0,
                delay: 0.0,
                fade_mix: 0.0,
                started: false,
            });
        }
        self.paths.truncate(p.paths.len());

        let fs = self.fs;
        // Block-rate targets; the per-sample values glide to them in about 30 ms.
        let glide = 1.0 - (-1.0 / (0.03 * fs)).exp() as f32;
        let targets: Vec<(f32, f32, f32)> = p
            .paths
            .iter()
            .map(|q| {
                let amp = if q.on { 10f32.powf(q.gain_db / 20.0) } else { 0.0 };
                let delay =
                    q.delay_ms.clamp(0.0, MAX_DELAY_MS) * fs as f32 / 1000.0 + BASE_DELAY as f32;
                (amp, delay, if q.spread_hz > 0.0 { 1.0 } else { 0.0 })
            })
            .collect();
        for (s, t) in self.paths.iter_mut().zip(&targets) {
            if !s.started {
                (s.amp, s.delay, s.fade_mix) = *t;
                s.started = true;
            }
        }
        let total: f32 = targets.iter().map(|t| t.0 * t.0).sum();
        let norm_target = if p.keep_level && total > 1e-12 {
            1.0 / total.sqrt()
        } else {
            1.0
        };
        let out_gain = 10f32.powf(p.out_gain_db / 20.0);
        // White noise whose power in 3 kHz is the signal power over the SNR.
        let snr = 10f32.powf(p.snr_db / 10.0);
        let noise_bw = match self.mode {
            Mode::Iq => fs as f32, // complex: both components carry sig_power each
            _ => fs as f32 / 2.0,
        };
        let noise_amp = if p.noise {
            (self.sig_power * noise_bw / (NOISE_BW_HZ * snr)).sqrt() * std::f32::consts::SQRT_2
        } else {
            0.0
        };

        let lines = if self.mode == Mode::Audio { ch } else { 1 };
        for (fin, fout) in input.chunks_exact(ch).zip(output.chunks_exact_mut(ch)) {
            match self.mode {
                Mode::Audio => {
                    for (c, &x) in fin.iter().enumerate() {
                        let (re, im) = self.hil_in[c].push(x);
                        self.lines[c].push(C32::new(re, im));
                    }
                }
                Mode::Iq => self.lines[0].push(C32::new(fin[0], fin[1])),
                Mode::Isb => {
                    // Upper sideband above the carrier, lower sideband mirrored below.
                    let (lr, li) = self.hil_in[0].push(fin[0]);
                    let (ur, ui) = self.hil_in[1].push(fin[1]);
                    self.lines[0].push(C32::new(ur + lr, ui - li));
                }
            }
            self.norm += glide * (norm_target - self.norm);
            let mut y = [C32::ZERO; 2];
            for ((s, q), t) in self.paths.iter_mut().zip(&p.paths).zip(&targets) {
                s.amp += glide * (t.0 - s.amp);
                s.delay += glide * (t.1 - s.delay);
                s.fade_mix += glide * (t.2 - s.fade_mix);
                let fade = s.fader.step(q.spread_hz, fs);
                s.phase = (s.phase + TAU * f64::from(q.shift_hz) / fs) % TAU;
                let rot = C32::new(s.phase.cos() as f32, s.phase.sin() as f32);
                let g = (C32::new(1.0 - s.fade_mix, 0.0) + fade * s.fade_mix) * rot * s.amp;
                for (yc, line) in y.iter_mut().zip(&self.lines).take(lines) {
                    *yc += g * line.read(s.delay);
                }
            }
            let k = self.norm * out_gain;
            match self.mode {
                Mode::Audio => {
                    for c in 0..ch {
                        fout[c] = (y[c].re * self.norm + self.noise.gauss().re * noise_amp) * out_gain;
                    }
                }
                Mode::Iq => {
                    let n = self.noise.gauss() * noise_amp;
                    fout[0] = (y[0].re * self.norm + n.re) * out_gain;
                    fout[1] = (y[0].im * self.norm + n.im) * out_gain;
                }
                Mode::Isb => {
                    // Split again: the part above the carrier is (y + jH{y}) / 2.
                    let (_, him) = self.hil_out.push(y[0].im);
                    let re = std::mem::replace(&mut self.re_wait[self.re_pos], y[0].re);
                    self.re_pos = (self.re_pos + 1) % self.re_wait.len();
                    let n = self.noise.gauss() * noise_amp;
                    fout[0] = (re + him) * 0.5 * k + n.re * out_gain;
                    fout[1] = (re - him) * 0.5 * k + n.im * out_gain;
                }
            }
            for v in fout.iter() {
                if v.abs() > 1.0 {
                    self.clipped += 1;
                }
            }
        }

        self.taps.clear();
        for (s, q) in self.paths.iter().zip(&p.paths) {
            if s.amp > 1e-4 {
                let rot = C32::new(s.phase.cos() as f32, s.phase.sin() as f32);
                let fade = s.fader.pts[1];
                self.taps.push(Tap {
                    delay_s: q.delay_ms / 1000.0,
                    gain: (C32::new(1.0 - s.fade_mix, 0.0) + fade * s.fade_mix)
                        * rot
                        * (s.amp * self.norm),
                });
            }
        }
    }
}

/// Run a whole file through the channel, lined up with the input (the channel's own
/// delay taken out). `progress` is called with 0..=1 now and then; return false to stop.
pub fn render(
    samples: &[f32],
    fs: u32,
    channels: usize,
    mode: Mode,
    params: &Params,
    seed: u64,
    mut progress: impl FnMut(f32) -> bool,
) -> Option<(Vec<f32>, u64)> {
    let mut chan = Channel::new(fs, channels, mode, seed, mean_square(samples));
    let skip = if params.bypass { 0 } else { chan.latency() * channels };
    let mut input = samples.to_vec();
    input.resize(samples.len() + skip, 0.0);
    let mut out = vec![0f32; input.len()];
    let block = 4096 * channels;
    let blocks = input.len().div_ceil(block).max(1);
    for (i, (a, b)) in input.chunks(block).zip(out.chunks_mut(block)).enumerate() {
        chan.process(params, a, b);
        if i % 16 == 0 && !progress(i as f32 / blocks as f32) {
            return None;
        }
    }
    out.drain(..skip);
    Some((out, chan.clipped))
}

pub fn mean_square(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / samples.len() as f64)
        as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs: f32, hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / fs).sin())
            .collect()
    }

    fn rms_err(a: &[f32], b: &[f32]) -> f32 {
        (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.len() as f32).sqrt()
    }

    fn plain() -> Params {
        Params {
            out_gain_db: 0.0,
            ..Params::default()
        }
    }

    #[test]
    fn fader_has_unit_power() {
        let mut f = Fader::new(7);
        let n = 400_000;
        let p: f64 = (0..n)
            .map(|_| f64::from(f.step(50.0, 8000.0).norm_sqr()))
            .sum::<f64>()
            / n as f64;
        assert!((p - 1.0).abs() < 0.15, "power {p}");
    }

    #[test]
    fn one_clean_path_is_transparent() {
        let x = tone(12000.0, 1000.0, 12000);
        let (y, _) = render(&x, 12000, 1, Mode::Audio, &plain(), 1, |_| true).unwrap();
        assert_eq!(y.len(), x.len());
        assert!(rms_err(&x[2000..10000], &y[2000..10000]) < 0.003);
    }

    #[test]
    fn isb_keeps_the_sidebands_apart() {
        let l = tone(12000.0, 700.0, 12000);
        let r = tone(12000.0, 1900.0, 12000);
        let x: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
        let (y, _) = render(&x, 12000, 2, Mode::Isb, &plain(), 1, |_| true).unwrap();
        assert!(rms_err(&x[6000..18000], &y[6000..18000]) < 0.005);
    }

    #[test]
    fn doppler_shift_moves_a_tone() {
        let fs = 12000.0;
        let x = tone(fs, 1000.0, 24000);
        let mut p = plain();
        p.paths[0].shift_hz = 10.0;
        let (y, _) = render(&x, 12000, 1, Mode::Audio, &p, 1, |_| true).unwrap();
        let want = tone(fs, 1010.0, 24000);
        // Same frequency means a constant phase offset: correlate against both phases.
        let seg = 4000..20000;
        let (mut i, mut q, mut e) = (0f32, 0f32, 0f32);
        for n in seg {
            let c = 0.5 * (std::f32::consts::TAU * 1010.0 * n as f32 / fs).cos();
            i += y[n] * want[n];
            q += y[n] * c;
            e += y[n] * y[n];
        }
        // All of the output's energy is at 1010 Hz.
        let at = (i * i + q * q).sqrt() / 0.125 / 16000.0;
        assert!((at * at * 0.125 * 16000.0 / e - 1.0).abs() < 0.02, "{at} {e}");
    }

    #[test]
    fn two_paths_make_a_notch() {
        // Equal paths 1 ms apart cancel at 500 Hz and add at 1000 Hz.
        let mut p = plain();
        p.keep_level = false;
        p.paths.push(Path::new(1.0, 0.0, 0.0, 0.0));
        let level = |hz: f32| {
            let x = tone(12000.0, hz, 12000);
            let (y, _) = render(&x, 12000, 1, Mode::Audio, &p, 1, |_| true).unwrap();
            mean_square(&y[4000..10000]).sqrt() / mean_square(&x[4000..10000]).sqrt()
        };
        assert!(level(500.0) < 0.02, "{}", level(500.0));
        assert!((level(1000.0) - 2.0).abs() < 0.05, "{}", level(1000.0));
    }
}
