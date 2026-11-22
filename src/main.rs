#![windows_subsystem = "windows"]
mod a4k_passes;
mod config;
mod detect;
mod engine;
mod gsync;
mod ingame;
#[allow(dead_code)] mod ipc;
mod limiter;
mod logzip;
mod watchdog;
mod osd;
#[allow(dead_code)] mod pacer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::{ffi::c_void, mem::size_of};

use serde_json::{json, Value};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{Scope, Store};

pub const WM_STATUS: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_DONE: u32 = WM_APP + 3;
const WM_AUTOSCALE: u32 = WM_APP + 10;
const WM_GAMES: u32 = WM_APP + 11;
const WM_LIMITED: u32 = WM_APP + 12;
const WM_FOLLOW: u32 = WM_APP + 13;
const WM_DETECTOR: u32 = WM_APP + 14;
const HOTKEYS: &[(i32, &str)] = &[(1, "hk_scale"), (2, "hk_limit"), (3, "hk_osd")];
const T_TELE: usize = 1; const T_DETECT: usize = 2; const T_COUNT: usize = 3; const T_RESTART: usize = 4;
const CMD_EXIT: usize = 9001; const CMD_OPEN: usize = 9002; const CMD_CAP_OFF: usize = 9003; const CMD_CAP: usize = 9100;
const TRAY_CAPS: &[u32] = &[30, 40, 45, 60, 72, 90, 120, 144, 165, 240];
fn taskbar_created() -> u32 { static M: OnceLock<u32> = OnceLock::new(); *M.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }) }
const UI_HTML: &str = include_str!("../ui/dist/index.html");

// ------------------------------------------------------------------ small Win32 helpers

pub fn log(s: &str) {
    use std::io::Write;
    let p = limiter::exe_dir().join("flowscale.log");
    if std::fs::metadata(&p).map(|m| m.len() > 4 << 20).unwrap_or(false) { logzip::rotate(&p); }
    // One write per line: several threads log at once and must not interleave.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = f.write_all(format!("{s}\r\n").as_bytes()); }
}
pub fn window_title(h: HWND) -> String { let mut b = [0u16; 256]; let n = unsafe { GetWindowTextW(h, &mut b) }; String::from_utf16_lossy(&b[..n.max(0) as usize]) }
fn pid_of(h: HWND) -> u32 { let mut p = 0; unsafe { GetWindowThreadProcessId(h, Some(&mut p)); } p }
fn exe_of_pid(pid: u32) -> Option<String> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512]; let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(p); r.ok()?;
        Some(String::from_utf16_lossy(&buf[..n as usize]).rsplit('\\').next()?.to_lowercase())
    }
}
fn exe_of(h: HWND) -> Option<String> { exe_of_pid(pid_of(h)) }
fn path_of_pid(pid: u32) -> String {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024]; let mut n = buf.len() as u32;
        let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n);
        let _ = CloseHandle(p);
        if r.is_ok() { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() }
    }
}
/// Everything a profile can match on: exe name, full path and window title.
fn target_of(h: HWND) -> config::Target {
    let pid = pid_of(h);
    config::Target { exe: exe_of_pid(pid).unwrap_or_default(), path: path_of_pid(pid), title: window_title(h) }
}
/// The processes above `pid` (parent, grandparent, ...), up to 4, stopping at the shell and system processes.
fn ancestors(pid: u32) -> Vec<(u32, config::Target)> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let mut parent: std::collections::HashMap<u32, (u32, String)> = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return vec![] };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            parent.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase()));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let mut out = vec![]; let mut cur = pid;
    for _ in 0..4 {
        let Some(&(pp, _)) = parent.get(&cur) else { break };
        let Some((_, exe)) = parent.get(&pp) else { break };
        if pp <= 4 || pp == cur || ["explorer.exe", "services.exe", "svchost.exe", "wininit.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "windowsterminal.exe", "flowscale.exe"].contains(&exe.as_str()) { break; }
        out.push((pp, config::Target { exe: exe.clone(), path: path_of_pid(pp), title: String::new() }));
        cur = pp;
    }
    out
}
/// The profile for a window: its own match, else the first matching launcher above it (launcher -> game: a
/// profile made for the launcher follows the game it starts). The third value names that launcher.
fn profile_of(h: HWND) -> (String, config::Profile, Option<String>) {
    let t = target_of(h);
    let (n, p) = app().store.profile_for(&t);
    if n != "Default" { return (n, p, None); }
    for (_, a) in ancestors(pid_of(h)) {
        let (n2, p2) = app().store.profile_for(&a);
        if n2 != "Default" { return (n2, p2, Some(a.exe)); }
    }
    (n, p, None)
}
pub fn monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, l: LPARAM) -> BOOL { unsafe { (*(l.0 as *mut Vec<HMONITOR>)).push(m); } true.into() }
    let mut v: Vec<HMONITOR> = vec![];
    unsafe { let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize)); }
    v
}
pub fn refresh_of_monitor(m: HMONITOR) -> f32 {
    unsafe {
        let mut mi = MONITORINFOEXW::default(); mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(m, &mut mi as *mut _ as *mut MONITORINFO).as_bool() { return 60.0; }
        let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        if EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() && dm.dmDisplayFrequency > 1 { dm.dmDisplayFrequency as f32 } else { 60.0 }
    }
}
fn refresh_of(h: HWND) -> f32 { refresh_of_monitor(unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) }) }
fn os_lang() -> &'static str { if unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3ff == 0x16 { "pt" } else { "en" } }
fn gpu_names() -> Vec<String> {
    let mut v = vec!["auto".to_string()];
    unsafe {
        if let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0;
            while let Ok(a) = f.EnumAdapters1(i) {
                if let Ok(d) = a.GetDesc1() { if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 { let n = d.Description.iter().position(|&c| c == 0).unwrap_or(128); v.push(String::from_utf16_lossy(&d.Description[..n])); } }
                i += 1;
            }
        }
    }
    v
}
fn display_names() -> Vec<String> {
    let mut v = vec!["same".to_string()];
    for (i, m) in monitors().iter().enumerate() {
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe { let _ = GetMonitorInfoW(*m, &mut mi); }
        let r = mi.rcMonitor;
        v.push(format!("{} · {}×{} · {:.0} Hz", i + 1, r.right - r.left, r.bottom - r.top, refresh_of_monitor(*m)));
    }
    v
}
fn candidate_windows() -> Vec<(HWND, String, String)> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = unsafe { &mut *(l.0 as *mut Vec<HWND>) };
        unsafe { if IsWindowVisible(h).as_bool() && GetWindow(h, GW_OWNER).is_err() && GetWindowTextLengthW(h) > 0 { v.push(h); } }
        true.into()
    }
    let mut hs: Vec<HWND> = vec![];
    unsafe { let _ = EnumWindows(Some(cb), LPARAM(&mut hs as *mut _ as isize)); }
    let me = std::process::id();
    hs.into_iter().filter(|h| pid_of(*h) != me).filter_map(|h| { let e = exe_of(h)?; if limiter::ignored(&e) { None } else { Some((h, e, window_title(h))) } }).collect()
}

// ------------------------------------------------------------------ app state

struct App {
    hwnd: HWND, store: Store, current: String,
    stop: Option<Arc<AtomicBool>>, worker: Option<std::thread::JoinHandle<()>>, status: Arc<engine::Status>,
    countdown: i32, gpus: Vec<String>, displays: Vec<String>,
    webview: Option<ICoreWebView2>, controller: Option<ICoreWebView2Controller>,
    lim_exe: String, lim_status: (String, Vec<String>), detector: (String, Vec<String>), scaling_game: isize, lim_busy: bool, last_fg_pid: u32, fg_since: u32, scaling_exe: String,
    games: Vec<ingame::Game>, games_busy: bool, games_msg: (String, Vec<String>), session: u64,
    /// (pid, exe) of the scaled game, kept while "follow the game window" is on.
    follow: Option<(u32, String)>,
    /// Limited game: overlays found inside it, bitness, and a pending per-game administrator request.
    lim_overlays: Vec<String>, lim_arch: u32, lim_admin: Option<(u32, bool, u32, u32, String)>,
}
unsafe impl Send for App {}
static APP: OnceLock<Mutex<App>> = OnceLock::new();
/// The limiter has its own lock: injection can take seconds and must never block the UI thread.
static LIM: OnceLock<Mutex<Option<limiter::Limiter>>> = OnceLock::new();
fn app() -> MutexGuard<'static, App> { APP.get().unwrap().lock().unwrap_or_else(|e| e.into_inner()) }
fn lim() -> MutexGuard<'static, Option<limiter::Limiter>> { LIM.get().unwrap().lock().unwrap_or_else(|e| e.into_inner()) }
fn ui_hwnd() -> HWND { app().hwnd }
fn post_ui(msg: u32, wp: usize) { let h = ui_hwnd(); unsafe { let _ = PostMessageW(Some(h), msg, WPARAM(wp), LPARAM(0)); } }

