use std::path::{Component, Path, PathBuf};

/// Strips Windows `\\?\` extended-length path prefixes produced by `canonicalize`.
fn normalize_windows_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc_tail) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc_tail}"));
    }
    if let Some(local_tail) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(local_tail);
    }
    path.to_path_buf()
}

/// Resolves relative paths and normalizes `.` / `..` components without requiring the path to exist.
fn normalize_absolute_components(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        normalize_windows_prefix(path)
    } else {
        let joined = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path);
        normalize_windows_prefix(&joined)
    };

    let mut clean = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    clean
}

/// Converts a filesystem path into a clean native Windows absolute path string
/// (e.g. `D:\Downloads\crachás teste\imagens\asset_015.jpg`), preserving literal UTF-8
/// spaces/accents with backslash separators and without `file:///` or percent-encoding.
pub fn path_to_native_absolute(path: &Path) -> String {
    let clean = normalize_absolute_components(path);
    clean.to_string_lossy().replace('/', "\\")
}

/// Formats an exported relative asset path (`imagens/asset_001.jpg`) into the native Windows
/// absolute path inside `export_dir` (e.g. `D:\Downloads\crachás teste\imagens\asset_001.jpg`).
pub fn format_asset_target(export_dir: &Path, rel_path: &str) -> String {
    let clean_rel = rel_path.replace('\\', "/");
    let clean_rel = clean_rel.trim_start_matches('/');
    let mut full = normalize_absolute_components(export_dir);
    for seg in clean_rel.split('/').filter(|s| !s.is_empty()) {
        full.push(seg);
    }
    path_to_native_absolute(&full)
}

/// Extracts `imagens/<filename>` from a native Windows path, UNC path, legacy `file:///...` URI,
/// or legacy relative `imagens/<filename>` value.
pub fn extract_asset_rel_path(raw_value: &str) -> Option<String> {
    let normalized = raw_value.trim().replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }
    if let Some(idx) = normalized.rfind("/imagens/") {
        let file_part = &normalized[idx + "/imagens/".len()..];
        if !file_part.is_empty() && !file_part.contains('/') {
            return Some(format!("imagens/{}", percent_decode_basic(file_part)));
        }
    }
    if let Some(file_part) = normalized.strip_prefix("imagens/")
        && !file_part.is_empty()
        && !file_part.contains('/')
    {
        return Some(format!("imagens/{}", percent_decode_basic(file_part)));
    }
    None
}

fn percent_decode_basic(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_native_absolute_preserves_windows_backslashes_spaces_and_accents() {
        let target = format_asset_target(
            Path::new(r"D:\Downloads\crachás teste"),
            "imagens/asset_015.jpg",
        );
        assert_eq!(target, r"D:\Downloads\crachás teste\imagens\asset_015.jpg");
        assert!(!target.starts_with("file://"));
        assert!(!target.contains("%20"));
        assert!(!target.contains("%C3"));
        assert!(!target.contains('/'));

        let with_fwd = format_asset_target(
            Path::new("D:/João Carlos (2026)/Lote #1/"),
            "imagens/asset_001.jpg",
        );
        assert_eq!(
            with_fwd,
            r"D:\João Carlos (2026)\Lote #1\imagens\asset_001.jpg"
        );

        let with_extended = format_asset_target(
            Path::new(r"\\?\D:\Downloads\crachás teste"),
            "imagens/asset_002.png",
        );
        assert_eq!(
            with_extended,
            r"D:\Downloads\crachás teste\imagens\asset_002.png"
        );

        let unc = format_asset_target(
            Path::new(r"\\192.168.0.12\semuce\crachás teste"),
            "imagens/asset_003.jpg",
        );
        assert_eq!(
            unc,
            r"\\192.168.0.12\semuce\crachás teste\imagens\asset_003.jpg"
        );
    }

    #[test]
    fn extract_asset_rel_path_handles_native_windows_relative_and_legacy_uris() {
        assert_eq!(
            extract_asset_rel_path(r"D:\Downloads\crachás teste\imagens\asset_015.jpg").as_deref(),
            Some("imagens/asset_015.jpg")
        );
        assert_eq!(
            extract_asset_rel_path("imagens/asset_001.jpg").as_deref(),
            Some("imagens/asset_001.jpg")
        );
        assert_eq!(
            extract_asset_rel_path("file:///D:/Crach%C3%A1s%20#1/imagens/asset_042.png").as_deref(),
            Some("imagens/asset_042.png")
        );
    }
}
