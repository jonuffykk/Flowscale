//! In-game overlay. One renderer (GDI text + hand-drawn frame-time bars, rounded panel, premultiplied alpha)
//! feeds two outputs: a texture composited by the scaling engine, and a click-through layered window that
//! sits on top of a game when only the limiter is attached (works for windowed and borderless games).
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::OsdStyle;

/// Hotkey toggle, shared by both outputs.
pub static VISIBLE: AtomicBool = AtomicBool::new(true);
/// The engine draws its own overlay while scaling; the standalone window then stays hidden.
pub static ENGINE_DRAWING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Ctl { on: bool, for_limiter: bool, style: OsdStyle }
static CTL: Mutex<Option<Ctl>> = Mutex::new(None);
pub fn configure(on: bool, for_limiter: bool, style: OsdStyle) { *CTL.lock().unwrap() = Some(Ctl { on, for_limiter, style }); }
pub fn engine_style() -> Option<OsdStyle> { CTL.lock().unwrap().filter(|c| c.on).map(|c| c.style) }

#[derive(Default, Clone)]
pub struct OsdData { pub fps: f32, pub game_fps: f32, pub frame_ms: f32, pub low1: f32, pub cap: f32, pub tag: String, pub graph: Vec<f32> }

pub struct Canvas { pub w: u32, pub h: u32, pub px: Vec<u32> }

struct Theme { bg: (u8, u8, u8), text: (u8, u8, u8), muted: (u8, u8, u8), accent: (u8, u8, u8), warn: (u8, u8, u8) }
/// Accent choices: blue, green, amber, pink, cyan, white/ink (contrast with the panel).
fn accent(i: i32, light: bool) -> (u8, u8, u8) {
    match (i, light) {
        (1, false) => (74, 222, 128), (1, true) => (17, 150, 90),
        (2, false) => (255, 196, 70), (2, true) => (196, 120, 0),
        (3, false) => (255, 120, 190), (3, true) => (212, 40, 130),
        (4, false) => (80, 220, 240), (4, true) => (0, 140, 170),
        (5, false) => (246, 247, 249), (5, true) => (17, 20, 26),
        (_, false) => (110, 150, 255), (_, true) => (36, 86, 245),
    }
}
fn theme(light: bool) -> Theme {
    if light { Theme { bg: (255, 255, 255), text: (17, 20, 26), muted: (96, 103, 116), accent: (36, 86, 245), warn: (214, 120, 0) } }
    else { Theme { bg: (18, 20, 25), text: (246, 247, 249), muted: (150, 157, 170), accent: (110, 150, 255), warn: (255, 176, 64) } }
}
fn cref(c: (u8, u8, u8)) -> COLORREF { COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16) }
fn wide(s: &str) -> Vec<u16> { s.encode_utf16().collect() }
fn f1(v: f32) -> String { if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") } }

/// Windows 11 ships Segoe UI Variable; on Windows 10 GDI would silently substitute an unrelated face.
fn variable_font(dc: HDC) -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| unsafe {
        let f = CreateFontW(-16, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_TT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, w!("Segoe UI Variable Display"));
        let old = SelectObject(dc, f.into());
        let mut b = [0u16; 64]; let n = GetTextFaceW(dc, Some(&mut b));
        SelectObject(dc, old); let _ = DeleteObject(f.into());
        String::from_utf16_lossy(&b[..(n.max(1) as usize - 1).min(64)]).eq_ignore_ascii_case("Segoe UI Variable Display")
    })
}

