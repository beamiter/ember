//! Bounded, inert previews of explicitly selected local regular files.
//!
//! No browser, interpreter, external opener, or subprocess is involved. A
//! Linux uses an inert handle, checks its type, then reopens that held inode
//! for reading. Other platforms use precheck plus descriptor validation with
//! weaker device-race protection. Call this on a worker, not the UI thread.
use std::io::Read;
use std::path::Path;

pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
const MAX_PIXELS: u64 = 4 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;

pub enum PreviewContent {
    Text(String),
    Image(egui::ColorImage),
}

// Linux O_PATH obtains an inert handle: it does not invoke a device's open
// operation. Verify that handle before reopening the same inode for reads.
#[cfg(target_os = "linux")]
pub(crate) fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cannot inspect local preview file".to_string())?;
    if !handle
        .metadata()
        .map_err(|_| "Cannot inspect preview file".to_string())?
        .is_file()
    {
        return Err("Preview requires a regular file".into());
    }
    // This is our own held descriptor, not a path from terminal content.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(format!("/proc/self/fd/{}", handle.as_raw_fd()))
        .map_err(|_| "Cannot read local preview file".to_string())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    // Reject known special files before opening as well as checking the final
    // descriptor. Unlike Linux O_PATH, this fallback is not race-proof against
    // replacement with a device between inspection and open.
    if !std::fs::symlink_metadata(path)
        .map_err(|_| "Cannot inspect preview file".to_string())?
        .is_file()
    {
        return Err("Preview requires a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| "Cannot open local preview file".to_string())
}

pub fn load(path: &Path) -> Result<PreviewContent, String> {
    let mut file = open_regular(path)?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect preview file".to_string())?;
    if !metadata.is_file() {
        return Err("Preview requires a regular file".into());
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err("Preview file exceeds 8 MiB".into());
    }
    // Read the signature first so text does not receive the larger image budget.
    let mut prefix = [0u8; 12];
    let mut count = 0;
    while count < prefix.len() {
        let n = file
            .read(&mut prefix[count..])
            .map_err(|_| "Cannot read preview file".to_string())?;
        if n == 0 {
            break;
        }
        count += n;
    }
    let format = raster_format(&prefix[..count]);
    let limit = if format.is_some() {
        MAX_IMAGE_BYTES
    } else {
        MAX_TEXT_BYTES
    };
    if metadata.len() > limit {
        let message = if format.is_none() {
            "Text preview exceeds the 1 MiB limit"
        } else {
            "Preview exceeds its byte budget"
        };
        return Err(message.into());
    }
    let mut bytes = prefix[..count].to_vec();
    file.take(limit + 1 - count as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read preview file".to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Preview file grew beyond its byte budget".into());
    }
    if let Some(format) = format {
        decode_raster(bytes, format).map(PreviewContent::Image)
    } else {
        let text = String::from_utf8(bytes).map_err(|_| {
            "Only UTF-8 text and PNG, JPEG or static WebP images are supported".to_string()
        })?;
        // Keep line endings and tabs, but never render terminal escapes, NUL,
        // bidi overrides, or other invisible controls from source as UI chrome.
        if text.chars().any(|c| {
            (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }) {
            return Err("Preview contains binary or unsafe control characters".into());
        }
        Ok(PreviewContent::Text(text))
    }
}

fn raster_format(prefix: &[u8]) -> Option<image::ImageFormat> {
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(image::ImageFormat::Png)
    } else if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(image::ImageFormat::Jpeg)
    } else if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        Some(image::ImageFormat::WebP)
    } else {
        None
    }
}

