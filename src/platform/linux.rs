//! Linux: network from sysinfo, counting only physical NICs (those with a
//! /sys/class/net/<if>/device link, so VPN, bridges and containers are skipped).
//! GPU from amdgpu sysfs; other drivers report no load, VRAM or temperature there,
//! so those show n/a. Disk activity from /proc/diskstats. No root needed.

use crate::sample::Gpu;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use sysinfo::Networks;

struct Card {
    name: String,
    dev: PathBuf,
    /// hwmon folder with temp1_input (millidegrees) and fan1_input (RPM).
    hwmon: Option<PathBuf>,
    vram: f64,
}

pub struct Platform {
    pub gpu_name: String,
    pub vram_total: f64,
    pub cpu_temp_hint: &'static str,
    /// The SMBIOS table with RAM speeds is root-only on Linux.
    pub mem_speed: Option<String>,
    pub disk_names: Vec<String>,
    disk_prev: Vec<f64>,
    disk_at: Instant,
    networks: Option<Networks>,
    dgpu: Option<Card>,
    /// Motherboard and other fans; the graphics card's fan comes with `gpu()`.
    pub fan_names: Vec<String>,
    /// Legend text while board fans cannot be read; hwmon needs no root.
    pub fan_hint: &'static str,
    fan_inputs: Vec<PathBuf>,
}

impl Platform {
    pub fn new() -> Self {
        // The card with the most VRAM is the graphics card.
        let dgpu = cards().into_iter().max_by(|a, b| a.vram.total_cmp(&b.vram));

        // Whole physical disks only: loop, dm, md and zram devices have no `device` link.
        let ticks = io_ticks();
        let mut disk_names: Vec<String> =
            ticks.keys().filter(|n| Path::new("/sys/block").join(n).join("device").exists()).cloned().collect();
        disk_names.sort();
        let (fan_names, fan_inputs) = board_fans(dgpu.as_ref().and_then(|c| c.hwmon.as_deref())).into_iter().unzip();
        Self {
            fan_names,
            fan_hint: "",
            fan_inputs,
            gpu_name: dgpu.as_ref().map_or("GPU".into(), |c| c.name.clone()),
            vram_total: dgpu.as_ref().map_or(0.0, |c| c.vram),
            cpu_temp_hint: "n/a",
            mem_speed: None,
            disk_prev: disk_names.iter().map(|n| ticks[n]).collect(),
            disk_names,
            disk_at: Instant::now(),
            networks: Networks::new_with_refreshed_list().ok(),
            dgpu,
        }
    }

    /// Share of the last interval each disk spent doing I/O, like iostat's %util.
    pub fn disks(&mut self) -> Vec<f32> {
        let ms = self.disk_at.elapsed().as_secs_f64() * 1000.0;
        self.disk_at = Instant::now();
        let ticks = io_ticks();
        let mut out = Vec::with_capacity(self.disk_names.len());
        for (name, prev) in self.disk_names.iter().zip(self.disk_prev.iter_mut()) {
            match ticks.get(name) {
                Some(&now) => {
                    out.push(((now - *prev) / ms.max(1.0) * 100.0).clamp(0.0, 100.0) as f32);
                    *prev = now;
                }
                None => out.push(f32::NAN),
            }
        }
        out
    }

    /// RPM of each fan in `fan_names`.
    pub fn board_fans(&mut self) -> Vec<f32> {
        self.fan_inputs.iter().map(|input| read(input).map_or(f32::NAN, |rpm| rpm as f32)).collect()
    }

    pub fn net_totals(&mut self) -> (u64, u64) {
        let Some(networks) = &mut self.networks else { return (0, 0) };
        networks.refresh(true);
        networks
            .iter()
            .filter(|(name, _)| Path::new("/sys/class/net").join(name).join("device").exists())
            .fold((0, 0), |(down, up), (_, n)| (down + n.total_received(), up + n.total_transmitted()))
    }