fn post(msg: &Value) {
    let wv = app().webview.clone();
    if let Some(wv) = wv { let s = HSTRING::from(msg.to_string()); unsafe { let _ = wv.PostWebMessageAsJson(&s); } }
}
/// Status messages are codes + arguments; the UI owns the wording in each language.
fn say(code: &str, args: &[&str]) {
    log(&format!("{code} {}", args.join(" | ")));
    post(&json!({"t": "status", "code": code, "args": args}));
}
fn split_err(e: &str) -> (String, Vec<String>) {
    match e.split_once(':') { Some((c, a)) if !c.contains(' ') => (c.to_string(), vec![a.to_string()]), _ if !e.contains(' ') => (e.to_string(), vec![]), _ => ("error".into(), vec![e.to_string()]) }
}

fn sync_osd(s: &Store) { osd::configure(s.app("osd") != 0, s.app("osd_limiter") != 0, s.osd_style()); }
fn register_hotkeys(h: HWND) {
    let keys: Vec<(i32, i32)> = { let a = app(); HOTKEYS.iter().map(|(id, k)| (*id, a.store.app(k))).collect() };
    for (id, code) in keys {
        unsafe {
            let _ = UnregisterHotKey(Some(h), id);
            if code & 0xFF == 0 { continue; }
            let mods = HOT_KEY_MODIFIERS(((code >> 16) & 0xF) as u32) | MOD_NOREPEAT;
            if RegisterHotKey(Some(h), id, mods, (code & 0xFF) as u32).is_err() { say("hotkey_taken", &[HOTKEYS[(id - 1) as usize].1]); }
        }
    }
}

// ------------------------------------------------------------------ scaling

fn is_scaling() -> bool { app().worker.as_ref().map(|w| !w.is_finished()).unwrap_or(false) }
fn start_scaling(game: HWND) {
    if is_scaling() || game.is_invalid() || game == ui_hwnd() { return; }
    let target = target_of(game);
    let exe = target.exe.clone();
    // Any window can be scaled (browser games, emulators); only the shell and FlowScale itself are off limits.
    if ["explorer.exe", "dwm.exe", "flowscale.exe", "flowscale32.exe", "searchhost.exe", "startmenuexperiencehost.exe", "shellexperiencehost.exe"].contains(&exe.as_str()) { say("not_a_game", &[&exe]); return; }
    let (_, prof, via) = profile_of(game);
    if let Some(l) = &via { log(&format!("profile of {exe}: from its launcher {l}")); }
    let (settings, status, ui) = { let a = app(); (config::Settings::from(&prof), a.status.clone(), a.hwnd) };
    if settings.gsync { gsync::ensure(true); }
    let follow = settings.follow_window;
    let stop = Arc::new(AtomicBool::new(false)); let st = stop.clone();
    let session = { let mut a = app(); a.session += 1; a.session };
    let (gh, uh) = (game.0 as isize, ui.0 as isize);
    let worker = std::thread::spawn(move || {
        let (g, u) = (HWND(gh as *mut c_void), HWND(uh as *mut c_void));
        if let Err(e) = engine::run(g, settings, st, status.clone(), u) { log(&format!("engine error: {e:?}")); *status.text.lock().unwrap() = format!("error:{}", e.message()); }
        unsafe { let _ = PostMessageW(Some(u), WM_DONE, WPARAM(session as usize), LPARAM(0)); }
    });
    { let mut a = app(); a.stop = Some(stop); a.worker = Some(worker); a.scaling_exe = exe.clone(); a.scaling_game = game.0 as isize; a.follow = if follow { Some((pid_of(game), exe.clone())) } else { None }; }
    if config::get(&prof, "limiter") != 0 { apply_limiter(game, false); }
    push_state();
}
/// The game closed its window. Many games re-create it (mode switch, launcher -> game, renderer restart);
/// wait up to 10 s for a new top-level window of the same process (or the same exe) and scale it again.
fn follow_game((pid, exe): (u32, String)) {
    say("following", &[&exe]);
    std::thread::spawn(move || {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let found = candidate_windows().into_iter().find(|(h, e, _)| (pid_of(*h) == pid || *e == exe) && unsafe { !IsIconic(*h).as_bool() });
            if let Some((h, _, _)) = found { log(&format!("follow: {exe} re-created its window")); post_ui(WM_FOLLOW, h.0 as usize); return; }
        }
        log(&format!("follow: {exe} did not come back"));
    });
}
fn stop_scaling() {
    let (s, w) = { let mut a = app(); (a.stop.take(), a.worker.take()) };
    if let Some(s) = s { s.store(true, Ordering::Relaxed); }
    if let Some(w) = w {
        // Never hang the UI on the engine: wait up to 3 s, then leave it (it stops on its own, or with the process).
        let t0 = std::time::Instant::now();
        while !w.is_finished() && t0.elapsed() < std::time::Duration::from_secs(3) { std::thread::sleep(std::time::Duration::from_millis(10)); }
        if w.is_finished() { let _ = w.join(); } else { log("stop: the engine did not stop within 3 s; left to finish"); }
    }
    app().scaling_exe.clear();
    // Frame generation stopped with the scaling: the game's cap goes back to its rate without FG.
    reconfigure_limiter();
    push_state();
}

// ------------------------------------------------------------------ limiter

/// The game's frame cap in milli-FPS. A fixed rate is the game's own rate. The display modes (refresh, VRR -3%, half,
/// match) name what reaches the screen: while frame generation runs (`fg`), the game gets that rate divided by the
/// multiplier (fixed mode) or half the output (target mode), so FG output and VRR never fight the cap.
fn cap_milli_fps(prof: &config::Profile, game: HWND, fg: bool) -> u32 {
    let g = |k| config::get(prof, k);
    cap_from(g("cap_mode"), g("fps_cap"), refresh_of(game), if fg { g("framegen") } else { 0 }, g("multiplier"), g("target"))
}
fn cap_from(mode: i32, fixed_mfps: i32, refresh: f32, framegen: i32, multiplier: i32, target: i32) -> u32 {
    let display = match mode {
        1 | 4 => refresh,
        2 => (refresh * 0.97).floor(),
        3 => refresh / 2.0,
        _ => return (fixed_mfps as f32).clamp(1000.0, 1_000_000.0) as u32,
    };
    let game_fps = match framegen {
        1 => display / multiplier.max(1) as f32,
        2 => (target as f32).min(display) / 2.0,
        _ => display,
    };
    (game_fps * 1000.0).clamp(1000.0, 1_000_000.0) as u32
}
#[cfg(test)]
mod cap_tests {
    use super::cap_from;
    #[test] fn fixed_rate_is_the_games_rate() { assert_eq!(cap_from(0, 60_000, 180.0, 1, 3, 120), 60_000); }
    #[test] fn vrr_with_fg_divides_by_the_multiplier() { assert_eq!(cap_from(2, 0, 180.0, 1, 3, 120), 58_000); assert_eq!(cap_from(2, 0, 144.0, 1, 2, 120), 69_500); }
    #[test] fn vrr_without_fg_is_the_display() { assert_eq!(cap_from(2, 0, 180.0, 0, 3, 120), 174_000); }
    #[test] fn target_mode_halves_the_output() { assert_eq!(cap_from(1, 0, 180.0, 2, 2, 120), 60_000); }
}

