//! macOS: network from sysinfo, counting only en* interfaces (Wi-Fi and Ethernet,
//! so utun VPN tunnels and bridges are skipped). GPU load and memory from the
//! IOAccelerator PerformanceStatistics in IOKit. macOS has no public GPU
//! temperature, so that line shows n/a. No root needed.

use crate::sample::Gpu;
use core_foundation::base::{CFType, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::{c_char, c_void};
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
    pub has_igpu: bool,
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
        Self {
            gpu_name: model.unwrap_or_else(|| "GPU".into()),
            vram_total: ram,
            has_igpu: false,
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
            igpu_temp: f32::NAN,
        }
    }
}

struct Accelerator {
    load: f32,
    memory: f64,
    model: Option<String>,
}

/// The first IOAccelerator that publishes PerformanceStatistics.
fn accelerator() -> Option<Accelerator> {
    let mut iterator = 0;
    let matching = unsafe { IOServiceMatching(c"IOAccelerator".as_ptr()) };
    // IOServiceGetMatchingServices takes ownership of `matching`.
    if unsafe { IOServiceGetMatchingServices(0, matching as CFDictionaryRef, &mut iterator) } != 0 {
        return None;
    }
    let mut found = None;
    loop {
        let entry = unsafe { IOIteratorNext(iterator) };
        if entry == 0 {
            break;
        }
        if found.is_none() {
            if let Some(stats) = property(entry, "PerformanceStatistics").and_then(|v| v.downcast_into::<CFDictionary>()) {
                let number = |key: &str| {
                    let key = CFString::new(key);
                    stats
                        .find(key.as_concrete_TypeRef() as *const c_void)
                        .map(|v| unsafe { CFType::wrap_under_get_rule(*v as CFTypeRef) })
                        .and_then(|v| v.downcast_into::<CFNumber>())
                        .and_then(|n| n.to_f64())
                };
                found = Some(Accelerator {
                    load: number("Device Utilization %").map_or(f32::NAN, |v| v as f32),
                    memory: number("In use system memory").unwrap_or(f64::NAN),
                    model: property(entry, "model").and_then(|v| v.downcast_into::<CFString>()).map(|s| s.to_string()),
                });
            }
        }
        unsafe { IOObjectRelease(entry) };
    }
    unsafe { IOObjectRelease(iterator) };
    found
}

/// One registry property as an owned CF object. Callers check its type with downcast.
fn property(entry: u32, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, key.as_concrete_TypeRef(), kCFAllocatorDefault as *const c_void, 0) };
    (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}
