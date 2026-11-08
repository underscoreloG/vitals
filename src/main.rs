#![windows_subsystem = "windows"]

mod sample;
mod theme;
#[cfg_attr(windows, path = "platform/windows.rs")]
#[cfg_attr(target_os = "linux", path = "platform/linux.rs")]
#[cfg_attr(target_os = "macos", path = "platform/macos.rs")]
mod platform;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Painter, Rect, RichText, Shape, Stroke, StrokeKind, ThemePreference, pos2, vec2};
use sample::{Info, Sample, Sampler};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// CPU, Memory, GPU, Network, Disk activity, Temperature, Fans.
const BLOCKS: usize = 7;

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
            // Low enough for minimal mode with a single block.
            .with_min_inner_size([320.0, minimal_height(1)])
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
    /// Window height minimal mode last fitted to; it refits only when the rows or the open menu
    /// need another height, so a height the user drags to stays until then.
    fitted: Option<f32>,
    /// Where the window must end to show the open settings menu; None while it is closed.
    menu_bottom: Option<f32>,
    /// Names the user gave the fans, keyed by the name Vitals shows by default.
    fan_labels: HashMap<String, String>,
    /// Blocks turned off in the settings, by name.
    hidden: HashSet<String>,
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
        // Saved as one "default name<TAB>user's name" per line.
        let fan_labels = saved("fan_names")
            .map(|s| s.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect())
            .unwrap_or_default();
        // Saved as one block name per line.
        let hidden = saved("hidden_blocks").map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
        Self { state, logo, minimal, full_size, autostart, window: saved("window"), always_on_top, fitted: None, menu_bottom: None, fan_labels, hidden }
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

    /// Gear button with the theme choice, always on top, start at sign-in, the blocks to show and
    /// the fan names. egui saves the theme with its own state and gives the window title bar the same theme.
    fn settings(&mut self, ui: &mut egui::Ui, blocks: &[&'static str]) {
        // Drawn by hand: the default fonts have no gear glyph.
        let gear = ui.add(egui::Button::new("").min_size(vec2(26.0, 24.0)).frame_when_inactive(false));
        // Always opens below the gear, so it does not jump when the window height changes. It scrolls
        // when the window is too short; in minimal mode the window grows to show it whole (see `ui`).
        let max_height = ui.ctx().content_rect().bottom() - gear.rect.bottom() - 20.0;
        let menu = egui::Popup::menu(&gear)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align_alternatives(&[])
            // The minimum too: egui lays the menu out in its last size, which would never grow back.
            .show(|ui| egui::ScrollArea::vertical().max_height(max_height).min_scrolled_height(max_height).show(ui, |ui| {
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

                ui.separator();
                ui.label(RichText::new("Blocks").size(12.0).weak());
                let shown = blocks.iter().filter(|b| !self.hidden.contains(**b)).count();
                egui::Grid::new("blocks").num_columns(2).show(ui, |ui| {
                    for (i, block) in blocks.iter().enumerate() {
                        let mut on = !self.hidden.contains(*block);
                        // The last block shown stays on.
                        if ui.add_enabled(!on || shown > 1, egui::Checkbox::new(&mut on, *block)).changed() {
                            if on {
                                self.hidden.remove(*block);
                            } else {
                                self.hidden.insert(block.to_string());
                            }
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });

                // Which header a fan input is wired to differs per board, so each fan can be renamed.
                let defaults = self.state.lock().unwrap().info.fan_names.clone();
                if !defaults.is_empty() && !self.hidden.contains("Fans") {
                    ui.separator();
                    ui.label(RichText::new("Fan names").size(12.0).weak());
                    egui::Grid::new("fan_names").num_columns(2).show(ui, |ui| {
                        for default in defaults {
                            ui.label(RichText::new(&default).size(12.0).weak());
                            let mut name = self.fan_labels.get(&default).cloned().unwrap_or_default();
                            let edit = egui::TextEdit::singleline(&mut name).hint_text(&default).desired_width(110.0).char_limit(12);
                            if ui.add(edit).changed() {
                                if name.trim().is_empty() {
                                    self.fan_labels.remove(&default);
                                } else {
                                    self.fan_labels.insert(default, name);
                                }
                            }
                            ui.end_row();
                        }
                    });
                }
            }));
        // Where the window must end to show the whole menu: its bottom plus whatever the scroll hides.
        self.menu_bottom = menu.map(|m| m.response.rect.bottom() + (m.inner.content_size.y - m.inner.inner_rect.height()).max(0.0) + 8.0);
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
        let labels: Vec<String> = self.fan_labels.iter().map(|(default, name)| format!("{default}\t{name}")).collect();
        storage.set_string("fan_names", labels.join("\n"));
        let mut hidden: Vec<&str> = self.hidden.iter().map(String::as_str).collect();
        hidden.sort();
        storage.set_string("hidden_blocks", hidden.join("\n"));
        if let Some(size) = self.full_size {
            storage.set_string("full_size", format!("{} {}", size.x, size.y));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = theme::tokens(ui.visuals().dark_mode);
        let mut view = view(&self.state.lock().unwrap(), t, &self.fan_labels);
        let blocks: Vec<&'static str> = view.iter().map(|chart| chart.name).collect();
        // At least one block shows, even if a settings file hides them all.
        if blocks.iter().any(|b| !self.hidden.contains(*b)) {
            view.retain(|chart| !self.hidden.contains(chart.name));
        }
        let mut toggle = false;

        // Minimal mode fits its rows (also when a window was saved with other blocks shown), and
        // grows while the settings menu is open so the menu shows whole.
        let height = minimal_height(view.len()).max(self.menu_bottom.unwrap_or(0.0));
        if self.minimal && self.fitted != Some(height) {
            if let Some(rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
                self.fitted = Some(height);
                if (rect.height() - height).abs() > 1.0 {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(rect.width(), height)));
                }
            }
        }

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
                self.settings(ui, &blocks);

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
    /// Fixed name of the block, used in the settings; the CPU and GPU titles are the hardware names.
    name: &'static str,
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
fn view(state: &State, t: &Tokens, fan_labels: &HashMap<String, String>) -> Vec<Chart> {
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

    // Every fan in RPM, against the fastest one seen in the history.
    let fan_max = s.iter().flat_map(|x| x.fans.iter().copied()).filter(|v| v.is_finite()).fold(1000.0, f32::max) * 1.15;
    let mut fans: Vec<Series> = info
        .fan_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let at = move |x: &Sample| x.fans.get(i).copied().unwrap_or(f32::NAN);
            let v = last.map_or(f32::NAN, at);
            let text = if v.is_finite() { format!("{v:.0}") } else { "n/a".into() };
            let label = fan_labels.get(name).unwrap_or(name);
            series(label.as_str(), palette[i % palette.len()], col(&at), text)
        })
        .collect();
    if !info.fan_hint.is_empty() {
        fans.insert(0, series("Board", t.muted, Vec::new(), info.fan_hint.into()));
    }

    let ghz = last.map(|x| x.cpu_ghz).filter(|v| v.is_finite() && *v > 0.0).map(|v| format!("{v:.2} GHz"));
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
            name: "CPU",
            title: if info.cpu_name.is_empty() { "CPU".into() } else { info.cpu_name.clone() },
            subtitle: ghz,
            max: 100.0,
            scale: None,
            series: vec![
                series("Usage", t.info, col(&|x| x.cpu), pct(|x| x.cpu)),
                series("Top core", t.warning, col(&|x| x.top_core), pct(|x| x.top_core)),
            ],
        },
        Chart { name: "Memory", title: "Memory".into(), subtitle: info.mem_speed.clone(), max: 100.0, scale: None, series: vec![memory_series] },
        Chart {
            name: "GPU",
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
            name: "Network",
            title: "Network".into(),
            subtitle: None,
            max: net_max as f32,
            scale: Some(rate(net_max)),
            series: vec![
                series("Down", t.info, col(&|x| x.net_down as f32), last.map_or("n/a".into(), |x| rate(x.net_down))),
                series("Up", t.warning, col(&|x| x.net_up as f32), last.map_or("n/a".into(), |x| rate(x.net_up))),
            ],
        },
        Chart { name: "Disk activity", title: "Disk activity".into(), subtitle: None, max: 100.0, scale: None, series: disks },
        Chart { name: "Temperature", title: "Temperature".into(), subtitle: None, max: 100.0, scale: Some("100°C".into()), series: temps },
        // RPM; the unit is on the graph scale only, so the legend has room for four or five fans.
        Chart { name: "Fans", title: "Fans".into(), subtitle: None, max: fan_max, scale: Some(format!("{fan_max:.0} rpm")), series: fans },
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

    // Legend, right-aligned: dot, label, current value, with a | between items. The title may be
    // clipped but keeps up to 30 px; items that still do not fit (many disks) count as "+N".
    let width = |text: &str, font: FontId| p.layout_no_wrap(text.to_string(), font, t.fg).size().x;
    let bar = width("|", FontId::proportional(12.0)) + 10.0;
    // Values count as at least 3 characters, so a percentage going from 9% to 10% does not change the count.
    let min_value = width("00%", FontId::monospace(12.0));
    let items: Vec<f32> = chart
        .series
        .iter()
        .map(|s| width(&s.text, FontId::monospace(12.0)).max(min_value) + 5.0 + width(&s.label, FontId::proportional(12.0)) + 21.0)
        .collect();
    // The title is clipped 10 px into the legend's last gap, hence 24 - 10.
    let budget = rect.width() - 14.0 - width(&chart.title, FontId::proportional(14.0)).min(30.0);
    let needed = |shown: usize| {
        let more = if shown < items.len() { width(&format!("+{}", items.len() - shown), FontId::proportional(12.0)) + 10.0 + bar } else { 0.0 };
        items[..shown].iter().sum::<f32>() + bar * shown.saturating_sub(1) as f32 + more
    };
    let mut shown = items.len();
    while shown > 1 && needed(shown) > budget {
        shown -= 1;
    }

    let mut x = rect.right() - 12.0;
    let y = rect.top() + 11.0;
    if shown < items.len() {
        let more = format!("+{}", items.len() - shown);
        x = p.text(pos2(x, y), Align2::RIGHT_TOP, more, FontId::proportional(12.0), t.muted).left() - 10.0;
    }
    for (i, s) in chart.series[..shown].iter().rev().enumerate() {
        if i > 0 || shown < items.len() {
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