/// Inject and configure on a worker thread; the result comes back as WM_LIMITED.
fn apply_limiter(game: HWND, force: bool) {
    let pid = pid_of(game);
    let target = target_of(game);
    let exe = target.exe.clone();
    let (_, mut p, _) = profile_of(game);
    let _ = &target;
    let (prof, ok) = { let mut a = app(); if a.lim_busy { return; } if force { config::set(&mut p, "limiter", 1); } let ok = config::get(&p, "limiter") != 0; if ok { a.lim_busy = true; } (p, ok) };
    if !ok { return; }
    let mfps = cap_milli_fps(&prof, game, is_scaling());
    let mode = config::get(&prof, "latency_mode") as u32;
    say("limiter_attaching", &[&exe]);
    std::thread::spawn(move || {
        let info = limiter::inspect(pid);
        let r = match &info {
            None => Err("no_access".to_string()),
            Some(info) => {
                let mut g = lim();
                match g.as_mut() { None => Err("ipc".to_string()), Some(l) => {
                    // Attach first: on failure target_pid still names the previous game, which must keep its own cap.
                    let r = l.attach(pid, info); if r.is_ok() { l.configure(true, mfps, mode); } r
                } }
            }
        };
        if let Some(i) = &info {
            log(&format!("limiter target {} pid {pid} {}bit gfx={:?} overlays={:?} packaged={} no_dynamic_code={}", i.exe, if i.is32 { 32 } else { 64 }, i.gfx, i.overlays, i.packaged, i.no_dynamic_code));
            let mut a = app();
            a.lim_overlays = i.overlays.iter().map(|s| s.to_string()).collect();
            a.lim_arch = if i.is32 { 32 } else { 64 };
            // A failed injection because the game runs elevated can still be allowed for this one game.
            a.lim_admin = if matches!(&r, Err(e) if e == "elevated") { Some((pid, i.is32, mfps, mode, exe.clone())) } else { None };
        }
        {
            let mut a = app();
            a.lim_busy = false;
            match &r {
                Ok(()) => { a.lim_exe = exe.clone(); a.lim_status = ("limiting".into(), vec![exe.clone(), format!("{:.2}", mfps as f32 / 1000.0)]); }
                Err(e) => { let (c, mut args) = split_err(e); args.insert(0, exe.clone()); a.lim_status = (format!("lim_{c}"), args); }
            }
        }
        post_ui(WM_LIMITED, 0);
    });
}
fn reconfigure_limiter() {
    let exe = app().lim_exe.clone();
    if exe.is_empty() { return; }
    let Some(pid) = lim().as_ref().map(|l| l.target()).filter(|p| *p != 0) else { return };
    let game = candidate_windows().into_iter().find(|(h, _, _)| pid_of(*h) == pid).map(|x| x.0).unwrap_or_default();
    let prof = profile_of(game).1;
    let mfps = cap_milli_fps(&prof, game, is_scaling());
    if let Some(l) = lim().as_ref() { l.configure(config::get(&prof, "limiter") != 0, mfps, config::get(&prof, "latency_mode") as u32); }
}
/// Stops limiting and takes the DLL back out of the game (on a worker: unloading waits for the game).
fn stop_limiter() {
    { let mut a = app(); a.lim_exe.clear(); a.lim_admin = None; a.lim_status = ("limiter_off".into(), vec![]); }
    std::thread::spawn(|| {
        let st = detach_now();
        if st == ipc::DETACH_UNLOADED || st == ipc::DETACH_RESIDENT { app().lim_status = (if st == ipc::DETACH_UNLOADED { "limiter_unloaded" } else { "limiter_resident" }.into(), vec![]); post_ui(WM_LIMITED, 0); }
    });
}
fn detach_now() -> u32 { let mut g = lim(); match g.as_mut() { Some(l) => { l.configure(false, 0, 0); l.detach() } None => ipc::DETACH_NONE } }
/// "Allow" for a game that runs as administrator: one elevated helper injects, FlowScale itself stays unelevated.
fn limit_as_admin() {
    let Some((pid, is32, mfps, mode, exe)) = app().lim_admin.take() else { return };
    say("limiter_attaching", &[&exe]);
    std::thread::spawn(move || {
        let dll = limiter::exe_dir().join(if is32 { "flowscale_hook32.dll" } else { "flowscale_hook.dll" });
        let helper = if is32 { "flowscale32.exe" } else { "" };
        let code = launch_code("runas", helper, &format!("-inject {pid} \"{}\"", dll.display()));
        let ok = matches!(code, Some(0) | Some(2));
        {
            let mut g = lim();
            if let Some(l) = g.as_mut() { if ok { l.adopt(pid, if code == Some(2) { limiter::Method::WindowHook } else { limiter::Method::RemoteThread }); l.configure(true, mfps, mode); } }
        }
        let mut a = app();
        if ok { a.lim_exe = exe.clone(); a.lim_status = ("limiting".into(), vec![exe.clone(), format!("{:.2}", mfps as f32 / 1000.0)]); }
        else { a.lim_status = (if code.is_none() { "lim_uac_denied" } else { "lim_inject32" }.into(), vec![exe.clone()]); }
        drop(a);
        post_ui(WM_LIMITED, 0);
    });
}

/// Store apps show an ApplicationFrameWindow owned by ApplicationFrameHost; the game is its CoreWindow child.
fn real_window(h: HWND) -> HWND {
    unsafe {
        let mut cls = [0u16; 64]; let n = GetClassNameW(h, &mut cls);
        if String::from_utf16_lossy(&cls[..n.max(0) as usize]) != "ApplicationFrameWindow" { return h; }
        unsafe extern "system" fn cb(c: HWND, l: LPARAM) -> BOOL {
            let mut cls = [0u16; 64]; let n = unsafe { GetClassNameW(c, &mut cls) };
            if String::from_utf16_lossy(&cls[..n.max(0) as usize]) == "Windows.UI.Core.CoreWindow" { unsafe { *(l.0 as *mut HWND) = c; } return false.into(); }
            true.into()
        }
        let mut found = h;
        let _ = EnumChildWindows(Some(h), Some(cb), LPARAM(&mut found as *mut _ as isize));
        found
    }
}
fn covers_monitor(h: HWND) -> bool {
    unsafe {
        let mut r = RECT::default(); let _ = GetWindowRect(h, &mut r);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi);
        r.left <= mi.rcMonitor.left && r.top <= mi.rcMonitor.top && r.right >= mi.rcMonitor.right && r.bottom >= mi.rcMonitor.bottom
    }
}
fn detect_tick() {
    let fg = real_window(unsafe { GetForegroundWindow() });
    if fg.is_invalid() || fg == ui_hwnd() { return; }
    let pid = pid_of(fg);
    {
        let mut a = app();
        if pid != a.last_fg_pid { a.last_fg_pid = pid; a.fg_since = 0; return; }
        a.fg_since += 1;
        if a.fg_since != 2 { return; }
    }
    let target = target_of(fg);
    let exe = target.exe.clone();
    if exe.is_empty() || limiter::ignored(&exe) { return; }
    let (pname, prof, via) = profile_of(fg);
    let auto = app().store.app("auto_detect") != 0;
    let has_profile = pname != "Default";
    let detected = |code: &str, args: Vec<String>| { app().detector = (code.to_string(), args); post_ui(WM_DETECTOR, 0); };
    if let Some(l) = &via { detected("det_via", vec![exe.clone(), l.clone(), pname.clone()]); }
    // try_lock: an injection in progress holds the limiter for seconds; never block the UI thread on it.
    let Ok(guard) = LIM.get().unwrap().try_lock() else { return };
    let already = guard.as_ref().map(|l| l.target() == pid).unwrap_or(true);
    drop(guard);
    if config::get(&prof, "limiter") != 0 && (has_profile || auto) && !already {
        if let Some(info) = limiter::inspect(pid) {
            let a = detect::assess(&info, covers_monitor(fg));
            log(&format!("detector {} pid {pid}: {:?} score {} [{}]", info.exe, a.verdict, a.score, a.describe()));
            let why = a.evidence.iter().filter(|e| e.1 < 0).map(|e| e.0.to_string()).collect::<Vec<_>>().join(", ");
            match a.verdict {
                _ if has_profile && via.is_none() => detected("det_profile", vec![exe.clone(), pname.clone()]),
                _ if via.is_some() => {}
                detect::Verdict::Game => detected("det_game", vec![exe.clone(), a.score.to_string()]),
                detect::Verdict::Waiting => detected("det_waiting", vec![exe.clone()]),
                detect::Verdict::NotGame => detected("det_not_game", vec![exe.clone(), a.score.to_string(), if why.is_empty() { "-".into() } else { why }]),
                detect::Verdict::Blocked => detected("det_blocked", vec![exe.clone(), info.anticheat.clone().unwrap_or_default()]),
            }
            match a.verdict {
                // A profile is an explicit choice: it only needs a renderer. Default needs real game evidence.
                _ if has_profile && !info.gfx.is_empty() && a.verdict != detect::Verdict::Blocked => apply_limiter(fg, false),
                detect::Verdict::Game => apply_limiter(fg, false),
                detect::Verdict::Blocked => { app().lim_status = ("lim_anticheat".into(), vec![exe.clone(), info.anticheat.clone().unwrap_or_default()]); post_ui(WM_LIMITED, 0); }
                // Renderer not loaded yet (launcher, splash): look again on the next focus change.
                detect::Verdict::Waiting => { app().last_fg_pid = 0; }
                detect::Verdict::NotGame => {}
            }
        }
    }
    if has_profile && config::get(&prof, "auto") != 0 && !is_scaling() {
        let d = config::get(&prof, "delay").max(0) as u64;
        say("autoscale", &[&exe, &d.to_string()]);
        let (ui, fh) = (ui_hwnd().0 as isize, fg.0 as isize);
        std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_secs(d)); unsafe { let _ = PostMessageW(Some(HWND(ui as *mut c_void)), WM_AUTOSCALE, WPARAM(fh as usize), LPARAM(0)); } });
    }
}

