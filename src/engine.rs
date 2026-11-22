use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, ffi::c_void, mem::size_of};

use windows::core::{s, w, Interface, Result, BOOL};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::WinRT::Direct3D11::*;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Settings;
#[path = "scale.rs"] mod scale;
use crate::osd;


#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Cb { src: [f32; 4], dst: [f32; 4], p0: [f32; 4], p1: [f32; 4], i0: [i32; 4], txt: [[i32; 4]; 4] }

struct Tex { t: ID3D11Texture2D, srv: Option<ID3D11ShaderResourceView>, uav: Option<ID3D11UnorderedAccessView>, rtv: Option<ID3D11RenderTargetView>, w: u32, h: u32 }

struct Gpu {
    dev: ID3D11Device, ctx: ID3D11DeviceContext, cb: ID3D11Buffer,
    lin: ID3D11SamplerState, pt: ID3D11SamplerState, blend: ID3D11BlendState, pma: ID3D11BlendState,
    vs: ID3D11VertexShader, ps: HashMap<&'static str, ID3D11PixelShader>, cs: HashMap<&'static str, ID3D11ComputeShader>,
}

/// Shaders are compiled by build.rs (fxc) and embedded; nothing is compiled at runtime.
mod blobs { include!(concat!(env!("OUT_DIR"), "/gpu_blobs.rs")); }
fn compile(entry: &str, _target: &str) -> Result<&'static [u8]> {
    let b = blobs::hlsl(entry);
    if b.is_empty() { return Err(windows::core::Error::new(E_FAIL, format!("shader {entry} missing from the build"))); }
    Ok(b)
}

fn auto_adapter(f: &IDXGIFactory1, mon: Option<HMONITOR>) -> Option<IDXGIAdapter> {
    unsafe {
        if let Some(m) = mon {
            let mut i = 0;
            while let Ok(a) = f.EnumAdapters1(i) {
                let mut j = 0;
                while let Ok(o) = a.EnumOutputs(j) { if o.GetDesc().map(|d| d.Monitor == m).unwrap_or(false) { return a.cast().ok(); } j += 1; }
                i += 1;
            }
        }
        f.cast::<IDXGIFactory6>().ok()?.EnumAdapterByGpuPreference::<IDXGIAdapter>(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE).ok()
    }
}

impl Gpu {
    fn new(adapter_index: i32, mon: Option<HMONITOR>) -> Result<Gpu> {
        unsafe {
            let f: IDXGIFactory1 = CreateDXGIFactory1()?;
            let adapter: Option<IDXGIAdapter> = if adapter_index > 0 {
                f.EnumAdapters1(adapter_index as u32 - 1).ok().and_then(|a| a.cast().ok())
            } else { auto_adapter(&f, mon) };
            let (mut dev, mut ctx) = (None, None);
            let dt = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
            D3D11CreateDevice(adapter.as_ref(), dt, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx))?;
            let dev: ID3D11Device = dev.unwrap(); let ctx = ctx.unwrap();
            let prio: i32 = std::env::var("FS_GPU_PRIO").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
            if let Ok(dx) = dev.cast::<IDXGIDevice>() { let _ = dx.SetGPUThreadPriority(if prio >= 4 { 7 } else { 0 }); }
            if let Ok(g) = LoadLibraryW(w!("gdi32.dll")) {
                if let Some(p) = GetProcAddress(g, s!("D3DKMTSetProcessSchedulingPriorityClass")) {
                    let f: extern "system" fn(HANDLE, i32) -> i32 = std::mem::transmute(p);
                    f(GetCurrentProcess(), prio);
                }
            }
            let mut cb = None;
            dev.CreateBuffer(&D3D11_BUFFER_DESC { ByteWidth: size_of::<Cb>() as u32, Usage: D3D11_USAGE_DYNAMIC, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default() }, None, Some(&mut cb))?;
            let samp = |f: D3D11_FILTER| -> Result<ID3D11SamplerState> {
                let mut s = None;
                dev.CreateSamplerState(&D3D11_SAMPLER_DESC { Filter: f, AddressU: D3D11_TEXTURE_ADDRESS_CLAMP, AddressV: D3D11_TEXTURE_ADDRESS_CLAMP, AddressW: D3D11_TEXTURE_ADDRESS_CLAMP, MaxLOD: f32::MAX, ..Default::default() }, Some(&mut s))?;
                Ok(s.unwrap())
            };
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC { BlendEnable: true.into(), SrcBlend: D3D11_BLEND_SRC_ALPHA, DestBlend: D3D11_BLEND_INV_SRC_ALPHA, BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE, DestBlendAlpha: D3D11_BLEND_ZERO, BlendOpAlpha: D3D11_BLEND_OP_ADD, RenderTargetWriteMask: 0xF };
            let mut blend = None; dev.CreateBlendState(&bd, Some(&mut blend))?;
            bd.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE;
            let mut pma = None; dev.CreateBlendState(&bd, Some(&mut pma))?;
            let mut vs = None; dev.CreateVertexShader(compile("VS", "vs_5_0")?, None, Some(&mut vs))?;
            let mut ps = HashMap::new();
            for e in ["PS_Nearest", "PS_Bilinear", "PS_SharpBilinear", "PS_Bicubic", "PS_Lanczos", "PS_Easu", "PS_Line", "PS_Rcas", "PS_Cursor", "PS_Overlay", "PS_EasuFast", "PS_Sgsr", "PS_Xbr", "PS_Cas", "PS_Rotate"] {
                let mut p = None; dev.CreatePixelShader(compile(e, "ps_5_0")?, None, Some(&mut p))?; ps.insert(e, p.unwrap());
            }
            let mut cs = HashMap::new();
            for e in ["CS_Dirty", "CS_Luma", "CS_Down", "CS_Flow1", "CS_Flow2", "CS_Median", "CS_Interp"] {
                let mut c = None; dev.CreateComputeShader(compile(e, "cs_5_0")?, None, Some(&mut c))?; cs.insert(e, c.unwrap());
            }
            Ok(Gpu { lin: samp(D3D11_FILTER_MIN_MAG_MIP_LINEAR)?, pt: samp(D3D11_FILTER_MIN_MAG_MIP_POINT)?, dev, ctx, cb: cb.unwrap(), blend: blend.unwrap(), pma: pma.unwrap(), vs: vs.unwrap(), ps, cs })
        }
    }

    fn name(&self) -> String {
        unsafe {
            self.dev.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetDesc()).map(|d| {
                let n = d.Description.iter().position(|&c| c == 0).unwrap_or(128); String::from_utf16_lossy(&d.Description[..n])
            }).unwrap_or_default()
        }
    }

    fn tex(&self, w: u32, h: u32, fmt: DXGI_FORMAT, uav: bool, rtv: bool) -> Result<Tex> {
        unsafe {
            let mut bind = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            if uav { bind |= D3D11_BIND_UNORDERED_ACCESS.0 as u32 }
            if rtv { bind |= D3D11_BIND_RENDER_TARGET.0 as u32 }
            let d = D3D11_TEXTURE2D_DESC { Width: w.max(1), Height: h.max(1), MipLevels: 1, ArraySize: 1, Format: fmt, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: bind, ..Default::default() };
            let mut t = None; self.dev.CreateTexture2D(&d, None, Some(&mut t))?; let t: ID3D11Texture2D = t.unwrap();
            let mut srv = None; self.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            let mut u = None; if uav { self.dev.CreateUnorderedAccessView(&t, None, Some(&mut u))?; }
            let mut r = None; if rtv { self.dev.CreateRenderTargetView(&t, None, Some(&mut r))?; }
            Ok(Tex { t, srv, uav: u, rtv: r, w: w.max(1), h: h.max(1) })
        }
    }

    fn set_cb(&self, c: &Cb) {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_ok() {
                std::ptr::copy_nonoverlapping(c as *const Cb as *const u8, m.pData as *mut u8, size_of::<Cb>());
                self.ctx.Unmap(&self.cb, 0);
            }
        }
    }

    fn dispatch(&self, name: &str, srvs: &[Option<ID3D11ShaderResourceView>], uav: &Tex, w: u32, h: u32, c: &Cb) {
        self.set_cb(c);
        unsafe {
            let x = &self.ctx;
            x.CSSetShader(&self.cs[name], None);
            x.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            x.CSSetSamplers(0, Some(&[Some(self.lin.clone()), Some(self.pt.clone())]));
            x.CSSetShaderResources(0, Some(srvs));
            x.CSSetUnorderedAccessViews(0, 1, Some(&uav.uav.clone()), None);
            x.Dispatch(w.div_ceil(8), h.div_ceil(8), 1);
            x.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
            x.CSSetShaderResources(0, Some(&[None, None, None, None]));
        }
    }

    /// Draw with an explicit pixel shader and several inputs (Anime4K passes).
    fn draw_ps(&self, ps: &ID3D11PixelShader, srvs: &[Option<ID3D11ShaderResourceView>], rtv: &ID3D11RenderTargetView, vp: [f32; 4], c: &Cb) {
        self.set_cb(c);
        unsafe {
            let x = &self.ctx;
            x.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            x.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: vp[0], TopLeftY: vp[1], Width: vp[2], Height: vp[3], MinDepth: 0.0, MaxDepth: 1.0 }]));
            x.OMSetBlendState(None, None, 0xffffffff);
            x.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            x.VSSetShader(&self.vs, None);
            x.PSSetShader(ps, None);
            x.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            x.PSSetSamplers(0, Some(&[Some(self.lin.clone()), Some(self.pt.clone())]));
            x.PSSetShaderResources(0, Some(srvs));
            x.Draw(3, 0);
            let none: Vec<Option<ID3D11ShaderResourceView>> = vec![None; srvs.len()];
            x.PSSetShaderResources(0, Some(&none));
            x.OMSetRenderTargets(None, None);
        }
    }

    fn draw(&self, name: &str, srv: &Option<ID3D11ShaderResourceView>, rtv: &ID3D11RenderTargetView, vp: [f32; 4], blend: u8, c: &Cb) {
        self.set_cb(c);
        unsafe {
            let x = &self.ctx;
            x.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            x.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: vp[0], TopLeftY: vp[1], Width: vp[2], Height: vp[3], MinDepth: 0.0, MaxDepth: 1.0 }]));
            x.OMSetBlendState(match blend { 1 => Some(&self.blend), 2 => Some(&self.pma), _ => None }, None, 0xffffffff);
            x.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            x.VSSetShader(&self.vs, None);
            x.PSSetShader(&self.ps[name], None);
            x.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            x.PSSetSamplers(0, Some(&[Some(self.lin.clone()), Some(self.pt.clone())]));
            x.PSSetShaderResources(0, Some(&[srv.clone()]));
            x.Draw(3, 0);
            x.PSSetShaderResources(0, Some(&[None]));
            x.OMSetRenderTargets(None, None);
        }
    }
}

use crate::pacer::{qpc, qpf, FrameStats, Pacer};

enum Capture {
    /// `pending` holds up to `queue` older frames: 0 = always the newest (least latency), 1-2 = smoother under load.
    Wgc { pool: Direct3D11CaptureFramePool, session: GraphicsCaptureSession, size: SizeInt32, fmt: DirectXPixelFormat, rtdev: IDirect3DDevice, ev: HANDLE,
          pending: std::collections::VecDeque<windows::Graphics::Capture::Direct3D11CaptureFrame>, queue: u32 },
    /// `rot` is the display rotation; rotated desktops are first turned upright into `upright`.
    Dd { dup: IDXGIOutputDuplication, origin: (i32, i32), rot: i32, upright: Option<Tex> },
    /// GDI (PrintWindow) for old or unusual windows the other two cannot read; polled at the display's refresh rate.
    Gdi { dc: HDC, bmp: HBITMAP, bits: *mut u8, w: i32, h: i32, period: i64, last: i64 },
}

