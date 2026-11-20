//! Scaling pipeline (child module of engine): single-pass pixel-shader scalers, the two-stage scalers
//! (NVIDIA Image Scaling and Anime4K produce up to 2x, Lanczos finishes the size), and the final
//! RCAS or CAS sharpening pass.
use std::collections::HashMap;
use std::mem::size_of;

use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{fit, Cb, Gpu, Tex};
use crate::a4k_passes;
use crate::config::Settings;

use super::blobs;

pub const NEAREST: i32 = 1; pub const INTEGER: i32 = 2; pub const BICUBIC: i32 = 5; pub const LANCZOS: i32 = 6;
pub const FSR: i32 = 7; pub const LINE: i32 = 8; pub const NIS: i32 = 9; pub const SGSR: i32 = 10; pub const XBR: i32 = 11; pub const ANIME4K: i32 = 12;

fn pixel_shader(s: &Settings) -> &'static str {
    match s.scaler {
        0 | NEAREST | INTEGER => "PS_Nearest", 3 => "PS_Bilinear", 4 => "PS_SharpBilinear", BICUBIC => "PS_Bicubic", LANCZOS => "PS_Lanczos",
        FSR => if s.fsr_fast { "PS_EasuFast" } else { "PS_Easu" }, SGSR => "PS_Sgsr", XBR => "PS_Xbr", LINE => "PS_Line",
        _ => "PS_Lanczos",
    }
}
/// FSR always ends with its sharpening pass; the other smooth scalers only when sharpness is above zero.
fn wants_sharpen(s: &Settings) -> bool {
    s.scaler == FSR || (matches!(s.scaler, BICUBIC | LANCZOS | SGSR | ANIME4K) && s.sharpness > 0.0)
}

