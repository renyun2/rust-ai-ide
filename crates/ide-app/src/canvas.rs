use anyhow::Result;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::ptr;
use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, ExtTextOutW,
    GdiFlush, GetTextExtentPoint32W, SelectObject, SetBkMode, SetTextColor, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DIB_RGB_COLORS, ETO_CLIPPED, FF_DONTCARE, FF_MODERN, FIXED_PITCH, FW_BOLD, FW_NORMAL,
    FW_SEMIBOLD, HBITMAP, HDC, HFONT, OUT_TT_PRECIS, TRANSPARENT,
};

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    UiRegular,
    UiBold,
    UiSmall,
    CodeMono,
    CodeMonoBold,
}

/// 基于 Win32 DIB Section + ClearType 原生矢量字体（Cascadia Code + Microsoft YaHei UI）与 4x4 超采样矢量图标引擎的纯 Rust 画布
pub struct SurfaceCanvas {
    pub width: usize,
    pub height: usize,
    hdc: HDC,
    hbmp: HBITMAP,
    bits_ptr: *mut u32,
    font_ui_reg: HFONT,
    font_ui_bold: HFONT,
    font_ui_small: HFONT,
    font_code_mono: HFONT,
    font_code_bold: HFONT,
}

impl SurfaceCanvas {
    pub fn new(width: usize, height: usize) -> Self {
        unsafe {
            let hdc = CreateCompatibleDC(ptr::null_mut());
            SetBkMode(hdc, TRANSPARENT as i32);

            let font_ui_reg =
                create_cleartype_font(-14, FW_NORMAL as i32, false, "Microsoft YaHei UI");
            let font_ui_bold =
                create_cleartype_font(-14, FW_SEMIBOLD as i32, false, "Microsoft YaHei UI");
            let font_ui_small =
                create_cleartype_font(-12, FW_NORMAL as i32, false, "Microsoft YaHei UI");
            // 优先使用现代编程等宽字体 Cascadia Mono（与 JetBrains Mono 字形比例一致）
            let font_code_mono =
                create_cleartype_font(-15, FW_NORMAL as i32, true, "Cascadia Mono");
            let font_code_bold =
                create_cleartype_font(-15, FW_BOLD as i32, true, "Cascadia Mono");

            let mut canvas = Self {
                width: 0,
                height: 0,
                hdc,
                hbmp: ptr::null_mut(),
                bits_ptr: ptr::null_mut(),
                font_ui_reg,
                font_ui_bold,
                font_ui_small,
                font_code_mono,
                font_code_bold,
            };
            canvas.resize(width, height);
            canvas
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        let w = width.max(1);
        let h = height.max(1);
        if w == self.width && h == self.height && !self.bits_ptr.is_null() {
            return;
        }
        unsafe {
            if !self.hbmp.is_null() {
                DeleteObject(self.hbmp);
                self.hbmp = ptr::null_mut();
                self.bits_ptr = ptr::null_mut();
            }

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [std::mem::zeroed()],
            };

            let mut bits: *mut core::ffi::c_void = ptr::null_mut();
            let hbmp = CreateDIBSection(
                self.hdc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            );
            SelectObject(self.hdc, hbmp);
            self.width = w;
            self.height = h;
            self.hbmp = hbmp;
            self.bits_ptr = bits as *mut u32;
        }
    }

    pub fn pixels(&self) -> &[u32] {
        unsafe {
            GdiFlush();
            std::slice::from_raw_parts(self.bits_ptr, self.width * self.height)
        }
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe {
            GdiFlush();
            std::slice::from_raw_parts_mut(self.bits_ptr, self.width * self.height)
        }
    }

    pub fn clear(&mut self, color: u32) {
        self.pixels_mut().fill(color);
    }

