#[cfg(feature = "gui")]
use crate::model::ClipboardSession;
#[cfg(all(feature = "gui", not(target_os = "android")))]
use crate::model::ClipboardSnapshot;
#[cfg(feature = "gui")]
use crate::runtime::NodeRuntime;
#[cfg(all(feature = "gui", not(target_os = "android")))]
use arboard::{Clipboard, Error as ClipboardError, ImageData};
#[cfg(all(feature = "gui", not(target_os = "android")))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::Limits;
use image::{DynamicImage, ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
#[cfg(all(feature = "gui", not(target_os = "android")))]
use std::borrow::Cow;
use std::io::Cursor;
#[cfg(feature = "gui")]
use std::sync::Arc;
#[cfg(all(feature = "gui", not(target_os = "android")))]
use std::thread;
#[cfg(all(feature = "gui", not(target_os = "android")))]
use std::time::Duration;

const MAX_CLIPBOARD_IMAGE_DIMENSION: u32 = 8_192;
const MAX_CLIPBOARD_IMAGE_ALLOCATION: u64 = 128 * 1024 * 1024;
#[cfg(all(feature = "gui", not(target_os = "android")))]
type ClipboardPayload = (Option<String>, Option<ImageData<'static>>);

#[cfg(all(feature = "gui", not(target_os = "android")))]
pub fn start_monitor(runtime: Arc<NodeRuntime>) {
    thread::Builder::new()
        .name("meshshare-clipboard".to_string())
        .spawn(move || monitor_loop(runtime))
        .expect("clipboard monitor thread");
}

#[cfg(all(feature = "gui", target_os = "android"))]
pub fn start_monitor(runtime: Arc<NodeRuntime>) {
    runtime.set_clipboard_unavailable(
        ClipboardSession::Other,
        "Android App 不自动监听系统剪贴板，请通过共享剪贴板面板手动操作",
        false,
    );
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn monitor_loop(runtime: Arc<NodeRuntime>) {
    let mut last_hash = String::new();
    'monitor: loop {
        if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let mut clipboard = match Clipboard::new() {
            Ok(value) => {
                runtime.set_clipboard_available(current_session());
                value
            }
            Err(error) => {
                runtime.set_clipboard_unavailable(current_session(), error.to_string(), true);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        loop {
            if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break 'monitor;
            }
            let observed_event_id = runtime.clipboard_event_id();
            if let Some(snapshot) = runtime.take_system_clipboard_write() {
                match write_snapshot(&mut clipboard, snapshot) {
                    Ok(hash) => {
                        if let Some(hash) = hash {
                            last_hash = hash;
                        }
                        runtime.set_clipboard_available(current_session());
                    }
                    Err(error) => {
                        eprintln!("写入系统剪贴板失败：{error}");
                        runtime.set_clipboard_unavailable(current_session(), error, true);
                    }
                }
                continue;
            }
            if !runtime
                .sync_enabled
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                runtime.wait_for_system_clipboard_write(Duration::from_millis(500));
                continue;
            }
            let payload = read_payload(&mut clipboard);
            if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break 'monitor;
            }
            match payload {
                Ok(payload) => {
                    runtime.set_clipboard_available(current_session());
                    if let Some((text, image)) = payload {
                        let hash = system_payload_hash(text.as_deref(), image.as_ref());
                        if hash != last_hash {
                            last_hash = hash;
                            let image = image.as_ref().and_then(encode_image);
                            runtime.create_local_clipboard(
                                text,
                                image,
                                observed_event_id.as_deref(),
                            );
                        }
                    }
                }
                Err(error) => {
                    runtime.set_clipboard_unavailable(current_session(), error, true);
                    // 重建 Clipboard 前退避，避免读取持续失败时空转。
                    thread::sleep(Duration::from_secs(2));
                    break;
                }
            }
            runtime.wait_for_system_clipboard_write(Duration::from_millis(350));
        }
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn current_session() -> ClipboardSession {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            ClipboardSession::Wayland
        } else if std::env::var_os("DISPLAY").is_some() {
            ClipboardSession::X11
        } else {
            ClipboardSession::Unknown
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        ClipboardSession::Other
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn read_payload(clipboard: &mut Clipboard) -> Result<Option<ClipboardPayload>, String> {
    let text = match clipboard.get_text() {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(ClipboardError::ContentNotAvailable) => None,
        Err(error) => return Err(error.to_string()),
    };
    // 图片读取失败在 Wayland 等环境很常见；文字可用时继续同步文字，
    // 只有两者都取不到才算剪贴板不可用。
    let image = match clipboard.get_image() {
        Ok(value) => Some(value),
        Err(ClipboardError::ContentNotAvailable) => None,
        Err(error) => {
            if text.is_none() {
                return Err(error.to_string());
            }
            None
        }
    };
    if text.is_none() && image.is_none() {
        Ok(None)
    } else {
        Ok(Some((text, image)))
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn encode_image(image: &ImageData<'_>) -> Option<Vec<u8>> {
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.to_vec(),
    )?;
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .ok()?;
    Some(bytes)
}

/// 系统返回的是像素，PNG 的编码方式可能与写入前不同。
#[cfg(all(feature = "gui", not(target_os = "android")))]
fn system_payload_hash(text: Option<&str>, image: Option<&ImageData<'_>>) -> String {
    let mut hasher = Sha256::new();
    if let Some(image) = image {
        hasher.update(b"rgba\0");
        hasher.update(image.width.to_le_bytes());
        hasher.update(image.height.to_le_bytes());
        hasher.update(&image.bytes);
    }
    if let Some(text) = text {
        hasher.update(b"text\0");
        hasher.update(text.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn decode_png(bytes: &[u8]) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_CLIPBOARD_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_CLIPBOARD_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_CLIPBOARD_IMAGE_ALLOCATION);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}

pub(crate) fn is_valid_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") && decode_png(bytes).is_ok()
}

pub fn payload_hash(text: Option<&str>, image_png: Option<&[u8]>) -> String {
    let mut hasher = Sha256::new();
    if let Some(text) = text {
        hasher.update(b"text\0");
        hasher.update(text.as_bytes());
    }
    if let Some(image) = image_png {
        hasher.update(b"image/png\0");
        hasher.update(image);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn write_snapshot(
    clipboard: &mut Clipboard,
    snapshot: ClipboardSnapshot,
) -> Result<Option<String>, String> {
    if let Some(encoded) = snapshot.image_png {
        let bytes = BASE64.decode(encoded).map_err(|error| error.to_string())?;
        let decoded = decode_png(&bytes)?.to_rgba8();
        let image = ImageData {
            width: decoded.width() as usize,
            height: decoded.height() as usize,
            bytes: Cow::Owned(decoded.into_raw()),
        };
        let hash = system_payload_hash(None, Some(&image));
        clipboard
            .set_image(image)
            .map_err(|error| error.to_string())?;
        return Ok(Some(hash));
    }
    if let Some(text) = snapshot.text {
        let hash = system_payload_hash(Some(&text), None);
        clipboard
            .set_text(text)
            .map_err(|error| error.to_string())?;
        return Ok(Some(hash));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{is_valid_png, payload_hash};
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    #[test]
    fn payload_hash_changes_when_text_or_image_changes() {
        let text_hash = payload_hash(Some("hello"), None);
        assert_ne!(text_hash, payload_hash(Some("world"), None));
        assert_ne!(text_hash, payload_hash(Some("hello"), Some(&[1, 2, 3])));
        assert_eq!(text_hash, payload_hash(Some("hello"), None));
    }

    #[test]
    fn png_validation_rejects_excessive_dimensions() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8_193, 1, Rgba([0, 0, 0, 255])));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode png");
        assert!(!is_valid_png(&bytes));
    }
}
