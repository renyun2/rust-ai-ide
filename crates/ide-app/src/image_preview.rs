#[cfg(target_os = "windows")]
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static CLIP_IMG_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Svg,
    Audio,
    Video,
}

#[derive(Debug, Clone)]
pub struct DecodedPreviewImage {
    pub orig_width: u32,
    pub orig_height: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct MediaPreviewTab {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub format_label: String,
    pub file_size_bytes: u64,
    pub preview_image: Option<Arc<DecodedPreviewImage>>,
    pub orig_dimensions: Option<(u32, u32)>,
    pub duration_secs: Option<f64>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
    pub waveform_peaks: Vec<f32>,
    pub zoom_pct: u32,
}

#[derive(Debug, Clone)]
pub enum ClipboardPastePayload {
    Files(Vec<PathBuf>),
    ImageFile(PathBuf),
    Text(String),
}

pub fn detect_media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "ico" | "tif" | "tiff" | "avif"
        | "heic" => Some(MediaKind::Image),
        "svg" => Some(MediaKind::Svg),
        "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" | "wma" | "opus" => {
            Some(MediaKind::Audio)
        }
        "mp4" | "webm" | "mov" | "avi" | "mkv" | "m4v" | "wmv" | "flv" => Some(MediaKind::Video),
        _ => None,
    }
}

pub fn is_image_file_path(path: &Path) -> bool {
    matches!(
        detect_media_kind(path),
        Some(MediaKind::Image | MediaKind::Svg)
    )
}

pub fn load_image_for_preview(path: &Path) -> Option<DecodedPreviewImage> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "svg" {
        let svg_text = fs::read_to_string(path).ok()?;
        let (orig_w, orig_h, rgba) = crate::svg_icon::rasterize_svg_to_rgba(&svg_text, 960)?;
        return Some(DecodedPreviewImage {
            orig_width: orig_w,
            orig_height: orig_h,
            width: orig_w,
            height: orig_h,
            rgba,
        });
    }

    let bytes = fs::read(path).ok()?;
    let decoded = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        decode_png_bytes(&bytes)
    } else if bytes.starts_with(b"BM") {
        decode_bmp_bytes(&bytes)
    } else {
        None
    }
    .or_else(|| decode_with_windows_gdiplus(path))
    .or_else(|| decode_with_image_crate(&bytes))
    .or_else(|| extract_windows_shell_thumbnail(path, 960, false));

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

pub fn load_media_preview_tab(
    path: &Path,
    existing_image: Option<Arc<DecodedPreviewImage>>,
) -> MediaPreviewTab {
    let kind = detect_media_kind(path).unwrap_or(MediaKind::Image);
    let format_label = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_else(|| "MEDIA".to_string());
    let file_size_bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let mut preview_image = existing_image;
    let mut orig_dimensions = preview_image
        .as_ref()
        .map(|im| (im.orig_width, im.orig_height));
    let mut duration_secs = None;
    let mut sample_rate_hz = None;
    let mut channels = None;
    let mut waveform_peaks = Vec::new();

    match kind {
        MediaKind::Image | MediaKind::Svg => {
            if preview_image.is_none() {
                if let Some(im) = load_image_for_preview(path) {
                    orig_dimensions = Some((im.orig_width, im.orig_height));
                    preview_image = Some(Arc::new(im));
                }
            }
        }
        MediaKind::Video => {
            if preview_image.is_none() {
                if let Some((w, h, rgba)) = extract_windows_shell_thumbnail(path, 960, true) {
                    orig_dimensions = Some((w, h));
                    preview_image = Some(Arc::new(DecodedPreviewImage {
                        orig_width: w,
                        orig_height: h,
                        width: w,
                        height: h,
                        rgba,
                    }));
                }
            }
            duration_secs = read_mp4_mvhd_duration_bounded(path, file_size_bytes);
        }
        MediaKind::Audio => {
            if format_label == "M4A" {
                duration_secs = read_mp4_mvhd_duration_bounded(path, file_size_bytes);
            } else if file_size_bytes <= 16 * 1024 * 1024 {
                if let Ok(bytes) = fs::read(path) {
                    if let Some((dur, sr, ch, peaks)) = parse_wav_info_and_peaks(&bytes) {
                        duration_secs = Some(dur);
                        sample_rate_hz = Some(sr);
                        channels = Some(ch);
                        waveform_peaks = peaks;
                    }
                }
            }
        }
    }

    MediaPreviewTab {
        path: path.to_path_buf(),
        kind,
        format_label,
        file_size_bytes,
        preview_image,
        orig_dimensions,
        duration_secs,
        sample_rate_hz,
        channels,
        waveform_peaks,
        zoom_pct: 100,
    }
}

