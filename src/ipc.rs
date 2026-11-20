use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::*;

pub const MAGIC: u32 = 0x5049_5346;
pub const VERSION: u32 = 3;
pub const RING: usize = 2048;

pub const API_NONE: u32 = 0; pub const API_D3D11: u32 = 1; pub const API_D3D12: u32 = 2; pub const API_D3D9: u32 = 3; pub const API_GL: u32 = 4; pub const API_VK: u32 = 5;
pub const MODE_SMOOTH: u32 = 0; pub const MODE_LOW_LATENCY: u32 = 1;

#[repr(C)]
pub struct Shared {
    pub magic: AtomicU32, pub version: AtomicU32, pub size: AtomicU32,
    pub owner_pid: AtomicU32,
    pub enabled: AtomicU32,
    pub milli_fps: AtomicU32,
    pub mode: AtomicU32,
    pub target_pid: AtomicU32,
    pub attached_pid: AtomicU32,
    pub api: AtomicU32,
    pub width: AtomicU32, pub height: AtomicU32,
    pub spin_us: AtomicU32,
    pub late_frames: AtomicU32,
    pub heartbeat: AtomicU64,
    pub write_idx: AtomicU32,
    pub frame_us: [AtomicU32; RING],
}

pub struct Mapping { pub ptr: *const Shared, handle: HANDLE }
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}
impl Mapping {
    pub fn get(&self) -> &Shared { unsafe { &*self.ptr } }

    pub fn create() -> Option<Mapping> {
        unsafe {
            let size = std::mem::size_of::<Shared>() as u32;
            let mut sd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
            let _ = windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:(A;;GA;;;WD)(A;;GA;;;AC)S:(ML;;NW;;;LW)"), 1, &mut sd, None);
            let sa = windows::Win32::Security::SECURITY_ATTRIBUTES { nLength: size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, if sd.0.is_null() { None } else { Some(&sa) }, PAGE_READWRITE, 0, size, w!("Local\\FlowScale_Limiter_v3")).ok()?;
            let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, size as usize);
            if v.Value.is_null() { return None; }
            let m = Mapping { ptr: v.Value as *const Shared, handle: h };
            let s = m.get();
            s.size.store(size, Relaxed); s.version.store(VERSION, Relaxed); s.owner_pid.store(std::process::id(), Relaxed);
            s.magic.store(MAGIC, Relaxed);
            Some(m)
        }
    }
    pub fn open() -> Option<Mapping> {
        unsafe {
            let size = std::mem::size_of::<Shared>();
            let h = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, w!("Local\\FlowScale_Limiter_v3")).ok()?;
            let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, size);
            if v.Value.is_null() { return None; }
            let m = Mapping { ptr: v.Value as *const Shared, handle: h };
            let s = m.get();
            if s.magic.load(Relaxed) != MAGIC || s.version.load(Relaxed) != VERSION || s.size.load(Relaxed) as usize != size { return None; }
            Some(m)
        }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) { unsafe { let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut _ }); let _ = windows::Win32::Foundation::CloseHandle(self.handle); } }
}

impl Shared {
    pub fn push_frame(&self, us: u32) {
        let i = self.write_idx.load(Relaxed) as usize;
        self.frame_us[i % RING].store(us, Relaxed);
        self.write_idx.store(i as u32 + 1, Relaxed);
    }
    pub fn read_since(&self, since: u32, out: &mut Vec<f32>) -> u32 {
        let end = self.write_idx.load(Relaxed);
        let start = if end.wrapping_sub(since) as usize > RING { end.wrapping_sub(RING as u32) } else { since };
        let mut i = start;
        while i != end { out.push(self.frame_us[i as usize % RING].load(Relaxed) as f32 / 1000.0); i = i.wrapping_add(1); }
        end
    }
}
