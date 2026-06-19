use base64::Engine;
use std::collections::HashMap;

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;

/// 图像格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Rgb,
    Rgba,
}

impl ImageFormat {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "png" => Some(ImageFormat::Png),
            "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
            "webp" => Some(ImageFormat::Webp),
            "rgb" => Some(ImageFormat::Rgb),
            "rgba" => Some(ImageFormat::Rgba),
            _ => None,
        }
    }
}

/// Kitty 图像
#[derive(Debug, Clone)]
pub struct KittyImage {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // 原始或解码后的图像数据
}

/// Kitty 图像放置
#[derive(Debug, Clone)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub z_index: i32,
}

/// Kitty 图像协议参数
#[derive(Debug, Default)]
pub struct KittyGraphicsParams {
    pub action: Option<String>,    // a: t=transfer, d=delete, p=place, q=query
    pub image_id: Option<u32>,     // i
    pub image_number: Option<u32>, // I
    pub placement_id: Option<u32>, // p
    pub format: Option<String>,    // f: png, jpeg, rgb, rgba
    pub width: Option<u32>,        // s
    pub height: Option<u32>,       // v
    pub x: Option<u32>,            // x: column
    pub y: Option<u32>,            // y: row
    pub z: Option<i32>,            // z: z-order
    pub more: bool,                // m: 1=more data, 0=last
    pub data: Option<String>,      // base64 encoded data
}

/// 待传输的图像数据
pub struct PendingTransfer {
    pub chunks: Vec<Vec<u8>>,
    pub bytes: usize,
}

/// Hard cap on bytes buffered while waiting for a Kitty graphics
/// chunked-transfer terminator (`m=0`). Without this a peer that keeps
/// sending `m=1` chunks but never closes the transfer would grow
/// `pending_transfer` without bound; the per-image `MAX_KITTY_CACHE_MB`
/// only kicks in after the transfer completes.
const MAX_PENDING_TRANSFER_BYTES: usize = (MAX_KITTY_CACHE_MB as usize) * 1024 * 1024;

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    pending_transfer: Option<PendingTransfer>,
    next_placement_id: u32,
    total_decoded: u32,
    total_bytes_processed: u64,
    total_image_memory: u64,
    access_order: std::collections::VecDeque<u32>,
    /// Protocol responses (e.g. query replies) awaiting transmission to the PTY.
    pending_responses: Vec<u8>,
}