pub fn format_media_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_media_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let mins = total / 60;
    let rem = total % 60;
    let hrs = mins / 60;
    if hrs > 0 {
        format!("{hrs}:{:02}:{rem:02}", mins % 60)
    } else {
        format!("{mins:02}:{rem:02}")
    }
}

#[cfg(target_os = "windows")]
pub fn open_media_with_system_app(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let res = ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        (res.0 as isize) > 32
    }
}

#[cfg(not(target_os = "windows"))]
pub fn open_media_with_system_app(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn parse_wav_info_and_peaks(bytes: &[u8]) -> Option<(f64, u32, u16, Vec<f32>)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits_per_sample = 0u16;
    let mut data_slice: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let payload_start = pos + 8;
        let payload_end = payload_start.saturating_add(size).min(bytes.len());
        if id == b"fmt " && payload_start + 16 <= payload_end {
            channels = u16::from_le_bytes(bytes[payload_start + 2..payload_start + 4].try_into().ok()?);
            sample_rate =
                u32::from_le_bytes(bytes[payload_start + 4..payload_start + 8].try_into().ok()?);
            bits_per_sample = u16::from_le_bytes(
                bytes[payload_start + 14..payload_start + 16].try_into().ok()?,
            );
        } else if id == b"data" {
            data_slice = Some(&bytes[payload_start..payload_end]);
        }
        pos = payload_start.saturating_add(size + (size & 1));
    }

    if channels == 0 || sample_rate == 0 || bits_per_sample == 0 {
        return None;
    }
    let data = data_slice?;
    let bytes_per_frame = (channels as usize) * ((bits_per_sample as usize) / 8).max(1);
    let total_frames = data.len() / bytes_per_frame.max(1);
    let duration_secs = total_frames as f64 / sample_rate as f64;

    let bars = 64usize;
    let mut peaks = Vec::with_capacity(bars);
    if total_frames > 0 && (bits_per_sample == 16 || bits_per_sample == 8) {
        let frames_per_bar = (total_frames / bars).max(1);
        for b in 0..bars {
            let start_f = (b * total_frames) / bars;
            let end_f = (((b + 1) * total_frames) / bars).min(total_frames);
            let step = ((end_f - start_f) / 32).max(1);
            let mut max_amp = 0.0f32;
            let mut f = start_f;
            while f < end_f {
                let off = f * bytes_per_frame;
                let amp = if bits_per_sample == 16 && off + 2 <= data.len() {
                    let s = i16::from_le_bytes([data[off], data[off + 1]]);
                    (s as f32 / 32768.0).abs()
                } else if bits_per_sample == 8 && off < data.len() {
                    ((data[off] as f32 - 128.0) / 128.0).abs()
                } else {
                    0.0
                };
                if amp > max_amp {
                    max_amp = amp;
                }
                f += step.max(1).min(frames_per_bar);
            }
            peaks.push(max_amp.clamp(0.06, 1.0));
        }
    }
    Some((duration_secs, sample_rate, channels, peaks))
}

/// 仅读取 MP4/M4A 头部与尾部各最多 512 KB 查找 `mvhd`，避免把几十 MB 视频整文件读入内存。
fn read_mp4_mvhd_duration_bounded(path: &Path, file_size: u64) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: usize = 512 * 1024;
    let mut f = File::open(path).ok()?;
    let head_len = (file_size as usize).min(WINDOW);
    let mut buf = vec![0u8; head_len];
    let n = f.read(&mut buf).ok()?;
    if let Some(d) = parse_mp4_mvhd_duration(&buf[..n]) {
        return Some(d);
    }
    if file_size > WINDOW as u64 {
        let tail_start = file_size.saturating_sub(WINDOW as u64);
        if f.seek(SeekFrom::Start(tail_start)).is_ok() {
            let mut tail = vec![0u8; WINDOW];
            if let Ok(tn) = f.read(&mut tail) {
                return parse_mp4_mvhd_duration(&tail[..tn]);
            }
        }
    }
    None
}

