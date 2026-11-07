//! Windows: network from the interface table, GPU load and VRAM from PDH, GPU
//! temperatures from D3DKMT (the same source Task Manager uses). No driver needed.

use crate::sample::Gpu;
use std::collections::HashMap;
use std::ffi::c_void;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO, D3DKMTOpenAdapterFromLuid,
    D3DKMTQueryAdapterInfo, KMTQAITYPE_ADAPTERPERFDATA,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

struct Adapter {
    name: String,
    /// LUID as PDH writes it in instance names, e.g. `luid_0x00000000_0x00014acc`.
    tag: String,
    vram: f64,
    kmt: Option<u32>,
}

pub struct Platform {
    pub gpu_name: String,
    pub vram_total: f64,
    pub has_igpu: bool,
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    vram: PDH_HCOUNTER,
    dgpu: Option<Adapter>,
    igpu: Option<Adapter>,
}

impl Platform {
    pub fn new() -> Self {
        let mut adapters = adapters();
        adapters.sort_by(|a, b| b.vram.total_cmp(&a.vram));
        let mut adapters = adapters.into_iter();
        let (dgpu, igpu) = (adapters.next(), adapters.next());

        let (mut query, mut engines, mut vram) = Default::default();
        unsafe {
            PdhOpenQueryW(PCWSTR::null(), 0, &mut query);
            PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engines);
            PdhAddEnglishCounterW(query, w!("\\GPU Adapter Memory(*)\\Dedicated Usage"), 0, &mut vram);
            PdhCollectQueryData(query);
        }

        Self {
            gpu_name: dgpu.as_ref().map_or("GPU".into(), |g| g.name.clone()),
            vram_total: dgpu.as_ref().map_or(0.0, |g| g.vram),
            has_igpu: igpu.is_some(),
            query,
            engines,
            vram,
            dgpu,
            igpu,
        }
    }

    /// Bytes received and sent since boot on physical NICs that are up. Virtual
    /// adapters (VPN tunnels, Hyper-V) and NDIS filter layers are skipped so the
    /// same traffic is not counted twice.
    pub fn net_totals(&mut self) -> (u64, u64) {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        unsafe {
            if GetIfTable2(&mut table).is_err() {
                return (0, 0);
            }
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
            let (mut down, mut up) = (0, 0);
            for row in rows {
                // Bit 0: HardwareInterface, bit 1: FilterInterface.
                if row.InterfaceAndOperStatusFlags._bitfield & 0b11 == 0b01 && row.OperStatus == IfOperStatusUp {
                    down += row.InOctets;
                    up += row.OutOctets;
                }
            }
            FreeMibTable(table as *const c_void);
            (down, up)
        }
    }

    pub fn gpu(&mut self) -> Gpu {
        unsafe { PdhCollectQueryData(self.query) };
        let (mut load, mut vram_used) = (f32::NAN, f64::NAN);
        if let Some(gpu) = &self.dgpu {
            // Task Manager's rule: sum each engine across processes, then take the busiest engine.
            let mut per_engine: HashMap<String, f64> = HashMap::new();
            for (name, value) in counter_items(self.engines) {
                if let Some((_, engine)) = name.split_once(&gpu.tag) {
                    *per_engine.entry(engine.to_string()).or_default() += value;
                }
            }
            load = per_engine.values().fold(0.0f64, |a, &b| a.max(b)).min(100.0) as f32;
            vram_used = counter_items(self.vram).iter().filter(|(n, _)| n.contains(&gpu.tag)).map(|(_, v)| v).sum();
        }
        Gpu { load, vram_used, temp: temperature(&self.dgpu), igpu_temp: temperature(&self.igpu) }
    }
}

fn adapters() -> Vec<Adapter> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let luid = desc.AdapterLuid;
        out.push(Adapter {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            tag: format!("luid_0x{:08x}_0x{:08x}", luid.HighPart as u32, luid.LowPart),
            vram: desc.DedicatedVideoMemory as f64,
            kmt: open_kmt(luid),
        });
    }
    out
}

fn open_kmt(luid: LUID) -> Option<u32> {
    let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: luid, ..Default::default() };
    unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok().then_some(open.hAdapter)
}

/// Temperature in degrees C. The driver reports tenths of a degree.
fn temperature(adapter: &Option<Adapter>) -> f32 {
    let Some(handle) = adapter.as_ref().and_then(|a| a.kmt) else { return f32::NAN };
    let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: &mut perf as *mut _ as *mut c_void,
        PrivateDriverDataSize: size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
    };
    let ok = unsafe { D3DKMTQueryAdapterInfo(&mut query) }.is_ok();
    if ok && perf.Temperature > 0 { perf.Temperature as f32 / 10.0 } else { f32::NAN }
}

/// Every instance of a wildcard counter as (lowercase instance name, value).
fn counter_items(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
    for _ in 0..3 {
        let (mut size, mut count) = (0u32, 0u32);
        unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None) };
        if size == 0 {
            return Vec::new();
        }
        // u64 storage keeps the item array aligned.
        let mut buf = vec![0u64; size as usize / 8 + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) } != 0 {
            continue; // instances changed between the two calls
        }
        let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
        return items
            .iter()
            .filter(|i| i.FmtValue.CStatus <= 1) // PDH_CSTATUS_VALID_DATA or NEW_DATA
            .map(|i| unsafe {
                (i.szName.to_string().unwrap_or_default().to_ascii_lowercase(), i.FmtValue.Anonymous.doubleValue)
            })
            .collect();
    }
    Vec::new()
}