fn decode_raster(bytes: Vec<u8>, format: image::ImageFormat) -> Result<egui::ColorImage, String> {
    match format {
        image::ImageFormat::Png => decode_png(bytes),
        image::ImageFormat::Jpeg => {
            let decoder = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes))
                .map_err(|_| "Invalid JPEG header".to_string())?;
            decode_checked(decoder)
        }
        image::ImageFormat::WebP => {
            preflight_webp(&bytes)?;
            let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))
                .map_err(|_| "Invalid WebP header".to_string())?;
            if decoder.has_animation() {
                return Err("Animated WebP preview is not supported".into());
            }
            decode_checked(decoder)
        }
        _ => Err("Unsupported preview image format".into()),
    }
}

// Check both extended canvas and actual codec header before entering the
// decoder. A tiny VP8X canvas must not hide a huge inner VP8 allocation.
fn preflight_webp(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err("Invalid WebP container".into());
    }
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if size.checked_add(8) != Some(bytes.len()) {
        return Err("Invalid WebP container size".into());
    }
    let mut offset = 12usize;
    let mut canvas = None;
    let mut raster = None;
    while offset < bytes.len() {
        let header = bytes
            .get(offset..offset + 8)
            .ok_or("Truncated WebP chunk")?;
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(size)
            .filter(|&end| end <= bytes.len())
            .ok_or("Invalid WebP chunk size")?;
        let data = &bytes[start..end];
        let dimensions = match &header[..4] {
            b"ANIM" | b"ANMF" => return Err("Animated WebP preview is not supported".into()),
            b"VP8X" => {
                if data.len() != 10 || canvas.is_some() {
                    return Err("Invalid WebP canvas".into());
                }
                if data[0] & 2 != 0 {
                    return Err("Animated WebP preview is not supported".into());
                }
                let width = 1 + u32::from_le_bytes([data[4], data[5], data[6], 0]);
                let height = 1 + u32::from_le_bytes([data[7], data[8], data[9], 0]);
                check_dimensions(width, height)?;
                canvas = Some((width, height));
                None
            }
            b"VP8 " => {
                if data.len() < 10 || data[0] & 1 != 0 || data[3..6] != [0x9d, 0x01, 0x2a] {
                    return Err("Invalid WebP VP8 header".into());
                }
                Some((
                    u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3fff),
                    u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3fff),
                ))
            }
            b"VP8L" => {
                if data.len() < 5 || data[0] != 0x2f {
                    return Err("Invalid WebP lossless header".into());
                }
                let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
                if bits >> 29 != 0 {
                    return Err("Unsupported WebP lossless version".into());
                }
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            _ => None,
        };
        if let Some((width, height)) = dimensions {
            if raster.is_some() {
                return Err("Multiple WebP images are not supported".into());
            }
            check_dimensions(width, height)?;
            raster = Some((width, height));
        }
        offset = end
            .checked_add(size & 1)
            .filter(|&end| end <= bytes.len())
            .ok_or("Missing WebP chunk padding")?;
    }
    let raster = raster.ok_or("Missing WebP image")?;
    if canvas.is_some_and(|canvas| canvas != raster) {
        return Err("WebP canvas and image dimensions differ".into());
    }
    Ok(())
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err("Image exceeds preview dimensions or pixel budget".into());
    }
    Ok(())
}

fn decode_checked(mut decoder: impl image::ImageDecoder) -> Result<egui::ColorImage, String> {
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    // Reserve output storage before asking the decoder to allocate internals.
    limits.max_alloc = Some(
        MAX_DECODE_BYTES
            .checked_sub(decoder.total_bytes())
            .ok_or_else(|| "Image exceeds decoded byte budget".to_string())?,
    );
    decoder
        .set_limits(limits)
        .map_err(|_| "Image exceeds decoder limits".to_string())?;
    let image = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| "Cannot decode image preview".to_string())?;
    if image.width() != width || image.height() != height {
        return Err("Image dimensions changed during decode".into());
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        image.to_rgba8().as_raw(),
    ))
}

