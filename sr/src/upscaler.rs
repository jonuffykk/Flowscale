use std::ffi::c_void;
use std::mem::{transmute_copy, ManuallyDrop};

use windows::core::{w, Interface, Result};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::config::Settings;
use crate::consts::{self, AoFrame, Constants, FrameNums};
use crate::ffx;
use crate::log;

mod blobs { include!(concat!(env!("OUT_DIR"), "/shaders.rs")); }

const K_CONSTANTS: u32 = (std::mem::size_of::<Constants>() / 4) as u32;
const K_SRVS: usize = 11;
const K_UAVS: usize = 5;
const K_TABLE: u32 = (K_SRVS + K_UAVS) as u32;
const K_TABLES_PER_FRAME: u32 = 8;
const K_FRAMES_IN_RING: u32 = 256;
const UP_GX: u32 = 16;
const UP_GY: u32 = 8;
const TIME_RING: u32 = 8;
const STAMPS: u32 = 5;

pub struct CreateInfo { pub flags: u32, pub max_upscale: [u32; 2] }

pub struct Frame {
    pub cl: ID3D12GraphicsCommandList,
    pub color: ID3D12Resource, pub depth: Option<ID3D12Resource>, pub mv: ID3D12Resource, pub exposure: Option<ID3D12Resource>,
    pub reactive: Option<ID3D12Resource>, pub output: ID3D12Resource,
    pub color_state: D3D12_RESOURCE_STATES, pub depth_state: D3D12_RESOURCE_STATES, pub mv_state: D3D12_RESOURCE_STATES,
    pub exposure_state: D3D12_RESOURCE_STATES, pub reactive_state: D3D12_RESOURCE_STATES, pub output_state: D3D12_RESOURCE_STATES,
    pub n: FrameNums, pub reset: bool, pub fov_y: f32, pub z_near: f32, pub z_far: f32, pub meters_per_unit: f32,
}

pub fn d3d_state(s: u32) -> D3D12_RESOURCE_STATES {
    match s {
        ffx::STATE_COMMON => D3D12_RESOURCE_STATE_COMMON,
        ffx::STATE_UNORDERED_ACCESS => D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        ffx::STATE_COMPUTE_READ => D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        ffx::STATE_PIXEL_READ => D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        ffx::STATE_PIXEL_COMPUTE_READ => D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        ffx::STATE_COPY_SRC => D3D12_RESOURCE_STATE_COPY_SOURCE,
        ffx::STATE_COPY_DEST => D3D12_RESOURCE_STATE_COPY_DEST,
        ffx::STATE_GENERIC_READ => D3D12_RESOURCE_STATE_GENERIC_READ,
        ffx::STATE_INDIRECT_ARGUMENT => D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
        ffx::STATE_PRESENT => D3D12_RESOURCE_STATE_PRESENT,
        ffx::STATE_RENDER_TARGET => D3D12_RESOURCE_STATE_RENDER_TARGET,
        _ => D3D12_RESOURCE_STATE_COMMON,
    }
}

fn view_format(f: DXGI_FORMAT, uav: bool) -> DXGI_FORMAT {
    match f {
        DXGI_FORMAT_R32G32B32A32_TYPELESS => DXGI_FORMAT_R32G32B32A32_FLOAT,
        DXGI_FORMAT_R16G16B16A16_TYPELESS => DXGI_FORMAT_R16G16B16A16_FLOAT,
        DXGI_FORMAT_R32G32_TYPELESS => DXGI_FORMAT_R32G32_FLOAT,
        DXGI_FORMAT_R10G10B10A2_TYPELESS => DXGI_FORMAT_R10G10B10A2_UNORM,
        DXGI_FORMAT_R8G8B8A8_TYPELESS => DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB if uav => DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_TYPELESS => DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_UNORM_SRGB if uav => DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_R16G16_TYPELESS => DXGI_FORMAT_R16G16_FLOAT,
        DXGI_FORMAT_R32_TYPELESS | DXGI_FORMAT_D32_FLOAT => DXGI_FORMAT_R32_FLOAT,
        DXGI_FORMAT_R32G8X24_TYPELESS | DXGI_FORMAT_D32_FLOAT_S8X24_UINT => DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS,
        DXGI_FORMAT_R24G8_TYPELESS | DXGI_FORMAT_D24_UNORM_S8_UINT => DXGI_FORMAT_R24_UNORM_X8_TYPELESS,
        DXGI_FORMAT_R16_TYPELESS | DXGI_FORMAT_D16_UNORM => DXGI_FORMAT_R16_UNORM,
        DXGI_FORMAT_R8_TYPELESS => DXGI_FORMAT_R8_UNORM,
        DXGI_FORMAT_R8G8_TYPELESS => DXGI_FORMAT_R8G8_UNORM,
        _ => f,
    }
}
pub fn float_format(f: DXGI_FORMAT) -> bool {
    matches!(view_format(f, false), DXGI_FORMAT_R32G32B32A32_FLOAT | DXGI_FORMAT_R32G32B32_FLOAT | DXGI_FORMAT_R16G16B16A16_FLOAT | DXGI_FORMAT_R11G11B10_FLOAT | DXGI_FORMAT_R9G9B9E5_SHAREDEXP)
}

fn transition(r: &ID3D12Resource, from: D3D12_RESOURCE_STATES, to: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 { Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
            pResource: unsafe { transmute_copy(r) }, Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, StateBefore: from, StateAfter: to }) },
    }
}
fn uav_barrier(r: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV, Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 { UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER { pResource: unsafe { transmute_copy(r) } }) },
    }
}

#[derive(Default)]
struct Tex { res: Option<ID3D12Resource>, state: D3D12_RESOURCE_STATES, fmt: DXGI_FORMAT, w: u32, h: u32 }

#[derive(Default)]
struct Psos {
    convert: Option<ID3D12PipelineState>, motion: Option<ID3D12PipelineState>, upscale: Option<ID3D12PipelineState>, sharpen: Option<ID3D12PipelineState>,
    upscale_as: Option<ID3D12PipelineState>, upscale16: Option<ID3D12PipelineState>, upscale16_as: Option<ID3D12PipelineState>, sharpen16: Option<ID3D12PipelineState>,
    fm: [[Option<ID3D12PipelineState>; 2]; 2],
    ao_prep: Option<ID3D12PipelineState>, ao_main: Option<ID3D12PipelineState>, ao_main16: Option<ID3D12PipelineState>, ao_filter: Option<ID3D12PipelineState>,
    ao_temporal: Option<ID3D12PipelineState>, ao_compose: Option<ID3D12PipelineState>, ao_mipall: Option<ID3D12PipelineState>, ao_lut: Option<ID3D12PipelineState>,
    ao_cacao: Option<ID3D12PipelineState>, ao_cacao2: Option<ID3D12PipelineState>, ao_curve: Option<ID3D12PipelineState>,
}

