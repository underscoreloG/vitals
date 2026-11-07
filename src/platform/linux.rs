//! Linux: network from sysinfo, counting only physical NICs (those with a
//! /sys/class/net/<if>/device link, so VPN, bridges and containers are skipped).
//! GPU from amdgpu sysfs; other drivers report no load, VRAM or temperature there,
//! so those show n/a. No root needed.

use crate::sample::Gpu;
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::Networks;

struct Card {
    name: String,
    dev: PathBuf,
    temp: Option<PathBuf>,
    vram: f64,
}

pub struct Platform {
    pub gpu_name: String,
    pub vram_total: f64,
    pub has_igpu: bool,
    networks: Option<Networks>,
    dgpu: Option<Card>,
    igpu: Option<Card>,
}

impl Platform {
    pub fn new() -> Self {
        let mut cards = cards();
        cards.sort_by(|a, b| b.vram.total_cmp(&a.vram));
        let mut cards = cards.into_iter();
        let (dgpu, igpu) = (cards.next(), cards.next());
        Self {
            gpu_name: dgpu.as_ref().map_or("GPU".into(), |c| c.name.clone()),
            vram_total: dgpu.as_ref().map_or(0.0, |c| c.vram),
            has_igpu: igpu.as_ref().is_some_and(|c| c.temp.is_some()),
            networks: Networks::new_with_refreshed_list().ok(),
            dgpu,
            igpu,
        }
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
            temp: temperature(card),
            igpu_temp: temperature(self.igpu.as_ref()),
        }
    }
}

fn read(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Edge temperature in degrees C; hwmon reports millidegrees.
fn temperature(card: Option<&Card>) -> f32 {
    card.and_then(|c| c.temp.as_deref()).and_then(read).map_or(f32::NAN, |m| (m / 1000.0) as f32)
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
            let hwmon = fs::read_dir(dev.join("hwmon")).ok().and_then(|mut d| d.next()?.ok());
            let temp = hwmon.map(|h| h.path().join("temp1_input")).filter(|p| p.exists());
            Some(Card { name, vram: read(&dev.join("mem_info_vram_total")).unwrap_or(0.0), temp, dev })
        })
        .collect()
}
