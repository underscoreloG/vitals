//! One reading per tick. CPU, memory and (outside Windows) CPU temperature come
//! from sysinfo; network and GPU come from the platform module.

use crate::platform::Platform;
use std::time::Instant;
use sysinfo::System;

/// GPU values; NaN means the platform does not report it.
#[derive(Clone, Copy)]
pub struct Gpu {
    pub load: f32,
    pub vram_used: f64,
    pub temp: f32,
    pub igpu_temp: f32,
}

#[derive(Clone)]
pub struct Sample {
    pub cpu: f32,
    pub top_core: f32,
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
    pub mem_total: f64,
    pub gpu_name: String,
    pub vram_total: f64,
    pub has_igpu: bool,
    pub disk_names: Vec<String>,
    /// Legend text while there is no CPU temperature, such as "needs admin".
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
            mem_total: sys.total_memory() as f64,
            gpu_name: platform.gpu_name.clone(),
            vram_total: platform.vram_total,
            has_igpu: platform.has_igpu,
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
            cpu_temp: self.cpu_temp(),
            mem_used: self.sys.used_memory() as f64,
            net_down: down as f64 / secs,
            net_up: up as f64 / secs,
            gpu: self.platform.gpu(),
            disks: self.platform.disks(),
        }
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
