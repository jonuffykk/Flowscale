use crate::config::Settings;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Constants {
    pub rs: [f32; 2], pub os: [f32; 2], pub rs_rcp: [f32; 2], pub os_rcp: [f32; 2],
    pub jit: [f32; 2], pub ratio: f32, pub width: f32,
    pub alpha_rest: f32, pub conf: f32, pub reset: f32, pub gamma_rest: f32,
    pub react: f32, pub reactive_scale: f32, pub stab_step: f32, pub depth_inverted: f32,
    pub mv_scale: [f32; 2], pub mv_size: [f32; 2],
    pub jit_cancel: [f32; 2], pub pre_exposure: f32, pub hdr: f32,
    pub exposure_scale: f32, pub has_exposure: f32, pub has_reactive: f32, pub debug_view: f32,
    pub indicator: f32, pub motion_gamma: f32, pub motion_share: f32, pub motion_min_max: f32,
    pub anti_shimmer: f32, pub sharpness: f32, pub change_scale: f32, pub share_far: f32,
    pub share_near: f32, pub encode_power: f32, pub motion_near: f32, pub split_clip: f32,
    pub history10: f32, pub frame_weight: f32, pub split_x: f32, pub auto_reactive: f32,
    pub auto_exposure: f32, pub history_sharpen: f32, pub fused_sharpen: f32,
    pub env_tol: f32, pub env_decay: f32, pub env_gain: f32, pub env_rest: f32,
    pub ramp_lo: f32, pub ramp_hi: f32, pub env_rel: f32, pub env_cap: f32, pub alpha_slow: f32,
    pub sharpen_mode: f32,
}
const _: () = assert!(std::mem::size_of::<Constants>() == 61 * 4);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AoConstants {
    pub rs: [f32; 2], pub rs_rcp: [f32; 2], pub tan_x: f32, pub tan_y: f32, pub z_near: f32, pub z_far: f32, pub inverted: f32, pub frame: f32,
    pub ao_strength: f32, pub gi_strength: f32, pub radius_world: f32, pub radius_screen: f32, pub rays: f32, pub contact: f32, pub gi_dark: f32,
    pub s_curve: f32, pub pivot: f32, pub temporal: f32, pub spatial: f32, pub axis: f32, pub mv_scale: [f32; 2], pub mv_size: [f32; 2],
    pub gi_ceiling: f32, pub plane_w: f32, pub mips: f32, pub ao_pow: f32, pub flags: f32, pub lut: f32, pub cmips: f32, pub samples: f32, pub half_res: f32, pub fade_z: f32,
    pub curve_x: [f32; 8], pub curve_y: [f32; 8], pub curve_y2: [f32; 8], pub curve_n: f32,
}
const _: () = assert!(std::mem::size_of::<AoConstants>() == 61 * 4);

/// The natural cubic spline's second derivatives at the curve's points (x increasing).
pub fn curve_y2(n: usize, x: &[f32], y: &[f32], y2: &mut [f32]) {
    let mut u = [0f32; 8];
    y2[0] = 0.0;
    for i in 1..n.saturating_sub(1) {
        let sig = (x[i] - x[i - 1]) / (x[i + 1] - x[i - 1]); let p = sig * y2[i - 1] + 2.0;
        y2[i] = (sig - 1.0) / p;
        let d = (y[i + 1] - y[i]) / (x[i + 1] - x[i]) - (y[i] - y[i - 1]) / (x[i] - x[i - 1]);
        u[i] = (6.0 * d / (x[i + 1] - x[i - 1]) - sig * u[i - 1]) / p;
    }
    y2[n - 1] = 0.0;
    for k in (0..n - 1).rev() { y2[k] = y2[k] * y2[k + 1] + u[k]; }
}
pub fn curve_is_line(s: &Settings) -> bool {
    s.curve_n < 2 || (s.curve_n == 2 && s.curve_x[0] == 0.0 && s.curve_y[0] == 0.0 && s.curve_x[1] == 1.0 && s.curve_y[1] == 1.0)
}