// ------------------------------------------------------------------ UI bridge

fn push_state() {
    let st = {
        let a = app();
        json!({"t": "state", "profiles": a.store.profiles.keys().collect::<Vec<_>>(), "current": a.current, "values": config::profile_json(&a.store.profiles[&a.current]), "match": a.store.matches.get(&a.current).cloned().unwrap_or_default(), "gsync_supported": gsync::available(),
               "app": a.store.app, "vulkan_layer": limiter::vulkan_layer_installed(), "rtss": limiter::rtss_needs_fix(), "elevated": limiter::is_elevated(),
               "os_lang": os_lang(), "scaling": a.worker.as_ref().map(|w| !w.is_finished()).unwrap_or(false), "scaling_exe": a.scaling_exe,
               "limiter_exe": a.lim_exe, "limiter_busy": a.lim_busy, "limiter_overlays": a.lim_overlays, "limiter_arch": a.lim_arch, "limiter_admin": a.lim_admin.is_some(), "limiter_status": {"code": a.lim_status.0, "args": a.lim_status.1}, "detector": {"code": a.detector.0, "args": a.detector.1}})
    };
    post(&st);
}
fn sr_config(s: &Store) -> ingame::SrConfig {
    let g = |k: &str| s.app(k);
    ingame::SrConfig { sharpness: g("sr_sharpness") as f32 / 100.0, frame_weight: g("sr_weight") as f32 / 100.0, sharpen_mode: g("sr_sharpen_mode"),
        relight: g("sr_relight") != 0, anti_shimmer: g("sr_anti_shimmer") != 0, fp16: match g("sr_fp16") { 1 => 1, 2 => -1, _ => 0 },
        ratio_override: g("sr_ratio") as f32 / 100.0, compare: g("sr_compare") != 0, indicator: g("sr_indicator") != 0,
        ao_strength: g("sr_ao") as f32 / 100.0, gi_strength: g("sr_gi") as f32 / 100.0, ao_samples: g("sr_ao_samples"), curve: g("sr_curve") != 0, fade_m: g("sr_fade") }
}
fn push_games() {
    let (msg, st) = { let a = app(); (json!({"t": "games", "busy": a.games_busy, "list": a.games.iter().map(|g| g.json()).collect::<Vec<_>>(), "msg": {"code": a.games_msg.0, "args": a.games_msg.1}}), a.games_msg.clone()) };
    post(&msg);
    if !st.0.is_empty() { let args: Vec<&str> = st.1.iter().map(|s| s.as_str()).collect(); say(&st.0, &args); app().games_msg.0.clear(); }
}
fn games_job(op: Option<(String, String)>) {
    { let mut a = app(); if a.games_busy { return; } a.games_busy = true; a.games_msg = (String::new(), vec![]); }
    post_ui(WM_GAMES, 0);
    std::thread::spawn(move || {
        let say_bg = |c: &str, args: Vec<String>| { app().games_msg = (c.to_string(), args); post_ui(WM_GAMES, 0); };
        match op {
            None => { let v = ingame::scan(); let n = v.len(); app().games = v; say_bg("games_found", vec![n.to_string()]); }
            Some((root, op)) => {
                // Games are addressed by folder, not list index, so a rescan in between cannot hit the wrong game.
                let (g, cfg) = { let a = app(); (a.games.iter().find(|g| g.root.to_string_lossy() == root).cloned(), sr_config(&a.store)) };
                if let Some(g) = g {
                    let r = match op.as_str() {
                        "install" => ingame::install(&g, &cfg).map(|apis| ("sr_installed".to_string(), vec![g.name.clone(), apis])),
                        "restore" => ingame::restore(&g).map(|_| ("sr_restored".to_string(), vec![g.name.clone()])),
                        "open" => { unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(g.exe_dir.as_os_str()), None, None, SW_SHOWNORMAL); } Ok((String::new(), vec![])) }
                        _ => Err("unknown operation".into()),
                    };
                    let fresh = ingame::inspect(g.name.clone(), g.store, g.root.clone());
                    if let Some(x) = app().games.iter_mut().find(|x| x.root == g.root) { *x = fresh; }
                    match r { Ok((c, a)) if !c.is_empty() => say_bg(&c, a), Ok(_) => {} Err(e) => say_bg("game_error", vec![g.name.clone(), e]) }
                }
            }
        }
        app().games_busy = false;
        post_ui(WM_GAMES, 0);
    });
}

#[repr(C)]
struct SrShared { magic: u32, pid: u32, api: u32, rw: u32, rh: u32, ow: u32, oh: u32, gpu_us: u32, frames: u64, qpc: i64, qpf: i64 }
fn sr_live() -> Value {
    use windows::Win32::System::Memory::*;
    unsafe {
        let Ok(h) = OpenFileMappingW(FILE_MAP_READ.0, false, w!("Local\\FlowScaleSR")) else { return json!({"active": false}) };
        let v = MapViewOfFile(h, FILE_MAP_READ, 0, 0, size_of::<SrShared>());
        let out = if v.Value.is_null() { json!({"active": false}) } else {
            let s = std::ptr::read_volatile(v.Value as *const SrShared);
            let _ = UnmapViewOfFile(v);
            let fresh = s.magic == 0x5253_5346 && s.qpf > 0 && pacer::qpc() - s.qpc < s.qpf * 2;
            static NAME: Mutex<(u32, String)> = Mutex::new((0, String::new()));
            let exe = if fresh { let mut n = NAME.lock().unwrap(); if n.0 != s.pid { *n = (s.pid, exe_of_pid(s.pid).unwrap_or_default()); } n.1.clone() } else { String::new() };
            json!({"active": fresh, "exe": exe, "api": if s.api == 2 { "XeSS" } else { "FSR 3.1" },
                   "rw": s.rw, "rh": s.rh, "ow": s.ow, "oh": s.oh, "gpu_ms": s.gpu_us as f32 / 1000.0, "frames": s.frames})
        };
        let _ = CloseHandle(h);
        out
    }
}
fn poll_limiter() -> Value {
    // try_lock: if an injection is in progress we simply skip this tick.
    let Ok(mut g) = LIM.get().unwrap().try_lock() else { return json!({"active": false, "busy": true}) };
    let Some(l) = g.as_mut() else { return json!({"active": false}) };
    limiter_tick(l);
    let (avg, low1, low01, sd) = l.stats.summary();
    let (hz15, hz2, hz3, hz4) = l.stats.hitches_ext();
    let m = l.metrics();
    let s = l.shared();
    json!({"active": l.alive(), "api": l.api_name(), "w": s.width.load(Ordering::Relaxed), "h": s.height.load(Ordering::Relaxed),
           "frames": l.recent, "fps": avg, "low1": low1, "low01": low01, "sd": sd, "spin_us": s.spin_us.load(Ordering::Relaxed),
           "late": m.late, "cap": s.milli_fps.load(Ordering::Relaxed) as f32 / 1000.0, "enabled": s.enabled.load(Ordering::Relaxed) != 0,
           "hitch15": hz15, "hitch2": hz2, "hitch3": hz3, "hitch4": hz4, "wake_avg": m.wake_avg_ms, "wake_max": m.wake_max_ms, "limited_pct": m.limited_pct,
           "p95": m.p95_err_ms, "p99": m.p99_err_ms, "drift_ppm": m.drift_ppm, "hooks": m.hooks, "arch": m.arch})
}
/// Per-tick limiter housekeeping: read frames, notice the game exiting (and how).
fn limiter_tick(l: &mut limiter::Limiter) {
    l.poll();
    if let Some((pid, code)) = l.take_exit() {
        let (avg, low1, _, sd) = l.stats.summary();
        let exe = { let mut a = app(); let e = std::mem::take(&mut a.lim_exe); a.lim_admin = None;
            a.lim_status = (if limiter::exit_is_crash(code) { "game_crashed" } else { "game_exited" }.into(), vec![e.clone(), format!("{code:#010x}"), limiter::exit_name(code).into()]); e };
        log(&format!("game session: {exe} pid {pid} exit {code:#010x} ({}), avg {avg:.2} fps, 1% low {low1:.2}, sd {sd:.3} ms", limiter::exit_name(code)));
        post_ui(WM_LIMITED, 0);
    }
}
fn push_tele() {
    let status = app().status.clone();
    let scale = {
        let mut t = status.tele.lock().unwrap();
        let (avg, low1, low01, sd) = t.out.summary();
        let (ravg, _, _, _) = t.real.summary();
        let (hz15, hz2) = t.out.hitches();
        let out: Vec<f32> = std::mem::take(&mut t.out_recent);
        json!({"active": t.active, "frames": out, "fps": avg, "low1": low1, "low01": low01, "sd": sd, "real_fps": ravg, "ratio": t.ratio, "refresh": t.refresh, "hitch15": hz15, "hitch2": hz2})
    };
    let mut l = poll_limiter();
    l["exe"] = json!(app().lim_exe);
    post(&json!({"t": "tele", "scale": scale, "lim": l, "sr": sr_live()}));
}

