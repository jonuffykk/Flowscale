//! In-game DLL: frame limiter hooks for every Windows presentation path, plus the optional Vulkan layer.
//!
//! Paths: DXGI (D3D10/11/12: Present, Present1), Direct3D 9 / 9Ex (device and swap-chain Present, PresentEx),
//! Direct3D 8 (Present), DirectDraw (primary-surface Flip, Blt, BltFast), OpenGL (wglSwapBuffers, gdi32 SwapBuffers,
//! wglSwapLayerBuffers, wglSwapMultipleBuffers) and Vulkan (directly in vulkan-1.dll, so it works with a game that is
//! already running, and as an implicit layer for games that resolve vkQueuePresentKHR before injection).
//! Nested presents (SwapBuffers calling wglSwapBuffers, the direct Vulkan hook calling the layer) pace exactly once.
//! On request the DLL removes every hook, waits for in-flight calls to drain and unloads itself.
#![allow(non_snake_case, clippy::missing_safety_doc)]
#[allow(dead_code)] #[path = "pacer.rs"] mod pacer;
#[allow(dead_code)] #[path = "ipc.rs"] mod ipc;

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::core::{s, w, Interface, BOOL, GUID, HRESULT};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use ipc::*;
use pacer::{qpc, Pacer};

static PACER: Mutex<Option<Pacer>> = Mutex::new(None);
static LAST_FRAME: AtomicI64 = AtomicI64::new(0);
static LAST_VK_LAYER: AtomicI64 = AtomicI64::new(0);
static FREQ: OnceLock<i64> = OnceLock::new();
fn freq() -> i64 { *FREQ.get_or_init(pacer::qpf) }

// ---------------------------------------------------------------- shared memory
static SHARED_PTR: std::sync::atomic::AtomicPtr<Shared> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static SHARED_RETRY: AtomicI64 = AtomicI64::new(0);
fn shared() -> Option<&'static Shared> {
    let p = SHARED_PTR.load(Relaxed);
    if !p.is_null() { return Some(unsafe { &*p }); }
    let now = qpc(); let last = SHARED_RETRY.load(Relaxed);
    if last != 0 && now - last < freq() { return None; }
    SHARED_RETRY.store(now, Relaxed);
    let m = ipc::Mapping::open()?;
    let ptr = m.ptr as *mut Shared;
    std::mem::forget(m);
    SHARED_PTR.store(ptr, Relaxed);
    Some(unsafe { &*ptr })
}
fn targeted(s: &Shared) -> bool { s.target_pid.load(Relaxed) == std::process::id() }

// ---------------------------------------------------------------- in-flight tracking
/// Calls currently executing inside one of our detours (unload waits for zero).
static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Set once hooks are being removed: detours then pass straight through.
static DETACHING: AtomicBool = AtomicBool::new(false);
thread_local! { static DEPTH: Cell<u32> = const { Cell::new(0) }; }
/// Entered at the top of every detour. `outer` is false for a present nested inside another present.
struct Call { outer: bool }
impl Call {
    fn enter() -> Call {
        INFLIGHT.fetch_add(1, Relaxed);
        let d = DEPTH.with(|c| { let v = c.get(); c.set(v + 1); v });
        Call { outer: d == 0 && !DETACHING.load(Relaxed) }
    }
}
impl Drop for Call { fn drop(&mut self) { DEPTH.with(|c| c.set(c.get().saturating_sub(1))); INFLIGHT.fetch_sub(1, Relaxed); } }

// ---------------------------------------------------------------- pacing
fn pace(api: u32, w: u32, h: u32, before: bool) {
    // Vulkan and OpenGL frames can reach the screen through a DXGI swap chain the driver makes (NVIDIA's layered
    // presentation, interop presenters): that DXGI present is the same frame and must not be paced a second time.
    let Some(s) = shared() else { return };
    if api == API_VK || api == API_GL { s.last_vk_qpc.store(qpc() as u64, Relaxed); }
    if !targeted(s) { return; }
    let me = pace as *const () as u64; // unique per loaded copy of this DLL
    let owner = s.pace_owner.load(Relaxed);
    if owner != me {
        if owner != 0 && qpc() - (s.heartbeat.load(Relaxed) as i64) < freq() / 2 { return; }
        s.pace_owner.store(me, Relaxed);
    }
    let mode = s.mode.load(Relaxed);
    if (mode == MODE_LOW_LATENCY) == before { return; }
    let enabled = s.enabled.load(Relaxed) != 0;
    let mfps = if enabled { s.milli_fps.load(Relaxed) } else { 0 };
    {
        let mut p = PACER.lock().unwrap_or_else(|e| e.into_inner());
        let p = p.get_or_insert_with(Pacer::new);
        p.set_rate(mfps);
        let st = p.limit();
        if mfps > 0 {
            s.paced.fetch_add(1, Relaxed);
            if st.late { s.late_frames.fetch_add(1, Relaxed); }
            if st.slept_ms > 0.0 || st.spun_ms > 0.0 { s.limited.fetch_add(1, Relaxed); }
            if st.slept_ms > 0.0 {
                let us = (st.wake_err_ms.max(0.0) * 1000.0) as u64;
                s.wake_us_sum.fetch_add(us, Relaxed); s.wake_n.fetch_add(1, Relaxed); s.wake_us_max.fetch_max(us as u32, Relaxed);
            }
        }
        s.spin_us.store((p.spin_ms * 1000.0) as u32, Relaxed);
    }
    let now = qpc();
    let last = LAST_FRAME.swap(now, Relaxed);
    if last != 0 {
        let us = ((now - last) as i128 * 1_000_000 / freq() as i128).clamp(0, u32::MAX as i128) as u32;
        if us < 2_000_000 { s.push_frame(us); }
    }
    s.attached_pid.store(std::process::id(), Relaxed);
    s.api.store(api, Relaxed);
    if w > 0 { s.width.store(w, Relaxed); s.height.store(h, Relaxed); }
    s.heartbeat.store(now as u64, Relaxed);
}

/// Several swap chains can present (launchers, tool windows, video): pace only the largest active one.
struct Election { primary: usize, area: u64, seen: i64, cache: HashMap<usize, (u32, u32, u32)> }
static ELECTION: Mutex<Option<Election>> = Mutex::new(None);
fn is_primary(key: usize, size: impl FnOnce() -> (u32, u32)) -> (bool, u32, u32) {
    let mut g = ELECTION.lock().unwrap_or_else(|e| e.into_inner());
    let e = g.get_or_insert_with(|| Election { primary: 0, area: 0, seen: 0, cache: HashMap::new() });
    let now = qpc();
    let entry = e.cache.entry(key).or_insert((0, 0, 0));
    if entry.2 == 0 { let (w, h) = size(); *entry = (w, h, 120); } else { entry.2 -= 1; }
    let (w, h, _) = *entry;
    let area = w as u64 * h as u64;
    if key == e.primary || now - e.seen > freq() || area > e.area {
        e.primary = key; e.area = area; e.seen = now;
        if e.cache.len() > 64 { e.cache.clear(); }
        (true, w, h)
    } else { (false, w, h) }
}