fn client_rect_screen(h: HWND) -> RECT {
    unsafe {
        let mut r = RECT::default(); let _ = GetClientRect(h, &mut r);
        let mut p = POINT::default(); let _ = ClientToScreen(h, &mut p);
        RECT { left: p.x, top: p.y, right: p.x + r.right, bottom: p.y + r.bottom }
    }
}

impl Capture {
    fn wgc(gpu: &Gpu, game: HWND, hdr: bool, queue: u32) -> Result<Capture> {
        unsafe {
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(game)?;
            let rtdev: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&gpu.dev.cast::<IDXGIDevice>()?)?.cast()?;
            let size = item.Size()?;
            let fmt = if hdr { DirectXPixelFormat::R16G16B16A16Float } else { DirectXPixelFormat::B8G8R8A8UIntNormalized };
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&rtdev, fmt, 2 + queue as i32, size)?;
            let ev = CreateEventW(None, false, false, None)?;
            let evs = ev.0 as isize;
            pool.FrameArrived(&windows::Foundation::TypedEventHandler::new(move |_, _| { let _ = SetEvent(HANDLE(evs as *mut c_void)); Ok(()) }))?;
            let session = pool.CreateCaptureSession(&item)?;
            let _ = session.SetIsCursorCaptureEnabled(false);
            let _ = session.SetIsBorderRequired(false);
            let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 10_000 });
            session.StartCapture()?;
            Ok(Capture::Wgc { pool, session, size, fmt, rtdev, ev, pending: Default::default(), queue: queue.min(2) })
        }
    }
    fn dd(gpu: &Gpu, game: HWND) -> Result<Capture> {
        unsafe {
            let mon = MonitorFromWindow(game, MONITOR_DEFAULTTONEAREST);
            let adapter: IDXGIAdapter = gpu.dev.cast::<IDXGIDevice>()?.GetAdapter()?;
            let mut i = 0;
            while let Ok(o) = adapter.EnumOutputs(i) {
                let d = o.GetDesc()?;
                if d.Monitor == mon {
                    let dup = o.cast::<IDXGIOutput1>()?.DuplicateOutput(&gpu.dev)?;
                    return Ok(Capture::Dd { dup, origin: (d.DesktopCoordinates.left, d.DesktopCoordinates.top), rot: d.Rotation.0, upright: None });
                }
                i += 1;
            }
            Err(windows::core::Error::new(E_FAIL, "monitor not on selected GPU"))
        }
    }

    fn gdi(refresh: f32) -> Result<Capture> {
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() { return Err(windows::core::Error::from_thread()); }
        Ok(Capture::Gdi { dc, bmp: HBITMAP::default(), bits: std::ptr::null_mut(), w: 0, h: 0, period: (crate::pacer::qpf() as f64 / refresh.max(30.0) as f64) as i64, last: 0 })
    }
    fn open(kind: i32, gpu: &Gpu, game: HWND, hdr: bool, queue: u32, refresh: f32) -> Result<Capture> {
        match kind {
            1 => Capture::dd(gpu, game).or_else(|_| Capture::wgc(gpu, game, hdr, queue)),
            2 => Capture::gdi(refresh),
            _ => Capture::wgc(gpu, game, hdr, queue).or_else(|_| Capture::dd(gpu, game)),
        }
    }

    fn grab(&mut self, gpu: &Gpu, game: HWND, crop: [i32; 4], dst: &Tex, freq: f64, skipped: &mut u32) -> Result<Option<i64>> {
        unsafe {
            let cr = client_rect_screen(game);
            match self {
                Capture::Wgc { pool, size, fmt, rtdev, pending, queue, .. } => {
                    // Keep at most `queue` frames waiting behind the one we hand out; anything older is dropped.
                    while let Ok(f) = pool.TryGetNextFrame() {
                        pending.push_back(f);
                        while pending.len() > *queue as usize + 1 { if let Some(old) = pending.pop_front() { let _ = old.Close(); *skipped += 1; } }
                    }
                    let Some(frame) = pending.pop_front() else { return Ok(None) };
                    let cs = frame.ContentSize()?;
                    if cs.Width != size.Width || cs.Height != size.Height {
                        let _ = frame.Close();
                        for f in pending.drain(..) { let _ = f.Close(); }
                        *size = cs; pool.Recreate(&*rtdev, *fmt, 2 + *queue as i32, cs)?; return Ok(None);
                    }
                    let ts = frame.SystemRelativeTime()?.Duration;
                    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                    let tex: ID3D11Texture2D = access.GetInterface()?;
                    let mut wr = RECT::default();
                    let _ = DwmGetWindowAttribute(game, DWMWA_EXTENDED_FRAME_BOUNDS, &mut wr as *mut _ as *mut c_void, size_of::<RECT>() as u32);
                    let (ox, oy) = (cr.left - wr.left + crop[0], cr.top - wr.top + crop[1]);
                    let bx = D3D11_BOX { left: ox.max(0) as u32, top: oy.max(0) as u32, right: (ox.max(0) as u32 + dst.w).min(cs.Width as u32), bottom: (oy.max(0) as u32 + dst.h).min(cs.Height as u32), front: 0, back: 1 };
                    if bx.right > bx.left && bx.bottom > bx.top { gpu.ctx.CopySubresourceRegion(&dst.t, 0, 0, 0, 0, &tex, 0, Some(&bx)); }
                    frame.Close()?;
                    Ok(Some((ts as f64 * freq / 1e7) as i64))
                }
                Capture::Dd { dup, origin, rot, upright } => {
                    let mut info = DXGI_OUTDUPL_FRAME_INFO::default(); let mut res = None;
                    match dup.AcquireNextFrame(2, &mut info, &mut res) {
                        Ok(()) => {}
                        Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                        Err(e) => return Err(e),
                    }
                    let mut out = None;
                    if info.LastPresentTime != 0 {
                        let mut tex: ID3D11Texture2D = res.unwrap().cast()?;
                        let mut td = D3D11_TEXTURE2D_DESC::default(); tex.GetDesc(&mut td);
                        if *rot >= 2 && td.Format == DXGI_FORMAT_B8G8R8A8_UNORM {
                            // Rotated display: the duplicated surface is in panel orientation; turn it upright first.
                            let (uw, uh) = if *rot == 3 { (td.Width, td.Height) } else { (td.Height, td.Width) };
                            if upright.as_ref().map(|u| (u.w, u.h) != (uw, uh)).unwrap_or(true) { *upright = gpu.tex(uw, uh, DXGI_FORMAT_B8G8R8A8_UNORM, false, true).ok(); }
                            if let Some(u) = upright.as_ref() {
                                let mut srv = None; gpu.dev.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                                let vp = [0.0, 0.0, uw as f32, uh as f32];
                                gpu.draw("PS_Rotate", &srv, u.rtv.as_ref().unwrap(), vp, 0, &Cb { dst: vp, i0: [*rot, 0, 0, 0], ..Default::default() });
                                tex = u.t.clone(); td.Width = uw; td.Height = uh;
                            }
                        }
                        let (ox, oy) = (cr.left - origin.0 + crop[0], cr.top - origin.1 + crop[1]);
                        let bx = D3D11_BOX { left: ox.max(0) as u32, top: oy.max(0) as u32, right: (ox.max(0) as u32 + dst.w).min(td.Width), bottom: (oy.max(0) as u32 + dst.h).min(td.Height), front: 0, back: 1 };
                        if bx.right > bx.left && bx.bottom > bx.top && td.Format == DXGI_FORMAT_B8G8R8A8_UNORM { gpu.ctx.CopySubresourceRegion(&dst.t, 0, 0, 0, 0, &tex, 0, Some(&bx)); }
                        out = Some(info.LastPresentTime);
                    }
                    let _ = dup.ReleaseFrame();
                    Ok(out)
                }
                Capture::Gdi { dc, bmp, bits, w, h, period, last } => {
                    let now = crate::pacer::qpc();
                    if *last != 0 && now - *last < *period { return Ok(None); }
                    *last = now;
                    let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);
                    if cw <= 0 || ch <= 0 { return Ok(None); }
                    if (cw, ch) != (*w, *h) {
                        if !bmp.is_invalid() { let _ = DeleteObject((*bmp).into()); }
                        let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: cw, biHeight: -ch, biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
                        let mut p: *mut c_void = std::ptr::null_mut();
                        *bmp = CreateDIBSection(Some(*dc), &bi, DIB_RGB_COLORS, &mut p, None, 0)?;
                        SelectObject(*dc, (*bmp).into());
                        (*bits, *w, *h) = (p as *mut u8, cw, ch);
                    }
                    // PW_CLIENTONLY | PW_RENDERFULLCONTENT: client area, including DirectX content.
                    if !PrintWindow(game, *dc, PRINT_WINDOW_FLAGS(0x1 | 0x2)).as_bool() { return Ok(None); }
                    let _ = GdiFlush();
                    let (ox, oy) = (crop[0].clamp(0, cw - 1), crop[1].clamp(0, ch - 1));
                    let (rw, rh) = ((cw - ox).min(dst.w as i32), (ch - oy).min(dst.h as i32));
                    if rw > 0 && rh > 0 {
                        let bx = D3D11_BOX { left: 0, top: 0, right: rw as u32, bottom: rh as u32, front: 0, back: 1 };
                        let src = bits.add((oy * cw * 4 + ox * 4) as usize);
                        gpu.ctx.UpdateSubresource(&dst.t, 0, Some(&bx), src as *const c_void, (cw * 4) as u32, 0);
                    }
                    Ok(Some(now))
                }
            }
        }
    }
}
impl Capture {
    fn wait(&self, ms: u32) {
        match self {
            Capture::Wgc { ev, .. } => unsafe { WaitForSingleObject(*ev, ms); },
            Capture::Gdi { .. } => std::thread::sleep(std::time::Duration::from_millis(1)),
            Capture::Dd { .. } => {}
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        match self {
            Capture::Wgc { session, pool, ev, pending, .. } => { for f in pending.drain(..) { let _ = f.Close(); } let _ = session.Close(); let _ = pool.Close(); unsafe { let _ = CloseHandle(*ev); } }
            Capture::Gdi { dc, bmp, .. } => unsafe { if !bmp.is_invalid() { let _ = DeleteObject((*bmp).into()); } let _ = DeleteDC(*dc); },
            Capture::Dd { .. } => {}
        }
    }
}

struct FrameGen { dims: Vec<(u32, u32)>, luma: [Vec<Tex>; 2], ff: Vec<Tex>, fb: Vec<Tex>, tmp: Vec<Tex>, out: Tex, scale: f32, cur: usize, frames: usize, warm: bool, probe: (usize, [ID3D11Texture2D; 2]), dedup: bool, pub cut: bool }

impl FrameGen {
    fn new(g: &Gpu, w: u32, h: u32, flow_scale: f32) -> Result<FrameGen> {
        let s = flow_scale.clamp(0.25, 1.0);
        let (mut fw, mut fh) = (((w as f32 * s) as u32).max(64), ((h as f32 * s) as u32).max(64));
        let scale = fw as f32 / w as f32;
        let mut dims = vec![];
        while dims.len() < 7 && fw.min(fh) >= 8 { dims.push((fw, fh)); fw = fw.div_ceil(2); fh = fh.div_ceil(2); }
        let mk = |fmt| -> Result<Vec<Tex>> { dims.iter().map(|&(a, b)| g.tex(a, b, fmt, true, false)).collect() };
        let pl = dims.iter().position(|d| d.0 <= 160).unwrap_or(dims.len() - 1);
        let stage = || -> Result<ID3D11Texture2D> {
            let d = D3D11_TEXTURE2D_DESC { Width: dims[pl].0, Height: dims[pl].1, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
            let mut t = None; unsafe { g.dev.CreateTexture2D(&d, None, Some(&mut t))?; } Ok(t.unwrap())
        };
        let probe = (pl, [stage()?, stage()?]);
        Ok(FrameGen { probe, dedup: true, cut: false, luma: [mk(DXGI_FORMAT_R16_FLOAT)?, mk(DXGI_FORMAT_R16_FLOAT)?], ff: mk(DXGI_FORMAT_R16G16_FLOAT)?, fb: mk(DXGI_FORMAT_R16G16_FLOAT)?, tmp: mk(DXGI_FORMAT_R16G16_FLOAT)?,
            out: g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT, true, false)?, dims, scale, cur: 0, frames: 0, warm: false })
    }

