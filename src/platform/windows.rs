//! Windows: network from the interface table, GPU load, VRAM and disk activity
//! from PDH, GPU temperatures from D3DKMT (the same source Task Manager uses).
//! None of that needs a driver. CPU temperature does: it comes from the PawnIO
//! driver when it is installed and Vitals runs as administrator.

use crate::sample::Gpu;
use std::collections::HashMap;
use std::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
use windows::Win32::System::Threading::{CreateMutexW, INFINITE, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::Shell::{IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, s};
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO, D3DKMTOpenAdapterFromLuid,
    D3DKMTQueryAdapterInfo, KMTQAITYPE_ADAPTERPERFDATA,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Performance::{
    PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue,
    PdhOpenQueryW,
};

/// Not exported by the windows crate (pdh.h: PDH_FMT_NOCAP100).
const PDH_FMT_NOCAP100: PDH_FMT = PDH_FMT(0x8000);
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
    /// Becomes true once the driver reports a fan speed, so 0 can mean "fan off".
    fan_seen: bool,
    clock_query: PDH_HQUERY,
    clock_base: PDH_HCOUNTER,
    clock_perf: PDH_HCOUNTER,
    /// Legend text while there is no CPU temperature.
    pub cpu_temp_hint: &'static str,
    /// RAM type and speed, such as "DDR5 5600 MT/s".
    pub mem_speed: Option<String>,
    pawnio: Option<PawnIo>,
    pub disk_names: Vec<String>,
    disk_keys: Vec<String>,
    disk_query: PDH_HQUERY,
    disk_idle: PDH_HCOUNTER,
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    vram: PDH_HCOUNTER,
    dgpu: Option<Adapter>,
}

impl Platform {
    pub fn new() -> Self {
        // The adapter with the most dedicated memory is the graphics card.
        let dgpu = adapters().into_iter().max_by(|a, b| a.vram.total_cmp(&b.vram));

        // Task Manager's CPU speed: base frequency times % Processor Performance.
        let (mut clock_query, mut clock_base, mut clock_perf) = Default::default();
        unsafe {
            PdhOpenQueryW(PCWSTR::null(), 0, &mut clock_query);
            PdhAddEnglishCounterW(clock_query, w!("\\Processor Information(_Total)\\Processor Frequency"), 0, &mut clock_base);
            PdhAddEnglishCounterW(clock_query, w!("\\Processor Information(_Total)\\% Processor Performance"), 0, &mut clock_perf);
            PdhCollectQueryData(clock_query);
        }

        let (mut query, mut engines, mut vram) = Default::default();
        unsafe {
            PdhOpenQueryW(PCWSTR::null(), 0, &mut query);
            PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engines);
            PdhAddEnglishCounterW(query, w!("\\GPU Adapter Memory(*)\\Dedicated Usage"), 0, &mut vram);
            PdhCollectQueryData(query);
        }

        // A rate counter lists its instances only after two collections.
        let (mut disk_query, mut disk_idle) = Default::default();
        unsafe {
            PdhOpenQueryW(PCWSTR::null(), 0, &mut disk_query);
            PdhAddEnglishCounterW(disk_query, w!("\\PhysicalDisk(*)\\% Idle Time"), 0, &mut disk_idle);
            PdhCollectQueryData(disk_query);
            std::thread::sleep(std::time::Duration::from_millis(100));
            PdhCollectQueryData(disk_query);
        }
        let mut disk_keys: Vec<String> =
            counter_items(disk_idle).into_iter().map(|(name, _)| name).filter(|n| n != "_total").collect();
        disk_keys.sort_by_key(|k| disk_label(k));
        let (pawnio, cpu_temp_hint) = match PawnIo::open() {
            Ok(p) => {
                offer_autostart();
                (Some(p), "n/a")
            }
            Err(hint) => (None, hint),
        };