pub struct Upscaler {
    dev: ID3D12Device, ci: CreateInfo, s: Settings,
    root: ID3D12RootSignature, p: Psos, heap: ID3D12DescriptorHeap, heap_size: u32, heap_next: u32, inc: u32,
    amd: bool, last_fp16: Option<bool>, hdr_from_format: bool, out_check: Option<(DXGI_FORMAT, bool, bool)>,
    mda: [Tex; 2], hist: [Tex; 2], aux: [Tex; 2], stab: [Tex; 2], env: [Tex; 2], expo: [Tex; 2], change1: Tex,
    ao_col: Tex, ao_half: Tex, ao_half_f: Tex, ao_nd: Tex, ao_ndp: Tex, ao_h: [Tex; 2], ao_hd: [Tex; 2], ao_mip: [Tex; 4], ao_lut: Tex, ao_curve: Tex,
    ao_lut_made: bool, curve_key: [f32; 26], ao_cur: usize, ao_ok: bool, ao_last: u64,
    split: bool, cur: usize, tex_valid: bool, frames: u64, prev_jit: [f32; 2],
    time_q: Option<ID3D12QueryHeap>, time_rb: Option<ID3D12Resource>, time_map: *const u64, tick_ms: f64, ms_acc: f64, ms_n: u32,
    pub upscale_ms: f32,
}
unsafe impl Send for Upscaler {}

fn pso(dev: &ID3D12Device, root: &ID3D12RootSignature, code: &[u8]) -> Result<ID3D12PipelineState> {
    let d = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: unsafe { transmute_copy(root) },
        CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr() as *const c_void, BytecodeLength: code.len() },
        ..Default::default()
    };
    unsafe { dev.CreateComputePipelineState(&d) }
}

