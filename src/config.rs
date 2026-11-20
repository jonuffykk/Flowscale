//! Single source of truth for every setting: schema, defaults, validation, ini persistence.
//! Profile options live per game (`[exe]` sections); app options live in `[App]`.
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
pub enum Scope { Profile, App }
#[derive(Clone, Copy)]
pub enum Kind { Choice(u8), Range(i32, i32, i32), Toggle, Gpu, Display, Hotkey }

pub struct Opt { pub key: &'static str, pub group: &'static str, pub scope: Scope, pub kind: Kind, pub def: i32 }
const fn p(key: &'static str, group: &'static str, kind: Kind, def: i32) -> Opt { Opt { key, group, scope: Scope::Profile, kind, def } }
const fn a(key: &'static str, group: &'static str, kind: Kind, def: i32) -> Opt { Opt { key, group, scope: Scope::App, kind, def } }

pub const MOD_ALT: i32 = 1; pub const MOD_CTRL: i32 = 2;
pub const fn hk(mods: i32, vk: u8) -> i32 { (mods << 16) | vk as i32 }

/// Labels and hints live in the UI (translated); Rust only owns semantics.
pub static OPTS: &[Opt] = &[
    // Output: where the picture goes
    p("output", "output", Kind::Choice(2), 0),            // 0 fullscreen overlay on the monitor, 1 over the game window (no fullscreen needed)
    p("sync", "output", Kind::Choice(4), 2),
    p("max_latency", "output", Kind::Range(1, 3, 1), 1),
    p("hdr", "output", Kind::Toggle, 0),
    p("capture", "output", Kind::Choice(3), 0),           // 0 Graphics Capture, 1 Desktop Duplication, 2 GDI
    p("queue", "output", Kind::Range(0, 2, 1), 0),        // captured frames kept in flight: 0 newest only (least latency) .. 2 smoothest
    p("gsync", "output", Kind::Toggle, 0),                // NVIDIA: allow G-SYNC on FlowScale's output window
    p("follow_window", "output", Kind::Toggle, 1),        // re-attach when the game re-creates its window
    p("gpu", "output", Kind::Gpu, 0),
    p("display", "output", Kind::Display, 0),
    p("keep_unfocused", "output", Kind::Toggle, 0),
    // Scaling
    p("scaler", "scaling", Kind::Choice(13), 7),
    p("fsr_mode", "scaling", Kind::Choice(2), 0),         // 0 quality, 1 performance (single direction analysis)
    p("a4k_size", "scaling", Kind::Choice(5), 0),         // Anime4K network: S, M, L, VL, UL
    p("sharpen_filter", "scaling", Kind::Choice(2), 0),   // 0 RCAS, 1 CAS
    p("vrs", "scaling", Kind::Toggle, 0),                 // variable rate scaling: redraw only what changed
    p("sharpness", "scaling", Kind::Range(0, 100, 1), 50),
    p("scale_mode", "scaling", Kind::Choice(2), 0),
    p("factor", "scaling", Kind::Range(100, 600, 5), 150),
    p("fit", "scaling", Kind::Choice(2), 0),
    p("resize", "scaling", Kind::Toggle, 0),
    // Frame generation
    p("framegen", "framegen", Kind::Choice(3), 0),
    p("multiplier", "framegen", Kind::Range(2, 8, 1), 2),
    p("target", "framegen", Kind::Range(30, 480, 1), 120),
    p("quality", "framegen", Kind::Choice(3), 0),
    p("flow_scale", "framegen", Kind::Range(25, 100, 5), 50),
    p("ui_threshold", "framegen", Kind::Range(0, 100, 5), 50),
    p("fg_min_fps", "framegen", Kind::Range(0, 60, 1), 10),  // below this game FPS, generation pauses (avoids smearing on hitches)
    p("dedup", "framegen", Kind::Toggle, 1),                 // skip repeated frames (emulators, 30-in-60 games)
    // Limiter
    p("limiter", "limiter", Kind::Toggle, 0),
    p("cap_mode", "limiter", Kind::Choice(5), 2),
    p("fps_cap", "limiter", Kind::Range(10_000, 500_000, 10), 60_000),
    p("latency_mode", "limiter", Kind::Choice(2), 0),
    // Cursor & crop
    p("draw_cursor", "cursor", Kind::Toggle, 1),
    p("scale_cursor", "cursor", Kind::Toggle, 1),
    p("clip_cursor", "cursor", Kind::Toggle, 1),
    p("cursor_speed", "cursor", Kind::Toggle, 0),
    p("hide_cursor", "cursor", Kind::Toggle, 0),
    p("crop_l", "cursor", Kind::Range(0, 400, 1), 0),
    p("crop_t", "cursor", Kind::Range(0, 400, 1), 0),
    p("crop_r", "cursor", Kind::Range(0, 400, 1), 0),
    p("crop_b", "cursor", Kind::Range(0, 400, 1), 0),
    // Automation
    p("auto", "auto", Kind::Toggle, 0),
    p("delay", "auto", Kind::Range(0, 30, 1), 3),
    // In-game overlay (global)
    a("osd", "osd", Kind::Toggle, 1),
    a("osd_detail", "osd", Kind::Choice(3), 1),
    a("osd_pos", "osd", Kind::Choice(4), 0),
    a("osd_theme", "osd", Kind::Choice(2), 0),
    a("osd_scale", "osd", Kind::Range(75, 200, 5), 100),
    a("osd_opacity", "osd", Kind::Range(30, 100, 5), 88),
    a("osd_limiter", "osd", Kind::Toggle, 1),
    a("osd_color", "osd", Kind::Choice(6), 0),
    a("osd_font", "osd", Kind::Choice(3), 0),
    a("osd_x", "osd", Kind::Range(0, 400, 1), 0),
    a("osd_y", "osd", Kind::Range(0, 400, 1), 0),
    // Shortcuts
    a("hk_scale", "keys", Kind::Hotkey, hk(MOD_CTRL | MOD_ALT, b'S')),
    a("hk_limit", "keys", Kind::Hotkey, hk(MOD_CTRL | MOD_ALT, b'L')),
    a("hk_osd", "keys", Kind::Hotkey, hk(MOD_CTRL | MOD_ALT, b'O')),
    // App
    a("auto_detect", "app", Kind::Toggle, 1),
    a("close_to_tray", "app", Kind::Toggle, 1),
    a("start_minimized", "app", Kind::Toggle, 0),
    a("autostart", "app", Kind::Toggle, 0),
    a("start_admin", "app", Kind::Toggle, 0),
    a("theme", "app", Kind::Choice(3), 0),                // 0 follow Windows, 1 light, 2 dark
    // FlowScale SR (written into game folders)
    a("sr_sharpness", "sr", Kind::Range(0, 100, 1), 60),
    a("sr_weight", "sr", Kind::Range(25, 400, 5), 200),
    a("sr_sharpen_mode", "sr", Kind::Choice(2), 0),
    a("sr_fp16", "sr", Kind::Choice(3), 0),
    a("sr_ratio", "sr", Kind::Range(0, 300, 10), 0),
    a("sr_anti_shimmer", "sr", Kind::Toggle, 1),
    a("sr_relight", "sr", Kind::Toggle, 0),
    a("sr_compare", "sr", Kind::Toggle, 0),
    a("sr_indicator", "sr", Kind::Toggle, 0),
];

