//! G-SYNC for FlowScale's own output window (NVIDIA only).
//!
//! The driver only engages G-SYNC for windowed swap chains when the application's profile allows it.
//! When the user turns "G-SYNC support" on, FlowScale adds an application profile for its own exe through
//! the public NVAPI driver-settings (DRS) interface and sets VRR to "fullscreen and windowed" with the
//! application override on "allow". Turning the option off deletes exactly those two settings again.
//! Nothing outside FlowScale's own profile is touched.
use std::ffi::c_void;
use std::sync::OnceLock;

use windows::core::{s, w};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const VRR_APP_OVERRIDE_ID: u32 = 0x10A8_79CF; // 0 = allow
const VRR_MODE_ID: u32 = 0x1194_F158;         // 2 = fullscreen and windowed
const PROFILE_NAME: &str = "FlowScale";

const UNI: usize = 2048;
#[repr(C)] struct Profile { version: u32, name: [u16; UNI], gpu_support: u32, is_predefined: u32, num_apps: u32, num_settings: u32 }
#[repr(C)] struct Application { version: u32, is_predefined: u32, app_name: [u16; UNI], friendly: [u16; UNI], launcher: [u16; UNI] }
#[repr(C)] struct Setting { version: u32, name: [u16; UNI], id: u32, kind: u32, location: u32, is_current_predefined: u32, is_predefined_valid: u32, predefined: [u32; 1025], current: [u32; 1025] }
fn ver<T>(v: u32) -> u32 { std::mem::size_of::<T>() as u32 | (v << 16) }
fn uni(s: &str) -> [u16; UNI] { let mut b = [0u16; UNI]; for (d, c) in b.iter_mut().zip(s.encode_utf16()) { *d = c; } b }

type Qi = unsafe extern "C" fn(u32) -> *const c_void;
struct Api { qi: Qi }
impl Api {
    fn load() -> Option<Api> {
        unsafe {
            let dll = LoadLibraryW(if cfg!(target_pointer_width = "64") { w!("nvapi64.dll") } else { w!("nvapi.dll") }).ok()?;
            let qi: Qi = std::mem::transmute(GetProcAddress(dll, s!("nvapi_QueryInterface"))?);
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute((qi)(0x0150_E828).as_ref()?);
            (init() == 0).then_some(Api { qi })
        }
    }
    unsafe fn f<T: Copy>(&self, id: u32) -> Option<T> {
        let p = unsafe { (self.qi)(id) };
        if p.is_null() { None } else { Some(unsafe { std::mem::transmute_copy(&p) }) }
    }
}

/// True when an NVIDIA driver with NVAPI is present.
pub fn available() -> bool { static A: OnceLock<bool> = OnceLock::new(); *A.get_or_init(|| Api::load().is_some()) }

fn exe_name() -> String { std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase())).unwrap_or_else(|| "flowscale.exe".into()) }

/// Allow (`on`) or stop allowing G-SYNC for FlowScale's window. Runs on a worker thread; failures are logged.
pub fn ensure(on: bool) {
    // Turning it on is idempotent at the driver level; write it once per run, not on every scaling start.
    static WRITTEN_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !available() || (on && WRITTEN_ON.swap(true, std::sync::atomic::Ordering::Relaxed)) { return; }
    if !on { WRITTEN_ON.store(false, std::sync::atomic::Ordering::Relaxed); }
    std::thread::spawn(move || match unsafe { apply(on) } { Ok(()) => crate::log(&format!("gsync: profile {}", if on { "allows VRR in windowed mode" } else { "restored" })), Err(e) => crate::log(&format!("gsync: {e}")) });
}