// ---------------------------------------------------------------- NVIDIA Image Scaling
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct NisCb {
    detect_ratio: f32, detect_thres: f32, min_contrast_ratio: f32, ratio_norm: f32,
    contrast_boost: f32, eps: f32, sharp_start_y: f32, sharp_scale_y: f32,
    sharp_strength_min: f32, sharp_strength_scale: f32, sharp_limit_min: f32, sharp_limit_scale: f32,
    scale_x: f32, scale_y: f32, dst_norm_x: f32, dst_norm_y: f32, src_norm_x: f32, src_norm_y: f32,
    in_ox: u32, in_oy: u32, in_w: u32, in_h: u32, out_ox: u32, out_oy: u32, out_w: u32, out_h: u32,
    reserved: [f32; 2],
}
/// Port of NVScalerUpdateConfig (NIS_Config.h, MIT). `hdr` selects the linear-HDR tuning.
fn nis_config(sharpness: f32, src: (u32, u32), dst: (u32, u32), hdr: bool) -> NisCb {
    let sl = sharpness.clamp(0.0, 1.0) - 0.5;
    let (max_s, min_s, lim_s) = if sl >= 0.0 { (1.25, 1.25, 1.25) } else { (1.75, 1.0, 1.0) };
    let (mut thres, mut min_cr, mut max_cr, mut start_y, mut end_y) = (64.0 / 1024.0, 2.0f32, 10.0f32, 0.45f32, 0.9f32);
    let (mut s_min, mut s_max, mut l_min, mut l_max) = ((0.4 + sl * min_s * 1.2f32).max(0.0), 1.6 + sl * max_s * 1.8, (0.14 + sl * lim_s * 0.32f32).max(0.1), 0.5 + sl * lim_s * 0.6);
    if hdr {
        thres = 32.0 / 1024.0; min_cr = 1.5; max_cr = 5.0; start_y = 0.3; end_y = 0.5;
        s_min = (0.4 + sl * min_s * 1.1f32).max(0.0); s_max = 2.2 + sl * max_s * 1.8; l_min = (0.10 + sl * lim_s * 0.28f32).max(0.06); l_max = 0.6 + sl * lim_s * 0.6;
    }
    NisCb {
        detect_ratio: 2.0 * 1127.0 / 1024.0, detect_thres: thres, min_contrast_ratio: min_cr, ratio_norm: 1.0 / (max_cr - min_cr),
        contrast_boost: 1.0, eps: 1.0 / 255.0, sharp_start_y: start_y, sharp_scale_y: 1.0 / (end_y - start_y),
        sharp_strength_min: s_min, sharp_strength_scale: s_max - s_min, sharp_limit_min: l_min, sharp_limit_scale: l_max - l_min,
        scale_x: src.0 as f32 / dst.0 as f32, scale_y: src.1 as f32 / dst.1 as f32,
        dst_norm_x: 1.0 / dst.0 as f32, dst_norm_y: 1.0 / dst.1 as f32, src_norm_x: 1.0 / src.0 as f32, src_norm_y: 1.0 / src.1 as f32,
        in_ox: 0, in_oy: 0, in_w: src.0, in_h: src.1, out_ox: 0, out_oy: 0, out_w: dst.0, out_h: dst.1, reserved: [0.0; 2],
    }
}
/// The 64-phase x 8-tap coefficient tables, read from the vendored NIS_Config.h so they stay byte-identical to NVIDIA's.
fn nis_table(name: &str) -> Vec<f32> {
    let src = include_str!("shaders/third_party/nis/NIS_Config.h");
    let start = src.find(&format!("{name}[kPhaseCount][kFilterSize] = {{")).expect("NIS table");
    let body = &src[start..]; let body = &body[body.find('{').unwrap() + 1..body.find("};").unwrap()];
    let v: Vec<f32> = body.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E' || c == '+'))
        .filter(|t| !t.is_empty() && t.chars().any(|c| c.is_ascii_digit())).filter_map(|t| t.trim_end_matches('f').parse().ok()).collect();
    assert_eq!(v.len(), 64 * 8, "NIS table {name}");
    v
}
struct Nis { cs: ID3D11ComputeShader, cb: ID3D11Buffer, coef: [Tex; 2] }
impl Nis {
    fn new(g: &Gpu, hdr: bool) -> Result<Nis> {
        unsafe {
            let mut cs = None; g.dev.CreateComputeShader(if hdr { blobs::NIS_HDR } else { blobs::NIS_SDR }, None, Some(&mut cs))?;
            let mut cb = None;
            g.dev.CreateBuffer(&D3D11_BUFFER_DESC { ByteWidth: size_of::<NisCb>().next_multiple_of(16) as u32, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default() }, None, Some(&mut cb))?;
            let table = |name: &str| -> Result<Tex> {
                let v = nis_table(name);
                let d = D3D11_TEXTURE2D_DESC { Width: 2, Height: 64, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R32G32B32A32_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
                let init = D3D11_SUBRESOURCE_DATA { pSysMem: v.as_ptr() as _, SysMemPitch: 8 * 4, SysMemSlicePitch: 0 };
                let mut t = None; g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; let t: ID3D11Texture2D = t.unwrap();
                let mut srv = None; g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?;
                Ok(Tex { t, srv, uav: None, rtv: None, w: 2, h: 64 })
            };
            Ok(Nis { cs: cs.unwrap(), cb: cb.unwrap(), coef: [table("coef_scale")?, table("coef_usm")?] })
        }
    }
    fn run(&self, g: &Gpu, src: &Option<ID3D11ShaderResourceView>, sw: u32, sh: u32, out: &Tex, sharpness: f32, hdr: bool) {
        let c = nis_config(sharpness, (sw, sh), (out.w, out.h), hdr);
        unsafe {
            let x = &g.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if x.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() { std::ptr::copy_nonoverlapping(&c as *const NisCb as *const u8, m.pData as *mut u8, size_of::<NisCb>()); x.Unmap(&self.cb, 0); }
            x.CSSetShader(&self.cs, None);
            x.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            x.CSSetSamplers(0, Some(&[Some(g.lin.clone())]));
            x.CSSetShaderResources(0, Some(&[src.clone(), self.coef[0].srv.clone(), self.coef[1].srv.clone()]));
            x.CSSetUnorderedAccessViews(0, 1, Some(&out.uav.clone()), None);
            x.Dispatch(out.w.div_ceil(32), out.h.div_ceil(24), 1);
            x.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
            x.CSSetShaderResources(0, Some(&[None, None, None]));
        }
    }
}

// ---------------------------------------------------------------- Anime4K
struct Anime4k { passes: &'static [a4k_passes::Pass], ps: Vec<ID3D11PixelShader>, tex: Vec<Tex> }
impl Anime4k {
    fn new(g: &Gpu, size: i32, w: u32, h: u32) -> Result<Anime4k> {
        let (passes, n) = match size { 0 => (a4k_passes::S, a4k_passes::S_TEXTURES), 1 => (a4k_passes::M, a4k_passes::M_TEXTURES),
            2 => (a4k_passes::L, a4k_passes::L_TEXTURES), 3 => (a4k_passes::VL, a4k_passes::VL_TEXTURES), _ => (a4k_passes::UL, a4k_passes::UL_TEXTURES) };
        let mut ps = vec![];
        for p in passes { let mut s = None; unsafe { g.dev.CreatePixelShader(blobs::anime4k(p.file), None, Some(&mut s))?; } ps.push(s.unwrap()); }
        // Texture 0 is the source frame (not owned); 1..n-1 are the network's feature maps at source size.
        let tex = (1..n).map(|_| g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT, false, true)).collect::<Result<Vec<_>>>()?;
        Ok(Anime4k { passes, ps, tex })
    }
    /// Runs the network: `src` (w x h) -> `out` (2w x 2h).
    fn run(&self, g: &Gpu, src: &Option<ID3D11ShaderResourceView>, out: &Tex) {
        for (p, ps) in self.passes.iter().zip(&self.ps) {
            let srvs: Vec<Option<ID3D11ShaderResourceView>> = p.inputs.iter().map(|&i| if i == 0 { src.clone() } else { self.tex[i as usize - 1].srv.clone() }).collect();
            let target = if p.output == 255 { out } else { &self.tex[p.output as usize - 1] };
            let vp = [0.0, 0.0, target.w as f32, target.h as f32];
            g.draw_ps(ps, &srvs, target.rtv.as_ref().unwrap(), vp, &Cb { dst: vp, ..Default::default() });
        }
    }
}

// ---------------------------------------------------------------- pipeline
/// Per-size resources, rebuilt whenever the captured size, the output size or the scaler changes.
pub struct Scaler {
    mid: Option<Tex>, stage: Option<Tex>, nis: Option<Nis>, a4k: Option<Anime4k>, key: (u32, u32, i32, i32, i32, i32),
    /// Variable rate scaling: per-tile change flags, the persistent scaled image, and whether it holds a complete frame.
    dirty: Option<Tex>, vrs_out: Option<Tex>, vrs_valid: bool,
}
static WARNED: std::sync::Mutex<Option<HashMap<i32, bool>>> = std::sync::Mutex::new(None);

impl Scaler {
    pub fn empty() -> Scaler { Scaler { mid: None, stage: None, nis: None, a4k: None, key: (0, 0, -1, 0, 0, 0), dirty: None, vrs_out: None, vrs_valid: false } }
    pub fn prepare(&mut self, g: &Gpu, s: &Settings, src: (u32, u32), out: (i32, i32)) {
        let key = (src.0, src.1, s.scaler, s.a4k_size, out.0, out.1);
        if key == self.key { return; }
        self.key = key;
        let d = fit(s, src, out);
        let (dw, dh) = (d[2].max(1.0) as u32, d[3].max(1.0) as u32);
        self.mid = g.tex(dw, dh, DXGI_FORMAT_R16G16B16A16_FLOAT, true, true).ok();
        self.nis = None; self.a4k = None; self.stage = None;
        self.dirty = None; self.vrs_out = None; self.vrs_valid = false;
        let warn = |e: &windows::core::Error| { let mut w = WARNED.lock().unwrap(); if w.get_or_insert_with(HashMap::new).insert(s.scaler, true).is_none() { crate::log(&format!("scaler {} unavailable, using Lanczos: {e:?}", s.scaler)); } };
        match s.scaler {
            NIS => match Nis::new(g, s.hdr) {
                Ok(n) => {
                    // NIS upscales 1x..2x per pass; beyond 2x it goes to 2x and Lanczos finishes.
                    if dw > src.0 * 2 || dh > src.1 * 2 { self.stage = g.tex(src.0 * 2, src.1 * 2, DXGI_FORMAT_R16G16B16A16_FLOAT, true, true).ok(); }
                    self.nis = Some(n);
                }
                Err(e) => warn(&e),
            },
            ANIME4K => match Anime4k::new(g, s.a4k_size, src.0, src.1) {
                Ok(a) => { self.a4k = Some(a); self.stage = g.tex(src.0 * 2, src.1 * 2, DXGI_FORMAT_R16G16B16A16_FLOAT, false, true).ok(); }
                Err(e) => warn(&e),
            },
            _ => {}
        }
    }

