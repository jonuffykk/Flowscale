//! Game process inspection, DLL injection / unload and the app side of the frame limiter.
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use windows::core::{s, w, Result, BOOL, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, LoadLibraryW, DONT_RESOLVE_DLL_REFERENCES};
use windows::Win32::System::Memory::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ipc::{self, Shared};
use crate::pacer::FrameStats;

pub const GFX_MODULES: &[(&str, &str)] = &[("d3d12.dll", "D3D12"), ("d3d11.dll", "D3D11"), ("vulkan-1.dll", "Vulkan"), ("d3d9.dll", "D3D9"), ("opengl32.dll", "OpenGL"),
    ("d3d10.dll", "D3D10"), ("d3d10_1.dll", "D3D10"), ("d3d8.dll", "D3D8"), ("ddraw.dll", "DirectDraw"), ("dxgi.dll", "DXGI")];

pub use crate::detect::{ignored, ANTICHEAT_MODULES, OVERLAY_MODULES};

pub struct ProcInfo {
    pub exe: String, pub path: String, pub is32: bool, pub gfx: Vec<&'static str>, pub anticheat: Option<String>, pub hooked: bool,
    pub modules: Vec<String>, pub overlays: Vec<&'static str>,
    /// Store / Game Pass apps run in an AppContainer: our DLL needs an ACL entry to be loadable there.
    pub packaged: bool,
    /// Process mitigations that make injection impossible or partial.
    pub signed_only: bool, pub no_dynamic_code: bool,
}

pub fn inspect(pid: u32) -> Option<ProcInfo> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = [0u16; 1024]; let mut n = buf.len() as u32;
        let path = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok() { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
        let exe = path.rsplit('\\').next().unwrap_or("").to_lowercase();
        let mut wow = BOOL(0); let _ = IsWow64Process(h, &mut wow);
        let mut mods = vec![HMODULE::default(); 4096]; let mut need = 0u32;
        let mut names = vec![];
        let mut paths: Vec<String> = vec![];
        if EnumProcessModulesEx(h, mods.as_mut_ptr(), (mods.len() * size_of::<HMODULE>()) as u32, &mut need, LIST_MODULES_ALL).is_ok() {
            for m in &mods[..(need as usize / size_of::<HMODULE>()).min(mods.len())] {
                let mut nb = [0u16; 260];
                let l = GetModuleBaseNameW(h, Some(*m), &mut nb);
                names.push(String::from_utf16_lossy(&nb[..l as usize]).to_lowercase());
                if String::from_utf16_lossy(&nb[..l as usize]).to_lowercase().starts_with("flowscale_hook") {
                    let mut pb = [0u16; 1024]; let pl = GetModuleFileNameExW(Some(h), Some(*m), &mut pb);
                    paths.push(String::from_utf16_lossy(&pb[..pl as usize]).to_lowercase());
                }
            }
        }
        // Mitigation policies: "Microsoft-signed only" blocks our DLL outright; "no dynamic code" blocks inline detours.
        let mut sig = 0u32; let mut dynp = 0u32;
        let _ = GetProcessMitigationPolicy(h, ProcessSignaturePolicy, &mut sig as *mut _ as *mut c_void, 4);
        let _ = GetProcessMitigationPolicy(h, ProcessDynamicCodePolicy, &mut dynp as *mut _ as *mut c_void, 4);
        let mut packaged = path.to_lowercase().contains("\\windowsapps\\");
        let mut tok = HANDLE::default();
        if OpenProcessToken(h, TOKEN_QUERY, &mut tok).is_ok() {
            let mut v = 0u32; let mut len = 0u32;
            if GetTokenInformation(tok, TokenIsAppContainer, Some(&mut v as *mut _ as *mut c_void), 4, &mut len).is_ok() && v != 0 { packaged = true; }
            let _ = CloseHandle(tok);
        }
        let _ = CloseHandle(h);
        let gfx = GFX_MODULES.iter().filter(|(m, _)| names.iter().any(|n| n == m)).map(|(_, a)| *a).collect();
        let anticheat = names.iter().find(|n| ANTICHEAT_MODULES.iter().any(|a| n.contains(a))).cloned()
            .or_else(|| crate::detect::blocked_title(&exe).then(|| format!("{exe} (protected title)")));
        let ours = [full(&exe_dir().join("flowscale_hook.dll")).to_lowercase(), full(&exe_dir().join("flowscale_hook32.dll")).to_lowercase()];
        let hooked = paths.iter().any(|p| ours.contains(p));
        let overlays = OVERLAY_MODULES.iter().filter(|(m, _)| names.iter().any(|n| n == m)).map(|(_, o)| *o).fold(vec![], |mut v, o| { if !v.contains(&o) { v.push(o); } v });
        Some(ProcInfo { exe, path, is32: wow.as_bool(), gfx, anticheat, hooked, modules: names, overlays, packaged, signed_only: sig & 0b11 != 0, no_dynamic_code: dynp & 1 != 0 })
    }
}

