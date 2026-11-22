use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::System::Registry::*;

use crate::limiter::exe_dir;

pub const SR_DLL: &str = "flowscale_sr.dll";
const BACKUP: &str = "flowscale-backup";
const MANIFEST: &str = "manifest.txt";
const FFX_DIRECT: &[&str] = &["amd_fidelityfx_dx12.dll", "amd_fidelityfx_loader_dx12.dll"];
const XESS: &str = "libxess.dll";
const DETECT: &[(&str, &str)] = &[
    ("amd_fidelityfx_dx12.dll", "FSR 3.1"), ("amd_fidelityfx_loader_dx12.dll", "FSR 3.1+"), ("amd_fidelityfx_upscaler_dx12.dll", "FSR 4"),
    ("amd_fidelityfx_vk.dll", "FSR 3.1 Vulkan"), ("amd_fidelityfx_loader_vk.dll", "FSR 3.1+ Vulkan"), ("ffx_fsr2_api_x64.dll", "FSR 2"), ("ffx_fsr2_api_dx12_x64.dll", "FSR 2"),
    ("nvngx_dlss.dll", "DLSS"), ("nvngx_dlssg.dll", "DLSS FG"), ("libxess.dll", "XeSS"), ("sl.interposer.dll", "Streamline"),
];
const ANTICHEAT: &[&str] = &["easyanticheat", "battleye", "beservice", "eac_launcher", "vgk", "faceit", "ricochet", "equ8", "anticheatexpert", "mhyprot"];

#[derive(Clone)]
pub struct Game { pub name: String, pub store: &'static str, pub root: PathBuf, pub exe_dir: PathBuf, pub has: Vec<String>, pub ffx: Vec<PathBuf>, pub xess: Vec<PathBuf>, pub opti: Vec<PathBuf>, pub anticheat: bool, pub state: String }
impl Game {
    pub fn json(&self) -> Value {
        json!({"name": self.name, "store": self.store, "root": self.root.to_string_lossy(), "exe_dir": self.exe_dir.to_string_lossy(),
               "has": self.has, "direct": !self.ffx.is_empty() || !self.xess.is_empty() || !self.opti.is_empty(), "optiscaler": !self.opti.is_empty(), "anticheat": self.anticheat, "state": self.state})
    }
}

fn reg_str(root: HKEY, sub: &str, val: &str) -> Option<String> {
    unsafe {
        let mut k = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(sub), None, KEY_READ, &mut k).is_err() { return None; }
        let mut buf = [0u16; 1024]; let mut n = (buf.len() * 2) as u32;
        let r = RegQueryValueExW(k, &HSTRING::from(val), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut n));
        let _ = RegCloseKey(k);
        r.ok().ok()?;
        Some(String::from_utf16_lossy(&buf[..(n as usize / 2).min(buf.len())]).trim_end_matches('\0').to_string())
    }
}
fn reg_subkeys(root: HKEY, sub: &str) -> Vec<String> {
    let mut v = vec![];
    unsafe {
        let mut k = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(sub), None, KEY_READ, &mut k).is_err() { return v; }
        for i in 0.. {
            let mut b = [0u16; 256]; let mut n = b.len() as u32;
            if RegEnumKeyExW(k, i, Some(PWSTR(b.as_mut_ptr())), &mut n, None, None, None, None).is_err() { break; }
            v.push(String::from_utf16_lossy(&b[..n as usize]));
        }
        let _ = RegCloseKey(k);
    }
    v
}
fn vdf_values<'a>(txt: &'a str, key: &str) -> Vec<String> {
    let q = format!("\"{key}\"");
    txt.lines().filter_map(|l| { let l = l.trim(); l.strip_prefix(&q).map(|r| r.trim().trim_matches('"').replace("\\\\", "\\")) }).collect()
}

