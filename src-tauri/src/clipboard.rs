use crate::model::ClipboardSnapshot;
use crate::runtime::NodeRuntime;
use arboard::{Clipboard, ImageData};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MAX_CLIPBOARD_IMAGE_DIMENSION: u32 = 8_192;
const MAX_CLIPBOARD_IMAGE_ALLOCATION: u64 = 128 * 1024 * 1024;

pub fn start_monitor(runtime: Arc<NodeRuntime>) {
    thread::Builder::new()
        .name("meshshare-clipboard".to_string())
        .spawn(move || monitor_loop(runtime))
        .expect("clipboard monitor thread");
}

fn monitor_loop(runtime: Arc<NodeRuntime>) {
    let mut clipboard = match Clipboard::new() {
        Ok(value) => value,
        Err(_) => {
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(2));
            }
            return;
        }
    };
    let mut last_hash = String::new();
    while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        if !runtime
            .sync_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            thread::sleep(Duration::from_millis(500));
            continue;
        }
        if let Some((text, image)) = read_payload(&mut clipboard) {
            let hash = payload_hash(text.as_deref(), image.as_deref());
            if hash != last_hash {
                last_hash = hash.clone();
                if runtime.is_clipboard_suppressed(&hash) {
                    thread::sleep(Duration::from_millis(350));
                    continue;
                }
                if let Some(event) = runtime.create_local_clipboard(text, image) {
                    runtime.publish_event(crate::model::WireMessage::ClipboardUpdate {
                        event: event.clone(),
                    });
                    runtime.send_to_peers_except(
                        None,
                        crate::model::WireMessage::ClipboardUpdate { event },
                    );
                }
            }
        }
        thread::sleep(Duration::from_millis(350));
    }
}

fn read_payload(clipboard: &mut Clipboard) -> Option<(Option<String>, Option<Vec<u8>>)> {
    let text = clipboard.get_text().ok().filter(|value| !value.is_empty());
    let image = clipboard
        .get_image()
        .ok()
        .and_then(|value| encode_image(&value));
    if text.is_none() && image.is_none() {
        None
    } else {
        Some((text, image))
    }
}

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

pub fn apply_snapshot(snapshot: &ClipboardSnapshot, runtime: &NodeRuntime) -> Result<(), String> {
    let text = snapshot.text.clone();
    let image = snapshot
        .image_png
        .as_deref()
        .map(|value| BASE64.decode(value).map_err(|error| error.to_string()))
        .transpose()?;
    let hash = payload_hash(text.as_deref(), image.as_deref());
    runtime.set_clipboard_suppression(hash);
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    if let Some(image_png) = image {
        let decoded = decode_png(&image_png)?.to_rgba8();
        let image = ImageData {
            width: decoded.width() as usize,
            height: decoded.height() as usize,
            bytes: Cow::Owned(decoded.into_raw()),
        };
        clipboard
            .set_image(image)
            .map_err(|error| error.to_string())?;
    } else if let Some(text) = text {
        clipboard
            .set_text(text)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
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
