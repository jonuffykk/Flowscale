use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use windows::core::{s, w, Result, BOOL, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, DONT_RESOLVE_DLL_REFERENCES};
use windows::Win32::System::Memory::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;

use crate::ipc::{self, Shared};
use crate::pacer::FrameStats;

pub const GFX_MODULES: &[(&str, &str)] = &[("d3d12.dll", "D3D12"), ("d3d11.dll", "D3D11"), ("vulkan-1.dll", "Vulkan"), ("d3d9.dll", "D3D9"), ("opengl32.dll", "OpenGL"), ("d3d10.dll", "D3D10"), ("dxgi.dll", "DXGI")];
const ANTICHEAT: &[&str] = &["easyanticheat", "beclient", "bedaisy", "vgk", "faceit", "xigncode", "nprotect", "gameguard", "ricochet", "eac_launcher", "battleye", "mhyprot", "ace-base", "nexon", "hackshield", "punkbuster", "pnkbstr", "equ8", "anticheatexpert"];
const IGNORE_EXE: &[&str] = &["explorer.exe", "flowscale.exe", "flowscale32.exe", "dwm.exe", "applicationframehost.exe", "searchhost.exe", "startmenuexperiencehost.exe",
    "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "code.exe", "devenv.exe", "discord.exe", "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe",
    "obs64.exe", "msedgewebview2.exe", "claude.exe", "slack.exe", "teams.exe", "spotify.exe", "windowsterminal.exe", "powershell.exe", "cmd.exe", "taskmgr.exe", "rtss.exe"];

#[allow(dead_code)]
pub struct ProcInfo { pub exe: String, pub is32: bool, pub gfx: Vec<&'static str>, pub anticheat: Option<String>, pub hooked: bool }

pub fn inspect(pid: u32) -> Option<ProcInfo> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = [0u16; 520]; let mut n = buf.len() as u32;
        let exe = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok() {
            String::from_utf16_lossy(&buf[..n as usize]).rsplit('\\').next().unwrap_or("").to_lowercase()
        } else { String::new() };
        let mut wow = BOOL(0); let _ = IsWow64Process(h, &mut wow);
        let mut mods = vec![HMODULE::default(); 2048]; let mut need = 0u32;
        let mut names = vec![];
        if EnumProcessModulesEx(h, mods.as_mut_ptr(), (mods.len() * size_of::<HMODULE>()) as u32, &mut need, LIST_MODULES_ALL).is_ok() {
            for m in &mods[..(need as usize / size_of::<HMODULE>()).min(mods.len())] {
                let mut nb = [0u16; 260];
                let l = GetModuleBaseNameW(h, Some(*m), &mut nb);
                names.push(String::from_utf16_lossy(&nb[..l as usize]).to_lowercase());
            }
        }
        let _ = CloseHandle(h);
        let gfx = GFX_MODULES.iter().filter(|(m, _)| names.iter().any(|n| n == m)).map(|(_, a)| *a).collect();
        let anticheat = names.iter().find(|n| ANTICHEAT.iter().any(|a| n.contains(a))).cloned();
        let hooked = names.iter().any(|n| n.starts_with("flowscale_hook"));
        Some(ProcInfo { exe, is32: wow.as_bool(), gfx, anticheat, hooked })
    }
}

pub fn ignored(exe: &str) -> bool { exe.is_empty() || IGNORE_EXE.contains(&exe) }