fn hlog(msg: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("flowscale_hook.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = f.write_all(format!("[pid {}] {msg}\r\n", std::process::id()).as_bytes()); }
}
fn mark_hook(bit: u32) { if let Some(s) = shared() { s.hooks.fetch_or(bit, Relaxed); } }

// ---------------------------------------------------------------- hook bookkeeping
/// Patched vtable slots (slot address, original pointer) and MinHook targets, so detach can undo everything.
static SLOTS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static DETOURS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe fn patch_slot(slot: *mut usize, new: usize) -> usize {
    unsafe {
        let mut old = PAGE_PROTECTION_FLAGS(0);
        if VirtualProtect(slot as *const c_void, size_of::<usize>(), PAGE_EXECUTE_READWRITE, &mut old).is_err() { return 0; }
        let orig = *slot;
        if orig != new { *slot = new; }
        let _ = VirtualProtect(slot as *const c_void, size_of::<usize>(), old, &mut old);
        orig
    }
}
unsafe fn vtable(obj: *mut c_void) -> *mut usize { unsafe { *(obj as *const *mut usize) } }
unsafe fn hook_vtable(vt: *mut usize, idx: usize, hook: usize, store: &Mutex<HashMap<usize, usize>>) {
    let slot = unsafe { vt.add(idx) };
    if unsafe { *slot } == hook { return; }
    let orig = unsafe { patch_slot(slot, hook) };
    if orig != 0 {
        store.lock().unwrap().insert(vt as usize, orig);
        SLOTS.lock().unwrap().push((slot as usize, orig));
        hlog(&format!("vtable {:p}[{idx}] {:#x} -> hook", vt, orig));
    }
}
fn orig_of(store: &Mutex<HashMap<usize, usize>>, obj: *mut c_void) -> usize {
    let vt = unsafe { vtable(obj) } as usize;
    let m = store.lock().unwrap();
    m.get(&vt).copied().or_else(|| m.values().next().copied()).unwrap_or(0)
}
/// Inline detour through MinHook; `orig` receives the trampoline. Returns true when installed now or before.
unsafe fn detour(target: usize, hook: usize, orig: &AtomicUsize, what: &str) -> bool {
    if target == 0 { return false; }
    if orig.load(Relaxed) != 0 { return true; }
    match unsafe { minhook::MinHook::create_hook(target as *mut c_void, hook as *mut c_void) } {
        Ok(o) => {
            orig.store(o as usize, Relaxed);
            let _ = unsafe { minhook::MinHook::enable_hook(target as *mut c_void) };
            DETOURS.lock().unwrap().push(target);
            hlog(&format!("detour {what} {target:#x} ok")); true
        }
        Err(e) => { hlog(&format!("detour {what} {target:#x} failed {e:?}")); false }
    }
}
fn module(name: windows::core::PCWSTR) -> Option<HMODULE> { unsafe { GetModuleHandleW(name).ok() } }
fn export(m: HMODULE, name: windows::core::PCSTR) -> usize { unsafe { GetProcAddress(m, name).map(|p| p as usize).unwrap_or(0) } }

// ---------------------------------------------------------------- DXGI (D3D10 / 11 / 12)
static DXGI_PRESENT: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
static DXGI_PRESENT1: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
static DXGI_KIND: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);
fn map(m: &'static OnceLock<Mutex<HashMap<usize, usize>>>) -> &'static Mutex<HashMap<usize, usize>> { m.get_or_init(|| Mutex::new(HashMap::new())) }

fn dxgi_pace(this: *mut c_void, flags: u32, before: bool) {
    if flags & DXGI_PRESENT_TEST.0 != 0 { return; }
    let vk = shared().map(|s| s.last_vk_qpc.load(Relaxed) as i64).unwrap_or(0).max(LAST_VK_LAYER.load(Relaxed));
    if qpc() - vk < freq() / 2 { return; } // a Vulkan / OpenGL frame presented again through DXGI: paced once, there
    let (prim, w, h) = is_primary(this as usize, || unsafe {
        IDXGISwapChain::from_raw_borrowed(&this).and_then(|sc| sc.GetDesc().ok()).map(|d| (d.BufferDesc.Width, d.BufferDesc.Height)).unwrap_or((0, 0))
    });
    if !prim { return; }
    let api = *DXGI_KIND.lock().unwrap().get_or_insert_with(HashMap::new).entry(this as usize).or_insert_with(|| unsafe {
        match IDXGISwapChain::from_raw_borrowed(&this) {
            Some(sc) if sc.GetDevice::<ID3D12Device>().is_ok() || sc.GetDevice::<ID3D12CommandQueue>().is_ok() => API_D3D12,
            Some(sc) if sc.GetDevice::<ID3D11Device>().is_ok() => API_D3D11,
            _ => if module(w!("d3d12.dll")).is_some() && module(w!("d3d11.dll")).is_none() { API_D3D12 } else { API_D3D11 },
        }
    });
    pace(api, w, h, before);
}
unsafe extern "system" fn hk_present(this: *mut c_void, sync: u32, flags: u32) -> HRESULT {
    let c = Call::enter();
    if c.outer { dxgi_pace(this, flags, true); }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT = unsafe { std::mem::transmute(orig_of(map(&DXGI_PRESENT), this)) };
    let r = unsafe { f(this, sync, flags) };
    if c.outer { dxgi_pace(this, flags, false); }
    r
}
unsafe extern "system" fn hk_present1(this: *mut c_void, sync: u32, flags: u32, params: *const c_void) -> HRESULT {
    let c = Call::enter();
    if c.outer { dxgi_pace(this, flags, true); }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> HRESULT = unsafe { std::mem::transmute(orig_of(map(&DXGI_PRESENT1), this)) };
    let r = unsafe { f(this, sync, flags, params) };
    if c.outer { dxgi_pace(this, flags, false); }
    r
}

unsafe extern "system" fn dummy_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT { unsafe { DefWindowProcW(h, m, w, l) } }
fn dummy_window() -> HWND {
    unsafe {
        let hi = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(dummy_proc), hInstance: hi.into(), lpszClassName: w!("FlowScaleHookDummy"), ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(WINDOW_EX_STYLE(0), w!("FlowScaleHookDummy"), w!(""), WS_OVERLAPPED, 0, 0, 64, 64, None, None, Some(hi.into()), None).unwrap_or_default()
    }
}

unsafe fn hook_dxgi(hwnd: HWND) {
    unsafe {
        let desc = DXGI_SWAP_CHAIN_DESC { BufferDesc: DXGI_MODE_DESC { Width: 64, Height: 64, Format: DXGI_FORMAT_R8G8B8A8_UNORM, ..Default::default() }, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2, OutputWindow: hwnd, Windowed: true.into(), SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, ..Default::default() };
        let mut sc = None;
        if D3D11CreateDeviceAndSwapChain(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION, Some(&desc), Some(&mut sc), None, None, None).is_ok() {
            if let Some(sc) = sc { let vt = vtable(sc.as_raw()); hook_vtable(vt, 8, hk_present as *const () as usize, map(&DXGI_PRESENT)); hook_vtable(vt, 22, hk_present1 as *const () as usize, map(&DXGI_PRESENT1)); mark_hook(HOOK_DXGI); }
        }
        // D3D12 swap chains are a different implementation with their own vtable.
        if module(w!("d3d12.dll")).is_some() {
            let mut dev: Option<ID3D12Device> = None;
            if D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut dev).is_ok() {
                let dev = dev.unwrap();
                if let Ok(q) = dev.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC::default()) {
                    if let Ok(f) = CreateDXGIFactory2::<IDXGIFactory2>(DXGI_CREATE_FACTORY_FLAGS(0)) {
                        let d = DXGI_SWAP_CHAIN_DESC1 { Width: 64, Height: 64, Format: DXGI_FORMAT_R8G8B8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2, SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, ..Default::default() };
                        if let Ok(sc) = f.CreateSwapChainForHwnd(&q, hwnd, &d, None, None) {
                            let vt = vtable(sc.as_raw());
                            hook_vtable(vt, 8, hk_present as *const () as usize, map(&DXGI_PRESENT)); hook_vtable(vt, 22, hk_present1 as *const () as usize, map(&DXGI_PRESENT1));
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- Direct3D 9
static D9_PRESENT: AtomicUsize = AtomicUsize::new(0);
static D9_PRESENTEX: AtomicUsize = AtomicUsize::new(0);
static D9_SCPRESENT: AtomicUsize = AtomicUsize::new(0);
fn d9_pace(this: *mut c_void, api: u32, before: bool) { let (p, w, h) = is_primary(this as usize, || (0, 0)); if p { pace(api, w, h, before); } }
type Present5 = unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, HWND, *const c_void) -> HRESULT;
type Present6 = unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, HWND, *const c_void, u32) -> HRESULT;
unsafe extern "system" fn hk_d9_present(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void) -> HRESULT {
    let g = Call::enter();
    if g.outer { d9_pace(this, API_D3D9, true); }
    let f: Present5 = unsafe { std::mem::transmute(D9_PRESENT.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d) }; if g.outer { d9_pace(this, API_D3D9, false); } r
}
unsafe extern "system" fn hk_d9_presentex(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void, fl: u32) -> HRESULT {
    let g = Call::enter();
    if g.outer { d9_pace(this, API_D3D9, true); }
    let f: Present6 = unsafe { std::mem::transmute(D9_PRESENTEX.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d, fl) }; if g.outer { d9_pace(this, API_D3D9, false); } r
}
unsafe extern "system" fn hk_d9_scpresent(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void, fl: u32) -> HRESULT {
    let g = Call::enter();
    if g.outer { d9_pace(this, API_D3D9, true); }
    let f: Present6 = unsafe { std::mem::transmute(D9_SCPRESENT.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d, fl) }; if g.outer { d9_pace(this, API_D3D9, false); } r
}
unsafe fn hook_d3d9(hwnd: HWND) {
    unsafe {
        if module(w!("d3d9.dll")).is_none() || D9_PRESENT.load(Relaxed) != 0 { return; }
        let mut pp = D3DPRESENT_PARAMETERS { Windowed: true.into(), SwapEffect: D3DSWAPEFFECT_DISCARD, hDeviceWindow: hwnd, BackBufferFormat: D3DFMT_UNKNOWN, ..Default::default() };
        let flags = (D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_DISABLE_DRIVER_MANAGEMENT) as u32;
        let Ok(d3d) = Direct3DCreate9Ex(D3D_SDK_VERSION) else { return };
        let mut dev = None;
        if d3d.CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, flags, &mut pp, std::ptr::null_mut(), &mut dev).is_err() { hlog("d3d9: dummy device failed"); return; }
        let Some(dev) = dev else { return };
        let vt = vtable(dev.as_raw());
        let ok = detour(*vt.add(17), hk_d9_present as *const () as usize, &D9_PRESENT, "IDirect3DDevice9::Present");
        detour(*vt.add(121), hk_d9_presentex as *const () as usize, &D9_PRESENTEX, "IDirect3DDevice9Ex::PresentEx");
        if let Ok(sc) = dev.GetSwapChain(0) { detour(*vtable(sc.as_raw()).add(3), hk_d9_scpresent as *const () as usize, &D9_SCPRESENT, "IDirect3DSwapChain9::Present"); }
        if ok { mark_hook(HOOK_D3D9); }
    }
}

// ---------------------------------------------------------------- Direct3D 8 (raw COM; not in the windows crate)
static D8_PRESENT: AtomicUsize = AtomicUsize::new(0);
unsafe extern "system" fn hk_d8_present(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void) -> HRESULT {
    let g = Call::enter();
    if g.outer { d9_pace(this, API_D3D8, true); }
    let f: Present5 = unsafe { std::mem::transmute(D8_PRESENT.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d) }; if g.outer { d9_pace(this, API_D3D8, false); } r
}
#[repr(C)] #[derive(Default)]
struct D3dpp8 { w: u32, h: u32, format: u32, count: u32, multisample: u32, swap_effect: u32, hwnd: isize, windowed: i32, auto_ds: i32, ds_format: u32, flags: u32, refresh: u32, interval: u32 }
unsafe fn hook_d3d8(hwnd: HWND) {
    unsafe {
        let Some(m) = module(w!("d3d8.dll")) else { return };
        if D8_PRESENT.load(Relaxed) != 0 { return; }
        let create = export(m, s!("Direct3DCreate8"));
        if create == 0 { return; }
        let create: unsafe extern "system" fn(u32) -> *mut c_void = std::mem::transmute(create);
        let d3d = create(220);
        if d3d.is_null() { return; }
        let vt = vtable(d3d);
        // IDirect3D8::GetAdapterDisplayMode (8) gives the desktop format a windowed device must use; CreateDevice is 15.
        let mut mode = [0u32; 4];
        let gadm: unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> HRESULT = std::mem::transmute(*vt.add(8));
        let _ = gadm(d3d, 0, mode.as_mut_ptr());
        let mut pp = D3dpp8 { w: 64, h: 64, format: mode[3], count: 1, swap_effect: 1, hwnd: hwnd.0 as isize, windowed: 1, ..Default::default() };
        let cd: unsafe extern "system" fn(*mut c_void, u32, u32, HWND, u32, *mut D3dpp8, *mut *mut c_void) -> HRESULT = std::mem::transmute(*vt.add(15));
        let mut dev: *mut c_void = std::ptr::null_mut();
        if cd(d3d, 0, 1, hwnd, 0x20, &mut pp, &mut dev).is_ok() && !dev.is_null() {
            if detour(*vtable(dev).add(15), hk_d8_present as *const () as usize, &D8_PRESENT, "IDirect3DDevice8::Present") { mark_hook(HOOK_D3D8); }
            let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtable(dev).add(2)); rel(dev);
        } else { hlog("d3d8: dummy device failed"); }
        let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vt.add(2)); rel(d3d);
    }
}

// ---------------------------------------------------------------- DirectDraw (raw COM)
// Surface vtable (all versions share the head): Blt 5, BltFast 7, Flip 11, GetCaps 14. Pacing happens on Flip, and on
// Blt / BltFast only when the destination is the primary surface (windowed DirectDraw games blit each frame to it).
static DD_BLT: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static DD_BLTFAST: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static DD_FLIP: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
const DDSCAPS_PRIMARYSURFACE: u32 = 0x200;
unsafe fn dd_is_primary(surf: *mut c_void) -> bool {
    unsafe {
        if surf.is_null() { return false; }
        let mut caps = [0u32; 4];
        let gc: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT = std::mem::transmute(*vtable(surf).add(14));
        gc(surf, caps.as_mut_ptr()).is_ok() && caps[0] & DDSCAPS_PRIMARYSURFACE != 0
    }
}
macro_rules! dd_hooks {
    ($blt:ident, $fast:ident, $flip:ident, $i:expr) => {
        unsafe extern "system" fn $blt(this: *mut c_void, r: *const c_void, src: *mut c_void, sr: *const c_void, fl: u32, fx: *const c_void) -> HRESULT {
            let g = Call::enter();
            let paced = g.outer && unsafe { dd_is_primary(this) };
            if paced { d9_pace(this, API_DDRAW, true); }
            let f: unsafe extern "system" fn(*mut c_void, *const c_void, *mut c_void, *const c_void, u32, *const c_void) -> HRESULT = unsafe { std::mem::transmute(DD_BLT[$i].load(Relaxed)) };
            let res = unsafe { f(this, r, src, sr, fl, fx) }; if paced { d9_pace(this, API_DDRAW, false); } res
        }
        unsafe extern "system" fn $fast(this: *mut c_void, x: u32, y: u32, src: *mut c_void, sr: *const c_void, tr: u32) -> HRESULT {
            let g = Call::enter();
            let paced = g.outer && unsafe { dd_is_primary(this) };
            if paced { d9_pace(this, API_DDRAW, true); }
            let f: unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *const c_void, u32) -> HRESULT = unsafe { std::mem::transmute(DD_BLTFAST[$i].load(Relaxed)) };
            let res = unsafe { f(this, x, y, src, sr, tr) }; if paced { d9_pace(this, API_DDRAW, false); } res
        }
        unsafe extern "system" fn $flip(this: *mut c_void, target: *mut c_void, fl: u32) -> HRESULT {
            let g = Call::enter();
            if g.outer { d9_pace(this, API_DDRAW, true); }
            let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT = unsafe { std::mem::transmute(DD_FLIP[$i].load(Relaxed)) };
            let res = unsafe { f(this, target, fl) }; if g.outer { d9_pace(this, API_DDRAW, false); } res
        }
    };
}
dd_hooks!(hk_dd_blt1, hk_dd_fast1, hk_dd_flip1, 0);
dd_hooks!(hk_dd_blt7, hk_dd_fast7, hk_dd_flip7, 1);
const IID_IDIRECTDRAWSURFACE7: GUID = GUID::from_u128(0x06675a80_3b9b_11d2_b92f_00609797ea5b);
unsafe fn hook_ddraw(hwnd: HWND) {
    unsafe {
        let Some(m) = module(w!("ddraw.dll")) else { return };
        if DD_FLIP[0].load(Relaxed) != 0 { return; }
        let create = export(m, s!("DirectDrawCreate"));
        if create == 0 { return; }
        let create: unsafe extern "system" fn(*const GUID, *mut *mut c_void, *mut c_void) -> HRESULT = std::mem::transmute(create);
        let mut dd: *mut c_void = std::ptr::null_mut();
        if create(std::ptr::null(), &mut dd, std::ptr::null_mut()).is_err() || dd.is_null() { hlog("ddraw: DirectDrawCreate failed"); return; }
        let vt = vtable(dd);
        let scl: unsafe extern "system" fn(*mut c_void, HWND, u32) -> HRESULT = std::mem::transmute(*vt.add(20));
        let _ = scl(dd, hwnd, 0x8); // DDSCL_NORMAL
        // DDSURFACEDESC holds a pointer (lpSurface): 108 bytes with ddsCaps at 104 on x86, 120 with ddsCaps at 112 on x64.
        let (size, caps) = if cfg!(target_pointer_width = "64") { (120usize, 112usize) } else { (108, 104) };
        let mut desc = [0u8; 120];
        // A small off-screen surface: the game may already own the primary, and every surface type shares the
        // same Blt / BltFast / Flip implementation, which is what gets detoured.
        desc[0..4].copy_from_slice(&(size as u32).to_le_bytes()); desc[4..8].copy_from_slice(&(1u32 | 2 | 4).to_le_bytes()); // DDSD_CAPS | HEIGHT | WIDTH
        desc[8..12].copy_from_slice(&16u32.to_le_bytes()); desc[12..16].copy_from_slice(&16u32.to_le_bytes());
        desc[caps..caps + 4].copy_from_slice(&0x40u32.to_le_bytes()); // DDSCAPS_OFFSCREENPLAIN
        let cs: unsafe extern "system" fn(*mut c_void, *mut u8, *mut *mut c_void, *mut c_void) -> HRESULT = std::mem::transmute(*vt.add(6));
        let mut surf: *mut c_void = std::ptr::null_mut();
        if cs(dd, desc.as_mut_ptr(), &mut surf, std::ptr::null_mut()).is_ok() && !surf.is_null() {
            let sv = vtable(surf);
            let ok = detour(*sv.add(11), hk_dd_flip1 as *const () as usize, &DD_FLIP[0], "IDirectDrawSurface::Flip");
            detour(*sv.add(5), hk_dd_blt1 as *const () as usize, &DD_BLT[0], "IDirectDrawSurface::Blt");
            detour(*sv.add(7), hk_dd_fast1 as *const () as usize, &DD_BLTFAST[0], "IDirectDrawSurface::BltFast");
            // IDirectDrawSurface7 is a separate implementation in ddraw.dll.
            let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT = std::mem::transmute(*sv.add(0));
            let mut s7: *mut c_void = std::ptr::null_mut();
            if qi(surf, &IID_IDIRECTDRAWSURFACE7, &mut s7).is_ok() && !s7.is_null() {
                let v7 = vtable(s7);
                if *v7.add(11) != *sv.add(11) { detour(*v7.add(11), hk_dd_flip7 as *const () as usize, &DD_FLIP[1], "IDirectDrawSurface7::Flip"); }
                if *v7.add(5) != *sv.add(5) { detour(*v7.add(5), hk_dd_blt7 as *const () as usize, &DD_BLT[1], "IDirectDrawSurface7::Blt"); }
                if *v7.add(7) != *sv.add(7) { detour(*v7.add(7), hk_dd_fast7 as *const () as usize, &DD_BLTFAST[1], "IDirectDrawSurface7::BltFast"); }
                let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*v7.add(2)); rel(s7);
            }
            if ok { mark_hook(HOOK_DDRAW); }
            let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*sv.add(2)); rel(surf);
        } else { hlog("ddraw: dummy surface failed"); }
        let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vt.add(2)); rel(dd);
    }
}