    pub fn fill_rect(&mut self, rect: Rect, color: u32) {
        let w = self.width as i32;
        let h = self.height as i32;
        let x0 = rect.x.clamp(0, w) as usize;
        let y0 = rect.y.clamp(0, h) as usize;
        let x1 = (rect.x + rect.w).clamp(0, w) as usize;
        let y1 = (rect.y + rect.h).clamp(0, h) as usize;

        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.width;
        let buf = self.pixels_mut();
        for y in y0..y1 {
            let row = y * stride;
            buf[row + x0..row + x1].fill(color);
        }
    }

    /// 4x4 超采样抗锯齿圆角矩形（呈现出与 JetBrains Islands UI 相同的丝滑圆角边缘）
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: i32, color: u32) {
        if radius <= 1 {
            self.fill_rect(rect, color);
            return;
        }
        let r = radius.min(rect.w / 2).min(rect.h / 2) as f32;
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = (rect.x + rect.w).min(self.width as i32);
        let y1 = (rect.y + rect.h).min(self.height as i32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let left_c = rect.x as f32 + r;
        let right_c = (rect.x + rect.w) as f32 - r;
        let top_c = rect.y as f32 + r;
        let bot_c = (rect.y + rect.h) as f32 - r;

        for py in y0..y1 {
            let fy = py as f32 + 0.5;
            if fy >= top_c && fy <= bot_c {
                let row = (py as usize) * self.width;
                let buf = self.pixels_mut();
                buf[row + (x0 as usize)..row + (x1 as usize)].fill(color);
                continue;
            }
            for px in x0..x1 {
                let fx = px as f32 + 0.5;
                if fx >= left_c && fx <= right_c {
                    self.put_pixel(px, py, color);
                } else {
                    let cx = if fx < left_c { left_c } else { right_c };
                    let cy = if fy < top_c { top_c } else { bot_c };
                    let dist = ((fx - cx) * (fx - cx) + (fy - cy) * (fy - cy)).sqrt();
                    let coverage = (r + 0.5 - dist).clamp(0.0, 1.0);
                    if coverage >= 0.99 {
                        self.put_pixel(px, py, color);
                    } else if coverage > 0.01 {
                        self.blend_pixel(px, py, color, (coverage * 255.0) as u8);
                    }
                }
            }
        }
    }

    pub fn stroke_rounded_rect(
        &mut self,
        rect: Rect,
        radius: i32,
        border_color: u32,
        fill_color: u32,
    ) {
        self.fill_rounded_rect(rect, radius, border_color);
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            (rect.w - 2).max(0),
            (rect.h - 2).max(0),
        );
        self.fill_rounded_rect(inner, (radius - 1).max(0), fill_color);
    }

    /// 抗锯齿实心圆（用于红点断点、状态灯）
    pub fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        let rf = r as f32;
        for dy in -(r + 1)..=(r + 1) {
            for dx in -(r + 1)..=(r + 1) {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                let cov = (rf + 0.5 - dist).clamp(0.0, 1.0);
                if cov >= 0.99 {
                    self.put_pixel(cx + dx, cy + dy, color);
                } else if cov > 0.02 {
                    self.blend_pixel(cx + dx, cy + dy, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// 抗锯齿空心圆环
    pub fn stroke_circle(&mut self, cx: i32, cy: i32, r: f32, thickness: f32, color: u32) {
        let bound = (r + thickness + 2.0) as i32;
        for dy in -bound..=bound {
            for dx in -bound..=bound {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                let d_edge = (dist - r).abs();
                let cov = (thickness * 0.5 + 0.5 - d_edge).clamp(0.0, 1.0);
                if cov > 0.02 {
                    self.blend_pixel(cx + dx, cy + dy, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// 抗锯齿线段绘制
    pub fn draw_line_aa(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: u32) {
        let min_x = (x0.min(x1) - thickness - 1.0).floor() as i32;
        let max_x = (x0.max(x1) + thickness + 1.0).ceil() as i32;
        let min_y = (y0.min(y1) - thickness - 1.0).floor() as i32;
        let max_y = (y0.max(y1) + thickness + 1.0).ceil() as i32;

        let vx = x1 - x0;
        let vy = y1 - y0;
        let len2 = (vx * vx + vy * vy).max(0.0001);

        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let fy = py as f32 + 0.5;
                let t = (((fx - x0) * vx + (fy - y0) * vy) / len2).clamp(0.0, 1.0);
                let proj_x = x0 + t * vx;
                let proj_y = y0 + t * vy;
                let dist = ((fx - proj_x) * (fx - proj_x) + (fy - proj_y) * (fy - proj_y)).sqrt();
                let cov = (thickness * 0.5 + 0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.02 {
                    self.blend_pixel(px, py, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// IDEA 标志性绿色运行三角 ▶（抗锯齿）
    pub fn fill_play_triangle(&mut self, x: i32, y: i32, size: i32, color: u32) {
        let half = size as f32 * 0.5;
        for dy in 0..size {
            let fy = dy as f32 + 0.5;
            let dist_from_mid = (fy - half).abs();
            let max_dx = ((half - dist_from_mid) * 1.25).max(0.5);
            for dx in 0..=(max_dx.ceil() as i32) {
                let cov = (max_dx + 0.5 - dx as f32).clamp(0.0, 1.0);
                if cov > 0.02 {
                    self.blend_pixel(x + dx, y + dy, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// 绘制平滑下拉箭头图标 ▾
    pub fn draw_chevron_down(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(x as f32, y as f32, (x + 4) as f32, (y + 4) as f32, 1.5, color);
        self.draw_line_aa((x + 4) as f32, (y + 4) as f32, (x + 8) as f32, y as f32, 1.5, color);
    }

    /// 绘制折叠状态向右箭头 ▸
    pub fn draw_chevron_right(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(x as f32, y as f32, (x + 4) as f32, (y + 4) as f32, 1.5, color);
        self.draw_line_aa((x + 4) as f32, (y + 4) as f32, x as f32, (y + 8) as f32, 1.5, color);
    }

    /// 绘制 IDEA 风格放大镜搜索图标
    pub fn draw_search_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_circle(x + 6, y + 6, 4.8, 1.5, color);
        self.draw_line_aa((x + 10) as f32, (y + 10) as f32, (x + 14) as f32, (y + 14) as f32, 1.8, color);
    }

    /// 绘制 IDEA 顶部三横杠主菜单图标 (Hamburger Menu)
    pub fn draw_hamburger_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_rounded_rect(Rect::new(x, y, 14, 2), 1, color);
        self.fill_rounded_rect(Rect::new(x, y + 5, 14, 2), 1, color);
        self.fill_rounded_rect(Rect::new(x, y + 10, 14, 2), 1, color);
    }

    /// 绘制 IDEA 风格 Debug 甲虫/靶心图标
    pub fn draw_debug_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_circle(x + 6, y + 7, 4, color);
        self.draw_line_aa((x + 1) as f32, (y + 4) as f32, (x + 11) as f32, (y + 4) as f32, 1.3, color);
        self.draw_line_aa((x + 1) as f32, (y + 7) as f32, (x + 11) as f32, (y + 7) as f32, 1.3, color);
        self.draw_line_aa((x + 2) as f32, (y + 10) as f32, (x + 10) as f32, (y + 10) as f32, 1.3, color);
    }

    /// 绘制 IDEA 风格 Git 分支矢量图标
    pub fn draw_git_branch_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_circle(x + 2, y + 2, 2, color);
        self.fill_circle(x + 2, y + 11, 2, color);
        self.fill_circle(x + 9, y + 5, 2, color);
        self.draw_line_aa((x + 2) as f32, (y + 2) as f32, (x + 2) as f32, (y + 11) as f32, 1.6, color);
        self.draw_line_aa((x + 2) as f32, (y + 8) as f32, (x + 9) as f32, (y + 5) as f32, 1.5, color);
    }

    /// 绘制 IDEA 风格文件夹矢量图标
    pub fn draw_folder_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_rounded_rect(Rect::new(x, y + 1, 6, 3), 1, color);
        self.fill_rounded_rect(Rect::new(x, y + 3, 14, 9), 2, color);
    }

    /// 绘制新版 IDEA 顶部空心绿色运行三角 ▶
    pub fn stroke_play_triangle(&mut self, x: i32, y: i32, size: i32, color: u32) {
        let fx = x as f32;
        let fy = y as f32;
        let s = size as f32;
        self.draw_line_aa(fx, fy, fx, fy + s, 1.6, color);
        self.draw_line_aa(fx, fy, fx + s * 0.85, fy + s * 0.5, 1.6, color);
        self.draw_line_aa(fx, fy + s, fx + s * 0.85, fy + s * 0.5, 1.6, color);
    }

    /// 绘制新版 IDEA 顶部设置齿轮矢量图标 ⚙
    pub fn draw_gear_icon(&mut self, x: i32, y: i32, color: u32) {
        let cx = x + 7;
        let cy = y + 7;
        self.stroke_circle(cx, cy, 4.8, 1.6, color);
        self.fill_circle(cx, cy, 1, color);
        for &(dx, dy) in &[
            (0, -6),
            (0, 6),
            (-6, 0),
            (6, 0),
            (-4, -4),
            (4, 4),
            (-4, 4),
            (4, -4),
        ] {
            self.fill_circle(cx + dx, cy + dy, 1, color);
        }
    }

    /// 绘制可拖拽/指示位置的垂直滚动条
    pub fn draw_vertical_scrollbar(
        &mut self,
        track_rect: Rect,
        scroll_offset: usize,
        visible_items: usize,
        total_items: usize,
        thumb_color: u32,
    ) -> Rect {
        if total_items <= visible_items || track_rect.h <= 16 {
            return Rect::new(track_rect.x, track_rect.y, track_rect.w, 0);
        }
        let max_scroll = total_items.saturating_sub(visible_items).max(1);
        let clamped_offset = scroll_offset.min(max_scroll);
        let thumb_h = ((visible_items as f32 / total_items as f32) * (track_rect.h as f32))
            .clamp(24.0, track_rect.h as f32 - 4.0) as i32;
        let avail_h = (track_rect.h - thumb_h).max(1);
        let thumb_y =
            track_rect.y + ((clamped_offset as f32 / max_scroll as f32) * (avail_h as f32)) as i32;
        let thumb_rect = Rect::new(track_rect.x + 1, thumb_y, (track_rect.w - 2).max(4), thumb_h);
        self.fill_rounded_rect(thumb_rect, 3, thumb_color);
        thumb_rect
    }

    #[inline]
    pub fn put_pixel(&mut self, x: i32, y: i32, color: u32) {
        if x >= 0 && (x as usize) < self.width && y >= 0 && (y as usize) < self.height {
            let idx = (y as usize) * self.width + (x as usize);
            unsafe {
                *self.bits_ptr.add(idx) = color;
            }
        }
    }

    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, fg: u32, alpha: u8) {
        if x >= 0 && (x as usize) < self.width && y >= 0 && (y as usize) < self.height {
            let idx = (y as usize) * self.width + (x as usize);
            unsafe {
                let bg = *self.bits_ptr.add(idx);
                let a = alpha as u32;
                let inv_a = 255 - a;
                let r = (((fg >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * inv_a) / 255;
                let g = (((fg >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * inv_a) / 255;
                let b = ((fg & 0xFF) * a + (bg & 0xFF) * inv_a) / 255;
                *self.bits_ptr.add(idx) = (r << 16) | (g << 8) | b;
            }
        }
    }

    fn select_font(&self, style: FontStyle) {
        let hfont = match style {
            FontStyle::UiRegular => self.font_ui_reg,
            FontStyle::UiBold => self.font_ui_bold,
            FontStyle::UiSmall => self.font_ui_small,
            FontStyle::CodeMono => self.font_code_mono,
            FontStyle::CodeMonoBold => self.font_code_bold,
        };
        unsafe {
            SelectObject(self.hdc, hfont);
        }
    }

    /// 精确测量文本在指定字体下的像素宽高 `(width, height)`
    pub fn measure_text(&self, text: &str, style: FontStyle) -> (i32, i32) {
        if text.is_empty() {
            return (0, 16);
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.select_font(style);
            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(self.hdc, wide.as_ptr(), wide.len() as i32, &mut sz);
            (sz.cx, sz.cy)
        }
    }

    /// 使用 Win32 ClearType 矢量字体渲染平滑中英文/符号文本，并返回绘制后的右边界 X 坐标
    pub fn draw_text_styled(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        max_width: i32,
        style: FontStyle,
    ) -> i32 {
        if text.is_empty() || max_width <= 0 {
            return x;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.select_font(style);
            SetTextColor(self.hdc, rgb_to_colorref(color));

            let clip_rect = RECT {
                left: x,
                top: y,
                right: x + max_width,
                bottom: y + 28,
            };

            ExtTextOutW(
                self.hdc,
                x,
                y,
                ETO_CLIPPED,
                &clip_rect,
                wide.as_ptr(),
                wide.len() as u32,
                ptr::null(),
            );

            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(self.hdc, wide.as_ptr(), wide.len() as i32, &mut sz);
            x + sz.cx.min(max_width)
        }
    }

    /// 在矩形区域内水平+垂直居中绘制文本
    pub fn draw_text_centered(
        &mut self,
        rect: Rect,
        text: &str,
        color: u32,
        style: FontStyle,
    ) {
        let (tw, th) = self.measure_text(text, style);
        let tx = rect.x + ((rect.w - tw).max(4)) / 2;
        let ty = rect.y + ((rect.h - th).max(2)) / 2;
        self.draw_text_styled(tx, ty, text, color, rect.w - 4, style);
    }

    /// 按像素最大宽度自动折行
    pub fn wrap_text_by_pixel_width(
        &self,
        text: &str,
        max_width: i32,
        style: FontStyle,
    ) -> Vec<String> {
        let mut wrapped = Vec::new();
        for raw_line in text.lines() {
            if raw_line.is_empty() {
                wrapped.push(String::new());
                continue;
            }
            let mut current = String::new();
            for ch in raw_line.chars() {
                current.push(ch);
                let (w, _) = self.measure_text(&current, style);
                if w > max_width && current.chars().count() > 1 {
                    current.pop();
                    wrapped.push(current);
                    current = ch.to_string();
                }
            }
            if !current.is_empty() {
                wrapped.push(current);
            }
        }
        wrapped
    }

    /// 将当前帧缓冲导出为 PNG 截图文件
    pub fn save_png(&self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        let w = BufWriter::new(file);
        let mut encoder = png::Encoder::new(w, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;

        let src = self.pixels();
        let mut rgba = Vec::with_capacity(src.len() * 4);
        for &px in src {
            let r = ((px >> 16) & 0xFF) as u8;
            let g = ((px >> 8) & 0xFF) as u8;
            let b = (px & 0xFF) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
        writer.write_image_data(&rgba)?;
        Ok(())
    }
}

impl Drop for SurfaceCanvas {
    fn drop(&mut self) {
        unsafe {
            if !self.hbmp.is_null() {
                DeleteObject(self.hbmp);
            }
            DeleteObject(self.font_ui_reg);
            DeleteObject(self.font_ui_bold);
            DeleteObject(self.font_ui_small);
            DeleteObject(self.font_code_mono);
            DeleteObject(self.font_code_bold);
            if !self.hdc.is_null() {
                DeleteDC(self.hdc);
            }
        }
    }
}

#[inline]
fn rgb_to_colorref(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    r | (g << 8) | (b << 16)
}

unsafe fn create_cleartype_font(height: i32, weight: i32, monospace: bool, face: &str) -> HFONT {
    let mut wide: Vec<u16> = face.encode_utf16().collect();
    wide.push(0);
    let pitch = if monospace {
        (FIXED_PITCH | FF_MODERN) as u32
    } else {
        FF_DONTCARE as u32
    };
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_TT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        pitch,
        wide.as_ptr(),
    )
}
