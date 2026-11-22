use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use windows::core::{Interface, HSTRING, PCSTR};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::consts::FrameNums;
use crate::upscaler::{CreateInfo, Frame, Upscaler};
use crate::{config, ffx, log, stats};

type Handle = *mut c_void;
type XResult = i32;
const OK: i32 = 0;
const CANT_LOAD: i32 = -11;

const HIGH_RES_MV: u32 = 1 << 0;
const INVERTED_DEPTH: u32 = 1 << 1;
const EXPOSURE_TEXTURE: u32 = 1 << 2;
const RESPONSIVE_MASK: u32 = 1 << 3;
const NDC_VELOCITY: u32 = 1 << 4;
const LDR_INPUT: u32 = 1 << 6;
const JITTERED_MV: u32 = 1 << 7;
const AUTO_EXPOSURE: u32 = 1 << 8;

#[repr(C)] #[derive(Clone, Copy, Default)]
pub struct Xy { pub x: u32, pub y: u32 }
#[repr(C)] #[derive(Clone, Copy)]
pub struct InitParams { pub output: Xy, pub quality: i32, pub flags: u32, pub creation_mask: u32, pub visible_mask: u32,
    pub temp_buffer_heap: *mut c_void, pub buffer_offset: u64, pub temp_texture_heap: *mut c_void, pub texture_offset: u64, pub pipeline_library: *mut c_void }
#[repr(C)]
pub struct ExecuteParams {
    pub color: *mut c_void, pub velocity: *mut c_void, pub depth: *mut c_void, pub exposure: *mut c_void, pub responsive: *mut c_void, pub output: *mut c_void,
    pub jitter_x: f32, pub jitter_y: f32, pub exposure_scale: f32, pub reset: u32, pub input_w: u32, pub input_h: u32,
    pub color_base: Xy, pub mv_base: Xy, pub depth_base: Xy, pub responsive_base: Xy, pub reserved: Xy, pub output_base: Xy,
    pub heap: *mut c_void, pub heap_offset: u32,
}
const _: () = assert!(std::mem::size_of::<InitParams>() == 64);
const _: () = assert!(std::mem::size_of::<ExecuteParams>() == 136);

type PCreate = unsafe extern "C" fn(*mut c_void, *mut Handle) -> XResult;
type PInit = unsafe extern "C" fn(Handle, *const InitParams) -> XResult;
type PExec = unsafe extern "C" fn(Handle, *mut c_void, *const ExecuteParams) -> XResult;
type PDestroy = unsafe extern "C" fn(Handle) -> XResult;
type PScale = unsafe extern "C" fn(Handle, f32, f32) -> XResult;
type PFloat = unsafe extern "C" fn(Handle, f32) -> XResult;
struct Orig { create: PCreate, init: PInit, exec: PExec, destroy: PDestroy, velocity: PScale, jitter: PScale, exposure: PFloat, responsive: PFloat }
static ORIG: OnceLock<Option<Orig>> = OnceLock::new();

fn orig() -> Option<&'static Orig> {
    ORIG.get_or_init(|| unsafe {
        let p = config::dir().join("libxess_orig.dll");
        let m = orig_module()?;
        let g = |n: &[u8]| GetProcAddress(m, PCSTR(n.as_ptr()));
        let f = (g(b"xessD3D12CreateContext\0"), g(b"xessD3D12Init\0"), g(b"xessD3D12Execute\0"), g(b"xessDestroyContext\0"),
                 g(b"xessSetVelocityScale\0"), g(b"xessSetJitterScale\0"), g(b"xessSetExposureMultiplier\0"), g(b"xessSetMaxResponsiveMaskValue\0"));
        let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(h), Some(i), Some(j)) = f else { log!("{}: missing exports", p.display()); return None };
        log!("original XeSS loaded: {}", p.display());
        Some(Orig { create: std::mem::transmute(a), init: std::mem::transmute(b), exec: std::mem::transmute(c), destroy: std::mem::transmute(d),
            velocity: std::mem::transmute(e), jitter: std::mem::transmute(h), exposure: std::mem::transmute(i), responsive: std::mem::transmute(j) })
    }).as_ref()
}

struct Ctx { device: Option<ID3D12Device>, init: Option<InitParams>, up: Option<Upscaler>, velocity: [f32; 2], jitter: [f32; 2], exposure: f32, responsive: f32,
    use_ours: bool, reset_real: bool, compare: bool, executes: u64 }
