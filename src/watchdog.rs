//! UI watchdog: when the window thread stops answering for 4 s, its call stack goes to the log (function names when
//! flowscale.pdb is found), once per hang. A frozen app then leaves the evidence of where it froze.
use std::ffi::c_void;

use windows::Win32::Foundation::*;
#[cfg(target_arch = "x86_64")]
use windows::Win32::System::Diagnostics::Debug::*;
#[cfg(target_arch = "x86_64")]
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// When an exit was asked (QPC-free: milliseconds since start); 0 = none.
pub static EXIT_ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn now_ms() -> u64 { unsafe { windows::Win32::System::SystemInformation::GetTickCount64() } }
pub fn exit_asked() { EXIT_ASKED.store(now_ms(), std::sync::atomic::Ordering::Relaxed); }

#[cfg(target_arch = "x86_64")]
#[repr(C, align(16))]
struct Ctx(CONTEXT);

pub fn start(ui: HWND) {
    let tid = unsafe { GetCurrentThreadId() };
    let h = ui.0 as isize;
    std::thread::spawn(move || {
        let ui = HWND(h as *mut c_void);
        let mut reported = false;
        let mut exit_reported = false;
        // FS_STACK_AT=<seconds>: diagnosis, the window thread's stack logged once at that time after start.
        let stack_at = std::env::var("FS_STACK_AT").ok().and_then(|v| v.parse::<u64>().ok()).map(|v| now_ms() + v * 1000);
        let mut stack_done = false;
        loop {
            if let Some(t) = stack_at { if !stack_done && now_ms() >= t { stack_done = true; crate::log(&format!("watchdog: FS_STACK_AT window thread stack:
{}", stack(tid))); } }
            std::thread::sleep(std::time::Duration::from_secs(2));
            let asked = EXIT_ASKED.load(std::sync::atomic::Ordering::Relaxed);
            if asked != 0 && !exit_reported && now_ms() > asked + 5000 {
                exit_reported = true;
                crate::log(&format!("watchdog: exit asked 5 s ago and still running; window thread stack:\r\n{}", stack(tid)));
            }
            if unsafe { !IsWindow(Some(ui)).as_bool() } { continue; }
            let mut r = 0usize;
            let ok = unsafe { SendMessageTimeoutW(ui, WM_NULL, WPARAM(0), LPARAM(0), SMTO_BLOCK | SMTO_ABORTIFHUNG, 4000, Some(&mut r)) }.0 != 0;
            if ok { if reported { crate::log("watchdog: the window thread answers again"); } reported = false; continue; }
            if reported { continue; }
            reported = true;
            crate::log(&format!("watchdog: the window thread has not answered for 4 s; its stack:\r\n{}", stack(tid)));
        }
    });
}

#[cfg(target_arch = "x86_64")]
unsafe extern "system" fn fta(p: HANDLE, a: u64) -> *mut c_void { unsafe { SymFunctionTableAccess64(p, a) } }
#[cfg(target_arch = "x86_64")]
unsafe extern "system" fn gmb(p: HANDLE, a: u64) -> u64 { unsafe { SymGetModuleBase64(p, a) } }
#[cfg(target_arch = "x86_64")]
fn stack(tid: u32) -> String {
    unsafe {
        let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, false, tid) else { return "  (thread not opened)".into() };
        let proc_ = GetCurrentProcess();
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| { SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS); let _ = SymInitializeW(proc_, None, true); });
        SuspendThread(th);
        let mut c = Box::new(Ctx(CONTEXT { ContextFlags: CONTEXT_FULL_AMD64, ..Default::default() }));
        let mut pcs = [0u64; 48]; let mut n = 0;
        if GetThreadContext(th, &mut c.0).is_ok() {
            let mut f = STACKFRAME64::default();
            f.AddrPC = ADDRESS64 { Offset: c.0.Rip, Segment: 0, Mode: AddrModeFlat };
            f.AddrFrame = ADDRESS64 { Offset: c.0.Rbp, Segment: 0, Mode: AddrModeFlat };
            f.AddrStack = ADDRESS64 { Offset: c.0.Rsp, Segment: 0, Mode: AddrModeFlat };
            while n < pcs.len() {
                if !StackWalk64(IMAGE_FILE_MACHINE_AMD64.0 as u32, proc_, th, &mut f, &mut c.0 as *mut _ as *mut c_void, None, Some(fta), Some(gmb), None).as_bool() || f.AddrPC.Offset == 0 { break; }
                pcs[n] = f.AddrPC.Offset; n += 1;
            }
        }
        ResumeThread(th);
        let _ = CloseHandle(th);
        // Names only after the thread runs again: it may hold the heap lock that formatting needs.
        let mut out = String::new();
        for &pc in &pcs[..n] { out += &format!("  {}\r\n", name(proc_, pc)); }
        if out.is_empty() { "  (no frames)".into() } else { out }
    }
}
#[cfg(not(target_arch = "x86_64"))]
fn stack(_tid: u32) -> String { "  (stack walk on 64-bit builds only)".into() }

#[cfg(target_arch = "x86_64")]
unsafe fn name(p: HANDLE, addr: u64) -> String {
    #[repr(C)] struct Sym { info: SYMBOL_INFO, more: [u8; 512] }
    let mut s: Sym = unsafe { std::mem::zeroed() };
    s.info.SizeOfStruct = std::mem::size_of::<SYMBOL_INFO>() as u32; s.info.MaxNameLen = 512;
    let mut disp = 0u64;
    let module = {
        let mut m = IMAGEHLP_MODULE64 { SizeOfStruct: std::mem::size_of::<IMAGEHLP_MODULE64>() as u32, ..Default::default() };
        if unsafe { SymGetModuleInfo64(p, addr, &mut m) }.is_ok() { let n = m.ModuleName.iter().position(|&c| c == 0).unwrap_or(32); String::from_utf8_lossy(&m.ModuleName[..n].iter().map(|&c| c as u8).collect::<Vec<_>>()).to_string() } else { "?".into() }
    };
    if unsafe { SymFromAddr(p, addr, Some(&mut disp), &mut s.info) }.is_ok() {
        let n = s.info.NameLen.min(512) as usize;
        let name = unsafe { std::slice::from_raw_parts(s.info.Name.as_ptr() as *const u8, n) };
        format!("{module}!{}+0x{disp:x}", String::from_utf8_lossy(name))
    } else { format!("{module}+0x{addr:x}") }
}