pub fn inject(pid: u32, dll: &Path) -> Result<()> {
    unsafe {
        let h = OpenProcess(PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ, false, pid)?;
        let wide: Vec<u16> = dll.as_os_str().encode_wide_z();
        let bytes = wide.len() * 2;
        let mem = VirtualAllocEx(h, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if mem.is_null() { let _ = CloseHandle(h); return Err(windows::core::Error::from_thread()); }
        let r = (|| -> Result<()> {
            WriteProcessMemory(h, mem, wide.as_ptr() as *const c_void, bytes, None)?;
            let k32 = GetModuleHandleW(w!("kernel32.dll"))?;
            let ll = GetProcAddress(k32, s!("LoadLibraryW")).ok_or_else(windows::core::Error::from_thread)?;
            let th = CreateRemoteThread(h, None, 0, Some(std::mem::transmute::<_, unsafe extern "system" fn(*mut c_void) -> u32>(ll)), Some(mem), 0, None)?;
            WaitForSingleObject(th, 8000);
            let mut code = 0u32; let _ = GetExitCodeThread(th, &mut code);
            let _ = CloseHandle(th);
            if code == 0 { return Err(windows::core::Error::new(E_FAIL, "LoadLibrary failed inside the game")); }
            call_attach(h, dll)
        })();
        let _ = VirtualFreeEx(h, mem, 0, MEM_RELEASE);
        let _ = CloseHandle(h);
        r
    }
}
unsafe fn call_attach(h: HANDLE, dll: &Path) -> Result<()> {
    unsafe {
        let name = dll.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let local = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, DONT_RESOLVE_DLL_REFERENCES)?;
        let rva = GetProcAddress(local, s!("fs_attach")).map(|p| p as usize - local.0 as usize);
        let _ = FreeLibrary(local);
        let rva = rva.ok_or_else(|| windows::core::Error::new(E_FAIL, "fs_attach export missing"))?;
        let mut mods = vec![HMODULE::default(); 2048]; let mut need = 0u32;
        EnumProcessModulesEx(h, mods.as_mut_ptr(), (mods.len() * size_of::<HMODULE>()) as u32, &mut need, LIST_MODULES_ALL)?;
        let base = mods[..(need as usize / size_of::<HMODULE>()).min(mods.len())].iter().find(|m| {
            let mut nb = [0u16; 260]; let l = GetModuleBaseNameW(h, Some(**m), &mut nb);
            String::from_utf16_lossy(&nb[..l as usize]).to_lowercase() == name
        }).ok_or_else(|| windows::core::Error::new(E_FAIL, "hook module not found in game"))?;
        let entry: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(base.0 as usize + rva);
        let th = CreateRemoteThread(h, None, 0, Some(entry), None, 0, None)?;
        WaitForSingleObject(th, 8000);
        let _ = CloseHandle(th);
        Ok(())
    }
}
trait WideZ { fn encode_wide_z(&self) -> Vec<u16>; }
impl WideZ for std::ffi::OsStr { fn encode_wide_z(&self) -> Vec<u16> { use std::os::windows::ffi::OsStrExt; self.encode_wide().chain(Some(0)).collect() } }

pub fn exe_dir() -> PathBuf { std::env::current_exe().unwrap().parent().unwrap().to_path_buf() }

pub fn set_vulkan_layer(enable: bool) -> Result<()> {
    let json = exe_dir().join("flowscale_vk_layer.json");
    let key_path = w!("Software\\Khronos\\Vulkan\\ImplicitLayers");
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(HKEY_CURRENT_USER, key_path, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None).ok()?;
        let name = HSTRING::from(json.as_os_str());
        let r = if enable {
            let body = r#"{ "file_format_version": "1.1.2", "layer": { "name": "VK_LAYER_FLOWSCALE_frame_limiter", "type": "GLOBAL",
  "library_path": ".\\flowscale_hook.dll", "api_version": "1.3.0", "implementation_version": "2",
  "description": "FlowScale in-game frame limiter (idle unless FlowScale targets the process)",
  "functions": { "vkNegotiateLoaderLayerInterfaceVersion": "vkNegotiateLoaderLayerInterfaceVersion" },
  "disable_environment": { "DISABLE_FLOWSCALE_LAYER": "1" } } }"#;
            let _ = std::fs::write(&json, body);
            let zero = 0u32.to_le_bytes();
            RegSetValueExW(key, &name, None, REG_DWORD, Some(&zero)).ok()
        } else {
            let _ = RegDeleteValueW(key, &name); Ok(())
        };
        let _ = RegCloseKey(key);
        r
    }
}
pub fn vulkan_layer_installed() -> bool {
    let json = exe_dir().join("flowscale_vk_layer.json");
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Khronos\\Vulkan\\ImplicitLayers"), None, KEY_QUERY_VALUE, &mut key).is_err() { return false; }
        let r = RegQueryValueExW(key, &HSTRING::from(json.as_os_str()), None, None, None, None).is_ok();
        let _ = RegCloseKey(key); r
    }
}

pub fn rtss_dir() -> Option<PathBuf> {
    for sub in [w!("SOFTWARE\\WOW6432Node\\Unwinder\\RTSS"), w!("SOFTWARE\\Unwinder\\RTSS")] {
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, sub, None, KEY_QUERY_VALUE, &mut key).is_err() { continue; }
            let mut buf = [0u16; 520]; let mut n = (buf.len() * 2) as u32;
            let r = RegQueryValueExW(key, w!("InstallDir"), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut n));
            let _ = RegCloseKey(key);
            if r.is_ok() {
                let len = (n as usize / 2).min(buf.len());
                let d = PathBuf::from(String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0'));
                if d.join("RTSS.exe").exists() { return Some(d); }
            }
        }
    }
    None
}
fn rtss_profile(d: &Path) -> PathBuf { d.join("Profiles").join("flowscale.exe.cfg") }
pub fn rtss_needs_fix() -> bool {
    let Some(d) = rtss_dir() else { return false };
    let txt = std::fs::read_to_string(rtss_profile(&d)).unwrap_or_default().replace([' ', '\t'], "");
    !txt.lines().any(|l| l == "EnableHooking=0")
}
pub fn rtss_fix() -> std::io::Result<()> {
    let d = rtss_dir().ok_or_else(|| std::io::Error::other("RTSS not installed"))?;
    std::fs::write(rtss_profile(&d), "[Hooking]\r\nEnableHooking=0\r\n")
}
pub fn rtss_hooked_here() -> bool { unsafe { GetModuleHandleW(w!("RTSSHooks64.dll")).is_ok() || GetModuleHandleW(w!("RTSSHooks.dll")).is_ok() } }

