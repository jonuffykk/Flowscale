//! Game detection by evidence. Every candidate window gets a score from what its process loads and where it lives;
//! anti-cheat protected processes are refused before anything is injected.
use crate::limiter::ProcInfo;

/// Never offered or auto-attached: shells, browsers, launchers, chat, creative tools, overlays, recorders, system helpers.
const NON_GAME_EXE: &[&str] = &[
    "explorer.exe", "dwm.exe", "applicationframehost.exe", "searchhost.exe", "startmenuexperiencehost.exe", "shellexperiencehost.exe", "textinputhost.exe",
    "lockapp.exe", "widgets.exe", "widgetservice.exe", "systemsettings.exe", "taskmgr.exe", "ctfmon.exe", "sihost.exe", "runtimebroker.exe", "smartscreen.exe",
    "flowscale.exe", "flowscale32.exe", "framepacer.exe", "losslessscaling.exe", "magpie.exe",
    "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "opera_gx.exe", "vivaldi.exe", "arc.exe", "msedgewebview2.exe", "cefsharp.browsersubprocess.exe",
    "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "epicwebhelper.exe", "galaxyclient.exe", "galaxyclient helper.exe", "gog galaxy.exe",
    "ubisoftconnect.exe", "upc.exe", "uplaywebcore.exe", "origin.exe", "ea.exe", "eadesktop.exe", "battle.net.exe", "battlenet.exe", "riotclientservices.exe",
    "riotclientux.exe", "xboxpcapp.exe", "xboxapp.exe", "gamingservices.exe", "gamebar.exe", "gamebarftserver.exe", "playnite.desktopapp.exe",
    "playnite.fullscreenapp.exe", "heroic.exe", "rockstarlauncher.exe", "launcher.exe", "itch.exe", "amazon games ui.exe", "bethesda.net_launcher.exe",
    "discord.exe", "discordptb.exe", "discordcanary.exe", "slack.exe", "teams.exe", "ms-teams.exe", "zoom.exe", "whatsapp.exe", "telegram.exe", "signal.exe", "skype.exe",
    "code.exe", "devenv.exe", "rider64.exe", "idea64.exe", "pycharm64.exe", "clion64.exe", "sublime_text.exe", "notepad.exe", "notepad++.exe", "windowsterminal.exe",
    "powershell.exe", "pwsh.exe", "cmd.exe", "conhost.exe", "openconsole.exe", "claude.exe", "chatgpt.exe",
    "blender.exe", "maya.exe", "3dsmax.exe", "photoshop.exe", "illustrator.exe", "afterfx.exe", "adobe premiere pro.exe", "resolve.exe", "figma.exe",
    "paintdotnet.exe", "mspaint.exe", "krita.exe", "gimp-2.10.exe", "obs64.exe", "obs32.exe", "streamlabs desktop.exe", "medal.exe", "spotify.exe", "vlc.exe",
    "mpc-hc64.exe", "mpc-be64.exe", "potplayermini64.exe", "rtss.exe", "msiafterburner.exe", "hwinfo64.exe", "presentmon.exe", "capframex.exe",
    "nvidia app.exe", "nvidia overlay.exe", "nvcplui.exe", "radeonsoftware.exe", "amddvr.exe", "overwolf.exe", "wallpaper64.exe", "wallpaper32.exe",
    "lghub.exe", "steelseriesgg.exe", "icue.exe", "nzxt cam.exe", "razer synapse service.exe", "unrealeditor.exe", "unity.exe", "godot.exe",
];
/// Words that mark helper processes rather than games (matched inside the exe name).
const NON_GAME_WORDS: &[&str] = &["launcher", "updater", "patcher", "crashhandler", "crashreport", "crash_handler", "bootstrapper", "setup", "installer", "uninstall", "redist", "webhelper", "dedicatedserver", "anticheat"];