fn parse_mp4_mvhd_duration(bytes: &[u8]) -> Option<f64> {
    let idx = bytes.windows(4).position(|w| w == b"mvhd")?;
    if idx + 24 > bytes.len() {
        return None;
    }
    let version = bytes[idx + 4];
    if version == 0 && idx + 24 <= bytes.len() {
        let timescale = u32::from_be_bytes(bytes[idx + 16..idx + 20].try_into().ok()?);
        let duration = u32::from_be_bytes(bytes[idx + 20..idx + 24].try_into().ok()?);
        if timescale > 0 {
            return Some(duration as f64 / timescale as f64);
        }
    } else if version == 1 && idx + 36 <= bytes.len() {
        let timescale = u32::from_be_bytes(bytes[idx + 24..idx + 28].try_into().ok()?);
        let duration = u64::from_be_bytes(bytes[idx + 28..idx + 36].try_into().ok()?);
        if timescale > 0 {
            return Some(duration as f64 / timescale as f64);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn extract_windows_shell_thumbnail(
    path: &Path,
    target_size: i32,
    thumbnail_only: bool,
) -> Option<(u32, u32, Vec<u8>)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
        SIIGBF_THUMBNAILONLY,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let com_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_inited = com_hr.is_ok();
        let factory_res: windows::core::Result<IShellItemImageFactory> =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None);
        let out = (|| -> Option<(u32, u32, Vec<u8>)> {
            let factory = factory_res.ok()?;
            let flags = if thumbnail_only {
                SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY
            } else {
                SIIGBF_BIGGERSIZEOK
            };
            let hbmp = factory
                .GetImage(
                    SIZE {
                        cx: target_size,
                        cy: target_size,
                    },
                    flags,
                )
                .ok()?;
            let mut bmp = BITMAP::default();
            if GetObjectW(
                HGDIOBJ(hbmp.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bmp as *mut _ as *mut c_void),
            ) == 0
            {
                let _ = DeleteObject(HGDIOBJ(hbmp.0));
                return None;
            }
            let w = bmp.bmWidth.max(0) as u32;
            let h = bmp.bmHeight.max(0) as u32;
            if w == 0 || h == 0 || w > 4096 || h > 4096 {
                let _ = DeleteObject(HGDIOBJ(hbmp.0));
                return None;
            }
            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let hdc = GetDC(None);
            let lines = GetDIBits(
                hdc,
                hbmp,
                0,
                h,
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut bi,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, hdc);
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            if lines == 0 {
                return None;
            }
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2); // BGRA -> RGBA
            }
            Some((w, h, buf))
        })();
        if com_inited {
            CoUninitialize();
        }
        out
    }
}