    fn pyramid(&self, g: &Gpu, frame: &Tex, hdr: bool, slot: usize) {
        let mut c = Cb { p0: [0.0, 0.0, 0.0, if hdr { 1.0 } else { 0.0 }], ..Default::default() };
        let (w0, h0) = self.dims[0];
        c.p1 = [self.scale, w0 as f32, h0 as f32, 0.0];
        g.dispatch("CS_Luma", &[frame.srv.clone()], &self.luma[slot][0], w0, h0, &c);
        for l in 1..self.dims.len() {
            let (w, h) = self.dims[l]; c.p1 = [self.scale, w as f32, h as f32, 0.0];
            g.dispatch("CS_Down", &[self.luma[slot][l - 1].srv.clone()], &self.luma[slot][l], w, h, &c);
        }
    }

    /// Compares the new frame's coarse luma with the last one: (same frame, scene cut). A cut (most of the image
    /// changed by a lot, as FidelityFX optical flow's scene change detection) is never interpolated across.
    fn compare(&self, g: &Gpu, slot: usize) -> (bool, bool) {
        if self.frames == 0 { return (false, false); }
        let (l, st) = (&self.probe.0, &self.probe.1);
        let (w, h) = self.dims[*l];
        unsafe {
            g.ctx.CopyResource(&st[0], &self.luma[slot][*l].t);
            g.ctx.CopyResource(&st[1], &self.luma[slot ^ 1][*l].t);
            let (mut a, mut b) = (D3D11_MAPPED_SUBRESOURCE::default(), D3D11_MAPPED_SUBRESOURCE::default());
            if g.ctx.Map(&st[0], 0, D3D11_MAP_READ, 0, Some(&mut a)).is_err() { return (false, false); }
            if g.ctx.Map(&st[1], 0, D3D11_MAP_READ, 0, Some(&mut b)).is_err() { g.ctx.Unmap(&st[0], 0); return (false, false); }
            let (mut maxd, mut big, mut sum) = (0f32, 0u32, 0f32);
            for y in 0..h as usize {
                let (ra, rb) = ((a.pData as *const u8).add(y * a.RowPitch as usize) as *const u16, (b.pData as *const u8).add(y * b.RowPitch as usize) as *const u16);
                for x in 0..w as usize { let d = (h2f(*ra.add(x)) - h2f(*rb.add(x))).abs(); maxd = maxd.max(d); sum += d; if d > 0.15 { big += 1; } }
            }
            g.ctx.Unmap(&st[0], 0); g.ctx.Unmap(&st[1], 0);
            let n = (w * h) as f32;
            (self.dedup && maxd <= 0.002, big as f32 > 0.6 * n && sum / n > 0.18)
        }
    }

    fn ingest(&mut self, g: &Gpu, frame: &Tex, quality: i32, hdr: bool) -> bool {
        let slot = self.cur ^ 1;
        self.pyramid(g, frame, hdr, slot);
        let (same, cut) = self.compare(g, slot);
        if same { return false; }
        self.cut = cut;
        if cut { crate::log("frame gen: scene cut, not interpolated"); }
        self.cur = slot; self.frames += 1;
        if self.frames < 2 { return true; }
        let mut c = Cb::default();
        let (prev, cur) = (1 - self.cur, self.cur);
        let n = self.dims.len();
        for dir in 0..2 {
            let (a, b) = if dir == 0 { (prev, cur) } else { (cur, prev) };
            let out = if dir == 0 { &self.ff } else { &self.fb };
            for l in (0..n).rev() {
                let (w, h) = self.dims[l];
                let coarsest = l == n - 1;
                let fine = l == 0;
                let patch = if fine || quality == 0 { 1 } else { 2 };
                let radius = if coarsest { 4 } else if fine || quality == 0 { 1 } else { 2 };
                let prop = if fine { if quality == 2 { 2 } else { 1 } } else if quality == 0 { 1 } else { 2 };
                let mode = if coarsest { if self.warm { 2 } else { 0 } } else { 1 };
                let coarse = if coarsest { out[l].srv.clone() } else { out[l + 1].srv.clone() };
                c.p1 = [self.scale, w as f32, h as f32, if coarsest { 0.005 } else { 0.002 }];
                c.i0 = [mode, radius, patch, prop];
                g.dispatch(if patch == 1 { "CS_Flow1" } else { "CS_Flow2" }, &[self.luma[a][l].srv.clone(), self.luma[b][l].srv.clone(), coarse], &self.tmp[l], w, h, &c);
                g.dispatch("CS_Median", &[None, None, self.tmp[l].srv.clone()], &out[l], w, h, &c);
                if quality >= 1 && !fine && (quality == 2 || l <= 2) {
                    c.i0 = [2, 1, patch, 1];
                    g.dispatch(if patch == 1 { "CS_Flow1" } else { "CS_Flow2" }, &[self.luma[a][l].srv.clone(), self.luma[b][l].srv.clone(), out[l].srv.clone()], &self.tmp[l], w, h, &c);
                    g.dispatch("CS_Median", &[None, None, self.tmp[l].srv.clone()], &out[l], w, h, &c);
                }
            }
        }
        self.warm = true;
        true
    }

    fn interp(&self, g: &Gpu, prev: &Tex, cur: &Tex, t: f32, ui: f32, hdr: bool) {
        let (w0, h0) = self.dims[0];
        let c = Cb { src: [prev.w as f32, prev.h as f32, 1.0 / prev.w as f32, 1.0 / prev.h as f32], p0: [0.0, t, ui, if hdr { 1.0 } else { 0.0 }], p1: [self.scale, w0 as f32, h0 as f32, 0.0], ..Default::default() };
        g.dispatch("CS_Interp", &[prev.srv.clone(), cur.srv.clone(), self.ff[0].srv.clone(), self.fb[0].srv.clone()], &self.out, prev.w, prev.h, &c);
    }
}

/// GPU time of the work done for one real frame (frame generation, scaling, overlay), from timestamp queries read
/// a few frames later without stalling.
struct GpuTimer { q: Vec<(ID3D11Query, ID3D11Query, ID3D11Query)>, idx: usize, issued: usize, pub ms: f64 }
impl GpuTimer {
    fn new(g: &Gpu) -> Option<GpuTimer> {
        let mk = |k| -> Option<ID3D11Query> { let mut q = None; unsafe { g.dev.CreateQuery(&D3D11_QUERY_DESC { Query: k, MiscFlags: 0 }, Some(&mut q)).ok()?; } q };
        let q = (0..4).map(|_| Some((mk(D3D11_QUERY_TIMESTAMP_DISJOINT)?, mk(D3D11_QUERY_TIMESTAMP)?, mk(D3D11_QUERY_TIMESTAMP)?))).collect::<Option<Vec<_>>>()?;
        Some(GpuTimer { q, idx: 0, issued: 0, ms: 0.0 })
    }
    fn begin(&mut self, g: &Gpu) {
        self.read(g);
        let (d, b, _) = &self.q[self.idx];
        unsafe { g.ctx.Begin(d); g.ctx.End(b); }
    }
    fn end(&mut self, g: &Gpu) {
        let (d, _, e) = &self.q[self.idx];
        unsafe { g.ctx.End(e); g.ctx.End(d); }
        self.idx = (self.idx + 1) % self.q.len(); self.issued += 1;
    }
    /// The oldest set (3 frames back): its result if ready, never waiting.
    fn read(&mut self, g: &Gpu) {
        if self.issued < self.q.len() { return; }
        let (d, b, e) = &self.q[self.idx];
        unsafe {
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default(); let (mut t0, mut t1) = (0u64, 0u64);
            let flag = D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32;
            if g.ctx.GetData(d, Some(&mut dj as *mut _ as *mut c_void), size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32, flag).is_ok()
                && g.ctx.GetData(b, Some(&mut t0 as *mut _ as *mut c_void), 8, flag).is_ok() && g.ctx.GetData(e, Some(&mut t1 as *mut _ as *mut c_void), 8, flag).is_ok()
                && !dj.Disjoint.as_bool() && dj.Frequency > 0 && t1 > t0 {
                let ms = (t1 - t0) as f64 * 1000.0 / dj.Frequency as f64;
                self.ms = if self.ms == 0.0 { ms } else { self.ms * 0.9 + ms * 0.1 };
            }
        }
    }
}

/// Adaptive frame generation: keeps our GPU time per real frame under a share of the frame's interval by moving the
/// flow resolution between 40 % and the user's setting. Measured in a real game on a GTX 1650: 80 % flow held the
/// game at 41 FPS, 50 % at 58.8 (cap 60) with the same 2x output quality bracket.
struct FgAuto { scale: f32, max: f32, last: i64 }
impl FgAuto {
    // Floor 40 %: lower flow resolutions blur fine motion; the timings under GPU contention read high.
    const STEPS: [f32; 5] = [1.0, 0.8, 0.65, 0.5, 0.4];
    /// The new flow scale when a change is due (`share`: our GPU ms / real frame interval ms).
    fn step(&mut self, share: f64, now: i64, freq: i64) -> Option<f32> {
        if now - self.last < freq * 2 { return None; }
        let i = Self::STEPS.iter().position(|&x| x <= self.scale + 1e-3).unwrap_or(Self::STEPS.len() - 1);
        let next = if share > 0.30 && i + 1 < Self::STEPS.len() { Some(Self::STEPS[i + 1]) }
            else if share < 0.12 && i > 0 && Self::STEPS[i - 1] <= self.max + 1e-3 { Some(Self::STEPS[i - 1]) } else { None };
        if let Some(n) = next { self.scale = n; self.last = now; }
        next
    }
}

struct CursorTex { h: HCURSOR, tex: Tex, hot: (i32, i32) }
fn cursor_texture(g: &Gpu, h: HCURSOR) -> Option<CursorTex> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(HICON(h.0), &mut ii).ok()?;
        let dc = GetDC(None);
        let read = |bm: HBITMAP| -> Option<(i32, i32, Vec<u32>)> {
            let mut bmp = BITMAP::default();
            if GetObjectW(bm.into(), size_of::<BITMAP>() as i32, Some(&mut bmp as *mut _ as *mut c_void)) == 0 { return None; }
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: bmp.bmWidth, biHeight: -bmp.bmHeight, biPlanes: 1, biBitCount: 32, ..Default::default() };
            let mut px = vec![0u32; (bmp.bmWidth * bmp.bmHeight) as usize];
            GetDIBits(dc, bm, 0, bmp.bmHeight as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
            Some((bmp.bmWidth, bmp.bmHeight, px))
        };
        let mask = read(ii.hbmMask);
        let (w, h2, mut px) = if !ii.hbmColor.is_invalid() {
            let (w, h, mut px) = read(ii.hbmColor)?;
            if px.iter().all(|p| p >> 24 == 0) {
                if let Some((_, _, m)) = &mask { for (i, p) in px.iter_mut().enumerate() { *p = if m[i] & 0xFFFFFF != 0 { 0 } else { *p | 0xFF000000 }; } }
            }
            (w, h, px)
        } else {
            let (w, h, m) = mask.clone()?; let hh = h / 2;
            let mut px = vec![0u32; (w * hh) as usize];
            for i in 0..(w * hh) as usize {
                let and = m[i] & 0xFFFFFF != 0; let xor = m[i + (w * hh) as usize] & 0xFFFFFF != 0;
                px[i] = match (and, xor) { (true, false) => 0, (false, false) => 0xFF000000, (false, true) => 0xFFFFFFFF, (true, true) => 0xFF000000 };
            }
            (w, hh, px)
        };
        for p in px.iter_mut() { let a = (*p >> 24) & 0xFF; if a == 0 { *p = 0; } }
        ReleaseDC(None, dc);
        if !ii.hbmColor.is_invalid() { let _ = DeleteObject(ii.hbmColor.into()); }
        let _ = DeleteObject(ii.hbmMask.into());
        let d = D3D11_TEXTURE2D_DESC { Width: w as u32, Height: h2 as u32, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as *const c_void, SysMemPitch: w as u32 * 4, SysMemSlicePitch: 0 };
        let mut t = None; g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t)).ok()?; let t: ID3D11Texture2D = t?;
        let mut srv = None; g.dev.CreateShaderResourceView(&t, None, Some(&mut srv)).ok()?;
        Some(CursorTex { h, tex: Tex { t, srv, uav: None, rtv: None, w: w as u32, h: h2 as u32 }, hot: (ii.xHotspot as i32, ii.yHotspot as i32) })
    }
}