fn candidates() -> Vec<(String, &'static str, PathBuf)> {
    let mut out = vec![];
    if let Some(steam) = reg_str(HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath") {
        let steam = PathBuf::from(steam.replace('/', "\\"));
        let mut libs = vec![steam.clone()];
        if let Ok(t) = std::fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) { libs.extend(vdf_values(&t, "path").into_iter().map(PathBuf::from)); }
        libs.sort(); libs.dedup();
        for lib in libs {
            let apps = lib.join("steamapps");
            for e in std::fs::read_dir(&apps).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if !(n.starts_with("appmanifest_") && n.ends_with(".acf")) { continue; }
                let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
                let (Some(name), Some(dir)) = (vdf_values(&t, "name").pop(), vdf_values(&t, "installdir").pop()) else { continue };
                let p = apps.join("common").join(dir);
                if p.is_dir() && !name.to_lowercase().contains("redistributable") && !name.contains("Steamworks") && !name.contains("Proton") { out.push((name, "Steam", p)); }
            }
        }
    }
    for e in std::fs::read_dir(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests").into_iter().flatten().flatten() {
        if let Ok(v) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).ok_or(()) {
            if let (Some(n), Some(p)) = (v["DisplayName"].as_str(), v["InstallLocation"].as_str()) { if Path::new(p).is_dir() { out.push((n.to_string(), "Epic", PathBuf::from(p))); } }
        }
    }
    for sub in [r"SOFTWARE\WOW6432Node\GOG.com\Games", r"SOFTWARE\GOG.com\Games"] {
        for id in reg_subkeys(HKEY_LOCAL_MACHINE, sub) {
            let k = format!(r"{sub}\{id}");
            if let (Some(n), Some(p)) = (reg_str(HKEY_LOCAL_MACHINE, &k, "gameName"), reg_str(HKEY_LOCAL_MACHINE, &k, "path")) { if Path::new(&p).is_dir() { out.push((n, "GOG", PathBuf::from(p))); } }
        }
    }
    if let Some(ad) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let h = ad.join("heroic");
        if let Some(v) = std::fs::read_to_string(h.join(r"legendaryConfig\legendary\installed.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            for g in v.as_object().into_iter().flat_map(|o| o.values()) {
                if let (Some(n), Some(p)) = (g["title"].as_str(), g["install_path"].as_str()) { if Path::new(p).is_dir() { out.push((n.to_string(), "Heroic", PathBuf::from(p))); } }
            }
        }
        if let Some(v) = std::fs::read_to_string(h.join(r"gog_store\installed.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            for g in v["installed"].as_array().into_iter().flatten() {
                if let Some(p) = g["install_path"].as_str() { if Path::new(p).is_dir() { out.push((Path::new(p).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), "Heroic", PathBuf::from(p))); } }
            }
        }
    }
    for d in b'C'..=b'Z' {
        let x = PathBuf::from(format!("{}:\\XboxGames", d as char));
        for e in std::fs::read_dir(&x).into_iter().flatten().flatten() {
            let c = e.path().join("Content");
            if c.is_dir() { out.push((e.file_name().to_string_lossy().to_string(), "Xbox", c)); }
        }
    }
    for p in extra_folders() { if p.is_dir() { out.push((p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), "Folder", p)); } }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out.dedup_by(|a, b| a.2 == b.2);
    out
}

fn extra_file() -> PathBuf { exe_dir().join("games.txt") }
pub fn extra_folders() -> Vec<PathBuf> { std::fs::read_to_string(extra_file()).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect() }
pub fn add_folder(p: &str) -> Result<(), String> {
    let p = PathBuf::from(p.trim().trim_matches('"'));
    if !p.is_dir() { return Err(format!("not a folder: {}", p.display())); }
    let mut v = extra_folders(); if !v.contains(&p) { v.push(p); }
    std::fs::write(extra_file(), v.iter().map(|x| x.to_string_lossy().to_string()).collect::<Vec<_>>().join("\r\n")).map_err(|e| e.to_string())
}

fn walk(root: &Path, depth: u32, budget: &mut u32, f: &mut dyn FnMut(&Path, &str)) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        if *budget == 0 { return; }
        *budget -= 1;
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_lowercase();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if depth > 0 && n != BACKUP && !n.starts_with('.') && n != "__installer" && n != "_commonredist" && n != "redist" { walk(&p, depth - 1, budget, f); }
            f(&p, &format!("{n}/"));
        } else { f(&p, &n); }
    }
}

