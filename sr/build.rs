use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: &[(&str, &str, &str, &[&str])] = &[
    ("convert", "convert.hlsl", "cs_6_0", &[]),
    ("motion", "motion.hlsl", "cs_6_0", &[]),
    ("upscale", "upscale.hlsl", "cs_6_0", &[]),
    ("upscale_as", "upscale.hlsl", "cs_6_0", &["ANTI_SHIMMER=1"]),
    ("upscale_fm", "upscale.hlsl", "cs_6_0", &["FUSED_MOTION=1"]),
    ("upscale_as_fm", "upscale.hlsl", "cs_6_0", &["FUSED_MOTION=1", "ANTI_SHIMMER=1"]),
    ("upscale16", "upscale.hlsl", "cs_6_2", &["DITTO_FP16=1"]),
    ("upscale16_as", "upscale.hlsl", "cs_6_2", &["DITTO_FP16=1", "ANTI_SHIMMER=1"]),
    ("upscale16_fm", "upscale.hlsl", "cs_6_2", &["DITTO_FP16=1", "FUSED_MOTION=1"]),
    ("upscale16_as_fm", "upscale.hlsl", "cs_6_2", &["DITTO_FP16=1", "FUSED_MOTION=1", "ANTI_SHIMMER=1"]),
    ("sharpen", "sharpen.hlsl", "cs_6_0", &[]),
    ("sharpen16", "sharpen.hlsl", "cs_6_2", &["DITTO_FP16=1"]),
    ("aoprep", "aoprep.hlsl", "cs_6_0", &[]),
    ("aomain", "aomain.hlsl", "cs_6_0", &[]),
    ("aofilter", "aofilter.hlsl", "cs_6_0", &[]),
    ("aotemporal", "aotemporal.hlsl", "cs_6_0", &[]),
    ("aocompose", "aocompose.hlsl", "cs_6_0", &[]),
];

const XESS_FORWARDS: &[&str] = &[
    "INTC_D3D11_XessCreateContext",
    "INTC_D3D11_XessDestroyContext",
    "INTC_D3D11_XessExecute",
    "INTC_D3D11_XessForceLegacyScaleFactors",
    "INTC_D3D11_XessGetExposureMultiplier",
    "INTC_D3D11_XessGetInitParams",
    "INTC_D3D11_XessGetInputResolution",
    "INTC_D3D11_XessGetIntelXeFXVersion",
    "INTC_D3D11_XessGetJitterScale",
    "INTC_D3D11_XessGetOptimalInputResolution",
    "INTC_D3D11_XessGetProfilingData",
    "INTC_D3D11_XessGetVelocityScale",
    "INTC_D3D11_XessGetVersion",
    "INTC_D3D11_XessInit",
    "INTC_D3D11_XessIsOptimalDriver",
    "INTC_D3D11_XessSelectNetworkModel",
    "INTC_D3D11_XessSetExposureMultiplier",
    "INTC_D3D11_XessSetJitterScale",
    "INTC_D3D11_XessSetLoggingCallback",
    "INTC_D3D11_XessSetVelocityScale",
    "INTC_D3D12_GetApplicationInfo",
    "INTC_D3D12_GetGpuCrashDumpBufferInfo",
    "INTC_D3D12_GpuCrashDumpOnCommandListCreate",
    "INTC_D3D12_GpuCrashDumpOnCommandListDestroy",
    "INTC_D3D12_GpuCrashDumpOnDeviceDestroy",
    "INTC_D3D12_RegisterDriverCallbacks",
    "xessAILGetDecision",
    "xessAILGetVersion",
    "xessD3D12BuildPipelines",
    "xessD3D12GetInitParams",
    "xessD3D12GetProfilingData",
    "xessD3D12GetResourcesToDump",
    "xessForceLegacyScaleFactors",
    "xessGetContextParameterP",
    "xessGetExposureMultiplier",
    "xessGetInputResolution",
    "xessGetIntelXeFXVersion",
    "xessGetJitterScale",
    "xessGetMaxResponsiveMaskValue",
    "xessGetOptimalInputResolution",
    "xessGetPipelineBuildStatus",
    "xessGetProfilingData",
    "xessGetProperties",
    "xessGetVelocityScale",
    "xessGetVersion",
    "xessIsOptimalDriver",
    "xessSelectNetworkModel",
    "xessSetContextParameterF",
    "xessSetLoggingCallback",
    "xessStartDump",
    "xessTrinityCreateContext",
    "xessTrinityGetRequiredDeviceExtensions",
    "xessTrinityGetRequiredDeviceFeatures",
    "xessTrinityGetRequiredInstanceExtensions",
    "xessVKAILGetDecision",
    "xessVKBuildPipelines",
    "xessVKCreateContext",
    "xessVKExecute",
    "xessVKGetInitParams",
    "xessVKGetRequiredDeviceExtensions",
    "xessVKGetRequiredDeviceFeatures",
    "xessVKGetRequiredInstanceExtensions",
    "xessVKGetResourcesToDump",
    "xessVKInit",
];