// ---------------------------------------------------------------- OpenGL
static GL_SWAP: AtomicUsize = AtomicUsize::new(0);
static GDI_SWAP: AtomicUsize = AtomicUsize::new(0);
static GL_SWAP_LAYER: AtomicUsize = AtomicUsize::new(0);
static GL_SWAP_MULTI: AtomicUsize = AtomicUsize::new(0);
fn gl_size(hdc: HDC) -> (u32, u32) {
    unsafe {
        let h = windows::Win32::Graphics::Gdi::WindowFromDC(hdc);
        let mut r = RECT::default();
        if !h.is_invalid() && GetClientRect(h, &mut r).is_ok() { ((r.right - r.left) as u32, (r.bottom - r.top) as u32) } else { (0, 0) }
    }
}
unsafe extern "system" fn hk_wglswapbuffers(hdc: HDC) -> BOOL {
    let g = Call::enter();
    let (w, h) = if g.outer { gl_size(hdc) } else { (0, 0) };
    if g.outer { pace(API_GL, w, h, true); }
    let f: unsafe extern "system" fn(HDC) -> BOOL = unsafe { std::mem::transmute(GL_SWAP.load(Relaxed)) };
    let r = unsafe { f(hdc) }; if g.outer { pace(API_GL, w, h, false); } r
}
unsafe extern "system" fn hk_gdi_swapbuffers(hdc: HDC) -> BOOL {
    let g = Call::enter();
    let (w, h) = if g.outer { gl_size(hdc) } else { (0, 0) };
    if g.outer { pace(API_GL, w, h, true); }
    let f: unsafe extern "system" fn(HDC) -> BOOL = unsafe { std::mem::transmute(GDI_SWAP.load(Relaxed)) };
    let r = unsafe { f(hdc) }; if g.outer { pace(API_GL, w, h, false); } r
}
unsafe extern "system" fn hk_wglswaplayer(hdc: HDC, planes: u32) -> BOOL {
    let g = Call::enter();
    // Only the main plane is a frame; overlay/underlay plane swaps are not.
    let main = planes & 1 != 0 && g.outer;
    if main { pace(API_GL, 0, 0, true); }
    let f: unsafe extern "system" fn(HDC, u32) -> BOOL = unsafe { std::mem::transmute(GL_SWAP_LAYER.load(Relaxed)) };
    let r = unsafe { f(hdc, planes) }; if main { pace(API_GL, 0, 0, false); } r
}
unsafe extern "system" fn hk_wglswapmulti(n: u32, list: *const c_void) -> u32 {
    let g = Call::enter();
    if g.outer && n > 0 { pace(API_GL, 0, 0, true); }
    let f: unsafe extern "system" fn(u32, *const c_void) -> u32 = unsafe { std::mem::transmute(GL_SWAP_MULTI.load(Relaxed)) };
    let r = unsafe { f(n, list) }; if g.outer && n > 0 { pace(API_GL, 0, 0, false); } r
}
unsafe fn hook_gl() {
    unsafe {
        let Some(gl) = module(w!("opengl32.dll")) else { return };
        let mut ok = detour(export(gl, s!("wglSwapBuffers")), hk_wglswapbuffers as *const () as usize, &GL_SWAP, "wglSwapBuffers");
        ok |= detour(export(gl, s!("wglSwapLayerBuffers")), hk_wglswaplayer as *const () as usize, &GL_SWAP_LAYER, "wglSwapLayerBuffers");
        ok |= detour(export(gl, s!("wglSwapMultipleBuffers")), hk_wglswapmulti as *const () as usize, &GL_SWAP_MULTI, "wglSwapMultipleBuffers");
        // Most games call gdi32!SwapBuffers, which forwards to opengl32; the depth guard keeps it to one pace.
        if let Some(gdi) = module(w!("gdi32.dll")) { ok |= detour(export(gdi, s!("SwapBuffers")), hk_gdi_swapbuffers as *const () as usize, &GDI_SWAP, "gdi32!SwapBuffers"); }
        if ok { mark_hook(HOOK_GL); }
    }
}