    /// Like `render`, but with variable rate scaling: only tiles that changed since `prev` are scaled again;
    /// the rest of the persistent image is reused. Multi-pass scalers (NIS, Anime4K) and frame generation
    /// always redraw everything, as their output depends on the whole frame.
    pub fn render_vrs(&mut self, g: &Gpu, s: &Settings, rtv: &ID3D11RenderTargetView, src: (&Option<ID3D11ShaderResourceView>, u32, u32), out: (i32, i32), prev: Option<&Option<ID3D11ShaderResourceView>>) {
        let single_pass = !matches!(s.scaler, NIS | ANIME4K);
        let Some(prev) = prev.filter(|_| s.vrs && single_pass) else { self.vrs_valid = false; return self.render(g, s, rtv, src, out); };
        let d = fit(s, (src.1, src.2), out);
        let (dw, dh) = (d[2].max(1.0) as u32, d[3].max(1.0) as u32);
        let (tw, th) = (src.1.div_ceil(16), src.2.div_ceil(16));
        if self.dirty.as_ref().map(|t| (t.w, t.h) != (tw, th)).unwrap_or(true) { self.dirty = g.tex(tw, th, DXGI_FORMAT_R16_FLOAT, true, false).ok(); self.vrs_valid = false; }
        if self.vrs_out.as_ref().map(|t| (t.w, t.h) != (dw, dh)).unwrap_or(true) { self.vrs_out = g.tex(dw, dh, DXGI_FORMAT_R16G16B16A16_FLOAT, false, true).ok(); self.vrs_valid = false; }
        let (Some(dirty), Some(vo)) = (&self.dirty, &self.vrs_out) else { return self.render(g, s, rtv, src, out); };
        let srcv = [src.1 as f32, src.2 as f32, 1.0 / src.1 as f32, 1.0 / src.2 as f32];
        g.dispatch("CS_Dirty", &[src.0.clone(), prev.clone()], dirty, tw, th, &Cb { src: srcv, p1: [0.0, tw as f32, th as f32, 0.0], ..Default::default() });
        let vp = [0.0, 0.0, dw as f32, dh as f32];
        let p0 = [s.sharpness, 0.0, 0.0, s.hdr as i32 as f32];
        unsafe { g.ctx.PSSetShaderResources(2, Some(&[dirty.srv.clone()])); }
        g.draw(pixel_shader(s), src.0, vo.rtv.as_ref().unwrap(), vp, 0, &Cb { src: srcv, dst: vp, p0, i0: [0, 0, 0, self.vrs_valid as i32], ..Default::default() });
        unsafe { g.ctx.PSSetShaderResources(2, Some(&[None])); g.ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]); }
        self.vrs_valid = true;
        let cb = Cb { src: [dw as f32, dh as f32, 1.0 / dw as f32, 1.0 / dh as f32], dst: d, p0, ..Default::default() };
        let ps = if wants_sharpen(s) { if s.sharpen_cas { "PS_Cas" } else { "PS_Rcas" } } else { "PS_Nearest" };
        g.draw(ps, &vo.srv, rtv, d, 0, &cb);
    }

    /// Draws `src` into the output render target at the fitted rectangle.
    pub fn render(&self, g: &Gpu, s: &Settings, rtv: &ID3D11RenderTargetView, src: (&Option<ID3D11ShaderResourceView>, u32, u32), out: (i32, i32)) {
        let d = fit(s, (src.1, src.2), out);
        unsafe { g.ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]); }
        let hdr = s.hdr as i32 as f32;
        let p0 = [s.sharpness, 0.0, 0.0, hdr];
        let sharpen = wants_sharpen(s) && self.mid.is_some();
        let cb_for = |w: u32, h: u32, dst: [f32; 4]| Cb { src: [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32], dst, p0, ..Default::default() };
        // Stage 1: produce the image at its final size, either in `mid` (when a sharpening pass follows) or straight into the output.
        let (target, td): (&ID3D11RenderTargetView, [f32; 4]) = match (&self.mid, sharpen || self.nis.is_some()) {
            (Some(m), true) => (m.rtv.as_ref().unwrap(), [0.0, 0.0, m.w as f32, m.h as f32]),
            _ => (rtv, d),
        };
        if let (Some(n), Some(m)) = (&self.nis, &self.mid) {
            match &self.stage {
                Some(st) => { n.run(g, src.0, src.1, src.2, st, s.sharpness, s.hdr); g.draw("PS_Lanczos", &st.srv, m.rtv.as_ref().unwrap(), [0.0, 0.0, m.w as f32, m.h as f32], 0, &cb_for(st.w, st.h, [0.0, 0.0, m.w as f32, m.h as f32])); }
                None if m.w >= src.1 && m.h >= src.2 => n.run(g, src.0, src.1, src.2, m, s.sharpness, s.hdr),
                None => g.draw("PS_Lanczos", src.0, m.rtv.as_ref().unwrap(), [0.0, 0.0, m.w as f32, m.h as f32], 0, &cb_for(src.1, src.2, [0.0, 0.0, m.w as f32, m.h as f32])),
            }
            // NIS already sharpens; copy mid to the output.
            g.draw("PS_Nearest", &m.srv, rtv, d, 0, &cb_for(m.w, m.h, d));
            return;
        }
        if let (Some(a), Some(st)) = (&self.a4k, &self.stage) {
            a.run(g, src.0, st);
            let ps = if st.w as f32 == td[2] && st.h as f32 == td[3] { "PS_Nearest" } else { "PS_Lanczos" };
            g.draw(ps, &st.srv, target, td, 0, &cb_for(st.w, st.h, td));
        } else {
            g.draw(pixel_shader(s), src.0, target, td, 0, &cb_for(src.1, src.2, td));
        }
        if sharpen {
            let m = self.mid.as_ref().unwrap();
            g.draw(if s.sharpen_cas { "PS_Cas" } else { "PS_Rcas" }, &m.srv, rtv, d, 0, &cb_for(m.w, m.h, d));
        }
    }
}
