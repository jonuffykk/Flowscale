use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::consts::FrameNums;

pub const MAGIC: u32 = 0x5253_5346;

#[repr(C)]
pub struct Shared { pub magic: u32, pub pid: u32, pub api: u32, pub rw: u32, pub rh: u32, pub ow: u32, pub oh: u32, pub gpu_us: u32, pub frames: u64, pub qpc: i64, pub qpf: i64 }

struct Map(*mut Shared);
unsafe impl Send for Map {}
unsafe impl Sync for Map {}
static MAP: OnceLock<Option<Map>> = OnceLock::new();

fn map() -> Option<&'static Map> {
    MAP.get_or_init(|| unsafe {
        let h = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, size_of::<Shared>() as u32, w!("Local\\FlowScaleSR")).ok()?;
        let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, size_of::<Shared>());
        if v.Value.is_null() { return None; }
        Some(Map(v.Value as *mut Shared))
    }).as_ref()
}

pub fn publish(api: u32, n: &FrameNums, gpu_ms: f32, frames: u64) {
    let Some(m) = map() else { return };
    unsafe {
        let mut qpc = 0i64; let mut qpf = 0i64;
        let _ = QueryPerformanceCounter(&mut qpc); let _ = QueryPerformanceFrequency(&mut qpf);
        std::ptr::write_volatile(m.0, Shared { magic: MAGIC, pid: std::process::id(), api, rw: n.rw, rh: n.rh, ow: n.ow, oh: n.oh,
            gpu_us: (gpu_ms * 1000.0) as u32, frames, qpc, qpf });
    }
}
