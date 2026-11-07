//! macOS: network from sysinfo, counting only en* interfaces (Wi-Fi and Ethernet,
//! so utun VPN tunnels and bridges are skipped). GPU load and memory from the
//! IOAccelerator PerformanceStatistics, disk activity from IOBlockStorageDriver
//! Statistics, both in IOKit. macOS has no public GPU temperature, so that line
//! shows n/a. No root needed.

use crate::sample::Gpu;
use core_foundation::base::{CFType, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::{CStr, c_char, c_void};
use std::time::Instant;
use sysinfo::{Networks, System};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(main_port: u32, matching: CFDictionaryRef, existing: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: CFStringRef, allocator: *const c_void, options: u32) -> CFTypeRef;
    fn IOObjectRelease(object: u32) -> i32;
}

pub struct Platform {
    pub gpu_name: String,
    pub vram_total: f64,
    pub cpu_temp_hint: &'static str,
    /// Apple silicon does not report a RAM speed.
    pub mem_speed: Option<String>,
    pub disk_names: Vec<String>,
    disk_prev: Vec<f64>,
    disk_at: Instant,
    networks: Option<Networks>,
}

impl Platform {
    pub fn new() -> Self {
        // Apple silicon GPUs share system memory, so VRAM is shown against total RAM.
        let ram = System::new().map_or(0.0, |mut s| {
            s.refresh_memory();
            s.total_memory() as f64
        });
        let model = accelerator().and_then(|a| a.model);
        let disk_prev = disk_busy_ns();
        Self {
            gpu_name: model.unwrap_or_else(|| "GPU".into()),
            vram_total: ram,
            cpu_temp_hint: "n/a",
            mem_speed: None,
            disk_names: (1..=disk_prev.len()).map(|i| format!("Disk {i}")).collect(),
            disk_prev,
            disk_at: Instant::now(),
            networks: Networks::new_with_refreshed_list().ok(),
        }
    }

    pub fn net_totals(&mut self) -> (u64, u64) {
        let Some(networks) = &mut self.networks else { return (0, 0) };
        networks.refresh(true);
        networks
            .iter()
            .filter(|(name, _)| name.as_encoded_bytes().starts_with(b"en"))
            .fold((0, 0), |(down, up), (_, n)| (down + n.total_received(), up + n.total_transmitted()))
    }

    pub fn gpu(&mut self) -> Gpu {
        let a = accelerator();
        Gpu {
            load: a.as_ref().map_or(f32::NAN, |a| a.load),
            vram_used: a.as_ref().map_or(f64::NAN, |a| a.memory),
            temp: f32::NAN,
            fan_rpm: f32::NAN,
        }
    }

    /// Share of the last interval each disk spent reading or writing, capped at 100%.
    /// Disks attached after launch are ignored.
    pub fn disks(&mut self) -> Vec<f32> {
        let ns = self.disk_at.elapsed().as_secs_f64() * 1e9;
        self.disk_at = Instant::now();
        let now = disk_busy_ns();
        let mut out = Vec::with_capacity(self.disk_prev.len());
        for (i, prev) in self.disk_prev.iter_mut().enumerate() {
            match now.get(i) {
                Some(&busy) => {
                    out.push(((busy - *prev) / ns.max(1.0) * 100.0).clamp(0.0, 100.0) as f32);
                    *prev = busy;
                }
                None => out.push(f32::NAN),
            }
        }
        out
    }
}

struct Accelerator {
    load: f32,
    memory: f64,
    model: Option<String>,
}

/// The first IOAccelerator that publishes PerformanceStatistics.
fn accelerator() -> Option<Accelerator> {
    let mut found = None;
    each_service(c"IOAccelerator", |entry| {
        if found.is_some() {
            return;
        }
        if let Some(stats) = dictionary(entry, "PerformanceStatistics") {
            found = Some(Accelerator {
                load: number(&stats, "Device Utilization %").map_or(f32::NAN, |v| v as f32),
                memory: number(&stats, "In use system memory").unwrap_or(f64::NAN),
                model: property(entry, "model").and_then(|v| v.downcast_into::<CFString>()).map(|s| s.to_string()),
            });
        }
    });
    found
}

/// Total nanoseconds each block storage driver has spent reading plus writing.
fn disk_busy_ns() -> Vec<f64> {
    let mut out = Vec::new();
    each_service(c"IOBlockStorageDriver", |entry| {
        if let Some(stats) = dictionary(entry, "Statistics") {
            let read = number(&stats, "Total Time (Read)").unwrap_or(0.0);
            out.push(read + number(&stats, "Total Time (Write)").unwrap_or(0.0));
        }
    });
    out
}

/// Calls `f` with every registered service of an IOKit class, then releases it.
fn each_service(class: &CStr, mut f: impl FnMut(u32)) {
    let mut iterator = 0;
    let matching = unsafe { IOServiceMatching(class.as_ptr()) };
    // IOServiceGetMatchingServices takes ownership of `matching`.
    if unsafe { IOServiceGetMatchingServices(0, matching as CFDictionaryRef, &mut iterator) } != 0 {
        return;
    }
    loop {
        let entry = unsafe { IOIteratorNext(iterator) };
        if entry == 0 {
            break;
        }
        f(entry);
        unsafe { IOObjectRelease(entry) };
    }
    unsafe { IOObjectRelease(iterator) };
}

/// One registry property as an owned CF object. Callers check its type with downcast.
fn property(entry: u32, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, key.as_concrete_TypeRef(), kCFAllocatorDefault as *const c_void, 0) };
    (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

fn dictionary(entry: u32, key: &str) -> Option<CFDictionary> {
    property(entry, key).and_then(|v| v.downcast_into::<CFDictionary>())
}

fn number(dict: &CFDictionary, key: &str) -> Option<f64> {
    let key = CFString::new(key);
    dict.find(key.as_concrete_TypeRef() as *const c_void)
        .map(|v| unsafe { CFType::wrap_under_get_rule(*v as CFTypeRef) })
        .and_then(|v| v.downcast_into::<CFNumber>())
        .and_then(|n| n.to_f64())
}