/// Lets AppContainer processes (Store / Game Pass games) load our DLL: read + execute for
/// "ALL APPLICATION PACKAGES" (S-1-15-2-1) and "ALL RESTRICTED APPLICATION PACKAGES" (S-1-15-2-2).
pub fn allow_app_containers(dll: &Path) -> Result<()> {
    unsafe {
        let path = HSTRING::from(dll.as_os_str());
        let mut old: *mut ACL = std::ptr::null_mut(); let mut sd = PSECURITY_DESCRIPTOR::default();
        GetNamedSecurityInfoW(&path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(&mut old), None, &mut sd).ok()?;
        let mut sids = [PSID::default(); 2];
        for (i, s) in [w!("S-1-15-2-1"), w!("S-1-15-2-2")].iter().enumerate() { let _ = ConvertStringSidToSidW(*s, &mut sids[i]); }
        let entries: Vec<EXPLICIT_ACCESS_W> = sids.iter().filter(|s| !s.0.is_null()).map(|s| EXPLICIT_ACCESS_W {
            grfAccessPermissions: 0x0012_0089 | 0x0012_00A0 /* FILE_GENERIC_READ | FILE_GENERIC_EXECUTE */, grfAccessMode: GRANT_ACCESS, grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W { TrusteeForm: TRUSTEE_IS_SID, TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP, ptstrName: PWSTR(s.0 as *mut u16), ..Default::default() },
        }).collect();
        let mut new: *mut ACL = std::ptr::null_mut();
        let r = SetEntriesInAclW(Some(&entries), Some(old), &mut new);
        let out = if r.is_ok() { SetNamedSecurityInfoW(&path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(new), None).ok() } else { r.ok() };
        for s in sids { if !s.0.is_null() { let _ = LocalFree(Some(HLOCAL(s.0))); } }
        if !new.is_null() { let _ = LocalFree(Some(HLOCAL(new as *mut c_void))); }
        if !sd.0.is_null() { let _ = LocalFree(Some(HLOCAL(sd.0))); }
        out
    }
}

/// How the DLL entered the game: decides how it is released again.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Method { RemoteThread, WindowHook }