// ---------------------------------------------------------------- Vulkan, direct (no layer, no restart)
static VK_PRESENT_EXPORT: AtomicUsize = AtomicUsize::new(0);
static VK_GDPA: AtomicUsize = AtomicUsize::new(0);
static VK_GIPA: AtomicUsize = AtomicUsize::new(0);
/// Real vkQueuePresentKHR per device dispatch key, captured when the game resolves it through vkGetDeviceProcAddr.
static VK_REAL_PRESENT: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);
type Gpa = unsafe extern "system" fn(*mut c_void, *const c_char) -> *const c_void;
type QueuePresent = unsafe extern "system" fn(*mut c_void, *const c_void) -> i32;
fn dispatch_key(h: *mut c_void) -> usize { unsafe { *(h as *const usize) } }
fn name_is(n: *const c_char, s: &str) -> bool { !n.is_null() && unsafe { CStr::from_ptr(n).to_bytes() == s.as_bytes() } }

unsafe extern "system" fn hk_vk_present_export(queue: *mut c_void, info: *const c_void) -> i32 {
    let g = Call::enter();
    if g.outer { pace(API_VK, 0, 0, true); }
    let f: QueuePresent = unsafe { std::mem::transmute(VK_PRESENT_EXPORT.load(Relaxed)) };
    let r = unsafe { f(queue, info) }; if g.outer { pace(API_VK, 0, 0, false); } r
}
unsafe extern "system" fn hk_vk_present_device(queue: *mut c_void, info: *const c_void) -> i32 {
    let g = Call::enter();
    let real = VK_REAL_PRESENT.lock().unwrap().as_ref().and_then(|m| m.get(&dispatch_key(queue)).copied()).unwrap_or(0);
    if real == 0 { return -3; } // VK_ERROR_INITIALIZATION_FAILED: never reached for a queue of a device we resolved
    if g.outer { pace(API_VK, 0, 0, true); }
    let f: QueuePresent = unsafe { std::mem::transmute(real) };
    let r = unsafe { f(queue, info) }; if g.outer { pace(API_VK, 0, 0, false); } r
}
unsafe extern "system" fn hk_vk_gdpa(dev: *mut c_void, name: *const c_char) -> *const c_void {
    let f: Gpa = unsafe { std::mem::transmute(VK_GDPA.load(Relaxed)) };
    let p = unsafe { f(dev, name) };
    if !p.is_null() && !dev.is_null() && name_is(name, "vkQueuePresentKHR") && !DETACHING.load(Relaxed) {
        VK_REAL_PRESENT.lock().unwrap().get_or_insert_with(HashMap::new).insert(dispatch_key(dev), p as usize);
        return hk_vk_present_device as *const c_void;
    }
    p
}
unsafe extern "system" fn hk_vk_gipa(inst: *mut c_void, name: *const c_char) -> *const c_void {
    let f: Gpa = unsafe { std::mem::transmute(VK_GIPA.load(Relaxed)) };
    let p = unsafe { f(inst, name) };
    if !p.is_null() && name_is(name, "vkGetDeviceProcAddr") && VK_GDPA.load(Relaxed) != 0 && !DETACHING.load(Relaxed) { return hk_vk_gdpa as *const c_void; }
    p
}
static VK_PRESENT_ICD: AtomicUsize = AtomicUsize::new(0);
unsafe extern "system" fn hk_vk_present_icd(queue: *mut c_void, info: *const c_void) -> i32 {
    let g = Call::enter();
    if g.outer { pace(API_VK, 0, 0, true); }
    let f: QueuePresent = unsafe { std::mem::transmute(VK_PRESENT_ICD.load(Relaxed)) };
    let r = unsafe { f(queue, info) }; if g.outer { pace(API_VK, 0, 0, false); } r
}
#[repr(C)] struct VkAppInfo { s_type: i32, next: *const c_void, app: *const c_char, app_ver: u32, engine: *const c_char, engine_ver: u32, api: u32 }
#[repr(C)] struct VkInstInfo { s_type: i32, next: *const c_void, flags: u32, app: *const VkAppInfo, layers: u32, layer_names: *const *const c_char, exts: u32, ext_names: *const *const c_char }
#[repr(C)] struct VkQueueInfo { s_type: i32, next: *const c_void, flags: u32, family: u32, count: u32, prio: *const f32 }
#[repr(C)] struct VkDevInfo { s_type: i32, next: *const c_void, flags: u32, queues: u32, queue_info: *const VkQueueInfo, layers: u32, layer_names: *const *const c_char, exts: u32, ext_names: *const *const c_char, features: *const c_void }
/// The game usually resolved vkQueuePresentKHR before we arrived and calls it directly. A throwaway device on the
/// same driver resolves the very same function (the driver's entry, or the first implicit layer's), which we detour.
unsafe fn hook_vulkan_driver_entry(vk: HMODULE) {
    unsafe {
        if VK_PRESENT_ICD.load(Relaxed) != 0 { return; }
        let gipa: Gpa = match VK_GIPA.load(Relaxed) { 0 => match export(vk, s!("vkGetInstanceProcAddr")) { 0 => return, p => std::mem::transmute(p) }, p => std::mem::transmute(p) };
        let get = |inst: *mut c_void, n: &CStr| gipa(inst, n.as_ptr());
        let create_inst: unsafe extern "system" fn(*const VkInstInfo, *const c_void, *mut *mut c_void) -> i32 = match get(std::ptr::null_mut(), c"vkCreateInstance") { p if p.is_null() => return, p => std::mem::transmute(p) };
        let app = VkAppInfo { s_type: 0, next: std::ptr::null(), app: c"FlowScale probe".as_ptr(), app_ver: 1, engine: std::ptr::null(), engine_ver: 0, api: (1 << 22) | (1 << 12) };
        let iexts = [c"VK_KHR_surface".as_ptr(), c"VK_KHR_win32_surface".as_ptr()];
        let ii = VkInstInfo { s_type: 1, next: std::ptr::null(), flags: 0, app: &app, layers: 0, layer_names: std::ptr::null(), exts: 2, ext_names: iexts.as_ptr() };
        let mut inst: *mut c_void = std::ptr::null_mut();
        if create_inst(&ii, std::ptr::null(), &mut inst) != 0 || inst.is_null() { hlog("vulkan: probe instance failed"); return; }
        let destroy_inst: unsafe extern "system" fn(*mut c_void, *const c_void) = std::mem::transmute(get(inst, c"vkDestroyInstance"));
        let enum_pd: unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32 = std::mem::transmute(get(inst, c"vkEnumeratePhysicalDevices"));
        let create_dev: unsafe extern "system" fn(*mut c_void, *const VkDevInfo, *const c_void, *mut *mut c_void) -> i32 = std::mem::transmute(get(inst, c"vkCreateDevice"));
        // Through the trampoline when our own vkGetDeviceProcAddr detour is in place, or it would answer with our hook.
        let gdpa: Gpa = match VK_GDPA.load(Relaxed) { 0 => std::mem::transmute(get(inst, c"vkGetDeviceProcAddr")), t => std::mem::transmute(t) };
        let mut n = 8u32; let mut pds = [std::ptr::null_mut::<c_void>(); 8];
        let _ = enum_pd(inst, &mut n, pds.as_mut_ptr());
        let prio = 1.0f32;
        let qi = VkQueueInfo { s_type: 2, next: std::ptr::null(), flags: 0, family: 0, count: 1, prio: &prio };
        let dexts = [c"VK_KHR_swapchain".as_ptr()];
        let mine = OWN_MODULE.load(Relaxed);
        // Every GPU's driver has its own entry point; hook each distinct one.
        for pd in pds.iter().take(n as usize).filter(|p| !p.is_null()) {
            let di = VkDevInfo { s_type: 3, next: std::ptr::null(), flags: 0, queues: 1, queue_info: &qi, layers: 0, layer_names: std::ptr::null(), exts: 1, ext_names: dexts.as_ptr(), features: std::ptr::null() };
            let mut dev: *mut c_void = std::ptr::null_mut();
            if create_dev(*pd, &di, std::ptr::null(), &mut dev) != 0 || dev.is_null() { continue; }
            let p = gdpa(dev, c"vkQueuePresentKHR".as_ptr()) as usize;
            let destroy_dev: unsafe extern "system" fn(*mut c_void, *const c_void) = std::mem::transmute(gdpa(dev, c"vkDestroyDevice".as_ptr()));
            // Our own layer or our own hook already catches it: nothing to add.
            let ours = p == vk_queue_present as *const () as usize || p == hk_vk_present_device as *const () as usize
                || (mine != 0 && { let mut m = HMODULE::default(); GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(p as *const u16), &mut m).is_ok() && m.0 as usize == mine });
            hlog(&format!("vulkan: probe device resolves vkQueuePresentKHR to {p:#x} (ours: {ours})"));
            if p != 0 && !ours && VK_PRESENT_ICD.load(Relaxed) == 0 {
                detour(p, hk_vk_present_icd as *const () as usize, &VK_PRESENT_ICD, "vkQueuePresentKHR (driver entry)");
            }
            destroy_dev(dev, std::ptr::null());
        }
        destroy_inst(inst, std::ptr::null());
    }
}
unsafe fn hook_vulkan_direct() {
    unsafe {
        let Some(vk) = module(w!("vulkan-1.dll")) else { return };
        let mut ok = detour(export(vk, s!("vkQueuePresentKHR")), hk_vk_present_export as *const () as usize, &VK_PRESENT_EXPORT, "vulkan-1!vkQueuePresentKHR");
        ok |= detour(export(vk, s!("vkGetDeviceProcAddr")), hk_vk_gdpa as *const () as usize, &VK_GDPA, "vulkan-1!vkGetDeviceProcAddr");
        ok |= detour(export(vk, s!("vkGetInstanceProcAddr")), hk_vk_gipa as *const () as usize, &VK_GIPA, "vulkan-1!vkGetInstanceProcAddr");
        hook_vulkan_driver_entry(vk);
        if ok || VK_PRESENT_ICD.load(Relaxed) != 0 { mark_hook(HOOK_VK); }
    }
}

