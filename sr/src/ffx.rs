#![allow(non_camel_case_types, dead_code)]
use std::ffi::{c_char, c_void};

pub type Context = *mut c_void;
pub type ReturnCode = u32;
pub type StructType = u64;

pub const RETURN_OK: u32 = 0;
pub const RETURN_ERROR: u32 = 1;
pub const RETURN_ERROR_UNKNOWN_DESCTYPE: u32 = 2;
pub const RETURN_ERROR_RUNTIME_ERROR: u32 = 3;
pub const RETURN_NO_PROVIDER: u32 = 4;
pub const RETURN_ERROR_MEMORY: u32 = 5;
pub const RETURN_ERROR_PARAMETER: u32 = 6;

pub const QUERY_GET_VERSIONS: u64 = 4;
pub const DESC_OVERRIDE_VERSION: u64 = 5;
pub const QUERY_GET_PROVIDER_VERSION: u64 = 6;
pub const CREATE_BACKEND_DX12: u64 = 2;
pub const CREATE_BACKEND_VK: u64 = 3;
pub const CREATE_UPSCALE: u64 = 0x0001_0000;
pub const DISPATCH_UPSCALE: u64 = 0x0001_0001;
pub const QUERY_RATIO_FROM_QUALITY: u64 = 0x0001_0002;
pub const QUERY_RENDER_RES_FROM_QUALITY: u64 = 0x0001_0003;
pub const QUERY_JITTER_PHASE_COUNT: u64 = 0x0001_0004;
pub const QUERY_JITTER_OFFSET: u64 = 0x0001_0005;
pub const DISPATCH_GENERATE_REACTIVE: u64 = 0x0001_0006;
pub const CONFIGURE_UPSCALE_KEYVALUE: u64 = 0x0001_0007;
pub const QUERY_GPU_MEMORY_USAGE: u64 = 0x0001_0008;

pub const KEY_VELOCITY_FACTOR: u64 = 0;
pub const KEY_REACTIVENESS_SCALE: u64 = 1;

pub const ENABLE_HDR: u32 = 1 << 0;
pub const ENABLE_DISPLAY_RES_MV: u32 = 1 << 1;
pub const ENABLE_MV_JITTER_CANCELLATION: u32 = 1 << 2;
pub const ENABLE_DEPTH_INVERTED: u32 = 1 << 3;
pub const ENABLE_DEPTH_INFINITE: u32 = 1 << 4;
pub const ENABLE_AUTO_EXPOSURE: u32 = 1 << 5;

pub const QUALITY_NATIVEAA: u32 = 0;
pub const QUALITY_QUALITY: u32 = 1;
pub const QUALITY_BALANCED: u32 = 2;
pub const QUALITY_PERFORMANCE: u32 = 3;
pub const QUALITY_ULTRA_PERFORMANCE: u32 = 4;

pub const STATE_COMMON: u32 = 1 << 0;
pub const STATE_UNORDERED_ACCESS: u32 = 1 << 1;
pub const STATE_COMPUTE_READ: u32 = 1 << 2;
pub const STATE_PIXEL_READ: u32 = 1 << 3;
pub const STATE_PIXEL_COMPUTE_READ: u32 = STATE_PIXEL_READ | STATE_COMPUTE_READ;
pub const STATE_COPY_SRC: u32 = 1 << 4;
pub const STATE_COPY_DEST: u32 = 1 << 5;
pub const STATE_GENERIC_READ: u32 = STATE_COPY_SRC | STATE_COMPUTE_READ;
pub const STATE_INDIRECT_ARGUMENT: u32 = 1 << 6;
pub const STATE_PRESENT: u32 = 1 << 7;
pub const STATE_RENDER_TARGET: u32 = 1 << 8;

#[repr(C)]
pub struct Header { pub ty: StructType, pub next: *mut Header }

#[repr(C)]
pub struct AllocationCallbacks { pub user: *mut c_void, pub alloc: *const c_void, pub dealloc: *const c_void }

#[repr(C)] #[derive(Clone, Copy, Default)]
pub struct Dim2 { pub width: u32, pub height: u32 }
#[repr(C)] #[derive(Clone, Copy, Default)]
pub struct Float2 { pub x: f32, pub y: f32 }