/// Render the panel. `base` is the pixel scale of the target (1.0 at 1080p).
pub fn render(d: &OsdData, st: &OsdStyle, base: f32) -> Option<Canvas> {
    let k = (st.scale * base).clamp(0.5, 4.0);
    let px = |v: f32| (v * k).round() as i32;
    let mut t = theme(st.light);
    t.accent = accent(st.color, st.light);
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() { return None; }
        let face = match st.font { 1 => w!("Bahnschrift"), 2 => w!("Consolas"), _ => if variable_font(dc) { w!("Segoe UI Variable Display") } else { w!("Segoe UI") } };
        let font = |h: f32, weight: i32| CreateFontW(-px(h), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_TT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, face);
        let (big, small) = (font(24.0, 650), font(12.5, 500));
        let measure = |f: HFONT, s: &str| -> (i32, i32) {
            let old = SelectObject(dc, f.into()); let mut sz = SIZE::default(); let v = wide(s);
            let _ = GetTextExtentPoint32W(dc, &v, &mut sz); SelectObject(dc, old); (sz.cx, sz.cy)
        };

        // Content
        let fg = d.game_fps > 0.5 && d.fps > d.game_fps * 1.15;
        let head = if d.fps > 0.0 { f1(d.fps) } else { "—".into() };
        let unit = " FPS";
        let mut sub: Vec<(String, (u8, u8, u8))> = vec![];
        if fg { sub.push((format!("{} → {}", f1(d.game_fps), f1(d.fps)), t.accent)); }
        if st.detail >= 1 {
            if d.frame_ms > 0.0 { sub.push((format!("{:.1} ms", d.frame_ms), t.muted)); }
            if d.low1 > 0.0 { sub.push((format!("1% {}", f1(d.low1)), if d.low1 < d.fps * 0.75 { t.warn } else { t.muted })); }
            if d.cap > 0.0 { sub.push((format!("cap {}", f1(d.cap)), t.muted)); }
            if !d.tag.is_empty() { sub.push((d.tag.clone(), t.muted)); }
        }
        let gap = px(10.0);
        let (hw, hh) = measure(big, &head); let (uw, _) = measure(small, unit);
        let sub_w: i32 = sub.iter().map(|(s, _)| measure(small, s).0).sum::<i32>() + gap * (sub.len() as i32 - 1).max(0);
        let (_, sh) = measure(small, "0");
        let pad = px(11.0);
        let graph_h = if st.detail >= 2 && d.graph.len() > 4 { px(34.0) } else { 0 };
        let row1_w = hw + uw;
        let inner_w = row1_w.max(sub_w).max(if graph_h > 0 { px(168.0) } else { 0 });
        let compact_inline = st.detail == 0 && sub.len() == 1;
        let (w, h) = if compact_inline {
            let sw = measure(small, &sub[0].0).0;
            (pad * 2 + row1_w + gap + sw, pad * 2 + hh)
        } else {
            (pad * 2 + inner_w, pad * 2 + hh + if sub.is_empty() { 0 } else { px(2.0) + sh } + if graph_h > 0 { px(8.0) + graph_h } else { 0 })
        };
        let (w, h) = (w.max(8) as u32, h.max(8) as u32);

        let bmi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else { let _ = DeleteDC(dc); return None; };
        let old_bmp = SelectObject(dc, dib.into());
        let brush = CreateSolidBrush(cref(t.bg));
        FillRect(dc, &RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }, brush);
        let _ = DeleteObject(brush.into());
        SetBkMode(dc, TRANSPARENT);
        let text = |f: HFONT, s: &str, x: i32, y: i32, c: (u8, u8, u8)| { let old = SelectObject(dc, f.into()); SetTextColor(dc, cref(c)); let v = wide(s); let _ = TextOutW(dc, x, y, &v); SelectObject(dc, old); };

        text(big, &head, pad, pad, t.text);
        let base_y = pad + hh - sh - px(3.0);
        text(small, unit, pad + hw, base_y, t.muted);
        let mut y = pad + hh + px(2.0);
        if compact_inline { text(small, &sub[0].0, pad + row1_w + gap, base_y, sub[0].1); }
        else if !sub.is_empty() {
            let mut x = pad;
            for (s, c) in &sub { text(small, s, x, y, *c); x += measure(small, s).0 + gap; }
            y += sh;
        }
        let _ = GdiFlush();
        let mut out = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize).to_vec();

        // Frame-time bars: height relative to the median, so spikes stand out at any frame rate.
        if graph_h > 0 {
            let top = y + px(8.0);
            let gw = w as i32 - pad * 2;
            let n = d.graph.len().min(gw.max(1) as usize);
            let v = &d.graph[d.graph.len() - n..];
            let mut s = v.to_vec(); s.sort_by(f32::total_cmp);
            let med = s[s.len() / 2].max(0.1);
            let bw = (gw as f32 / n as f32).max(1.0);
            for (i, &ms) in v.iter().enumerate() {
                let r = (ms / (med * 2.0)).clamp(0.08, 1.0);
                let bh = ((graph_h as f32) * r).round().max(1.0) as i32;
                // Soft area with a crisp 2px edge on top: steady pacing reads as a flat line, spikes stand out.
                let col = if ms > med * 1.5 { t.warn } else { t.accent };
                let mix = |a: u8, b: u8, f: f32| (a as f32 + (b as f32 - a as f32) * f) as u32;
                let soft = 0xFF00_0000 | mix(t.bg.0, col.0, 0.22) << 16 | mix(t.bg.1, col.1, 0.22) << 8 | mix(t.bg.2, col.2, 0.22);
                let edge = 0xFF00_0000 | (col.0 as u32) << 16 | (col.1 as u32) << 8 | col.2 as u32;
                let line = px(2.0).max(1);
                let x0 = pad + (i as f32 * bw) as i32; let x1 = (pad + ((i + 1) as f32 * bw) as i32).max(x0 + 1);
                let y0 = top + graph_h - bh;
                for yy in y0..(top + graph_h) { for xx in x0..x1.min(w as i32 - pad) {
                    if yy >= 0 && (yy as u32) < h && xx >= 0 { out[(yy as u32 * w + xx as u32) as usize] = if yy < y0 + line { edge } else { soft }; }
                } }
            }
        }

        SelectObject(dc, old_bmp);
        let _ = DeleteObject(dib.into()); let _ = DeleteObject(big.into()); let _ = DeleteObject(small.into()); let _ = DeleteDC(dc);

        // Rounded, anti-aliased panel mask + opacity, premultiplied.
        let r = px(9.0) as f32;
        let a_panel = st.opacity.clamp(0.3, 1.0);
        for yy in 0..h { for xx in 0..w {
            let (fx, fy) = (xx as f32 + 0.5, yy as f32 + 0.5);
            let cx = fx.clamp(r, w as f32 - r); let cy = fy.clamp(r, h as f32 - r);
            let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let cov = (r - dist + 0.5).clamp(0.0, 1.0);
            let a = a_panel * cov;
            let p = &mut out[(yy * w + xx) as usize];
            let (rr, gg, bb) = ((*p >> 16) & 255, (*p >> 8) & 255, *p & 255);
            let m = |c: u32| ((c as f32) * a + 0.5) as u32;
            *p = ((a * 255.0 + 0.5) as u32) << 24 | m(rr) << 16 | m(gg) << 8 | m(bb);
        } }
        Some(Canvas { w, h, px: out })
    }
}

