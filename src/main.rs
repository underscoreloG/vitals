#![windows_subsystem = "windows"]

mod sample;
mod theme;
#[cfg_attr(windows, path = "platform/windows.rs")]
#[cfg_attr(target_os = "linux", path = "platform/linux.rs")]
#[cfg_attr(target_os = "macos", path = "platform/macos.rs")]
mod platform;

use eframe::egui::containers::menu::{MenuButton, MenuConfig};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Painter, Rect, RichText, Shape, Stroke, StrokeKind, ThemePreference, pos2, vec2};
use sample::{Info, Sample, Sampler};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use theme::Tokens;

const HISTORY: usize = 120;
const TICK: Duration = Duration::from_secs(1);
const GAP: f32 = 10.0;
/// Minimal mode: one row per block, values only.
const ROW_HEIGHT: f32 = 34.0;
const ROW_GAP: f32 = 6.0;
/// Window margins plus the header line, used to size the window in minimal mode.
const CHROME_HEIGHT: f32 = 12.0 + 30.0 + 12.0;
/// CPU, Memory, GPU, Network, Disk activity, Temperature.
const BLOCKS: usize = 6;

/// Window height that fits `rows` minimal-mode rows exactly.
fn minimal_height(rows: usize) -> f32 {
    CHROME_HEIGHT + rows as f32 * ROW_HEIGHT + (rows - 1) as f32 * ROW_GAP
}
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(theme::ICON).expect("bundled icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // First launch opens in minimal mode, sized to its rows.
            .with_inner_size([460.0, minimal_height(BLOCKS)])
            .with_min_inner_size([320.0, 240.0])
            .with_icon(Arc::new(icon)),
        // Reopens with the last window size and position.
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native("Vitals", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(Default)]
struct State {
    info: Info,
    samples: VecDeque<Sample>,
}

struct App {
    state: Arc<Mutex<State>>,
    logo: egui::TextureHandle,
    minimal: bool,
    /// Window size to restore when leaving minimal mode.
    full_size: Option<egui::Vec2>,
    /// Whether Vitals starts at sign-in, read from the OS; None while it is being read or changed.
    autostart: Arc<Mutex<Option<bool>>>,
    /// Last window geometry eframe saved while the window was not minimized.
    window: Option<String>,
    always_on_top: bool,
}

fn set_always_on_top(ctx: &egui::Context, on: bool) {
    let level = if on { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
}

/// Optionally turns start at sign-in on or off, then reads it back, on a thread:
/// on Windows this runs schtasks and may wait on a UAC prompt.
fn refresh_autostart(slot: &Arc<Mutex<Option<bool>>>, ctx: &egui::Context, change: Option<bool>) {
    *slot.lock().unwrap() = None;
    let (slot, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        if let Some(on) = change {
            platform::set_autostart(on);
        }
        *slot.lock().unwrap() = Some(platform::autostart());
        ctx.request_repaint();
    });
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = &cc.egui_ctx;
        let saved = |key| cc.storage.and_then(|s| s.get_string(key));
        // Minimal is the default; a saved choice of Graphs wins.
        let minimal = saved("minimal").as_deref() != Some("false");
        let full_size = saved("full_size").and_then(|s| {
            let (w, h) = s.split_once(' ')?;
            Some(vec2(w.parse().ok()?, h.parse().ok()?))
        });
        theme::apply(ctx);
        let always_on_top = saved("always_on_top").as_deref() == Some("true");
        if always_on_top {
            set_always_on_top(ctx, true);
        }
        let autostart = Arc::new(Mutex::new(None));
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
        Self { state, logo, minimal, full_size, autostart, window: saved("window"), always_on_top }
    }

    /// Switches mode and resizes the window: minimal fits its rows exactly,
    /// and leaving it restores the size the graphs had.
    fn toggle(&mut self, ctx: &egui::Context, rows: usize) {
        let current = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        let size = if self.minimal {
            self.full_size.unwrap_or(vec2(460.0, 860.0))
        } else {
            self.full_size = current;
            vec2(current.map_or(460.0, |s| s.x), minimal_height(rows))
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        self.minimal = !self.minimal;
    }

    /// Gear button with the theme choice, always on top and start at sign-in. egui saves the theme
    /// with its own state and gives the window title bar the same theme.
    fn settings(&mut self, ui: &mut egui::Ui) {
        // Drawn by hand: the default fonts have no gear glyph.
        let gear = egui::Button::new("").min_size(vec2(26.0, 24.0)).frame_when_inactive(false);
        let config = MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (gear, _) = MenuButton::from_button(gear).config(config).ui(ui, |ui| {
            ui.label(RichText::new("Theme").size(12.0).weak());
            ui.horizontal(|ui| {
                let mut theme = ui.ctx().options(|o| o.theme_preference);
                for (value, label) in [(ThemePreference::System, "System"), (ThemePreference::Light, "Light"), (ThemePreference::Dark, "Dark")] {
                    if ui.selectable_value(&mut theme, value, label).clicked() {
                        ui.ctx().set_theme(theme);
                    }
                }
            });
            ui.separator();
            if ui.checkbox(&mut self.always_on_top, "Always on top").changed() {
                set_always_on_top(ui.ctx(), self.always_on_top);
            }
            let state = *self.autostart.lock().unwrap();
            let mut on = state.unwrap_or(false);
            if ui.add_enabled(state.is_some(), egui::Checkbox::new(&mut on, "Start at sign-in")).changed() {
                refresh_autostart(&self.autostart, ui.ctx(), Some(on));
            }
        });
        // Read when the menu opens, so a change made elsewhere (the first-run offer, Task Scheduler) shows.
        if gear.clicked() {
            refresh_autostart(&self.autostart, ui.ctx(), None);
        }
        let t = theme::tokens(ui.visuals().dark_mode);
        let (c, color) = (gear.rect.center(), if gear.hovered() { t.fg } else { t.muted });
        ui.painter().circle_stroke(c, 4.0, Stroke::new(1.6, color));
        for i in 0..8 {
            let dir = egui::Vec2::angled(i as f32 * std::f32::consts::FRAC_PI_4);
            ui.painter().line_segment([c + dir * 4.6, c + dir * 7.0], Stroke::new(2.4, color));
        }
        gear.on_hover_text("Settings");
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // eframe has just stored the window geometry. A minimized window on Windows is
        // 0 x 0 at -32000, which would reopen as a tiny window off screen, so put back
        // the last real geometry (an empty value makes eframe use the default size).
        match storage.get_string("window") {
            Some(window) if window.contains("inner_size_points:(x:0.0,y:0.0)") => {
                storage.set_string("window", self.window.clone().unwrap_or_default());
            }
            window => self.window = window,
        }
        storage.set_string("minimal", self.minimal.to_string());
        storage.set_string("always_on_top", self.always_on_top.to_string());
        if let Some(size) = self.full_size {
            storage.set_string("full_size", format!("{} {}", size.x, size.y));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = theme::tokens(ui.visuals().dark_mode);
        let view = view(&self.state.lock().unwrap(), t);
        let mut toggle = false;

        let frame = egui::Frame::new().fill(t.bg).inner_margin(12);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(vec2(22.0, 22.0)));
                ui.label(RichText::new("Vitals").size(16.0).strong());
                ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).size(12.0).color(t.muted));
                ui.spacing_mut().item_spacing.x = 2.0;

                // Mode switch: a chart icon shows the graphs, a rows icon goes back to values only.
                let button = ui.add(egui::Button::new("").min_size(vec2(26.0, 24.0)).frame_when_inactive(false));
                let stroke = Stroke::new(1.6, if button.hovered() { t.fg } else { t.muted });
                let at = |x: f32, y: f32| button.rect.center() + vec2(x, y);
                if self.minimal {
                    ui.painter().add(Shape::line(vec![at(-7.0, 4.0), at(-3.0, -1.0), at(1.0, 2.0), at(7.0, -5.0)], stroke));
                    ui.painter().hline(at(-7.0, 0.0).x..=at(7.0, 0.0).x, at(0.0, 6.0).y, Stroke::new(1.0, stroke.color));
                } else {
                    for y in [-4.0, 0.0, 4.0] {
                        ui.painter().hline(at(-6.0, 0.0).x..=at(6.0, 0.0).x, at(0.0, y).y, stroke);
                    }
                }
                toggle = button.on_hover_text(if self.minimal { "Show graphs" } else { "Values only" }).clicked();
                self.settings(ui);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new(".org").size(13.0).color(theme::ACCENT));
                    ui.label(RichText::new("kvlh").size(13.0).strong());
                });
            });
            ui.add_space(8.0);

            let area = ui.available_rect_before_wrap();

            // Minimal mode: every block as one row of values, no graphs.
            if self.minimal {
                for (i, chart) in view.iter().enumerate() {
                    let min = area.min + vec2(0.0, i as f32 * (ROW_HEIGHT + ROW_GAP));
                    let rect = Rect::from_min_size(min, vec2(area.width(), ROW_HEIGHT));
                    header(&ui.painter_at(rect), rect, chart, t);
                }
                return;
            }

            // One column when narrow, two when wide; a short last row stretches to full width.
            let cols = if area.width() >= 760.0 { 2 } else { 1 };
            let rows = view.len().div_ceil(cols);
            let h = (area.height() - GAP * (rows - 1) as f32) / rows as f32;
            for (i, chart) in view.iter().enumerate() {
                let (row, col) = (i / cols, i % cols);
                let in_row = (view.len() - row * cols).min(cols);
                let w = (area.width() - GAP * (in_row - 1) as f32) / in_row as f32;
                let min = area.min + vec2(col as f32 * (w + GAP), row as f32 * (h + GAP));
                draw(ui, Rect::from_min_size(min, vec2(w, h)), chart, t);
            }
        });
        if toggle {
            self.toggle(ui.ctx(), view.len());
        }
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
    /// Shown muted after the title, such as the RAM speed or the CPU clock.
    subtitle: Option<String>,
    max: f32,
    scale: Option<String>,
    series: Vec<Series>,
}

