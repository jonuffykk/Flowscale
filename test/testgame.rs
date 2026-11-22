use windows::core::{w, Interface};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::OpenGL::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn wp(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == WM_DESTROY { unsafe { PostQuitMessage(0) }; return LRESULT(0); }
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let api = args.get(1).cloned().unwrap_or("d3d11".into());
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15.0);
    unsafe {
        let hi = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW { lpfnWndProc: Some(wp), hInstance: hi.into(), lpszClassName: w!("FsTestGame"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap(), ..Default::default() };
        RegisterClassW(&wc);
        let title = format!("FlowScale Test Game ({api})");
        let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("FsTestGame"), &windows::core::HSTRING::from(title), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 656, 399, None, None, Some(hi.into()), None).unwrap();
        let start = std::time::Instant::now();
        let mut frames = 0u64;
        let pump = || -> bool {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { if msg.message == WM_QUIT { return false; } let _ = TranslateMessage(&msg); DispatchMessageW(&msg); }
            start.elapsed().as_secs_f64() < secs
        };
        let col = |t: f32| [0.5 + 0.5 * (t * 3.0).sin(), 0.25, 0.5 + 0.5 * (t * 2.0).cos(), 1.0];
        match api.as_str() {
            "d3d9" => {
                let d3d = Direct3DCreate9(D3D_SDK_VERSION).unwrap();
                let mut pp = D3DPRESENT_PARAMETERS { Windowed: true.into(), SwapEffect: D3DSWAPEFFECT_DISCARD, hDeviceWindow: h, PresentationInterval: D3DPRESENT_INTERVAL_IMMEDIATE as u32, ..Default::default() };
                let mut dev = None;
                d3d.CreateDevice(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, h, D3DCREATE_HARDWARE_VERTEXPROCESSING as u32, &mut pp, &mut dev).unwrap();
                let dev = dev.unwrap();
                while pump() {
                    let c = col(start.elapsed().as_secs_f32());
                    let _ = dev.Clear(0, std::ptr::null(), D3DCLEAR_TARGET as u32, ((c[0] * 255.0) as u32) << 16 | 0x40 << 8 | (c[2] * 255.0) as u32, 1.0, 0);
                    let x = (frames % 600) as i32;
                    let r = D3DRECT { x1: x, y1: 150, x2: x + 40, y2: 220 };
                    let _ = dev.Clear(1, &r, D3DCLEAR_TARGET as u32, 0xFFFFFFFF, 1.0, 0);
                    let _ = dev.Present(std::ptr::null(), std::ptr::null(), HWND::default(), std::ptr::null());
                    frames += 1;
                }
            }
            // Direct3D 8 and DirectDraw through raw COM vtables (the windows crate does not wrap them).
            "d3d8" => {
                let m = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("d3d8.dll")).unwrap();
                let create: unsafe extern "system" fn(u32) -> *mut std::ffi::c_void = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(m, windows::core::s!("Direct3DCreate8")).unwrap());
                let d3d = create(220);
                let vt = *(d3d as *const *const usize);
                let mut mode = [0u32; 4];
                let gadm: unsafe extern "system" fn(*mut std::ffi::c_void, u32, *mut u32) -> windows::core::HRESULT = std::mem::transmute(*vt.add(8));
                let _ = gadm(d3d, 0, mode.as_mut_ptr());
                #[repr(C)] struct Pp { w: u32, h: u32, format: u32, count: u32, ms: u32, swap: u32, hwnd: isize, windowed: i32, ads: i32, dsf: u32, flags: u32, refresh: u32, interval: u32 }
                let mut pp = Pp { w: 640, h: 360, format: mode[3], count: 1, ms: 0, swap: 1, hwnd: h.0 as isize, windowed: 1, ads: 0, dsf: 0, flags: 0, refresh: 0, interval: 0 };
                let cd: unsafe extern "system" fn(*mut std::ffi::c_void, u32, u32, HWND, u32, *mut Pp, *mut *mut std::ffi::c_void) -> windows::core::HRESULT = std::mem::transmute(*vt.add(15));
                let mut dev = std::ptr::null_mut();
                cd(d3d, 0, 1, h, 0x20, &mut pp, &mut dev).unwrap();
                let dvt = *(dev as *const *const usize);
                // IDirect3DDevice8::Clear is 36, Present is 15.
                let clear: unsafe extern "system" fn(*mut std::ffi::c_void, u32, *const i32, u32, u32, f32, u32) -> windows::core::HRESULT = std::mem::transmute(*dvt.add(36));
                let present: unsafe extern "system" fn(*mut std::ffi::c_void, *const u8, *const u8, HWND, *const u8) -> windows::core::HRESULT = std::mem::transmute(*dvt.add(15));
                while pump() {
                    let c = col(start.elapsed().as_secs_f32());
                    let _ = clear(dev, 0, std::ptr::null(), 1, ((c[0] * 255.0) as u32) << 16 | 0x40 << 8 | (c[2] * 255.0) as u32, 1.0, 0);
                    let _ = present(dev, std::ptr::null(), std::ptr::null(), HWND::default(), std::ptr::null());
                    frames += 1;
                }
            }
            "ddraw" => {
                let m = windows::Win32::System::LibraryLoader::LoadLibraryW(w!("ddraw.dll")).unwrap();
                let create: unsafe extern "system" fn(*const u8, *mut *mut std::ffi::c_void, *mut u8) -> windows::core::HRESULT = std::mem::transmute(windows::Win32::System::LibraryLoader::GetProcAddress(m, windows::core::s!("DirectDrawCreate")).unwrap());
                let mut dd = std::ptr::null_mut(); create(std::ptr::null(), &mut dd, std::ptr::null_mut()).unwrap();
                let vt = *(dd as *const *const usize);
                let scl: unsafe extern "system" fn(*mut std::ffi::c_void, HWND, u32) -> windows::core::HRESULT = std::mem::transmute(*vt.add(20));
                let _ = scl(dd, h, 8);
                let cs: unsafe extern "system" fn(*mut std::ffi::c_void, *mut u8, *mut *mut std::ffi::c_void, *mut u8) -> windows::core::HRESULT = std::mem::transmute(*vt.add(6));
                let (dsize, coff) = if cfg!(target_pointer_width = "64") { (120usize, 112usize) } else { (108, 104) };
                let surf = |caps: u32, w: u32, ht: u32| { let mut d = [0u8; 120]; d[0..4].copy_from_slice(&(dsize as u32).to_le_bytes());
                    let flags = if w > 0 { 1 | 2 | 4 } else { 1 }; d[4..8].copy_from_slice(&(flags as u32).to_le_bytes()); d[8..12].copy_from_slice(&ht.to_le_bytes()); d[12..16].copy_from_slice(&w.to_le_bytes());
                    d[coff..coff + 4].copy_from_slice(&caps.to_le_bytes()); let mut s = std::ptr::null_mut(); cs(dd, d.as_mut_ptr(), &mut s, std::ptr::null_mut()).unwrap(); s };
                let primary = surf(0x200, 0, 0);
                let back = surf(0x40, 640, 360);
                let sv = *(primary as *const *const usize);
                // IDirectDrawSurface::Blt (5) of the off-screen image to the window's area of the primary surface.
                let blt: unsafe extern "system" fn(*mut std::ffi::c_void, *const RECT, *mut std::ffi::c_void, *const RECT, u32, *const u8) -> windows::core::HRESULT = std::mem::transmute(*sv.add(5));
                while pump() {
                    let mut r = RECT::default(); let _ = GetClientRect(h, &mut r);
                    let mut p = POINT::default(); let _ = ClientToScreen(h, &mut p);
                    let dst = RECT { left: p.x, top: p.y, right: p.x + r.right, bottom: p.y + r.bottom };
                    let _ = blt(primary, &dst, back, std::ptr::null(), 0x0100_0000, std::ptr::null());
                    frames += 1;
                }
            }
            "gl" => {
                let dc = GetDC(Some(h));
                let pfd = PIXELFORMATDESCRIPTOR { nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16, nVersion: 1, dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, iPixelType: PFD_TYPE_RGBA, cColorBits: 32, ..Default::default() };
                let pf = ChoosePixelFormat(dc, &pfd); let _ = SetPixelFormat(dc, pf, &pfd);
                let rc = wglCreateContext(dc).unwrap(); let _ = wglMakeCurrent(dc, rc);
                if let Some(p) = wglGetProcAddress(windows::core::s!("wglSwapIntervalEXT")) { let f: extern "system" fn(i32) -> i32 = std::mem::transmute(p); f(0); }
                while pump() {
                    let c = col(start.elapsed().as_secs_f32());
                    glClearColor(c[0], c[1], c[2], 1.0); glClear(GL_COLOR_BUFFER_BIT);
                    glEnable(GL_SCISSOR_TEST); glScissor((frames % 600) as i32, 150, 40, 70); glClearColor(1.0, 1.0, 1.0, 1.0); glClear(GL_COLOR_BUFFER_BIT); glDisable(GL_SCISSOR_TEST);
                    let _ = SwapBuffers(dc);
                    frames += 1;
                }
            }
            _ => {
                let desc = DXGI_SWAP_CHAIN_DESC { BufferDesc: DXGI_MODE_DESC { Width: 640, Height: 360, Format: DXGI_FORMAT_B8G8R8A8_UNORM, ..Default::default() }, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2, OutputWindow: h, Windowed: true.into(), SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, ..Default::default() };
                let (mut sc, mut dev, mut ctx) = (None, None, None);
                D3D11CreateDeviceAndSwapChain(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION, Some(&desc), Some(&mut sc), Some(&mut dev), None, Some(&mut ctx)).unwrap();
                let (sc, dev, ctx): (IDXGISwapChain, ID3D11Device, ID3D11DeviceContext) = (sc.unwrap(), dev.unwrap(), ctx.unwrap());
                let bb: ID3D11Texture2D = sc.GetBuffer(0).unwrap();
                let mut rtv = None; dev.CreateRenderTargetView(&bb, None, Some(&mut rtv)).unwrap(); let rtv = rtv.unwrap();
                let ctx1: ID3D11DeviceContext1 = ctx.cast().unwrap();
                let dup: u64 = std::env::var("FS_DUP").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
                while pump() {
                    let k = frames / dup;
                    let c = if dup > 1 { col(k as f32 * 0.01) } else { col(start.elapsed().as_secs_f32()) };
                    ctx.ClearRenderTargetView(&rtv, &c);
                    let x = if dup > 1 { ((k * 8) % 600) as i32 } else { (frames % 600) as i32 };
                    ctx1.ClearView(&rtv, &[1.0, 1.0, 1.0, 1.0], Some(&[RECT { left: x, top: 150, right: x + 40, bottom: 220 }]));
                    let _ = sc.Present(0, DXGI_PRESENT(0));
                    frames += 1;
                    if dup > 1 { let due = std::time::Duration::from_secs_f64(frames as f64 / 180.0); while start.elapsed() < due { std::hint::spin_loop(); } }
                }
            }
        }
        let el = start.elapsed().as_secs_f64();
        println!("{api}: {frames} frames in {el:.2}s = {:.1} fps", frames as f64 / el);
    }
}