        Self {
            gpu_name: dgpu.as_ref().map_or("GPU".into(), |g| g.name.clone()),
            vram_total: dgpu.as_ref().map_or(0.0, |g| g.vram),
            fan_seen: false,
            clock_query,
            clock_base,
            clock_perf,
            cpu_temp_hint,
            mem_speed: memory_speed(),
            pawnio,
            disk_names: disk_keys.iter().map(|k| disk_label(k)).collect(),
            disk_keys,
            disk_query,
            disk_idle,
            query,
            engines,
            vram,
            dgpu,
        }
    }

    /// Current CPU clock in GHz, as Task Manager computes it.
    pub fn cpu_ghz(&mut self) -> f32 {
        unsafe { PdhCollectQueryData(self.clock_query) };
        let value = |counter| {
            let mut v = PDH_FMT_COUNTERVALUE::default();
            // NOCAP100: % Processor Performance goes above 100 while boosting.
            let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100.0);
            let ok = unsafe { PdhGetFormattedCounterValue(counter, format, None, &mut v) } == 0;
            if ok && v.CStatus <= 1 { unsafe { v.Anonymous.doubleValue } } else { f64::NAN }
        };
        (value(self.clock_base) * value(self.clock_perf) / 100.0 / 1000.0) as f32
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
        let perf = perf_data(&self.dgpu);
        let temp = perf.filter(|p| p.Temperature > 0).map_or(f32::NAN, |p| p.Temperature as f32 / 10.0);
        let fan = perf.map_or(0, |p| p.FanRPM);
        self.fan_seen |= fan > 0;
        Gpu { load, vram_used, temp, fan_rpm: if self.fan_seen { fan as f32 } else { f32::NAN } }
    }

    pub fn cpu_temp(&mut self) -> f32 {
        self.pawnio.as_ref().map_or(f32::NAN, PawnIo::tctl)
    }

    /// Active time per physical disk, in the order of `disk_names`, like Task Manager.
    pub fn disks(&mut self) -> Vec<f32> {
        unsafe { PdhCollectQueryData(self.disk_query) };
        let idle: HashMap<String, f64> = counter_items(self.disk_idle).into_iter().collect();
        self.disk_keys.iter().map(|k| idle.get(k).map_or(f32::NAN, |v| (100.0 - v).clamp(0.0, 100.0) as f32)).collect()
    }
}

const PAWNIO_SETUP: &[u8] = include_bytes!("../../assets/pawnio/PawnIO_setup.exe");
const AMD_MODULE: &[u8] = include_bytes!("../../assets/pawnio/AMDFamily17.bin");

type PawnOpen = unsafe extern "system" fn(*mut HANDLE) -> i32;
type PawnLoad = unsafe extern "system" fn(HANDLE, *const u8, usize) -> i32;
type PawnExecute = unsafe extern "system" fn(HANDLE, *const u8, *const u64, usize, *mut u64, usize, *mut usize) -> i32;

/// AMD Zen (family 17h to 1Ah) CPU temperature through PawnIO's AMDFamily17 module.
struct PawnIo {
    handle: HANDLE,
    execute: PawnExecute,
    /// Hardware tools share the PCI index/data registers through this named mutex.
    pci_mutex: HANDLE,
}