// ---------------------------------------------------------------- Vulkan implicit layer (registered by FlowScale)
#[repr(C)] struct LayerInstanceLink { next: *mut LayerInstanceLink, gipa: Gpa, gpdpa: *const c_void }
#[repr(C)] struct LayerDeviceLink { next: *mut LayerDeviceLink, gipa: Gpa, gdpa: Gpa }
#[repr(C)] struct LayerCreateInfo { s_type: i32, p_next: *const c_void, function: i32, link: *mut c_void }
#[repr(C)] pub struct NegotiateLayerInterface { s_type: i32, p_next: *mut c_void, version: u32, gipa: Option<Gpa>, gdpa: Option<Gpa>, gpdpa: *const c_void }
#[derive(Clone, Copy)] struct DevFns { gdpa: Gpa, present: usize }
static VK_NEXT_GIPA: Mutex<Option<Gpa>> = Mutex::new(None);
static VK_DEV: Mutex<Option<HashMap<usize, DevFns>>> = Mutex::new(None);

unsafe fn find_link(mut p: *const c_void, s_type: i32) -> *mut LayerCreateInfo {
    unsafe {
        while !p.is_null() {
            let ci = p as *mut LayerCreateInfo;
            if (*ci).s_type == s_type && (*ci).function == 0 { return ci; }
            p = (*ci).p_next;
        }
        std::ptr::null_mut()
    }
}
unsafe extern "system" fn vk_create_instance(ci: *const c_void, alloc: *const c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        let info = find_link((*(ci as *const LayerCreateInfo)).p_next, 47);
        if info.is_null() { return -3; }
        let link = (*info).link as *mut LayerInstanceLink;
        let next = (*link).gipa;
        (*info).link = (*link).next as *mut c_void;
        let create: unsafe extern "system" fn(*const c_void, *const c_void, *mut *mut c_void) -> i32 = std::mem::transmute(next(std::ptr::null_mut(), c"vkCreateInstance".as_ptr()));
        *VK_NEXT_GIPA.lock().unwrap() = Some(next);
        create(ci, alloc, out)
    }
}
unsafe extern "system" fn vk_create_device(pd: *mut c_void, ci: *const c_void, alloc: *const c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        let info = find_link((*(ci as *const LayerCreateInfo)).p_next, 48);
        if info.is_null() { return -3; }
        let link = (*info).link as *mut LayerDeviceLink;
        let (gipa, gdpa) = ((*link).gipa, (*link).gdpa);
        (*info).link = (*link).next as *mut c_void;
        let create: unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, *mut *mut c_void) -> i32 = std::mem::transmute(gipa(std::ptr::null_mut(), c"vkCreateDevice".as_ptr()));
        let r = create(pd, ci, alloc, out);
        if r == 0 {
            let dev = *out;
            let present = gdpa(dev, c"vkQueuePresentKHR".as_ptr()) as usize;
            VK_DEV.lock().unwrap().get_or_insert_with(HashMap::new).insert(dispatch_key(dev), DevFns { gdpa, present });
            mark_hook(HOOK_VK_LAYER);
        }
        r
    }
}
unsafe extern "system" fn vk_queue_present(queue: *mut c_void, info: *const c_void) -> i32 {
    let g = Call::enter();
    let fns = VK_DEV.lock().unwrap().as_ref().and_then(|m| m.get(&dispatch_key(queue)).copied());
    let Some(fns) = fns else { return -3 };
    LAST_VK_LAYER.store(qpc(), Relaxed);
    if g.outer { pace(API_VK, 0, 0, true); }
    let f: QueuePresent = unsafe { std::mem::transmute(fns.present) };
    let r = unsafe { f(queue, info) };
    if g.outer { pace(API_VK, 0, 0, false); }
    r
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceProcAddr(dev: *mut c_void, name: *const c_char) -> *const c_void {
    if name_is(name, "vkGetDeviceProcAddr") { return vkGetDeviceProcAddr as *const c_void; }
    if name_is(name, "vkQueuePresentKHR") { return vk_queue_present as *const c_void; }
    let fns = VK_DEV.lock().unwrap().as_ref().and_then(|m| m.get(&dispatch_key(dev)).copied());
    match fns { Some(f) => unsafe { (f.gdpa)(dev, name) }, None => std::ptr::null() }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetInstanceProcAddr(inst: *mut c_void, name: *const c_char) -> *const c_void {
    if name_is(name, "vkGetInstanceProcAddr") { return vkGetInstanceProcAddr as *const c_void; }
    if name_is(name, "vkCreateInstance") { return vk_create_instance as *const c_void; }
    if name_is(name, "vkCreateDevice") { return vk_create_device as *const c_void; }
    if name_is(name, "vkGetDeviceProcAddr") { return vkGetDeviceProcAddr as *const c_void; }
    if name_is(name, "vkQueuePresentKHR") { return vk_queue_present as *const c_void; }
    match *VK_NEXT_GIPA.lock().unwrap() { Some(n) => unsafe { n(inst, name) }, None => std::ptr::null() }
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(p: *mut NegotiateLayerInterface) -> i32 {
    unsafe {
        if p.is_null() || (*p).version < 2 { return -3; }
        (*p).gipa = Some(vkGetInstanceProcAddr); (*p).gdpa = Some(vkGetDeviceProcAddr); (*p).gpdpa = std::ptr::null(); (*p).version = 2;
    }
    0
}

// ---------------------------------------------------------------- install, late hooks, unload
/// Hooks are in place (reset by unhook_all so a later attach to the same process installs them again).
static INSTALLED: AtomicBool = AtomicBool::new(false);
static OWN_MODULE: AtomicUsize = AtomicUsize::new(0);
static WORKER: AtomicBool = AtomicBool::new(false);

unsafe fn hook_all(hwnd: HWND) {
    unsafe {
        hook_dxgi(hwnd); hook_d3d9(hwnd); hook_d3d8(hwnd); hook_ddraw(hwnd); hook_gl(); hook_vulkan_direct();
        if let Some(s) = shared() { s.arch_bits.store(if cfg!(target_pointer_width = "64") { 64 } else { 32 }, Relaxed); }
    }
}
/// APIs loaded after injection (launchers that load the renderer late) are hooked by a short polling loop.
unsafe fn late_hooks() {
    unsafe {
        let need = |m: windows::core::PCWSTR, done: &AtomicUsize| module(m).is_some() && done.load(Relaxed) == 0;
        if need(w!("opengl32.dll"), &GL_SWAP) { hook_gl(); }
        if need(w!("vulkan-1.dll"), &VK_PRESENT_EXPORT) { hook_vulkan_direct(); }
        let (d9, d8, dd) = (need(w!("d3d9.dll"), &D9_PRESENT), need(w!("d3d8.dll"), &D8_PRESENT), need(w!("ddraw.dll"), &DD_FLIP[0]));
        // A dummy device per attempt is not free: try a few times, then stop.
        static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
        if (d9 || d8 || dd) && ATTEMPTS.fetch_add(1, Relaxed) < 5 {
            let h = dummy_window();
            if d9 { hook_d3d9(h); } if d8 { hook_d3d8(h); } if dd { hook_ddraw(h); }
            let _ = DestroyWindow(h);
        }
    }
}

/// Whether code at `addr` still belongs to a loaded module (a copy of this DLL that was not freed).
fn module_alive(addr: u64) -> bool {
    let mut m = HMODULE::default();
    unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(addr as *const u16), &mut m).is_ok() }
}

fn install_hooks() {
    let me = pace as *const () as u64;
    if let Some(s) = shared() {
        let owner = s.hook_owner.load(Relaxed);
        if owner != 0 && owner != me && module_alive(owner) {
            // Another copy's hooks are live: it paces this game from the same shared settings.
            hlog("hooks: another FlowScale copy already hooks this game; using it");
            s.attached_pid.store(std::process::id(), Relaxed);
            return;
        }
        s.hook_owner.store(me, Relaxed);
    }
    if !INSTALLED.swap(true, Relaxed) { unsafe {
        DETACHING.store(false, Relaxed);
        // Own one reference to this module so a hook-based injection can be released without unloading us.
        let mut me = HMODULE::default();
        if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, windows::core::PCWSTR(install_hooks as *const u16), &mut me).is_ok() { OWN_MODULE.store(me.0 as usize, Relaxed); }
        let hwnd = dummy_window();
        hook_all(hwnd);
        let _ = DestroyWindow(hwnd);
        hlog("hooks installed");
        if !WORKER.swap(true, Relaxed) { let _ = CreateThread(None, 0, Some(worker), None, THREAD_CREATION_FLAGS(0), None); }
    } }
    if let Some(s) = shared() { s.attached_pid.store(std::process::id(), Relaxed); }
}

