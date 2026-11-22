#![allow(clippy::missing_safety_doc)]
pub mod config;
mod consts;
mod ffx;
mod stats;
mod upscaler;
mod xess;

use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

use windows::core::{Interface, BOOL, HSTRING, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use ffx::*;
use upscaler::{CreateInfo, Upscaler};

const VERSION_ID: u64 = 0x0046_4C4F_5753_0002;
const VERSION_NAME: &[u8] = b"FlowScale SR (FSR 3.1.5 API) 2.1.0\0";

struct Orig { create: PfnCreate, destroy: PfnDestroy, configure: PfnConfigure, query: PfnQuery, dispatch: PfnDispatch }
static ORIG: OnceLock<Option<Orig>> = OnceLock::new();
static SELF_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());
static CTXS: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

fn orig() -> Option<&'static Orig> {
    ORIG.get_or_init(|| unsafe {
        let me = SELF_PATH.get()?;
        let p = me.with_extension("orig.dll");
        if !p.exists() { log!("no {}: FlowScale SR alone (no frame generation)", p.display()); return None; }
        let Ok(m) = LoadLibraryW(&HSTRING::from(p.as_os_str())) else { log!("cannot load {}", p.display()); return None };
        let g = |n: &[u8]| GetProcAddress(m, PCSTR(n.as_ptr()));
        let (Some(a), Some(b), Some(c), Some(d), Some(e)) = (g(b"ffxCreateContext\0"), g(b"ffxDestroyContext\0"), g(b"ffxConfigure\0"), g(b"ffxQuery\0"), g(b"ffxDispatch\0")) else {
            log!("{}: missing exports", p.display()); return None;
        };
        log!("original FidelityFX DLL loaded: {}", p.display());
        Some(Orig { create: std::mem::transmute(a), destroy: std::mem::transmute(b), configure: std::mem::transmute(c), query: std::mem::transmute(d), dispatch: std::mem::transmute(e) })
    }).as_ref()
}

struct Ctx {
    real: Context, up: Option<Upscaler>, ci: CreateInfo, use_ours: bool, reset_real: bool, compare: bool, dispatches: u64,
    velocity: f32, reactiveness: f32, last: (f32, f32), last_size: [u32; 4], size_logs: u32,
}

fn ours(c: *mut Context) -> Option<&'static mut Ctx> {
    if c.is_null() { return None; }
    let p = unsafe { *c } as usize;
    if p == 0 || !CTXS.lock().unwrap().as_ref().is_some_and(|s| s.contains(&p)) { return None; }
    Some(unsafe { &mut *(p as *mut Ctx) })
}

fn frame_weight_of(c: &Ctx) -> f32 {
    let v = if c.velocity >= 0.0 { c.velocity } else { 1.0 };
    let r = if c.reactiveness >= 0.0 { c.reactiveness } else { 1.0 };
    if v >= 1.0 && r >= 1.0 { return -1.0; }
    (if v < 1.0 { 2.0 * v } else { 2.0 + 2.0 * (1.0 - r) }).clamp(0.25, 4.0)
}

pub(crate) fn key_edge(vk: i32, state: &std::sync::atomic::AtomicBool) -> bool {
    let now = vk != 0 && unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0;
    let was = state.swap(now, std::sync::atomic::Ordering::Relaxed);
    now && !was
}

