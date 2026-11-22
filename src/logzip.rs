//! Log history: a log past its size limit is deflated into `logs\flowscale-<date>.zip` (one entry, standard ZIP),
//! the newest `KEEP` archives kept.
use std::path::{Path, PathBuf};

const KEEP: usize = 10;

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() { let mut c = i as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }; } *e = c; }
        t
    });
    !data.iter().fold(!0u32, |c, &b| t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}

fn dos_time(t: &windows::Win32::Foundation::SYSTEMTIME) -> (u16, u16) {
    ((t.wHour << 11) | (t.wMinute << 5) | (t.wSecond / 2), ((t.wYear.saturating_sub(1980)) << 9) | (t.wMonth << 5) | t.wDay)
}

/// A one-entry ZIP holding `data` as `name`, deflated.
pub fn zip_one(name: &str, data: &[u8]) -> Vec<u8> {
    let comp = miniz_oxide::deflate::compress_to_vec(data, 6);
    let crc = crc32(data);
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let (tm, dt) = dos_time(&now);
    let n = name.as_bytes();
    let mut z = Vec::with_capacity(comp.len() + 128);
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    le32(&mut z, 0x0403_4b50); le16(&mut z, 20); le16(&mut z, 0x0800); le16(&mut z, 8); le16(&mut z, tm); le16(&mut z, dt);
    le32(&mut z, crc); le32(&mut z, comp.len() as u32); le32(&mut z, data.len() as u32); le16(&mut z, n.len() as u16); le16(&mut z, 0);
    z.extend_from_slice(n); z.extend_from_slice(&comp);
    let cd = z.len() as u32;
    le32(&mut z, 0x0201_4b50); le16(&mut z, 20); le16(&mut z, 20); le16(&mut z, 0x0800); le16(&mut z, 8); le16(&mut z, tm); le16(&mut z, dt);
    le32(&mut z, crc); le32(&mut z, comp.len() as u32); le32(&mut z, data.len() as u32); le16(&mut z, n.len() as u16);
    le16(&mut z, 0); le16(&mut z, 0); le16(&mut z, 0); le16(&mut z, 0); le32(&mut z, 0); le32(&mut z, 0);
    z.extend_from_slice(n);
    let cd_len = z.len() as u32 - cd;
    le32(&mut z, 0x0605_4b50); le16(&mut z, 0); le16(&mut z, 0); le16(&mut z, 1); le16(&mut z, 1); le32(&mut z, cd_len); le32(&mut z, cd); le16(&mut z, 0);
    z
}

/// Moves `log` aside and archives it on a background thread (the next line goes to a fresh file at once).
pub fn rotate(log: &Path) {
    let dir = log.parent().map(|p| p.join("logs")).unwrap_or_else(|| PathBuf::from("logs"));
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let stamp = format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    let stem = log.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "flowscale".into());
    let held = log.with_extension(format!("{stamp}.tmp"));
    if std::fs::rename(log, &held).is_err() { return; }
    std::thread::spawn(move || {
        let Ok(data) = std::fs::read(&held) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let zip = dir.join(format!("{stem}-{stamp}.zip"));
        if std::fs::write(&zip, zip_one(&format!("{stem}-{stamp}.log"), &data)).is_ok() { let _ = std::fs::remove_file(&held); }
        prune(&dir, &stem);
    });
}

fn prune(dir: &Path, stem: &str) {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "zip") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(stem))).collect();
    v.sort();
    while v.len() > KEEP { let _ = std::fs::remove_file(v.remove(0)); }
}

/// Archives left as `.tmp` by an exit mid-rotation are finished on the next start.
pub fn recover(log: &Path) {
    let Some(dir) = log.parent() else { return };
    let stem = log.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path(); let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with(&format!("{stem}.")) && n.ends_with(".tmp") {
            let stamp = n.trim_start_matches(&format!("{stem}.")).trim_end_matches(".tmp").to_string();
            if let Ok(data) = std::fs::read(&p) {
                let d = dir.join("logs"); let _ = std::fs::create_dir_all(&d);
                if std::fs::write(d.join(format!("{stem}-{stamp}.zip")), zip_one(&format!("{stem}-{stamp}.log"), &data)).is_ok() { let _ = std::fs::remove_file(&p); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_and_roundtrip() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
        let data = b"FlowScale log line\r\n".repeat(500);
        let z = super::zip_one("a.log", &data);
        assert_eq!(&z[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let clen = u32::from_le_bytes(z[18..22].try_into().unwrap()) as usize;
        let back = miniz_oxide::inflate::decompress_to_vec(&z[30 + 5..30 + 5 + clen]).unwrap();
        assert_eq!(back, data);
    }
}