fn is_ours(p: &Path) -> bool { std::fs::read(p).map(|b| b.windows(13).any(|w| w == b"FlowScale SR ")).unwrap_or(false) }
fn exports(p: &Path, name: &[u8]) -> bool { std::fs::read(p).map(|b| b.windows(name.len()).any(|w| w == name)).unwrap_or(false) }

pub fn inspect(name: String, store: &'static str, root: PathBuf) -> Game {
    let mut has: Vec<String> = vec![];
    let mut ffx = vec![];
    let mut xess = vec![];
    let mut opti: Vec<PathBuf> = vec![];
    let mut exes: Vec<(PathBuf, u64, i32)> = vec![];
    let mut anticheat = false;
    let mut budget = 60_000;
    walk(&root, 7, &mut budget, &mut |p, n| {
        if let Some((_, k)) = DETECT.iter().find(|(f, _)| *f == n) {
            if !has.iter().any(|h| h == k) { has.push(k.to_string()); }
            if FFX_DIRECT.contains(&n) { ffx.push(p.to_path_buf()); }
            if n == XESS { xess.push(p.to_path_buf()); }
        }
        if n == "optiscaler.ini" { if let Some(d) = p.parent() { opti.push(d.to_path_buf()); } }
        if ANTICHEAT.iter().any(|a| n.contains(a)) { anticheat = true; }
        if n.ends_with(".exe") && !["unins", "crash", "setup", "redist", "vc_redist", "dxsetup", "launcher", "uploader", "report", "helper", "dotnet", "installer", "eac"].iter().any(|x| n.contains(x)) {
            let sz = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let s = p.to_string_lossy().to_lowercase();
            let bonus = if s.contains("\\binaries\\win64\\") { 3 } else if s.contains("\\x64\\") || s.contains("\\bin\\") { 1 } else { 0 };
            exes.push((p.to_path_buf(), sz, bonus));
        }
    });
    exes.sort_by(|a, b| (b.2, b.1).cmp(&(a.2, a.1)));
    let exe_dir = ffx.first().or(xess.first()).and_then(|p| p.parent()).map(|p| p.to_path_buf())
        .or_else(|| exes.first().and_then(|e| e.0.parent().map(|p| p.to_path_buf()))).unwrap_or_else(|| root.clone());
    let state = std::fs::read_to_string(root.join(BACKUP).join(MANIFEST)).ok().and_then(|t| t.lines().next().map(|l| l.trim_start_matches("mode\t").to_string())).unwrap_or_default();
    // OptiScaler's own DLLs (its FSR / XeSS backends beside its ini, its custom providers) are not the game's.
    let in_opti = |p: &PathBuf| opti.iter().any(|o| p.parent() == Some(o.as_path())) || (!opti.is_empty() && p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("custom")));
    ffx.retain(|p| !in_opti(p)); xess.retain(|p| !in_opti(p));
    if !opti.is_empty() && !has.iter().any(|h| h == "OptiScaler") { has.push("OptiScaler".into()); }
    Game { name, store, root, exe_dir, has, ffx, xess, opti, anticheat, state }
}

pub fn scan() -> Vec<Game> { candidates().into_iter().map(|(n, s, p)| inspect(n, s, p)).collect() }

struct Manifest { root: PathBuf, lines: Vec<String> }
impl Manifest {
    fn new(root: &Path, mode: &str) -> Manifest { Manifest { root: root.to_path_buf(), lines: vec![format!("mode\t{mode}")] } }
    fn rel(&self, p: &Path) -> String { p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().to_string() }
    fn add(&mut self, p: &Path) { let r = self.rel(p); self.lines.push(format!("add\t{r}")); }
    fn replaced(&mut self, p: &Path) { let r = self.rel(p); self.lines.push(format!("replace\t{r}")); }
    fn save(&self) -> std::io::Result<()> {
        let d = self.root.join(BACKUP); std::fs::create_dir_all(&d)?;
        std::fs::write(d.join(MANIFEST), self.lines.join("\r\n"))
    }
}

fn sr_source() -> Result<PathBuf, String> {
    let p = exe_dir().join(SR_DLL);
    if p.exists() { Ok(p) } else { Err(format!("{SR_DLL} is missing next to flowscale.exe")) }
}