/// Classic path: LoadLibraryW on a remote thread, then fs_attach. Falls back to a WH_GETMESSAGE hook on the game's
/// UI thread when remote threads are refused (some launchers and protections), or when FS_INJECT=hook is set.
pub fn inject(pid: u32, dll: &Path) -> std::result::Result<Method, windows::core::Error> {
    if std::env::var("FS_INJECT").as_deref() == Ok("hook") { return inject_hook(pid, dll).map(|_| Method::WindowHook); }
    match inject_remote(pid, dll) {
        Ok(()) => Ok(Method::RemoteThread),
        Err(e) if e.code() == E_ACCESSDENIED && !is_elevated_target(pid) => inject_hook(pid, dll).map(|_| Method::WindowHook).map_err(|_| e),
        Err(e) if e.code() == E_ACCESSDENIED => Err(e),
        Err(e) => inject_hook(pid, dll).map(|_| Method::WindowHook).map_err(|_| e),
    }
}
fn inject_remote(pid: u32, dll: &Path) -> Result<()> {
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
/// The UI thread of the game's main window (needed by the window-hook injection).
fn ui_thread(pid: u32) -> Option<u32> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let st = unsafe { &mut *(l.0 as *mut (u32, u32, i64)) };
        let mut p = 0u32; let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        if p == st.0 && unsafe { IsWindowVisible(h).as_bool() } {
            let mut r = RECT::default(); let _ = unsafe { GetWindowRect(h, &mut r) };
            let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
            if area > st.2 { st.1 = tid; st.2 = area; }
        }
        true.into()
    }
    let mut st = (pid, 0u32, -1i64);
    unsafe { let _ = EnumWindows(Some(cb), LPARAM(&mut st as *mut _ as isize)); }
    (st.1 != 0).then_some(st.1)
}
fn inject_hook(pid: u32, dll: &Path) -> Result<()> {
    unsafe {
        let tid = ui_thread(pid).ok_or_else(|| windows::core::Error::new(E_FAIL, "no game window for the window-hook injection"))?;
        let local = LoadLibraryW(&HSTRING::from(dll.as_os_str()))?;
        let proc_ = GetProcAddress(local, s!("GetMessageProc")).ok_or_else(|| windows::core::Error::new(E_FAIL, "GetMessageProc export missing"))?;
        let hk = SetWindowsHookExW(WH_GETMESSAGE, Some(std::mem::transmute::<_, HookProcFn>(proc_)), Some(local.into()), tid);
        let r = match hk {
            Ok(hk) => {
                let name = full(dll);
                let mut loaded = false;
                for _ in 0..100 {
                    let _ = PostThreadMessageW(tid, WM_NULL, WPARAM(0), LPARAM(0));
                    if module_base(pid, &name).is_some() { loaded = true; break; }
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                // Give the DLL time to take its own module reference before Windows drops the hook's.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = UnhookWindowsHookEx(hk);
                if loaded { Ok(()) } else { Err(windows::core::Error::new(E_FAIL, "the game did not load the hook DLL")) }
            }
            Err(e) => Err(e),
        };
        let _ = FreeLibrary(local);
        r
    }
}
type HookProcFn = unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT;

/// Base address of a module inside another process. `name` is either a lowercase file name or, when it contains a
/// backslash, a full path: two copies of flowscale_hook.dll from different folders can be loaded at once (an old
/// Vulkan layer registration), and the injector must always talk to the exact file it loaded.
pub fn module_base(pid: u32, name: &str) -> Option<usize> {
    let by_path = name.contains('\\');
    let want = name.to_lowercase();
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut mods = vec![HMODULE::default(); 4096]; let mut need = 0u32;
        let r = if EnumProcessModulesEx(h, mods.as_mut_ptr(), (mods.len() * size_of::<HMODULE>()) as u32, &mut need, LIST_MODULES_ALL).is_ok() {
            mods[..(need as usize / size_of::<HMODULE>()).min(mods.len())].iter().find(|m| {
                let mut nb = [0u16; 1024];
                let l = if by_path { GetModuleFileNameExW(Some(h), Some(**m), &mut nb) } else { GetModuleBaseNameW(h, Some(**m), &mut nb) };
                String::from_utf16_lossy(&nb[..l as usize]).to_lowercase() == want
            }).map(|m| m.0 as usize)
        } else { None };
        let _ = CloseHandle(h);
        r
    }
}
fn full(dll: &Path) -> String { std::fs::canonicalize(dll).map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string()).unwrap_or_else(|_| dll.to_string_lossy().to_string()) }
/// Releases the injector's LoadLibrary reference with a remote FreeLibrary (same bitness only).
pub fn free_remote(pid: u32, name: &str) -> Result<()> {
    unsafe {
        let base = module_base(pid, name).ok_or_else(|| windows::core::Error::new(E_FAIL, "not loaded"))?;
        let h = OpenProcess(PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_READ, false, pid)?;
        let k32 = GetModuleHandleW(w!("kernel32.dll"))?;
        let fl = GetProcAddress(k32, s!("FreeLibrary")).ok_or_else(windows::core::Error::from_thread)?;
        let r = CreateRemoteThread(h, None, 0, Some(std::mem::transmute::<_, unsafe extern "system" fn(*mut c_void) -> u32>(fl)), Some(base as *const c_void), 0, None)
            .map(|th| { WaitForSingleObject(th, 5000); let _ = CloseHandle(th); });
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
        let _ = name;
        let pid = GetProcessId(h);
        let base = module_base(pid, &full(dll)).ok_or_else(|| windows::core::Error::new(E_FAIL, "hook module not found in game"))?;
        let entry: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(base + rva);
        let th = CreateRemoteThread(h, None, 0, Some(entry), None, 0, None)?;
        WaitForSingleObject(th, 8000);
        let _ = CloseHandle(th);
        Ok(())
    }
}
/// True when the target runs elevated and we do not (only then is the remote-thread refusal about elevation).
pub fn is_elevated_target(pid: u32) -> bool {
    if is_elevated() { return false; }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
        let mut tok = HANDLE::default();
        let r = if OpenProcessToken(h, TOKEN_QUERY, &mut tok).is_ok() {
            let mut e = TOKEN_ELEVATION::default(); let mut n = 0u32;
            let ok = GetTokenInformation(tok, TokenElevation, Some(&mut e as *mut _ as *mut c_void), size_of::<TOKEN_ELEVATION>() as u32, &mut n).is_ok() && e.TokenIsElevated != 0;
            let _ = CloseHandle(tok); ok
        } else { true };
        let _ = CloseHandle(h);
        r
    }
}
trait WideZ { fn encode_wide_z(&self) -> Vec<u16>; }
impl WideZ for std::ffi::OsStr { fn encode_wide_z(&self) -> Vec<u16> { use std::os::windows::ffi::OsStrExt; self.encode_wide().chain(Some(0)).collect() } }

