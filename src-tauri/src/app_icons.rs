//! Extracts real 32x32 Windows executable icons as PNG data URLs, cached per process name.
//! Uses only Win32 Shell/GDI APIs already linked by `windows` and a minimal pure-Rust stored-DEFLATE
//! PNG encoder (zero extra dependencies, works offline).

use base64::Engine;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ICON_CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns a `data:image/png;base64,...` icon for `process_name` (e.g. `"claude.exe"`), or `None`.
pub fn icon_data_url(process_name: &str) -> Option<String> {
    let trimmed = process_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let key = trimmed.to_lowercase();
    if let Some(hit) = cache().lock().get(&key) {
        return hit.clone();
    }

    let result = find_exe_path(trimmed).and_then(|p| extract_icon_png(&p)).map(|bytes| {
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!("data:image/png;base64,{b64}")
    });

    // Only cache positive hits permanently; negative lookups stay cached too unless the process
    // isn't running yet, so we cap the cache cleanly and avoid repeated disk scans.
    let mut guard = cache().lock();
    if guard.len() > 256 {
        guard.clear();
    }
    guard.insert(key, result.clone());
    result
}

/// Locate the executable path for a process file name (running processes first, then common Windows dirs).
fn find_exe_path(process_name: &str) -> Option<PathBuf> {
    let needle = if process_name.to_lowercase().ends_with(".exe") {
        process_name.to_lowercase()
    } else {
        format!("{}.exe", process_name.to_lowercase())
    };

    if let Some(p) = find_running_process_path(&needle) {
        return Some(p);
    }

    // Check PATH entries directly.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(&needle);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Check standard Windows directories (shallow: depth 0..=2) so closed apps still resolve icons.
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "WINDIR"] {
        if let Some(v) = std::env::var_os(var) {
            let base = PathBuf::from(v);
            if var == "LOCALAPPDATA" {
                roots.push(base.join("Programs"));
                roots.push(base.join("Microsoft").join("WindowsApps"));
                roots.push(base.clone());
            } else if var == "WINDIR" {
                roots.push(base.join("System32"));
                roots.push(base);
            } else {
                roots.push(base);
            }
        }
    }

    for root in roots {
        if let Some(found) = find_in_dir(&root, &needle, 2) {
            return Some(found);
        }
    }
    None
}

