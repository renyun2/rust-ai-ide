use std::ffi::c_void;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static CLIP_IMG_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct DecodedPreviewImage {
    pub orig_width: u32,
    pub orig_height: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum ClipboardPastePayload {
    Files(Vec<PathBuf>),
    ImageFile(PathBuf),
    Text(String),
}

pub fn is_image_file_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "ico" | "tif" | "tiff"
    )
}

pub fn load_image_for_preview(path: &Path) -> Option<DecodedPreviewImage> {
    let bytes = fs::read(path).ok()?;
    let decoded = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        decode_png_bytes(&bytes)
    } else if bytes.starts_with(b"BM") {
        decode_bmp_bytes(&bytes)
    } else {
        None
    }
    .or_else(|| decode_with_windows_gdiplus(path));

    let (orig_w, orig_h, rgba) = decoded?;
    if orig_w == 0 || orig_h == 0 || rgba.len() < (orig_w as usize * orig_h as usize * 4) {
        return None;
    }

    let (w, h, final_rgba) = downsample_if_huge(orig_w, orig_h, rgba, 1440);
    Some(DecodedPreviewImage {
        orig_width: orig_w,
        orig_height: orig_h,
        width: w,
        height: h,
        rgba: final_rgba,
    })
}

fn decode_png_bytes(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let w = info.width;
    let h = info.height;
    let total = (w as usize).saturating_mul(h as usize);
    let mut rgba = Vec::with_capacity(total * 4);
    match info.color_type {
        png::ColorType::Rgba => {
            rgba.extend_from_slice(&data[..total * 4]);
        }
        png::ColorType::Rgb => {
            for px in data.chunks_exact(3).take(total) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in data.chunks_exact(2).take(total) {
                rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in data.iter().take(total) {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::Indexed => return None,
    }
    Some((w, h, rgba))
}

fn decode_bmp_bytes(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return None;
    }
    let pixel_offset = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
    if pixel_offset >= bytes.len() {
        return None;
    }
    decode_dib_slice(&bytes[14..], Some(&bytes[pixel_offset..]))
}

fn decode_dib_slice(header_and_pixels: &[u8], explicit_pixels: Option<&[u8]>) -> Option<(u32, u32, Vec<u8>)> {
    if header_and_pixels.len() < 40 {
        return None;
    }
    let hdr_size = u32::from_le_bytes(header_and_pixels[0..4].try_into().ok()?) as usize;
    if hdr_size < 40 || hdr_size > header_and_pixels.len() {
        return None;
    }
    let width_i = i32::from_le_bytes(header_and_pixels[4..8].try_into().ok()?);
    let height_i = i32::from_le_bytes(header_and_pixels[8..12].try_into().ok()?);
    let bpp = u16::from_le_bytes(header_and_pixels[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(header_and_pixels[16..20].try_into().ok()?);
    if width_i <= 0 || height_i == 0 || (bpp != 24 && bpp != 32) || (compression != 0 && compression != 3) {
        return None;
    }
    let w = width_i as usize;
    let h = height_i.unsigned_abs() as usize;
    let top_down = height_i < 0;
    let extra_masks = if compression == 3 && hdr_size == 40 { 12 } else { 0 };
    let pixels = explicit_pixels.unwrap_or(&header_and_pixels[(hdr_size + extra_masks).min(header_and_pixels.len())..]);
    let bytes_per_px = (bpp / 8) as usize;
    let stride = ((w * bytes_per_px + 3) / 4) * 4;
    if pixels.len() < stride.saturating_mul(h) {
        return None;
    }

    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src_row = if top_down { y } else { h - 1 - y };
        let row_bytes = &pixels[src_row * stride..src_row * stride + w * bytes_per_px];
        for x in 0..w {
            let s = x * bytes_per_px;
            let d = (y * w + x) * 4;
            let b = row_bytes[s];
            let g = row_bytes[s + 1];
            let r = row_bytes[s + 2];
            let a = if bytes_per_px == 4 {
                let raw_a = row_bytes[s + 3];
                if raw_a == 0 { 255 } else { raw_a }
            } else {
                255
            };
            rgba[d] = r;
            rgba[d + 1] = g;
            rgba[d + 2] = b;
            rgba[d + 3] = a;
        }
    }
    Some((w as u32, h as u32, rgba))
}

fn downsample_if_huge(w: u32, h: u32, rgba: Vec<u8>, max_side: u32) -> (u32, u32, Vec<u8>) {
    let cur_max = w.max(h);
    if cur_max <= max_side {
        return (w, h, rgba);
    }
    let scale = max_side as f32 / cur_max as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for dy in 0..nh {
        let sy = ((dy as f64 * h as f64) / nh as f64).floor() as u32;
        let sy = sy.min(h - 1);
        for dx in 0..nw {
            let sx = ((dx as f64 * w as f64) / nw as f64).floor() as u32;
            let sx = sx.min(w - 1);
            let si = ((sy * w + sx) * 4) as usize;
            let di = ((dy * nw + dx) * 4) as usize;
            out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
        }
    }
    (nw, nh, out)
}

#[cfg(target_os = "windows")]
fn decode_with_windows_gdiplus(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct GdiplusStartupInput {
        gdiplus_version: u32,
        debug_event_callback: *const c_void,
        suppress_background_thread: i32,
        suppress_external_codecs: i32,
    }

    #[repr(C)]
    struct GpRect {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    }

    #[repr(C)]
    struct BitmapData {
        width: u32,
        height: u32,
        stride: i32,
        pixel_format: i32,
        scan0: *mut u8,
        reserved: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> usize;
    }

    struct GdipFns {
        create_from_file: unsafe extern "system" fn(*const u16, *mut *mut c_void) -> i32,
        get_width: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
        get_height: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
        lock_bits: unsafe extern "system" fn(*mut c_void, *const GpRect, u32, i32, *mut BitmapData) -> i32,
        unlock_bits: unsafe extern "system" fn(*mut c_void, *mut BitmapData) -> i32,
        dispose_image: unsafe extern "system" fn(*mut c_void) -> i32,
    }

    static GDIP_INIT: OnceLock<Option<GdipFns>> = OnceLock::new();
    let fns = GDIP_INIT
        .get_or_init(|| unsafe {
            let hmod = LoadLibraryA(b"gdiplus.dll\0".as_ptr());
            if hmod == 0 {
                return None;
            }
            let startup_p = GetProcAddress(hmod, b"GdiplusStartup\0".as_ptr());
            let create_p = GetProcAddress(hmod, b"GdipCreateBitmapFromFile\0".as_ptr());
            let w_p = GetProcAddress(hmod, b"GdipGetImageWidth\0".as_ptr());
            let h_p = GetProcAddress(hmod, b"GdipGetImageHeight\0".as_ptr());
            let lock_p = GetProcAddress(hmod, b"GdipBitmapLockBits\0".as_ptr());
            let unlock_p = GetProcAddress(hmod, b"GdipBitmapUnlockBits\0".as_ptr());
            let disp_p = GetProcAddress(hmod, b"GdipDisposeImage\0".as_ptr());
            if startup_p == 0
                || create_p == 0
                || w_p == 0
                || h_p == 0
                || lock_p == 0
                || unlock_p == 0
                || disp_p == 0
            {
                return None;
            }
            let startup: unsafe extern "system" fn(*mut usize, *const GdiplusStartupInput, *mut c_void) -> i32 =
                std::mem::transmute(startup_p);
            let mut token: usize = 0;
            let input = GdiplusStartupInput {
                gdiplus_version: 1,
                debug_event_callback: std::ptr::null(),
                suppress_background_thread: 0,
                suppress_external_codecs: 0,
            };
            if startup(&mut token, &input, std::ptr::null_mut()) != 0 {
                return None;
            }
            Some(GdipFns {
                create_from_file: std::mem::transmute(create_p),
                get_width: std::mem::transmute(w_p),
                get_height: std::mem::transmute(h_p),
                lock_bits: std::mem::transmute(lock_p),
                unlock_bits: std::mem::transmute(unlock_p),
                dispose_image: std::mem::transmute(disp_p),
            })
        })
        .as_ref()?;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    unsafe {
        let mut gp_img: *mut c_void = std::ptr::null_mut();
        if (fns.create_from_file)(wide.as_ptr(), &mut gp_img) != 0 || gp_img.is_null() {
            return None;
        }
        let mut w: u32 = 0;
        let mut h: u32 = 0;
        let ok_dim = (fns.get_width)(gp_img, &mut w) == 0 && (fns.get_height)(gp_img, &mut h) == 0;
        if !ok_dim || w == 0 || h == 0 || w > 12000 || h > 12000 {
            let _ = (fns.dispose_image)(gp_img);
            return None;
        }
        let rect = GpRect {
            x: 0,
            y: 0,
            width: w as i32,
            height: h as i32,
        };
        let mut bdata = BitmapData {
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: 0,
            scan0: std::ptr::null_mut(),
            reserved: 0,
        };
        // PixelFormat32bppARGB = 0x0026200A, ImageLockModeRead = 1
        if (fns.lock_bits)(gp_img, &rect, 1, 0x0026_200A, &mut bdata) != 0 || bdata.scan0.is_null() {
            let _ = (fns.dispose_image)(gp_img);
            return None;
        }

        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..(h as isize) {
            let row_ptr = bdata.scan0.offset(y * (bdata.stride as isize));
            let row_dst = (y as usize) * (w as usize) * 4;
            for x in 0..(w as usize) {
                let px = std::slice::from_raw_parts(row_ptr.add(x * 4), 4);
                let di = row_dst + x * 4;
                // GDI+ ARGB on little-endian is BGRA
                rgba[di] = px[2];
                rgba[di + 1] = px[1];
                rgba[di + 2] = px[0];
                rgba[di + 3] = px[3];
            }
        }
        let _ = (fns.unlock_bits)(gp_img, &mut bdata);
        let _ = (fns.dispose_image)(gp_img);
        Some((w, h, rgba))
    }
}

#[cfg(not(target_os = "windows"))]
fn decode_with_windows_gdiplus(_path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    None
}

pub(crate) fn encode_rgba_to_temp_png(w: u32, h: u32, rgba: &[u8]) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("rust-ai-ide-attachments");
    fs::create_dir_all(&dir).ok()?;
    let seq = CLIP_IMG_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out_path = dir.join(format!("clipboard_{ts}_{seq}.png"));
    let file = File::create(&out_path).ok()?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    Some(out_path)
}

#[cfg(target_os = "windows")]
pub fn read_windows_clipboard_payload() -> Option<ClipboardPastePayload> {
    use windows::core::w;
    use windows::Win32::Foundation::{HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

    unsafe {
        let png_fmt = RegisterClipboardFormatW(w!("PNG"));
        if OpenClipboard(Some(HWND::default())).is_err() {
            return None;
        }
        let result = (|| -> Option<ClipboardPastePayload> {
            // 1. 优先识别从资源管理器复制的文件/图片列表 (CF_HDROP)
            if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
                if let Ok(handle) = GetClipboardData(CF_HDROP) {
                    let hdrop = HDROP(handle.0);
                    let count = DragQueryFileW(hdrop, u32::MAX, None);
                    let mut paths = Vec::new();
                    for idx in 0..count {
                        let need = DragQueryFileW(hdrop, idx, None);
                        if need > 0 {
                            let mut buf = vec![0u16; need as usize + 1];
                            let copied = DragQueryFileW(hdrop, idx, Some(&mut buf));
                            if copied > 0 {
                                let s = String::from_utf16_lossy(&buf[..copied as usize]);
                                paths.push(PathBuf::from(s));
                            }
                        }
                    }
                    if !paths.is_empty() {
                        return Some(ClipboardPastePayload::Files(paths));
                    }
                }
            }

            // 2. 识别 PNG 剪贴板数据
            if png_fmt != 0 && IsClipboardFormatAvailable(png_fmt).is_ok() {
                if let Ok(handle) = GetClipboardData(png_fmt) {
                    let hglobal = HGLOBAL(handle.0);
                    let sz = GlobalSize(hglobal);
                    let ptr = GlobalLock(hglobal) as *const u8;
                    if !ptr.is_null() && sz > 16 {
                        let bytes = std::slice::from_raw_parts(ptr, sz).to_vec();
                        let _ = GlobalUnlock(hglobal);
                        if let Some((w, h, rgba)) = decode_png_bytes(&bytes) {
                            if let Some(saved) = encode_rgba_to_temp_png(w, h, &rgba) {
                                return Some(ClipboardPastePayload::ImageFile(saved));
                            }
                        }
                    } else if !ptr.is_null() {
                        let _ = GlobalUnlock(hglobal);
                    }
                }
            }

            // 3. 识别截屏/浏览器复制图像 (CF_DIB)
            if IsClipboardFormatAvailable(CF_DIB).is_ok() {
                if let Ok(handle) = GetClipboardData(CF_DIB) {
                    let hglobal = HGLOBAL(handle.0);
                    let sz = GlobalSize(hglobal);
                    let ptr = GlobalLock(hglobal) as *const u8;
                    if !ptr.is_null() && sz > 40 {
                        let bytes = std::slice::from_raw_parts(ptr, sz).to_vec();
                        let _ = GlobalUnlock(hglobal);
                        if let Some((w, h, rgba)) = decode_dib_slice(&bytes, None) {
                            if let Some(saved) = encode_rgba_to_temp_png(w, h, &rgba) {
                                return Some(ClipboardPastePayload::ImageFile(saved));
                            }
                        }
                    } else if !ptr.is_null() {
                        let _ = GlobalUnlock(hglobal);
                    }
                }
            }

            // 4. 文本内容
            if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
                let hglobal = HGLOBAL(handle.0);
                let ptr = GlobalLock(hglobal) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    let text = String::from_utf16_lossy(slice).replace("\r\n", "\n");
                    let _ = GlobalUnlock(hglobal);
                    return Some(ClipboardPastePayload::Text(text));
                }
            }
            None
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn read_windows_clipboard_payload() -> Option<ClipboardPastePayload> {
    None
}

/// 识别粘贴到 AI Composer 中的长代码/多行代码片段，以便折叠展示而不撑爆输入框
pub fn detect_pasted_code_snippet(text: &str) -> Option<(String, String, usize)> {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let line_count = lines.len();
    let total_chars = normalized.chars().count();
    let looks_like_code = normalized.contains("fn ")
        || normalized.contains("impl ")
        || normalized.contains("struct ")
        || normalized.contains("function ")
        || normalized.contains("const ")
        || normalized.contains("class ")
        || normalized.contains("def ")
        || normalized.contains("import ")
        || (normalized.contains('{') && normalized.contains('}'))
        || normalized.contains("=>")
        || normalized.contains("::")
        || lines.iter().any(|l| l.starts_with("    ") || l.starts_with('\t'));

    let should_collapse =
        line_count >= 5 || (line_count >= 3 && (looks_like_code || total_chars >= 160));
    if !should_collapse {
        return None;
    }

    let lang = if normalized.contains("fn ")
        || normalized.contains("impl ")
        || normalized.contains("let mut ")
        || normalized.contains("::")
        || normalized.contains("pub struct")
    {
        "rust"
    } else if normalized.contains("def ")
        || normalized.contains("elif ")
        || (normalized.contains("import ") && !normalized.contains(';'))
    {
        "python"
    } else if normalized.contains("interface ")
        || normalized.contains("function ")
        || normalized.contains("const ")
        || normalized.contains("=>")
    {
        "typescript"
    } else if normalized.trim_start().starts_with('{') && normalized.trim_end().ends_with('}') {
        "json"
    } else {
        "code"
    };

    let first_sig = lines
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("代码片段");
    let preview: String = first_sig.chars().take(18).collect();
    let title = if first_sig.chars().count() > 18 {
        format!("{preview}…")
    } else {
        preview
    };
    Some((title, lang.to_string(), line_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_png_roundtrip_preview_and_pasted_code_detection() {
        let mut rgba = Vec::with_capacity(16 * 12 * 4);
        for y in 0..12u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[x * 15, y * 20, 180, 255]);
            }
        }
        let png_path = encode_rgba_to_temp_png(16, 12, &rgba).expect("encode temp png");
        let loaded = load_image_for_preview(&png_path).expect("load png preview");
        assert_eq!(loaded.width, 16);
        assert_eq!(loaded.height, 12);
        assert_eq!(loaded.orig_width, 16);
        assert_eq!(loaded.orig_height, 12);
        assert_eq!(loaded.rgba.len(), 16 * 12 * 4);
        assert_eq!(&loaded.rgba[0..4], &[0, 0, 180, 255]);
        let _ = fs::remove_file(&png_path);

        assert!(detect_pasted_code_snippet("hello world").is_none());
        let code = "fn demo(x: i32) -> i32 {\n    let y = x + 1;\n    println!(\"{y}\");\n    y\n}";
        let detected = detect_pasted_code_snippet(code).expect("multi-line rust code detected");
        assert_eq!(detected.1, "rust");
        assert_eq!(detected.2, 5);
    }
}
