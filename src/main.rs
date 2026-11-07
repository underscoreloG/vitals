#![windows_subsystem = "windows"]

mod sample;
mod theme;
#[cfg_attr(windows, path = "platform/windows.rs")]
#[cfg_attr(target_os = "linux", path = "platform/linux.rs")]
#[cfg_attr(target_os = "macos", path = "platform/macos.rs")]
mod platform;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Shape, Stroke, StrokeKind, pos2, vec2};
use sample::{Info, Sample, Sampler};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use theme::Tokens;

const HISTORY: usize = 120;
const TICK: Duration = Duration::from_secs(1);
const GAP: f32 = 10.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

fn main() -> eframe::Result {
    #[cfg(windows)]
    if std::env::args().any(|a| a == "--install-pawnio") {
        platform::install_pawnio();
        return Ok(());
    }
    let icon = eframe::icon_data::from_png_bytes(theme::ICON).expect("bundled icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 860.0])
            .with_min_inner_size([320.0, 420.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native("Vitals", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[derive(Default)]
struct State {
    info: Info,
    samples: VecDeque<Sample>,
}

struct App {
    state: Arc<Mutex<State>>,
    logo: egui::TextureHandle,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        let icon = eframe::icon_data::from_png_bytes(theme::ICON).expect("bundled icon is a valid PNG");
        let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        let logo = ctx.load_texture("vitals-icon", image, egui::TextureOptions::LINEAR);

        // Sampling runs on its own thread so history keeps filling while the window is minimized.
        let state = Arc::new(Mutex::new(State::default()));
        let (out, repaint) = (state.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut sampler = Sampler::new();
            out.lock().unwrap().info = sampler.info.clone();
            loop {
                std::thread::sleep(TICK);
                let sample = sampler.sample();
                let mut state = out.lock().unwrap();
                if state.samples.len() == HISTORY {
                    state.samples.pop_front();
                }
                state.samples.push_back(sample);
                drop(state);
                repaint.request_repaint();
            }
        });
        Self { state, logo }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = theme::tokens(ui.visuals().dark_mode);
        let charts = charts(&self.state.lock().unwrap(), t);

        let frame = egui::Frame::new().fill(t.bg).inner_margin(12);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(22.0, 22.0)));
                ui.label(RichText::new("Vitals").size(16.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new(".org").size(13.0).color(theme::ACCENT));
                    ui.label(RichText::new("kvlh").size(13.0).strong());
                });
            });
            ui.add_space(8.0);

            // One column when narrow, two when wide; a short last row stretches to full width.
            let area = ui.available_rect_before_wrap();
            let cols = if area.width() >= 760.0 { 2 } else { 1 };
            let rows = charts.len().div_ceil(cols);
            let h = (area.height() - GAP * (rows - 1) as f32) / rows as f32;
            for (i, chart) in charts.iter().enumerate() {
                let (row, col) = (i / cols, i % cols);
                let in_row = (charts.len() - row * cols).min(cols);
                let w = (area.width() - GAP * (in_row - 1) as f32) / in_row as f32;
                let min = area.min + vec2(col as f32 * (w + GAP), row as f32 * (h + GAP));
                draw(ui, Rect::from_min_size(min, vec2(w, h)), chart, t);
            }
        });
    }
}

struct Series {
    label: String,
    color: Color32,
    values: Vec<f32>,
    text: String,
    text_color: Option<Color32>,
}

struct Chart {
    title: String,
    max: f32,
    scale: Option<String>,
    series: Vec<Series>,
}

fn series(label: impl Into<String>, color: Color32, values: Vec<f32>, text: String) -> Series {
    Series { label: label.into(), color, values, text, text_color: None }
}