fn speed_file() -> std::path::PathBuf { crate::limiter::exe_dir().join("mouse_speed.restore") }
/// After a crash the cursor could stay hidden or slowed down; undo that on the next start.
pub fn recover_input() {
    show_system_cursor(true);
    if let Some(v) = std::fs::read_to_string(speed_file()).ok().and_then(|t| t.trim().parse::<usize>().ok()) {
        unsafe { let _ = SystemParametersInfoW(SPI_SETMOUSESPEED, 0, Some(v.clamp(1, 20) as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)); }
    }
    let _ = std::fs::remove_file(speed_file());
}
fn show_system_cursor(show: bool) {
    unsafe {
        if let Ok(u) = GetModuleHandleW(w!("user32.dll")) {
            if let Some(p) = GetProcAddress(u, s!("ShowSystemCursor")) {
                let f: extern "system" fn(BOOL) = std::mem::transmute(p);
                f(show.into());
            }
        }
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == WM_NCHITTEST { return LRESULT(-1); }
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn create_overlay(r: RECT) -> Result<HWND> {
    unsafe {
        let hi = GetModuleHandleW(None)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hi.into(), lpszClassName: w!("FlowScaleOverlay"), hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0), ..Default::default() };
        RegisterClassW(&wc);
        let h = CreateWindowExW(WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            w!("FlowScaleOverlay"), w!("FlowScale"), WS_POPUP, r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, Some(hi.into()), None)?;
        SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA)?;
        let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
        let pref: i32 = 1;
        let _ = DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, 4);
        Ok(h)
    }
}

pub struct Tele { pub out: FrameStats, pub real: FrameStats, pub out_recent: Vec<f32>, pub refresh: f32, pub ratio: f32, pub active: bool }
impl Tele { pub fn new() -> Tele { Tele { out: FrameStats::new(4096), real: FrameStats::new(1024), out_recent: vec![], refresh: 60.0, ratio: 1.0, active: false } } }
pub struct Status { pub text: Mutex<String>, pub tele: Mutex<Tele> }

fn refresh_rate(m: HMONITOR) -> f32 {
    unsafe {
        let mut mi = MONITORINFOEXW::default(); mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(m, &mut mi as *mut _ as *mut MONITORINFO).as_bool() { return 60.0; }
        let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        if EnumDisplaySettingsW(windows::core::PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() && dm.dmDisplayFrequency > 1 { dm.dmDisplayFrequency as f32 } else { 60.0 }
    }
}

fn vblank(refresh: f32, freq: i64) -> Option<(i64, i64)> {
    let mut ti = DWM_TIMING_INFO { cbSize: size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut ti).ok()?; }
    let per = ti.qpcRefreshPeriod as i64;
    if per <= 0 || ti.qpcVBlank == 0 { return None; }
    let hz = freq as f64 / per as f64;
    if (hz - refresh as f64).abs() > refresh as f64 * 0.03 { return None; }
    Some((ti.qpcVBlank as i64, per))
}

fn fit(s: &Settings, src: (u32, u32), mon: (i32, i32)) -> [f32; 4] {
    let (sw, sh, mw, mh) = (src.0 as f32, src.1 as f32, mon.0 as f32, mon.1 as f32);
    let auto = (mw / sw).min(mh / sh);
    let (dw, dh) = if s.scaler == 0 { (sw, sh) }
        else if s.scaler == 2 { let k = auto.floor().max(1.0); (sw * k, sh * k) }
        else if s.scale_mode == 1 { let k = s.factor.min(auto); (sw * k, sh * k) }
        else if s.fit == 1 { (mw, mh) }
        else { (sw * auto, sh * auto) };
    let (dw, dh) = (dw.round().min(mw), dh.round().min(mh));
    [((mw - dw) / 2.0).floor(), ((mh - dh) / 2.0).floor(), dw, dh]
}

