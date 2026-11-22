//! Shared memory between FlowScale and the in-game DLL (one mapping, lock-free atomics).
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::*;

pub const MAGIC: u32 = 0x5049_5346;
pub const VERSION: u32 = 5;
pub const RING: usize = 2048;

pub const API_NONE: u32 = 0; pub const API_D3D11: u32 = 1; pub const API_D3D12: u32 = 2; pub const API_D3D9: u32 = 3; pub const API_GL: u32 = 4; pub const API_VK: u32 = 5;
pub const API_D3D8: u32 = 6; pub const API_DDRAW: u32 = 7;
pub const MODE_SMOOTH: u32 = 0; pub const MODE_LOW_LATENCY: u32 = 1;

/// Bits in `hooks`: which presentation paths the DLL installed.
pub const HOOK_DXGI: u32 = 1; pub const HOOK_D3D9: u32 = 2; pub const HOOK_GL: u32 = 4; pub const HOOK_VK: u32 = 8; pub const HOOK_VK_LAYER: u32 = 16;
pub const HOOK_DDRAW: u32 = 32; pub const HOOK_D3D8: u32 = 64;

/// Result of an unload request.
pub const DETACH_NONE: u32 = 0; pub const DETACH_UNLOADED: u32 = 1; pub const DETACH_RESIDENT: u32 = 2; pub const DETACH_FAILED: u32 = 3;

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
    // --- v4: unload, hook inventory and pacing quality
    /// FlowScale asks the DLL in this process to remove its hooks and unload.
    pub detach_pid: AtomicU32,
    pub detach_state: AtomicU32,
    pub hooks: AtomicU32,
    pub arch_bits: AtomicU32,
    /// Sum / count / max of how late the timer woke (before the spin), in microseconds.
    pub wake_us_sum: AtomicU64, pub wake_n: AtomicU32, pub wake_us_max: AtomicU32,
    /// Frames that actually had to wait (the cap was binding) vs. all paced frames.
    pub limited: AtomicU32, pub paced: AtomicU32,
    /// Identity of the one DLL copy that paces this game. Two copies can be loaded at once (an injected one and a
    /// Vulkan-layer one from another folder); only the owner paces, and a silent owner is taken over.
    pub pace_owner: AtomicU64,
    // --- v5: one copy of the DLL hooks a process
    /// Identity of the copy whose hooks are installed in this game: a second copy (another folder, an update, the
    /// Vulkan layer's) never stacks its hooks on top. Stacked inline hooks removed out of order jump into freed code.
    pub hook_owner: AtomicU64,
    /// When a Vulkan or OpenGL frame was last presented (QPC): the DXGI present a driver makes for it is the same frame.
    pub last_vk_qpc: AtomicU64,
    pub write_idx: AtomicU32,
    pub frame_us: [AtomicU32; RING],
}

pub struct Mapping { pub ptr: *const Shared, handle: HANDLE }
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}
const NAME: windows::core::PCWSTR = w!("Local\\FlowScale_Limiter_v5");
impl Mapping {
    pub fn get(&self) -> &Shared { unsafe { &*self.ptr } }

    pub fn create() -> Option<Mapping> {
        unsafe {
            let size = std::mem::size_of::<Shared>() as u32;
            // Everyone and AppContainers (Store / Game Pass games) may open it; low integrity can write.
            let mut sd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
            let _ = windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:(A;;GA;;;WD)(A;;GA;;;AC)S:(ML;;NW;;;LW)"), 1, &mut sd, None);
            let sa = windows::Win32::Security::SECURITY_ATTRIBUTES { nLength: size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
            let h = CreateFileMappingW(INVALID_HANDLE_VALUE, if sd.0.is_null() { None } else { Some(&sa) }, PAGE_READWRITE, 0, size, NAME);
            if !sd.0.is_null() { let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(sd.0))); }
            let h = h.ok()?;
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
            let h = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, NAME).ok()?;
            let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, size);
            if v.Value.is_null() { let _ = windows::Win32::Foundation::CloseHandle(h); return None; }
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
    /// Clears the per-game counters when a new game is targeted.
    pub fn reset_stats(&self) {
        for a in [&self.late_frames, &self.wake_n, &self.wake_us_max, &self.limited, &self.paced, &self.hooks, &self.detach_state] { a.store(0, Relaxed); }
        self.wake_us_sum.store(0, Relaxed); self.pace_owner.store(0, Relaxed);
    }
}