pub struct Limiter { map: ipc::Mapping, cursor: u32, pub stats: FrameStats, pub injected: HashSet<u32>, pub recent: Vec<f32> }
impl Limiter {
    pub fn new() -> Option<Limiter> { Some(Limiter { map: ipc::Mapping::create()?, cursor: 0, stats: FrameStats::new(4096), injected: HashSet::new(), recent: vec![] }) }
    pub fn shared(&self) -> &Shared { self.map.get() }
    pub fn configure(&self, enabled: bool, milli_fps: u32, mode: u32) {
        let s = self.shared(); s.enabled.store(enabled as u32, Relaxed); s.milli_fps.store(milli_fps, Relaxed); s.mode.store(mode, Relaxed);
    }
    pub fn target(&self) -> u32 { self.shared().target_pid.load(Relaxed) }
    pub fn attach(&mut self, pid: u32, info: &ProcInfo) -> std::result::Result<(), String> {
        // Errors are codes ("code" or "code:detail"); the UI words them.
        if let Some(ac) = &info.anticheat { return Err(format!("anticheat:{ac}")); }
        if self.shared().target_pid.load(Relaxed) != pid {
            self.shared().target_pid.store(pid, Relaxed);
            self.stats = FrameStats::new(4096);
            self.cursor = self.shared().write_idx.load(Relaxed);
        }
        if self.injected.contains(&pid) { return Ok(()); }
        let r = if info.is32 {
            let helper = exe_dir().join("flowscale32.exe");
            let dll = exe_dir().join("flowscale_hook32.dll");
            if !helper.exists() || !dll.exists() { return Err("missing32".into()); }
            std::process::Command::new(helper).arg("-inject").arg(pid.to_string()).arg(&dll).status()
                .map_err(|e| format!("inject:{e}")).and_then(|st| if st.success() { Ok(()) } else { Err("inject32".into()) })
        } else {
            inject(pid, &exe_dir().join("flowscale_hook.dll")).map_err(|e| {
                if e.code() == E_ACCESSDENIED { "elevated".to_string() } else { format!("inject:{}", e.message()) }
            })
        };
        if r.is_ok() { self.injected.insert(pid); }
        r
    }
    pub fn detach(&self) { self.shared().target_pid.store(0, Relaxed); }
    pub fn poll(&mut self) {
        self.recent.clear();
        let mut v = vec![];
        self.cursor = self.shared().read_since(self.cursor, &mut v);
        for x in &v { self.stats.push(*x); }
        self.recent = v;
    }
    pub fn alive(&self) -> bool {
        let hb = self.shared().heartbeat.load(Relaxed) as i64;
        hb != 0 && crate::pacer::qpc() - hb < crate::pacer::qpf() * 2 && self.shared().attached_pid.load(Relaxed) == self.target()
    }
    pub fn api_name(&self) -> &'static str {
        match self.shared().api.load(Relaxed) { ipc::API_D3D11 => "D3D11", ipc::API_D3D12 => "D3D12", ipc::API_D3D9 => "D3D9", ipc::API_GL => "OpenGL", ipc::API_VK => "Vulkan", _ => "—" }
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None).is_err() { return; }
        if on {
            let cmd = format!("\"{}\" -minimized", std::env::current_exe().unwrap().display());
            let wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            let _ = RegSetValueExW(key, w!("FlowScale"), None, REG_SZ, Some(bytes));
        } else { let _ = RegDeleteValueW(key, w!("FlowScale")); }
        let _ = RegCloseKey(key);
    }
}
pub fn is_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    unsafe {
        let mut tok = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() { return false; }
        let mut e = TOKEN_ELEVATION::default(); let mut n = 0u32;
        let ok = GetTokenInformation(tok, TokenElevation, Some(&mut e as *mut _ as *mut c_void), size_of::<TOKEN_ELEVATION>() as u32, &mut n).is_ok();
        let _ = CloseHandle(tok);
        ok && e.TokenIsElevated != 0
    }
}