fn charts(state: &State, t: &Tokens) -> Vec<Chart> {
    let (info, s) = (&state.info, &state.samples);
    let last = s.back();
    let col = |f: &dyn Fn(&Sample) -> f32| s.iter().map(f).collect::<Vec<f32>>();
    let pct = |f: fn(&Sample) -> f32| last.map(f).filter(|v| v.is_finite()).map_or("n/a".into(), |v| format!("{v:.0}%"));

    let mem_total = info.mem_total.max(1.0);
    let vram_total = info.vram_total;
    let net_max = s.iter().map(|x| x.net_down.max(x.net_up)).fold(64.0 * 1024.0, f64::max) * 1.15;

    let temp = |label: &str, color, f: fn(&Sample) -> f32, missing: &str| {
        let v = last.map(f).unwrap_or(f32::NAN);
        let mut series = series(label, color, col(&|x| f(x)), if v.is_finite() { format!("{v:.0}°C") } else { missing.into() });
        series.text_color = if v >= 85.0 { Some(t.danger) } else if v >= 70.0 { Some(t.warning) } else { None };
        series
    };
    let mut temps = vec![
        temp("CPU", t.info, |x| x.cpu_temp, if info.cpu_temp_hint.is_empty() { "n/a" } else { info.cpu_temp_hint }),
        temp("GPU", t.warning, |x| x.gpu.temp, "n/a"),
    ];
    if info.has_igpu {
        temps.push(temp("iGPU", t.muted, |x| x.gpu.igpu_temp, "n/a"));
    }

    let palette = [t.info, t.warning, t.success, t.muted, t.danger];
    let disks = info
        .disk_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let at = move |x: &Sample| x.disks.get(i).copied().unwrap_or(f32::NAN);
            let v = last.map_or(f32::NAN, at);
            let text = if v.is_finite() { format!("{v:.0}%") } else { "n/a".into() };
            series(name.as_str(), palette[i % palette.len()], col(&at), text)
        })
        .collect();

    vec![
        Chart {
            title: "CPU".into(),
            max: 100.0,
            scale: None,
            series: vec![
                series("Usage", t.info, col(&|x| x.cpu), pct(|x| x.cpu)),
                series("Top core", t.warning, col(&|x| x.top_core), pct(|x| x.top_core)),
            ],
        },
        Chart {
            title: "Memory".into(),
            max: 100.0,
            scale: None,
            series: vec![series(
                "Used",
                t.info,
                col(&|x| (x.mem_used / mem_total * 100.0) as f32),
                last.map_or("n/a".into(), |x| format!("{:.1} / {:.1} GB", x.mem_used / GB, mem_total / GB)),
            )],
        },
        Chart { title: "Disks".into(), max: 100.0, scale: None, series: disks },
        Chart {
            title: "Network".into(),
            max: net_max as f32,
            scale: Some(rate(net_max)),
            series: vec![
                series("Down", t.info, col(&|x| x.net_down as f32), last.map_or("n/a".into(), |x| rate(x.net_down))),
                series("Up", t.warning, col(&|x| x.net_up as f32), last.map_or("n/a".into(), |x| rate(x.net_up))),
            ],
        },
        Chart {
            title: info.gpu_name.clone(),
            max: 100.0,
            scale: None,
            series: vec![
                series("Load", t.info, col(&|x| x.gpu.load), pct(|x| x.gpu.load)),
                series(
                    "VRAM",
                    t.warning,
                    col(&|x| (x.gpu.vram_used / vram_total * 100.0) as f32),
                    last.filter(|x| x.gpu.vram_used.is_finite() && vram_total > 0.0)
                        .map_or("n/a".into(), |x| format!("{:.1} / {:.0} GB", x.gpu.vram_used / GB, vram_total / GB)),
                ),
            ],
        },
        Chart { title: "Temperature".into(), max: 100.0, scale: Some("100°C".into()), series: temps },
    ]
}

fn rate(bytes: f64) -> String {
    let (mut v, mut unit) = (bytes, 0);
    while v >= 1024.0 && unit < 3 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", ["B/s", "KB/s", "MB/s", "GB/s"][unit])
}

fn draw(ui: &egui::Ui, rect: Rect, chart: &Chart, t: &Tokens) {
    let p = ui.painter_at(rect);
    p.rect(rect, CornerRadius::same(12), t.elevated, Stroke::new(1.0, t.border), StrokeKind::Inside);
    p.text(rect.left_top() + vec2(12.0, 10.0), Align2::LEFT_TOP, &chart.title, FontId::proportional(14.0), t.fg);

    // Legend, right-aligned: dot, label, current value, with a | between items.
    let mut x = rect.right() - 12.0;
    let y = rect.top() + 11.0;
    for (i, s) in chart.series.iter().rev().enumerate() {
        if i > 0 {
            x = p.text(pos2(x, y), Align2::RIGHT_TOP, "|", FontId::proportional(12.0), t.muted).left() - 10.0;
        }
        let value = p.text(pos2(x, y), Align2::RIGHT_TOP, &s.text, FontId::monospace(12.0), s.text_color.unwrap_or(t.fg));
        let label = p.text(pos2(value.left() - 5.0, y), Align2::RIGHT_TOP, &s.label, FontId::proportional(12.0), t.muted);
        p.circle_filled(pos2(label.left() - 7.0, label.center().y), 3.5, s.color);
        x = label.left() - 21.0;
    }

    let plot = Rect::from_min_max(rect.left_top() + vec2(12.0, 34.0), rect.right_bottom() - vec2(12.0, 12.0));
    for i in 0..=4 {
        let y = plot.bottom() - plot.height() * i as f32 / 4.0;
        p.hline(plot.x_range(), y, Stroke::new(1.0, t.border));
    }
    if let Some(scale) = &chart.scale {
        p.text(plot.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, scale, FontId::proportional(11.0), t.muted);
    }

    // Newest sample on the right edge; NaN breaks the line.
    let step = plot.width() / (HISTORY - 1) as f32;
    for s in &chart.series {
        let n = s.values.len();
        let mut line = Vec::with_capacity(n);
        for (i, &v) in s.values.iter().enumerate() {
            if v.is_finite() {
                let x = plot.right() - (n - 1 - i) as f32 * step;
                line.push(pos2(x, plot.bottom() - (v / chart.max).clamp(0.0, 1.0) * plot.height()));
            } else {
                flush(&p, &mut line, s.color);
            }
        }
        flush(&p, &mut line, s.color);
    }
}

fn flush(p: &egui::Painter, line: &mut Vec<egui::Pos2>, color: Color32) {
    if line.len() > 1 {
        p.add(Shape::line(std::mem::take(line), Stroke::new(1.6, color)));
    }
    line.clear();
}
