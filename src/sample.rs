//! One reading per tick. CPU, memory and (outside Windows) CPU clock and
//! temperature come from sysinfo; network, disks and GPU come from the platform module.

use crate::platform::Platform;
use std::time::Instant;
use sysinfo::System;

/// GPU values; NaN means the platform does not report it.
#[derive(Clone, Copy)]
pub struct Gpu {
    pub load: f32,
    pub vram_used: f64,
    pub temp: f32,
    /// 0 means the fan is stopped; NaN means the driver does not report it.
    pub fan_rpm: f32,
}

#[derive(Clone)]
pub struct Sample {
    pub cpu: f32,
    pub top_core: f32,
    pub cpu_ghz: f32,
    pub cpu_temp: f32,
    pub mem_used: f64,
    pub net_down: f64,
    pub net_up: f64,
    pub gpu: Gpu,
    /// Active time % per disk, in the order of `Info::disk_names`.
    pub disks: Vec<f32>,
}

#[derive(Clone, Default)]
pub struct Info {
    pub cpu_name: String,
    pub mem_total: f64,
    pub mem_speed: Option<String>,
    pub gpu_name: String,
    pub vram_total: f64,
    pub disk_names: Vec<String>,
    /// Legend text while there is no CPU temperature, such as "run as admin".
    pub cpu_temp_hint: &'static str,
}

pub struct Sampler {
    pub info: Info,
    sys: System,
    #[cfg(not(windows))]
    components: Option<sysinfo::Components>,
    platform: Platform,
    net_prev: (u64, u64),
    at: Instant,
}

impl Sampler {
    pub fn new() -> Self {
        let mut sys = System::new().expect("sysinfo supports this OS");
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let mut platform = Platform::new();
        let info = Info {
            cpu_name: short_cpu_name(sys.cpus().first().map_or("", |c| c.brand())),
            mem_total: sys.total_memory() as f64,
            mem_speed: platform.mem_speed.clone(),
            gpu_name: platform.gpu_name.clone(),
            vram_total: platform.vram_total,
            disk_names: platform.disk_names.clone(),
            cpu_temp_hint: platform.cpu_temp_hint,
        };
        Self {
            info,
            sys,
            #[cfg(not(windows))]
            components: sysinfo::Components::new_with_refreshed_list().ok(),
            net_prev: platform.net_totals(),
            platform,
            at: Instant::now(),
        }
    }

    pub fn sample(&mut self) -> Sample {
        let secs = self.at.elapsed().as_secs_f64().max(0.001);
        self.at = Instant::now();
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let net = self.platform.net_totals();
        let (down, up) = (net.0.saturating_sub(self.net_prev.0), net.1.saturating_sub(self.net_prev.1));
        self.net_prev = net;

        Sample {
            cpu: self.sys.global_cpu_usage(),
            top_core: self.sys.cpus().iter().map(|c| c.usage()).fold(0.0, f32::max),
            cpu_ghz: self.cpu_ghz(),
            cpu_temp: self.cpu_temp(),
            mem_used: self.sys.used_memory() as f64,
            net_down: down as f64 / secs,
            net_up: up as f64 / secs,
            gpu: self.platform.gpu(),
            disks: self.platform.disks(),
        }
    }

    /// Windows reports a stale clock through the API sysinfo uses; the platform
    /// reads the same counters as Task Manager instead.
    #[cfg(windows)]
    fn cpu_ghz(&mut self) -> f32 {
        self.platform.cpu_ghz()
    }

    /// Average current clock across cores.
    #[cfg(not(windows))]
    fn cpu_ghz(&mut self) -> f32 {
        self.sys.refresh_cpu_frequency();
        let mhz: Vec<f32> = self.sys.cpus().iter().map(|c| c.frequency() as f32).filter(|&f| f > 0.0).collect();
        if mhz.is_empty() { f32::NAN } else { mhz.iter().sum::<f32>() / mhz.len() as f32 / 1000.0 }
    }

    /// Windows has no CPU temperature without a kernel driver; the platform uses PawnIO.
    #[cfg(windows)]
    fn cpu_temp(&mut self) -> f32 {
        self.platform.cpu_temp()
    }

    /// Hottest CPU sensor: k10temp/zenpower (AMD) and coretemp (Intel) on Linux,
    /// tdie die sensors on Apple silicon.
    #[cfg(not(windows))]
    fn cpu_temp(&mut self) -> f32 {
        const KEYS: [&str; 7] = ["k10temp", "zenpower", "coretemp", "tctl", "tdie", "package", "cpu"];
        let Some(components) = &mut self.components else { return f32::NAN };
        components.refresh(false);
        components
            .iter()
            .filter(|c| {
                let label = c.label().to_ascii_lowercase();
                KEYS.iter().any(|k| label.contains(k)) && !label.contains("gpu")
            })
            .filter_map(|c| c.temperature())
            .fold(f32::NAN, f32::max)
    }
}

/// "AMD Ryzen 9 7950X 16-Core Processor" becomes "AMD Ryzen 9 7950X",
/// "Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz" becomes "Intel Core i7-8700K".
fn short_cpu_name(brand: &str) -> String {
    let name = brand.replace("(R)", "").replace("(TM)", "").replace("(tm)", "");
    let name = name.split(" @ ").next().unwrap_or_default().split(" with ").next().unwrap_or_default();
    let words: Vec<&str> =
        name.split_whitespace().filter(|w| !w.ends_with("-Core") && !matches!(*w, "Processor" | "CPU")).collect();
    if words.is_empty() { "CPU".into() } else { words.join(" ") }
}
