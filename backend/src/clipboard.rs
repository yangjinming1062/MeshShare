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
#[cfg(all(feature = "gui", target_os = "linux"))]
use std::sync::atomic::{AtomicU64, Ordering};
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
struct ClipboardRead {
    payload: Option<ClipboardPayload>,
    complete: bool,
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
pub fn start_monitor(runtime: Arc<NodeRuntime>) {
    let changes = ClipboardChanges::new();
    thread::Builder::new()
        .name("meshshare-clipboard".to_string())
        .spawn(move || monitor_loop(runtime, changes))
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
fn monitor_loop(runtime: Arc<NodeRuntime>, changes: ClipboardChanges) {
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
        let mut last_change = None;
        loop {
            if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break 'monitor;
            }
            let observed_event_id = runtime.clipboard_event_id();
            if let Some(snapshot) = runtime.take_system_clipboard_write() {
                #[cfg(target_os = "macos")]
                let result = {
                    let has_image = snapshot.image_png.is_some();
                    let result =
                        objc2::rc::autoreleasepool(|_| write_snapshot(&mut clipboard, snapshot));
                    if has_image {
                        release_image_allocations();
                    }
                    result
                };
                #[cfg(not(target_os = "macos"))]
                let result = write_snapshot(&mut clipboard, snapshot);
                match result {
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
            let payload = read_changed_payload(
                &mut last_change,
                || changes.current(),
                || read_payload(&mut clipboard),
            );
            if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break 'monitor;
            }
            match payload {
                Ok(payload) => {
                    runtime.set_clipboard_available(current_session());
                    if let Some((text, image)) = payload {
                        #[cfg(target_os = "macos")]
                        let large_image = image
                            .as_ref()
                            .is_some_and(|image| image.bytes.len() >= 16 * 1024 * 1024);
                        let hash = system_payload_hash(text.as_deref(), image.as_ref());
                        if hash != last_hash {
                            last_hash = hash;
                            let image_png = image.and_then(encode_image);
                            runtime.create_local_clipboard(
                                text,
                                image_png,
                                observed_event_id.as_deref(),
                            );
                        } else {
                            drop(image);
                        }
                        #[cfg(target_os = "macos")]
                        if large_image {
                            release_image_allocations();
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

#[cfg(all(feature = "gui", target_os = "macos"))]
fn release_image_allocations() {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut libc::malloc_zone_t, goal: usize) -> usize;
    }
    // 图片转换结束后回收系统分配器保留的空闲页，避免大块像素缓冲继续计入进程内存。
    unsafe {
        malloc_zone_pressure_relief(libc::malloc_default_zone(), 0);
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
struct ClipboardChanges {
    #[cfg(target_os = "linux")]
    x11: Option<Arc<AtomicU64>>,
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
impl ClipboardChanges {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            x11: Self::observe_x11(),
        }
    }

    #[cfg(target_os = "linux")]
    fn observe_x11() -> Option<Arc<AtomicU64>> {
        use gtk::prelude::{DisplayExtManual, ObjectExt};

        // Wayland 会话中的 arboard 可能使用 data-control 或回退 XWayland，无法用 X11 通知判断两者。
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return None;
        }
        let display = gtk::gdk::Display::default()?;
        if !display.backend().is_x11() || !display.supports_selection_notification() {
            return None;
        }
        let generation = Arc::new(AtomicU64::new(0));
        let owner_changes = generation.clone();
        let clipboard = gtk::Clipboard::for_display(&display, &gtk::gdk::SELECTION_CLIPBOARD);
        clipboard.connect_local("owner-change", false, move |_| {
            owner_changes.fetch_add(1, Ordering::Relaxed);
            None
        });
        Some(generation)
    }

    fn current(&self) -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            use objc2::rc::autoreleasepool;
            use objc2_app_kit::NSPasteboard;
            Some(autoreleasepool(|_| {
                NSPasteboard::generalPasteboard().changeCount() as u64
            }))
        }
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
            // Win32 返回零时无法判断是否更新，仍读取剪贴板。
            let count = unsafe { GetClipboardSequenceNumber() };
            (count != 0).then_some(u64::from(count))
        }
        #[cfg(target_os = "linux")]
        {
            self.x11
                .as_ref()
                .map(|generation| generation.load(Ordering::Relaxed))
        }
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn read_changed_payload(
    last_change: &mut Option<u64>,
    current_change: impl Fn() -> Option<u64>,
    read: impl FnOnce() -> Result<ClipboardRead, String>,
) -> Result<Option<ClipboardPayload>, String> {
    let change = current_change();
    if change.is_some() && *last_change == change {
        return Ok(None);
    }
    let result = read();
    // 文字和图片分别读取。如果期间又发生复制，下轮再读同一份快照。
    if change.is_some() && current_change() != change {
        return Ok(None);
    }
    let result = result?;
    // 空内容可能处于清空后尚未写入的间隙，部分读取也需要补读。
    if result.complete && result.payload.is_some() {
        *last_change = change;
    }
    Ok(result.payload)
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
fn read_payload(clipboard: &mut Clipboard) -> Result<ClipboardRead, String> {
    let text = match clipboard.get_text() {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(ClipboardError::ContentNotAvailable) => None,
        Err(error) => return Err(error.to_string()),
    };
    // 图片读取失败在 Wayland 等环境很常见；文字可用时继续同步文字，
    // 只有两者都取不到才算剪贴板不可用。
    let (image, complete) = match clipboard.get_image() {
        Ok(value) => (Some(value), true),
        Err(ClipboardError::ContentNotAvailable) => (None, true),
        Err(_) if text.is_some() => (None, false),
        Err(error) => return Err(error.to_string()),
    };
    Ok(ClipboardRead {
        payload: (text.is_some() || image.is_some()).then_some((text, image)),
        complete,
    })
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
fn encode_image(image: ImageData<'_>) -> Option<Vec<u8>> {
    let rgba = image::RgbaImage::from_raw(
        u32::try_from(image.width).ok()?,
        u32::try_from(image.height).ok()?,
        image.bytes.into_owned(),
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
    let Some(mut reader) = png_rows(bytes, png::Transformations::EXPAND) else {
        return false;
    };
    if reader
        .output_buffer_size()
        .is_none_or(|size| size as u64 > MAX_CLIPBOARD_IMAGE_ALLOCATION)
    {
        return false;
    }
    loop {
        match reader.next_row() {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

fn png_rows(
    bytes: &[u8],
    transformation: png::Transformations,
) -> Option<png::Reader<Cursor<&[u8]>>> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_CLIPBOARD_IMAGE_ALLOCATION as usize,
        },
    );
    let header = decoder.read_header_info().ok()?;
    if header.width > MAX_CLIPBOARD_IMAGE_DIMENSION || header.height > MAX_CLIPBOARD_IMAGE_DIMENSION
    {
        return None;
    }
    decoder.set_transformations(transformation);
    // 验证像素流时不保留展示和历史不需要的图像元数据。
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.read_info().ok()
}

pub(crate) fn clipboard_preview_png(bytes: &[u8]) -> Option<Vec<u8>> {
    const EDGE: u32 = 720;
    let mut reader = png_rows(bytes, png::Transformations::normalize_to_color8())?;
    let (source_width, source_height) = reader.info().size();
    let edge = source_width.max(source_height);
    if edge <= EDGE || reader.info().interlaced {
        return None;
    }
    let scale = |size: u32| ((size * EDGE + edge / 2) / edge).max(1);
    let (width, height) = (scale(source_width), scale(source_height));
    let (color, depth) = reader.output_color_type();
    let channels = color.samples();
    let mut pixels = Vec::with_capacity(width as usize * height as usize * channels);
    let source_coordinate = |index, target, source| (index * 2 + 1) * source / (target * 2);
    let offsets: Vec<_> = (0..width)
        .map(|x| source_coordinate(x, width, source_width) as usize * channels)
        .collect();
    let mut target_y = 0;
    for y in 0..source_height {
        let row = reader.next_row().ok()??;
        if y == source_coordinate(target_y, height, source_height) {
            for &offset in &offsets {
                pixels.extend_from_slice(row.data().get(offset..offset + channels)?);
            }
            target_y += 1;
            if target_y == height {
                break;
            }
        }
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        encoder.set_compression(png::Compression::Fast);
        encoder
            .write_header()
            .ok()?
            .write_image_data(&pixels)
            .ok()?;
    }
    Some(output)
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
        let bytes = BASE64
            .decode(encoded.as_bytes())
            .map_err(|error| error.to_string())?;
        let decoded = decode_png(&bytes)?.into_rgba8();
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
            .set_text(&*text)
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

    #[cfg(all(feature = "gui", not(target_os = "android")))]
    #[test]
    fn a_partial_clipboard_read_leaves_the_same_generation_available_for_retry() {
        use super::{read_changed_payload, ClipboardRead, ImageData};

        let mut seen = Some(12);
        let generation = || Some(13);
        let partial = read_changed_payload(&mut seen, generation, || {
            Ok(ClipboardRead {
                payload: Some((Some("text".into()), None)),
                complete: false,
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(partial.0.as_deref(), Some("text"));
        assert_eq!(seen, Some(12));

        let recovered = read_changed_payload(&mut seen, generation, || {
            Ok(ClipboardRead {
                payload: Some((
                    Some("text".into()),
                    Some(ImageData {
                        width: 1,
                        height: 1,
                        bytes: vec![0, 0, 0, 255].into(),
                    }),
                )),
                complete: true,
            })
        })
        .unwrap()
        .unwrap();
        assert!(recovered.1.is_some());
        assert_eq!(seen, Some(13));
        assert!(read_changed_payload(&mut seen, generation, || {
            panic!("已完整读取的变更无需重试")
        })
        .unwrap()
        .is_none());
    }

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

    #[test]
    fn png_validation_checks_image_data_after_the_header() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(16, 16, Rgba([3, 5, 7, 255])));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        assert!(is_valid_png(&bytes));
        let idat = bytes.windows(4).position(|chunk| chunk == b"IDAT").unwrap();
        bytes.truncate(idat + 6);
        assert!(!is_valid_png(&bytes));
    }
}
