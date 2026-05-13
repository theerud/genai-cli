//! Filesystem and stdout helpers shared between the one-off CLI path and
//! the REPL command handlers. Anything that writes audio, image, or
//! arbitrary-blob output to disk lives here so the two callers stay in sync.

use anyhow::{Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::gemini::image::{self as image_api, ImageOut, InputImage};
use crate::gemini::tts::{AudioOut, extension_for_mime, pcm16_to_wav};
use crate::session::attachment;

/// Expand a leading `~/` against `$HOME`. Anything else is passed through.
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
/// stem and we append `-<n>` plus a per-image extension.
pub fn write_images(output: &str, images: &[ImageOut]) -> Result<()> {
    if output == "-" {
        if images.len() > 1 {
            bail!("multiple images: cannot write all to stdout");
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&images[0].bytes)?;
        return Ok(());
    }
    if images.len() == 1 {
        let path = PathBuf::from(expand_path(output));
        create_parent_dirs(&path)?;
        std::fs::write(&path, &images[0].bytes)?;
        eprintln!("wrote {}", path.display());
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
        eprintln!("wrote {}", path.display());
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
