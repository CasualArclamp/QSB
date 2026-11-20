//! WAV in and out (hound). The file is held in memory as interleaved f32, one or two
//! channels; the output is written in the input's sample format.

use std::path::Path;

pub struct Audio {
    pub fs: u32,
    /// 1 or 2.
    pub channels: usize,
    pub bits: u16,
    pub float: bool,
    /// Interleaved, full scale = 1.
    pub samples: Vec<f32>,
    /// Mean square of the samples, for the noise level.
    pub power: f32,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.fs)
    }
}

pub fn load(path: &Path) -> Result<Audio, String> {
    let mut r = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = r.spec();
    let file_ch = spec.channels as usize;
    if file_ch == 0 {
        return Err("no channels".into());
    }
    let all: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    // More than two channels: keep the first two.
    let channels = file_ch.min(2);
    let samples: Vec<f32> = if file_ch > 2 {
        all.chunks_exact(file_ch)
            .flat_map(|f| [f[0], f[1]])
            .collect()
    } else {
        all
    };
    if samples.len() < channels {
        return Err("the file has no audio in it".into());
    }
    Ok(Audio {
        fs: spec.sample_rate,
        channels,
        bits: spec.bits_per_sample,
        float: spec.sample_format == hound::SampleFormat::Float,
        power: crate::channel::mean_square(&samples),
        samples,
    })
}

/// Write `samples` in the format `like` was read in. Clips at full scale.
pub fn save(path: &Path, like: &Audio, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: like.channels as u16,
        sample_rate: like.fs,
        bits_per_sample: like.bits,
        sample_format: if like.float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    if like.float {
        for &v in samples {
            w.write_sample(v).map_err(|e| e.to_string())?;
        }
    } else {
        let full = (1i64 << (like.bits - 1)) as f32;
        for &v in samples {
            let s = (v * full).round().clamp(-full, full - 1.0) as i32;
            w.write_sample(s).map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())
}