    pub fn gpu(&mut self) -> Gpu {
        let card = self.dgpu.as_ref();
        Gpu {
            load: card.and_then(|c| read(&c.dev.join("gpu_busy_percent"))).map_or(f32::NAN, |v| v as f32),
            vram_used: card.and_then(|c| read(&c.dev.join("mem_info_vram_used"))).unwrap_or(f64::NAN),
            temp: hwmon(card, "temp1_input").map_or(f32::NAN, |m| (m / 1000.0) as f32),
            fan_rpm: hwmon(card, "fan1_input").map_or(f32::NAN, |rpm| rpm as f32),
        }
    }
}

/// Milliseconds each block device has spent doing I/O (field 13 of /proc/diskstats).
fn io_ticks() -> HashMap<String, f64> {
    let stats = fs::read_to_string("/proc/diskstats").unwrap_or_default();
    stats
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            Some((f.get(2)?.to_string(), f.get(12)?.parse().ok()?))
        })
        .collect()
}

fn read(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Fans of every hwmon chip (the motherboard's Super I/O through nct6775, it87 and
/// the like) except the graphics card's, with their labels when the driver has them.
/// Only fans spinning at startup: these chips also list every unused header at 0 RPM.
fn board_fans(gpu_hwmon: Option<&Path>) -> Vec<(String, PathBuf)> {
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else { return Vec::new() };
    let gpu = gpu_hwmon.and_then(|p| fs::canonicalize(p).ok());
    let mut chips: Vec<PathBuf> = dir.flatten().map(|e| e.path()).collect();
    chips.sort();
    let mut fans = Vec::new();
    for chip in chips.into_iter().filter(|c| gpu.is_none() || fs::canonicalize(c).ok() != gpu) {
        for n in 1..=16 {
            let input = chip.join(format!("fan{n}_input"));
            if read(&input).is_some_and(|rpm| rpm > 0.0) {
                let label = fs::read_to_string(chip.join(format!("fan{n}_label"))).map(|l| l.trim().to_string());
                fans.push((label.ok().filter(|l| !l.is_empty()).unwrap_or(format!("Fan {n}")), input));
            }
        }
    }
    fans
}

/// One hwmon value of the card, such as temp1_input (edge temperature).
fn hwmon(card: Option<&Card>, file: &str) -> Option<f64> {
    read(&card?.hwmon.as_ref()?.join(file))
}

/// GPUs as /sys/class/drm/cardN. Connector entries such as card0-DP-1 are skipped.
fn cards() -> Vec<Card> {
    let Ok(dir) = fs::read_dir("/sys/class/drm") else { return Vec::new() };
    dir.flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let index = file.strip_prefix("card")?;
            if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let dev = entry.path().join("device");
            let vendor = match fs::read_to_string(dev.join("vendor")).ok()?.trim() {
                "0x1002" => "AMD GPU",
                "0x10de" => "NVIDIA GPU",
                "0x8086" => "Intel GPU",
                _ => "GPU",
            };
            let product = fs::read_to_string(dev.join("product_name")).unwrap_or_default();
            let name = if product.trim().is_empty() { vendor.to_string() } else { product.trim().to_string() };
            let hwmon = fs::read_dir(dev.join("hwmon")).ok().and_then(|mut d| d.next()?.ok()).map(|h| h.path());
            Some(Card { name, vram: read(&dev.join("mem_info_vram_total")).unwrap_or(0.0), hwmon, dev })
        })
        .collect()
}

/// Start at sign-in: an entry in ~/.config/autostart, the freedesktop way.
fn autostart_entry() -> Option<(PathBuf, String)> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let exe = std::env::current_exe().ok()?;
    // Desktop Entry quoting: a backslash before " ` $ and \.
    let mut quoted = String::new();
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    let entry = format!("[Desktop Entry]\nType=Application\nName=Vitals\nExec=\"{quoted}\"\n");
    Some((config.join("autostart").join("vitals.desktop"), entry))
}

/// True when the autostart entry exists and starts this executable.
pub fn autostart() -> bool {
    autostart_entry().is_some_and(|(path, entry)| fs::read_to_string(path).is_ok_and(|s| s == entry))
}

pub fn set_autostart(on: bool) {
    let Some((path, entry)) = autostart_entry() else { return };
    let _ = if on {
        path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| fs::write(&path, entry))
    } else {
        fs::remove_file(&path)
    };
}
