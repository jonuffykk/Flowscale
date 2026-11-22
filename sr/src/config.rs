use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub alpha_rest: f32, pub conf: f32, pub gamma_rest: f32, pub width: f32, pub react: f32, pub reactive_scale: f32,
    pub exposure_scale: f32, pub jitter_sign_x: f32, pub jitter_sign_y: f32, pub mv_sign: f32, pub motion_gamma: f32,
    pub motion_share: f32, pub motion_min_max: f32, pub sharpness: f32, pub sharpen_mode: i32,
    pub relight: bool, pub ao_radius: f32, pub ao_screen: f32, pub ao_rays: f32, pub ao_contact: f32,
    pub ao_strength: f32, pub gi_strength: f32, pub gi_dark: f32, pub s_curve: f32, pub ao_spatial: f32,
    pub ao_temporal: bool, pub ao_temporal_weight: f32, pub gi_ceiling: f32, pub ao_plane_filter: bool, pub ao_mips: bool, pub ao_power: bool,
    pub ao_backface_boost: bool, pub ao_lut: bool, pub ao_wg16: bool, pub ao_color_mips: bool, pub ao_samples: f32, pub ao_decorrelate: bool,
    pub ao_cacao: bool, pub ao_cacao_passes: i32, pub ao_history_length: bool, pub ao_ambient_only: bool, pub ao_emitter_cosine: bool,
    pub ao_better_normals: bool, pub ao_gi_wide: bool, pub ao_gi_long: bool, pub ao_curve_lut: bool, pub ao_noise8: bool, pub ao_gi_falloff: bool,
    pub ao_fade_meters: f32, pub curve_n: usize, pub curve_x: [f32; 8], pub curve_y: [f32; 8],
    pub frame_weight: f32, pub frame_weight_override: bool, pub change_scale: f32, pub share_far: f32, pub share_near: f32,
    pub encode_power: f32, pub motion_near: f32, pub split_clip: f32, pub history10: bool, pub fp16: i32,
    pub auto_reactive: f32, pub stab_step: f32, pub anti_shimmer: f32, pub history_sharpen: f32, pub fused_sharpen: f32,
    pub env_tol: f32, pub env_rel: f32, pub env_decay: f32, pub env_gain: f32, pub env_cap: f32, pub env_rest: f32,
    pub alpha_slow: f32, pub ramp_lo: f32, pub ramp_hi: f32, pub indicator: bool, pub debug_view: i32,
}
impl Default for Settings {
    fn default() -> Self {
        Settings {
            alpha_rest: 0.1, conf: 0.05, gamma_rest: 3.0, width: 0.8, react: 1.0, reactive_scale: 1.0, exposure_scale: 1.0,
            jitter_sign_x: 1.0, jitter_sign_y: 1.0, mv_sign: 1.0, motion_gamma: 1.5, motion_share: 0.15, motion_min_max: 1.0,
            sharpness: 0.6, sharpen_mode: 0, relight: false, ao_radius: 0.0, ao_screen: 0.5, ao_rays: 4.0, ao_contact: 2.0,
            ao_strength: 0.75, gi_strength: 2.0, gi_dark: 1.0, s_curve: 0.3, ao_spatial: 1.0, ao_temporal: true, ao_temporal_weight: 0.85,
            gi_ceiling: 2.0, ao_plane_filter: true, ao_mips: true, ao_power: true, ao_backface_boost: true, ao_lut: true, ao_wg16: true, ao_color_mips: true,
            ao_samples: 32.0, ao_decorrelate: true, ao_cacao: true, ao_cacao_passes: 2, ao_history_length: true, ao_ambient_only: true, ao_emitter_cosine: true,
            ao_better_normals: true, ao_gi_wide: true, ao_gi_long: true, ao_curve_lut: true, ao_noise8: true, ao_gi_falloff: true, ao_fade_meters: 100.0,
            curve_n: 3, curve_x: [0.03774, 0.228, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0], curve_y: [0.0, 0.2453, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            frame_weight: 2.0, frame_weight_override: false, change_scale: 1.0, share_far: 0.2, share_near: 3.0, encode_power: 0.5,
            motion_near: 1.5, split_clip: 1.0, history10: true, fp16: 0, auto_reactive: 0.5, stab_step: 0.34, anti_shimmer: -1.0,
            history_sharpen: 1.0, fused_sharpen: 1.0, env_tol: 0.03, env_rel: 0.25, env_decay: 0.05, env_gain: 40.0, env_cap: 0.5,
            env_rest: 1.0, alpha_slow: 0.15, ramp_lo: 0.25, ramp_hi: 1.0, indicator: false, debug_view: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool, pub toggle_key: i32, pub reload_key: i32, pub compare_key: i32, pub compare: bool,
    pub log_dispatches: u64, pub ratio: [f32; 4], pub game_sharpness: i32, pub s: Settings,
}
impl Default for Config {
    fn default() -> Self { Config { enabled: true, toggle_key: 0, reload_key: 0x78, compare_key: 0, compare: false, log_dispatches: 0, ratio: [0.0; 4], game_sharpness: -1, s: Settings::default() } }
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static CFG: Mutex<Option<Config>> = Mutex::new(None);

pub fn set_dir(d: PathBuf) { let _ = DIR.set(d); }
pub fn dir() -> PathBuf { DIR.get().cloned().unwrap_or_default() }

pub fn log(msg: &str) {
    let mut g = LOG.lock().unwrap();
    if g.is_none() { *g = std::fs::File::create(dir().join("flowscale_sr.log")).ok(); }
    if let Some(f) = g.as_mut() {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let _ = writeln!(f, "{:02}:{:02}:{:02}.{:03} {msg}", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
        let _ = f.flush();
    }
}
#[macro_export]
macro_rules! log { ($($a:tt)*) => { $crate::config::log(&format!($($a)*)) } }

fn parse(txt: &str) -> HashMap<String, String> {
    let mut m = HashMap::new(); let mut sec = String::new();
    for l in txt.lines().map(str::trim) {
        if l.starts_with(';') || l.starts_with('#') || l.is_empty() { continue; }
        if l.starts_with('[') && l.ends_with(']') { sec = l[1..l.len() - 1].trim().to_lowercase(); continue; }
        if let Some((k, v)) = l.split_once('=') { m.insert(format!("{sec}.{}", k.trim().to_lowercase()), v.split(';').next().unwrap_or("").trim().to_string()); }
    }
    m
}
fn num(m: &HashMap<String, String>, k: &str) -> Option<f32> {
    let v = m.get(k)?;
    if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) { return i64::from_str_radix(h, 16).ok().map(|x| x as f32); }
    v.parse::<f32>().ok()
}

/// "x,y x,y ...": 2 - 8 points in [0, 1], x increasing.
pub fn parse_curve(v: &str) -> Option<(usize, [f32; 8], [f32; 8])> {
    let (mut x, mut y, mut n) = ([0f32; 8], [0f32; 8], 0);
    for p in v.split_whitespace().take(8) {
        let (a, b) = p.split_once(',')?;
        let (a, b) = (a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?);
        if !(0.0..=1.0).contains(&a) || !(0.0..=1.0).contains(&b) || (n > 0 && a <= x[n - 1]) { return None; }
        x[n] = a; y[n] = b; n += 1;
    }
    (n >= 2).then_some((n, x, y))
}

pub fn load() -> Config {
    let txt = std::fs::read_to_string(dir().join("flowscale_sr.ini")).unwrap_or_default();
    let m = parse(&txt);
    let mut c = Config::default();
    let f = |k: &str, d: f32| num(&m, &format!("sr.{k}")).unwrap_or(d);
    let b = |k: &str, d: bool| num(&m, &format!("sr.{k}")).map(|x| x != 0.0).unwrap_or(d);
    let i = |k: &str, d: i32| num(&m, &format!("sr.{k}")).map(|x| x as i32).unwrap_or(d);
    c.enabled = b("enabled", true);
    let debug = num(&m, "debug.enabled").map(|x| x != 0.0).unwrap_or(false);
    c.toggle_key = if debug { i("toggle_key", 0x79) } else { 0 };
    c.reload_key = i("reload_key", 0x78);
    c.compare_key = i("compare_key", 0);
    c.compare = b("compare", false);
    c.log_dispatches = i("log_dispatches", if debug { 3 } else { 0 }).max(0) as u64;
    c.game_sharpness = i("game_sharpness", -1);
    c.ratio = [f("ratio_quality", 0.0), f("ratio_balanced", 0.0), f("ratio_performance", 0.0), f("ratio_ultra_performance", 0.0)];
    let d = Settings::default();
    let s = &mut c.s;
    s.frame_weight = f("frame_weight", d.frame_weight); s.frame_weight_override = b("frame_weight_override", d.frame_weight_override);
    s.alpha_rest = f("alpha_rest", d.alpha_rest); s.conf = f("conf", d.conf);
    s.gamma_rest = f("gamma_rest", d.gamma_rest); if s.gamma_rest <= 0.0 { s.gamma_rest = d.gamma_rest; }
    s.width = f("width", d.width); s.react = f("react", d.react); s.reactive_scale = f("reactive_scale", d.reactive_scale);
    s.exposure_scale = f("exposure_scale", d.exposure_scale); s.jitter_sign_x = f("jitter_sign_x", d.jitter_sign_x);
    s.jitter_sign_y = f("jitter_sign_y", d.jitter_sign_y); s.mv_sign = f("mv_sign", d.mv_sign);
    s.motion_gamma = f("motion_gamma", d.motion_gamma); s.motion_share = f("motion_share", d.motion_share);
    s.motion_min_max = f("motion_minmax", d.motion_min_max); s.sharpness = f("sharpness", d.sharpness);
    s.change_scale = f("change_scale", d.change_scale); s.share_far = f("share_far", d.share_far); s.share_near = f("share_near", d.share_near);
    s.encode_power = f("encode_power", d.encode_power); s.motion_near = f("motion_near", d.motion_near); s.split_clip = f("split_clip", d.split_clip);
    s.history10 = b("history_10bit", d.history10); s.fp16 = i("fp16", d.fp16); s.auto_reactive = f("auto_reactive", d.auto_reactive);
    s.stab_step = f("stab_step", d.stab_step); s.anti_shimmer = f("anti_shimmer", d.anti_shimmer);
    s.history_sharpen = f("history_sharpen", d.history_sharpen); s.fused_sharpen = f("fused_sharpen", d.fused_sharpen);
    s.sharpen_mode = i("sharpen_mode", d.sharpen_mode).clamp(0, 2);
    s.relight = b("relight", d.relight); s.ao_radius = f("ao_radius", d.ao_radius); s.ao_screen = f("ao_screen", d.ao_screen);
    s.ao_rays = f("ao_rays", d.ao_rays); s.ao_contact = f("ao_contact", d.ao_contact); s.ao_strength = f("ao_strength", d.ao_strength);
    s.gi_strength = f("gi_strength", d.gi_strength); s.gi_dark = f("gi_dark", d.gi_dark); s.s_curve = f("s_curve", d.s_curve);
    s.ao_temporal = b("ao_temporal", d.ao_temporal); s.ao_temporal_weight = f("ao_temporal_weight", d.ao_temporal_weight);
    s.ao_spatial = f("ao_spatial_radius", d.ao_spatial); s.gi_ceiling = f("gi_ceiling", d.gi_ceiling);
    s.ao_plane_filter = b("ao_plane_filter", d.ao_plane_filter); s.ao_mips = b("ao_mips", d.ao_mips); s.ao_power = b("ao_power", d.ao_power);
    s.ao_backface_boost = b("ao_backface_boost", d.ao_backface_boost); s.ao_lut = b("ao_lut", d.ao_lut); s.ao_wg16 = b("ao_workgroup16", d.ao_wg16);
    s.ao_color_mips = b("ao_color_mips", d.ao_color_mips); s.ao_samples = f("ao_samples", d.ao_samples); s.ao_decorrelate = b("ao_decorrelate", d.ao_decorrelate);
    s.ao_cacao = b("ao_cacao", d.ao_cacao); s.ao_cacao_passes = i("ao_cacao_passes", d.ao_cacao_passes).clamp(1, 8);
    s.ao_history_length = b("ao_history_length", d.ao_history_length); s.ao_ambient_only = b("ao_ambient_only", d.ao_ambient_only);
    s.ao_emitter_cosine = b("ao_emitter_cosine", d.ao_emitter_cosine); s.ao_better_normals = b("ao_better_normals", d.ao_better_normals);
    s.ao_gi_wide = b("ao_gi_wide", d.ao_gi_wide); s.ao_gi_long = b("ao_gi_long_history", d.ao_gi_long); s.ao_curve_lut = b("ao_curve_lut", d.ao_curve_lut);
    s.ao_noise8 = b("ao_noise_cycle8", d.ao_noise8); s.ao_gi_falloff = b("ao_gi_falloff", d.ao_gi_falloff); s.ao_fade_meters = f("ao_fade_distance", d.ao_fade_meters);
    if let Some(v) = m.get("sr.curve") { if let Some((n, x, y)) = parse_curve(v) { s.curve_n = n; s.curve_x = x; s.curve_y = y; } }
    s.env_tol = f("env_tol", d.env_tol); s.env_decay = f("env_decay", d.env_decay); s.env_gain = f("env_gain", d.env_gain);
    s.env_rest = f("env_rest", d.env_rest); s.ramp_lo = f("ramp_lo", d.ramp_lo); s.ramp_hi = f("ramp_hi", d.ramp_hi);
    s.env_cap = f("env_cap", d.env_cap); s.env_rel = f("env_rel", d.env_rel); s.alpha_slow = f("alpha_slow", d.alpha_slow);
    s.indicator = num(&m, "debug.indicator").map(|x| x != 0.0).unwrap_or(false);
    s.debug_view = if debug { i("debug_view", 0) } else { 0 };
    c
}

/// The ini's write time, looked at twice a second at most: FlowScale's app rewrites it and the game picks it up live.
pub fn changed_on_disk() -> bool {
    static LAST: Mutex<(u64, Option<std::time::SystemTime>)> = Mutex::new((0, None));
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let mut g = LAST.lock().unwrap();
    if now < g.0 + 500 { return false; }
    g.0 = now;
    let t = std::fs::metadata(dir().join("flowscale_sr.ini")).and_then(|m| m.modified()).ok();
    let changed = g.1.is_some() && t != g.1;
    g.1 = t;
    changed
}

pub fn get() -> Config { let mut g = CFG.lock().unwrap(); g.get_or_insert_with(load).clone() }
pub fn reload() -> Config { let c = load(); *CFG.lock().unwrap() = Some(c.clone()); log!("settings reloaded"); c }
