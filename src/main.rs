#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Multipath: put a WAV file through a simulated HF channel (echoes, Doppler shift,
//! fading, noise), listen to it while moving the sliders, and write the result.
//!
//! `multipath [FILE.wav] [--preset NAME] [--mode audio|iq|isb] [--seed N]`
//! `          [--play] [--mute] [--screenshot OUT.png --after SECS]`
//! `multipath --render IN.wav OUT.wav [--preset NAME] [--mode M] [--seed N] [--snr DB]`
//! `multipath --spectrogram IN.wav OUT.png [--mode M]`

mod channel;
mod player;
mod presets;
mod spectro;
mod wav;

use channel::{MAX_DELAY_MS, Mode, Params, Path as ChanPath};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Frame, Margin, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use num_complex::Complex32 as C32;
use player::{Output, Shared, Source};
use spectro::Spectrogram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wav::Audio;

// DecSAT's colours (from DecHAMDRM's `theme.rs`), so the apps look alike.
const BG: Color32 = Color32::from_rgb(17, 17, 18);
const PANEL: Color32 = Color32::from_rgb(27, 27, 28);
const WELL: Color32 = Color32::from_rgb(10, 11, 17);
const GRID: Color32 = Color32::from_rgb(38, 38, 46);
const DIM: Color32 = Color32::from_rgb(140, 140, 144);
const WARN: Color32 = Color32::from_rgb(240, 200, 80);
const BAD: Color32 = Color32::from_rgb(230, 110, 110);
const TRACE: Color32 = Color32::from_rgb(150, 205, 255);

/// Spectrogram columns kept (about 23 s at the usual 23 ms a column).
const COLS: usize = 1000;

struct Shot {
    path: PathBuf,
    after: f64,
    requested: Option<Instant>,
}

struct RenderJob {
    /// Thousandths done.
    progress: Arc<AtomicU32>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
}

struct App {
    shared: Arc<Shared>,
    output: Option<Output>,
    audio: Option<Arc<Audio>>,
    file: Option<PathBuf>,
    mode: Mode,
    seed: u64,
    params: Params,
    preset: Option<usize>,
    presets: Vec<presets::Preset>,
    volume: f32,
    max_hz: f32,
    spectro: Option<Spectrogram>,
    drawn: u64,
    tex: Vec<TextureHandle>,
    palette: Vec<[u8; 3]>,
    job: Option<RenderJob>,
    status: String,
    status_bad: bool,
    shot: Option<Shot>,
    started: Instant,
}

impl App {
    fn open(&mut self, path: &Path) {
        match wav::load(path) {
            Ok(a) => {
                if a.channels < 2 {
                    self.mode = Mode::Audio;
                }
                self.say(format!(
                    "{}: {} Hz, {}, {}-bit{}, {:.1} s",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    a.fs,
                    if a.channels == 2 { "stereo" } else { "mono" },
                    a.bits,
                    if a.float { " float" } else { "" },
                    a.seconds()
                ));
                self.audio = Some(Arc::new(a));
                self.file = Some(path.to_path_buf());
                self.restart();
            }
            Err(e) => self.fail(format!("cannot open {}: {e}", path.display())),
        }
    }

    /// New file, mode or fading pattern: start the channel afresh.
    fn restart(&mut self) {
        let Some(audio) = self.audio.clone() else { return };
        self.spectro = Some(Spectrogram::new(audio.fs, audio.channels, self.mode, COLS));
        self.drawn = u64::MAX;
        self.shared.set_source(Some(Source {
            audio,
            mode: self.mode,
            seed: self.seed,
        }));
    }

    fn say(&mut self, s: String) {
        self.status = s;
        self.status_bad = false;
    }
    fn fail(&mut self, s: String) {
        self.status = s;
        self.status_bad = true;
    }

    fn set_playing(&mut self, on: bool) {
        if on && self.output.is_none() {
            match Output::start(self.shared.clone()) {
                Ok(o) => self.output = Some(o),
                Err(e) => return self.fail(format!("sound card: {e}")),
            }
        }
        self.shared.playing.store(on, Ordering::Relaxed);
    }