fn find_in_dir(dir: &Path, needle_lower: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join(needle_lower);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten().take(160) {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_file() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(needle_lower))
            {
                return Some(path);
            }
        } else if ft.is_dir() && depth > 1 {
            if let Some(found) = find_in_dir(&path, needle_lower, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn find_running_process_path(needle_lower: &str) -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found: Option<PathBuf> = None;
        let mut more = Process32FirstW(snap, &mut pe).is_ok();
        while more {
            let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
            let exe_name = String::from_utf16_lossy(&pe.szExeFile[..len]);
            if exe_name.eq_ignore_ascii_case(needle_lower) {
                if let Ok(proc_handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pe.th32ProcessID) {
                    let mut buf = [0u16; 1024];
                    let mut size = buf.len() as u32;
                    if QueryFullProcessImageNameW(
                        proc_handle,
                        PROCESS_NAME_WIN32,
                        PWSTR(buf.as_mut_ptr()),
                        &mut size,
                    )
                    .is_ok()
                        && size > 0
                    {
                        let p = PathBuf::from(String::from_utf16_lossy(&buf[..size as usize]));
                        if p.is_file() {
                            found = Some(p);
                        }
                    }
                    let _ = CloseHandle(proc_handle);
                }
                if found.is_some() {
                    break;
                }
            }
            more = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Extract the Shell large icon (32x32) from `exe_path` and encode it as an RGBA PNG.
fn extract_icon_png(exe_path: &Path) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    };
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    struct IconGuard(HICON);
    impl Drop for IconGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = DestroyIcon(self.0);
                }
            }
        }
    }
    struct BitmapGuard(HBITMAP);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(self.0.0));
                }
            }
        }
    }
    struct DcGuard(HDC);
    impl Drop for DcGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = DeleteDC(self.0);
                }
            }
        }
    }

    let wide: Vec<u16> = exe_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut shfi = SHFILEINFOW::default();
    let res = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if res == 0 || shfi.hIcon.is_invalid() {
        return None;
    }
    let _icon = IconGuard(shfi.hIcon);

    let mut info = ICONINFO::default();
    if unsafe { GetIconInfo(shfi.hIcon, &mut info) }.is_err() {
        return None;
    }
    let _color = BitmapGuard(info.hbmColor);
    let _mask = BitmapGuard(info.hbmMask);

    if info.hbmColor.is_invalid() {
        return None;
    }

    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(info.hbmColor.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        )
    };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmWidth > 256 || bm.bmHeight > 256 {
        return None;
    }

    let width = bm.bmWidth as u32;
    let height = bm.bmHeight as u32;
    let dc = DcGuard(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return None;
    }

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bgra = vec![0u8; (width * height * 4) as usize];
    let lines = unsafe {
        GetDIBits(
            dc.0,
            info.hbmColor,
            0,
            height,
            Some(bgra.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return None;
    }

    // Legacy icons have alpha=0 across all pixels and store transparency in `hbmMask`.
    let has_alpha = bgra.chunks_exact(4).any(|px| px[3] != 0);
    if !has_alpha && !info.hbmMask.is_invalid() {
        let mut mask_bgra = vec![0u8; (width * height * 4) as usize];
        let mask_lines = unsafe {
            GetDIBits(
                dc.0,
                info.hbmMask,
                0,
                height,
                Some(mask_bgra.as_mut_ptr() as *mut core::ffi::c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        if mask_lines > 0 {
            for (px, m) in bgra.chunks_exact_mut(4).zip(mask_bgra.chunks_exact(4)) {
                px[3] = if m[0] == 0 { 255 } else { 0 };
            }
        } else {
            for px in bgra.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
    }

    // Convert BGRA -> RGBA for PNG.
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.push(px[2]);
        rgba.push(px[1]);
        rgba.push(px[0]);
        rgba.push(px[3]);
    }

    Some(encode_rgba_png(width, height, &rgba))
}

/// Encode an RGBA buffer as a valid uncompressed (stored-DEFLATE) PNG with zero external crates.
pub(crate) fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let stride = (width * 4) as usize;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in 0..height as usize {
        raw.push(0); // filter type 0 (None)
        raw.extend_from_slice(&rgba[row * stride..(row + 1) * stride]);
    }

    // Build zlib stream using stored (non-compressed) DEFLATE blocks.
    let mut zlib = Vec::with_capacity(raw.len() + 64);
    zlib.push(0x78); // CMF: deflate, 32K window
    zlib.push(0x01); // FLG: no dict, fastest, check bits make (0x7801 % 31 == 0)
    let mut pos = 0;
    while pos < raw.len() {
        let end = (pos + 65_535).min(raw.len());
        let is_final = end == raw.len();
        let len = (end - pos) as u16;
        let nlen = !len;
        zlib.push(if is_final { 1 } else { 0 });
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&nlen.to_le_bytes());
        zlib.extend_from_slice(&raw[pos..end]);
        pos = end;
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(zlib.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // colour type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &zlib);
    push_chunk(&mut out, b"IEND", &[]);
    out
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    crc = crc32_update(crc, kind);
    crc = crc32_update(crc, data);
    out.extend_from_slice(&(!crc).to_be_bytes());
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_encoder_produces_valid_decodable_png() {
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, // row 0
            0, 0, 255, 128, 255, 255, 255, 0, // row 1
        ];
        let png = encode_rgba_png(2, 2, &rgba);
        let decoded = tauri::image::Image::from_bytes(&png).expect("valid PNG");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 2);
        assert_eq!(decoded.rgba(), &rgba);
    }

    #[test]
    fn extracts_icon_from_windows_explorer() {
        let url = icon_data_url("explorer.exe");
        assert!(
            url.as_deref().is_some_and(|s| s.starts_with("data:image/png;base64,")),
            "expected PNG data URL for explorer.exe, got {url:?}"
        );
    }

    #[test]
    fn missing_executable_returns_none() {
        assert_eq!(icon_data_url("definitely-not-a-real-wren-app-xyz.exe"), None);
    }
}
