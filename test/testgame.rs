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