pub fn run(game: HWND, s: Settings, stop: Arc<AtomicBool>, status: Arc<Status>, ui: HWND) -> Result<()> {
    let r = run_session(game, s, stop, status.clone(), ui);
    // A window that disappears while the session starts is the game closing (or re-creating) it, not a GPU fault.
    if r.is_err() && unsafe { !IsWindow(Some(game)).as_bool() } {
        *status.text.lock().unwrap() = "gone".into();
        unsafe { let _ = PostMessageW(Some(ui), crate::WM_STATUS, WPARAM(0), LPARAM(0)); }
        return Ok(());
    }
    r
}
fn run_session(game: HWND, s: Settings, stop: Arc<AtomicBool>, status: Arc<Status>, ui: HWND) -> Result<()> {
    unsafe { let _ = RoInitialize(RO_INIT_MULTITHREADED); let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST); }
    let set = |t: String| { crate::log(&t); *status.text.lock().unwrap() = t; unsafe { let _ = PostMessageW(Some(ui), crate::WM_STATUS, WPARAM(0), LPARAM(0)); } };
    let mon = unsafe {
        let mut m = MonitorFromWindow(game, MONITOR_DEFAULTTONEAREST);
        if s.display > 0 { if let Some(&x) = crate::monitors().get(s.display as usize - 1) { m = x; } }
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi); (mi.rcMonitor, m)
    };
    let (mon, hmon) = mon;
    let g = Gpu::new(s.gpu, Some(hmon))?;
    crate::log(&format!("gpu {}", g.name()));
    let mut pacer = Pacer::new();
    let refresh = refresh_rate(hmon);
    { let mut t = status.tele.lock().unwrap(); *t = Tele::new(); t.refresh = refresh; t.active = true; }
    // Output rectangle: the whole monitor (classic fullscreen scaling) or exactly the game's client area
    // (windowed: frame generation / sharpening over a normal window, desktop stays visible and usable).
    let mut out_rect = if s.windowed { client_rect_screen(game) } else { mon };
    if out_rect.right - out_rect.left < 16 || out_rect.bottom - out_rect.top < 16 { out_rect = mon; }
    let (mut mw, mut mh) = (out_rect.right - out_rect.left, out_rect.bottom - out_rect.top);
    osd::ENGINE_DRAWING.store(true, Ordering::Relaxed);
    let mut overlay = Overlay::new();
    // Runs on every exit path, including early `?` errors: hand the overlay back to the limiter window,
    // mark telemetry inactive, restore a shrunk game window and destroy our output window.
    let mut guard = Guard { status: status.clone(), game, restore: None, ov: HWND::default() };

    let mut saved_rect = RECT::default();
    unsafe { let _ = GetWindowRect(game, &mut saved_rect); }
    let resized = s.resize && s.scale_mode == 1 && s.factor > 1.0 && unsafe { !IsZoomed(game).as_bool() };
    if resized {
        unsafe {
            let (cw, ch) = ((mw as f32 / s.factor) as i32, (mh as f32 / s.factor) as i32);
            let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
            let st = WINDOW_STYLE(GetWindowLongW(game, GWL_STYLE) as u32); let ex = WINDOW_EX_STYLE(GetWindowLongW(game, GWL_EXSTYLE) as u32);
            let _ = AdjustWindowRectEx(&mut r, st, false, ex);
            let (ww, wh) = (r.right - r.left, r.bottom - r.top);
            let _ = SetWindowPos(game, None, mon.left + (mw - ww) / 2, mon.top + (mh - wh) / 2, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        guard.restore = Some(saved_rect);
    }

    let ov = create_overlay(out_rect)?;
    guard.ov = ov;
    let factory: IDXGIFactory2 = unsafe { g.dev.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()? };
    let tearing = unsafe {
        let mut allow = BOOL(0);
        factory.cast::<IDXGIFactory5>().map(|f| f.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut allow as *mut _ as *mut c_void, 4).is_ok() && allow.as_bool()).unwrap_or(false)
    };
    let bbfmt = if s.hdr { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
    let mut flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
    if tearing { flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32; }
    let sc: IDXGISwapChain2 = unsafe {
        let d = DXGI_SWAP_CHAIN_DESC1 { Width: mw as u32, Height: mh as u32, Format: bbfmt, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 3, SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, AlphaMode: DXGI_ALPHA_MODE_IGNORE, Flags: flags, ..Default::default() };
        factory.CreateSwapChainForHwnd(&g.dev, ov, &d, None, None)?.cast()?
    };
    unsafe {
        let _ = factory.MakeWindowAssociation(ov, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
        let _ = sc.SetMaximumFrameLatency(s.max_latency.clamp(1, 3) as u32);
        if s.hdr { let _ = sc.cast::<IDXGISwapChain3>()?.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709); }
    }
    let waitable = unsafe { sc.GetFrameLatencyWaitableObject() };
    let make_rtv = |sc: &IDXGISwapChain2| -> Result<ID3D11RenderTargetView> { unsafe { let bb: ID3D11Texture2D = sc.GetBuffer(0)?; let mut r = None; g.dev.CreateRenderTargetView(&bb, None, Some(&mut r))?; Ok(r.unwrap()) } };
    let mut rtv_slot = Some(make_rtv(&sc)?);
    // Windowed output is composed by DWM, which already shows frames on vblanks and cannot tear. With 3+ generated
    // frames per real one at high refresh our vblank scheduler (every vblank taken) collapsed in a real game: 7.7 real
    // frames/s against DWM's 28 / 84 shown. There VSync is DWM's; elsewhere the exact scheduler stays.
    // Measured: at 2x the scheduler wins (58.8 real / 117 shown vs 40 / 70); at 3x and above, and in target mode, DWM's.
    let dwm_sync = s.windowed && s.sync >= 2 && s.framegen > 0 && (s.framegen == 2 || s.multiplier >= 3);
    let (interval, pflags) = match s.sync { 0 => (0, if tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) }), 1 => (0, DXGI_PRESENT(0)), _ if dwm_sync => (0, DXGI_PRESENT(0)), 2 => (1, DXGI_PRESENT(0)), _ => (2, DXGI_PRESENT(0)) };

    let mut cap = Capture::open(s.capture, &g, game, s.hdr, s.queue, refresh)?;
    let capfmt = if s.hdr && matches!(cap, Capture::Wgc { .. }) { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };

    let mut old_speed: i32 = 0;
    if s.cursor_speed { unsafe { let _ = SystemParametersInfoW(SPI_GETMOUSESPEED, 0, Some(&mut old_speed as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)); } let _ = std::fs::write(speed_file(), old_speed.to_string()); }
    let mut cursor: Option<CursorTex> = None;
    let mut visible = false; let mut speed_set = false;

    let mut size = (0u32, 0u32);
    let mut frames: Vec<Tex> = vec![]; let mut fi = 0usize; let mut nframes = 0usize;
    let mut fg: Option<FrameGen> = None;
    // Two timers: the flow (once per real frame) and one generated output (interpolation + scaling).
    let (mut t_flow, mut t_out) = (GpuTimer::new(&g), GpuTimer::new(&g));
    let mut fg_auto = FgAuto { scale: s.flow_scale.clamp(0.25, 1.0), max: s.flow_scale.clamp(0.25, 1.0), last: 0 };
    let mut scaler = scale::Scaler::empty();
    let mut last_ts: i64 = 0; let mut deltas: Vec<f64> = vec![]; let mut phase: f32 = 0.0; let mut clock: i64 = 0;
    let mut hist: std::collections::VecDeque<(i64, u64)> = std::collections::VecDeque::new(); let mut frame_idx: u64 = 0;
    let (mut real_n, mut out_n, mut stat_t) = (0u32, 0u32, qpc()); let mut last_present: i64 = 0; let mut fps: (u32, u32);
    let vsync_k: i64 = if dwm_sync { 0 } else { match s.sync { 2 => 1, 3 => 2, _ => 0 } };
    let mut last_v: i64 = 0;
    let (mut dbg_dup, mut dbg_skip) = (0u32, 0u32);
    set(format!("scaling:{}", crate::window_title(game)));

    let mut result = Ok(());
    let mut gone = false;
    let (mut total_real, mut total_out, t_start) = (0u64, 0u64, qpc());
    'main: while !stop.load(Ordering::Relaxed) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { let _ = TranslateMessage(&msg); DispatchMessageW(&msg); }
            if !IsWindow(Some(game)).as_bool() { gone = true; break 'main; }
        }
        let fgw = unsafe { GetForegroundWindow() };
        let want = s.keep_unfocused || fgw == game || fgw == ov;
        let want = want && unsafe { !IsIconic(game).as_bool() };
        if want != visible {
            visible = want;
            unsafe { let _ = ShowWindow(ov, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
            if s.draw_cursor || s.hide_cursor { show_system_cursor(!visible); }
            if s.cursor_speed {
                let v = if visible { ((old_speed as f32) / (mw as f32 / size.0.max(1) as f32).max(1.0)).round().max(1.0) as i32 } else { old_speed };
                unsafe { let _ = SystemParametersInfoW(SPI_SETMOUSESPEED, 0, Some(v as usize as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)); }
                speed_set = visible;
            }
            if !visible { unsafe { let _ = ClipCursor(None); } }
        }
        if !visible { std::thread::sleep(std::time::Duration::from_millis(50)); continue; }

        let cr = client_rect_screen(game);
        if s.windowed && cr != out_rect && cr.right - cr.left >= 16 && cr.bottom - cr.top >= 16 {
            let resized_out = (cr.right - cr.left, cr.bottom - cr.top) != (mw, mh);
            out_rect = cr;
            unsafe { let _ = SetWindowPos(ov, Some(HWND_TOPMOST), cr.left, cr.top, cr.right - cr.left, cr.bottom - cr.top, SWP_NOACTIVATE); }
            if resized_out {
                (mw, mh) = (cr.right - cr.left, cr.bottom - cr.top);
                unsafe { g.ctx.OMSetRenderTargets(None, None); g.ctx.Flush(); }
                drop(rtv_slot.take()); // every back-buffer reference must be gone before ResizeBuffers
                if let Err(e) = unsafe { sc.ResizeBuffers(3, mw as u32, mh as u32, bbfmt, DXGI_SWAP_CHAIN_FLAG(flags as i32)) } { result = Err(e); break 'main; }
                rtv_slot = match make_rtv(&sc) { Ok(r) => Some(r), Err(e) => { result = Err(e); break 'main } };
                size = (0, 0);
            }
        }
        let want_size = (((cr.right - cr.left) - s.crop[0] - s.crop[2]).max(16) as u32, ((cr.bottom - cr.top) - s.crop[1] - s.crop[3]).max(16) as u32);
        if want_size != size {
            size = want_size;
            frames = match (0..2).map(|_| g.tex(size.0, size.1, capfmt, false, false)).collect() { Ok(v) => v, Err(e) => { result = Err(e); break 'main } };
            fg = if s.framegen > 0 { FrameGen::new(&g, size.0, size.1, fg_auto.scale).ok().map(|mut f| { f.dedup = s.dedup; f }) } else { None };
            let d = fit(&s, size, (mw, mh));
            scaler.prepare(&g, &s, size, (mw, mh));
            nframes = 0; phase = 0.0;
            crate::log(&format!("input {}x{} -> dest {:?}", size.0, size.1, d));
        }
        if s.clip_cursor && fgw == game { unsafe { let _ = ClipCursor(Some(&cr)); } }

        let Some(rtv) = rtv_slot.clone() else { break 'main };
        let next = (fi + 1) % 2;
        let mut skipped = 0u32;
        let ts = match cap.grab(&g, game, s.crop, &frames[next], pacer.freq as f64, &mut skipped) {
            Ok(Some(t)) => t,
            Ok(None) => { cap.wait(4); continue; }
            Err(e) => {
                crate::log(&format!("capture error {e:?}, restarting"));
                std::thread::sleep(std::time::Duration::from_millis(100));
                match Capture::open(s.capture, &g, game, s.hdr, s.queue, refresh) { Ok(c) => { cap = c; continue; } Err(e) => { result = Err(e); break 'main } }
            }
        };
        if fg.is_some() { if let Some(t) = t_flow.as_mut() { t.begin(&g); } }
        if let Some(f) = fg.as_mut() {
            let fresh = f.ingest(&g, &frames[next], s.quality, s.hdr);
            if let Some(t) = t_flow.as_mut() { t.end(&g); }
            if !fresh { dbg_dup += 1; continue; }
        }
        let mut timed_out = false;
        fi = next; nframes += 1; real_n += 1; total_real += 1;
        let arrive = qpc();
        let dt_ms = if last_ts != 0 { (pacer.ms(ts - last_ts) / (skipped + 1) as f64).clamp(1.0, 250.0) } else { 16.67 };
        last_ts = ts;
        deltas.push(dt_ms); if deltas.len() > 15 { deltas.remove(0); }
        status.tele.lock().unwrap().real.push(dt_ms as f32);
        frame_idx += (skipped + 1) as u64;
        let mut sorted = deltas.clone(); sorted.sort_by(f64::total_cmp);
        let cut = sorted.len() / 5;
        let kept = &sorted[cut..sorted.len() - cut];
        let short = kept.iter().sum::<f64>() / kept.len() as f64;
        if let Some(&(t0, i0)) = hist.front() {
            let slope = pacer.ms(ts - t0) / (frame_idx - i0).max(1) as f64;
            if (slope - short).abs() > short * 0.35 || dt_ms > short * 3.0 { hist.clear(); }
        }
        hist.push_back((ts, frame_idx)); if hist.len() > 32 { hist.pop_front(); }
        let dt = match (hist.front(), hist.len()) {
            (Some(&(t0, i0)), n) if n >= 8 => pacer.ms(ts - t0) / (frame_idx - i0).max(1) as f64,
            _ => short,
        };

        let mut outputs: Vec<f32> = vec![];
        let mut step = 1.0f32;
        if fg.is_some() {
            if nframes >= 2 {
                // Below the minimum game frame rate there is too little motion continuity to interpolate: show real frames only.
                if s.fg_min_fps > 0.0 && 1000.0 / dt < s.fg_min_fps as f64 { step = 1.0; } else {
                step = if s.framegen == 2 { ((1000.0 / s.target.max(1) as f64) / dt) as f32 } else { 1.0 / s.multiplier.max(1) as f32 };
                // Never show more than the display takes: its refresh, half in half-rate mode, and with G-SYNC the VRR
                // ceiling (3 % under refresh) so generated frames stay inside the VRR range. Tearing mode alone is free.
                let vrr = (refresh as f64 * 0.97).floor();
                let max_out = match s.sync { 0 if !s.gsync => f64::INFINITY, 3 => refresh as f64 / 2.0, _ if s.gsync => vrr, _ => refresh as f64 };
                let min_step = ((1000.0 / max_out) / dt) as f32;
                step = step.max(min_step).clamp(0.05, 1.0);
                if dt > 100.0 { step = 1.0; }
                }
            }
        }
        while phase + step <= 1.0 + 1e-4 { phase += step; outputs.push(phase.min(1.0)); }
        phase -= 1.0; if phase < -0.5 { phase = 0.0; }

        let dt_ticks = pacer.ticks(dt);
        let pred = if clock == 0 { arrive } else { clock + dt_ticks };
        let err = arrive - pred;
        clock = if clock == 0 || err.abs() > dt_ticks / 2 { arrive } else { pred + err / 5 };
        let base = clock;
        let nout = outputs.len();
        for (k, &t) in outputs.iter().enumerate() {
            if stop.load(Ordering::Relaxed) { break; }
            let due = base + pacer.ticks(dt * (t - step).max(0.0) as f64);
            if k + 1 < nout && qpc() > due + pacer.ticks(dt * step as f64 * 0.5) { dbg_skip += 1; continue; }
            // A scene cut keeps the output cadence but shows the new frame itself: nothing to blend between.
            let src: (&Option<ID3D11ShaderResourceView>, u32, u32) = if t >= 0.95 || nframes < 2 || fg.as_ref().is_none_or(|f| f.cut) {
                (&frames[fi].srv, size.0, size.1)
            } else if t <= 0.05 {
                (&frames[1 - fi].srv, size.0, size.1)
            } else {
                let f = fg.as_ref().unwrap();
                if !timed_out { if let Some(tm) = t_out.as_mut() { tm.begin(&g); } }
                f.interp(&g, &frames[1 - fi], &frames[fi], t, s.ui_threshold, s.hdr);
                (&f.out.srv, size.0, size.1)
            };
            unsafe { WaitForSingleObjectEx(waitable, 100, true); }
            // Variable rate scaling compares against the previous captured frame (real frames only).
            let prev = if fg.is_none() && nframes >= 2 { Some(&frames[1 - fi].srv) } else { None };
            scaler.render_vrs(&g, &s, &rtv, src, (mw, mh), prev);
            if fg.is_some() && t > 0.05 && t < 0.95 && !timed_out { timed_out = true; if let Some(tm) = t_out.as_mut() { tm.end(&g); } }
            if s.draw_cursor && !s.hide_cursor {
                let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
                if unsafe { GetCursorInfo(&mut ci) }.is_ok() && ci.flags.0 & 1 != 0 && ci.ptScreenPos.x >= cr.left && ci.ptScreenPos.x < cr.right && ci.ptScreenPos.y >= cr.top && ci.ptScreenPos.y < cr.bottom {
                    if cursor.as_ref().map(|c| c.h != ci.hCursor).unwrap_or(true) { cursor = cursor_texture(&g, ci.hCursor); }
                    if let Some(c) = &cursor {
                        let d = fit(&s, size, (mw, mh));
                        let k = d[2] / size.0 as f32;
                        let cs = if s.scale_cursor { k } else { 1.0 };
                        let x = d[0] + (ci.ptScreenPos.x - cr.left - s.crop[0]) as f32 * k - c.hot.0 as f32 * cs;
                        let y = d[1] + (ci.ptScreenPos.y - cr.top - s.crop[1]) as f32 * k - c.hot.1 as f32 * cs;
                        let vp = [x, y, c.tex.w as f32 * cs, c.tex.h as f32 * cs];
                        g.draw("PS_Cursor", &c.tex.srv, &rtv, vp, 1, &Cb { dst: vp, p0: [0.0, 0.0, 0.0, s.hdr as i32 as f32], ..Default::default() });
                    }
                }
            }
            let area = if s.windowed { [0.0, 0.0, mw as f32, mh as f32] } else { fit(&s, size, (mw, mh)) };
            overlay.draw(&g, &rtv, area, s.hdr, (mh as f32 / 1080.0).clamp(0.6, 2.5), qpc(), pacer.freq);
            let mut at = due;
            if vsync_k > 0 {
                if let Some((vb, per)) = vblank(refresh, pacer.freq) {
                    let lead = pacer.ticks(1.5).min(per / 3);
                    let mut v = vb + (due + lead - vb).div_euclid(per) * per + per;
                    if v - per >= due + lead { v -= per; }
                    if last_v != 0 && v < last_v + per * vsync_k && v > last_v - per * 4 { v = last_v + per * vsync_k; }
                    if k + 1 < nout && qpc() > v - lead / 3 { dbg_skip += 1; continue; }
                    last_v = v; at = v - lead;
                }
            }
            if qpc() < at { pacer.wait_until(at); }
            let hr = unsafe { sc.Present(interval, pflags) };
            if hr.is_err() { crate::log(&format!("present {hr:?}")); }
            out_n += 1; total_out += 1;
            let now = qpc();
            if last_present != 0 { let ms = pacer.ms(now - last_present) as f32; let mut tl = status.tele.lock().unwrap(); tl.out.push(ms); if tl.out_recent.len() < 4096 { tl.out_recent.push(ms); } }
            last_present = now;
        }
        if let (Some(tf), Some(to)) = (t_flow.as_mut(), t_out.as_mut()) {
            let ngen = outputs.len().saturating_sub(1) as f64;
            let ms = tf.ms + to.ms * ngen;
            if s.fg_auto && fg.is_some() && tf.ms > 0.0 && to.ms > 0.0 && dt > 1.0 {
                if let Some(n) = fg_auto.step(ms / dt, qpc(), pacer.freq) {
                    crate::log(&format!("frame gen: GPU {:.2} ms (flow {:.2} + {ngen:.0} x {:.2}) of a {:.2} ms frame, flow resolution now {:.0}%", ms, tf.ms, to.ms, dt, n * 100.0));
                    fg = FrameGen::new(&g, size.0, size.1, n).ok().map(|mut f| { f.dedup = s.dedup; f });
                    nframes = 0; phase = 0.0; tf.ms = 0.0; to.ms = 0.0;
                }
            }
        }

        let now = qpc();
        if pacer.ms(now - stat_t) >= 500.0 {
            let sec = pacer.ms(now - stat_t) / 1000.0;
            fps = ((real_n as f64 / sec).round() as u32, (out_n as f64 / sec).round() as u32);
            {
                let tl = status.tele.lock().unwrap();
                let g = tl.out.recent(240);
                let mean = { let l = &g[g.len().saturating_sub(60)..]; if l.is_empty() { 0.0 } else { l.iter().sum::<f32>() / l.len() as f32 } };
                let mut srt = g.clone(); srt.sort_by(|a, b| b.total_cmp(a));
                let k = (srt.len() / 100).max(1); let low = if srt.is_empty() { 0.0 } else { srt[..k.min(srt.len())].iter().sum::<f32>() / k as f32 };
                overlay.data = osd::OsdData { fps: fps.1 as f32, game_fps: if fg.is_some() { fps.0 as f32 } else { 0.0 }, frame_ms: mean, low1: if low > 0.0 { 1000.0 / low } else { 0.0 }, cap: 0.0,
                    tag: if s.windowed { String::new() } else { format!("{}×{}", size.0, size.1) }, graph: g[g.len().saturating_sub(150)..].to_vec() };
            }
            if cfg!(debug_assertions) { { let v = status.tele.lock().unwrap().out.recent(out_n as usize); let m = v.iter().sum::<f32>() / v.len().max(1) as f32; let sd = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len().max(1) as f32).sqrt(); let mx = v.iter().cloned().fold(0.0f32, f32::max); crate::log(&format!("fps real {} out {} (dt {:.2} ms, step {:.3}) window sd {:.2} ms max {:.2} ms | dup {} skip {}", fps.0, fps.1, dt, step, sd, mx, dbg_dup, dbg_skip)); dbg_skip = 0; dbg_dup = 0; } }
            { let mut tl = status.tele.lock().unwrap(); tl.ratio = if real_n > 0 { out_n as f32 / real_n as f32 } else { 1.0 }; }
            real_n = 0; out_n = 0; stat_t = now;
        }
    }

    crate::log(&format!("session end: {}", if gone { "window closed" } else if result.is_err() { "error" } else { "stopped" }));
    // Release the capture on its own thread: closing a Graphics Capture session whose window was just destroyed
    // can stall inside Windows, and that must never keep the session (or the app's stop/exit) waiting.
    struct SendCap(#[allow(dead_code)] Capture);
    unsafe impl Send for SendCap {}
    let held = SendCap(cap);
    std::thread::spawn(move || { let c = held; drop(c); crate::log("session end: capture released"); });
    unsafe {
        if s.draw_cursor || s.hide_cursor { show_system_cursor(true); }
        let _ = ClipCursor(None);
        if s.cursor_speed && speed_set { let _ = SystemParametersInfoW(SPI_SETMOUSESPEED, 0, Some(old_speed as usize as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)); }
        if s.cursor_speed { let _ = std::fs::remove_file(speed_file()); }
    }
    drop(guard);
    let secs = (qpc() - t_start) as f64 / qpf() as f64;
    let pacing = {
        let mut v = status.tele.lock().map(|t| t.out_recent.clone()).unwrap_or_default();
        if v.len() > 20 { v.drain(..10); }
        if v.is_empty() { String::new() } else {
            let m = v.iter().sum::<f32>() / v.len() as f32;
            let sd = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt();
            v.sort_by(f32::total_cmp);
            let q = |p: f32| v[((v.len() - 1) as f32 * p) as usize];
            format!(", output frame time p50 {:.2} / p99 {:.2} / max {:.2} ms, σ {:.2} ms", q(0.5), q(0.99), v[v.len() - 1], sd)
        }
    };
    crate::log(&format!("session: {:.1} s, {} captured ({:.1}/s), {} presented ({:.1}/s){pacing}, scaler {}, capture {}, queue {}, vrs {}, framegen {}", secs, total_real, total_real as f64 / secs.max(0.001), total_out, total_out as f64 / secs.max(0.001), s.scaler, s.capture, s.queue, s.vrs, s.framegen));
    // "gone": the game closed its window; the app may follow a re-created one.
    set(match &result { Ok(()) if gone => "gone".into(), Ok(()) => "stopped".into(), Err(e) => format!("error:{}", e.message()) });
    result
}


struct Guard { status: Arc<Status>, game: HWND, restore: Option<RECT>, ov: HWND }
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            if let Some(r) = self.restore { if IsWindow(Some(self.game)).as_bool() { let _ = SetWindowPos(self.game, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE); } }
            if !self.ov.is_invalid() { let _ = DestroyWindow(self.ov); }
        }
        osd::ENGINE_DRAWING.store(false, Ordering::Relaxed);
        if let Ok(mut t) = self.status.tele.lock() { t.active = false; }
    }
}

struct Overlay { tex: Option<Tex>, last: i64, data: osd::OsdData }
impl Overlay {
    fn new() -> Overlay { Overlay { tex: None, last: 0, data: osd::OsdData::default() } }
    /// Re-renders the panel at most 10x per second; draws the cached texture every frame.
    fn draw(&mut self, g: &Gpu, rtv: &ID3D11RenderTargetView, area: [f32; 4], hdr: bool, base: f32, now: i64, freq: i64) {
        let Some(style) = osd::engine_style() else { return };
        if !osd::VISIBLE.load(Ordering::Relaxed) { return; }
        if now - self.last > freq / 10 || self.tex.is_none() {
            self.last = now;
            if let Some(c) = osd::render(&self.data, &style, base) {
                if self.tex.as_ref().map(|t| (t.w, t.h) != (c.w, c.h)).unwrap_or(true) { self.tex = g.tex(c.w, c.h, DXGI_FORMAT_B8G8R8A8_UNORM, false, false).ok(); }
                if let Some(t) = &self.tex { unsafe { g.ctx.UpdateSubresource(&t.t, 0, None, c.px.as_ptr() as *const c_void, c.w * 4, 0); } }
            }
        }
        let Some(t) = &self.tex else { return };
        let (x, y) = osd::place(style.pos, area, (t.w, t.h), (12.0 * base).round(), (style.offset.0 * base, style.offset.1 * base));
        let vp = [x.round(), y.round(), t.w as f32, t.h as f32];
        g.draw("PS_Overlay", &t.srv, rtv, vp, 2, &Cb { dst: vp, p0: [0.0, 0.0, 0.0, hdr as i32 as f32], ..Default::default() });
    }
}

pub fn selftest() -> Result<String> {
    let g = Gpu::new(0, None)?;
    let (w, h) = (640u32, 360u32);
    let mk = |shift: u32| -> Result<Tex> {
        let px: Vec<u32> = (0..w * h).map(|i| { let (x, y) = (i % w, i / w); let n = vnoise((x + shift) as f32 / 7.0, y as f32 / 7.0) * 0.6 + vnoise((x + shift) as f32 / 2.5, y as f32 / 2.5) * 0.4; let v = (n * 220.0 + 20.0) as u32; 0xFF000000 | (v << 16) | ((x * 255 / w) << 8) | (y * 255 / h) }).collect();
        let d = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut t = None; unsafe { g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; }
        let t: ID3D11Texture2D = t.unwrap(); let mut srv = None; unsafe { g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?; }
        Ok(Tex { t, srv, uav: None, rtv: None, w, h })
    };
    let (a, b) = (mk(0)?, mk(6)?);
    let mut fg = FrameGen::new(&g, w, h, 0.5)?;
    let f = qpf() as f64; let t0 = qpc();
    for q in 0..3 { fg.ingest(&g, &a, q, false); fg.ingest(&g, &b, q, false); }
    fg.interp(&g, &a, &b, 0.5, 0.5, false);
    let read = |src: &ID3D11Texture2D, fmt: DXGI_FORMAT, tw: u32, th: u32, bpp: usize| -> Result<Vec<u8>> {
        let d = D3D11_TEXTURE2D_DESC { Width: tw, Height: th, MipLevels: 1, ArraySize: 1, Format: fmt, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
        let mut st = None; unsafe { g.dev.CreateTexture2D(&d, None, Some(&mut st))?; } let st: ID3D11Texture2D = st.unwrap();
        unsafe { g.ctx.CopyResource(&st, src); let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let mut v = vec![0u8; tw as usize * th as usize * bpp];
            for y in 0..th as usize { std::ptr::copy_nonoverlapping((m.pData as *const u8).add(y * m.RowPitch as usize), v.as_mut_ptr().add(y * tw as usize * bpp), tw as usize * bpp); }
            g.ctx.Unmap(&st, 0); Ok(v) }
    };
    let (fw, fh) = fg.dims[0];
    let flow = read(&fg.ff[0].t, DXGI_FORMAT_R16G16_FLOAT, fw, fh, 4)?;
    let mut sum = 0.0; let mut n = 0.0;
    for y in fh / 4..fh * 3 / 4 { for x in fw / 4..fw * 3 / 4 { let o = ((y * fw + x) * 4) as usize; sum += h2f(u16::from_le_bytes([flow[o], flow[o + 1]])); n += 1.0; } }
    let mean_dx = sum / n / fg.scale;
    let out = read(&fg.out.t, DXGI_FORMAT_R16G16B16A16_FLOAT, w, h, 8)?;
    let nan = out.chunks(2).any(|c| { let v = u16::from_le_bytes([c[0], c[1]]); (v >> 10) & 0x1f == 0x1f });
    let rt = g.tex(1920, 1080, DXGI_FORMAT_R16G16B16A16_FLOAT, false, true)?;
    let mut s = Settings::test();
    // Every scaler, both FSR variants and both sharpeners; Anime4K at every size.
    for sc in 0..=12 { for variant in 0..2 {
        s.scaler = sc; s.fsr_fast = variant == 1; s.sharpen_cas = variant == 1; s.sharpness = 0.5;
        for a4 in 0..(if sc == scale::ANIME4K { 5 } else { 1 }) {
            s.a4k_size = a4;
            let mut sc_res = scale::Scaler::empty();
            sc_res.prepare(&g, &s, (w, h), (1920, 1080));
            sc_res.render(&g, &s, rt.rtv.as_ref().unwrap(), (&a.srv, w, h), (1920, 1080));
            let px = read(&rt.t, DXGI_FORMAT_R16G16B16A16_FLOAT, 1920, 1080, 8)?;
            if px.chunks(2).any(|c| { let v = u16::from_le_bytes([c[0], c[1]]); (v >> 10) & 0x1f == 0x1f }) {
                return Err(windows::core::Error::new(E_FAIL, format!("scaler {sc} (variant {variant}, size {a4}) produced NaN/Inf")));
            }
        }
    } }
    let mut ov = Overlay::new(); ov.data = osd::OsdData { fps: 120.0, game_fps: 60.0, frame_ms: 8.3, low1: 110.0, cap: 60.0, tag: "test".into(), graph: vec![8.3; 120] };
    osd::configure(true, false, crate::config::OsdStyle { detail: 2, pos: 1, light: false, scale: 1.0, opacity: 0.9, color: 0, font: 0, offset: (0.0, 0.0) });
    ov.draw(&g, rt.rtv.as_ref().unwrap(), [0.0, 0.0, 1920.0, 1080.0], false, 1.0, qpc(), qpf());
    if ov.tex.is_none() { return Err(windows::core::Error::new(E_FAIL, "overlay did not render")); }
    let _ = read(&rt.t, DXGI_FORMAT_R16G16B16A16_FLOAT, 1920, 1080, 8)?;
    let rot = rotation_test(&g)?;
    let ms = (qpc() - t0) as f64 * 1000.0 / f;
    let r = format!("ok: shaders compiled, flow mean dx = {mean_dx:.2} px (expect ~ -6), interp NaN = {nan}, rotation {rot}, total {ms:.1} ms");
    if nan || !(-7.5..-4.5).contains(&mean_dx) { return Err(windows::core::Error::new(E_FAIL, r)); }
    Ok(r)
}

/// A rotated display, simulated: a desktop image (every pixel unique) is turned into the panel-orientation surface
/// Desktop Duplication hands over for each DXGI rotation (90: the panel turned clockwise is the desktop), then the
/// capture's PS_Rotate pass must give back the desktop exactly, pixel for pixel.
fn rotation_test(g: &Gpu) -> Result<String> {
    let (w, h) = (96u32, 54u32);
    let desk: Vec<u32> = (0..w * h).map(|i| 0xFF00_0000 | ((i % w) << 8) | (i / w) | (((i * 7) & 0xff) << 16)).collect();
    let mut res = vec![];
    for rot in [2i32, 3, 4] {
        let (pw, ph) = if rot == 3 { (w, h) } else { (h, w) };
        let mut panel = vec![0u32; (pw * ph) as usize];
        for y in 0..h { for x in 0..w {
            let (px, py) = match rot { 2 => (y, ph - 1 - x), 3 => (w - 1 - x, h - 1 - y), _ => (pw - 1 - y, x) };
            panel[(py * pw + px) as usize] = desk[(y * w + x) as usize];
        } }
        let d = D3D11_TEXTURE2D_DESC { Width: pw, Height: ph, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: panel.as_ptr() as _, SysMemPitch: pw * 4, SysMemSlicePitch: 0 };
        let mut t = None; unsafe { g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; }
        let t: ID3D11Texture2D = t.unwrap(); let mut srv = None; unsafe { g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?; }
        let up = g.tex(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, false, true)?;
        let vp = [0.0, 0.0, w as f32, h as f32];
        g.draw("PS_Rotate", &srv, up.rtv.as_ref().unwrap(), vp, 0, &Cb { dst: vp, i0: [rot, 0, 0, 0], ..Default::default() });
        let sd = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, Width: w, Height: h, ..d };
        let mut st = None; unsafe { g.dev.CreateTexture2D(&sd, None, Some(&mut st))?; } let st: ID3D11Texture2D = st.unwrap();
        let mut bad = 0;
        unsafe {
            g.ctx.CopyResource(&st, &up.t);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            for y in 0..h { for x in 0..w { if *((m.pData as *const u8).add((y * m.RowPitch + x * 4) as usize) as *const u32) != desk[(y * w + x) as usize] { bad += 1; } } }
            g.ctx.Unmap(&st, 0);
        }
        if bad > 0 { return Err(windows::core::Error::new(E_FAIL, format!("rotation {}: {bad} of {} pixels wrong", (rot - 1) * 90, w * h))); }
        res.push(format!("{}°", (rot - 1) * 90));
    }
    Ok(format!("{} exact", res.join("/")))
}

fn h2f(v: u16) -> f32 { let s = if v & 0x8000 != 0 { -1.0 } else { 1.0 }; let e = ((v >> 10) & 0x1f) as i32; let m = (v & 0x3ff) as f32; if e == 0 { s * m * 2f32.powi(-24) } else { s * (1.0 + m / 1024.0) * 2f32.powi(e - 15) } }
fn hash(x: i32, y: i32) -> f32 { let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263); h = (h ^ (h >> 13)).wrapping_mul(1274126177); (h & 0xffff) as f32 / 65535.0 }
fn vnoise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32); let (fx, fy) = (x - x.floor(), y - y.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (a, b, c, d) = (hash(xi, yi), hash(xi + 1, yi), hash(xi, yi + 1), hash(xi + 1, yi + 1));
    let (u, v) = (s(fx), s(fy)); a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v
}
fn scene(w: u32, h: u32, t: f32, bv: (f32, f32), fv: (f32, f32), r: f32) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0; 3]; (w * h) as usize];
    let (cx, cy) = (w as f32 * 0.5 + fv.0 * t, h as f32 * 0.55 + fv.1 * t);
    for y in 0..h { for x in 0..w {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let mut c;
        let (bx, by) = (px - bv.0 * t, py - bv.1 * t);
        let mut n = vnoise(bx / 9.0, by / 9.0) * 0.6 + vnoise(bx / 3.0, by / 3.0) * 0.4;
        if r < 0.0 { n = 0.25 + 0.6 * ((((bx / 16.0).floor() as i32 + (by / 16.0).floor() as i32) & 1) as f32) + 0.15 * vnoise(bx / 40.0, by / 40.0); }
        c = [0.15 + 0.6 * n, 0.25 + 0.5 * n, 0.35 + 0.3 * vnoise(bx / 17.0, by / 17.0)];
        let (dx, dy) = (px - cx, py - cy);
        if dx * dx + dy * dy < r * r {
            let m = vnoise(dx / 4.0 + 50.0, dy / 4.0 + 50.0);
            c = [0.9 * m + 0.1, 0.35 * m, 0.2 + 0.2 * m];
        }
        if x >= 16 && x < 200 && y >= 12 && y < 40 {
            let on = ((x / 6) + (y / 7)) % 3 == 0;
            c = if on { [0.95, 0.95, 0.9] } else { [0.05, 0.05, 0.06] };
        }
        out[(y * w + x) as usize] = c;
    } }
    out
}
fn psnr(a: &[[f32; 3]], b: &[[f32; 3]]) -> f64 {
    let mse = a.iter().zip(b).map(|(p, q)| (0..3).map(|i| ((p[i] - q[i]) as f64).powi(2)).sum::<f64>()).sum::<f64>() / (a.len() * 3) as f64;
    10.0 * (1.0 / mse.max(1e-12)).log10()
}
pub fn fgbench() -> Result<String> {
    let g = Gpu::new(0, None)?;
    let (w, h) = (640u32, 360u32);
    let upload = |px: &[[f32; 3]]| -> Result<Tex> {
        let data: Vec<u32> = px.iter().map(|c| { let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32; 0xFF000000 | (q(c[0]) << 16) | (q(c[1]) << 8) | q(c[2]) }).collect();
        let d = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut t = None; unsafe { g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; }
        let t: ID3D11Texture2D = t.unwrap(); let mut srv = None; unsafe { g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?; }
        Ok(Tex { t, srv, uav: None, rtv: None, w, h })
    };
    let readback = |src: &ID3D11Texture2D| -> Result<Vec<[f32; 3]>> {
        let d = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16G16B16A16_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
        let mut st = None; unsafe { g.dev.CreateTexture2D(&d, None, Some(&mut st))?; } let st: ID3D11Texture2D = st.unwrap();
        unsafe {
            g.ctx.CopyResource(&st, src); let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let mut out = vec![[0.0; 3]; (w * h) as usize];
            for y in 0..h as usize { let row = (m.pData as *const u8).add(y * m.RowPitch as usize) as *const u16;
                for x in 0..w as usize { let p = row.add(x * 4); out[y * w as usize + x] = [h2f(*p), h2f(*p.add(1)), h2f(*p.add(2))]; } }
            g.ctx.Unmap(&st, 0); Ok(out)
        }
    };
    let cases: [(&str, (f32, f32), (f32, f32), f32); 5] = [
        ("repetitive grate -6px", (-6.0, 0.0), (0.0, 0.0), -1.0),
        ("slow pan 6px", (-6.0, 0.0), (0.0, 0.0), 0.0),
        ("pan 16px + object 24px", (-16.0, 0.0), (24.0, 6.0), 60.0),
        ("fast 40px + object -32px", (40.0, 8.0), (-32.0, 0.0), 70.0),
        ("diagonal 20px, big object", (-14.0, -14.0), (20.0, -10.0), 110.0),
    ];
    let mut report = format!("  gpu {}
", g.name()); let mut sum = (0.0, 0.0, 0.0);
    for (name, bv, fv, r) in cases {
        let r = if r == 0.0 { 0.001 } else { r };
        let (f0, f1, gt) = (scene(w, h, 0.0, bv, fv, r), scene(w, h, 1.0, bv, fv, r), scene(w, h, 0.5, bv, fv, r));
        let (t0, t1) = (upload(&f0)?, upload(&f1)?);
        let flow: f32 = std::env::var("FS_FLOW").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5);
        let qual: i32 = std::env::var("FS_QUALITY").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let mut fg = FrameGen::new(&g, w, h, flow)?;
        fg.ingest(&g, &t0, qual, false); fg.ingest(&g, &t1, qual, false);
        fg.interp(&g, &t0, &t1, 0.5, 0.5, false);
        let out = readback(&fg.out.t)?;
        let (fw, fh) = fg.dims[0];
        let flow = {
            let d = D3D11_TEXTURE2D_DESC { Width: fw, Height: fh, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16G16_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
            let mut st = None; unsafe { g.dev.CreateTexture2D(&d, None, Some(&mut st))?; } let st: ID3D11Texture2D = st.unwrap();
                unsafe { g.ctx.CopyResource(&st, &fg.ff[0].t); let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
                let mut v = vec![(0.0f32, 0.0f32); (fw * fh) as usize];
                for y in 0..fh as usize { let row = (m.pData as *const u8).add(y * m.RowPitch as usize) as *const u16; for x in 0..fw as usize { v[y * fw as usize + x] = (h2f(*row.add(x * 2)), h2f(*row.add(x * 2 + 1))); } }
                g.ctx.Unmap(&st, 0); v }
        };
        let (mut err, mut n) = (0.0f32, 0.0f32);
        for y in fh / 2..fh { for x in 0..fw / 6 { let (vx, vy) = flow[(y * fw + x) as usize]; let (ex, ey) = (vx / fg.scale - bv.0, vy / fg.scale - bv.1); err += (ex * ex + ey * ey).sqrt(); n += 1.0; } }
        report += &format!("  bg flow EPE {:.2}px  ", err / n);
        let blend: Vec<[f32; 3]> = f0.iter().zip(&f1).map(|(a, b)| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5]).collect();
        let (p_fg, p_rep, p_blend) = (psnr(&out, &gt), psnr(&f0, &gt), psnr(&blend, &gt));
        sum.0 += p_fg; sum.1 += p_rep; sum.2 += p_blend;
        let (ox, oy) = (w as f32 * 0.5 + fv.0 * 0.5, h as f32 * 0.55 + fv.1 * 0.5);
        let (mut ro, mut rh, mut rb) = ((vec![], vec![]), (vec![], vec![]), (vec![], vec![]));
        for y in 0..h { for x in 0..w {
            let i = (y * w + x) as usize; let (dx, dy) = (x as f32 - ox, y as f32 - oy);
            let tgt = if (16..200).contains(&x) && (12..40).contains(&y) { &mut rh } else if (dx * dx + dy * dy).sqrt() < r.max(0.001) + 6.0 { &mut ro } else { &mut rb };
            tgt.0.push(out[i]); tgt.1.push(gt[i]);
        } }
        let rp = |v: &(Vec<[f32; 3]>, Vec<[f32; 3]>)| if v.0.is_empty() { f64::NAN } else { psnr(&v.0, &v.1) };
        report += &format!("{name:<28} FG {p_fg:6.2} dB | repeat {p_rep:6.2} | blend {p_blend:6.2}   [object {:.1} | bg {:.1} | hud {:.1}]\n", rp(&ro), rp(&rb), rp(&rh));
    }
    report += &format!("{:<28} FG {:6.2} dB | repeat {:6.2} | blend {:6.2}", "MEAN", sum.0 / 5.0, sum.1 / 5.0, sum.2 / 5.0);
    Ok(report)
}