fn series(label: impl Into<String>, color: Color32, values: Vec<f32>, text: String) -> Series {
    Series { label: label.into(), color, values, text, text_color: None }
}

/// The blocks in display order.
fn view(state: &State, t: &Tokens) -> Vec<Chart> {
    let (info, s) = (&state.info, &state.samples);
    let last = s.back();
    let col = |f: &dyn Fn(&Sample) -> f32| s.iter().map(f).collect::<Vec<f32>>();
    let pct = |f: fn(&Sample) -> f32| last.map(f).filter(|v| v.is_finite()).map_or("n/a".into(), |v| format!("{v:.0}%"));

    let mem_total = info.mem_total.max(1.0);
    let vram_total = info.vram_total;
    let net_max = s.iter().map(|x| x.net_down.max(x.net_up)).fold(64.0 * 1024.0, f64::max) * 1.15;

    // Amber and red thresholds per sensor: a Ryzen 7000 runs at 95 °C by design.
    let temp = |label: &str, color, f: fn(&Sample) -> f32, missing: &str, warn: f32, hot: f32| {
        let v = last.map(f).unwrap_or(f32::NAN);
        let mut series = series(label, color, col(&|x| f(x)), if v.is_finite() { format!("{v:.0}°C") } else { missing.into() });
        series.text_color = if v >= hot { Some(t.danger) } else if v >= warn { Some(t.warning) } else { None };
        series
    };
    let cpu_hint = if info.cpu_temp_hint.is_empty() { "n/a" } else { info.cpu_temp_hint };
    let temps = vec![
        temp("CPU", t.info, |x| x.cpu_temp, cpu_hint, 90.0, 95.0),
        temp("GPU", t.warning, |x| x.gpu.temp, "n/a", 80.0, 90.0),
    ];

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

    let ghz = last.map(|x| x.cpu_ghz).filter(|v| v.is_finite() && *v > 0.0).map(|v| format!("{v:.2} GHz"));
    let fan = last.map(|x| x.gpu.fan_rpm).filter(|v| v.is_finite()).map(|rpm| {
        if rpm < 1.0 { "GPU fan off".to_string() } else { format!("GPU fan {rpm:.0} rpm") }
    });
    // RAM line and value turn amber from 80% used and red from 90%.
    let used = last.map_or(f64::NAN, |x| x.mem_used);
    let share = used / mem_total;
    let memory_tone = if share >= 0.9 { Some(t.danger) } else if share >= 0.8 { Some(t.warning) } else { None };
    let mut memory_series = series(
        "Used",
        memory_tone.unwrap_or(t.info),
        col(&|x| (x.mem_used / mem_total * 100.0) as f32),
        if used.is_finite() { format!("{:.1} / {:.1} GB", used / GB, mem_total / GB) } else { "n/a".into() },
    );
    memory_series.text_color = memory_tone;

    vec![
        Chart {
            title: if info.cpu_name.is_empty() { "CPU".into() } else { info.cpu_name.clone() },
            subtitle: ghz,
            max: 100.0,
            scale: None,
            series: vec![
                series("Usage", t.info, col(&|x| x.cpu), pct(|x| x.cpu)),
                series("Top core", t.warning, col(&|x| x.top_core), pct(|x| x.top_core)),
            ],
        },
        Chart { title: "Memory".into(), subtitle: info.mem_speed.clone(), max: 100.0, scale: None, series: vec![memory_series] },
        Chart {
            title: info.gpu_name.clone(),
            subtitle: None,
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
        Chart {
            title: "Network".into(),
            subtitle: None,
            max: net_max as f32,
            scale: Some(rate(net_max)),
            series: vec![
                series("Down", t.info, col(&|x| x.net_down as f32), last.map_or("n/a".into(), |x| rate(x.net_down))),
                series("Up", t.warning, col(&|x| x.net_up as f32), last.map_or("n/a".into(), |x| rate(x.net_up))),
            ],
        },
        Chart { title: "Disk activity".into(), subtitle: None, max: 100.0, scale: None, series: disks },
        Chart { title: "Temperature".into(), subtitle: fan, max: 100.0, scale: Some("100°C".into()), series: temps },
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

/// Card background, legend on the right and title on the left; the whole block in minimal mode.
fn header(p: &Painter, rect: Rect, chart: &Chart, t: &Tokens) {
    p.rect(rect, CornerRadius::same(12), t.elevated, Stroke::new(1.0, t.border), StrokeKind::Inside);

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

    // Title and subtitle, clipped so a long CPU or GPU name never runs into the legend.
    let title = p.with_clip_rect(Rect::from_min_max(rect.left_top(), pos2(x + 10.0, rect.top() + 30.0)));
    let end = title.text(rect.left_top() + vec2(12.0, 10.0), Align2::LEFT_TOP, &chart.title, FontId::proportional(14.0), t.fg);
    if let Some(subtitle) = &chart.subtitle {
        title.text(pos2(end.right() + 8.0, y), Align2::LEFT_TOP, subtitle, FontId::proportional(12.0), t.muted);
    }
}

fn draw(ui: &egui::Ui, rect: Rect, chart: &Chart, t: &Tokens) {
    let p = ui.painter_at(rect);
    header(&p, rect, chart, t);

    let plot = Rect::from_min_max(rect.left_top() + vec2(12.0, 34.0), rect.right_bottom() - vec2(12.0, 12.0));
    for i in 0..=4 {
        let y = plot.bottom() - plot.height() * i as f32 / 4.0;
        p.hline(plot.x_range(), y, Stroke::new(1.0, t.border));
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

    // Scale label last, on its own background, so lines near the top never hide it.
    if let Some(scale) = &chart.scale {
        let galley = p.layout_no_wrap(scale.clone(), FontId::proportional(11.0), t.muted);
        let pos = plot.left_top() + vec2(4.0, 3.0);
        p.rect_filled(Rect::from_min_size(pos, galley.size()).expand(2.0), CornerRadius::same(3), t.elevated);
        p.galley(pos, galley, t.muted);
    }
}

fn flush(p: &egui::Painter, line: &mut Vec<egui::Pos2>, color: Color32) {
    if line.len() > 1 {
        p.add(Shape::line(std::mem::take(line), Stroke::new(1.6, color)));
    }
    line.clear();
}