pub struct AoFrame { pub rw: u32, pub rh: u32, pub fov_y: f32, pub z_near: f32, pub z_far: f32, pub meters_per_unit: f32, pub depth_inverted: bool, pub hdr: bool, pub mv_to_uv: [f32; 2], pub mv_res: [f32; 2] }

pub fn fill_ao(s: &Settings, fr: &AoFrame, float_colour: bool, frames: u64) -> AoConstants {
    let (rw, rh) = (fr.rw as f32, fr.rh as f32);
    let mut k = AoConstants { rs: [rw, rh], rs_rcp: [1.0 / rw, 1.0 / rh], ..Default::default() };
    let fov = if fr.fov_y > 0.01 && fr.fov_y < 3.1 { fr.fov_y } else { 1.0472 };
    k.tan_y = (0.5 * fov).tan(); k.tan_x = k.tan_y * rw / rh;
    k.z_near = if fr.z_near > 0.0 { fr.z_near } else { 0.1 }; k.z_far = if fr.z_far > 0.0 { fr.z_far } else { 100000.0 };
    k.inverted = fr.depth_inverted as u8 as f32; k.frame = (frames % 1024) as f32;
    k.ao_strength = s.ao_strength.clamp(0.0, 1.0); k.gi_strength = s.gi_strength.clamp(0.0, 4.0); k.gi_dark = s.gi_dark.clamp(0.0, 4.0);
    k.s_curve = s.s_curve.clamp(0.0, 1.0); k.gi_ceiling = s.gi_ceiling.clamp(0.0, 4.0);
    k.plane_w = s.ao_plane_filter as u8 as f32; k.mips = s.ao_mips as u8 as f32; k.ao_pow = s.ao_power as u8 as f32;
    k.flags = ((s.ao_backface_boost as u32) | (s.ao_decorrelate as u32) << 1 | (s.ao_ambient_only as u32) << 8 | (s.ao_emitter_cosine as u32) << 9
        | (s.ao_better_normals as u32) << 11 | (s.ao_gi_wide as u32) << 14 | (s.ao_gi_long as u32) << 15 | (s.ao_noise8 as u32) << 19 | (s.ao_gi_falloff as u32) << 20) as f32;
    k.lut = s.ao_lut as u8 as f32; k.cmips = (s.ao_color_mips && s.ao_mips) as u8 as f32;
    k.samples = if s.ao_samples < 4.0 { 4.0 } else if s.ao_samples > 32.0 { 32.0 } else { (s.ao_samples + 0.5).floor() };
    k.fade_z = if s.ao_fade_meters > 0.0 { s.ao_fade_meters / if fr.meters_per_unit > 0.0 { fr.meters_per_unit } else { 1.0 } } else { 0.0 };
    if !curve_is_line(s) {
        let n = s.curve_n.min(8);
        k.curve_x[..n].copy_from_slice(&s.curve_x[..n]); k.curve_y[..n].copy_from_slice(&s.curve_y[..n]);
        let (x, y) = (k.curve_x, k.curve_y);
        curve_y2(n, &x, &y, &mut k.curve_y2);
        k.curve_n = n as f32;
    }
    k.pivot = if fr.hdr || float_colour { 0.071 } else { 0.3 };
    k.radius_world = s.ao_radius; k.radius_screen = if s.ao_screen > 0.01 { s.ao_screen } else { 0.5 };
    k.rays = if s.ao_rays < 4.0 { 4.0 } else if s.ao_rays > 32.0 { 32.0 } else { (s.ao_rays + 0.5).floor() };
    k.contact = s.ao_contact.clamp(0.0, 4.0); k.spatial = s.ao_spatial.clamp(1.0, 2.0);
    k.mv_scale = [s.mv_sign * fr.mv_to_uv[0], s.mv_sign * fr.mv_to_uv[1]]; k.mv_size = fr.mv_res;
    k
}

pub struct FrameNums { pub rw: u32, pub rh: u32, pub ow: u32, pub oh: u32, pub jit: [f32; 2], pub mv_to_uv: [f32; 2], pub mv_res: [f32; 2],
    pub jittered_mv: bool, pub depth_inverted: bool, pub hdr: bool, pub exposure_mul: f32, pub pre_exposure: f32, pub reactive_mul: f32 }

