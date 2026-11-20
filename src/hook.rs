#![allow(non_snake_case, clippy::missing_safety_doc)]
#[allow(dead_code)] #[path = "pacer.rs"] mod pacer;
#[allow(dead_code)] #[path = "ipc.rs"] mod ipc;

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::core::{s, w, Interface, BOOL, HRESULT};
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
static LAST_VK: AtomicI64 = AtomicI64::new(0);
static FREQ: OnceLock<i64> = OnceLock::new();

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
fn freq() -> i64 { *FREQ.get_or_init(pacer::qpf) }

fn targeted(s: &Shared) -> bool { let t = s.target_pid.load(Relaxed); t == std::process::id() }

fn pace(api: u32, w: u32, h: u32, before: bool) {
    let Some(s) = shared() else { return };
    if !targeted(s) { return; }
    let mode = s.mode.load(Relaxed);
    let pace_now = (mode == MODE_LOW_LATENCY) != before;
    if !pace_now { return; }
    let enabled = s.enabled.load(Relaxed) != 0;
    let mfps = if enabled { s.milli_fps.load(Relaxed) } else { 0 };
    {
        let mut p = PACER.lock().unwrap();
        let p = p.get_or_insert_with(Pacer::new);
        p.set_rate(mfps);
        let st = p.limit();
        if st.late { s.late_frames.fetch_add(1, Relaxed); }
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

struct Election { primary: usize, area: u64, seen: i64, cache: HashMap<usize, (u32, u32, u32)> }
static ELECTION: Mutex<Option<Election>> = Mutex::new(None);
fn is_primary(key: usize, size: impl FnOnce() -> (u32, u32)) -> (bool, u32, u32) {
    let mut g = ELECTION.lock().unwrap();
    let e = g.get_or_insert_with(|| Election { primary: 0, area: 0, seen: 0, cache: HashMap::new() });
    let now = qpc();
    let entry = e.cache.entry(key).or_insert((0, 0, 0));
    if entry.2 == 0 { let (w, h) = size(); *entry = (w, h, 120); } else { entry.2 -= 1; }
    let (w, h, _) = *entry;
    let area = w as u64 * h as u64;
    let stale = now - e.seen > freq();
    if key == e.primary || stale || area > e.area {
        e.primary = key; e.area = area; e.seen = now;
        if e.cache.len() > 64 { e.cache.clear(); }
        (true, w, h)
    } else { (false, w, h) }
}

fn hlog(msg: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("flowscale_hook.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "[pid {}] {msg}", std::process::id()); }
}

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
    let cur = unsafe { *slot };
    if cur == hook { return; }
    let orig = unsafe { patch_slot(slot, hook) };
    hlog(&format!("vtable {:p}[{idx}] {:#x} -> hook", vt, orig));
    if orig != 0 { store.lock().unwrap().insert(vt as usize, orig); }
}
fn orig_of(store: &Mutex<HashMap<usize, usize>>, obj: *mut c_void) -> usize {
    let vt = unsafe { vtable(obj) } as usize;
    let m = store.lock().unwrap();
    m.get(&vt).copied().or_else(|| m.values().next().copied()).unwrap_or(0)
}

static DXGI_PRESENT: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
static DXGI_PRESENT1: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
static DXGI_API: AtomicUsize = AtomicUsize::new(API_D3D11 as usize);
static DXGI_KIND: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);
fn map(m: &'static OnceLock<Mutex<HashMap<usize, usize>>>) -> &'static Mutex<HashMap<usize, usize>> { m.get_or_init(|| Mutex::new(HashMap::new())) }

fn dxgi_pace(this: *mut c_void, flags: u32, before: bool) {
    if flags & DXGI_PRESENT_TEST.0 != 0 { return; }
    if qpc() - LAST_VK.load(Relaxed) < freq() / 2 { return; }
    let (prim, w, h) = is_primary(this as usize, || unsafe {
        IDXGISwapChain::from_raw_borrowed(&this).and_then(|sc| sc.GetDesc().ok()).map(|d| (d.BufferDesc.Width, d.BufferDesc.Height)).unwrap_or((0, 0))
    });
    if prim {
        let api = *DXGI_KIND.lock().unwrap().get_or_insert_with(HashMap::new).entry(this as usize).or_insert_with(|| unsafe {
            match IDXGISwapChain::from_raw_borrowed(&this) { Some(sc) if sc.GetDevice::<ID3D12Device>().is_ok() || sc.GetDevice::<ID3D12CommandQueue>().is_ok() => API_D3D12, Some(sc) if sc.GetDevice::<ID3D11Device>().is_ok() => API_D3D11, _ => if GetModuleHandleW(w!("d3d12.dll")).is_ok() && GetModuleHandleW(w!("d3d11.dll")).is_err() { API_D3D12 } else { DXGI_API.load(Relaxed) as u32 } }
        });
        pace(api, w, h, before);
    }
}
unsafe extern "system" fn hk_present(this: *mut c_void, sync: u32, flags: u32) -> HRESULT {
    dxgi_pace(this, flags, true);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT = unsafe { std::mem::transmute(orig_of(map(&DXGI_PRESENT), this)) };
    let r = unsafe { f(this, sync, flags) };
    dxgi_pace(this, flags, false);
    r
}
unsafe extern "system" fn hk_present1(this: *mut c_void, sync: u32, flags: u32, params: *const c_void) -> HRESULT {
    dxgi_pace(this, flags, true);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> HRESULT = unsafe { std::mem::transmute(orig_of(map(&DXGI_PRESENT1), this)) };
    let r = unsafe { f(this, sync, flags, params) };
    dxgi_pace(this, flags, false);
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
            if let Some(sc) = sc { let vt = vtable(sc.as_raw()); hook_vtable(vt, 8, hk_present as *const () as usize, map(&DXGI_PRESENT)); hook_vtable(vt, 22, hk_present1 as *const () as usize, map(&DXGI_PRESENT1)); }
        }
        if GetModuleHandleW(w!("d3d12.dll")).is_ok() {
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

static D9_PRESENT: AtomicUsize = AtomicUsize::new(0);
static D9_PRESENTEX: AtomicUsize = AtomicUsize::new(0);
static D9_SCPRESENT: AtomicUsize = AtomicUsize::new(0);
static GL_SWAP: AtomicUsize = AtomicUsize::new(0);

unsafe fn detour(target: usize, hook: usize, orig: &AtomicUsize, what: &str) {
    if target == 0 || orig.load(Relaxed) != 0 { return; }
    match unsafe { minhook::MinHook::create_hook(target as *mut c_void, hook as *mut c_void) } {
        Ok(o) => { orig.store(o as usize, Relaxed); let _ = unsafe { minhook::MinHook::enable_hook(target as *mut c_void) }; hlog(&format!("detour {what} {target:#x} ok")); }
        Err(e) => hlog(&format!("detour {what} {target:#x} failed {e:?}")),
    }
}

fn d9_pace(this: *mut c_void, before: bool) { let (p, w, h) = is_primary(this as usize, || (0, 0)); if p { pace(API_D3D9, w, h, before); } }
unsafe extern "system" fn hk_d9_present(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void) -> HRESULT {
    d9_pace(this, true);
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, HWND, *const c_void) -> HRESULT = unsafe { std::mem::transmute(D9_PRESENT.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d) }; d9_pace(this, false); r
}
unsafe extern "system" fn hk_d9_presentex(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void, fl: u32) -> HRESULT {
    d9_pace(this, true);
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, HWND, *const c_void, u32) -> HRESULT = unsafe { std::mem::transmute(D9_PRESENTEX.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d, fl) }; d9_pace(this, false); r
}
unsafe extern "system" fn hk_d9_scpresent(this: *mut c_void, a: *const c_void, b: *const c_void, c: HWND, d: *const c_void, fl: u32) -> HRESULT {
    d9_pace(this, true);
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, HWND, *const c_void, u32) -> HRESULT = unsafe { std::mem::transmute(D9_SCPRESENT.load(Relaxed)) };
    let r = unsafe { f(this, a, b, c, d, fl) }; d9_pace(this, false); r
}
unsafe fn hook_d3d9(hwnd: HWND) {
    unsafe {
        if GetModuleHandleW(w!("d3d9.dll")).is_err() { return; }
        let mut pp = D3DPRESENT_PARAMETERS { Windowed: true.into(), SwapEffect: D3DSWAPEFFECT_DISCARD, hDeviceWindow: hwnd, BackBufferFormat: D3DFMT_UNKNOWN, ..Default::default() };
        let flags = (D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_DISABLE_DRIVER_MANAGEMENT) as u32;
        let Ok(d3d) = Direct3DCreate9Ex(D3D_SDK_VERSION) else { return };
        let mut dev = None;
        if d3d.CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, flags, &mut pp, std::ptr::null_mut(), &mut dev).is_err() { hlog("d3d9: dummy device failed"); return; }
        let Some(dev) = dev else { return };
        let vt = vtable(dev.as_raw());
        detour(*vt.add(17), hk_d9_present as *const () as usize, &D9_PRESENT, "IDirect3DDevice9::Present");
        detour(*vt.add(121), hk_d9_presentex as *const () as usize, &D9_PRESENTEX, "IDirect3DDevice9Ex::PresentEx");
        if let Ok(sc) = dev.GetSwapChain(0) { detour(*vtable(sc.as_raw()).add(3), hk_d9_scpresent as *const () as usize, &D9_SCPRESENT, "IDirect3DSwapChain9::Present"); }
    }
}

