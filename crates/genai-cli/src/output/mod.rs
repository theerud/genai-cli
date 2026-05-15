//! Filesystem and stdout helpers shared between the one-off CLI path and
//! the REPL command handlers. Anything that writes audio, image, or
//! arbitrary-blob output to disk lives here so the two callers stay in sync.

pub mod image_preview;

use anyhow::{Context, Result, bail};
use imagesize::ImageType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::gemini::image::{self as image_api, ImageOut, InputImage};
use crate::gemini::tts::{AudioOut, extension_for_mime, pcm16_to_wav};
use crate::session::attachment;

/// One-line `[W×H format, N KB]` description for any image blob. Returns
/// just `[N KB]` when the format isn't recognized (header sniff failed) or
/// `[empty]` for zero-length input. Cheap — `imagesize` only reads headers.
pub fn describe_image(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "[empty]".to_string();
    }
    let size_kb = bytes.len() as f64 / 1024.0;
    match imagesize::blob_size(bytes) {
        Ok(s) => {
            let fmt = imagesize::image_type(bytes)
                .map(format_label)
                .unwrap_or("?");
            format!("[{}×{} {fmt}, {size_kb:.1} KB]", s.width, s.height)
        }
        Err(_) => format!("[{size_kb:.1} KB]"),
    }
}

fn format_label(t: ImageType) -> &'static str {
    match t {
        ImageType::Png => "png",
        ImageType::Jpeg => "jpeg",
        ImageType::Gif => "gif",
        ImageType::Webp => "webp",
        ImageType::Bmp => "bmp",
        ImageType::Tiff => "tiff",
        ImageType::Heif(_) => "heif",
        ImageType::Ico => "ico",
        ImageType::Psd => "psd",
        ImageType::Jxl => "jxl",
        _ => "?",
    }
}

/// Media generation flavors. Determines the filename prefix and which
/// config dir override applies when the user didn't pass `-o`.
#[derive(Debug, Clone, Copy)]
pub enum GeneratedKind {
    Image,
    Tts,
    Music,
}

impl GeneratedKind {
    fn prefix(&self) -> &'static str {
        match self {
            GeneratedKind::Image => "image",
            GeneratedKind::Tts => "tts",
            GeneratedKind::Music => "music",
        }
    }

    fn config_override<'a>(&self, cfg: &'a Config) -> Option<&'a str> {
        match self {
            GeneratedKind::Image => cfg.output.image_dir.as_deref(),
            GeneratedKind::Tts | GeneratedKind::Music => cfg.output.audio_dir.as_deref(),
        }
    }
}

/// Auto-generated path for media output when the caller didn't pass `-o`.
/// Returns a path *without* extension — `write_audio` and `write_images`
/// already append the right one based on the response mime.
///
/// Directory resolution: `[output].image_dir` / `[output].audio_dir`
/// (if set) → `<data_dir>/generated/`. The directory is created if it
/// doesn't exist.
pub fn default_generated_path(
    cfg: &Config,
    kind: GeneratedKind,
    prompt: &str,
) -> Result<PathBuf> {
    let base = match kind.config_override(cfg) {
        Some(dir) => PathBuf::from(expand_path(dir)),
        None => {
            let paths = crate::config::paths()?;
            paths.data_dir.join("generated")
        }
    };
    std::fs::create_dir_all(&base)
        .with_context(|| format!("creating {}", base.display()))?;
    let stem = format!(
        "{}-{}-{}",
        kind.prefix(),
        compact_timestamp(),
        slugify(prompt)
    );
    Ok(base.join(stem))
}

/// `YYYYMMDDhhmmss` UTC. Plenty unique for human-paced media generation.
fn compact_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let s_of_day = secs.rem_euclid(86400);
    let h = s_of_day / 3600;
    let m = (s_of_day % 3600) / 60;
    let s = s_of_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}{mo:02}{d:02}{h:02}{m:02}{s:02}")
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i32) + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Filesystem-safe slug derived from a prompt. ASCII-lowercase, hyphens
/// for separators, capped at 22 chars. Empty / non-ASCII-only input
/// returns `"untitled"`.
fn slugify(prompt: &str) -> String {
    const MAX_LEN: usize = 22;
    let mut out = String::with_capacity(MAX_LEN);
    let mut prev_dash = true; // suppresses leading dashes
    for ch in prompt.chars() {
        if out.len() >= MAX_LEN {
            break;
        }
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("untitled");
    }
    out
}

/// Expand a leading `~/` against `$HOME`. Anything else is passed through.
/// Imagen-only knobs: pass `-a / -n` (or their tool equivalents) only when
/// the model is an Imagen family one. For Gemini image models (the
/// nano-banana family), warn and drop — those want orientation phrasing
/// in the prompt instead.
pub fn imagen_image_params(
    model_id: &str,
    aspect: Option<&str>,
    count: Option<u32>,
) -> (Option<String>, Option<u32>) {
    let is_imagen = model_id.starts_with("imagen");
    if !is_imagen && (aspect.is_some() || count.is_some()) {
        eprintln!(
            "warning: aspect / count are honored only for Imagen models. \
             For {model_id}, describe orientation / variant count in the prompt."
        );
        return (None, None);
    }
    (aspect.map(String::from), count)
}

pub fn expand_path(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    s.to_string()
}