/// Top-left of the panel inside `area` (x, y, w, h) for corner `pos` (0 TL, 1 TR, 2 BL, 3 BR).
pub fn place(pos: i32, area: [f32; 4], c: (u32, u32), margin: f32, offset: (f32, f32)) -> (f32, f32) {
    let (cw, ch) = (c.0 as f32, c.1 as f32);
    // The extra offset pushes the panel away from its corner (towards the centre).
    let (mx, my) = (margin + offset.0, margin + offset.1);
    let x = if pos == 1 || pos == 3 { area[0] + area[2] - cw - mx } else { area[0] + mx };
    let y = if pos >= 2 { area[1] + area[3] - ch - my } else { area[1] + my };
    (x.max(area[0]), y.max(area[1]))
}

// ---------------------------------------------------------------- standalone window (limiter only)

unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == WM_NCHITTEST { return LRESULT(-1); }
    unsafe { DefWindowProcW(h, m, wp, lp) }
}

fn push(hwnd: HWND, c: &Canvas, x: i32, y: i32) {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: c.w as i32, biHeight: -(c.h as i32), biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
        let mut bits: *mut c_void = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            std::ptr::copy_nonoverlapping(c.px.as_ptr(), bits as *mut u32, c.px.len());
            let old = SelectObject(dc, dib.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(hwnd, None, Some(&POINT { x, y }), Some(&SIZE { cx: c.w as i32, cy: c.h as i32 }), Some(dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(dc, old); let _ = DeleteObject(dib.into());
        }
        let _ = DeleteDC(dc);
    }
}

/// Runs for the app's lifetime. Reads the limiter's shared ring directly, so it never touches the UI thread.
pub fn spawn_limiter_window() {
    std::thread::spawn(|| unsafe {
        let hi = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hi.into(), lpszClassName: w!("FlowScaleOsd"), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("FlowScaleOsd"), PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hi.into()), None) else { return };
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        let mut map: Option<crate::ipc::Mapping> = None;
        let mut cursor = 0u32;
        let mut ring: std::collections::VecDeque<f32> = Default::default();
        let mut shown = false;
        let mut ring_pid = 0u32;
        let freq = crate::pacer::qpf();
        loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { DispatchMessageW(&msg); }
            let ctl = *CTL.lock().unwrap();
            let want = ctl.filter(|c| c.on && c.for_limiter).filter(|_| VISIBLE.load(Relaxed) && !ENGINE_DRAWING.load(Relaxed));
            let mut drawn = false;
            if let Some(c) = want {
                if map.is_none() { map = crate::ipc::Mapping::open(); if let Some(m) = &map { cursor = m.get().write_idx.load(Relaxed); } }
                if let Some(m) = &map {
                    let s = m.get();
                    let mut v = vec![]; cursor = s.read_since(cursor, &mut v);
                    let pid = s.attached_pid.load(Relaxed);
                    if pid != ring_pid { ring.clear(); ring_pid = pid; } // never mix frames from another game
                    ring.extend(v); while ring.len() > 600 { ring.pop_front(); }
                    let fresh = pid != 0 && pid == s.target_pid.load(Relaxed) && (crate::pacer::qpc() - s.heartbeat.load(Relaxed) as i64) < freq;
                    let fgw = GetForegroundWindow();
                    let mut fgp = 0u32; GetWindowThreadProcessId(fgw, Some(&mut fgp));
                    if fresh && fgp == pid && !IsIconic(fgw).as_bool() && ring.len() >= 4 {
                        let recent: Vec<f32> = ring.iter().rev().take(240).rev().copied().collect();
                        let last: Vec<f32> = recent.iter().rev().take(60).copied().collect();
                        let mean = last.iter().sum::<f32>() / last.len() as f32;
                        let mut srt = recent.clone(); srt.sort_by(|a, b| b.total_cmp(a));
                        let k = (srt.len() / 100).max(1); let low = srt[..k].iter().sum::<f32>() / k as f32;
                        let cap = if s.enabled.load(Relaxed) != 0 { s.milli_fps.load(Relaxed) as f32 / 1000.0 } else { 0.0 };
                        let api = match s.api.load(Relaxed) { crate::ipc::API_D3D11 => "D3D11", crate::ipc::API_D3D12 => "D3D12", crate::ipc::API_D3D9 => "D3D9", crate::ipc::API_GL => "OpenGL", crate::ipc::API_VK => "Vulkan", _ => "" };
                        let d = OsdData { fps: if mean > 0.0 { 1000.0 / mean } else { 0.0 }, game_fps: 0.0, frame_ms: mean, low1: if low > 0.0 { 1000.0 / low } else { 0.0 }, cap, tag: api.into(), graph: recent.iter().rev().take(150).rev().copied().collect() };
                        let mut cr = RECT::default(); let _ = GetClientRect(fgw, &mut cr);
                        let mut o = POINT::default(); let _ = windows::Win32::Graphics::Gdi::ClientToScreen(fgw, &mut o);
                        let base = ((cr.bottom - cr.top) as f32 / 1080.0).clamp(0.6, 2.5);
                        if let Some(cv) = render(&d, &c.style, base) {
                            let (x, y) = place(c.style.pos, [o.x as f32, o.y as f32, (cr.right - cr.left) as f32, (cr.bottom - cr.top) as f32], (cv.w, cv.h), 12.0 * base, (c.style.offset.0 * base, c.style.offset.1 * base));
                            push(hwnd, &cv, x as i32, y as i32);
                            if !shown { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); shown = true; }
                            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            drawn = true;
                        }
                    }
                }
            }
            if want.is_none() { ring.clear(); if let Some(m) = &map { cursor = m.get().write_idx.load(Relaxed); } } // resume with fresh frames only
            if !drawn && shown { let _ = ShowWindow(hwnd, SW_HIDE); shown = false; }
            std::thread::sleep(std::time::Duration::from_millis(if drawn { 100 } else { 250 }));
        }
    });
}