    fn start_render(&mut self) {
        let (Some(audio), Some(file)) = (self.audio.clone(), self.file.clone()) else {
            return;
        };
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .set_file_name(format!("{stem}_multipath.wav"));
        if let Some(dir) = file.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(out) = dialog.save_file() else { return };
        if out == file {
            return self.fail("not writing over the input file: pick another name".into());
        }
        let job = RenderJob {
            progress: Arc::new(AtomicU32::new(0)),
            result: Arc::new(Mutex::new(None)),
        };
        let (progress, result) = (job.progress.clone(), job.result.clone());
        let (params, mode, seed) = (self.params.clone(), self.mode, self.seed);
        std::thread::spawn(move || {
            let r = render_to(&audio, mode, &params, seed, &out, |f| {
                progress.store((f * 1000.0) as u32, Ordering::Relaxed);
            });
            *result.lock().unwrap() = Some(r);
        });
        self.job = Some(job);
    }

    // ---------------------------------------------------------------- top bar
    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Multipath").strong().color(Color32::WHITE).size(16.0));
            ui.add_space(8.0);
            if ui.button("Open WAV…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("WAV", &["wav"])
                    .pick_file()
            {
                self.open(&p);
            }
            let Some(audio) = self.audio.clone() else {
                ui.label(RichText::new("open a WAV file, or drop one here").color(DIM));
                return;
            };
            ui.separator();
            ui.label("Treat as");
            let before = self.mode;
            ui.add_enabled_ui(audio.channels == 2, |ui| {
                egui::ComboBox::from_id_salt("mode")
                    .selected_text(self.mode.label())
                    .show_ui(ui, |ui| {
                        for m in Mode::ALL {
                            ui.selectable_value(&mut self.mode, m, m.label());
                        }
                    });
            });
            if self.mode != before {
                self.restart();
            }
            ui.separator();
            let playing = self.shared.playing.load(Ordering::Relaxed);
            if ui
                .add_sized([54.0, 20.0], egui::Button::new(if playing { "Stop" } else { "Play" }))
                .clicked()
            {
                self.set_playing(!playing);
            }
            let mut looping = self.shared.looping.load(Ordering::Relaxed);
            if ui.checkbox(&mut looping, "Loop").changed() {
                self.shared.looping.store(looping, Ordering::Relaxed);
            }
            ui.checkbox(&mut self.params.bypass, "Bypass")
                .on_hover_text("Hear and see the file as it is");
            ui.label("Vol");
            ui.spacing_mut().slider_width = 70.0;
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false))
                .changed()
            {
                self.shared.volume.store(self.volume.to_bits(), Ordering::Relaxed);
            }
            ui.separator();
            match &self.job {
                Some(j) => {
                    let done = j.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                    ui.add(
                        egui::ProgressBar::new(done)
                            .desired_width(140.0)
                            .text("rendering"),
                    );
                }
                None => {
                    if ui
                        .button("Render to WAV…")
                        .on_hover_text("Write the whole file through this channel")
                        .clicked()
                    {
                        self.start_render();
                    }
                }
            }
        });
        if let Some(audio) = self.audio.clone() {
            ui.horizontal(|ui| {
                let fs = audio.fs as f32;
                let mut at = self.shared.pos.load(Ordering::Relaxed) as f32 / fs;
                let len = audio.seconds() as f32;
                ui.label(RichText::new(format!("{} / {}", clock(at), clock(len))).monospace());
                ui.spacing_mut().slider_width = (ui.available_width() - 16.0).max(100.0);
                if ui
                    .add(egui::Slider::new(&mut at, 0.0..=len).show_value(false))
                    .changed()
                {
                    self.shared.seek.store((at * fs) as i64, Ordering::Relaxed);
                    if !self.shared.playing.load(Ordering::Relaxed) {
                        self.shared.pos.store((at * fs) as usize, Ordering::Relaxed);
                    }
                }
            });
        }
        let col = if self.status_bad { BAD } else { DIM };
        ui.label(RichText::new(&self.status).color(col).small());
    }

    // ---------------------------------------------------------------- controls
    fn controls(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Channel").strong());
        let shown = self
            .preset
            .map_or("Custom", |i| self.presets[i].name)
            .to_string();
        let mut pick = None;
        egui::ComboBox::from_id_salt("preset")
            .width(300.0)
            .height(500.0)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for (i, p) in self.presets.iter().enumerate() {
                    if ui
                        .selectable_label(self.preset == Some(i), p.name)
                        .on_hover_text(p.note)
                        .clicked()
                    {
                        pick = Some(i);
                    }
                }
            });
        if let Some(i) = pick {
            self.preset = Some(i);
            self.params.paths = self.presets[i].paths.clone();
        }
        if let Some(i) = self.preset {
            ui.label(RichText::new(self.presets[i].note).color(DIM).small());
        }
        ui.add_space(6.0);

        let before = self.params.paths.clone();
        let mut remove = None;
        egui::Grid::new("paths")
            .num_columns(6)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                for h in ["", "Delay", "Gain", "Shift", "Spread", ""] {
                    ui.label(RichText::new(h).color(DIM).small());
                }
                ui.end_row();
                for (i, p) in self.params.paths.iter_mut().enumerate() {
                    ui.checkbox(&mut p.on, format!("{}", i + 1));
                    ui.add(
                        egui::DragValue::new(&mut p.delay_ms)
                            .speed(0.02)
                            .range(0.0..=MAX_DELAY_MS)
                            .fixed_decimals(2)
                            .suffix(" ms"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut p.gain_db)
                            .speed(0.1)
                            .range(-40.0..=6.0)
                            .fixed_decimals(1)
                            .suffix(" dB"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut p.shift_hz)
                            .speed(0.01)
                            .range(-100.0..=100.0)
                            .fixed_decimals(2)
                            .suffix(" Hz"),
                    )
                    .on_hover_text("Doppler shift: moves the whole path in frequency");
                    ui.add(
                        egui::DragValue::new(&mut p.spread_hz)
                            .speed(0.01)
                            .range(0.0..=100.0)
                            .fixed_decimals(2)
                            .suffix(" Hz"),
                    )
                    .on_hover_text("Doppler spread: how fast the path fades. 0 = steady");
                    if ui.small_button("x").on_hover_text("Remove this path").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove
            && self.params.paths.len() > 1
        {
            self.params.paths.remove(i);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.params.paths.len() < 8, egui::Button::new("Add path"))
                .clicked()
            {
                let last = self.params.paths.last().map_or(0.0, |p| p.delay_ms);
                self.params
                    .paths
                    .push(ChanPath::new((last + 1.0).min(MAX_DELAY_MS), -3.0, 0.0, 0.5));
            }
            if ui
                .button("New fading pattern")
                .on_hover_text("Another random run of the same channel")
                .clicked()
            {
                self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                self.restart();
            }
        });
        if self.params.paths != before {
            self.preset = None;
        }
        ui.label(
            RichText::new("Drag a number to change it, or double-click to type.")
                .color(DIM)
                .small(),
        );

        ui.add_space(10.0);
        ui.label(RichText::new("Noise").strong());
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.params.noise, "Add noise");
            ui.spacing_mut().slider_width = 150.0;
            ui.add_enabled(
                self.params.noise,
                egui::Slider::new(&mut self.params.snr_db, -10.0..=50.0)
                    .fixed_decimals(0)
                    .suffix(" dB SNR"),
            )
            .on_hover_text("Signal (average of the file) to white noise in 3 kHz");
        });

        ui.add_space(10.0);
        ui.label(RichText::new("Output").strong());
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = 150.0;
            ui.add(
                egui::Slider::new(&mut self.params.out_gain_db, -30.0..=12.0)
                    .fixed_decimals(1)
                    .suffix(" dB"),
            );
            ui.label("level");
        });
        ui.checkbox(&mut self.params.keep_level, "Keep the average level")
            .on_hover_text("Scale by the paths' total power, so more paths are not louder");
        let clipped = self.shared.clipped.load(Ordering::Relaxed);
        if clipped > 0 {
            ui.label(
                RichText::new(format!("{clipped} samples over full scale: turn the level down"))
                    .color(WARN),
            );
        }
    }

    // ---------------------------------------------------------------- pictures
    fn update_spectrogram(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.spectro else { return };
        let fresh: Vec<f32> = self.shared.scope.lock().unwrap().drain(..).collect();
        s.feed(&fresh);
        if s.count == self.drawn {
            return;
        }
        self.drawn = s.count;
        let rows = if s.is_complex() {
            s.bins()
        } else {
            ((self.max_hz / s.bin_hz()) as usize).clamp(8, s.bins())
        };
        let top = s
            .cols
            .iter()
            .flatten()
            .flat_map(|c| c[..rows].iter().copied())
            .fold(-60.0f32, f32::max);
        for (pane, cols) in s.cols.iter().enumerate() {
            let mut px = vec![Color32::from_rgb(0, 0, 4); COLS * rows];
            let x0 = COLS - cols.len();
            for (i, col) in cols.iter().enumerate() {
                for (b, &db) in col[..rows].iter().enumerate() {
                    let t = ((db - (top - 65.0)) / 65.0).clamp(0.0, 1.0);
                    let [r, g, bl] = self.palette[(t * 255.0) as usize];
                    px[(rows - 1 - b) * COLS + x0 + i] = Color32::from_rgb(r, g, bl);
                }
            }
            let img = ColorImage::new([COLS, rows], px);
            match self.tex.get_mut(pane) {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self
                    .tex
                    .push(ctx.load_texture(format!("spec{pane}"), img, TextureOptions::LINEAR)),
            }
        }
    }

    fn pictures(&mut self, ui: &mut Ui) {
        let Some(s) = &self.spectro else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Open a WAV file, or drop one on the window").color(DIM));
            });
            return;
        };
        let panes = s.panes();
        let complex = s.is_complex();
        let nyquist = s.bin_hz() * s.n as f32 / 2.0;
        let span_s = COLS as f32 * s.seconds_per_col();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Spectrogram, last {span_s:.0} s")).color(DIM));
            if !complex {
                ui.label(RichText::new("   show up to").color(DIM));
                for hz in [3000.0, 5500.0, 12000.0, 24000.0] {
                    if hz < nyquist * 1.01 || hz == 3000.0 {
                        let label = format!("{} kHz", hz / 1000.0);
                        if ui.selectable_label(self.max_hz == hz, label).clicked() {
                            self.max_hz = hz;
                            self.drawn = u64::MAX;
                        }
                    }
                }
            }
        });
        let top_hz = self.max_hz.min(nyquist);
        let resp_h = 130.0;
        let avail = ui.available_size();
        let pane_h = ((avail.y - resp_h - 26.0 - 4.0 * panes as f32) / panes as f32).max(60.0);
        let names: &[&str] = match (self.mode, panes) {
            (Mode::Isb, 2) => &["L: lower sideband", "R: upper sideband"],
            (_, 2) => &["L", "R"],
            _ if complex => &["I/Q"],
            _ => &[""],
        };
        for pane in 0..panes {
            let (rect, _) = ui.allocate_exact_size(vec2(avail.x, pane_h), Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 2.0, WELL);
            if let Some(t) = self.tex.get(pane) {
                p.image(
                    t.id(),
                    rect,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            // Frequency marks down the left edge.
            let (lo, hi) = if complex { (-nyquist, nyquist) } else { (0.0, top_hz) };
            let step = nice_step((hi - lo) / 6.0);
            let mut f = (lo / step).ceil() * step;
            while f <= hi {
                let y = rect.bottom() - (f - lo) / (hi - lo) * rect.height();
                p.line_segment(
                    [pos2(rect.left(), y), pos2(rect.left() + 5.0, y)],
                    Stroke::new(1.0, Color32::WHITE),
                );
                if y > rect.top() + 10.0 && y < rect.bottom() - 4.0 {
                    p.text(
                        pos2(rect.left() + 8.0, y),
                        Align2::LEFT_CENTER,
                        format!("{f:.0}"),
                        FontId::monospace(10.0),
                        Color32::WHITE,
                    );
                }
                f += step;
            }
            p.text(
                rect.right_top() + vec2(-6.0, 4.0),
                Align2::RIGHT_TOP,
                names[pane.min(names.len() - 1)],
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            ui.add_space(4.0);
        }
        self.response(ui, resp_h, top_hz, nyquist, complex);
    }

    /// The channel's frequency response at this instant: |sum of g e^(-j 2 pi f tau)|.
    fn response(&self, ui: &mut Ui, height: f32, top_hz: f32, nyquist: f32, complex: bool) {
        ui.label(RichText::new("Channel response now (dB)").color(DIM).small());
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height - 20.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, WELL);
        let (lo, hi) = match self.mode {
            _ if complex => (-nyquist, nyquist),
            Mode::Isb => (-top_hz, top_hz),
            _ => (0.0, top_hz),
        };
        let (db_lo, db_hi) = (-40.0f32, 12.0f32);
        let y_of = |db: f32| rect.bottom() - (db - db_lo) / (db_hi - db_lo) * rect.height();
        for db in [-30.0, -20.0, -10.0, 0.0, 10.0] {
            let col = if db == 0.0 { Color32::from_rgb(70, 70, 82) } else { GRID };
            p.line_segment(
                [pos2(rect.left(), y_of(db)), pos2(rect.right(), y_of(db))],
                Stroke::new(1.0, col),
            );
            p.text(
                pos2(rect.left() + 3.0, y_of(db) - 1.0),
                Align2::LEFT_BOTTOM,
                format!("{db:.0}"),
                FontId::monospace(9.0),
                DIM,
            );
        }
        let step = nice_step((hi - lo) / 8.0);
        let mut f = (lo / step).ceil() * step;
        while f <= hi {
            let x = rect.left() + (f - lo) / (hi - lo) * rect.width();
            p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, GRID));
            p.text(
                pos2(x + 2.0, rect.bottom() - 1.0),
                Align2::LEFT_BOTTOM,
                format!("{f:.0} Hz"),
                FontId::monospace(9.0),
                DIM,
            );
            f += step;
        }
        let taps = self.shared.taps.lock().unwrap().clone();
        if taps.is_empty() {
            return;
        }
        let n = (rect.width() as usize / 2).max(2);
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let u = i as f32 / n as f32;
                let f = lo + (hi - lo) * u;
                let h: C32 = taps
                    .iter()
                    .map(|t| t.gain * C32::cis(-std::f32::consts::TAU * f * t.delay_s))
                    .sum();
                let db = 20.0 * h.norm().max(1e-4).log10();
                pos2(rect.left() + u * rect.width(), y_of(db.clamp(db_lo, db_hi)))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, TRACE)));
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::save_buffer(
                &shot.path,
                &rgba,
                image.size[0] as u32,
                image.size[1] as u32,
                image::ColorType::Rgba8,
            ) {
                Ok(()) => eprintln!("screenshot saved to {}", shot.path.display()),
                Err(e) => eprintln!("cannot save {}: {e}", shot.path.display()),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if shot.requested.is_none() && self.started.elapsed().as_secs_f64() >= shot.after {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            shot.requested = Some(Instant::now());
        } else if shot.requested.is_some_and(|t| t.elapsed().as_secs() > 3) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let done = self
            .job
            .as_ref()
            .and_then(|j| j.result.lock().unwrap().take());
        if let Some(r) = done {
            self.job = None;
            match r {
                Ok(s) => self.say(s),
                Err(e) => self.fail(e),
            }
        }
        self.update_spectrogram(ctx);
        self.screenshot(ctx);
        if self.shared.playing.load(Ordering::Relaxed) || self.job.is_some() || self.shot.is_some() {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let dropped: Option<PathBuf> =
            ui.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped {
            self.open(&p);
        }
        let plain = |fill: Color32, m: i8| Frame::new().fill(fill).inner_margin(Margin::same(m));
        egui::Panel::top("top")
            .frame(plain(PANEL, 6))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("controls")
            .frame(plain(PANEL, 8))
            .resizable(false)
            .default_size(400.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::default()
            .frame(plain(BG, 6))
            .show(ui, |ui| self.pictures(ui));
        // The sound card reads the sliders from here.
        let mut shared = self.shared.params.lock().unwrap();
        if *shared != self.params {
            *shared = self.params.clone();
        }
    }
}

fn clock(s: f32) -> String {
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// 1, 2 or 5 times a power of ten, at least `raw`.
fn nice_step(raw: f32) -> f32 {
    let mag = 10f32.powf(raw.max(1e-3).log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|k| k * mag)
        .find(|&s| s >= raw)
        .unwrap_or(10.0 * mag)
}

fn render_to(
    audio: &Audio,
    mode: Mode,
    params: &Params,
    seed: u64,
    out: &Path,
    mut progress: impl FnMut(f32),
) -> Result<String, String> {
    let (samples, clipped) = channel::render(
        &audio.samples,
        audio.fs,
        audio.channels,
        mode,
        params,
        seed,
        |f| {
            progress(f);
            true
        },
    )
    .ok_or("stopped")?;
    wav::save(out, audio, &samples)?;
    let mut msg = format!("wrote {}", out.display());
    if clipped > 0 && !audio.float {
        msg += &format!(" ({clipped} samples clipped: turn the level down)");
    }
    Ok(msg)
}

// ---------------------------------------------------------------- command line

struct Args {
    file: Option<PathBuf>,
    preset: Option<String>,
    mode: Option<Mode>,
    seed: u64,
    snr: Option<f32>,
    play: bool,
    mute: bool,
    shot: Option<PathBuf>,
    after: f64,
    render: Option<(PathBuf, PathBuf)>,
    spectrogram: Option<(PathBuf, PathBuf)>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        file: None,
        preset: None,
        mode: None,
        seed: 1,
        snr: None,
        play: false,
        mute: false,
        shot: None,
        after: 5.0,
        render: None,
        spectrogram: None,
    };
    let mut it = std::env::args().skip(1);
    let need = |it: &mut dyn Iterator<Item = String>, what: &str| {
        it.next().ok_or(format!("{what} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--preset" => a.preset = Some(need(&mut it, "--preset")?),
            "--mode" => {
                let m = need(&mut it, "--mode")?;
                a.mode = Some(
                    Mode::ALL
                        .into_iter()
                        .find(|k| k.key() == m)
                        .ok_or(format!("unknown mode {m}: audio, iq or isb"))?,
                );
            }
            "--seed" => a.seed = need(&mut it, "--seed")?.parse().map_err(|_| "bad --seed")?,
            "--snr" => a.snr = Some(need(&mut it, "--snr")?.parse().map_err(|_| "bad --snr")?),
            "--after" => a.after = need(&mut it, "--after")?.parse().map_err(|_| "bad --after")?,
            "--screenshot" => a.shot = Some(need(&mut it, "--screenshot")?.into()),
            "--play" => a.play = true,
            "--mute" => a.mute = true,
            "--render" => {
                a.render = Some((need(&mut it, "--render")?.into(), need(&mut it, "--render")?.into()));
            }
            "--spectrogram" => {
                a.spectrogram = Some((
                    need(&mut it, "--spectrogram")?.into(),
                    need(&mut it, "--spectrogram")?.into(),
                ));
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => a.file = Some(arg.into()),
        }
    }
    Ok(a)
}

fn headless(a: &Args) -> Option<Result<String, String>> {
    if let Some((input, out)) = &a.render {
        return Some((|| {
            let audio = wav::load(input)?;
            let mut params = Params::default();
            if let Some(name) = &a.preset {
                params.paths = presets::find(name).ok_or(format!("no preset like {name}"))?.paths;
            }
            if let Some(snr) = a.snr {
                params.noise = true;
                params.snr_db = snr;
            }
            let mode = a.mode.unwrap_or(Mode::Audio);
            render_to(&audio, mode, &params, a.seed, out, |_| {})
        })());
    }
    if let Some((input, out)) = &a.spectrogram {
        return Some((|| {
            let audio = wav::load(input)?;
            spectro::save_png(&audio, a.mode.unwrap_or(Mode::Audio), 5500.0, out)?;
            Ok(format!("wrote {}", out.display()))
        })());
    }
    None
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Some(r) = headless(&args) {
        match r {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 820.0])
        .with_min_inner_size([980.0, 560.0])
        .with_title("Multipath")
        .with_drag_and_drop(true);
    // The window's icon (assets/icon.png, drawn by scripts/make-icon.py).
    if let Ok(icon) = image::load_from_memory(include_bytes!("../assets/icon.png")) {
        let icon = icon.to_rgba8();
        viewport = viewport.with_icon(egui::IconData {
            width: icon.width(),
            height: icon.height(),
            rgba: icon.into_raw(),
        });
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Multipath",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let all = presets::all();
            let start = args
                .preset
                .as_ref()
                .and_then(|n| {
                    let n = n.to_lowercase();
                    all.iter().position(|p| p.name.to_lowercase().contains(&n))
                })
                .unwrap_or(3);
            let mut params = Params {
                paths: all[start].paths.clone(),
                ..Params::default()
            };
            if let Some(snr) = args.snr {
                params.noise = true;
                params.snr_db = snr;
            }
            let shared = Shared::new();
            let volume = if args.mute { 0.0 } else { 0.7 };
            shared.volume.store(f32::to_bits(volume), Ordering::Relaxed);
            let mut app = App {
                shared,
                output: None,
                audio: None,
                file: None,
                mode: args.mode.unwrap_or(Mode::Audio),
                seed: args.seed,
                params,
                preset: Some(start),
                presets: all,
                volume,
                max_hz: 5500.0,
                spectro: None,
                drawn: u64::MAX,
                tex: Vec::new(),
                palette: spectro::palette(),
                job: None,
                status: String::new(),
                status_bad: false,
                shot: args.shot.map(|path| Shot {
                    path,
                    after: args.after,
                    requested: None,
                }),
                started: Instant::now(),
            };
            if let Some(f) = &args.file {
                app.open(f);
                if args.play {
                    app.set_playing(true);
                }
            }
            Ok(Box::new(app))
        }),
    )
}