/// `-scalebench`: halve a synthetic scene (box filter), rebuild it 2x with every scaler and report PSNR against
/// the original plus GPU time. A correct port of NIS/SGSR/Anime4K must beat bilinear here.
pub fn scalebench(dump: Option<&std::path::Path>) -> Result<String> {
    let g = Gpu::new(0, None)?;
    let (w, h) = (1280u32, 720u32);
    let hi = scene(w, h, 0.0, (0.0, 0.0), (0.0, 0.0), 110.0);
    let (lw, lh) = (w / 2, h / 2);
    let lo: Vec<[f32; 3]> = (0..lw * lh).map(|i| { let (x, y) = ((i % lw) * 2, (i / lw) * 2); let mut c = [0.0; 3];
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] { let p = hi[((y + dy) * w + x + dx) as usize]; for k in 0..3 { c[k] += p[k] * 0.25; } } c }).collect();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let data: Vec<u32> = lo.iter().map(|c| 0xFF000000 | (q(c[0]) << 16) | (q(c[1]) << 8) | q(c[2])).collect();
    let src = unsafe {
        let d = D3D11_TEXTURE2D_DESC { Width: lw, Height: lh, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as _, SysMemPitch: lw * 4, SysMemSlicePitch: 0 };
        let mut t = None; g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; let t: ID3D11Texture2D = t.unwrap();
        let mut srv = None; g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?; Tex { t, srv, uav: None, rtv: None, w: lw, h: lh }
    };
    let rt = g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT, false, true)?;
    let stage = unsafe {
        let d = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16G16B16A16_FLOAT, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
        let mut t = None; g.dev.CreateTexture2D(&d, None, Some(&mut t))?; t.unwrap()
    };
    let sync = || unsafe {
        let mut q = None; let _ = g.dev.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut q));
        if let Some(q) = q { g.ctx.End(&q); let mut done = BOOL(0); loop { let _ = g.ctx.GetData(&q, Some(&mut done as *mut _ as *mut c_void), 4, 0); if done.as_bool() { break; } std::hint::spin_loop(); } }
    };
    let names = ["Off", "Nearest", "Integer", "Bilinear", "Sharp bilinear", "Bicubic", "Lanczos 3", "FSR 1", "Line", "NIS", "SGSR", "xBR", "Anime4K"];
    let mut cases: Vec<(String, i32, bool, i32)> = vec![];
    for sc in 1..=12 { if sc == 2 || sc == 8 { continue; } cases.push((names[sc as usize].to_string(), sc, false, 0)); }
    cases.push(("FSR 1 performance".into(), 7, true, 0));
    for (i, n) in ["S", "M", "L", "VL", "UL"].iter().enumerate().skip(1) { cases.push((format!("Anime4K {n}"), 12, false, i as i32)); }
    let mut s = Settings::test();
    s.sharpness = 0.0; s.fit = 1; s.scale_mode = 0;
    let mut report = format!("  gpu {}  ({lw}x{lh} -> {w}x{h}, PSNR vs. original, higher is better)
", g.name());
    for (name, sc, fast, a4) in cases {
        s.scaler = sc; s.fsr_fast = fast; s.a4k_size = a4;
        let mut r = scale::Scaler::empty();
        r.prepare(&g, &s, (lw, lh), (w as i32, h as i32));
        for _ in 0..3 { r.render(&g, &s, rt.rtv.as_ref().unwrap(), (&src.srv, lw, lh), (w as i32, h as i32)); }
        sync();
        let t0 = qpc(); let n = 20;
        for _ in 0..n { r.render(&g, &s, rt.rtv.as_ref().unwrap(), (&src.srv, lw, lh), (w as i32, h as i32)); }
        sync();
        let ms = (qpc() - t0) as f64 * 1000.0 / qpf() as f64 / n as f64;
        let out = unsafe {
            g.ctx.CopyResource(&stage, &rt.t); let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&stage, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let mut o = vec![[0.0f32; 3]; (w * h) as usize];
            for y in 0..h as usize { let row = (m.pData as *const u8).add(y * m.RowPitch as usize) as *const u16;
                for x in 0..w as usize { let p = row.add(x * 4); o[y * w as usize + x] = [h2f(*p), h2f(*p.add(1)), h2f(*p.add(2))]; } }
            g.ctx.Unmap(&stage, 0); o
        };
        report += &format!("  {name:<20} {:6.2} dB  {ms:6.2} ms
", psnr(&out, &hi));
        if let Some(dir) = dump {
            let file = dir.join(format!("scale_{}.bmp", name.replace(' ', "_")));
            let (cw, ch) = (320u32, 180u32);
            let mut f = Vec::with_capacity(54 + (cw * ch * 4) as usize);
            f.extend_from_slice(b"BM"); f.extend_from_slice(&(54 + cw * ch * 4).to_le_bytes()); f.extend_from_slice(&[0; 4]); f.extend_from_slice(&54u32.to_le_bytes());
            f.extend_from_slice(&40u32.to_le_bytes()); f.extend_from_slice(&cw.to_le_bytes()); f.extend_from_slice(&(-(ch as i32)).to_le_bytes()); f.extend_from_slice(&[1, 0, 32, 0]); f.extend_from_slice(&[0; 24]);
            for y in 0..ch { for x in 0..cw { let p = out[((y + 300) * w + x + 480) as usize]; f.extend_from_slice(&[q(p[2]) as u8, q(p[1]) as u8, q(p[0]) as u8, 255]); } }
            let _ = std::fs::write(file, f);
        }
    }
    // Variable rate scaling: after a partial change the image must equal a full redraw, and a static frame must cost almost nothing.
    let mut changed = data.clone();
    for y in 100..140 { for x in 200..260 { changed[(y * lw + x) as usize] ^= 0x00FF_8040; } }
    let upload = |px: &[u32]| -> Result<Tex> { unsafe {
        let d = D3D11_TEXTURE2D_DESC { Width: lw, Height: lh, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as _, SysMemPitch: lw * 4, SysMemSlicePitch: 0 };
        let mut t = None; g.dev.CreateTexture2D(&d, Some(&init), Some(&mut t))?; let t: ID3D11Texture2D = t.unwrap();
        let mut srv = None; g.dev.CreateShaderResourceView(&t, None, Some(&mut srv))?; Ok(Tex { t, srv, uav: None, rtv: None, w: lw, h: lh }) } };
    let b = upload(&changed)?;
    let grab = |rt: &Tex| -> Result<Vec<[f32; 3]>> { unsafe {
        g.ctx.CopyResource(&stage, &rt.t); let mut m = D3D11_MAPPED_SUBRESOURCE::default(); g.ctx.Map(&stage, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        let mut o = vec![[0.0f32; 3]; (w * h) as usize];
        for y in 0..h as usize { let row = (m.pData as *const u8).add(y * m.RowPitch as usize) as *const u16;
            for x in 0..w as usize { let p = row.add(x * 4); o[y * w as usize + x] = [h2f(*p), h2f(*p.add(1)), h2f(*p.add(2))]; } }
        g.ctx.Unmap(&stage, 0); Ok(o) } };
    s.scaler = 6; s.vrs = true; s.a4k_size = 0;
    let mut full = scale::Scaler::empty(); full.prepare(&g, &s, (lw, lh), (w as i32, h as i32));
    full.render(&g, &s, rt.rtv.as_ref().unwrap(), (&b.srv, lw, lh), (w as i32, h as i32));
    let reference = grab(&rt)?;
    let mut v = scale::Scaler::empty(); v.prepare(&g, &s, (lw, lh), (w as i32, h as i32));
    v.render_vrs(&g, &s, rt.rtv.as_ref().unwrap(), (&src.srv, lw, lh), (w as i32, h as i32), Some(&src.srv));
    v.render_vrs(&g, &s, rt.rtv.as_ref().unwrap(), (&b.srv, lw, lh), (w as i32, h as i32), Some(&src.srv));
    let partial = grab(&rt)?;
    let same = partial.iter().zip(&reference).all(|(a, b)| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3));
    let time = |f: &mut dyn FnMut()| { for _ in 0..3 { f(); } sync(); let t0 = qpc(); for _ in 0..20 { f(); } sync(); (qpc() - t0) as f64 * 1000.0 / qpf() as f64 / 20.0 };
    let t_full = time(&mut || full.render(&g, &s, rt.rtv.as_ref().unwrap(), (&b.srv, lw, lh), (w as i32, h as i32)));
    let t_static = time(&mut || v.render_vrs(&g, &s, rt.rtv.as_ref().unwrap(), (&b.srv, lw, lh), (w as i32, h as i32), Some(&b.srv)));
    report += &format!("  VRS (Lanczos 3)      partial change matches a full redraw: {same}; full {t_full:.2} ms, static frame {t_static:.2} ms
");
    if !same { return Err(windows::core::Error::new(E_FAIL, format!("VRS output differs from a full redraw
{report}"))); }
    Ok(report)
}

pub fn fgcost(w: u32, h: u32, flow: f32, quality: i32) -> Result<f64> {
    let g = Gpu::new(0, None)?;
    let a = g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT, true, false)?;
    let b = g.tex(w, h, DXGI_FORMAT_R16G16B16A16_FLOAT, true, false)?;
    let mut fg = FrameGen::new(&g, w, h, flow)?;
    fg.dedup = false;
    let sync = || unsafe {
        let mut q = None; let _ = g.dev.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut q));
        if let Some(q) = q { g.ctx.End(&q); let mut done = BOOL(0); loop { let _ = g.ctx.GetData(&q, Some(&mut done as *mut _ as *mut c_void), 4, 0); if done.as_bool() { break; } std::hint::spin_loop(); } }
    };
    for _ in 0..5 { fg.ingest(&g, &a, quality, false); fg.interp(&g, &a, &b, 0.5, 0.5, false); }
    sync();
    let n = 40; let t0 = qpc();
    for i in 0..n { fg.ingest(&g, if i % 2 == 0 { &a } else { &b }, quality, false); fg.interp(&g, &a, &b, 0.5, 0.5, false); }
    sync();
    Ok((qpc() - t0) as f64 * 1000.0 / qpf() as f64 / n as f64)
}