unsafe fn apply(on: bool) -> Result<(), String> {
    type H = *mut c_void;
    let api = Api::load().ok_or("NVAPI unavailable")?;
    unsafe {
        let create: unsafe extern "C" fn(*mut H) -> i32 = api.f(0x0694_D52E).ok_or("CreateSession")?;
        let destroy: unsafe extern "C" fn(H) -> i32 = api.f(0xDAD9_CFF8).ok_or("DestroySession")?;
        let load: unsafe extern "C" fn(H) -> i32 = api.f(0x375D_BD6B).ok_or("LoadSettings")?;
        let save: unsafe extern "C" fn(H) -> i32 = api.f(0xFCBC_7E14).ok_or("SaveSettings")?;
        let find_app: unsafe extern "C" fn(H, *const u16, *mut H, *mut Application) -> i32 = api.f(0xEEE5_66B2).ok_or("FindApplicationByName")?;
        let find_profile: unsafe extern "C" fn(H, *const u16, *mut H) -> i32 = api.f(0x7E4A_9A0B).ok_or("FindProfileByName")?;
        let create_profile: unsafe extern "C" fn(H, *const Profile, *mut H) -> i32 = api.f(0xCC17_6068).ok_or("CreateProfile")?;
        let create_app: unsafe extern "C" fn(H, H, *mut Application) -> i32 = api.f(0x4347_A9DE).ok_or("CreateApplication")?;
        let set: unsafe extern "C" fn(H, H, *const Setting) -> i32 = api.f(0x577D_D202).ok_or("SetSetting")?;
        let delete: unsafe extern "C" fn(H, H, u32) -> i32 = api.f(0xE4A2_6362).ok_or("DeleteProfileSetting")?;

        let mut sess: H = std::ptr::null_mut();
        if create(&mut sess) != 0 { return Err("could not open a driver settings session".into()); }
        let r = (|| -> Result<(), String> {
            if load(sess) != 0 { return Err("could not load driver settings".into()); }
            let exe = uni(&exe_name());
            let mut prof: H = std::ptr::null_mut();
            let mut app: Box<Application> = Box::new(std::mem::zeroed()); app.version = ver::<Application>(1);
            let found = find_app(sess, exe.as_ptr(), &mut prof, &mut *app) == 0;
            if !on {
                if found { let _ = delete(sess, prof, VRR_MODE_ID); let _ = delete(sess, prof, VRR_APP_OVERRIDE_ID); }
                return if save(sess) == 0 { Ok(()) } else { Err("could not save driver settings".into()) };
            }
            if !found {
                if find_profile(sess, uni(PROFILE_NAME).as_ptr(), &mut prof) != 0 {
                    let mut p: Box<Profile> = Box::new(std::mem::zeroed());
                    p.version = ver::<Profile>(1); p.name = uni(PROFILE_NAME);
                    if create_profile(sess, &*p, &mut prof) != 0 { return Err("could not create the FlowScale driver profile".into()); }
                }
                let mut a: Box<Application> = Box::new(std::mem::zeroed());
                a.version = ver::<Application>(1); a.app_name = exe; a.friendly = uni("FlowScale");
                if create_app(sess, prof, &mut *a) != 0 { return Err("could not add flowscale.exe to its driver profile".into()); }
            }
            for (id, value) in [(VRR_MODE_ID, 2u32), (VRR_APP_OVERRIDE_ID, 0u32)] {
                let mut st: Box<Setting> = Box::new(std::mem::zeroed());
                st.version = ver::<Setting>(1); st.id = id; st.kind = 0; st.current[0] = value;
                if set(sess, prof, &*st) != 0 { return Err(format!("could not set driver setting {id:#x}")); }
            }
            if save(sess) == 0 { Ok(()) } else { Err("could not save driver settings".into()) }
        })();
        let _ = destroy(sess);
        r
    }
}

/// Read-only report for `flowscale.exe -gsync status`: is NVAPI there, and does FlowScale's profile allow VRR?
pub fn status() -> String {
    type H = *mut c_void;
    let Some(api) = Api::load() else { return "NVAPI not available (no NVIDIA driver)".into() };
    unsafe {
        let (Some(create), Some(destroy), Some(load), Some(find_app), Some(get)) = (
            api.f::<unsafe extern "C" fn(*mut H) -> i32>(0x0694_D52E), api.f::<unsafe extern "C" fn(H) -> i32>(0xDAD9_CFF8),
            api.f::<unsafe extern "C" fn(H) -> i32>(0x375D_BD6B), api.f::<unsafe extern "C" fn(H, *const u16, *mut H, *mut Application) -> i32>(0xEEE5_66B2),
            api.f::<unsafe extern "C" fn(H, H, u32, *mut Setting) -> i32>(0x73BF_8338)) else { return "NVAPI DRS entry points missing".into() };
        let mut sess: H = std::ptr::null_mut();
        if create(&mut sess) != 0 { return "could not open a driver settings session".into(); }
        let out = if load(sess) != 0 { "could not load driver settings".to_string() } else {
            let mut prof: H = std::ptr::null_mut();
            let mut app: Box<Application> = Box::new(std::mem::zeroed()); app.version = ver::<Application>(1);
            if find_app(sess, uni(&exe_name()).as_ptr(), &mut prof, &mut *app) != 0 { format!("NVAPI ok; {} has no driver profile yet (G-SYNC support off)", exe_name()) } else {
                let mut st: Box<Setting> = Box::new(std::mem::zeroed()); st.version = ver::<Setting>(1);
                let mode = if get(sess, prof, VRR_MODE_ID, &mut *st) == 0 { st.current[0] as i64 } else { -1 };
                format!("NVAPI ok; {} is in a driver profile, VRR mode = {}", exe_name(), match mode { 2 => "fullscreen and windowed", 1 => "fullscreen only", 0 => "off", _ => "not set" })
            }
        };
        let _ = destroy(sess);
        out
    }
}