#[repr(C)] #[derive(Clone, Copy)]
pub struct ResourceDescription { pub ty: u32, pub format: u32, pub width: u32, pub height: u32, pub depth: u32, pub mip_count: u32, pub flags: u32, pub usage: u32 }
#[repr(C)] #[derive(Clone, Copy)]
pub struct Resource { pub resource: *mut c_void, pub description: ResourceDescription, pub state: u32 }

#[repr(C)]
pub struct CreateUpscale { pub header: Header, pub flags: u32, pub max_render: Dim2, pub max_upscale: Dim2, pub fp_message: *const c_void }

#[repr(C)]
pub struct BackendDx12 { pub header: Header, pub device: *mut c_void }

#[repr(C)]
pub struct OverrideVersion { pub header: Header, pub version_id: u64 }

#[repr(C)] #[derive(Clone, Copy)]
pub struct DispatchUpscale {
    pub header_ty: StructType, pub header_next: *mut Header,
    pub command_list: *mut c_void,
    pub color: Resource, pub depth: Resource, pub motion_vectors: Resource, pub exposure: Resource, pub reactive: Resource,
    pub transparency: Resource, pub output: Resource,
    pub jitter: Float2, pub mv_scale: Float2, pub render_size: Dim2, pub upscale_size: Dim2,
    pub enable_sharpening: bool, pub sharpness: f32, pub frame_time_delta: f32, pub pre_exposure: f32, pub reset: bool,
    pub camera_near: f32, pub camera_far: f32, pub camera_fov_y: f32, pub view_to_meters: f32, pub flags: u32,
}

#[repr(C)]
pub struct QueryRatio { pub header: Header, pub quality: u32, pub out_ratio: *mut f32 }
#[repr(C)]
pub struct QueryRenderRes { pub header: Header, pub display_w: u32, pub display_h: u32, pub quality: u32, pub out_w: *mut u32, pub out_h: *mut u32 }
#[repr(C)]
pub struct QueryPhaseCount { pub header: Header, pub render_w: u32, pub display_w: u32, pub out_count: *mut i32 }
#[repr(C)]
pub struct QueryJitterOffset { pub header: Header, pub index: i32, pub phase_count: i32, pub out_x: *mut f32, pub out_y: *mut f32 }
#[repr(C)] #[derive(Clone, Copy)]
pub struct QueryVersions { pub header_ty: StructType, pub header_next: *mut Header, pub create_desc_type: u64, pub device: *mut c_void, pub output_count: *mut u64, pub version_ids: *mut u64, pub version_names: *mut *const c_char }
#[repr(C)]
pub struct QueryProviderVersion { pub header: Header, pub version_id: u64, pub version_name: *const c_char }
#[repr(C)]
pub struct MemoryUsage { pub total: u64, pub aliasable: u64 }
#[repr(C)]
pub struct QueryMemory { pub header: Header, pub usage: *mut MemoryUsage }
#[repr(C)]
pub struct ConfigureKeyValue { pub header: Header, pub key: u64, pub u64v: u64, pub ptr: *mut c_void }

pub type PfnCreate = unsafe extern "C" fn(*mut Context, *mut Header, *const AllocationCallbacks) -> ReturnCode;
pub type PfnDestroy = unsafe extern "C" fn(*mut Context, *const AllocationCallbacks) -> ReturnCode;
pub type PfnConfigure = unsafe extern "C" fn(*mut Context, *const Header) -> ReturnCode;
pub type PfnQuery = unsafe extern "C" fn(*mut Context, *mut Header) -> ReturnCode;
pub type PfnDispatch = unsafe extern "C" fn(*mut Context, *const Header) -> ReturnCode;

pub unsafe fn find(mut h: *const Header, ty: StructType) -> *const Header {
    unsafe {
        while !h.is_null() { if (*h).ty == ty { return h; } h = (*h).next; }
    }
    std::ptr::null()
}

const _: () = assert!(std::mem::size_of::<Resource>() == 48);
const _: () = assert!(std::mem::size_of::<DispatchUpscale>() == 24 + 7 * 48 + 32 + 40);