fn on_message(m: Value) {
    let str_of = |k: &str| m[k].as_str().unwrap_or("").to_string();
    match m["t"].as_str().unwrap_or("") {
        "ready" => {
            let sch = { let a = app(); config::schema(&a.gpus, &a.displays) };
            post(&sch); push_state();
            let args: Vec<String> = std::env::args().collect();
            if let Some(i) = args.iter().position(|x| x == "-page") { post(&json!({"t": "go", "page": args.get(i + 1).cloned().unwrap_or_default()})); }
        }
        "set" => {
            let key = str_of("key");
            let Some(o) = config::opt(&key) else { return };
            let mut v = config::sanitize(o, m["value"].as_i64().unwrap_or(0).clamp(i32::MIN as i64, i32::MAX as i64) as i32);
            {
                let mut a = app();
                if let config::Kind::Gpu = o.kind { v = v.min(a.gpus.len() as i32 - 1); }
                if let config::Kind::Display = o.kind { v = v.min(a.displays.len() as i32 - 1); }
                match o.scope {
                    Scope::Profile => { let c = a.current.clone(); if let Some(p) = a.store.profiles.get_mut(&c) { config::set(p, &key, v); } }
                    Scope::App => { a.store.app.insert(key.clone(), v); }
                }
                config::save(&a.store);
                if o.group == "osd" { sync_osd(&a.store); }
            }
            match key.as_str() {
                "autostart" => limiter::set_autostart(v != 0),
                k if k.starts_with("hk_") => register_hotkeys(ui_hwnd()),
                "theme" => apply_window_theme(ui_hwnd()),
                "always_on_top" => apply_topmost(ui_hwnd()),
                "gsync" => { if v == 0 { gsync::ensure(false); } }
                _ => {}
            }
            if o.scope == Scope::Profile { reconfigure_limiter(); }
            // The engine reads its settings when a session starts: a change that affects it restarts the session on the
            // same game (debounced for sliders), so the engine and the limiter's FG-aware cap always agree.
            if o.scope == Scope::Profile && o.group != "limiter" && is_scaling() { unsafe { let _ = SetTimer(Some(ui_hwnd()), T_RESTART, 400, None); } }
        }
        "vulkan_layer" => { if let Err(e) = limiter::set_vulkan_layer(m["value"].as_bool().unwrap_or(false)) { say("vk_error", &[&e.message()]); } push_state(); }
        "profile" => {
            let name = str_of("name").trim().to_lowercase();
            {
                let mut a = app();
                match m["op"].as_str().unwrap_or("") {
                    "select" => { let n = str_of("name"); if a.store.profiles.contains_key(&n) { a.current = n; } }
                    "add" if !name.is_empty() && name != "app" => { let base = a.store.profiles[&a.current].clone(); a.store.profiles.entry(name.clone()).or_insert(base); a.current = name; config::save(&a.store); }
                    "remove" => { if a.current != "Default" { let c = a.current.clone(); a.store.profiles.remove(&c); a.current = "Default".into(); config::save(&a.store); } }
                    "match" => { let c = a.current.clone(); let v = str_of("value"); if c != "Default" { if v.trim().is_empty() { a.store.matches.remove(&c); } else { a.store.matches.insert(c, v.trim().replace(['\r', '\n'], " ")); } config::save(&a.store); } }
                    "reset" => { let c = a.current.clone(); a.store.profiles.insert(c, config::defaults()); config::save(&a.store); }
                    _ => {}
                }
            }
            reconfigure_limiter(); push_state();
        }
        "windows" => {
            let list: Vec<Value> = candidate_windows().into_iter().map(|(h, e, t)| json!({"hwnd": h.0 as isize, "exe": e, "title": t})).collect();
            post(&json!({"t": "windows", "list": list}));
        }
        "scale" => {
            if is_scaling() { stop_scaling(); }
            else if let Some(h) = m["hwnd"].as_i64() { let g = HWND(h as isize as *mut c_void); unsafe { if IsWindow(Some(g)).as_bool() { let _ = SetForegroundWindow(g); start_scaling(g); } } }
            else { let h = { let mut a = app(); a.countdown = 4; a.hwnd }; unsafe { SetTimer(Some(h), T_COUNT, 1000, None); } say("countdown", &["4"]); }
        }
        "limit" => {
            if m["off"].as_bool().unwrap_or(false) { stop_limiter(); push_state(); }
            else if let Some(h) = m["hwnd"].as_i64() { apply_limiter(HWND(h as isize as *mut c_void), true); push_state(); }
        }
        "limit_admin" => limit_as_admin(),
        "elevate" => { if launch("runas", "-replace", false) { unsafe { let _ = DestroyWindow(ui_hwnd()); } } }
        "rtss" => {
            let ok = limiter::rtss_fix().is_ok() || launch("runas", "-rtss", true);
            if ok && limiter::rtss_hooked_here() { if launch("open", "-replace", false) { unsafe { let _ = DestroyWindow(ui_hwnd()); } } }
            else { say(if ok { "rtss_ok" } else { "rtss_unchanged" }, &[]); push_state(); }
        }
        "games" => games_job(None),
        "game_add" => { match ingame::add_folder(&str_of("path")) { Ok(()) => games_job(None), Err(e) => say("game_folder_bad", &[&e]) } }
        "game" => games_job(Some((str_of("root"), str_of("op")))),
        "sr_apply" => {
            let (games, cfg) = { let a = app(); (a.games.clone(), sr_config(&a.store)) };
            say("sr_applied", &[&ingame::apply_settings(&games, &cfg).to_string()]);
        }
        "open_folder" => { unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(limiter::exe_dir().as_os_str()), None, None, SW_SHOWNORMAL); } }
        _ => {}
    }
}

/// Runs `exe` (FlowScale itself when empty) with a verb and waits; None when it could not start (UAC declined).
fn launch_code(verb: &str, exe: &str, args: &str) -> Option<u32> {
    let path = if exe.is_empty() { std::env::current_exe().ok()? } else { limiter::exe_dir().join(exe) };
    let (file, verb, args) = (HSTRING::from(path.as_os_str()), HSTRING::from(verb), HSTRING::from(args));
    let mut sei = SHELLEXECUTEINFOW { cbSize: size_of::<SHELLEXECUTEINFOW>() as u32, fMask: SEE_MASK_NOCLOSEPROCESS, lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()), lpParameters: PCWSTR(args.as_ptr()), nShow: SW_HIDE.0, ..Default::default() };
    unsafe {
        ShellExecuteExW(&mut sei).ok()?;
        let hp = sei.hProcess; // copy out: the struct is packed on x86
        if hp.is_invalid() { return None; }
        WaitForSingleObject(hp, 20000);
        let mut code = 1u32; let _ = GetExitCodeProcess(hp, &mut code); let _ = CloseHandle(hp);
        Some(code)
    }
}
fn launch(verb: &str, args: &str, wait: bool) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = HSTRING::from(exe.as_os_str());
    let (verb, args) = (HSTRING::from(verb), HSTRING::from(args));
    let mut sei = SHELLEXECUTEINFOW { cbSize: size_of::<SHELLEXECUTEINFOW>() as u32, fMask: SEE_MASK_NOCLOSEPROCESS, lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()), lpParameters: PCWSTR(args.as_ptr()), nShow: if wait { SW_HIDE.0 } else { SW_SHOWNORMAL.0 }, ..Default::default() };
    unsafe {
        if ShellExecuteExW(&mut sei).is_err() { return false; }
        let hp = sei.hProcess;
        let mut ok = true;
        if wait && !hp.is_invalid() { WaitForSingleObject(hp, 15000); let mut code = 1u32; let _ = GetExitCodeProcess(hp, &mut code); ok = code == 0; }
        if !hp.is_invalid() { let _ = CloseHandle(hp); }
        ok
    }
}

// ------------------------------------------------------------------ window, webview, tray

