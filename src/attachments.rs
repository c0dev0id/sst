//! Attachments: staging local files, uploading them, and downloading received ones.

use std::path::{Path, PathBuf};

use presage::Manager;
use presage::libsignal_service::content::Content;
use presage::libsignal_service::proto::AttachmentPointer;
use presage::libsignal_service::sender::AttachmentSpec;
use presage::manager::Registered;
use presage::store::Store;

use crate::signal::data_message_of;

#[derive(Clone)]
pub struct StagedAttachment {
    pub path: PathBuf,
    pub mime: &'static str,
    pub size: u64,
}

pub fn stage_attachment(path: &Path) -> Result<StagedAttachment, String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    Ok(StagedAttachment {
        path: path.to_path_buf(),
        mime: mime_from_path(path),
        size: meta.len(),
    })
}

fn mime_from_path(path: &Path) -> &'static str {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png"  => "image/png",
        "gif"  => "image/gif",
        "webp" => "image/webp",
        "heic" | "heif" => "image/heic",
        "mp4"  => "video/mp4",
        "mov"  => "video/quicktime",
        "avi"  => "video/x-msvideo",
        "webm" => "video/webm",
        "mkv"  => "video/x-matroska",
        "mp3"  => "audio/mpeg",
        "m4a"  => "audio/mp4",
        "ogg"  => "audio/ogg",
        "opus" => "audio/opus",
        "aac"  => "audio/aac",
        "flac" => "audio/flac",
        "wav"  => "audio/wav",
        "pdf"  => "application/pdf",
        "zip"  => "application/zip",
        "tar"  => "application/x-tar",
        "gz"   => "application/gzip",
        "bz2"  => "application/x-bzip2",
        _      => "application/octet-stream",
    }
}

pub(crate) fn kind_from_mime(mime: &str) -> &'static str {
    if mime == "image/gif" { "gif" }
    else if mime.starts_with("image/") { "image" }
    else if mime.starts_with("video/") { "video" }
    else if mime.starts_with("audio/") { "audio" }
    else { "file" }
}

// Signal caps attachments at ~100 MB, well below the u64→f64 precision boundary (2^53).
#[allow(clippy::cast_precision_loss)]
pub fn fmt_attachment_size(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{} B", bytes)
    } else if bytes < 1_024 * 1_024 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

const ATTACHMENT_FLAG_GIF: u32 = 8;

/// Upload staged attachments one by one; on any failure returns an error with
/// a human-readable message and the path of the file that failed, so the caller
/// can remove just that entry and let the user retry.
pub async fn upload_staged_attachments<S: Store>(
    manager: &mut Manager<S, Registered>,
    staged: &[StagedAttachment],
) -> Result<Vec<AttachmentPointer>, (String, PathBuf)> {
    let mut pointers = Vec::with_capacity(staged.len());
    for att in staged {
        let bytes = std::fs::read(&att.path)
            .map_err(|e| (format!("{}: {}", att.path.display(), e), att.path.clone()))?;
        let is_gif = att.mime == "image/gif";
        let spec = AttachmentSpec {
            content_type: att.mime.to_string(),
            length: bytes.len(),
            file_name: att.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut pointer = match manager.upload_attachment(spec, bytes).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Err((format!("upload failed: {e}"), att.path.clone())),
            Err(e)     => return Err((format!("upload error: {e}"), att.path.clone())),
        };
        if is_gif {
            pointer.flags = Some(pointer.flags.unwrap_or(0) | ATTACHMENT_FLAG_GIF);
        }
        pointers.push(pointer);
    }
    Ok(pointers)
}

/// Attachments the user may download. View-once media is excluded: linked
/// devices show a placeholder for it, as the official clients do.
pub fn message_attachments(content: &Content) -> &[AttachmentPointer] {
    match data_message_of(content) {
        Some(dm) if dm.is_view_once != Some(true) => &dm.attachments,
        _ => &[],
    }
}

/// Derive a filename for a downloaded attachment.
/// Uses the pointer's fileName field when present; falls back to `attachment_N.<ext>`.
pub fn attachment_filename(pointer: &AttachmentPointer, idx: usize) -> String {
    if let Some(name) = &pointer.file_name
        && !name.is_empty()
    {
        return name.clone();
    }
    let ext = pointer.content_type.as_deref()
        .and_then(|ct| ct.split('/').nth(1))
        .unwrap_or("bin");
    format!("attachment_{}.{}", idx, ext)
}

/// Return a path in `dir` for `filename` that does not yet exist.
/// Inserts `_N` before the extension on each collision.
pub fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate.file_stem().and_then(|s| s.to_str()).unwrap_or(filename);
    let ext  = candidate.extension().and_then(|e| e.to_str());
    for n in 1u32.. {
        let name = match ext {
            Some(e) => format!("{}_{}.{}", stem, n, e),
            None    => format!("{}_{}", stem, n),
        };
        let candidate = dir.join(&name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Download and decrypt one attachment, writing atomically via a `.tmp` sibling.
/// If the process is killed mid-download nothing is written; if killed mid-write
/// only an orphaned `.tmp` is left, never a partial real file.
pub async fn download_attachment<S: Store>(
    manager: &Manager<S, Registered>,
    pointer: &AttachmentPointer,
    dir: &Path,
    filename: &str,
) -> Result<PathBuf, String> {
    let final_path = unique_path(dir, filename);
    let mut tmp_name = final_path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);

    let bytes = manager.get_attachment(pointer).await
        .map_err(|e| format!("download failed: {e}"))?;
    std::fs::write(&tmp_path, &bytes)
        .map_err(|e| format!("{}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("rename failed: {e}"))?;
    Ok(final_path)
}