impl Upscaler {
    pub fn new(dev: ID3D12Device, ci: CreateInfo, s: Settings) -> Result<Upscaler> {
        unsafe {
            let ranges = [
                D3D12_DESCRIPTOR_RANGE { RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV, NumDescriptors: K_SRVS as u32, BaseShaderRegister: 0, RegisterSpace: 0, OffsetInDescriptorsFromTableStart: 0 },
                D3D12_DESCRIPTOR_RANGE { RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV, NumDescriptors: K_UAVS as u32, BaseShaderRegister: 0, RegisterSpace: 0, OffsetInDescriptorsFromTableStart: K_SRVS as u32 },
            ];
            let params = [
                D3D12_ROOT_PARAMETER { ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS, ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    Anonymous: D3D12_ROOT_PARAMETER_0 { Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: K_CONSTANTS } } },
                D3D12_ROOT_PARAMETER { ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE, ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    Anonymous: D3D12_ROOT_PARAMETER_0 { DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 2, pDescriptorRanges: ranges.as_ptr() } } },
            ];
            let samp = D3D12_STATIC_SAMPLER_DESC { Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR, AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP, AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP, MaxLOD: D3D12_FLOAT32_MAX, ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL, ..Default::default() };
            let rd = D3D12_ROOT_SIGNATURE_DESC { NumParameters: 2, pParameters: params.as_ptr(), NumStaticSamplers: 1, pStaticSamplers: &samp, Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE };
            let mut blob: Option<ID3DBlob> = None; let mut err: Option<ID3DBlob> = None;
            if let Err(e) = D3D12SerializeRootSignature(&rd, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, Some(&mut err)) {
                log!("root signature: {e:?}"); return Err(e);
            }
            let blob = blob.unwrap();
            let root: ID3D12RootSignature = dev.CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()))?;
            let mk = |c: &[u8], n: &str| pso(&dev, &root, c).inspect_err(|e| log!("pipeline {n}: {e:?}"));
            let mut p = Psos {
                convert: Some(mk(blobs::CONVERT, "convert")?), motion: Some(mk(blobs::MOTION, "motion")?), upscale: Some(mk(blobs::UPSCALE, "upscale")?),
                sharpen: Some(mk(blobs::SHARPEN, "sharpen")?), upscale_as: Some(mk(blobs::UPSCALE_AS, "upscale_as")?),
                ao_prep: Some(mk(blobs::AOPREP, "aoprep")?), ao_main: Some(mk(blobs::AOMAIN, "aomain")?), ao_filter: Some(mk(blobs::AOFILTER, "aofilter")?),
                ao_temporal: Some(mk(blobs::AOTEMPORAL, "aotemporal")?), ao_compose: Some(mk(blobs::AOCOMPOSE, "aocompose")?),
                ao_main16: Some(mk(blobs::AOMAIN16, "aomain16")?), ao_mipall: Some(mk(blobs::AOMIPALL, "aomipall")?), ao_lut: Some(mk(blobs::AOLUT, "aolut")?),
                ao_cacao: Some(mk(blobs::AOCACAO, "aocacao")?), ao_cacao2: Some(mk(blobs::AOCACAO2, "aocacao2")?), ao_curve: Some(mk(blobs::AOCURVE, "aocurve")?),
                ..Default::default()
            };
            let mut sm = D3D12_FEATURE_DATA_SHADER_MODEL { HighestShaderModel: D3D_SHADER_MODEL_6_2 };
            let mut o4 = D3D12_FEATURE_DATA_D3D12_OPTIONS4::default();
            let native16 = dev.CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, &mut sm as *mut _ as *mut c_void, size_of_val(&sm) as u32).is_ok()
                && sm.HighestShaderModel.0 >= D3D_SHADER_MODEL_6_2.0
                && dev.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS4, &mut o4 as *mut _ as *mut c_void, size_of_val(&o4) as u32).is_ok()
                && o4.Native16BitShaderOpsSupported.as_bool();
            if native16 {
                let (a, b, c) = (pso(&dev, &root, blobs::UPSCALE16).ok(), pso(&dev, &root, blobs::SHARPEN16).ok(), pso(&dev, &root, blobs::UPSCALE16_AS).ok());
                if a.is_some() && b.is_some() && c.is_some() { p.upscale16 = a; p.sharpen16 = b; p.upscale16_as = c; }
            }
            p.fm[0][0] = pso(&dev, &root, blobs::UPSCALE_FM).ok();
            p.fm[0][1] = pso(&dev, &root, blobs::UPSCALE_AS_FM).ok();
            if p.upscale16.is_some() {
                p.fm[1][0] = pso(&dev, &root, blobs::UPSCALE16_FM).ok();
                p.fm[1][1] = pso(&dev, &root, blobs::UPSCALE16_AS_FM).ok();
            }
            let mut amd = false;
            if let Ok(f) = CreateDXGIFactory1::<IDXGIFactory4>() {
                if let Ok(a) = f.EnumAdapterByLuid::<IDXGIAdapter1>(dev.GetAdapterLuid()) {
                    if let Ok(d) = a.GetDesc1() {
                        amd = d.VendorId == 0x1002;
                        let n = d.Description.iter().position(|&c| c == 0).unwrap_or(128);
                        log!("GPU: {} (vendor 0x{:04x})", String::from_utf16_lossy(&d.Description[..n]), d.VendorId);
                    }
                }
            }
            log!("FP16 shaders: {}", if p.upscale16.is_some() { "available" } else if native16 { "failed, FP32" } else { "no native 16-bit math, FP32" });
            let heap_size = K_TABLE * K_TABLES_PER_FRAME * K_FRAMES_IN_RING;
            let heap: ID3D12DescriptorHeap = dev.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, NumDescriptors: heap_size, Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, NodeMask: 0 })?;
            let inc = dev.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            let mut u = Upscaler { dev, ci, s, root, p, heap, heap_size, heap_next: 0, inc, amd, last_fp16: None, hdr_from_format: false, out_check: None,
                mda: Default::default(), hist: Default::default(), aux: Default::default(), stab: Default::default(), env: Default::default(), expo: Default::default(),
                change1: Tex::default(), ao_col: Tex::default(), ao_half: Tex::default(), ao_half_f: Tex::default(), ao_nd: Tex::default(), ao_ndp: Tex::default(), ao_h: Default::default(), ao_hd: Default::default(),
                ao_mip: Default::default(), ao_lut: Tex::default(), ao_curve: Tex::default(), ao_lut_made: false, curve_key: [0.0; 26],
                ao_cur: 0, ao_ok: false, ao_last: u64::MAX, split: false, cur: 0, tex_valid: false, frames: 0, prev_jit: [0.0; 2],
                time_q: None, time_rb: None, time_map: std::ptr::null(), tick_ms: 0.0, ms_acc: 0.0, ms_n: 0, upscale_ms: 0.0 };
            u.init_timing();
            Ok(u)
        }
    }

    fn init_timing(&mut self) {
        unsafe {
            let Ok(q) = self.dev.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() }) else { return };
            let Ok(hz) = q.GetTimestampFrequency() else { return };
            if hz == 0 { return; }
            let mut qh: Option<ID3D12QueryHeap> = None;
            if self.dev.CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: STAMPS * TIME_RING, NodeMask: 0 }, &mut qh).is_err() { return; }
            let Some(qh) = qh else { return };
            let hp = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
            let bd = D3D12_RESOURCE_DESC { Dimension: D3D12_RESOURCE_DIMENSION_BUFFER, Width: 8 * (STAMPS * TIME_RING) as u64, Height: 1, DepthOrArraySize: 1, MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR, ..Default::default() };
            let mut rb: Option<ID3D12Resource> = None;
            if self.dev.CreateCommittedResource(&hp, D3D12_HEAP_FLAG_NONE, &bd, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut rb).is_err() { return; }
            let rb = rb.unwrap();
            let mut p: *mut c_void = std::ptr::null_mut();
            if rb.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut p)).is_err() { return; }
            self.time_map = p as *const u64; self.time_q = Some(qh); self.time_rb = Some(rb); self.tick_ms = 1000.0 / hz as f64;
        }
    }

    pub fn frames(&self) -> u64 { self.frames }
    pub fn set_settings(&mut self, s: Settings) { self.s = s; }
    pub fn set_split(&mut self, on: bool) { self.split = on; }

    fn make_tex(&self, t: &mut Tex, w: u32, h: u32, f: DXGI_FORMAT) -> bool {
        let hp = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let rd = D3D12_RESOURCE_DESC { Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D, Width: w as u64, Height: h, DepthOrArraySize: 1, MipLevels: 1, Format: f,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN, Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, ..Default::default() };
        let mut r: Option<ID3D12Resource> = None;
        let st = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        if let Err(e) = unsafe { self.dev.CreateCommittedResource(&hp, D3D12_HEAP_FLAG_NONE, &rd, st, None, &mut r) } { log!("CreateCommittedResource {w}x{h}: {e:?}"); return false; }
        let r = r.unwrap();
        unsafe { let _ = r.SetName(w!("FlowScale SR")); }
        *t = Tex { res: Some(r), state: st, fmt: f, w, h };
        true
    }

    fn release_textures(&mut self) {
        for t in self.mda.iter_mut().chain(self.hist.iter_mut()).chain(self.aux.iter_mut()).chain(self.stab.iter_mut()).chain(self.env.iter_mut()).chain(self.expo.iter_mut()) { *t = Tex::default(); }
        self.change1 = Tex::default();
        self.release_ao();
        self.tex_valid = false;
    }
    fn release_ao(&mut self) {
        for t in [&mut self.ao_col, &mut self.ao_half, &mut self.ao_half_f, &mut self.ao_nd, &mut self.ao_ndp] { *t = Tex::default(); }
        for t in self.ao_h.iter_mut().chain(self.ao_hd.iter_mut()).chain(self.ao_mip.iter_mut()) { *t = Tex::default(); }
        self.ao_ok = false;
    }

    fn ensure_textures(&mut self, rw: u32, rh: u32, ow: u32, oh: u32, hf: DXGI_FORMAT, sf: DXGI_FORMAT) -> Option<bool> {
        if self.tex_valid && self.mda[0].w == rw && self.mda[0].h == rh && self.hist[0].w == ow && self.hist[0].h == oh && self.hist[0].fmt == hf && self.stab[0].fmt == sf { return Some(false); }
        self.release_textures();
        let mut t: [Tex; 13] = Default::default();
        let spec: [(u32, u32, DXGI_FORMAT); 13] = [
            (rw, rh, DXGI_FORMAT_R16G16B16A16_FLOAT), (rw, rh, DXGI_FORMAT_R16G16B16A16_FLOAT), (ow, oh, hf), (ow, oh, hf),
            (rw, rh, DXGI_FORMAT_R8G8B8A8_UNORM), (rw, rh, DXGI_FORMAT_R8G8B8A8_UNORM), (rw, rh, DXGI_FORMAT_R8G8_UNORM), (rw, rh, DXGI_FORMAT_R8G8_UNORM),
            (ow, oh, sf), (ow, oh, sf), (1, 1, DXGI_FORMAT_R16_FLOAT), (1, 1, DXGI_FORMAT_R32_FLOAT), (1, 1, DXGI_FORMAT_R32_FLOAT)];
        for (x, (w, h, f)) in t.iter_mut().zip(spec) { if !self.make_tex(x, w, h, f) { return None; } }
        let [m0, m1, h0, h1, a0, a1, e0, e1, s0, s1, c1, x0, x1] = t;
        self.mda = [m0, m1]; self.hist = [h0, h1]; self.aux = [a0, a1]; self.env = [e0, e1]; self.stab = [s0, s1]; self.change1 = c1; self.expo = [x0, x1];
        self.tex_valid = true;
        log!("textures: render {rw}x{rh}, output {ow}x{oh}, history {}", if hf == DXGI_FORMAT_R10G10B10A2_UNORM { "RGB10A2" } else { "RGBA16F" });
        Some(true)
    }

    fn output_writable(&mut self, od: &D3D12_RESOURCE_DESC) -> bool {
        let uav = (od.Flags & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS) == D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
        if let Some((f, u, ok)) = self.out_check { if f == od.Format && u == uav { return ok; } }
        let mut fs = D3D12_FEATURE_DATA_FORMAT_SUPPORT { Format: view_format(od.Format, true), ..Default::default() };
        let store = unsafe { self.dev.CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, &mut fs as *mut _ as *mut c_void, size_of_val(&fs) as u32) }.is_ok()
            && (fs.Support1.0 & D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW.0) != 0 && (fs.Support2.0 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE.0) != 0;
        let ok = uav && store;
        log!("output: dxgi {}, unordered access {}, typed UAV stores {}{}", od.Format.0, uav, store, if ok { "" } else { " - not written" });
        self.out_check = Some((od.Format, uav, ok));
        ok
    }

    fn table(&mut self, srv: &[Option<&ID3D12Resource>; K_SRVS], sf: &[DXGI_FORMAT; K_SRVS], uav: &[Option<&ID3D12Resource>; K_UAVS], uf: &[DXGI_FORMAT; K_UAVS]) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        if self.heap_next + K_TABLE > self.heap_size { self.heap_next = 0; }
        let base = self.heap_next; self.heap_next += K_TABLE;
        unsafe {
            let cpu0 = self.heap.GetCPUDescriptorHandleForHeapStart().ptr + (base * self.inc) as usize;
            let gpu = D3D12_GPU_DESCRIPTOR_HANDLE { ptr: self.heap.GetGPUDescriptorHandleForHeapStart().ptr + (base * self.inc) as u64 };
            for i in 0..K_TABLE as usize {
                let h = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: cpu0 + i * self.inc as usize };
                if i < K_SRVS {
                    let d = D3D12_SHADER_RESOURCE_VIEW_DESC { Format: if srv[i].is_some() { view_format(sf[i], false) } else { DXGI_FORMAT_R8G8B8A8_UNORM },
                        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D, Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D12_TEX2D_SRV { MipLevels: 1, ..Default::default() } } };
                    self.dev.CreateShaderResourceView(srv[i], Some(&d), h);
                } else {
                    let j = i - K_SRVS;
                    let d = D3D12_UNORDERED_ACCESS_VIEW_DESC { Format: if uav[j].is_some() { view_format(uf[j], true) } else { DXGI_FORMAT_R8G8B8A8_UNORM },
                        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D, Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 { Texture2D: D3D12_TEX2D_UAV::default() } };
                    self.dev.CreateUnorderedAccessView(uav[j], None, Some(&d), h);
                }
            }
            gpu
        }
    }

    fn trans(cl: &ID3D12GraphicsCommandList, t: &mut Tex, to: D3D12_RESOURCE_STATES) {
        if t.state == to { return; }
        if let Some(r) = t.res.as_ref() { unsafe { cl.ResourceBarrier(&[transition(r, t.state, to)]); } }
        t.state = to;
    }

    #[allow(clippy::too_many_arguments)]
    fn run<T: Copy>(&mut self, cl: &ID3D12GraphicsCommandList, pso: &ID3D12PipelineState, k: &T,
                    srv: [Option<&ID3D12Resource>; K_SRVS], sf: [DXGI_FORMAT; K_SRVS], uav: [Option<&ID3D12Resource>; K_UAVS], uf: [DXGI_FORMAT; K_UAVS], gx: u32, gy: u32) {
        let t = self.table(&srv, &sf, &uav, &uf);
        unsafe {
            cl.SetPipelineState(pso);
            cl.SetComputeRoot32BitConstants(0, (std::mem::size_of::<T>() / 4) as u32, k as *const T as *const c_void, 0);
            cl.SetComputeRootDescriptorTable(1, t);
            cl.Dispatch(gx, gy, 1);
        }
    }

    pub fn dispatch(&mut self, fr: &Frame, force_reset: bool) -> bool {
        let (rw, rh, ow, oh) = (fr.n.rw, fr.n.rh, fr.n.ow, fr.n.oh);
        if rw == 0 || rh == 0 || ow == 0 || oh == 0 { return false; }
        let cl = &fr.cl;
        let (cd, md, od) = unsafe { (fr.color.GetDesc(), fr.mv.GetDesc(), fr.output.GetDesc()) };
        let df = fr.depth.as_ref().map(|d| unsafe { d.GetDesc() }.Format).unwrap_or(DXGI_FORMAT_UNKNOWN);
        if !self.output_writable(&od) { return false; }
        let s = self.s;
        let hfmt = if s.history10 { DXGI_FORMAT_R10G10B10A2_UNORM } else { DXGI_FORMAT_R16G16B16A16_FLOAT };
        let sfmt = if s.anti_shimmer != 0.0 { DXGI_FORMAT_R8G8_UNORM } else { DXGI_FORMAT_R8_UNORM };
        let Some(resized) = self.ensure_textures(rw, rh, ow, oh, hfmt, sfmt) else { return false };
        let reset = fr.reset || force_reset || resized;

        let mut uses: Vec<(ID3D12Resource, D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATES)> = Vec::with_capacity(6);
        let mut useit = |r: Option<&ID3D12Resource>, st: D3D12_RESOURCE_STATES, need: D3D12_RESOURCE_STATES| {
            let Some(r) = r else { return };
            let uav = need == D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
            if (st & need) == need && !(uav && st != need) { return; }
            let to = if uav { need } else { (st & D3D12_RESOURCE_STATE_DEPTH_READ) | need };
            uses.push((r.clone(), st, to));
        };
        let srvst = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        useit(Some(&fr.color), fr.color_state, srvst);
        useit(fr.depth.as_ref(), fr.depth_state, srvst);
        useit(Some(&fr.mv), fr.mv_state, srvst);
        useit(fr.exposure.as_ref(), fr.exposure_state, srvst);
        useit(fr.reactive.as_ref(), fr.reactive_state, srvst);
        useit(Some(&fr.output), fr.output_state, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        unsafe {
            if !uses.is_empty() { let b: Vec<_> = uses.iter().map(|(r, f, t)| transition(r, *f, *t)).collect(); cl.ResourceBarrier(&b); }
        }

        let ts = (self.frames % TIME_RING as u64) as u32;
        if self.tick_ms > 0.0 {
            unsafe {
                let st = std::slice::from_raw_parts(self.time_map.add((STAMPS * ts) as usize), STAMPS as usize);
                let (a, b) = (st[0], st[STAMPS as usize - 1]);
                if self.frames >= TIME_RING as u64 && b > a && ((b - a) as f64 * self.tick_ms) < 1000.0 {
                    let ms = (b - a) as f64 * self.tick_ms;
                    self.upscale_ms = ms as f32; self.ms_acc += ms; self.ms_n += 1;
                    if self.ms_n == 600 { log!("GPU time: {:.3} ms (mean of 600)", self.ms_acc / 600.0); self.ms_acc = 0.0; self.ms_n = 0; }
                }
                cl.EndQuery(self.time_q.as_ref().unwrap(), D3D12_QUERY_TYPE_TIMESTAMP, STAMPS * ts);
            }
        }

        let auto_expo = fr.exposure.is_none() && (self.ci.flags & ffx::ENABLE_AUTO_EXPOSURE) != 0 && rw >= 16;
        let mut k = consts::fill(&s, &fr.n, reset, fr.exposure.is_some() || auto_expo, fr.reactive.is_some(), hfmt == DXGI_FORMAT_R10G10B10A2_UNORM, &mut self.prev_jit);
        k.auto_exposure = auto_expo as u8 as f32;
        if self.split { k.split_x = (ow / 2) as f32; }
        k.fused_sharpen = if !self.split && s.debug_view == 0 && k.sharpen_mode == 0.0 && s.fused_sharpen > 0.0 && k.ratio <= 2.55 { s.fused_sharpen } else { 0.0 };
        if !fr.n.hdr && float_format(cd.Format) {
            if !self.hdr_from_format { log!("colour: float format {} without the HDR flag - read as HDR", cd.Format.0); self.hdr_from_format = true; }
            k.hdr = 1.0;
        }
        let fp16 = self.p.upscale16.is_some() && (s.fp16 > 0 || (s.fp16 < 0 && self.amd));
        if self.last_fp16 != Some(fp16) { log!("shaders: {}", if fp16 { "FP16" } else { "FP32" }); self.last_fp16 = Some(fp16); }

        self.cur ^= 1;
        let (c, p) = (self.cur, self.cur ^ 1);
        unsafe {
            cl.SetDescriptorHeaps(&[Some(self.heap.clone())]);
            cl.SetComputeRootSignature(&self.root);
        }
        const X: DXGI_FORMAT = DXGI_FORMAT_UNKNOWN;
        let rf = fr.reactive.as_ref().map(|r| unsafe { r.GetDesc() }.Format).unwrap_or(X);
        if auto_expo {
            Self::trans(cl, &mut self.expo[p], srvst);
            Self::trans(cl, &mut self.expo[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        }
        let expo_in: Option<ID3D12Resource> = if auto_expo { self.expo[p].res.clone() } else { fr.exposure.clone() };
        let exf_in = expo_in.as_ref().map(|r| unsafe { r.GetDesc() }.Format).unwrap_or(X);

        let mut color = fr.color.clone();
        let mut cfmt = cd.Format;
        if s.relight && s.debug_view == 0 {
            if let Some(depth) = fr.depth.as_ref() { if let Some((col, f)) = self.relight(cl, fr, depth, df, &fr.color, cd.Format, md.Format, reset) { color = col; cfmt = f; } }
        }

        Self::trans(cl, &mut self.mda[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        Self::trans(cl, &mut self.change1, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        Self::trans(cl, &mut self.hist[p], srvst);
        {
            let (mc, ch, hp, ec) = (self.mda[c].res.clone(), self.change1.res.clone(), self.hist[p].res.clone(), if auto_expo { self.expo[c].res.clone() } else { None });
            let s_ = [fr.depth.as_ref(), Some(&fr.mv), Some(&color), hp.as_ref(), None, expo_in.as_ref(), None, None, None, None, None];
            let f_ = [df, md.Format, cfmt, self.hist[p].fmt, X, exf_in, X, X, X, X, X];
            let u_ = [mc.as_ref(), ch.as_ref(), ec.as_ref(), None, None];
            let uf_ = [self.mda[c].fmt, self.change1.fmt, if auto_expo { self.expo[c].fmt } else { X }, X, X];
            let pso = self.p.convert.clone().unwrap();
            self.run(cl, &pso, &k, s_, f_, u_, uf_, rw.div_ceil(8), rh.div_ceil(8) + 1);
        }
        self.stamp(cl, ts, 1);
        let expo: Option<ID3D12Resource> = if auto_expo { Self::trans(cl, &mut self.expo[c], srvst); self.expo[c].res.clone() } else { fr.exposure.clone() };
        let exf = expo.as_ref().map(|r| unsafe { r.GetDesc() }.Format).unwrap_or(X);
        Self::trans(cl, &mut self.mda[c], srvst);
        Self::trans(cl, &mut self.change1, srvst);
        Self::trans(cl, &mut self.mda[p], srvst);
        Self::trans(cl, &mut self.aux[p], srvst);
        let anti = s.anti_shimmer != 0.0;
        let fm = if rw == ow && rh == oh { self.p.fm[fp16 as usize][anti as usize].clone() } else { None };
        Self::trans(cl, &mut self.env[p], srvst);
        Self::trans(cl, &mut self.env[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        Self::trans(cl, &mut self.aux[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        if fm.is_none() {
            let (ap, mc, mp, ep, ac, ec) = (self.aux[p].res.clone(), self.mda[c].res.clone(), self.mda[p].res.clone(), self.env[p].res.clone(), self.aux[c].res.clone(), self.env[c].res.clone());
            let s_ = [ap.as_ref(), mc.as_ref(), Some(&color), mp.as_ref(), ep.as_ref(), expo.as_ref(), None, None, None, None, None];
            let f_ = [self.aux[p].fmt, self.mda[c].fmt, cfmt, self.mda[p].fmt, self.env[p].fmt, exf, X, X, X, X, X];
            let u_ = [ac.as_ref(), ec.as_ref(), None, None, None];
            let uf_ = [self.aux[c].fmt, self.env[c].fmt, X, X, X];
            let pso = self.p.motion.clone().unwrap();
            self.run(cl, &pso, &k, s_, f_, u_, uf_, rw.div_ceil(8), rh.div_ceil(8));
            Self::trans(cl, &mut self.aux[c], srvst);
        }
        self.stamp(cl, ts, 2);
        Self::trans(cl, &mut self.hist[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        Self::trans(cl, &mut self.stab[c], D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        Self::trans(cl, &mut self.stab[p], srvst);
        {
            let r = |t: &Tex| t.res.clone();
            let (hp, mc, ac, ch, sp, ap, mp, ep, hc, sc, ec) = (r(&self.hist[p]), r(&self.mda[c]), r(&self.aux[c]), r(&self.change1), r(&self.stab[p]),
                r(&self.aux[p]), r(&self.mda[p]), r(&self.env[p]), r(&self.hist[c]), r(&self.stab[c]), r(&self.env[c]));
            let f = fm.is_some();
            let s_ = [hp.as_ref(), mc.as_ref(), Some(&color), if f { None } else { ac.as_ref() }, ch.as_ref(), expo.as_ref(), fr.reactive.as_ref(), sp.as_ref(),
                if f { ap.as_ref() } else { None }, if f { mp.as_ref() } else { None }, if f { ep.as_ref() } else { None }];
            let f_ = [self.hist[p].fmt, self.mda[c].fmt, cfmt, if f { X } else { self.aux[c].fmt }, self.change1.fmt, exf, rf, self.stab[p].fmt, self.aux[p].fmt, self.mda[p].fmt, self.env[p].fmt];
            let u_ = [hc.as_ref(), Some(&fr.output), sc.as_ref(), if f { ac.as_ref() } else { None }, if f { ec.as_ref() } else { None }];
            let uf_ = [self.hist[c].fmt, od.Format, self.stab[c].fmt, self.aux[c].fmt, self.env[c].fmt];
            let pso = fm.clone().unwrap_or_else(|| if fp16 { if anti { self.p.upscale16_as.clone() } else { self.p.upscale16.clone() } } else if anti { self.p.upscale_as.clone() } else { self.p.upscale.clone() }.unwrap());
            self.run(cl, &pso, &k, s_, f_, u_, uf_, ow.div_ceil(UP_GX), oh.div_ceil(UP_GY));
        }
        if fm.is_some() { Self::trans(cl, &mut self.aux[c], srvst); }
        self.stamp(cl, ts, 3);
        Self::trans(cl, &mut self.hist[c], srvst);
        Self::trans(cl, &mut self.stab[c], srvst);
        if s.debug_view == 0 && k.fused_sharpen <= 0.0 && !(k.sharpen_mode > 1.5 && !self.split) {
            let (hc, mc, sc) = (self.hist[c].res.clone(), self.mda[c].res.clone(), self.stab[c].res.clone());
            let s_ = [hc.as_ref(), mc.as_ref(), None, None, None, expo.as_ref(), None, sc.as_ref(), None, None, None];
            let f_ = [self.hist[c].fmt, self.mda[c].fmt, X, X, X, exf, X, self.stab[c].fmt, X, X, X];
            let u_ = [None, Some(&fr.output), None, None, None];
            let uf_ = [X, od.Format, X, X, X];
            let pso = if fp16 { self.p.sharpen16.clone() } else { self.p.sharpen.clone() }.unwrap();
            self.run(cl, &pso, &k, s_, f_, u_, uf_, ow.div_ceil(8), oh.div_ceil(8));
        }
        unsafe {
            cl.ResourceBarrier(&[uav_barrier(&fr.output)]);
            if !uses.is_empty() { let b: Vec<_> = uses.iter().map(|(r, f, t)| transition(r, *t, *f)).collect(); cl.ResourceBarrier(&b); }
        }
        if self.tick_ms > 0.0 {
            self.stamp(cl, ts, 4);
            unsafe { cl.ResolveQueryData(self.time_q.as_ref().unwrap(), D3D12_QUERY_TYPE_TIMESTAMP, STAMPS * ts, STAMPS, self.time_rb.as_ref().unwrap(), (8 * STAMPS * ts) as u64); }
        }
        self.frames += 1;
        true
    }

    fn stamp(&self, cl: &ID3D12GraphicsCommandList, ts: u32, i: u32) {
        if let Some(q) = self.time_q.as_ref() { unsafe { cl.EndQuery(q, D3D12_QUERY_TYPE_TIMESTAMP, STAMPS * ts + i); } }
    }

    #[allow(clippy::too_many_arguments)]
    /// Relight (CORTAO-style occluders, FastGI and CACAO filters): the prepass and the main pass at half size, the
    /// depth mips in one pass, the occluder table, the sparse filter, the temporal smoothing by history length, CACAO's
    /// two blurs in one, and the composition upsampling and applying the curve's baked table.
    fn relight(&mut self, cl: &ID3D12GraphicsCommandList, fr: &Frame, depth: &ID3D12Resource, df: DXGI_FORMAT, color: &ID3D12Resource, cf: DXGI_FORMAT, mf: DXGI_FORMAT, reset: bool) -> Option<(ID3D12Resource, DXGI_FORMAT)> {
        let (rw, rh) = (fr.n.rw, fr.n.rh);
        let (hw, hh) = (rw.div_ceil(2), rh.div_ceil(2));
        let s = self.s;
        const X: DXGI_FORMAT = DXGI_FORMAT_UNKNOWN;
        const F16: DXGI_FORMAT = DXGI_FORMAT_R16G16B16A16_FLOAT;
        let srvst = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let uavst = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
        if !(self.ao_col.res.is_some() && self.ao_col.w == rw && self.ao_col.h == rh) {
            self.release_ao();
            let mut t: [Tex; 5] = Default::default();
            let spec = [(rw, rh, F16), (hw, hh, F16), (hw, hh, F16), (hw, hh, DXGI_FORMAT_R32G32B32A32_FLOAT), (hw, hh, DXGI_FORMAT_R32G32_UINT)];
            for (x, (w, h, f)) in t.iter_mut().zip(spec) { if !self.make_tex(x, w, h, f) { return None; } }
            let [a, b, c, d, e] = t; self.ao_col = a; self.ao_half = b; self.ao_half_f = c; self.ao_nd = d; self.ao_ndp = e;
            for l in 1..=4u32 { let mut m = Tex::default(); if !self.make_tex(&mut m, (rw + (1 << l) - 1) >> l, (rh + (1 << l) - 1) >> l, DXGI_FORMAT_R32G32B32A32_UINT) { return None; } self.ao_mip[l as usize - 1] = m; }
        }
        let af = AoFrame { rw, rh, fov_y: fr.fov_y, z_near: fr.z_near, z_far: fr.z_far, meters_per_unit: fr.meters_per_unit, depth_inverted: fr.n.depth_inverted,
            hdr: fr.n.hdr, mv_to_uv: fr.n.mv_to_uv, mv_res: fr.n.mv_res };
        let mut ka = consts::fill_ao(&s, &af, float_format(cf), self.frames);
        ka.mips = 1.0; ka.cmips = s.ao_color_mips as u8 as f32; ka.half_res = 1.0;
        let mut ao_t = s.ao_temporal;
        if ao_t && self.ao_h[0].res.is_none() {
            let mut t: [Tex; 4] = Default::default();
            let fs = [F16, F16, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_FLOAT];
            ao_t = t.iter_mut().zip(fs).all(|(x, f)| self.make_tex(x, hw, hh, f));
            if ao_t { let [a, b, c, d] = t; self.ao_h = [a, b]; self.ao_hd = [c, d]; }
        }
        let valid = ao_t && self.ao_ok && self.ao_last.wrapping_add(1) == self.frames && !reset;
        ka.temporal = if valid { s.ao_temporal_weight.clamp(0.0, 0.95) } else { 0.0 };
        fn sv<'a>(v: &[Option<&'a ID3D12Resource>]) -> [Option<&'a ID3D12Resource>; K_SRVS] { let mut a = [None; K_SRVS]; a[..v.len()].copy_from_slice(v); a }
        fn sf(v: &[DXGI_FORMAT]) -> [DXGI_FORMAT; K_SRVS] { let mut a = [DXGI_FORMAT_UNKNOWN; K_SRVS]; a[..v.len()].copy_from_slice(v); a }
        fn uv<'a>(v: &[Option<&'a ID3D12Resource>]) -> [Option<&'a ID3D12Resource>; K_UAVS] { let mut a = [None; K_UAVS]; a[..v.len()].copy_from_slice(v); a }
        fn uf(v: &[DXGI_FORMAT]) -> [DXGI_FORMAT; K_UAVS] { let mut a = [DXGI_FORMAT_UNKNOWN; K_UAVS]; a[..v.len()].copy_from_slice(v); a }
        let r = |t: &Tex| t.res.clone();
        let lut = s.ao_lut && (self.ao_lut.res.is_some() || { let mut t = Tex::default(); let ok = self.make_tex(&mut t, 32 * 32, 32, DXGI_FORMAT_R32_UINT); if ok { self.ao_lut = t; } ok });
        if lut && !self.ao_lut_made {
            Self::trans(cl, &mut self.ao_lut, uavst);
            let (l, p) = (r(&self.ao_lut), self.p.ao_lut.clone().unwrap());
            self.run(cl, &p, &ka, sv(&[]), sf(&[]), uv(&[l.as_ref()]), uf(&[DXGI_FORMAT_R32_UINT]), (32 * 32u32).div_ceil(8), 4);
            Self::trans(cl, &mut self.ao_lut, srvst);
            self.ao_lut_made = true;
        }
        ka.lut = lut as u8 as f32;
        let cl8 = ka.curve_n > 1.5 && s.ao_curve_lut && (self.ao_curve.res.is_some() || { let mut t = Tex::default(); let ok = self.make_tex(&mut t, 4096, 1, DXGI_FORMAT_R32_FLOAT); if ok { self.ao_curve = t; self.curve_key[24] = -1.0; } ok });
        if cl8 {
            let mut key = [0f32; 26];
            key[..8].copy_from_slice(&ka.curve_x); key[8..16].copy_from_slice(&ka.curve_y); key[16..24].copy_from_slice(&ka.curve_y2); key[24] = ka.curve_n; key[25] = ka.pivot;
            if key != self.curve_key {
                Self::trans(cl, &mut self.ao_curve, uavst);
                let (c, p) = (r(&self.ao_curve), self.p.ao_curve.clone().unwrap());
                self.run(cl, &p, &ka, sv(&[]), sf(&[]), uv(&[c.as_ref()]), uf(&[DXGI_FORMAT_R32_FLOAT]), 4096 / 64, 1);
                Self::trans(cl, &mut self.ao_curve, srvst);
                self.curve_key = key;
            }
        }
        ka.flags = ((ka.flags as u32) | 4 | 8 | if s.ao_history_length { 32 } else { 0 } | 131072 | if cl8 { 262144 } else { 0 }) as f32;
        let (nd, ndp) = (r(&self.ao_nd), r(&self.ao_ndp));
        let (ndf, ndpf) = (self.ao_nd.fmt, self.ao_ndp.fmt);
        // Prepass at half size: normal and depth, packed copy at t7 for every later pass.
        Self::trans(cl, &mut self.ao_nd, uavst); Self::trans(cl, &mut self.ao_ndp, uavst);
        let p = self.p.ao_prep.clone().unwrap();
        self.run(cl, &p, &ka, sv(&[Some(depth)]), sf(&[df]), uv(&[nd.as_ref(), ndp.as_ref()]), uf(&[ndf, ndpf]), hw.div_ceil(8), hh.div_ceil(8));
        Self::trans(cl, &mut self.ao_nd, srvst); Self::trans(cl, &mut self.ao_ndp, srvst);
        macro_rules! with7 { ($a:expr, $f:expr) => {{ let mut a = $a; let mut f = $f; a[7] = ndp.as_ref(); f[7] = ndpf; (a, f) }} }
        // Depth mips 1 - 4 (the colour's mean packed in), one pass.
        for m in self.ao_mip.iter_mut() { Self::trans(cl, m, uavst); }
        let mips: Vec<Option<ID3D12Resource>> = self.ao_mip.iter().map(r).collect();
        let mipf = DXGI_FORMAT_R32G32B32A32_UINT;
        let (a, f) = with7!(sv(&[nd.as_ref(), Some(color)]), sf(&[ndf, cf]));
        let p = self.p.ao_mipall.clone().unwrap();
        self.run(cl, &p, &ka, a, f, uv(&[mips[0].as_ref(), mips[1].as_ref(), mips[2].as_ref(), mips[3].as_ref()]), uf(&[mipf, mipf, mipf, mipf]), self.ao_mip[0].w.div_ceil(16), self.ao_mip[0].h.div_ceil(16));
        for m in self.ao_mip.iter_mut() { Self::trans(cl, m, srvst); }
        // Main pass at half size.
        let (h0, h1) = (r(&self.ao_half), r(&self.ao_half_f));
        Self::trans(cl, &mut self.ao_half, uavst);
        let lr = r(&self.ao_lut);
        let (a, f) = with7!(sv(&[nd.as_ref(), Some(color), mips[0].as_ref(), mips[1].as_ref(), mips[2].as_ref(), mips[3].as_ref(), if lut { lr.as_ref() } else { None }]),
            sf(&[ndf, cf, mipf, mipf, mipf, mipf, if lut { DXGI_FORMAT_R32_UINT } else { X }]));
        let (p, g) = if s.ao_wg16 { (self.p.ao_main16.clone().unwrap(), 16) } else { (self.p.ao_main.clone().unwrap(), 8) };
        self.run(cl, &p, &ka, a, f, uv(&[h0.as_ref()]), uf(&[F16]), hw.div_ceil(g), hh.div_ceil(g));
        ka.rs = [hw as f32, hh as f32]; ka.rs_rcp = [1.0 / hw as f32, 1.0 / hh as f32];
        // Sparse spatial filter: half -> half_f.
        Self::trans(cl, &mut self.ao_half, srvst); Self::trans(cl, &mut self.ao_half_f, uavst);
        ka.axis = 2.0;
        let (a, f) = with7!(sv(&[nd.as_ref(), h0.as_ref()]), sf(&[ndf, F16]));
        let p = self.p.ao_filter.clone().unwrap();
        self.run(cl, &p, &ka, a, f, uv(&[h1.as_ref()]), uf(&[F16]), hw.div_ceil(8), hh.div_ceil(8));
        Self::trans(cl, &mut self.ao_half_f, srvst);
        let mut out = h1.clone();
        let mut out_is_f = true;
        if ao_t {
            let (ci, pi) = (self.ao_cur, self.ao_cur ^ 1);
            Self::trans(cl, &mut self.ao_h[pi], srvst); Self::trans(cl, &mut self.ao_hd[pi], srvst);
            Self::trans(cl, &mut self.ao_h[ci], uavst); Self::trans(cl, &mut self.ao_hd[ci], uavst);
            let (hp, hdp, hc, hdc) = (r(&self.ao_h[pi]), r(&self.ao_hd[pi]), r(&self.ao_h[ci]), r(&self.ao_hd[ci]));
            let (a, f) = with7!(sv(&[nd.as_ref(), h1.as_ref(), if valid { hp.as_ref() } else { h1.as_ref() }, hdp.as_ref(), Some(&fr.mv)]), sf(&[ndf, F16, F16, DXGI_FORMAT_R32_FLOAT, mf]));
            let p = self.p.ao_temporal.clone().unwrap();
            self.run(cl, &p, &ka, a, f, uv(&[hc.as_ref(), hdc.as_ref()]), uf(&[F16, DXGI_FORMAT_R32_FLOAT]), hw.div_ceil(8), hh.div_ceil(8));
            Self::trans(cl, &mut self.ao_h[ci], srvst);
            out = hc; out_is_f = false;
            self.ao_cur ^= 1; self.ao_ok = true; self.ao_last = self.frames;
        } else { self.ao_ok = false; }
        if s.ao_cacao {
            let passes = s.ao_cacao_passes.clamp(1, 8);
            let fused = passes == 2;
            for _ in 0..if fused { 1 } else { passes } {
                let to_f = !out_is_f;
                if to_f { Self::trans(cl, &mut self.ao_half_f, uavst); Self::trans(cl, &mut self.ao_half, srvst); } else { Self::trans(cl, &mut self.ao_half, uavst); Self::trans(cl, &mut self.ao_half_f, srvst); }
                let dst = if to_f { h1.clone() } else { h0.clone() };
                let (a, f) = with7!(sv(&[nd.as_ref(), out.as_ref()]), sf(&[ndf, F16]));
                let p = if fused { self.p.ao_cacao2.clone() } else { self.p.ao_cacao.clone() }.unwrap();
                self.run(cl, &p, &ka, a, f, uv(&[dst.as_ref()]), uf(&[F16]), hw.div_ceil(8), hh.div_ceil(8));
                if to_f { Self::trans(cl, &mut self.ao_half_f, srvst); } else { Self::trans(cl, &mut self.ao_half, srvst); }
                out = dst; out_is_f = to_f;
            }
        }
        ka.rs = [rw as f32, rh as f32]; ka.rs_rcp = [1.0 / rw as f32, 1.0 / rh as f32];
        // Composition at render size: upsample by depth and normal, occlusion and bounce, curves.
        Self::trans(cl, &mut self.ao_col, uavst);
        let (col, cr) = (r(&self.ao_col), r(&self.ao_curve));
        let (a, f) = with7!(sv(&[nd.as_ref(), Some(color), out.as_ref(), Some(depth), if cl8 { cr.as_ref() } else { None }]), sf(&[ndf, cf, F16, df, if cl8 { DXGI_FORMAT_R32_FLOAT } else { X }]));
        let p = self.p.ao_compose.clone().unwrap();
        self.run(cl, &p, &ka, a, f, uv(&[col.as_ref()]), uf(&[F16]), rw.div_ceil(8), rh.div_ceil(8));
        Self::trans(cl, &mut self.ao_col, srvst);
        Some((col?, F16))
    }
}

pub fn frame_from_ffx(d: &ffx::DispatchUpscale, ci: &CreateInfo) -> Option<Frame> {
    unsafe {
        let res = |p: *mut c_void| -> Option<ID3D12Resource> { if p.is_null() { None } else { ID3D12Resource::from_raw_borrowed(&p).cloned() } };
        let cl = if d.command_list.is_null() { None } else { ID3D12GraphicsCommandList::from_raw_borrowed(&d.command_list).cloned() }?;
        let color = res(d.color.resource)?; let mv = res(d.motion_vectors.resource)?; let output = res(d.output.resource)?;
        let depth = res(d.depth.resource);
        depth.as_ref()?;
        let (cd, od) = (color.GetDesc(), output.GetDesc());
        let rw = if d.render_size.width != 0 { d.render_size.width } else { cd.Width as u32 };
        let rh = if d.render_size.height != 0 { d.render_size.height } else { cd.Height };
        let (mut ow, mut oh) = (if d.upscale_size.width != 0 { d.upscale_size.width } else { ci.max_upscale[0] }, if d.upscale_size.height != 0 { d.upscale_size.height } else { ci.max_upscale[1] });
        if ow == 0 || oh == 0 { ow = od.Width as u32; oh = od.Height; }
        let display_mv = ci.flags & ffx::ENABLE_DISPLAY_RES_MV != 0;
        let mv_res = if display_mv { [ow as f32, oh as f32] } else { [rw as f32, rh as f32] };
        Some(Frame {
            cl, color, depth, mv, output, exposure: res(d.exposure.resource), reactive: res(d.reactive.resource),
            color_state: d3d_state(d.color.state), depth_state: d3d_state(d.depth.state), mv_state: d3d_state(d.motion_vectors.state),
            exposure_state: d3d_state(d.exposure.state), reactive_state: d3d_state(d.reactive.state), output_state: d3d_state(d.output.state),
            n: FrameNums { rw, rh, ow, oh, jit: [d.jitter.x, d.jitter.y], mv_to_uv: [d.mv_scale.x / mv_res[0], d.mv_scale.y / mv_res[1]], mv_res,
                jittered_mv: ci.flags & ffx::ENABLE_MV_JITTER_CANCELLATION != 0, depth_inverted: ci.flags & ffx::ENABLE_DEPTH_INVERTED != 0,
                hdr: ci.flags & ffx::ENABLE_HDR != 0, exposure_mul: 1.0, pre_exposure: if d.pre_exposure > 0.0 { d.pre_exposure } else { 1.0 }, reactive_mul: 1.0 },
            reset: d.reset, fov_y: d.camera_fov_y, z_near: d.camera_near, z_far: d.camera_far, meters_per_unit: d.view_to_meters,
        })
    }
}

pub fn ratio_from_quality(q: u32, r: &[f32; 4]) -> f32 {
    let pick = |i: usize, d: f32| if (1.0..=4.0).contains(&r[i]) { r[i] } else { d };
    match q { ffx::QUALITY_NATIVEAA => 1.0, ffx::QUALITY_QUALITY => pick(0, 1.5), ffx::QUALITY_BALANCED => pick(1, 1.7),
              ffx::QUALITY_PERFORMANCE => pick(2, 2.0), ffx::QUALITY_ULTRA_PERFORMANCE => pick(3, 3.0), _ => 0.0 }
}
pub fn jitter_phase_count(rw: u32, dw: u32) -> i32 { let r = if rw != 0 { dw as f32 / rw as f32 } else { 1.0 }; (8.0 * r * r) as i32 }
fn halton(mut i: i32, b: i32) -> f32 { let (mut f, mut r) = (1.0f32, 0.0f32); while i > 0 { f /= b as f32; r += f * (i % b) as f32; i /= b; } r }
pub fn jitter_offset(index: i32, phases: i32) -> (f32, f32) {
    let n = phases.max(1);
    let i = (index % n + n) % n + 1;
    (halton(i, 2) - 0.5, halton(i, 3) - 0.5)
}