pub fn opt(k: &str) -> Option<&'static Opt> { OPTS.iter().find(|o| o.key == k) }
fn pidx(k: &str) -> usize { OPTS.iter().filter(|o| o.scope == Scope::Profile).position(|o| o.key == k).expect("profile key") }

/// Clamp a value to what the option allows. Unknown dynamic lists (GPU/display) are clamped by the caller's list size.
pub fn sanitize(o: &Opt, v: i32) -> i32 {
    match o.kind {
        Kind::Choice(n) => v.clamp(0, n as i32 - 1),
        Kind::Range(lo, hi, _) => if o.key == "sr_ratio" && v > 0 && v < 100 { 100 } else { v.clamp(lo, hi) },
        Kind::Toggle => (v != 0) as i32,
        Kind::Gpu | Kind::Display => v.clamp(0, 16),
        Kind::Hotkey => v & 0x000F_00FF,
    }
}

pub type Profile = Vec<i32>;
pub fn defaults() -> Profile { OPTS.iter().filter(|o| o.scope == Scope::Profile).map(|o| o.def).collect() }
pub fn get(p: &Profile, k: &str) -> i32 { p[pidx(k)] }
pub fn set(p: &mut Profile, k: &str, v: i32) { let i = pidx(k); p[i] = v; }

