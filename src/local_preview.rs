//! Bounded, inert previews of explicitly selected local regular files.
//!
//! No browser, interpreter, external opener, or subprocess is involved. A
//! descriptor is opened once and checked before reading, so replacing the path
//! cannot redirect the subsequent read. Call this on a worker, not the UI thread.
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

pub fn load(path: &Path) -> Result<PreviewContent, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Nonblocking protects against FIFO/device substitution before the
        // descriptor check. Refuse a final symlink rather than follow it.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "Cannot open local preview file".to_string())?;
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
    let mut prefix = [0u8; 8];
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
    let png = count == 8 && prefix == *b"\x89PNG\r\n\x1a\n";
    let limit = if png { MAX_IMAGE_BYTES } else { MAX_TEXT_BYTES };
    if metadata.len() > limit {
        return Err("Preview exceeds its byte budget".into());
    }
    let mut bytes = prefix[..count].to_vec();
    file.take(limit + 1 - count as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read preview file".to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Preview file grew beyond its byte budget".into());
    }
    if png {
        decode_png(bytes).map(PreviewContent::Image)
    } else {
        let text = String::from_utf8(bytes)
            .map_err(|_| "Only UTF-8 text and PNG images are supported".to_string())?;
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
        assert!(load(&f.0).is_err());
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
    }
}