/// Titles whose anti-cheat bans for any injected DLL. Refused by name even before their protection module loads.
const PROTECTED_TITLES: &[&str] = &[
    "valorant-win64-shipping.exe", "vanguard.exe", "league of legends.exe", "r5apex.exe", "r5apex_dx12.exe", "fortniteclient-win64-shipping.exe", "cs2.exe",
    "dota2.exe", "eldenring.exe", "helldivers2.exe", "rustclient.exe", "deadbydaylight-win64-shipping.exe", "escapefromtarkov.exe", "tarkovarena.exe",
    "tslgame.exe", "pubg.exe", "rainbowsix.exe", "rainbowsix_vulkan.exe", "rainbowsix_be.exe", "destiny2.exe", "overwatch.exe", "cod.exe", "blackops4.exe",
    "modernwarfare.exe", "bf2042.exe", "bf6.exe", "bfv.exe", "bf1.exe", "fc24.exe", "fc25.exe", "fc26.exe", "madden25.exe", "f1_24.exe", "f1_25.exe",
    "genshinimpact.exe", "starrail.exe", "zenlesszonezero.exe", "wuthering waves.exe", "client-win64-shipping.exe", "narakabladepoint.exe", "lostark.exe",
    "blackdesert64.exe", "newworld.exe", "thefinals.exe", "discovery.exe", "marvelrivals-win64-shipping.exe", "haloinfinite.exe", "mcc-win64-shipping.exe",
    "squadgame.exe", "huntgame.exe", "hunt.exe", "dayz_x64.exe", "arma3_x64.exe", "armareforgersteam.exe", "arkascended.exe", "shootergame.exe", "vrising.exe",
    "fallguys_client_game.exe", "chivalry2-win64-shipping.exe", "brawlhalla.exe", "paladins.exe", "smite.exe", "smite2-win64-shipping.exe", "rocketleague.exe",
    "robloxplayerbeta.exe", "vrchat.exe", "gta5.exe", "gta5_enhanced.exe", "rdr2.exe", "deltaforceclient-win64-shipping.exe", "arenabreakoutinfinite.exe",
    "thedivision2.exe", "forhonor.exe", "sotgame.exe", "battlebit.exe", "unturned.exe", "planetside2_x64.exe", "warframe.x64.exe", "pathofexile.exe",
    "pathofexile_x64.exe", "diablo iv.exe", "wow.exe", "maplestory.exe", "tibia.exe", "aces.exe", "strinova.exe", "spectre.exe", "xdefiant.exe",
];
/// Anti-cheat modules (substring match on loaded module names).
pub const ANTICHEAT_MODULES: &[&str] = &[
    "easyanticheat", "eac_launcher", "beclient", "bedaisy", "battleye", "vgk", "vanguard", "faceit", "esea", "xigncode", "x3.xem", "xhunter", "wellbia",
    "nprotect", "gameguard", "npggnt", "gamemon", "hackshield", "ahnlab", "blackcipher", "nexongamesecurity", "ngsclient", "ricochet", "punkbuster", "pnkbstr",
    "equ8", "anticheatexpert", "ace-base", "ace_game", "tencentace", "tenprotect", "mhyprot", "hoyokprotect", "denuvoanticheat", "denuvo_anti_cheat",
    "denuvo-anti-cheat", "anybrain", "byfron", "hyperion", "eaanticheat", "eajavelin", "javelin", "fairfight", "ggshield", "defensematrix", "eop.dll", "eac.dll",
];
/// Overlays that hook presentation inside the game (module, family).
pub const OVERLAY_MODULES: &[(&str, &str)] = &[
    ("rtsshooks.dll", "RTSS"), ("rtsshooks64.dll", "RTSS"), ("gameoverlayrenderer.dll", "Steam"), ("gameoverlayrenderer64.dll", "Steam"),
    ("discordhook.dll", "Discord"), ("discordhook64.dll", "Discord"), ("nvspcap.dll", "NVIDIA"), ("nvspcap64.dll", "NVIDIA"), ("nvoverlay.dll", "NVIDIA"),
    ("nvoverlay64.dll", "NVIDIA"), ("amdow.dll", "AMD"), ("amdow64.dll", "AMD"), ("radeonoverlay.dll", "AMD"), ("radeonoverlay64.dll", "AMD"),
    ("graphics-hook32.dll", "OBS"), ("graphics-hook64.dll", "OBS"), ("ow-graphics-hook32.dll", "Overwolf"), ("ow-graphics-hook64.dll", "Overwolf"),
    ("igo32.dll", "EA"), ("igo64.dll", "EA"), ("galaxyoverlayrenderer.dll", "GOG"), ("galaxyoverlayrenderer64.dll", "GOG"),
    ("eosovh-win32-shipping.dll", "Epic"), ("eosovh-win64-shipping.dll", "Epic"), ("eosoverlayrenderer-win32-shipping.dll", "Epic"),
    ("eosoverlayrenderer-win64-shipping.dll", "Epic"), ("reshade.dll", "ReShade"), ("reshade32.dll", "ReShade"), ("reshade64.dll", "ReShade"),
    ("specialk32.dll", "Special K"), ("specialk64.dll", "Special K"), ("presentmon.dll", "PresentMon"), ("gamebarpresencewriter.dll", "Game Bar"),
];
const ENGINES: &[&str] = &["unityplayer.dll", "gameassembly.dll", "mono-2.0-bdwgc.dll", "libgodot", "godot", "gamemaker", "cryrenderd3d11.dll", "crysystem.dll", "frostbite", "tier0.dll", "engine2.dll", "anvil", "luajit", "raylib", "libgdx"];
const RUNTIMES: &[&str] = &["steam_api", "steamclient", "eossdk", "galaxy", "xinput", "dinput8", "gameinput", "physx", "havok", "fmod", "wwise", "aksoundengine",
    "bink2", "binkw", "openal32", "discord_game_sdk", "xaudio2_9redist", "xess", "amd_ags"];