/// OptiScaler's main folder: [Libraries] OptiDllPath (default "OptiScaler", relative to its ini's folder) when it
/// exists, else the ini's folder - where OptiScaler v10 looks for custom\<name>\ffx (its DirectX 12 providers).
fn opti_main(dir: &Path) -> PathBuf {
    let mut main = String::from("OptiScaler"); let mut sec = String::new();
    for l in std::fs::read_to_string(dir.join("OptiScaler.ini")).unwrap_or_default().lines().map(str::trim) {
        if l.starts_with('[') { sec = l.trim_matches(|c| c == '[' || c == ']').trim().to_lowercase(); continue; }
        if let Some((k, v)) = l.split_once('=') { let v = v.trim().trim_matches('"'); if sec == "libraries" && k.trim().eq_ignore_ascii_case("OptiDllPath") && !v.is_empty() && !v.eq_ignore_ascii_case("auto") { main = v.to_string(); } }
    }
    let p = if Path::new(&main).is_absolute() { PathBuf::from(&main) } else { dir.join(&main) };
    if p.is_dir() { p } else { dir.to_path_buf() }
}

pub fn write_sr_ini(dir: &Path, cfg: &SrConfig) -> std::io::Result<()> {
    let mut s = String::from("; FlowScale SR (written by FlowScale; the game picks changes up live, F9 too)\r\n[sr]\r\nenabled=1\r\n");
    s += &format!("sharpness={:.2}\r\nframe_weight={:.2}\r\nsharpen_mode={}\r\nrelight={}\r\nanti_shimmer={}\r\nfp16={}\r\n",
        cfg.sharpness, cfg.frame_weight, cfg.sharpen_mode, cfg.relight as u8, if cfg.anti_shimmer { "-1" } else { "0" }, cfg.fp16);
    s += &format!("ao_strength={:.2}\r\ngi_strength={:.2}\r\nao_samples={}\r\nao_fade_distance={}\r\n", cfg.ao_strength, cfg.gi_strength, cfg.ao_samples, cfg.fade_m);
    if !cfg.curve { s += "curve=0,0 1,1\r\n"; }
    if cfg.ratio_override > 0.0 { for k in ["ratio_quality", "ratio_balanced", "ratio_performance", "ratio_ultra_performance"] { s += &format!("{k}={:.2}\r\n", cfg.ratio_override); } }
    if cfg.compare { s += "compare_key=0x7A\r\n"; }
    s += &format!("[debug]\r\nenabled={}\r\nindicator={}\r\n", cfg.compare as u8, cfg.indicator as u8);
    std::fs::write(dir.join("flowscale_sr.ini"), s)
}

#[derive(Clone, Copy)]
pub struct SrConfig { pub sharpness: f32, pub frame_weight: f32, pub sharpen_mode: i32, pub relight: bool, pub anti_shimmer: bool, pub fp16: i32, pub ratio_override: f32, pub compare: bool, pub indicator: bool,
    pub ao_strength: f32, pub gi_strength: f32, pub ao_samples: i32, pub curve: bool, pub fade_m: i32 }