/// Watches for late-loaded renderers and for FlowScale's unload request.
unsafe extern "system" fn worker(_: *mut c_void) -> u32 {
    let start = qpc();
    loop {
        unsafe { Sleep(100); }
        if qpc() - start < freq() * 30 { unsafe { late_hooks(); } }
        let Some(s) = shared() else { continue };
        if s.detach_pid.load(Relaxed) == std::process::id() {
            let state = unsafe { unhook_all() };
            s.detach_state.store(state, Relaxed);
            s.detach_pid.store(0, Relaxed);
            hlog(&format!("detach: state {state}"));
            WORKER.store(false, Relaxed);
            // Unloadable: release our own reference and exit from inside the module safely (FlowScale frees the
            // injector's LoadLibrary reference once this thread is gone). Resident: keep every reference.
            let m = OWN_MODULE.swap(0, Relaxed);
            if state == DETACH_UNLOADED && m != 0 { unsafe { FreeLibraryAndExitThread(HMODULE(m as *mut c_void), 0) } }
            if m != 0 { unsafe { let _ = FreeLibrary(HMODULE(m as *mut c_void)); } } // resident: drop only the extra reference taken at install
            return 0;
        }
    }
}

/// Restores every patched vtable slot and removes every inline hook, then waits for in-flight calls to drain.
unsafe fn unhook_all() -> u32 {
    DETACHING.store(true, Relaxed);
    unsafe {
        for (slot, orig) in SLOTS.lock().unwrap().drain(..) { patch_slot(slot as *mut usize, orig); }
        let targets: Vec<usize> = DETOURS.lock().unwrap().drain(..).collect();
        for t in &targets { let _ = minhook::MinHook::disable_hook(*t as *mut c_void); }
        // Wait until no thread is inside a detour, then a little longer for threads between a lookup and the call.
        let deadline = qpc() + freq() * 2;
        let mut quiet_since = 0i64;
        while qpc() < deadline {
            if INFLIGHT.load(Relaxed) == 0 { if quiet_since == 0 { quiet_since = qpc(); } else if qpc() - quiet_since > freq() / 5 { break; } } else { quiet_since = 0; }
            Sleep(5);
        }
        if INFLIGHT.load(Relaxed) != 0 { return DETACH_FAILED; }
        for t in &targets { let _ = minhook::MinHook::remove_hook(*t as *mut c_void); }
    }
    // Forget trampolines so a later attach installs everything again.
    for a in [&D9_PRESENT, &D9_PRESENTEX, &D9_SCPRESENT, &D8_PRESENT, &GL_SWAP, &GDI_SWAP, &GL_SWAP_LAYER, &GL_SWAP_MULTI, &VK_PRESENT_EXPORT, &VK_GDPA, &VK_GIPA, &VK_PRESENT_ICD] { a.store(0, Relaxed); }
    for arr in [&DD_BLT, &DD_BLTFAST, &DD_FLIP] { for a in arr { a.store(0, Relaxed); } }
    map(&DXGI_PRESENT).lock().unwrap().clear(); map(&DXGI_PRESENT1).lock().unwrap().clear();
    INSTALLED.store(false, Relaxed);
    if let Some(s) = shared() {
        let _ = s.pace_owner.compare_exchange(pace as *const () as u64, 0, Relaxed, Relaxed);
        let _ = s.hook_owner.compare_exchange(pace as *const () as u64, 0, Relaxed, Relaxed);
    }
    // Stay resident when the game holds pointers into this module: the Vulkan loader (layer), or a vkQueuePresentKHR
    // our vkGetDeviceProcAddr detour handed out. Those entry points keep working and simply pass through.
    let handed_out = VK_REAL_PRESENT.lock().unwrap().as_ref().is_some_and(|m| !m.is_empty());
    if handed_out || VK_DEV.lock().unwrap().as_ref().is_some_and(|m| !m.is_empty()) { DETACH_RESIDENT } else { DETACH_UNLOADED }
}