unsafe extern "system" fn hk_wglswapbuffers(hdc: HDC) -> BOOL {
    pace(API_GL, 0, 0, true);
    let f: unsafe extern "system" fn(HDC) -> BOOL = unsafe { std::mem::transmute(GL_SWAP.load(Relaxed)) };
    let r = unsafe { f(hdc) }; pace(API_GL, 0, 0, false); r
}
unsafe fn hook_gl() {
    unsafe {
        let Ok(gl) = GetModuleHandleW(w!("opengl32.dll")) else { return };
        if let Some(p) = GetProcAddress(gl, s!("wglSwapBuffers")) { detour(p as usize, hk_wglswapbuffers as *const () as usize, &GL_SWAP, "wglSwapBuffers"); }
    }
}

type Gipa = unsafe extern "system" fn(*mut c_void, *const c_char) -> *const c_void;
#[repr(C)] struct LayerInstanceLink { next: *mut LayerInstanceLink, gipa: Gipa, gpdpa: *const c_void }
#[repr(C)] struct LayerDeviceLink { next: *mut LayerDeviceLink, gipa: Gipa, gdpa: Gipa }
#[repr(C)] struct LayerCreateInfo { s_type: i32, p_next: *const c_void, function: i32, link: *mut c_void }
#[repr(C)] pub struct NegotiateLayerInterface { s_type: i32, p_next: *mut c_void, version: u32, gipa: Option<Gipa>, gdpa: Option<Gipa>, gpdpa: *const c_void }
#[derive(Clone, Copy)] struct DevFns { gdpa: Gipa, present: usize }
static VK_NEXT_GIPA: Mutex<Option<Gipa>> = Mutex::new(None);
static VK_DEV: Mutex<Option<HashMap<usize, DevFns>>> = Mutex::new(None);
fn dispatch_key(h: *mut c_void) -> usize { unsafe { *(h as *const usize) } }

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
            hlog(&format!("vk device created, next present {present:#x}"));
        }
        r
    }
}
unsafe extern "system" fn vk_queue_present(queue: *mut c_void, info: *const c_void) -> i32 {
    let fns = VK_DEV.lock().unwrap().as_ref().and_then(|m| m.get(&dispatch_key(queue)).copied());
    let Some(fns) = fns else { hlog("vk present: unknown queue"); return -3 };
    if LAST_VK.swap(qpc(), Relaxed) == 0 { hlog("vk first present through layer"); }
    pace(API_VK, 0, 0, true);
    let f: unsafe extern "system" fn(*mut c_void, *const c_void) -> i32 = unsafe { std::mem::transmute(fns.present) };
    let r = unsafe { f(queue, info) };
    pace(API_VK, 0, 0, false);
    r
}
fn name_is(n: *const c_char, s: &str) -> bool { unsafe { CStr::from_ptr(n).to_bytes() == s.as_bytes() } }

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
    if name_is(name, "vkQueuePresentKHR") { hlog("vkQueuePresentKHR via instance proc addr"); return vk_queue_present as *const c_void; }
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

static INSTALLED: std::sync::Once = std::sync::Once::new();
fn install_hooks() {
    INSTALLED.call_once(|| unsafe {
        let mut pinned = HMODULE::default();
        let _ = GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN, windows::core::PCWSTR(install_hooks as *const u16), &mut pinned);
        let hwnd = dummy_window();
        hook_dxgi(hwnd);
        hook_d3d9(hwnd);
        hook_gl();
        let _ = DestroyWindow(hwnd);
        hlog("hooks installed");
        std::thread::spawn(|| { std::thread::sleep(std::time::Duration::from_secs(3)); hook_gl(); hook_d3d9(dummy_window()); });
    });
    if let Some(s) = shared() { s.attached_pid.store(std::process::id(), Relaxed); }
}

unsafe extern "system" fn init_thread(_: *mut c_void) -> u32 {
    if shared().map(targeted).unwrap_or(false) { install_hooks(); }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn fs_attach(_: *mut c_void) -> u32 { install_hooks(); 1 }

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