/// Human name for a process exit code (crashes show up as NTSTATUS values).
pub fn exit_name(code: u32) -> &'static str {
    match code {
        0 => "success", 0xC0000005 => "access_violation", 0xC00000FD => "stack_overflow", 0xC0000374 => "heap_corruption", 0xC0000409 => "stack_buffer_overrun",
        0xC000001D => "illegal_instruction", 0xC0000094 => "integer_divide_by_zero", 0x80000003 => "breakpoint", 0xC0000135 => "dll_not_found",
        0xC0000139 => "entry_point_not_found", 0x40000015 => "fatal_app_exit", 0xC000013A => "closed_by_ctrl_c", 1 => "error", _ => "nonzero",
    }
}
pub fn exit_is_crash(code: u32) -> bool { code & 0xC000_0000 == 0xC000_0000 && code != 0xC000013A }

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

/// Pacing quality beyond frame times, read from the DLL's counters.
#[derive(Default, Clone, Copy)]
pub struct Metrics {
    pub wake_avg_ms: f32, pub wake_max_ms: f32, pub limited_pct: f32, pub late: u32,
    pub p95_err_ms: f32, pub p99_err_ms: f32,
    /// QPC against the system tick clock over the session (a drifting QPC makes every deadline drift too).
    pub drift_ppm: f32, pub hooks: u32, pub arch: u32,
}

pub struct Limiter {
    map: ipc::Mapping, cursor: u32, pub stats: FrameStats, injected: HashMap<u32, Method>, pub recent: Vec<f32>,
    /// Handle to the target (exit tracking), its bitness, and the clocks at attach (drift).
    proc_: Option<(u32, HANDLE)>, is32: bool, clock0: Option<(i64, u64)>,
}
// The process handle is only used from behind the app's limiter mutex.
unsafe impl Send for Limiter {}
impl Drop for Limiter { fn drop(&mut self) { if let Some((_, h)) = self.proc_.take() { unsafe { let _ = CloseHandle(h); } } } }