unsafe impl Send for Ctx {}
static CTX: Mutex<Option<HashMap<usize, Ctx>>> = Mutex::new(None);
static TOGGLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static COMPARE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RELOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn with<R>(h: Handle, f: impl FnOnce(&mut Ctx) -> R) -> Option<R> { CTX.lock().unwrap().as_mut().and_then(|m| m.get_mut(&(h as usize))).map(f) }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessD3D12CreateContext(device: *mut c_void, out: *mut Handle) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    let r = unsafe { (o.create)(device, out) };
    if r >= OK && !out.is_null() && unsafe { !(*out).is_null() } {
        let dev = unsafe { ID3D12Device::from_raw_borrowed(&device).cloned() };
        let c = Ctx { device: dev, init: None, up: None, velocity: [1.0; 2], jitter: [1.0; 2], exposure: 1.0, responsive: 1.0, use_ours: true, reset_real: false, compare: false, executes: 0 };
        CTX.lock().unwrap().get_or_insert_with(HashMap::new).insert(unsafe { *out } as usize, c);
    }
    log!("xessD3D12CreateContext: {r}");
    r
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessD3D12Init(h: Handle, p: *const InitParams) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    let r = unsafe { (o.init)(h, p) };
    if p.is_null() { return r; }
    let p = unsafe { *p };
    let cfg = config::get();
    log!("xessD3D12Init: {r}, output {}x{}, quality {}, flags 0x{:x}", p.output.x, p.output.y, p.quality, p.flags);
    with(h, |c| {
        c.init = Some(p);
        if !cfg.enabled { c.use_ours = false; return; }
        if c.up.is_none() {
            let flags = if p.flags & AUTO_EXPOSURE != 0 { ffx::ENABLE_AUTO_EXPOSURE } else { 0 };
            c.up = c.device.clone().and_then(|d| Upscaler::new(d, CreateInfo { flags, max_upscale: [p.output.x, p.output.y] }, cfg.s).inspect_err(|e| log!("pipelines failed: {e:?}")).ok());
        }
        c.use_ours = c.up.is_some();
        c.compare = cfg.compare && c.use_ours; c.reset_real = c.compare;
        log!("XeSS context {:p}: {}", h, if c.use_ours { "FlowScale SR" } else { "original XeSS" });
    });
    r
}