fn dxc() -> PathBuf {
    if let Ok(p) = std::env::var("DXC") { return PathBuf::from(p); }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut vers: Vec<PathBuf> = std::fs::read_dir(kits).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("x64").join("dxc.exe").exists()).collect()).unwrap_or_default();
    vers.sort();
    vers.pop().map(|p| p.join("x64").join("dxc.exe")).unwrap_or_else(|| PathBuf::from("dxc.exe"))
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("shaders");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=shaders");
    let dxc = dxc();
    let mut rs = String::new();
    for (name, file, profile, defs) in SHADERS {
        let dst = out.join(format!("{name}.dxil"));
        let mut c = Command::new(&dxc);
        c.current_dir(&dir).args(["-nologo", "-T", profile, "-E", "main", "-O3", "-Qstrip_debug", "-Qstrip_reflect", "-Fo"]).arg(&dst);
        if profile.ends_with("6_2") { c.arg("-enable-16bit-types"); }
        for d in *defs { c.arg("-D").arg(d); }
        c.arg(file);
        let o = c.output().unwrap_or_else(|e| panic!("dxc not runnable ({}): {e}", dxc.display()));
        if !o.status.success() { panic!("dxc {name}: {}", String::from_utf8_lossy(&o.stderr)); }
        rs += &format!("pub const {}: &[u8] = include_bytes!(r\"{}\");\n", name.to_uppercase(), dst.display());
    }
    std::fs::write(out.join("shaders.rs"), rs).unwrap();
    let mut t = String::from("pub const NAMES: &[&[u8]] = &[");
    for n in XESS_FORWARDS { t += &format!("b\"{n}\0\", "); }
    t += "];\npub static mut SLOTS: [usize; NAMES.len()] = [0; NAMES.len()];\n";
    let asm = ["test rax, rax", "jz 2f", "jmp rax", "2:", "push rcx", "push rdx", "push r8", "push r9", "sub rsp, 0x68",
        "movdqu [rsp+0x20], xmm0", "movdqu [rsp+0x30], xmm1", "movdqu [rsp+0x40], xmm2", "movdqu [rsp+0x50], xmm3"];
    let tail = ["movdqu xmm0, [rsp+0x20]", "movdqu xmm1, [rsp+0x30]", "movdqu xmm2, [rsp+0x40]", "movdqu xmm3, [rsp+0x50]",
        "add rsp, 0x68", "pop r9", "pop r8", "pop rdx", "pop rcx", "jmp rax"];
    let q = |v: &[&str]| v.iter().map(|x| format!("\"{x}\", ")).collect::<String>();
    for (i, n) in XESS_FORWARDS.iter().enumerate() {
        t += &format!("#[unsafe(naked)]\n#[unsafe(no_mangle)]\npub unsafe extern \"C\" fn {n}() {{ core::arch::naked_asm!(\"mov rax, qword ptr [rip + {{s}} + {}]\", {}\"mov ecx, {i}\", \"call {{r}}\", {}s = sym SLOTS, r = sym super::resolve_forward) }}\n",
            i * 8, q(&asm), q(&tail));
    }
    std::fs::write(out.join("thunks.rs"), t).unwrap();
}