impl Limiter {
    pub fn new() -> Option<Limiter> {
        Some(Limiter { map: ipc::Mapping::create()?, cursor: 0, stats: FrameStats::new(4096), injected: HashMap::new(), recent: vec![], proc_: None, is32: false, clock0: None })
    }
    pub fn shared(&self) -> &Shared { self.map.get() }
    /// The mapping lives as long as the limiter; a detached reference lets methods also mutate `self`.
    fn sh(&self) -> &'static Shared { unsafe { &*self.map.ptr } }
    pub fn configure(&self, enabled: bool, milli_fps: u32, mode: u32) {
        let s = self.shared(); s.enabled.store(enabled as u32, Relaxed); s.milli_fps.store(milli_fps, Relaxed); s.mode.store(mode, Relaxed);
    }
    pub fn target(&self) -> u32 { self.shared().target_pid.load(Relaxed) }
    fn hook_dll(is32: bool) -> PathBuf { exe_dir().join(if is32 { "flowscale_hook32.dll" } else { "flowscale_hook.dll" }) }

    /// Errors are codes ("code" or "code:detail"); the UI words them.
    pub fn attach(&mut self, pid: u32, info: &ProcInfo) -> std::result::Result<(), String> {
        if let Some(ac) = &info.anticheat { return Err(format!("anticheat:{ac}")); }
        if info.signed_only { return Err("protected".into()); }
        if self.target() != pid {
            let s = self.sh();
            s.target_pid.store(pid, Relaxed); s.reset_stats();
            self.stats = FrameStats::new(4096);
            self.cursor = s.write_idx.load(Relaxed);
            if let Some((_, h)) = self.proc_.take() { unsafe { let _ = CloseHandle(h); } }
            self.proc_ = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok() }.map(|h| (pid, h));
            self.is32 = info.is32;
            self.clock0 = Some((crate::pacer::qpc(), unsafe { GetTickCount64() }));
        }
        if self.injected.contains_key(&pid) && info.hooked { return Ok(()); }
        let dll = Self::hook_dll(info.is32);
        if !dll.exists() { return Err(if info.is32 { "missing32".into() } else { "inject:flowscale_hook.dll is missing".into() }); }
        if info.packaged { if let Err(e) = allow_app_containers(&dll) { crate::log(&format!("appcontainer acl: {e:?}")); } }
        let r = if info.is32 {
            let helper = exe_dir().join("flowscale32.exe");
            if !helper.exists() { return Err("missing32".into()); }
            std::process::Command::new(helper).arg("-inject").arg(pid.to_string()).arg(&dll).status()
                .map_err(|e| format!("inject:{e}"))
                .and_then(|st| match st.code() { Some(0) => Ok(Method::RemoteThread), Some(2) => Ok(Method::WindowHook), Some(5) => Err("elevated".into()), _ => Err("inject32".into()) })
        } else {
            inject(pid, &dll).map_err(|e| if e.code() == E_ACCESSDENIED { "elevated".to_string() } else { format!("inject:{}", e.message()) })
        };
        match r { Ok(m) => { crate::log(&format!("injected {} ({m:?})", info.exe)); self.injected.insert(pid, m); Ok(()) } Err(e) => Err(e) }
    }

    /// Records an injection done by a separate (elevated) helper.
    pub fn adopt(&mut self, pid: u32, m: Method) { self.injected.insert(pid, m); }
    /// Stops limiting and takes the DLL back out of the game. Returns an ipc::DETACH_* state.
    pub fn detach(&mut self) -> u32 {
        let pid = self.target();
        let s = self.sh();
        s.target_pid.store(0, Relaxed);
        if let Some((_, h)) = self.proc_.take() { unsafe { let _ = CloseHandle(h); } }
        let Some(method) = self.injected.remove(&pid) else { return ipc::DETACH_NONE };
        let path = full(&Self::hook_dll(self.is32));
        let name = path.as_str();
        if module_base(pid, name).is_none() { return ipc::DETACH_UNLOADED; }
        s.detach_state.store(ipc::DETACH_NONE, Relaxed);
        s.detach_pid.store(pid, Relaxed);
        let mut state = ipc::DETACH_NONE;
        for _ in 0..150 { state = s.detach_state.load(Relaxed); if state != ipc::DETACH_NONE { break; } std::thread::sleep(std::time::Duration::from_millis(20)); }
        s.detach_pid.store(0, Relaxed);
        if state == ipc::DETACH_NONE || state == ipc::DETACH_FAILED { crate::log(&format!("detach {pid}: the DLL did not confirm ({state})")); return ipc::DETACH_FAILED; }
        std::thread::sleep(std::time::Duration::from_millis(50)); // the DLL's worker thread leaves the module
        // Resident means the game still holds pointers into the DLL: freeing it now would crash the game.
        if method == Method::RemoteThread && state == ipc::DETACH_UNLOADED {
            let r = if self.is32 {
                std::process::Command::new(exe_dir().join("flowscale32.exe")).arg("-free").arg(pid.to_string()).arg(&path).status().map(|st| st.success()).unwrap_or(false)
            } else { free_remote(pid, name).is_ok() };
            if !r { crate::log(&format!("detach {pid}: remote FreeLibrary failed")); }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let gone = module_base(pid, name).is_none();
        crate::log(&format!("detach {pid}: hooks removed, module {}", if gone { "unloaded" } else { "resident" }));
        if gone { ipc::DETACH_UNLOADED } else { ipc::DETACH_RESIDENT }
    }

    pub fn poll(&mut self) {
        self.recent.clear();
        let mut v = vec![];
        self.cursor = self.shared().read_since(self.cursor, &mut v);
        for x in &v { self.stats.push(*x); }
        self.recent = v;
    }
    /// The limited game exited: (pid, exit code). Clears the target.
    pub fn take_exit(&mut self) -> Option<(u32, u32)> {
        let (pid, h) = self.proc_?;
        if unsafe { WaitForSingleObject(h, 0) } != WAIT_OBJECT_0 { return None; }
        let mut code = 0u32; unsafe { let _ = GetExitCodeProcess(h, &mut code); let _ = CloseHandle(h); }
        self.proc_ = None;
        self.injected.remove(&pid);
        if self.target() == pid { self.shared().target_pid.store(0, Relaxed); }
        Some((pid, code))
    }
    pub fn alive(&self) -> bool {
        let hb = self.shared().heartbeat.load(Relaxed) as i64;
        hb != 0 && crate::pacer::qpc() - hb < crate::pacer::qpf() * 2 && self.shared().attached_pid.load(Relaxed) == self.target()
    }
    pub fn api_name(&self) -> &'static str {
        match self.shared().api.load(Relaxed) {
            ipc::API_D3D11 => "D3D11", ipc::API_D3D12 => "D3D12", ipc::API_D3D9 => "D3D9", ipc::API_GL => "OpenGL", ipc::API_VK => "Vulkan",
            ipc::API_D3D8 => "D3D8", ipc::API_DDRAW => "DirectDraw", _ => "—",
        }
    }
    pub fn metrics(&self) -> Metrics {
        let s = self.shared();
        let wn = s.wake_n.load(Relaxed).max(1) as f64;
        let paced = s.paced.load(Relaxed);
        let cap = s.milli_fps.load(Relaxed) as f32 / 1000.0;
        let (p95, p99) = if cap > 0.0 && s.enabled.load(Relaxed) != 0 { self.stats.errors(1000.0 / cap) } else { (0.0, 0.0) };
        let drift = self.clock0.map(|(q0, t0)| {
            let dq = (crate::pacer::qpc() - q0) as f64 / crate::pacer::qpf() as f64 * 1000.0;
            let dt = (unsafe { GetTickCount64() } - t0) as f64;
            // Tick resolution is ~16 ms: only meaningful after a minute or so.
            if dt > 60_000.0 { ((dq - dt) / dt * 1e6) as f32 } else { 0.0 }
        }).unwrap_or(0.0);
        Metrics {
            wake_avg_ms: (s.wake_us_sum.load(Relaxed) as f64 / wn / 1000.0) as f32, wake_max_ms: s.wake_us_max.load(Relaxed) as f32 / 1000.0,
            limited_pct: if paced > 0 { s.limited.load(Relaxed) as f32 * 100.0 / paced as f32 } else { 0.0 }, late: s.late_frames.load(Relaxed),
            p95_err_ms: p95, p99_err_ms: p99, drift_ppm: drift, hooks: s.hooks.load(Relaxed), arch: s.arch_bits.load(Relaxed),
        }
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