/// Write a single `AudioOut` to `output` (`"-"` for stdout). PCM blobs are
/// wrapped into WAV first. Mime/extension mismatches print a stderr warning
/// rather than failing.
pub fn write_audio(output: &str, audio: &AudioOut) -> Result<()> {
    let natural_ext = extension_for_mime(&audio.mime);
    let (bytes, ext): (std::borrow::Cow<[u8]>, &str) =
        if audio.mime.starts_with("audio/L16") || audio.mime.starts_with("audio/pcm") {
            let sr = audio.sample_rate.unwrap_or(24000);
            (
                std::borrow::Cow::Owned(pcm16_to_wav(&audio.bytes, sr, 1)),
                natural_ext,
            )
        } else {
            (std::borrow::Cow::Borrowed(audio.bytes.as_slice()), natural_ext)
        };

    if output == "-" {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&bytes)?;
        return Ok(());
    }

    let mut path = PathBuf::from(expand_path(output));
    let user_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    if user_ext.is_none() {
        path.set_extension(ext);
    } else if let Some(u) = &user_ext
        && u != ext
        && ext != "bin"
    {
        eprintln!(
            "warning: writing {} content to .{} file ({} would match the data)",
            audio.mime, u, ext
        );
    }
    create_parent_dirs(&path)?;
    std::fs::write(&path, &*bytes)?;
    eprintln!("wrote {} ({})", path.display(), audio.mime);
    Ok(())
}

/// Write one or more images. With multiple images, `output` is treated as a
/// stem and we append `-<n>` plus a per-image extension. After writing, an
/// in-terminal preview is attempted (silent no-op on unsupported terminals).
pub fn write_images(output: &str, images: &[ImageOut], preview: image_preview::Preference) -> Result<()> {
    if output == "-" {
        if images.len() > 1 {
            bail!("multiple images: cannot write all to stdout");
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&images[0].bytes)?;
        return Ok(());
    }
    if images.len() == 1 {
        let mut path = PathBuf::from(expand_path(output));
        // If the user (or auto-generated stem) gave us a path without
        // an extension, infer one from the response mime so the file
        // opens with the right viewer.
        if path.extension().is_none() {
            path.set_extension(image_api::extension_for_mime(&images[0].mime));
        }
        create_parent_dirs(&path)?;
        std::fs::write(&path, &images[0].bytes)?;
        eprintln!("wrote {} {}", path.display(), describe_image(&images[0].bytes));
        let _ = image_preview::show(preview, &images[0].bytes);
        return Ok(());
    }
    let base = PathBuf::from(expand_path(output));
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let ext_from_path = base.extension().and_then(|s| s.to_str()).map(String::from);
    let dir = base.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    for (i, img) in images.iter().enumerate() {
        let ext = ext_from_path
            .clone()
            .unwrap_or_else(|| image_api::extension_for_mime(&img.mime).to_string());
        let path = dir.join(format!("{stem}-{i}.{ext}"));
        std::fs::write(&path, &img.bytes)?;
        eprintln!("wrote {} {}", path.display(), describe_image(&img.bytes));
        let _ = image_preview::show(preview, &img.bytes);
    }
    Ok(())
}

/// Load image files from disk into the shape `generate_image` expects. Emits
/// a stderr warning if the detected MIME doesn't look like an image.
pub fn load_input_images(paths: &[String]) -> Result<Vec<InputImage>> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let expanded = expand_path(p);
        let path = PathBuf::from(&expanded);
        let att = attachment::load(&path)?;
        if !att.mime.starts_with("image/") {
            eprintln!("warning: {} is {}, not an image", path.display(), att.mime);
        }
        out.push(InputImage {
            mime: att.mime,
            bytes: att.bytes,
        });
    }
    Ok(out)
}

fn create_parent_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_empty() {
        assert_eq!(describe_image(&[]), "[empty]");
    }

    #[test]
    fn describe_unknown_format_falls_back_to_size() {
        let s = describe_image(b"not an image at all, just plain bytes");
        assert!(s.starts_with("["), "got {s}");
        assert!(s.ends_with("KB]"), "got {s}");
        assert!(!s.contains('×'), "got {s}");
    }

    #[test]
    fn slug_basic() {
        assert_eq!(slugify("a watercolor cat"), "a-watercolor-cat");
    }

    #[test]
    fn slug_truncates_at_word_or_letter_boundary() {
        // 30+ chars input → exactly 22 chars output (cap).
        let s = slugify("the quick brown fox jumps over a lazy dog");
        assert!(s.len() <= 22, "got {s:?}");
        assert!(!s.starts_with('-') && !s.ends_with('-'));
    }

    #[test]
    fn slug_strips_punctuation_and_collapses() {
        assert_eq!(slugify("Hello, world!!  Foo??"), "hello-world-foo");
    }

    #[test]
    fn slug_empty_falls_back() {
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("###"), "untitled");
        assert_eq!(slugify("日本語"), "untitled"); // non-ASCII only
    }

    #[test]
    fn describe_png_header() {
        // Minimal PNG: signature + IHDR with width=42 height=24
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        buf.extend_from_slice(&13u32.to_be_bytes()); // chunk len
        buf.extend_from_slice(b"IHDR");
        buf.extend_from_slice(&42u32.to_be_bytes()); // width
        buf.extend_from_slice(&24u32.to_be_bytes()); // height
        buf.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth + filler
        buf.extend_from_slice(&[0u8; 4]); // CRC
        let s = describe_image(&buf);
        assert!(s.contains("42×24"), "got {s}");
        assert!(s.contains("png"), "got {s}");
    }
}