fn decode_png(bytes: Vec<u8>) -> Result<egui::ColorImage, String> {
    // PNG has a fixed first IHDR chunk. Check pixel count before the decoder
    // allocates, in addition to the decoder's allocation/dimension limits.
    if bytes.len() < 33 || &bytes[8..16] != b"\0\0\0\rIHDR" {
        return Err("Invalid PNG header".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err("PNG exceeds preview dimensions or pixel budget".into());
    }
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "Cannot decode PNG preview".to_string())?;
    if image.width() != width || image.height() != height {
        return Err("PNG dimensions changed during decode".into());
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        image.to_rgba8().as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ember-preview-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn source_is_inert_text() {
        for source in [
            "<script>alert(1)</script>",
            "export const C = () => <div/>;",
            "# Markdown\n![image](https://example.invalid/tracker)",
            "<svg onload='alert(1)'/>",
        ] {
            let f = Fixture::new(source.as_bytes());
            assert!(matches!(load(&f.0), Ok(PreviewContent::Text(t)) if t == source));
        }
    }
    #[test]
    fn rejects_binary_and_control_characters() {
        for bytes in [
            b"abc\0".as_slice(),
            b"\x1b[31mred",
            b"\xff\xfe",
            "bad\u{202e}path".as_bytes(),
        ] {
            let f = Fixture::new(bytes);
            assert!(load(&f.0).is_err());
        }
    }
    #[test]
    fn rejects_oversized_text_before_reading() {
        let f = Fixture::new(b"");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&f.0)
            .unwrap()
            .set_len(MAX_TEXT_BYTES + 1)
            .unwrap();
        assert_eq!(
            load(&f.0).err().as_deref(),
            Some("Text preview exceeds the 1 MiB limit")
        );
    }
    #[test]
    fn png_decode_and_pixel_limit() {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let f = Fixture::new(&png);
        assert!(matches!(load(&f.0), Ok(PreviewContent::Image(image)) if image.size == [1, 1]));
        png[16..20].copy_from_slice(&4096u32.to_be_bytes());
        png[20..24].copy_from_slice(&4096u32.to_be_bytes());
        assert!(decode_png(png).unwrap_err().contains("pixel budget"));
    }

    #[test]
    fn jpeg_and_webp_use_content_not_filename() {
        use image::ImageEncoder;
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[10, 20, 30, 128], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        for bytes in [&jpeg, &webp] {
            let fixture = Fixture::new(bytes);
            assert!(
                matches!(load(&fixture.0), Ok(PreviewContent::Image(image)) if image.size == [1, 1])
            );
        }
        let image = decode_raster(webp, image::ImageFormat::WebP).unwrap();
        assert_eq!(image.pixels[0].to_srgba_unmultiplied(), [10, 20, 30, 128]);
    }

    #[test]
    fn raster_headers_are_checked_before_pixel_decode() {
        use image::ImageEncoder;
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .write_image(&[0, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        let sof = jpeg
            .windows(2)
            .position(|bytes| bytes == [0xff, 0xc0])
            .unwrap();
        jpeg[sof + 5..sof + 7].copy_from_slice(&4096u16.to_be_bytes());
        jpeg[sof + 7..sof + 9].copy_from_slice(&4096u16.to_be_bytes());
        assert!(decode_raster(jpeg, image::ImageFormat::Jpeg)
            .unwrap_err()
            .contains("pixel budget"));

        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        let vp8l = webp.windows(4).position(|bytes| bytes == b"VP8L").unwrap();
        let dims = 4095u32 | (4095u32 << 14);
        webp[vp8l + 9..vp8l + 13].copy_from_slice(&dims.to_le_bytes());
        assert!(decode_raster(webp, image::ImageFormat::WebP)
            .unwrap_err()
            .contains("pixel budget"));
    }

    #[test]
    fn corrupt_and_oversized_rasters_fail_closed() {
        for signature in [b"\xff\xd8\xff".as_slice(), b"RIFF\0\0\0\0WEBP"] {
            let fixture = Fixture::new(signature);
            assert!(load(&fixture.0).is_err());
            std::fs::OpenOptions::new()
                .write(true)
                .open(&fixture.0)
                .unwrap()
                .set_len(MAX_IMAGE_BYTES + 1)
                .unwrap();
            assert!(matches!(load(&fixture.0), Err(error) if error.contains("8 MiB")));
        }
        assert_eq!(raster_format(b"RIFF1234WAVE"), None);
        assert_eq!(raster_format(b"<svg/>"), None);
        for (width, height) in [(0, 1), (1, 0), (4097, 1), (1, 4097), (4096, 4096)] {
            assert!(check_dimensions(width, height).is_err());
        }
    }

    #[test]
    fn webp_rejects_animation_and_hidden_inner_size_before_decode() {
        fn container(payload: Vec<u8>) -> Vec<u8> {
            let mut result = b"RIFF".to_vec();
            result.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
            result.extend_from_slice(b"WEBP");
            result.extend(payload);
            result
        }
        let mut chunks = b"VP8X\x0a\0\0\0".to_vec();
        chunks.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(preflight_webp(&container(chunks.clone()))
            .unwrap_err()
            .contains("Animated"));
        chunks[8] = 0;
        chunks.extend_from_slice(b"VP8 \x0a\0\0\0");
        chunks.extend_from_slice(&[0, 0, 0, 0x9d, 1, 0x2a, 0, 0x10, 0, 0x10]);
        assert!(preflight_webp(&container(chunks))
            .unwrap_err()
            .contains("pixel budget"));
    }

    #[test]
    fn lossy_webp_and_extended_canvas_decode() {
        // Original 2x1 solid-color fixture encoded with Pillow/libwebp.
        let bytes = vec![
            82, 73, 70, 70, 54, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 42, 0, 0, 0, 144, 1, 0,
            157, 1, 42, 2, 0, 1, 0, 1, 64, 38, 37, 160, 2, 116, 186, 0, 3, 152, 0, 254, 246, 230,
            7, 208, 163, 101, 18, 169, 127, 242, 207, 252, 254, 248, 187, 209, 128, 0, 0,
        ];
        assert_eq!(
            decode_raster(bytes.clone(), image::ImageFormat::WebP)
                .unwrap()
                .size,
            [2, 1]
        );
        let mut extended = b"RIFF".to_vec();
        extended.extend_from_slice(&((bytes.len() - 8 + 18) as u32).to_le_bytes());
        extended.extend_from_slice(b"WEBPVP8X\x0a\0\0\0");
        extended.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
        extended.extend_from_slice(&bytes[12..]);
        assert_eq!(
            decode_raster(extended.clone(), image::ImageFormat::WebP)
                .unwrap()
                .size,
            [2, 1]
        );
        extended[24] = 0; // Outer canvas 1x1 but inner image 2x1.
        assert!(preflight_webp(&extended)
            .unwrap_err()
            .contains("dimensions differ"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn held_regular_descriptor_is_not_retargeted_by_path_replacement() {
        let fixture = Fixture::new(b"original");
        let mut file = open_regular(&fixture.0).unwrap();
        std::fs::remove_file(&fixture.0).unwrap();
        std::os::unix::fs::symlink("/dev/null", &fixture.0).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "original");
        assert!(
            matches!(open_regular(Path::new("/dev/null")), Err(error) if error.contains("regular file"))
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_fifo_and_directory() {
        use std::os::unix::ffi::OsStrExt;
        let f = Fixture::new(b"safe");
        let link = Fixture::new(b"");
        std::fs::remove_file(&link.0).unwrap();
        std::os::unix::fs::symlink(&f.0, &link.0).unwrap();
        assert!(load(&link.0).is_err());
        let fifo = Fixture::new(b"");
        std::fs::remove_file(&fifo.0).unwrap();
        let name = std::ffi::CString::new(fifo.0.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(load(&fifo.0).is_err());
        assert!(load(&std::env::temp_dir()).is_err());
        #[cfg(target_os = "linux")]
        assert!(load(Path::new("/dev/null")).is_err());
    }
}