impl KittyGraphicsState {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            pending_transfer: None,
            next_placement_id: 1,
            total_decoded: 0,
            total_bytes_processed: 0,
            total_image_memory: 0,
            access_order: std::collections::VecDeque::new(),
            pending_responses: Vec::new(),
        }
    }

    /// Drain any protocol responses (query replies) for transmission to the PTY.
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_responses)
    }

    fn enforce_image_limits(&mut self) {
        while self.images.len() > MAX_KITTY_IMAGES
            || self.total_image_memory > MAX_KITTY_CACHE_MB * 1024 * 1024 {
            if let Some(oldest_id) = self.access_order.pop_front() {
                if let Some(img) = self.images.remove(&oldest_id) {
                    self.total_image_memory -= img.data.len() as u64;
                    self.placements.retain(|p| p.image_id != oldest_id);
                }
            } else {
                break;
            }
        }
    }

    /// 解析 Kitty 图像协议的 DCS 数据
    pub fn parse_graphics_payload(&mut self, payload: &str) -> Result<(), String> {
        let params = Self::parse_params(payload)?;

        match params.action.as_deref() {
            Some("t") => self.handle_transfer(params),
            Some("p") => self.handle_placement(params),
            Some("d") => self.handle_delete(params),
            Some("q") => self.handle_query(params),
            _ => Err("Unknown action".to_string()),
        }
    }

    /// 解析参数字符串
    fn parse_params(payload: &str) -> Result<KittyGraphicsParams, String> {
        let mut params = KittyGraphicsParams::default();

        // 将 payload 按 ';' 分割
        for pair in payload.split(';') {
            if pair.is_empty() {
                continue;
            }

            // 分割 key=value
            let (key, value) = if let Some(pos) = pair.find('=') {
                (&pair[..pos], &pair[pos + 1..])
            } else {
                (pair, "")
            };

            match key {
                "a" => params.action = Some(value.to_string()),
                "i" => params.image_id = value.parse().ok(),
                "I" => params.image_number = value.parse().ok(),
                "p" => params.placement_id = value.parse().ok(),
                "f" => params.format = Some(value.to_string()),
                "s" => params.width = value.parse().ok(),
                "v" => params.height = value.parse().ok(),
                "x" => params.x = value.parse().ok(),
                "y" => params.y = value.parse().ok(),
                "z" => params.z = value.parse().ok(),
                "m" => params.more = value == "1",
                _ => {
                    // 最后一个没有 key= 的参数是 base64 数据
                    if !value.is_empty() {
                        params.data = Some(value.to_string());
                    } else if !key.contains('=') && !key.is_empty() {
                        params.data = Some(key.to_string());
                    }
                }
            }
        }

        Ok(params)
    }

    /// 处理传输操作 (a=t)
    fn handle_transfer(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        let image_id = params.image_id.ok_or("Missing image ID")?;
        let format_str = params.format.as_deref().unwrap_or("png");
        let format =
            ImageFormat::from_str(format_str).ok_or(format!("Unknown format: {}", format_str))?;

        // 解码 base64 数据
        let data = if let Some(encoded) = params.data {
            let engine = base64::engine::general_purpose::STANDARD;
            engine
                .decode(&encoded)
                .map_err(|e| format!("Base64 decode error: {}", e))?
        } else {
            return Err("No image data provided".to_string());
        };

        if params.more {
            // 分块传输，需要缓存。对累积大小做硬上限,防止恶意/异常
            // 流持续发 m=1 但永不发 m=0 时无界堆积。
            let pending = self.pending_transfer.get_or_insert(PendingTransfer {
                chunks: Vec::new(),
                bytes: 0,
            });
            let new_bytes = pending.bytes.saturating_add(data.len());
            if new_bytes > MAX_PENDING_TRANSFER_BYTES {
                self.pending_transfer = None;
                return Err(format!(
                    "Pending Kitty transfer exceeded {} MiB; dropping",
                    MAX_KITTY_CACHE_MB
                ));
            }
            pending.bytes = new_bytes;
            pending.chunks.push(data);
        } else {
            // 最后一块或单块传输
            let pending = self.pending_transfer.take();

            // 合并所有块
            let mut final_data = if let Some(pending) = pending {
                let mut combined = Vec::with_capacity(pending.bytes + data.len());
                for chunk in pending.chunks {
                    combined.extend_from_slice(&chunk);
                }
                combined.extend_from_slice(&data);
                combined
            } else {
                data
            };

            // 获取尺寸并把数据统一归一化为 RGBA(每像素 4 字节),
            // 并严格校验长度，避免后续 egui::ColorImage::from_rgba_unmultiplied
            // 的内部 assert 因尺寸/数据不匹配而 panic 整个应用。
            let (width, height) = match format {
                // Webp 同样是压缩格式，交给 image 解码（原先按原始格式处理会渲染错乱）
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp => {
                    let (decoded_data, w, h) = self.decode_compressed_image(final_data, format)?;
                    final_data = decoded_data;
                    (w, h)
                }
                ImageFormat::Rgb => {
                    let w = params.width.ok_or("Missing width for raw image format")?;
                    let h = params.height.ok_or("Missing height for raw image format")?;
                    let px = (w as usize)
                        .checked_mul(h as usize)
                        .ok_or("Image dimensions overflow")?;
                    let expected = px.checked_mul(3).ok_or("Image dimensions overflow")?;
                    if final_data.len() < expected {
                        return Err(format!(
                            "RGB data too short: got {} bytes, need {} for {}x{}",
                            final_data.len(),
                            expected,
                            w,
                            h
                        ));
                    }
                    // 展开 RGB -> RGBA(alpha=255)
                    let mut rgba = Vec::with_capacity(px * 4);
                    for chunk in final_data[..expected].chunks_exact(3) {
                        rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
                    }
                    final_data = rgba;
                    (w, h)
                }
                ImageFormat::Rgba => {
                    let w = params.width.ok_or("Missing width for raw image format")?;
                    let h = params.height.ok_or("Missing height for raw image format")?;
                    let px = (w as usize)
                        .checked_mul(h as usize)
                        .ok_or("Image dimensions overflow")?;
                    let expected = px.checked_mul(4).ok_or("Image dimensions overflow")?;
                    if final_data.len() < expected {
                        return Err(format!(
                            "RGBA data too short: got {} bytes, need {} for {}x{}",
                            final_data.len(),
                            expected,
                            w,
                            h
                        ));
                    }
                    final_data.truncate(expected);
                    (w, h)
                }
            };

            let data_size = final_data.len() as u64;
            self.total_decoded += 1;
            self.total_bytes_processed += data_size;

            // 若同一 image_id 已存在，先移除旧条目，避免内存计数泄漏与 LRU 顺序错乱
            if let Some(old) = self.images.remove(&image_id) {
                self.total_image_memory = self
                    .total_image_memory
                    .saturating_sub(old.data.len() as u64);
                self.access_order.retain(|&id| id != image_id);
            }

            self.total_image_memory += data_size;
            self.access_order.push_back(image_id);

            self.images.insert(
                image_id,
                KittyImage {
                    id: image_id,
                    width,
                    height,
                    data: final_data,
                },
            );

            self.enforce_image_limits();

            log::info!("[KITTY_GRAPHICS] Stored image {} ({}x{}) format: {:?} | Stats: {} images, {}MB total",
                image_id, width, height, format, self.images.len(), self.total_bytes_processed / 1_000_000);
        }

        Ok(())
    }

    /// 解码压缩图像格式（PNG/JPEG），返回 (RGBA数据, 宽度, 高度)
    fn decode_compressed_image(
        &self,
        data: Vec<u8>,
        format: ImageFormat,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let img =
            image::load_from_memory(&data).map_err(|e| format!("Failed to load image: {}", e))?;

        let width = img.width();
        let height = img.height();
        let rgba_image = img.to_rgba8();

        log::debug!(
            "[KITTY_GRAPHICS] Decoded {:?} image {}x{} -> RGBA {}B",
            format,
            width,
            height,
            rgba_image.len()
        );

        Ok((rgba_image.into_raw(), width, height))
    }

    /// 处理放置操作 (a=p)
    fn handle_placement(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        let image_id = params.image_id.ok_or("Missing image ID")?;
        let x = params.x.unwrap_or(0);
        let y = params.y.unwrap_or(0);
        let width = params.width.unwrap_or(1);
        let height = params.height.unwrap_or(1);
        let z = params.z.unwrap_or(0);

        let placement_id = params.placement_id.or_else(|| {
            let id = self.next_placement_id;
            self.next_placement_id += 1;
            Some(id)
        });

        self.placements.push(KittyPlacement {
            image_id,
            placement_id,
            x,
            y,
            width,
            height,
            z_index: z,
        });

        // 按 z-order 排序
        self.placements.sort_by_key(|p| p.z_index);

        log::info!(
            "[KITTY_GRAPHICS] Placed image {} at ({},{}) size {}x{} z={}",
            image_id,
            x,
            y,
            width,
            height,
            z
        );

        Ok(())
    }

    /// 处理删除操作 (a=d)
    fn handle_delete(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        if let Some(image_id) = params.image_id {
            if let Some(img) = self.images.remove(&image_id) {
                self.total_image_memory -= img.data.len() as u64;
            }
            self.placements.retain(|p| p.image_id != image_id);
            self.access_order.retain(|&id| id != image_id);
            log::info!("[KITTY_GRAPHICS] Deleted image {}", image_id);
        } else if let Some(placement_id) = params.placement_id {
            self.placements
                .retain(|p| p.placement_id != Some(placement_id));
            log::info!("[KITTY_GRAPHICS] Deleted placement {}", placement_id);
        } else {
            return Err("Missing image_id or placement_id for delete".to_string());
        }

        Ok(())
    }

    /// 处理查询操作 (a=q)
    ///
    /// Apps probe protocol support by sending a query with an image id/number;
    /// the terminal must answer with an APC `OK` response (and must NOT store
    /// the image). The response echoes back whichever identifier the app used.
    fn handle_query(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        let id_field = if let Some(id) = params.image_id {
            format!("i={id}")
        } else if let Some(num) = params.image_number {
            format!("I={num}")
        } else {
            // No identifier to correlate the reply with; reply with i=0 so the
            // app still learns the protocol is supported.
            "i=0".to_string()
        };
        let response = format!("\x1b_G{id_field};OK\x1b\\");
        self.pending_responses.extend_from_slice(response.as_bytes());
        log::info!("[KITTY_GRAPHICS] Query response: {id_field};OK");
        Ok(())
    }

    /// 获取所有放置
    pub fn get_placements(&self) -> &[KittyPlacement] {
        &self.placements
    }

    /// 获取图像
    pub fn get_image(&self, id: u32) -> Option<&KittyImage> {
        self.images.get(&id)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn image_memory_mb(&self) -> u64 {
        self.total_image_memory / 1_000_000
    }
}