fn webview_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).map(|p| p.join("FlowScale").join("WebView2")).unwrap_or_else(|| limiter::exe_dir().join("webview-data"))
}
fn create_webview(h: HWND) -> windows::core::Result<()> {
    // Trim the embedded browser: no background networking, SmartScreen or first-run work for a local page.
    unsafe { std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--disable-background-networking --disable-component-update --disable-features=msSmartScreenProtection,msEdgeShopping,msWebOOUI --no-first-run"); }
    let data_w = HSTRING::from(webview_dir().as_os_str());
    let (tx, rx) = std::sync::mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &data_w, None, &handler).map_err(webview2_com::Error::WindowsError) }),
        Box::new(move |err, env| { err?; tx.send(env).ok(); Ok(()) }),
    ).map_err(|e| windows::core::Error::new(E_FAIL, format!("{e:?}")))?;
    let env = rx.recv().ok().flatten().ok_or_else(|| windows::core::Error::new(E_FAIL, "WebView2 runtime not available"))?;
    let env2 = env.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe { env.CreateCoreWebView2Controller(h, &handler).map_err(webview2_com::Error::WindowsError) }),
        Box::new(move |err, c| { err?; tx.send(c).ok(); Ok(()) }),
    ).map_err(|e| windows::core::Error::new(E_FAIL, format!("{e:?}")))?;
    let controller = rx.recv().ok().flatten().ok_or_else(|| windows::core::Error::new(E_FAIL, "controller"))?;
    unsafe {
        if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() { let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 255, G: 255, B: 255 }); }
        let mut r = RECT::default(); let _ = GetClientRect(h, &mut r);
        controller.SetBounds(r)?;
        controller.SetIsVisible(true)?;
        let wv = controller.CoreWebView2()?;
        let st = wv.Settings()?;
        let dbg = std::env::args().any(|a| a == "-devtools");
        st.SetAreDefaultContextMenusEnabled(dbg)?; st.SetAreDevToolsEnabled(dbg)?; st.SetIsStatusBarEnabled(false)?; st.SetIsZoomControlEnabled(false)?;
        if let Ok(s3) = st.cast::<ICoreWebView2Settings3>() { let _ = s3.SetAreBrowserAcceleratorKeysEnabled(dbg); }
        let mut tok = 0i64;
        wv.add_WebMessageReceived(&WebMessageReceivedEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                let mut p = PWSTR::null();
                if args.WebMessageAsJson(&mut p).is_ok() {
                    let s = CoTaskMemPWSTR::from(p).to_string();
                    if let Ok(v) = serde_json::from_str::<Value>(&s) { on_message(v); }
                }
            }
            Ok(())
        })), &mut tok)?;
        wv.AddWebResourceRequestedFilter(w!("https://flowscale.ui/*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        let mut tok2 = 0i64;
        wv.add_WebResourceRequested(&WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let stream = SHCreateMemStream(Some(UI_HTML.as_bytes()));
                if let Ok(resp) = env2.CreateWebResourceResponse(stream.as_ref(), 200, w!("OK"), w!("Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store")) { let _ = args.SetResponse(&resp); }
            }
            Ok(())
        })), &mut tok2)?;
        wv.Navigate(w!("https://flowscale.ui/index.html"))?;
        let mut a = app(); a.webview = Some(wv); a.controller = Some(controller);
    }
    Ok(())
}
/// Hidden to tray: stop rendering and let WebView2 drop its memory.
fn set_ui_active(on: bool) {
    let (c, wv) = { let a = app(); (a.controller.clone(), a.webview.clone()) };
    unsafe {
        if let Some(c) = c { let _ = c.SetIsVisible(on); }
        if let Some(w3) = wv.and_then(|w| w.cast::<ICoreWebView2_3>().ok()) {
            if on { let _ = w3.Resume(); } else { let _ = w3.TrySuspend(&TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())))); }
        }
    }
}

fn tray(h: HWND, add: bool) {
    unsafe {
        let mut nid = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: h, uID: 1, uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAY, ..Default::default() };
        nid.hIcon = GetModuleHandleW(None).ok().and_then(|m| LoadIconW(Some(m.into()), PCWSTR(1 as _)).ok()).or_else(|| LoadIconW(None, IDI_APPLICATION).ok()).unwrap_or_default();
        let mut tip = [0u16; 128]; for (d, c) in tip.iter_mut().zip("FlowScale".encode_utf16()) { *d = c; } nid.szTip = tip; // packed on x86: no field refs
        let _ = Shell_NotifyIconW(if add { NIM_ADD } else { NIM_DELETE }, &nid);
    }
}
/// Windows' own light/dark choice for apps.
fn system_dark() -> bool {
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut v = 1u32; let mut n = 4u32;
        let r = RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut n));
        r.is_ok() && v == 0
    }
}
fn ui_dark() -> bool { match app().store.app("theme") { 1 => false, 2 => true, _ => system_dark() } }
/// Title bar and window border follow the app theme so the native frame never clashes with the page.
fn apply_window_theme(h: HWND) {
    let dark = ui_dark();
    unsafe {
        let d: BOOL = dark.into(); let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as _, 4);
        let cap = COLORREF(if dark { 0x00191512 } else { 0x00FFFFFF }); let _ = DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR, &cap as *const _ as _, 4);
        let border = COLORREF(if dark { 0x00332B26 } else { 0x00ECE7E4 }); let _ = DwmSetWindowAttribute(h, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
    }
    if let Some(c2) = app().controller.clone().and_then(|c| c.cast::<ICoreWebView2Controller2>().ok()) {
        let bg = if dark { COREWEBVIEW2_COLOR { A: 255, R: 18, G: 21, B: 25 } } else { COREWEBVIEW2_COLOR { A: 255, R: 255, G: 255, B: 255 } };
        unsafe { let _ = c2.SetDefaultBackgroundColor(bg); }
    }
}
fn apply_topmost(h: HWND) {
    let on = app().store.app("always_on_top") != 0;
    unsafe { let _ = SetWindowPos(h, Some(if on { HWND_TOPMOST } else { HWND_NOTOPMOST }), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE); }
}
fn hide_main(h: HWND) { unsafe { let _ = ShowWindow(h, SW_HIDE); } set_ui_active(false); }
fn show_main(h: HWND) { set_ui_active(true); unsafe { let _ = ShowWindow(h, SW_SHOW); let _ = ShowWindow(h, SW_RESTORE); let _ = SetForegroundWindow(h); } push_state(); }

unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match m {
            WM_CREATE => {
                app().hwnd = h;
                apply_window_theme(h);
                tray(h, true);
                register_hotkeys(h);
                SetTimer(Some(h), T_TELE, 50, None);
                SetTimer(Some(h), T_DETECT, 1000, None);
                if let Err(e) = create_webview(h) { log(&format!("webview: {e:?}")); MessageBoxW(Some(h), w!("FlowScale needs the Microsoft Edge WebView2 Runtime."), w!("FlowScale"), MB_ICONERROR); }
                apply_window_theme(h); // again, now that the WebView exists: its background must match before the page paints
                apply_topmost(h);
            }
            WM_SIZE => {
                if wp.0 == SIZE_MINIMIZED as usize { if app().store.app("close_to_tray") != 0 { hide_main(h); } }
                else { let c = app().controller.clone(); if let Some(c) = c { let mut r = RECT::default(); let _ = GetClientRect(h, &mut r); let _ = c.SetBounds(r); } }
            }
            WM_GETMINMAXINFO => { let mi = &mut *(lp.0 as *mut MINMAXINFO); let d = GetDpiForWindow(h).max(96) as i32; mi.ptMinTrackSize = POINT { x: 860 * d / 96, y: 560 * d / 96 }; }
            WM_HOTKEY => match wp.0 as i32 {
                1 => { if is_scaling() { stop_scaling() } else { start_scaling(GetForegroundWindow()) } }
                2 => {
                    let msg = lim().as_ref().filter(|l| l.target() != 0).map(|l| { let s = l.shared(); let on = s.enabled.load(Ordering::Relaxed) == 0; s.enabled.store(on as u32, Ordering::Relaxed); if on { "limiter_on" } else { "limiter_paused" } });
                    match msg { Some(c) => { app().lim_status = (c.into(), vec![]); say(c, &[]); } None => { apply_limiter(GetForegroundWindow(), true); } }
                    push_state();
                }
                3 => { let v = !osd::VISIBLE.load(Ordering::Relaxed); osd::VISIBLE.store(v, Ordering::Relaxed); say(if v { "osd_shown" } else { "osd_hidden" }, &[]); }
                _ => {}
            },
            WM_TIMER => match wp.0 {
                T_TELE => { if IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() { push_tele(); } else if let Ok(mut g) = LIM.get().unwrap().try_lock() { if let Some(l) = g.as_mut() { limiter_tick(l); } } }
                T_DETECT => detect_tick(),
                T_RESTART => {
                    let _ = KillTimer(Some(h), T_RESTART);
                    let g = HWND(app().scaling_game as *mut c_void);
                    if is_scaling() && IsWindow(Some(g)).as_bool() { log("settings changed: restarting the scaling session"); stop_scaling(); start_scaling(g); }
                }
                T_COUNT => {
                    let n = { let mut a = app(); a.countdown -= 1; a.countdown };
                    if n <= 0 { let _ = KillTimer(Some(h), T_COUNT); start_scaling(GetForegroundWindow()); } else { say("countdown", &[&n.to_string()]); }
                }
                _ => {}
            },
            WM_GAMES => push_games(),
            WM_LIMITED => { let (c, a) = app().lim_status.clone(); let args: Vec<&str> = a.iter().map(|s| s.as_str()).collect(); say(&c, &args); push_state(); }
            WM_DETECTOR => { let (c, a) = app().detector.clone(); post(&json!({"t": "detector", "code": c, "args": a})); }
            WM_FOLLOW => { let g = HWND(wp.0 as *mut c_void); if IsWindow(Some(g)).as_bool() && !is_scaling() { start_scaling(g); } }
            WM_SETTINGCHANGE => { apply_window_theme(h); return DefWindowProcW(h, m, wp, lp); }
            WM_AUTOSCALE => { let g = HWND(wp.0 as *mut c_void); if GetForegroundWindow() == g { start_scaling(g); } }
            WM_STATUS | WM_DONE => {
                // Only the session that ended may tear down: a quick stop/start must not kill the new run.
                if m == WM_DONE {
                    if wp.0 as u64 != app().session { return LRESULT(0); }
                    let follow = app().follow.take();
                    stop_scaling();
                    if app().status.text.lock().unwrap().as_str() == "gone" { if let Some(f) = follow { follow_game(f); } }
                }
                let t = app().status.text.lock().unwrap().clone();
                let (c, a) = split_err(&t); let args: Vec<&str> = a.iter().map(|s| s.as_str()).collect(); say(&c, &args);
            }
            WM_TRAY => {
                let e = (lp.0 & 0xFFFF) as u32;
                if e == WM_LBUTTONUP { show_main(h); }
                else if e == WM_RBUTTONUP {
                    let pt = os_lang() == "pt";
                    if let Ok(menu) = CreatePopupMenu() {
                        let open = if is_scaling() { if pt { w!("Parar escala") } else { w!("Stop scaling") } } else if pt { w!("Abrir FlowScale") } else { w!("Open FlowScale") };
                        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, open);
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                        // Quick frame cap for the limited game, straight from the tray.
                        if let Ok(sub) = CreatePopupMenu() {
                            let cur = lim().as_ref().filter(|l| l.target() != 0).map(|l| (l.shared().enabled.load(Ordering::Relaxed) != 0, l.shared().milli_fps.load(Ordering::Relaxed)));
                            for (i, fps) in TRAY_CAPS.iter().enumerate() {
                                let label = HSTRING::from(format!("{fps} FPS"));
                                let checked = cur.is_some_and(|(on, m)| on && m == fps * 1000);
                                let _ = AppendMenuW(sub, if cur.is_some() { MF_STRING } else { MF_STRING | MF_GRAYED } | if checked { MF_CHECKED } else { MF_UNCHECKED }, CMD_CAP + i, &label);
                            }
                            let _ = AppendMenuW(sub, MF_SEPARATOR, 0, None);
                            let _ = AppendMenuW(sub, if cur.is_some() { MF_STRING } else { MF_STRING | MF_GRAYED }, CMD_CAP_OFF, if pt { w!("Desligar limite") } else { w!("Turn the cap off") });
                            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, if pt { w!("Limite de FPS") } else { w!("Frame cap") });
                        }
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, if pt { w!("Sair") } else { w!("Exit") });
                        let mut p = POINT::default(); let _ = GetCursorPos(&mut p); let _ = SetForegroundWindow(h);
                        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, None, h, None); let _ = DestroyMenu(menu);
                    }
                }
            }
            WM_COMMAND => match wp.0 & 0xFFFF {
                CMD_EXIT => { watchdog::exit_asked(); log("exit: asked"); let _ = DestroyWindow(h); }
                CMD_OPEN => { if is_scaling() { stop_scaling(); } else { show_main(h); } }
                CMD_CAP_OFF => { stop_limiter(); push_state(); }
                c if (CMD_CAP..CMD_CAP + TRAY_CAPS.len()).contains(&c) => {
                    let fps = TRAY_CAPS[c - CMD_CAP];
                    let exe = app().lim_exe.clone();
                    if let Some(l) = lim().as_ref() { let mode = l.shared().mode.load(Ordering::Relaxed); l.configure(true, fps * 1000, mode); }
                    app().lim_status = ("limiting".into(), vec![exe.clone(), format!("{fps}.00")]);
                    say("limiting", &[&exe, &format!("{fps}.00")]); push_state();
                }
                _ => {}
            },
            WM_CLOSE => { if app().store.app("close_to_tray") != 0 { hide_main(h); } else { let _ = DestroyWindow(h); } }
            WM_DESTROY => {
                log("exit: stopping");
                stop_scaling();
                log("exit: engine stopped, detaching the limiter");
                detach_now();
                log("exit: done"); // synchronously: the game must not keep our DLL after FlowScale exits
                for (id, _) in HOTKEYS { let _ = UnregisterHotKey(Some(h), *id); }
                tray(h, false); PostQuitMessage(0);
            }
            // Explorer restarted: the notification area is new and our icon must be added again.
            _ if m == taskbar_created() => { tray(h, false); tray(h, true); }
            _ => return DefWindowProcW(h, m, wp, lp),
        }
        LRESULT(0)
    }
}

// ------------------------------------------------------------------ entry & command line

