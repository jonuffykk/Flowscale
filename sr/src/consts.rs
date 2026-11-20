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
    pub s_curve: f32, pub pivot: f32, pub temporal: f32, pub spatial: f32, pub axis: f32, pub mv_scale: [f32; 2], pub mv_size: [f32; 2], pub pad: [f32; 2],
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
    k.sharpen_mode = if s.sharpen_mode == 1 { 1.0 } else { 0.0 };
    k
}