#[derive(Clone)]
pub struct Settings {
    pub windowed: bool, pub scaler: i32, pub sharpness: f32, pub scale_mode: i32, pub factor: f32, pub fit: i32,
    pub framegen: i32, pub multiplier: i32, pub target: i32, pub flow_scale: f32, pub quality: i32, pub ui_threshold: f32,
    pub capture: i32, pub sync: i32, pub max_latency: i32, pub hdr: bool, pub draw_cursor: bool, pub scale_cursor: bool,
    pub clip_cursor: bool, pub cursor_speed: bool, pub resize: bool, pub keep_unfocused: bool, pub gpu: i32, pub display: i32, pub crop: [i32; 4],
    pub fsr_fast: bool, pub sharpen_cas: bool, pub a4k_size: i32, pub queue: u32, pub gsync: bool, pub follow_window: bool,
    pub fg_min_fps: f32, pub dedup: bool, pub hide_cursor: bool, pub vrs: bool,
}
impl Settings {
    pub fn from(p: &Profile) -> Settings {
        let g = |k: &str| get(p, k); let f = |k: &str| g(k) as f32 / 100.0; let b = |k: &str| g(k) != 0;
        let windowed = g("output") == 1;
        Settings { windowed, scaler: g("scaler"), sharpness: f("sharpness"), scale_mode: g("scale_mode"), factor: f("factor"), fit: g("fit"),
            framegen: g("framegen"), multiplier: g("multiplier"), target: g("target"), flow_scale: f("flow_scale"), quality: g("quality"), ui_threshold: f("ui_threshold"),
            capture: g("capture"), sync: g("sync"), max_latency: g("max_latency"), hdr: b("hdr"),
            // Over a window the real system cursor already sits on top and maps 1:1, so never hide/redraw/clip it.
            draw_cursor: b("draw_cursor") && !windowed, scale_cursor: b("scale_cursor"), clip_cursor: b("clip_cursor") && !windowed,
            cursor_speed: b("cursor_speed") && !windowed, resize: b("resize") && !windowed, keep_unfocused: b("keep_unfocused"),
            gpu: g("gpu"), display: g("display"), crop: [g("crop_l"), g("crop_t"), g("crop_r"), g("crop_b")],
            fsr_fast: g("fsr_mode") == 1, sharpen_cas: g("sharpen_filter") == 1, a4k_size: g("a4k_size"), queue: g("queue") as u32,
            gsync: b("gsync"), follow_window: b("follow_window"), fg_min_fps: g("fg_min_fps") as f32, dedup: b("dedup"), hide_cursor: b("hide_cursor"), vrs: b("vrs") }
    }
    pub fn test() -> Settings { Settings::from(&defaults()) }
}

#[derive(Clone, Copy)]
pub struct OsdStyle { pub detail: i32, pub pos: i32, pub light: bool, pub scale: f32, pub opacity: f32, pub color: i32, pub font: i32, pub offset: (f32, f32) }

/// What identifies a game window when picking its profile.
#[derive(Clone, Default)]
pub struct Target { pub exe: String, pub path: String, pub title: String }