impl Default for KittyGraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_graphics_params() {
        let payload = "a=t;i=1;s=100;v=100;f=png";
        let params = KittyGraphicsState::parse_params(payload).unwrap();
        assert_eq!(params.action.as_deref(), Some("t"));
        assert_eq!(params.image_id, Some(1));
        assert_eq!(params.width, Some(100));
        assert_eq!(params.height, Some(100));
        assert_eq!(params.format.as_deref(), Some("png"));
    }

    #[test]
    fn test_placement_ordering() {
        let mut state = KittyGraphicsState::new();
        state.placements.push(KittyPlacement {
            image_id: 1,
            placement_id: None,
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            z_index: 5,
        });
        state.placements.push(KittyPlacement {
            image_id: 2,
            placement_id: None,
            x: 10,
            y: 10,
            width: 10,
            height: 10,
            z_index: -1,
        });

        // Sort by z_index
        state.placements.sort_by_key(|p| p.z_index);

        assert_eq!(state.placements[0].z_index, -1);
        assert_eq!(state.placements[1].z_index, 5);
    }

    #[test]
    fn test_complete_kitty_workflow() {
        let mut state = KittyGraphicsState::new();

        // Create a simple 2x2 RGBA image (red square)
        // 4 pixels * 4 bytes (RGBA) = 16 bytes
        let mut image_data = Vec::new();
        for _ in 0..4 {
            image_data.extend_from_slice(&[255, 0, 0, 255]); // Red pixel RGBA
        }

        // Encode to base64
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

        println!("Base64 data: {}", base64_data);

        // Simulate receiving image transfer
        // Test 1: Simple parameter test (no data in this call, just verify params)
        let param_test = "a=t;i=1;s=2;v=2;f=rgba;m=0";
        match KittyGraphicsState::parse_params(param_test) {
            Ok(params) => {
                println!("Parsed params - action: {:?}, id: {:?}, w: {:?}, h: {:?}, fmt: {:?}, more: {}, data: {:?}",
                    params.action, params.image_id, params.width, params.height, params.format, params.more, params.data);
            }
            Err(e) => {
                println!("Parse params error: {}", e);
            }
        }

        // Now test with data
        let payload = format!("a=t;i=1;s=2;v=2;f=rgba;m=0;{}", base64_data);
        println!("Full payload: {}", payload);

        // Try parsing the full payload
        match state.parse_graphics_payload(&payload) {
            Ok(_) => {
                println!("Successfully parsed and processed image");
            }
            Err(e) => {
                println!("Full parse error: {}", e);
            }
        }
    }
}