pub fn install(g: &Game, cfg: &SrConfig) -> Result<String, String> {
    if g.anticheat { return Err("anti-cheat files found in this game: not modifying it".into()); }
    if !g.state.is_empty() { return Err(format!("already installed ({}); restore first", g.state)); }
    let src = sr_source()?;
    let ffx: Vec<&PathBuf> = g.ffx.iter().filter(|p| exports(p, b"ffxCreateContext") && !is_ours(p)).collect();
    let xess: Vec<&PathBuf> = g.xess.iter().filter(|p| exports(p, b"xessD3D12Execute") && !is_ours(p)).collect();
    if !g.opti.is_empty() && ffx.is_empty() && xess.is_empty() { return install_opti(g, cfg, &src); }
    if ffx.is_empty() && xess.is_empty() { return Err("this game has no DirectX 12 FSR 3.1 or XeSS DLL FlowScale SR can take over".into()); }
    let mut m = Manifest::new(&g.root, "FlowScale SR");
    let bk = g.root.join(BACKUP);
    let mut dirs: Vec<PathBuf> = vec![];
    for (t, orig_name) in ffx.iter().map(|t| (*t, None)).chain(xess.iter().map(|t| (*t, Some("libxess_orig.dll")))) {
        let rel = m.rel(t);
        let b = bk.join(&rel);
        std::fs::create_dir_all(b.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(t, &b).map_err(|e| format!("backup {rel}: {e}"))?;
        let orig = match orig_name { Some(n) => t.with_file_name(n), None => t.with_extension("orig.dll") };
        if orig.exists() { return Err(format!("{} already exists", orig.display())); }
        std::fs::rename(t, &orig).map_err(|e| format!("rename {rel}: {e}"))?;
        m.replaced(t);
        if let Err(e) = std::fs::copy(&src, t) { let _ = std::fs::rename(&orig, t); return Err(format!("copy over {rel}: {e}")); }
        m.add(&orig);
        let d = t.parent().unwrap().to_path_buf();
        if !dirs.contains(&d) {
            let ini = d.join("flowscale_sr.ini");
            if !ini.exists() { write_sr_ini(&d, cfg).map_err(|e| e.to_string())?; m.add(&ini); }
            dirs.push(d);
        }
    }
    m.save().map_err(|e| e.to_string())?;
    let apis = [(!ffx.is_empty(), "FSR 3.1"), (!xess.is_empty(), "XeSS")].iter().filter(|x| x.0).map(|x| x.1).collect::<Vec<_>>().join(" or ");
    Ok(apis)
}

/// OptiScaler v10: FlowScale SR as its custom FidelityFX provider (custom\FlowScaleSR\ffx), the game's
/// files untouched. It then shows in OptiScaler's upscaler list.
fn install_opti(g: &Game, cfg: &SrConfig, src: &Path) -> Result<String, String> {
    let mut m = Manifest::new(&g.root, "FlowScale SR (OptiScaler)");
    for o in &g.opti {
        let base = opti_main(o).join("custom").join("FlowScaleSR");
        if base.exists() { return Err(format!("{} already exists", base.display())); }
        let d = base.join("ffx");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        std::fs::copy(src, d.join("amd_fidelityfx_dx12.dll")).map_err(|e| format!("copy into {}: {e}", d.display()))?;
        write_sr_ini(&d, cfg).map_err(|e| e.to_string())?;
        m.lines.push(format!("adddir\t{}", m.rel(&base)));
    }
    m.save().map_err(|e| e.to_string())?;
    Ok("OptiScaler → FlowScaleSR".into())
}

pub fn restore(g: &Game) -> Result<String, String> {
    let bk = g.root.join(BACKUP);
    let txt = std::fs::read_to_string(bk.join(MANIFEST)).map_err(|_| "nothing installed by FlowScale here".to_string())?;
    let mut errs = vec![];
    for l in txt.lines().rev() {
        let Some((op, rel)) = l.split_once('\t') else { continue };
        let p = g.root.join(rel);
        let r = match op {
            "add" => if p.exists() { std::fs::remove_file(&p) } else { Ok(()) },
            "adddir" => { let r = if p.exists() { std::fs::remove_dir_all(&p) } else { Ok(()) }; if let Some(c) = p.parent() { let _ = std::fs::remove_dir(c); } r }
            "replace" => std::fs::copy(bk.join(rel), &p).map(|_| ()),
            _ => Ok(()),
        };
        if let Err(e) = r { errs.push(format!("{rel}: {e}")); }
    }
    if !errs.is_empty() { return Err(format!("restore incomplete: {}", errs.join("; "))); }
    std::fs::remove_dir_all(&bk).map_err(|e| e.to_string())?;
    Ok(g.name.clone())
}

pub fn apply_settings(games: &[Game], cfg: &SrConfig) -> usize {
    let mut n = 0;
    for g in games.iter().filter(|g| !g.state.is_empty()) {
        let mut budget = 60_000;
        let mut inis = vec![];
        walk(&g.root, 9, &mut budget, &mut |p, name| { if name == "flowscale_sr.ini" { inis.push(p.to_path_buf()); } });
        for i in inis { if write_sr_ini(i.parent().unwrap(), cfg).is_ok() { n += 1; } }
    }
    n
}