/// Case-insensitive glob with `*` (any run) and `?` (one character).
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.to_lowercase().chars().collect(), text.to_lowercase().chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, usize::MAX, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) { pi += 1; ti += 1; }
        else if pi < p.len() && p[pi] == '*' { star = pi; mark = ti; pi += 1; }
        else if star != usize::MAX { pi = star + 1; mark += 1; ti = mark; }
        else { return false; }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    pi == p.len()
}

pub struct Store { pub profiles: BTreeMap<String, Profile>, pub app: BTreeMap<String, i32>, pub matches: BTreeMap<String, String> }
impl Store {
    pub fn app(&self, k: &str) -> i32 { self.app.get(k).copied().or_else(|| opt(k).map(|o| o.def)).unwrap_or(0) }
    pub fn osd_style(&self) -> OsdStyle {
        OsdStyle { detail: self.app("osd_detail"), pos: self.app("osd_pos"), light: self.app("osd_theme") == 1,
            scale: self.app("osd_scale") as f32 / 100.0, opacity: self.app("osd_opacity") as f32 / 100.0,
            color: self.app("osd_color"), font: self.app("osd_font"), offset: (self.app("osd_x") as f32, self.app("osd_y") as f32) }
    }
    /// Profile for a window: its exe name first, then any profile whose match list (exe, full path or title,
    /// several separated by `;`, wildcards allowed) fits the window, otherwise Default.
    pub fn profile_for(&self, t: &Target) -> (String, Profile) {
        let fits = |pat: &str| { let pat = pat.trim(); !pat.is_empty() && (glob(pat, &t.exe) || glob(pat, &t.path) || glob(pat, &t.title)) };
        let k = if self.profiles.contains_key(&t.exe) { t.exe.clone() }
            else { self.matches.iter().find(|(name, m)| self.profiles.contains_key(*name) && m.split(';').any(fits)).map(|(n, _)| n.clone()).unwrap_or_else(|| "Default".into()) };
        (k.clone(), self.profiles[&k].clone())
    }
}

fn ini_path() -> PathBuf { crate::limiter::exe_dir().join("settings.ini") }