impl PawnIo {
    /// Err carries the legend hint. The PawnIO device only accepts administrators,
    /// so an elevated Vitals offers to install the bundled driver when it is missing.
    fn open() -> Result<Self, &'static str> {
        if !is_amd_zen() {
            return Err("AMD only");
        }
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            return Err("run as admin");
        }
        let dir = std::env::var("ProgramFiles").map_err(|_| "n/a")?;
        let path = HSTRING::from(format!("{dir}\\PawnIO\\PawnIOLib.dll"));
        let lib = match unsafe { LoadLibraryW(&path) } {
            Ok(lib) => lib,
            Err(_) if ask_install() => {
                install_pawnio();
                unsafe { LoadLibraryW(&path) }.map_err(|_| "no driver")?
            }
            Err(_) => return Err("no driver"),
        };
        let (Some(open), Some(load), Some(execute)) = (unsafe {
            (GetProcAddress(lib, s!("pawnio_open")), GetProcAddress(lib, s!("pawnio_load")), GetProcAddress(lib, s!("pawnio_execute")))
        }) else {
            return Err("n/a");
        };
        let (open, load, execute): (PawnOpen, PawnLoad, PawnExecute) =
            unsafe { (std::mem::transmute(open), std::mem::transmute(load), std::mem::transmute(execute)) };

        let mut handle = HANDLE::default();
        if unsafe { open(&mut handle) } < 0 {
            return Err("n/a");
        }
        if unsafe { load(handle, AMD_MODULE.as_ptr(), AMD_MODULE.len()) } < 0 {
            return Err("n/a");
        }
        let pci_mutex = unsafe { CreateMutexW(None, false, w!("Global\\Access_PCI")) }.map_err(|_| "n/a")?;
        Ok(Self { handle, execute, pci_mutex })
    }

    /// Tctl in degrees C, from SMN register THM_TCON_CUR_TMP (0x59800).
    fn tctl(&self) -> f32 {
        let (input, mut output, mut written) = ([0x0005_9800u64], [0u64], 0usize);
        unsafe {
            let wait = WaitForSingleObject(self.pci_mutex, 100);
            if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                return f32::NAN;
            }
            let hr = (self.execute)(self.handle, c"ioctl_read_smn".as_ptr().cast(), input.as_ptr(), 1, output.as_mut_ptr(), 1, &mut written);
            let _ = ReleaseMutex(self.pci_mutex);
            if hr < 0 {
                return f32::NAN;
            }
        }
        let raw = output[0] as u32;
        let t = ((raw >> 21) & 0x7FF) as f32 * 0.125;
        // Range select (bit 19), or both TJ_SEL bits (17:16), shift the scale by 49 C.
        if raw & 0x8_0000 != 0 || raw & 0x3_0000 == 0x3_0000 { t - 49.0 } else { t }
    }
}

/// AMD family 17h to 1Ah (Zen to Zen 5), the CPUs the AMDFamily17 module supports.
fn is_amd_zen() -> bool {
    use std::arch::x86_64::__cpuid;
    let id = __cpuid(0);
    let vendor = [id.ebx, id.edx, id.ecx].map(u32::to_le_bytes).concat();
    let eax = __cpuid(1).eax;
    let base = (eax >> 8) & 0xF;
    let family = if base == 0xF { base + ((eax >> 20) & 0xFF) } else { base };
    vendor == b"AuthenticAMD" && (0x17..=0x1A).contains(&family)
}

fn ask_install() -> bool {
    let text = w!("Vitals reads the CPU temperature through the PawnIO driver, which is bundled with it. PawnIO is signed by Microsoft and also used by LibreHardwareMonitor.\n\nInstall PawnIO now?");
    ask(text)
}

/// Yes/No question in front of the Vitals window (it is asked from the sampler thread).
fn ask(text: PCWSTR) -> bool {
    let style = MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST;
    unsafe { MessageBoxW(None, text, w!("Vitals"), style) == IDYES }
}