const UPSCALERS: &[&str] = &["nvngx_dlss", "nvngx.dll", "sl.interposer", "sl.dlss", "libxess", "amd_fidelityfx", "ffx_fsr", "nvngx_dlssg", "sl.dlss_g"];
const UI_FRAMEWORKS: &[&str] = &["qt5core.dll", "qt6core.dll", "presentationframework", "presentationcore", "microsoft.winui.dll", "wxmsw", "electron", "libcef.dll", "uiautomationcore"];
const STORE_DIRS: &[&str] = &["\\steamapps\\common\\", "\\epic games\\", "\\gog games\\", "\\gog galaxy\\games\\", "\\xboxgames\\", "\\riot games\\", "\\ea games\\",
    "\\origin games\\", "\\ubisoft game launcher\\games\\", "\\ubisoft\\ubisoft game launcher\\games\\", "\\battle.net\\", "\\heroic\\", "\\amazon games\\library\\", "\\itch\\apps\\"];
const GAME_DIRS: &[&str] = &["\\games\\", "\\gaming\\", "\\steamlibrary\\", "\\jogos\\", "\\juegos\\"];

pub fn ignored(exe: &str) -> bool { exe.is_empty() || NON_GAME_EXE.contains(&exe) }
pub fn blocked_title(exe: &str) -> bool { PROTECTED_TITLES.contains(&exe) }
fn non_game_name(exe: &str) -> bool { ignored(exe) || NON_GAME_WORDS.iter().any(|w| exe.contains(w)) }

#[derive(Debug, PartialEq)]
pub enum Verdict { Game, NotGame, Waiting, Blocked }
pub struct Assessment { pub score: i32, pub evidence: Vec<(&'static str, i32)>, pub verdict: Verdict }
impl Assessment {
    pub fn describe(&self) -> String { self.evidence.iter().map(|(k, v)| format!("{k}:{v:+}")).collect::<Vec<_>>().join(",") }
}

/// Scores a process. `fullscreen`: its window covers its monitor (exclusive or borderless).
pub fn assess(info: &ProcInfo, fullscreen: bool) -> Assessment {
    let mut ev: Vec<(&'static str, i32)> = vec![];
    let has = |list: &[&str]| info.modules.iter().any(|m| list.iter().any(|x| m.contains(x)));
    let count = |list: &[&str]| list.iter().filter(|x| info.modules.iter().any(|m| m.contains(*x))).count() as i32;
    let path = info.path.to_lowercase();
    if info.anticheat.is_some() { return Assessment { score: -100, evidence: vec![("anticheat", -100)], verdict: Verdict::Blocked }; }
    let gfx = info.gfx.iter().any(|g| *g != "DXGI");
    if gfx { ev.push(("graphics_api", 4)); }
    if fullscreen { ev.push(("fullscreen", 2)); }
    if has(ENGINES) || info.exe.ends_with("-win64-shipping.exe") || path.contains("\\binaries\\win64\\") { ev.push(("game_engine", 3)); }
    let rt = count(RUNTIMES).min(3); if rt > 0 { ev.push(("game_runtime", rt)); }
    if has(UPSCALERS) { ev.push(("upscaler", 3)); }
    if STORE_DIRS.iter().any(|d| path.contains(d)) { ev.push(("store_path", 3)); } else if GAME_DIRS.iter().any(|d| path.contains(d)) { ev.push(("game_dir", 1)); }
    if has(UI_FRAMEWORKS) { ev.push(("ui_framework", -8)); }
    if non_game_name(&info.exe) { ev.push(("non_game_name", -10)); } else if info.exe.contains("game") || info.exe.contains("shipping") { ev.push(("game_name", 1)); }
    let score = ev.iter().map(|e| e.1).sum();
    let verdict = if !gfx && score > 0 { Verdict::Waiting } else if score >= 6 { Verdict::Game } else { Verdict::NotGame };
    Assessment { score, evidence: ev, verdict }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(exe: &str, path: &str, mods: &[&str]) -> ProcInfo {
        let modules: Vec<String> = mods.iter().map(|s| s.to_string()).collect();
        let gfx = crate::limiter::GFX_MODULES.iter().filter(|(m, _)| modules.iter().any(|n| n == m)).map(|(_, a)| *a).collect();
        ProcInfo { exe: exe.into(), path: path.into(), is32: false, gfx, anticheat: None, hooked: false, modules, overlays: vec![], packaged: false, signed_only: false, no_dynamic_code: false }
    }
    #[test] fn steam_unity_game_is_a_game() {
        let a = assess(&info("hollow.exe", r"D:\SteamLibrary\steamapps\common\Hollow\hollow.exe", &["d3d11.dll", "unityplayer.dll", "steam_api64.dll"]), true);
        assert_eq!(a.verdict, Verdict::Game, "{}", a.describe());
    }
    #[test] fn qt_tool_with_opengl_is_not() {
        let a = assess(&info("obs-like.exe", r"C:\Program Files\Tool\obs-like.exe", &["opengl32.dll", "qt6core.dll"]), false);
        assert_eq!(a.verdict, Verdict::NotGame);
    }
    #[test] fn launcher_is_not() {
        let a = assess(&info("gamelauncher.exe", r"C:\Games\X\gamelauncher.exe", &["d3d11.dll", "libcef.dll"]), false);
        assert_eq!(a.verdict, Verdict::NotGame);
    }
    #[test] fn protected_title_is_blocked() { assert!(blocked_title("valorant-win64-shipping.exe")); assert!(!blocked_title("hollow.exe")); }
}
