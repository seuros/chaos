use std::path::Path;
use std::path::PathBuf;
use tempfile::Builder;

#[derive(Debug, Clone)]
pub enum PasteImageError {
    ClipboardUnavailable(String),
    NoImage(String),
    EncodeFailed(String),
    IoError(String),
}

impl std::fmt::Display for PasteImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteImageError::ClipboardUnavailable(msg) => write!(f, "clipboard unavailable: {msg}"),
            PasteImageError::NoImage(msg) => write!(f, "no image on clipboard: {msg}"),
            PasteImageError::EncodeFailed(msg) => write!(f, "could not encode image: {msg}"),
            PasteImageError::IoError(msg) => write!(f, "io error: {msg}"),
        }
    }
}
impl std::error::Error for PasteImageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedImageFormat {
    Png,
    Jpeg,
    Other,
}

impl EncodedImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            EncodedImageFormat::Png => "PNG",
            EncodedImageFormat::Jpeg => "JPEG",
            EncodedImageFormat::Other => "IMG",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PastedImageInfo {
    pub width: u32,
    pub height: u32,
    pub encoded_format: EncodedImageFormat, // Always PNG for now.
}

/// Capture image from system clipboard, encode to PNG, and return bytes + info.
pub fn paste_image_as_png() -> Result<(Vec<u8>, PastedImageInfo), PasteImageError> {
    let _span = tracing::debug_span!("paste_image_as_png").entered();
    tracing::debug!("attempting clipboard image read");
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()))?;
    // Sometimes images on the clipboard come as files (e.g. when copy/pasting from
    // Finder), sometimes they come as image data (e.g. when pasting from Chrome).
    // Accept both, and prefer files if both are present.
    let files = cb
        .get()
        .file_list()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()));
    let dyn_img = if let Some(img) = files
        .unwrap_or_default()
        .into_iter()
        .find_map(|f| image::open(f).ok())
    {
        tracing::debug!(
            "clipboard image opened from file: {}x{}",
            img.width(),
            img.height()
        );
        img
    } else {
        let _span = tracing::debug_span!("get_image").entered();
        let img = cb
            .get_image()
            .map_err(|e| PasteImageError::NoImage(e.to_string()))?;
        let w = img.width as u32;
        let h = img.height as u32;
        tracing::debug!("clipboard image opened from image: {}x{}", w, h);

        let Some(rgba_img) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return Err(PasteImageError::EncodeFailed("invalid RGBA buffer".into()));
        };

        image::DynamicImage::ImageRgba8(rgba_img)
    };

    let mut png: Vec<u8> = Vec::new();
    {
        let span =
            tracing::debug_span!("encode_image", byte_length = tracing::field::Empty).entered();
        let mut cursor = std::io::Cursor::new(&mut png);
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
        span.record("byte_length", png.len());
    }

    Ok((
        png,
        PastedImageInfo {
            width: dyn_img.width(),
            height: dyn_img.height(),
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

/// Convenience: write to a temp file and return its path + info.
pub fn paste_image_to_temp_png() -> Result<(PathBuf, PastedImageInfo), PasteImageError> {
    // First attempt: read image from system clipboard via arboard (native paths or image data).
    match paste_image_as_png() {
        Ok((png, info)) => {
            // Create a unique temporary file with a .png suffix to avoid collisions.
            let tmp = Builder::new()
                .prefix("chaos-clipboard-")
                .suffix(".png")
                .tempfile()
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            std::fs::write(tmp.path(), &png)
                .map_err(|e| PasteImageError::IoError(e.to_string()))?;
            // Persist the file (so it remains after the handle is dropped) and return its PathBuf.
            let (_file, path) = tmp
                .keep()
                .map_err(|e| PasteImageError::IoError(e.error.to_string()))?;
            Ok((path, info))
        }
        Err(e) => Err(e),
    }
}

/// Normalize pasted text that may represent a filesystem path.
///
/// Supports:
/// - `file://` URLs (converted to local paths)
/// - Windows/UNC paths
/// - shell-escaped single paths (via `shlex`)
pub fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| pasted.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(pasted);

    // file:// URL → filesystem path
    if let Ok(url) = url::Url::parse(unquoted)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }

    // TODO: We'll improve the implementation/unit tests over time, as appropriate.
    // Possibly use typed-path.
    //
    // Detect unquoted Windows paths and bypass POSIX shlex which
    // treats backslashes as escapes (e.g., C:\Users\Alice\file.png).
    // Also handles UNC paths (\\server\share\path).
    if let Some(path) = normalize_windows_path(unquoted) {
        return Some(path);
    }

    // shell-escaped single path → unescaped
    let parts: Vec<String> = shlex::Shlex::new(pasted).collect();
    if parts.len() == 1 {
        let part = parts.into_iter().next()?;
        if let Some(path) = normalize_windows_path(&part) {
            return Some(path);
        }
        return Some(PathBuf::from(part));
    }

    None
}

fn normalize_windows_path(input: &str) -> Option<PathBuf> {
    // Drive letter path: C:\ or C:/
    let drive = input
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
        && input.get(1..2) == Some(":")
        && input
            .get(2..3)
            .map(|s| s == "\\" || s == "/")
            .unwrap_or(false);
    // UNC path: \\server\share
    let unc = input.starts_with("\\\\");
    if !drive && !unc {
        return None;
    }

    Some(PathBuf::from(input))
}

/// Infer an image format for the provided path based on its extension.
pub fn pasted_image_format(path: &Path) -> EncodedImageFormat {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => EncodedImageFormat::Png,
        Some("jpg") | Some("jpeg") => EncodedImageFormat::Jpeg,
        _ => EncodedImageFormat::Other,
    }
}

#[cfg(test)]
mod pasted_paths_tests {
    use super::*;

    #[test]
    fn normalize_file_url() {
        let input = "file:///tmp/example.png";
        let result = normalize_pasted_path(input).expect("should parse file URL");
        assert_eq!(result, PathBuf::from("/tmp/example.png"));
    }

    #[test]
    fn normalize_shell_escaped_single_path() {
        let input = "/home/user/My\\ File.png";
        let result = normalize_pasted_path(input).expect("should unescape shell-escaped path");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_simple_quoted_path_fallback() {
        let input = "\"/home/user/My File.png\"";
        let result = normalize_pasted_path(input).expect("should trim simple quotes");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_single_quoted_unix_path() {
        let input = "'/home/user/My File.png'";
        let result = normalize_pasted_path(input).expect("should trim single quotes via shlex");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_multiple_tokens_returns_none() {
        // Two tokens after shell splitting → not a single path
        let input = "/home/user/a\\ b.png /home/user/c.png";
        let result = normalize_pasted_path(input);
        assert!(result.is_none());
    }

    #[test]
    fn pasted_image_format_png_jpeg_unknown() {
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.PNG")),
            EncodedImageFormat::Png
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.jpg")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.JPEG")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c")),
            EncodedImageFormat::Other
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.webp")),
            EncodedImageFormat::Other
        );
    }
}