/// `flowscale.exe -osdshot <file.bmp>`: every detail level in both themes over a busy background, for review.
pub fn snapshot(path: &std::path::Path) -> std::io::Result<()> {
    let (w, h) = (960u32, 420u32);
    let mut img: Vec<u32> = (0..w * h).map(|i| { let (x, y) = (i % w, i / w); let v = ((x / 24 + y / 24) % 2) * 40 + 70 + (x * 60 / w); 0xFF00_0000 | (v / 2) << 16 | v << 8 | (v + 30).min(255) }).collect();
    let graph: Vec<f32> = (0..150).map(|i| 8.3 + ((i * 37) % 11) as f32 * 0.05 + if i % 47 == 0 { 9.0 } else { 0.0 }).collect();
    let d = OsdData { fps: 179.6, game_fps: 89.8, frame_ms: 5.57, low1: 168.0, cap: 90.0, tag: "1920×1080".into(), graph };
    let mut y = 12;
    for light in [false, true] {
        let mut x = 12;
        for detail in 0..3 {
            let st = OsdStyle { detail, pos: 0, light, scale: 1.0, opacity: 0.9, color: detail + if light { 2 } else { 0 }, font: detail % 3, offset: (0.0, 0.0) };
            let c = render(&d, &st, 1.0).ok_or_else(|| std::io::Error::other("render failed"))?;
            for cy in 0..c.h { for cx in 0..c.w {
                let (dx, dy) = (x + cx, y + cy); if dx >= w || dy >= h { continue; }
                let s = c.px[(cy * c.w + cx) as usize]; let a = s >> 24; let dst = &mut img[(dy * w + dx) as usize];
                let ch = |sh: u32| (((s >> sh) & 255) + ((*dst >> sh) & 255) * (255 - a) / 255).min(255) << sh;
                *dst = 0xFF00_0000 | ch(16) | ch(8) | ch(0);
            } }
            x += c.w + 16;
        }
        y += 200;
    }
    let mut f = Vec::with_capacity(54 + (w * h * 4) as usize);
    let le = |v: u32| v.to_le_bytes();
    f.extend_from_slice(b"BM"); f.extend_from_slice(&le(54 + w * h * 4)); f.extend_from_slice(&[0; 4]); f.extend_from_slice(&le(54));
    f.extend_from_slice(&le(40)); f.extend_from_slice(&le(w)); f.extend_from_slice(&(-(h as i32)).to_le_bytes()); f.extend_from_slice(&[1, 0, 32, 0]);
    f.extend_from_slice(&[0; 24]);
    for p in img { f.extend_from_slice(&p.to_le_bytes()); }
    std::fs::write(path, f)
}