/// Offers, once, a Task Scheduler task that starts Vitals at sign-in with
/// administrator rights and no UAC prompt, so the CPU temperature is always there.
/// Only called from an elevated Vitals with PawnIO working.
fn offer_autostart() {
    let declined = std::env::var("APPDATA").map(|d| std::path::PathBuf::from(d).join("Vitals").join("autostart-declined"));
    if declined.as_ref().is_ok_and(|p| p.exists()) || schtasks(&["/Query", "/TN", "Vitals"]) {
        return;
    }
    let text = w!("Start Vitals when you sign in?\n\nIt starts with administrator rights and no UAC prompt, so the CPU temperature is always shown. You can remove it later in Task Scheduler (task \"Vitals\").");
    if !ask(text) {
        if let Ok(path) = declined {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(path, b"");
        }
        return;
    }
    let (Ok(exe), Ok(domain), Ok(user)) = (std::env::current_exe(), std::env::var("USERDOMAIN"), std::env::var("USERNAME")) else {
        return;
    };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let user = esc(&format!("{domain}\\{user}"));
    // Not the schtasks defaults: no 72-hour time limit, and it also runs on battery.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Vitals at sign-in with administrator rights, so it can read the CPU temperature.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command></Exec></Actions>
</Task>"#,
        esc(&exe.to_string_lossy())
    );
    // schtasks reads task XML as UTF-16 with a byte order mark.
    let bytes: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
    let file = std::env::temp_dir().join("vitals-task.xml");
    if std::fs::write(&file, bytes).is_ok() {
        schtasks(&["/Create", "/TN", "Vitals", "/XML", &file.to_string_lossy(), "/F"]);
        let _ = std::fs::remove_file(&file);
    }
}

/// Runs schtasks.exe without a console window; true when it succeeds.
fn schtasks(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs the embedded PawnIO installer and waits for it. Vitals is already
/// elevated here, so there is no extra UAC prompt.
fn install_pawnio() {
    let path = std::env::temp_dir().join("PawnIO_setup.exe");
    if std::fs::write(&path, PAWNIO_SETUP).is_err() {
        return;
    }
    let file = HSTRING::from(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: w!("-install"),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut info).is_ok() && !info.hProcess.is_invalid() {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = CloseHandle(info.hProcess);
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// RAM type and speed from the SMBIOS memory device entries (type 17), the same
/// data Task Manager shows. Prefers the configured speed (XMP/EXPO) over the rated one.
fn memory_speed() -> Option<String> {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    let mut buf = vec![0u8; size as usize];
    if size == 0 || unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) } != size {
        return None;
    }
    let data = buf.get(8..)?; // RawSMBIOSData header
    let (mut speed, mut kind, mut i) = (0u16, 0u8, 0usize);
    while i + 4 <= data.len() && data[i] != 127 {
        let len = data[i + 1] as usize;
        if data[i] == 17 && len >= 0x22 && i + len <= data.len() {
            let entry = &data[i..i + len];
            let word = |at: usize| u16::from_le_bytes([entry[at], entry[at + 1]]);
            // 0 and 0xFFFF mean the configured speed is unknown: fall back to the rated one.
            let mts = if (1..0xFFFF).contains(&word(0x20)) { word(0x20) } else { word(0x15) };
            if mts > speed {
                (speed, kind) = (mts, entry[0x12]);
            }
        }
        // The formatted part is followed by a string set that ends with two NULs.
        let mut j = i + len;
        while j + 1 < data.len() && (data[j] != 0 || data[j + 1] != 0) {
            j += 1;
        }
        i = j + 2;
    }
    let kind = match kind {
        0x18 => "DDR3 ",
        0x1A => "DDR4 ",
        0x1E => "LPDDR4 ",
        0x22 => "DDR5 ",
        0x23 => "LPDDR5 ",
        _ => "",
    };
    (speed > 0).then(|| format!("{kind}{speed} MT/s"))
}

/// PDH instance to label: "3 c: g:" becomes "C: G:", a disk without a letter "Disk 4".
fn disk_label(instance: &str) -> String {
    match instance.split_once(' ') {
        Some((_, letters)) => letters.to_ascii_uppercase(),
        None => format!("Disk {instance}"),
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

/// The driver's live data: temperature in tenths of a degree C, fan speed in RPM.
fn perf_data(adapter: &Option<Adapter>) -> Option<D3DKMT_ADAPTER_PERFDATA> {
    let handle = adapter.as_ref()?.kmt?;
    let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: &mut perf as *mut _ as *mut c_void,
        PrivateDriverDataSize: size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut query) }.is_ok().then_some(perf)
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