fn frame(c: &Ctx, cl: *mut c_void, e: &ExecuteParams) -> Option<Frame> {
    let init = c.init?;
    let res = |p: *mut c_void| -> Option<ID3D12Resource> { if p.is_null() { None } else { unsafe { ID3D12Resource::from_raw_borrowed(&p).cloned() } } };
    let cl = if cl.is_null() { None } else { unsafe { ID3D12GraphicsCommandList::from_raw_borrowed(&cl).cloned() } }?;
    let (color, mv, output) = (res(e.color)?, res(e.velocity)?, res(e.output)?);
    let depth = res(e.depth);
    depth.as_ref()?;
    let fl = init.flags;
    let cd = unsafe { color.GetDesc() };
    let rw = if e.input_w != 0 { e.input_w } else { cd.Width as u32 };
    let rh = if e.input_h != 0 { e.input_h } else { cd.Height };
    let (mut ow, mut oh) = (init.output.x, init.output.y);
    if ow == 0 || oh == 0 { let od = unsafe { output.GetDesc() }; ow = od.Width as u32; oh = od.Height; }
    let hi = fl & HIGH_RES_MV != 0;
    let mv_res = if hi { [ow as f32, oh as f32] } else { [rw as f32, rh as f32] };
    let mv_to_uv = if fl & NDC_VELOCITY != 0 { [0.5 * c.velocity[0], -0.5 * c.velocity[1]] } else { [c.velocity[0] / mv_res[0], c.velocity[1] / mv_res[1]] };
    let srv = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    Some(Frame {
        cl, color, depth, mv, output,
        exposure: if fl & EXPOSURE_TEXTURE != 0 { res(e.exposure) } else { None },
        reactive: if fl & RESPONSIVE_MASK != 0 { res(e.responsive) } else { None },
        color_state: srv, depth_state: srv, mv_state: srv, exposure_state: srv, reactive_state: srv, output_state: D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        n: FrameNums { rw, rh, ow, oh, jit: [e.jitter_x * c.jitter[0], e.jitter_y * c.jitter[1]], mv_to_uv, mv_res,
            jittered_mv: fl & JITTERED_MV != 0, depth_inverted: fl & INVERTED_DEPTH != 0, hdr: fl & LDR_INPUT == 0,
            exposure_mul: (if e.exposure_scale > 0.0 { e.exposure_scale } else { 1.0 }) * c.exposure, pre_exposure: 1.0,
            reactive_mul: if c.responsive > 0.0 { 1.0 / c.responsive } else { 1.0 } },
        reset: e.reset != 0, fov_y: 0.0, z_near: 0.0, z_far: 0.0, meters_per_unit: 0.0,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessD3D12Execute(h: Handle, cl: *mut c_void, e: *const ExecuteParams) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    if e.is_null() { return unsafe { (o.exec)(h, cl, e) }; }
    let ep = unsafe { &*e };
    let mut cfg = config::get();
    if crate::key_edge(cfg.reload_key, &RELOAD) { cfg = config::reload(); }
    let toggle = crate::key_edge(cfg.toggle_key, &TOGGLE);
    let cmp_key = crate::key_edge(cfg.compare_key, &COMPARE);
    let outcome = with(h, |c| -> Option<XResult> {
        c.init?;
        let mut switched = false;
        if toggle && c.up.is_some() { c.use_ours = !c.use_ours; switched = true; if !c.use_ours { c.reset_real = true; } log!("toggle: {}", if c.use_ours { "FlowScale SR" } else { "original XeSS" }); }
        if cmp_key && c.up.is_some() { c.compare = !c.compare; if c.compare { c.reset_real = true; } }
        c.executes += 1;
        if c.use_ours && c.up.is_some() {
            let fr = frame(c, cl, ep);
            let (compare, reset_real) = (c.compare, c.reset_real);
            let up = c.up.as_mut().unwrap();
            up.set_settings(cfg.s);
            if let Some(fr) = fr {
                if compare { let mut r = unsafe { std::ptr::read(e) }; if reset_real { r.reset = 1; } unsafe { (o.exec)(h, cl, &r); } c.reset_real = false; }
                let up = c.up.as_mut().unwrap();
                up.set_split(compare);
                if up.dispatch(&fr, switched) {
                    stats::publish(2, &fr.n, up.upscale_ms, up.frames());
                    if up.frames() == 1 || up.frames() % 3600 == 0 { log!("FlowScale SR ran (frame {})", up.frames()); }
                    return Some(OK);
                }
            }
        }
        if c.reset_real { c.reset_real = false; let mut r = unsafe { std::ptr::read(e) }; r.reset = 1; return Some(unsafe { (o.exec)(h, cl, &r) }); }
        None
    }).flatten();
    outcome.unwrap_or_else(|| unsafe { (o.exec)(h, cl, e) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessDestroyContext(h: Handle) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    if let Some(c) = CTX.lock().unwrap().as_mut().and_then(|m| m.remove(&(h as usize))) { log!("XeSS context destroyed after {} executes", c.executes); }
    unsafe { (o.destroy)(h) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessSetVelocityScale(h: Handle, x: f32, y: f32) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    with(h, |c| c.velocity = [x, y]);
    unsafe { (o.velocity)(h, x, y) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessSetJitterScale(h: Handle, x: f32, y: f32) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    with(h, |c| c.jitter = [x, y]);
    unsafe { (o.jitter)(h, x, y) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessSetExposureMultiplier(h: Handle, m: f32) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    with(h, |c| c.exposure = m);
    unsafe { (o.exposure)(h, m) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xessSetMaxResponsiveMaskValue(h: Handle, v: f32) -> XResult {
    let Some(o) = orig() else { return CANT_LOAD };
    with(h, |c| c.responsive = v);
    unsafe { (o.responsive)(h, v) }
}

mod thunks { include!(concat!(env!("OUT_DIR"), "/thunks.rs")); }

static ORIG_MODULE: OnceLock<usize> = OnceLock::new();
fn orig_module() -> Option<windows::Win32::Foundation::HMODULE> {
    let m = *ORIG_MODULE.get_or_init(|| unsafe {
        let p = config::dir().join("libxess_orig.dll");
        LoadLibraryW(&HSTRING::from(p.as_os_str())).map(|m| m.0 as usize).inspect_err(|e| log!("cannot load {}: {e:?}", p.display())).unwrap_or(0)
    });
    (m != 0).then_some(windows::Win32::Foundation::HMODULE(m as *mut c_void))
}

#[unsafe(no_mangle)]
pub extern "C" fn resolve_forward(i: u32) -> usize {
    let i = i as usize;
    let f = orig_module().and_then(|m| unsafe { GetProcAddress(m, PCSTR(thunks::NAMES[i].as_ptr())) }).map(|f| f as usize).unwrap_or(missing as extern "C" fn() -> XResult as usize);
    unsafe { std::ptr::write_volatile(std::ptr::addr_of_mut!(thunks::SLOTS[i]), f); }
    f
}
extern "C" fn missing() -> XResult { CANT_LOAD }