fn cli(args: &[String]) -> Option<i32> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let pos = |f: &str| args.iter().position(|a| a == f);
    // Helper modes (also used by the 32-bit build and by the per-game administrator prompt).
    if let Some(i) = pos("-inject") {
        let pid: u32 = arg(i + 1).parse().unwrap_or(0);
        let dll = std::path::PathBuf::from(arg(i + 2));
        if let Some(info) = limiter::inspect(pid) { if info.packaged { let _ = limiter::allow_app_containers(&dll); } }
        return Some(match limiter::inject(pid, &dll) {
            Ok(limiter::Method::RemoteThread) => 0, Ok(limiter::Method::WindowHook) => 2,
            Err(e) if e.code() == windows::Win32::Foundation::E_ACCESSDENIED => 5, Err(_) => 1,
        });
    }
    if let Some(i) = pos("-free") { return Some(limiter::free_remote(arg(i + 1).parse().unwrap_or(0), &arg(i + 2)).is_ok() as i32 ^ 1); }
    if pos("--uninstall-cleanup").is_some() || pos("-uninstall-cleanup").is_some() {
        // Leaves nothing behind: autostart entry, Vulkan layer, G-SYNC profile settings, WebView2 cache.
        limiter::set_autostart(false);
        let _ = limiter::set_vulkan_layer(false);
        gsync::ensure(false); std::thread::sleep(std::time::Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(webview_dir());
        log("uninstall cleanup done");
        return Some(0);
    }
    if let Some(i) = pos("-limit") {
        let needle = arg(i + 1).to_lowercase();
        let fps: f32 = arg(i + 2).parse().unwrap_or(60.0);
        let secs: u64 = arg(i + 3).parse().unwrap_or(5);
        let r = (|| -> std::result::Result<String, String> {
            let (g, exe, _) = candidate_windows().into_iter().find(|(_, _, t)| t.to_lowercase().contains(&needle)).ok_or("window not found")?;
            let info = limiter::inspect(pid_of(g)).ok_or("cannot inspect process")?;
            let mut l = limiter::Limiter::new().ok_or("ipc")?;
            l.configure(true, (fps * 1000.0) as u32, 0);
            l.attach(pid_of(g), &info)?;
            std::thread::sleep(std::time::Duration::from_millis(1500)); l.poll(); l.stats = pacer::FrameStats::new(8192);
            std::thread::sleep(std::time::Duration::from_secs(secs)); l.poll();
            let (avg, low1, low01, sd) = l.stats.summary();
            let m = l.metrics();
            let mut out = format!("{exe} [{}] gfx={:?}: target {fps} -> avg {avg:.2} fps, 1% low {low1:.2}, 0.1% low {low01:.2}, sd {sd:.3} ms, p99 err {:.3} ms, wake {:.3}/{:.3} ms, limited {:.0}%, spin {:.2} ms, frames {}, hooks {:#x}",
                l.api_name(), info.gfx, m.p99_err_ms, m.wake_avg_ms, m.wake_max_ms, m.limited_pct, l.shared().spin_us.load(Ordering::Relaxed) as f32 / 1000.0, l.stats.count, m.hooks);
            // "-limit <title> <fps> <s> detach": take the DLL back out and prove the game survives without it.
            if arg(i + 4) == "detach" {
                let st = l.detach();
                std::thread::sleep(std::time::Duration::from_millis(500));
                let path = limiter::exe_dir().join(if info.is32 { "flowscale_hook32.dll" } else { "flowscale_hook.dll" });
                let alive = unsafe { IsWindow(Some(g)).as_bool() };
                out += &format!(" | detach state {st}, module still loaded: {}, game alive: {alive}", limiter::module_base(pid_of(g), &path.to_string_lossy()).is_some());
            }
            Ok(out)
        })();
        log(&format!("limit: {r:?}"));
        return Some(r.is_ok() as i32 ^ 1);
    }
    if pos("-games").is_some() { log(&format!("games: {}", serde_json::to_string_pretty(&ingame::scan().iter().map(|g| g.json()).collect::<Vec<_>>()).unwrap_or_default())); return Some(0); }
    if let Some(i) = pos("-game") {
        let dir = std::path::PathBuf::from(arg(i + 1));
        let op = arg(i + 2);
        let g = ingame::inspect(dir.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), "Folder", dir);
        let cfg = sr_config(&config::load());
        let r = match op.as_str() { "install" => ingame::install(&g, &cfg), "restore" => ingame::restore(&g), _ => Ok(serde_json::to_string(&g.json()).unwrap_or_default()) };
        log(&format!("game {op}: {r:?}"));
        return Some(r.is_ok() as i32 ^ 1);
    }
    if pos("-rtss").is_some() { return Some(limiter::rtss_fix().is_ok() as i32 ^ 1); }
    if pos("-fgcost").is_some() {
        let mut r = String::from("fgcost (ms per real+generated frame):\n");
        for (w, h) in [(1280u32, 720u32), (1920, 1080)] { for q in 0..3 { for fl in [0.5f32, 1.0] {
            r += &format!("  {w}x{h} quality {q} flow {:.0}%: {:.2} ms\n", fl * 100.0, engine::fgcost(w, h, fl, q).unwrap_or(-1.0));
        } } }
        log(&r); return Some(0);
    }
    if let Some(i) = pos("-scalebench") { let d = args.get(i + 1).map(std::path::PathBuf::from); let r = engine::scalebench(d.as_deref()); log(&format!("scalebench:
{}", r.as_deref().unwrap_or("error"))); if let Err(e) = &r { log(&format!("{e:?}")); } return Some(r.is_ok() as i32 ^ 1); }
    if let Some(i) = pos("-gsync") { match arg(i + 1).as_str() { "on" => gsync::ensure(true), "off" => gsync::ensure(false), _ => {} } std::thread::sleep(std::time::Duration::from_millis(1500)); log(&format!("gsync: {}", gsync::status())); return Some(0); }
    if pos("-fgbench").is_some() { let r = engine::fgbench(); log(&format!("fgbench:\n{}", r.as_deref().unwrap_or("error"))); return Some(r.is_ok() as i32 ^ 1); }
    if let Some(i) = pos("-osdshot") { return Some(osd::snapshot(std::path::Path::new(&arg(i + 1))).is_ok() as i32 ^ 1); }
    if pos("-selftest").is_some() { let r = engine::selftest(); log(&format!("selftest: {r:?}")); return Some(r.is_ok() as i32 ^ 1); }
    None
}

/// Crash reporting: a Rust panic (abort in release) or a native exception leaves a readable line in flowscale.crash.log.
fn install_crash_log() {
    fn write(line: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(limiter::exe_dir().join("flowscale.crash.log")) {
            let _ = f.write_all(format!("{} {line}\r\n", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)).as_bytes());
        }
    }
    std::panic::set_hook(Box::new(|p| write(&format!("panic: {p}"))));
    unsafe extern "system" fn filter(e: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
        unsafe {
            if let Some(r) = e.as_ref().and_then(|e| e.ExceptionRecord.as_ref()) {
                let addr = r.ExceptionAddress as usize;
                let mut m = windows::Win32::Foundation::HMODULE::default();
                let _ = windows::Win32::System::LibraryLoader::GetModuleHandleExW(windows::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | windows::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, PCWSTR(addr as *const u16), &mut m);
                let mut name = [0u16; 260]; let n = windows::Win32::System::LibraryLoader::GetModuleFileNameW(Some(m), &mut name);
                write(&format!("exception {:#010x} ({}) at {addr:#x} in {} +{:#x}", r.ExceptionCode.0 as u32, limiter::exit_name(r.ExceptionCode.0 as u32),
                    String::from_utf16_lossy(&name[..n as usize]), addr.wrapping_sub(m.0 as usize)));
            }
        }
        0 // EXCEPTION_CONTINUE_SEARCH: Windows Error Reporting still runs
    }
    unsafe { windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(filter)); }
}

fn main() {
    install_crash_log();
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = cli(&args) { std::process::exit(code); }
    unsafe {
        if args.iter().any(|a| a == "-replace") {
            for _ in 0..100 { if FindWindowW(w!("FlowScaleMain"), None).is_err() { break; } std::thread::sleep(std::time::Duration::from_millis(50)); }
        } else if let Ok(prev) = FindWindowW(w!("FlowScaleMain"), None) {
            if !args.iter().any(|a| a == "-scale") { let _ = PostMessageW(Some(prev), WM_TRAY, WPARAM(0), LPARAM(WM_LBUTTONUP as isize)); return; }
        }
        engine::recover_input();
        logzip::recover(&limiter::exe_dir().join("flowscale.log"));
        // "Always run as administrator": relaunch elevated once; if the UAC prompt is declined, keep running normally.
        if !args.iter().any(|a| a == "-replace" || a == "-noadmin") && !limiter::is_elevated() && config::load().app("start_admin") != 0 {
            let rest: Vec<String> = args.iter().skip(1).map(|a| format!("\"{a}\"")).collect();
            if launch("runas", &format!("-replace {}", rest.join(" ")), false) { return; }
        }
        if limiter::is_elevated() && limiter::rtss_needs_fix() { let _ = limiter::rtss_fix(); }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let store = config::load();
        sync_osd(&store);
        let minimized = args.iter().any(|a| a == "-minimized") || store.app("start_minimized") != 0;
        let _ = LIM.set(Mutex::new(limiter::Limiter::new()));
        let _ = APP.set(Mutex::new(App { hwnd: HWND::default(), store, current: "Default".into(), stop: None, worker: None,
            status: Arc::new(engine::Status { text: Mutex::new(String::new()), tele: Mutex::new(engine::Tele::new()) }), countdown: 0,
            gpus: gpu_names(), displays: display_names(), webview: None, controller: None,
            lim_exe: String::new(), lim_status: (String::new(), vec![]), detector: (String::new(), vec![]), scaling_game: 0, lim_busy: false, last_fg_pid: 0, fg_since: 0, scaling_exe: String::new(),
            games: vec![], games_busy: false, games_msg: (String::new(), vec![]), session: 0, follow: None, lim_overlays: vec![], lim_arch: 0, lim_admin: None }));
        osd::spawn_limiter_window();
        let Ok(hi) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hi.into(), lpszClassName: w!("FlowScaleMain"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(Some(hi.into()), PCWSTR(1 as _)).or_else(|_| LoadIconW(None, IDI_APPLICATION)).unwrap_or_default(), hbrBackground: CreateSolidBrush(COLORREF(0x00FFFFFF)), ..Default::default() };
        RegisterClassW(&wc);
        let d = GetDpiForSystem().max(96) as i32;
        let Ok(h) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("FlowScaleMain"), w!("FlowScale"), WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT, CW_USEDEFAULT, 1120 * d / 96, 740 * d / 96, None, None, Some(hi.into()), None) else { return };
        watchdog::start(h);
        if let Some(i) = args.iter().position(|a| a == "-scale") {
            let needle = args.get(i + 1).cloned().unwrap_or_default().to_lowercase();
            let secs: u64 = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
            if let Some((g, _, _)) = candidate_windows().into_iter().find(|(_, _, t)| t.to_lowercase().contains(&needle)) {
                let _ = SetForegroundWindow(g);
                start_scaling(g);
                if secs > 0 { let hh = h.0 as isize; std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_secs(secs)); watchdog::exit_asked(); let _ = PostMessageW(Some(HWND(hh as *mut c_void)), WM_COMMAND, WPARAM(CMD_EXIT), LPARAM(0)); }); }
            } else { log(&format!("-scale: no window matching '{needle}'")); return; }
        } else if minimized { set_ui_active(false); } else { let _ = ShowWindow(h, SW_SHOW); }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() { let _ = TranslateMessage(&msg); DispatchMessageW(&msg); }
    }
    // Leave at once: a driver or WebView2 thread must never keep a closed FlowScale alive.
    log("exit: process ends");
    std::process::exit(0);
}