pub fn load() -> Store {
    let mut profiles = BTreeMap::new(); profiles.insert("Default".to_string(), defaults());
    let mut app: BTreeMap<String, i32> = OPTS.iter().filter(|o| o.scope == Scope::App).map(|o| (o.key.to_string(), o.def)).collect();
    let txt = std::fs::read_to_string(ini_path()).unwrap_or_default();
    let mut cur = String::new();
    let mut legacy_fps_overlay = None;
    let mut matches = BTreeMap::new();
    for line in txt.lines().map(str::trim) {
        if line.starts_with(';') || line.is_empty() { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            cur = line[1..line.len() - 1].trim().to_string();
            if cur != "App" { profiles.entry(cur.clone()).or_insert_with(defaults); }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim() == "match" && cur != "App" { if !v.trim().is_empty() { matches.insert(cur.clone(), v.trim().to_string()); } continue; }
        let (k, Ok(v)) = (k.trim(), v.trim().parse::<i32>()) else { continue };
        if k == "draw_fps" { legacy_fps_overlay = Some(legacy_fps_overlay.unwrap_or(0) | v); continue; }
        let Some(o) = opt(k) else { continue };
        let v = sanitize(o, v);
        match (o.scope, cur == "App") {
            (Scope::App, true) => { app.insert(k.to_string(), v); }
            (Scope::Profile, false) => { if let Some(p) = profiles.get_mut(&cur) { set(p, k, v); } }
            _ => {}
        }
    }
    // Pre-2.1 ini: the per-profile FPS counter becomes the global overlay switch.
    if let Some(v) = legacy_fps_overlay { if !txt.contains("osd=") { app.insert("osd".into(), (v != 0) as i32); } }
    Store { profiles, app, matches }
}

pub fn save(s: &Store) {
    let mut out = String::from("; FlowScale settings. Edit while FlowScale is closed, or use the app.\r\n[App]\r\n");
    for (k, v) in &s.app { out += &format!("{k}={v}\r\n"); }
    for (name, v) in &s.profiles {
        out += &format!("\r\n[{name}]\r\n");
        if let Some(m) = s.matches.get(name) { out += &format!("match={}\r\n", m.replace(['\r', '\n'], " ")); }
        for (o, x) in OPTS.iter().filter(|o| o.scope == Scope::Profile).zip(v) { out += &format!("{}={}\r\n", o.key, x); }
    }
    // Write-then-rename so a crash mid-write never leaves a truncated settings file.
    let tmp = ini_path().with_extension("ini.tmp");
    if std::fs::write(&tmp, out).is_ok() { let _ = std::fs::rename(&tmp, ini_path()); }
}

pub fn schema(gpus: &[String], displays: &[String]) -> Value {
    let opts: Vec<Value> = OPTS.iter().map(|o| {
        let mut v = json!({"key": o.key, "group": o.group, "app": o.scope == Scope::App, "def": o.def});
        match o.kind {
            Kind::Choice(n) => { v["kind"] = "choice".into(); v["n"] = n.into(); }
            Kind::Range(lo, hi, st) => { v["kind"] = "range".into(); v["min"] = lo.into(); v["max"] = hi.into(); v["step"] = st.into(); }
            Kind::Toggle => v["kind"] = "toggle".into(),
            Kind::Gpu => { v["kind"] = "list".into(); v["options"] = json!(gpus); }
            Kind::Display => { v["kind"] = "list".into(); v["options"] = json!(displays); }
            Kind::Hotkey => v["kind"] = "hotkey".into(),
        }
        v
    }).collect();
    json!({"t": "schema", "opts": opts})
}

pub fn profile_json(p: &Profile) -> Value {
    Value::Object(OPTS.iter().filter(|o| o.scope == Scope::Profile).zip(p).map(|(o, v)| (o.key.to_string(), json!(v))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn sanitize_clamps() {
        assert_eq!(sanitize(opt("multiplier").unwrap(), 99), 8);
        assert_eq!(sanitize(opt("scaler").unwrap(), -3), 0);
        assert_eq!(sanitize(opt("sr_ratio").unwrap(), 40), 100);
        assert_eq!(sanitize(opt("osd").unwrap(), 7), 1);
    }
    #[test] fn glob_matches() {
        assert!(glob("*.exe", "game.exe")); assert!(glob("ga?e*", "GAME.exe")); assert!(glob("*\\steamapps\\*", r"c:\steamapps\x\g.exe"));
        assert!(!glob("game", "game.exe")); assert!(glob("*", "")); assert!(!glob("?", ""));
    }
    #[test] fn profile_by_title_and_path() {
        let mut s = Store { profiles: BTreeMap::new(), app: BTreeMap::new(), matches: BTreeMap::new() };
        s.profiles.insert("Default".into(), defaults()); s.profiles.insert("emu".into(), defaults());
        s.matches.insert("emu".into(), "retroarch.exe; *Dolphin*".into());
        let t = |e: &str, p: &str, ti: &str| Target { exe: e.into(), path: p.into(), title: ti.into() };
        assert_eq!(s.profile_for(&t("dolphin.exe", "", "Dolphin 5.0 | Mario")).0, "emu");
        assert_eq!(s.profile_for(&t("retroarch.exe", "", "")).0, "emu");
        assert_eq!(s.profile_for(&t("other.exe", "", "x")).0, "Default");
    }
    #[test] fn windowed_never_touches_cursor() {
        let mut p = defaults(); set(&mut p, "output", 1); set(&mut p, "clip_cursor", 1);
        let s = Settings::from(&p); assert!(s.windowed && !s.clip_cursor && !s.draw_cursor);
    }
}