unsafe fn own_query(context: *mut Context, desc: *mut Header, rc: &mut ReturnCode) -> bool {
    unsafe {
        *rc = RETURN_OK;
        let cfg = config::get();
        match (*desc).ty {
            QUERY_RATIO_FROM_QUALITY => {
                let q = &*(desc as *mut QueryRatio);
                let r = upscaler::ratio_from_quality(q.quality, &cfg.ratio);
                if r <= 0.0 { *rc = RETURN_ERROR_PARAMETER } else if !q.out_ratio.is_null() { *q.out_ratio = r; }
                true
            }
            QUERY_RENDER_RES_FROM_QUALITY => {
                let q = &*(desc as *mut QueryRenderRes);
                let r = upscaler::ratio_from_quality(q.quality, &cfg.ratio);
                if r <= 0.0 { *rc = RETURN_ERROR_PARAMETER; return true; }
                if !q.out_w.is_null() { *q.out_w = (q.display_w as f32 / r) as u32; }
                if !q.out_h.is_null() { *q.out_h = (q.display_h as f32 / r) as u32; }
                true
            }
            QUERY_JITTER_PHASE_COUNT => {
                let q = &*(desc as *mut QueryPhaseCount);
                if !q.out_count.is_null() { *q.out_count = upscaler::jitter_phase_count(q.render_w, q.display_w); }
                true
            }
            QUERY_JITTER_OFFSET => {
                let q = &*(desc as *mut QueryJitterOffset);
                let (x, y) = upscaler::jitter_offset(q.index, q.phase_count);
                if !q.out_x.is_null() { *q.out_x = x; }
                if !q.out_y.is_null() { *q.out_y = y; }
                true
            }
            QUERY_GET_VERSIONS => {
                let q = &mut *(desc as *mut QueryVersions);
                if q.create_desc_type != CREATE_UPSCALE { return false; }
                let cap = if q.output_count.is_null() { 0 } else { *q.output_count };
                let mut n = 1u64;
                if cap > 0 {
                    if !q.version_ids.is_null() { *q.version_ids = VERSION_ID; }
                    if !q.version_names.is_null() { *q.version_names = VERSION_NAME.as_ptr() as *const c_char; }
                }
                if let Some(o) = orig() {
                    let mut rest = cap.saturating_sub(1);
                    let mut oq = *q;
                    oq.output_count = &mut rest;
                    oq.version_ids = if !q.version_ids.is_null() && cap > 1 { q.version_ids.add(1) } else { std::ptr::null_mut() };
                    oq.version_names = if !q.version_names.is_null() && cap > 1 { q.version_names.add(1) } else { std::ptr::null_mut() };
                    if (o.query)(std::ptr::null_mut(), &mut oq as *mut QueryVersions as *mut Header) == RETURN_OK { n += rest; }
                }
                if !q.output_count.is_null() { *q.output_count = if cap != 0 { n.min(cap) } else { n }; }
                true
            }
            QUERY_GET_PROVIDER_VERSION => {
                match ours(context) { Some(c) if c.use_ours => {} _ => return false }
                let q = &mut *(desc as *mut QueryProviderVersion);
                q.version_id = VERSION_ID; q.version_name = VERSION_NAME.as_ptr() as *const c_char;
                true
            }
            _ => false,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffxCreateContext(context: *mut Context, desc: *mut Header, mem: *const AllocationCallbacks) -> ReturnCode {
    let _g = LOCK.lock().unwrap();
    let o = orig();
    if context.is_null() || desc.is_null() { return RETURN_ERROR_PARAMETER; }
    unsafe {
        if (*desc).ty != CREATE_UPSCALE { return o.map(|o| (o.create)(context, desc, mem)).unwrap_or(RETURN_NO_PROVIDER); }
        let cfg = config::get();
        let ov = find(desc, DESC_OVERRIDE_VERSION) as *const OverrideVersion;
        if let Some(o) = o { if !cfg.enabled || (!ov.is_null() && (*ov).version_id != VERSION_ID) { return (o.create)(context, desc, mem); } }
        let u = &*(desc as *const CreateUpscale);
        let be = find(desc, CREATE_BACKEND_DX12) as *const BackendDx12;
        if be.is_null() || (*be).device.is_null() {
            log!("upscale context without a D3D12 device: {}", if o.is_some() { "original FSR" } else { "refused" });
            return o.map(|o| (o.create)(context, desc, mem)).unwrap_or(RETURN_NO_PROVIDER);
        }
        let ci = CreateInfo { flags: u.flags, max_upscale: [u.max_upscale.width, u.max_upscale.height] };
        log!("upscale context: flags 0x{:x}, max render {}x{}, max upscale {}x{}", u.flags, u.max_render.width, u.max_render.height, u.max_upscale.width, u.max_upscale.height);
        let mut real: Context = std::ptr::null_mut();
        if let Some(o) = o { if (o.create)(&mut real, desc, mem) != RETURN_OK { log!("original upscale context failed"); real = std::ptr::null_mut(); } }
        let dev = ID3D12Device::from_raw_borrowed(&(*be).device).cloned();
        let up = dev.and_then(|d| Upscaler::new(d, CreateInfo { flags: ci.flags, max_upscale: ci.max_upscale }, cfg.s).inspect_err(|e| log!("pipelines failed: {e:?}")).ok());
        if up.is_none() && real.is_null() { return RETURN_ERROR_RUNTIME_ERROR; }
        let use_ours = up.is_some();
        let compare = cfg.compare && !real.is_null() && use_ours;
        let c = Box::new(Ctx { real, up, ci, use_ours, reset_real: false, compare, dispatches: 0, velocity: -1.0, reactiveness: -1.0, last: (-1.0, -1.0), last_size: [0; 4], size_logs: 0 });
        let p = Box::into_raw(c);
        CTXS.lock().unwrap().get_or_insert_with(HashSet::new).insert(p as usize);
        *context = p as Context;
        log!("upscale context {:p}: {}", p, if use_ours { "FlowScale SR" } else { "original FSR" });
        RETURN_OK
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffxDestroyContext(context: *mut Context, mem: *const AllocationCallbacks) -> ReturnCode {
    let _g = LOCK.lock().unwrap();
    let o = orig();
    let Some(c) = ours(context) else { return o.map(|o| unsafe { (o.destroy)(context, mem) }).unwrap_or(RETURN_ERROR_PARAMETER) };
    log!("upscale context destroyed after {} dispatches ({} ours)", c.dispatches, c.up.as_ref().map(|u| u.frames()).unwrap_or(0));
    unsafe {
        if !c.real.is_null() { if let Some(o) = o { (o.destroy)(&mut c.real, mem); } }
        let p = *context as usize;
        CTXS.lock().unwrap().as_mut().map(|s| s.remove(&p));
        drop(Box::from_raw(p as *mut Ctx));
        *context = std::ptr::null_mut();
    }
    RETURN_OK
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffxConfigure(context: *mut Context, desc: *const Header) -> ReturnCode {
    let _g = LOCK.lock().unwrap();
    let o = orig();
    let Some(c) = ours(context) else { return o.map(|o| unsafe { (o.configure)(context, desc) }).unwrap_or(RETURN_OK) };
    unsafe {
        if !desc.is_null() && (*desc).ty == CONFIGURE_UPSCALE_KEYVALUE {
            let kv = &*(desc as *const ConfigureKeyValue);
            let vel = kv.key == KEY_VELOCITY_FACTOR;
            if (vel || kv.key == KEY_REACTIVENESS_SCALE) && !kv.ptr.is_null() {
                let v = (*(kv.ptr as *const f32)).clamp(0.0, 1.0);
                if vel { c.velocity = v } else { c.reactiveness = v }
                log!("configure: {} {v} -> frame weight {}", if vel { "velocity factor" } else { "reactiveness scale" }, frame_weight_of(c));
            }
        }
        if !c.real.is_null() { if let Some(o) = o { return (o.configure)(&mut c.real, desc); } }
    }
    RETURN_OK
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffxQuery(context: *mut Context, desc: *mut Header) -> ReturnCode {
    let _g = LOCK.lock().unwrap();
    let o = orig();
    if desc.is_null() { return RETURN_ERROR_PARAMETER; }
    unsafe {
        let c = ours(context);
        let free = context.is_null() || (*context).is_null() || c.is_some();
        let mut rc = RETURN_OK;
        if (config::get().enabled || o.is_none()) && free && own_query(context, desc, &mut rc) { return rc; }
        let Some(c) = c else { return o.map(|o| (o.query)(context, desc)).unwrap_or(RETURN_NO_PROVIDER) };
        if !c.real.is_null() { if let Some(o) = o { return (o.query)(&mut c.real, desc); } }
        if (*desc).ty == QUERY_GPU_MEMORY_USAGE {
            let q = &*(desc as *mut QueryMemory);
            if !q.usage.is_null() { (*q.usage).total = 0; (*q.usage).aliasable = 0; }
            return RETURN_OK;
        }
        RETURN_ERROR_UNKNOWN_DESCTYPE
    }
}

static TOGGLE_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static COMPARE_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RELOAD_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffxDispatch(context: *mut Context, desc: *const Header) -> ReturnCode {
    let _g = LOCK.lock().unwrap();
    let o = orig();
    let Some(c) = ours(context) else { return o.map(|o| unsafe { (o.dispatch)(context, desc) }).unwrap_or(RETURN_ERROR_PARAMETER) };
    if desc.is_null() { return RETURN_ERROR_PARAMETER; }
    unsafe {
        if (*desc).ty != DISPATCH_UPSCALE {
            if !c.real.is_null() { if let Some(o) = o { return (o.dispatch)(&mut c.real, desc); } }
            return if (*desc).ty == DISPATCH_GENERATE_REACTIVE { RETURN_OK } else { RETURN_ERROR_UNKNOWN_DESCTYPE };
        }
        let d = &*(desc as *const DispatchUpscale);
        let mut cfg = config::get();
        if key_edge(cfg.reload_key, &RELOAD_DOWN) || config::changed_on_disk() { cfg = config::reload(); }
        let has_real = !c.real.is_null() && o.is_some();
        let mut switched = false;
        if key_edge(cfg.toggle_key, &TOGGLE_DOWN) && c.up.is_some() && has_real {
            c.use_ours = !c.use_ours; switched = true;
            if !c.use_ours { c.reset_real = true; }
            log!("toggle: {} from dispatch {}", if c.use_ours { "FlowScale SR" } else { "original FSR" }, c.dispatches);
        }
        if key_edge(cfg.compare_key, &COMPARE_DOWN) && c.up.is_some() && has_real {
            c.compare = !c.compare;
            if c.compare { c.reset_real = true; }
            log!("comparison {}", if c.compare { "on (FlowScale SR left, original right)" } else { "off" });
        }
        let mut s = cfg.s;
        let fw = frame_weight_of(c);
        if fw >= 0.0 && !s.frame_weight_override { s.frame_weight = fw; }
        let gs = cfg.game_sharpness > 0;
        if gs && d.enable_sharpening { s.sharpness = d.sharpness.clamp(0.0, 1.0); }
        if (s.sharpness, s.frame_weight) != c.last { log!("settings: sharpness {}, frame weight {}", s.sharpness, s.frame_weight); c.last = (s.sharpness, s.frame_weight); }
        let size = [d.render_size.width, d.render_size.height, d.upscale_size.width, d.upscale_size.height];
        if c.dispatches < cfg.log_dispatches || (size != c.last_size && { c.size_logs += 1; c.size_logs } <= 32) {
            log!("dispatch {}: render {}x{} -> {}x{}, jitter {:.4} {:.4}, mv scale {:.2} {:.2}, reset {}, sharpen {} ({:.2}), pre-exposure {}, near {}, far {}, fov {:.4}",
                c.dispatches, size[0], size[1], size[2], size[3], d.jitter.x, d.jitter.y, d.mv_scale.x, d.mv_scale.y, d.reset, d.enable_sharpening, d.sharpness,
                d.pre_exposure, d.camera_near, d.camera_far, d.camera_fov_y);
        }
        c.last_size = size;
        c.dispatches += 1;
        if c.use_ours && let Some(up) = c.up.as_mut() {
            up.set_settings(s);
            let cmp = c.compare && has_real;
            if cmp {
                let mut r = *d;
                if c.reset_real { r.reset = true; c.reset_real = false; }
                (o.unwrap().dispatch)(&mut c.real, &r as *const DispatchUpscale as *const Header);
            }
            up.set_split(cmp);
            if let Some(fr) = upscaler::frame_from_ffx(d, &c.ci) {
                if up.dispatch(&fr, switched) {
                    let n = up.frames();
                    stats::publish(1, &fr.n, up.upscale_ms, n);
                    if n == 1 || n % 3600 == 0 { log!("FlowScale SR ran (frame {n})"); }
                    return RETURN_OK;
                }
            }
            static FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if FAILS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 { log!("could not run this dispatch{}", if has_real { ": original FSR instead" } else { "" }); }
        }
        let Some(o) = o.filter(|_| !c.real.is_null()) else { return RETURN_ERROR_RUNTIME_ERROR };
        if c.reset_real {
            c.reset_real = false;
            let mut r = *d; r.reset = true;
            return (o.dispatch)(&mut c.real, &r as *const DispatchUpscale as *const Header);
        }
        (o.dispatch)(&mut c.real, desc)
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(h: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == 1 {
        let mut b = [0u16; 1024];
        let n = unsafe { GetModuleFileNameW(Some(HMODULE(h.0)), &mut b) } as usize;
        let p = std::path::PathBuf::from(String::from_utf16_lossy(&b[..n]));
        config::set_dir(p.parent().map(|d| d.to_path_buf()).unwrap_or_default());
        let _ = SELF_PATH.set(p);
    }
    TRUE
}