#[cfg(not(target_os = "windows"))]
fn extract_windows_shell_thumbnail(
    _path: &Path,
    _target_size: i32,
    _thumbnail_only: bool,
) -> Option<(u32, u32, Vec<u8>)> {
    None
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

fn decode_dib_slice(
    header_and_pixels: &[u8],
    explicit_pixels: Option<&[u8]>,
) -> Option<(u32, u32, Vec<u8>)> {
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
    if width_i <= 0
        || height_i == 0
        || (bpp != 24 && bpp != 32)
        || (compression != 0 && compression != 3)
    {
        return None;
    }
    let w = width_i as usize;
    let h = height_i.unsigned_abs() as usize;
    let top_down = height_i < 0;
    let extra_masks = if compression == 3 && hdr_size == 40 {
        12
    } else {
        0
    };
    let pixels = explicit_pixels
        .unwrap_or(&header_and_pixels[(hdr_size + extra_masks).min(header_and_pixels.len())..]);
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
                if raw_a == 0 {
                    255
                } else {
                    raw_a
                }
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
        lock_bits:
            unsafe extern "system" fn(*mut c_void, *const GpRect, u32, i32, *mut BitmapData) -> i32,
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
            let startup: unsafe extern "system" fn(
                *mut usize,
                *const GdiplusStartupInput,
                *mut c_void,
            ) -> i32 = std::mem::transmute(startup_p);
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
        if (fns.lock_bits)(gp_img, &rect, 1, 0x0026_200A, &mut bdata) != 0 || bdata.scan0.is_null()
        {
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

#[cfg(not(target_os = "windows"))]
fn decode_with_image_crate(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

#[cfg(target_os = "windows")]
fn decode_with_image_crate(_bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
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
    crate::ide_state::with_arboard_clipboard(|cb| {
        if let Ok(files) = cb.get().file_list() {
            if !files.is_empty() {
                return Some(ClipboardPastePayload::Files(files));
            }
        }
        if let Ok(img) = cb.get_image() {
            if img.width > 0 && img.height > 0 {
                if let Some(saved) =
                    encode_rgba_to_temp_png(img.width as u32, img.height as u32, &img.bytes)
                {
                    return Some(ClipboardPastePayload::ImageFile(saved));
                }
            }
        }
        if let Ok(text) = cb.get_text() {
            return Some(ClipboardPastePayload::Text(text.replace("\r\n", "\n")));
        }
        None
    })
    .flatten()
}

/// 识别粘贴到 AI Composer 中的长代码/多行代码片段，以便折叠展示而不撑爆输入框
pub fn detect_pasted_code_snippet(text: &str) -> Option<(String, String, usize)> {
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let line_count = lines.len();
    let total_chars = normalized.chars().count();
    let looks_like_markup = normalized.contains("<template")
        || normalized.contains("</template>")
        || normalized.contains("v-model")
        || normalized.contains("v-if=")
        || normalized.contains("v-for=")
        || normalized.contains("v-bind")
        || normalized.contains("@click")
        || (normalized.contains('<') && normalized.contains("/>"));
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
        || looks_like_markup
        || lines
            .iter()
            .any(|l| l.starts_with("    ") || l.starts_with('\t'));

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
    } else if normalized.contains("<template")
        || normalized.contains("</template>")
        || normalized.contains("<script setup")
        || normalized.contains("v-model")
        || normalized.contains("v-if=")
        || normalized.contains("v-for=")
        || normalized.contains("v-bind")
        || normalized.contains("v-on")
        || normalized.contains("@click")
        || (normalized.contains('<')
            && (normalized.contains(" :") || normalized.contains("\n"))
            && (normalized.contains("/>") || normalized.contains("</")))
    {
        "vue"
    } else if normalized.contains("<!doctype")
        || normalized.contains("<!DOCTYPE")
        || normalized.contains("<html")
    {
        "html"
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

        let vue_code = "<EstimateModalThFilter\n  v-model=\"modalFilterRemarks\"\n  col-key=\"modal.inst.remarks\"\n  :default-width=\"80\"\n/>";
        let vue_det = detect_pasted_code_snippet(vue_code).expect("multi-line vue code detected");
        assert_eq!(vue_det.1, "vue");
    }

    #[test]
    fn test_media_detection_svg_rasterize_and_wav_waveform_parsing() {
        assert_eq!(detect_media_kind(Path::new("logo.svg")), Some(MediaKind::Svg));
        assert_eq!(detect_media_kind(Path::new("pic.webp")), Some(MediaKind::Image));
        assert_eq!(detect_media_kind(Path::new("song.mp3")), Some(MediaKind::Audio));
        assert_eq!(detect_media_kind(Path::new("clip.mp4")), Some(MediaKind::Video));
        assert_eq!(detect_media_kind(Path::new("main.rs")), None);

        let svg_path = std::env::temp_dir().join("rust_ai_ide_media_test.svg");
        fs::write(
            &svg_path,
            r##"<svg viewBox="0 0 32 32"><rect x="2" y="2" width="28" height="28" fill="#3B82F6"/></svg>"##,
        )
        .unwrap();
        let tab = load_media_preview_tab(&svg_path, None);
        assert_eq!(tab.kind, MediaKind::Svg);
        assert!(tab.preview_image.is_some());
        assert_eq!(tab.orig_dimensions, Some((960, 960)));
        let _ = fs::remove_file(&svg_path);

        // Minimal 16-bit mono 8kHz WAV header + 8000 samples (1.0s)
        let num_samples = 8000u32;
        let data_bytes = num_samples * 2;
        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        wav.extend_from_slice(&(8000u32 * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..num_samples {
            let v: i16 = if i % 2 == 0 { 16000 } else { -16000 };
            wav.extend_from_slice(&v.to_le_bytes());
        }
        let wav_path = std::env::temp_dir().join("rust_ai_ide_media_test.wav");
        fs::write(&wav_path, &wav).unwrap();
        let wav_tab = load_media_preview_tab(&wav_path, None);
        assert_eq!(wav_tab.kind, MediaKind::Audio);
        assert_eq!(wav_tab.sample_rate_hz, Some(8000));
        assert_eq!(wav_tab.channels, Some(1));
        assert!((wav_tab.duration_secs.unwrap_or(0.0) - 1.0).abs() < 0.01);
        assert_eq!(wav_tab.waveform_peaks.len(), 64);
        let _ = fs::remove_file(&wav_path);
    }
}