#[allow(clippy::too_many_arguments)]
pub fn fill(s: &Settings, fr: &FrameNums, reset: bool, has_exposure: bool, has_reactive: bool, history10: bool, prev_jit: &mut [f32; 2]) -> Constants {
    let (rw, rh, ow, oh) = (fr.rw as f32, fr.rh as f32, fr.ow as f32, fr.oh as f32);
    let mut k = Constants { rs: [rw, rh], os: [ow, oh], rs_rcp: [1.0 / rw, 1.0 / rh], os_rcp: [1.0 / ow, 1.0 / oh], ..Default::default() };
    k.jit = [fr.jit[0] * s.jitter_sign_x, fr.jit[1] * s.jitter_sign_y];
    k.ratio = ow / rw;
    let lo = if k.ratio <= 1.5 { 0.0 } else if k.ratio >= 2.0 { 1.0 } else { (k.ratio - 1.5) / 0.5 };
    k.width = s.width * (0.875 + 0.125 * lo);
    k.alpha_rest = s.alpha_rest; k.conf = s.conf; k.reset = if reset { 1.0 } else { 0.0 }; k.gamma_rest = s.gamma_rest;
    k.react = s.react; k.reactive_scale = s.reactive_scale * fr.reactive_mul;
    k.mv_scale = [s.mv_sign * fr.mv_to_uv[0], s.mv_sign * fr.mv_to_uv[1]];
    k.mv_size = fr.mv_res;
    if fr.jittered_mv { k.jit_cancel = [(prev_jit[0] - k.jit[0]) / rw, (prev_jit[1] - k.jit[1]) / rh]; }
    *prev_jit = k.jit;
    k.depth_inverted = fr.depth_inverted as u8 as f32;
    k.pre_exposure = if fr.pre_exposure > 0.0 { fr.pre_exposure } else { 1.0 };
    k.hdr = fr.hdr as u8 as f32;
    k.exposure_scale = s.exposure_scale * fr.exposure_mul; k.has_exposure = has_exposure as u8 as f32;
    k.has_reactive = has_reactive as u8 as f32;
    k.debug_view = s.debug_view as f32; k.indicator = s.indicator as u8 as f32;
    k.motion_gamma = s.motion_gamma; k.motion_share = s.motion_share; k.motion_min_max = s.motion_min_max;
    k.sharpness = s.sharpness; k.change_scale = s.change_scale;
    k.share_far = s.share_far; k.share_near = s.share_near; k.encode_power = s.encode_power;
    k.motion_near = s.motion_near; k.split_clip = s.split_clip;
    k.history10 = history10 as u8 as f32;
    k.frame_weight = s.frame_weight.clamp(0.25, 4.0);
    let r15 = if k.ratio <= 1.0 { 0.0 } else if k.ratio >= 1.5 { 1.0 } else { (k.ratio - 1.0) / 0.5 };
    k.frame_weight *= 0.5 + 0.5 * r15;
    k.auto_reactive = if has_reactive { 0.0 } else { s.auto_reactive };
    k.stab_step = if s.stab_step > 0.01 { s.stab_step } else { 0.34 };
    let hr = if k.ratio <= 2.0 { 0.0 } else if k.ratio >= 3.0 { 1.0 } else { k.ratio - 2.0 };
    k.history_sharpen = if s.history_sharpen > 0.0 { (0.03 + 0.02 * hr) * s.history_sharpen } else { 0.0 };
    let a = if s.anti_shimmer >= 0.0 { s.anti_shimmer } else { 0.6 - 0.15 * hr };
    k.anti_shimmer = a.min(0.9);
    k.fused_sharpen = 0.0;
    k.ramp_lo = s.ramp_lo; k.ramp_hi = s.ramp_hi; k.env_rel = s.env_rel; k.alpha_slow = s.alpha_slow;
    k.env_cap = s.env_cap * (1.0 - 0.5 * hr);
    k.share_far *= 1.0 - 0.5 * hr;
    k.env_tol = s.env_tol; k.env_decay = s.env_decay; k.env_gain = s.env_gain; k.env_rest = s.env_rest;
    k.sharpen_mode = s.sharpen_mode.clamp(0, 2) as f32;
    k
}