unsafe extern "system" fn init_thread(_: *mut c_void) -> u32 {
    if shared().map(targeted).unwrap_or(false) { install_hooks(); }
    0
}

/// Called by FlowScale's injector right after LoadLibrary (CreateRemoteThread path).
#[unsafe(no_mangle)]
pub unsafe extern "system" fn fs_attach(_: *mut c_void) -> u32 { install_hooks(); 1 }

/// WH_GETMESSAGE hook procedure for SetWindowsHookEx-based injection: the first message on the game's UI
/// thread installs the hooks (the module reference taken in install_hooks keeps us loaded after unhooking).
#[unsafe(no_mangle)]
pub unsafe extern "system" fn GetMessageProc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    static ONCE: AtomicU32 = AtomicU32::new(0);
    if ONCE.swap(1, Relaxed) == 0 && shared().map(targeted).unwrap_or(false) {
        let _ = unsafe { CreateThread(None, 0, Some(init_thread), None, THREAD_CREATION_FLAGS(0), None) };
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(h: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == 1 {
        unsafe {
            let _ = DisableThreadLibraryCalls(HMODULE(h.0));
            let _ = CreateThread(None, 0, Some(init_thread), None, THREAD_CREATION_FLAGS(0), None);
        }
    }
    true.into()
}
