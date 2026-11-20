use std::path::{Path, PathBuf};
use std::process::Command;

/// Every entry point of src/shaders.hlsl that the engine creates.
const HLSL_ENTRIES: &[(&str, &str)] = &[
    ("VS", "vs_5_0"),
    ("PS_Nearest", "ps_5_0"), ("PS_Bilinear", "ps_5_0"), ("PS_SharpBilinear", "ps_5_0"), ("PS_Bicubic", "ps_5_0"), ("PS_Lanczos", "ps_5_0"),
    ("PS_Easu", "ps_5_0"), ("PS_EasuFast", "ps_5_0"), ("PS_Line", "ps_5_0"), ("PS_Rcas", "ps_5_0"), ("PS_Cas", "ps_5_0"), ("PS_Sgsr", "ps_5_0"),
    ("PS_Xbr", "ps_5_0"), ("PS_Cursor", "ps_5_0"), ("PS_Overlay", "ps_5_0"), ("PS_Rotate", "ps_5_0"),
    ("CS_Dirty", "cs_5_0"), ("CS_Luma", "cs_5_0"), ("CS_Down", "cs_5_0"), ("CS_Flow1", "cs_5_0"), ("CS_Flow2", "cs_5_0"), ("CS_Median", "cs_5_0"), ("CS_Interp", "cs_5_0"),
];

fn main() {
    println!("cargo:rerun-if-changed=ui/dist/index.html");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    compile_shaders(&out);
    copy_webview_loader(&out);
}

/// fxc from the newest installed Windows SDK (override with the FXC environment variable).
fn fxc() -> PathBuf {
    if let Ok(p) = std::env::var("FXC") { return PathBuf::from(p); }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut vers: Vec<PathBuf> = std::fs::read_dir(kits).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("x64").join("fxc.exe").exists()).collect()).unwrap_or_default();
    vers.sort();
    vers.pop().map(|p| p.join("x64").join("fxc.exe")).unwrap_or_else(|| PathBuf::from("fxc.exe"))
}

/// Large shaders (NVIDIA Image Scaling, the Anime4K networks) are compiled at build time so the app never
/// pays seconds of runtime compilation. The small hand-written shaders stay in src/shaders.hlsl.
fn compile_shaders(out: &Path) {
    println!("cargo:rerun-if-changed=src/shaders/anime4k");
    println!("cargo:rerun-if-changed=src/shaders/third_party/nis");
    let fxc = fxc();
    let run = |args: &[&str], src: &Path, dst: &Path| {
        let o = Command::new(&fxc).args(["/nologo", "/O3", "/Qstrip_debug", "/Qstrip_reflect"]).args(args).arg("/Fo").arg(dst).arg(src).output()
            .unwrap_or_else(|e| panic!("fxc not runnable ({}): {e}. Install the Windows SDK or set FXC.", fxc.display()));
        if !o.status.success() { panic!("fxc {}: {}", src.display(), String::from_utf8_lossy(&o.stderr)); }
    };
    let mut rs = String::new();
    let nis_dir = Path::new("src/shaders/third_party/nis");
    for (name, hdr) in [("NIS_SDR", "0"), ("NIS_HDR", "1")] {
        let dst = out.join(format!("{name}.cso"));
        run(&["/T", "cs_5_0", "/E", "main", "/D", "NIS_SCALER=1", "/D", &format!("NIS_HDR_MODE={hdr}"), "/D", "NIS_BLOCK_WIDTH=32", "/D", "NIS_BLOCK_HEIGHT=24",
              "/D", "NIS_THREAD_GROUP_SIZE=256", "/I", nis_dir.to_str().unwrap()], &nis_dir.join("NIS_Main.hlsl"), &dst);
        rs += &format!("pub const {name}: &[u8] = include_bytes!(r\"{}\");\n", dst.display());
    }
    // The hand-written shaders (scalers, frame generation, overlay) are precompiled too: no runtime compiler, instant start.
    println!("cargo:rerun-if-changed=src/shaders.hlsl");
    rs += "pub fn hlsl(entry: &str) -> &'static [u8] {\n    match entry {\n";
    for (entry, profile) in HLSL_ENTRIES {
        let dst = out.join(format!("hlsl_{entry}.cso"));
        run(&["/T", profile, "/E", entry], Path::new("src/shaders.hlsl"), &dst);
        rs += &format!("        \"{entry}\" => include_bytes!(r\"{}\"),\n", dst.display());
    }
    rs += "        _ => &[],\n    }\n}\n";
    rs += "pub fn anime4k(file: &str) -> &'static [u8] {\n    match file {\n";
    let mut files: Vec<PathBuf> = std::fs::read_dir("src/shaders/anime4k").map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "hlsl")).collect()).unwrap_or_default();
    files.sort();
    for f in files {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let dst = out.join(format!("a4k_{stem}.cso"));
        run(&["/T", "ps_5_0", "/E", "main"], &f, &dst);
        rs += &format!("        \"{stem}\" => include_bytes!(r\"{}\"),\n", dst.display());
    }
    rs += "        _ => &[],\n    }\n}\n";
    std::fs::write(out.join("gpu_blobs.rs"), rs).unwrap();
}

fn copy_webview_loader(out: &Path) {
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() { Ok("x86") => "x86", Ok("aarch64") => "arm64", _ => "x64" };
    let home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".cargo"));
    let profile_dir = out.ancestors().nth(3).unwrap().to_path_buf();
    if let Ok(regs) = std::fs::read_dir(home.join("registry").join("src")) {
        for reg in regs.flatten() {
            for c in std::fs::read_dir(reg.path()).into_iter().flatten().flatten() {
                if c.file_name().to_string_lossy().starts_with("webview2-com-sys-0.39") {
                    let dll = c.path().join(arch).join("WebView2Loader.dll");
                    if dll.exists() { let _ = std::fs::copy(&dll, profile_dir.join("WebView2Loader.dll")); return; }
                }
            }
        }
    }
    println!("cargo:warning=WebView2Loader.dll not found; copy it next to flowscale.exe manually");
}
