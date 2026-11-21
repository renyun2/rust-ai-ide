use crate::canvas::{FontStyle, Rect};
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight, Wrap,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use tiny_skia::{Pixmap, PremultipliedColorU8};

const MEASURE_CACHE_CAP: usize = 3072;
const SHAPED_CACHE_CAP: usize = 1024;
const MAX_CACHED_TEXT_BYTES: usize = 300;

struct SharedFontContext {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl SharedFontContext {
    fn new() -> Self {
        let mut font_system = FontSystem::new();
        let sans_candidates: &[&str] = if cfg!(target_os = "macos") {
            &[
                "SF Pro Text",
                "Helvetica Neue",
                "PingFang SC",
                "Arial",
            ]
        } else {
            &[
                "Ubuntu",
                "Noto Sans",
                "Noto Sans CJK SC",
                "DejaVu Sans",
                "Liberation Sans",
            ]
        };
        let mono_candidates: &[&str] = if cfg!(target_os = "macos") {
            &["SF Mono", "JetBrains Mono", "Menlo", "Monaco", "Courier New"]
        } else {
            &[
                "JetBrains Mono",
                "Ubuntu Mono",
                "DejaVu Sans Mono",
                "Liberation Mono",
                "Noto Sans Mono",
            ]
        };

        if let Some(sans) = pick_installed_family(&font_system, sans_candidates) {
            font_system.db_mut().set_sans_serif_family(&sans);
        }
        if let Some(mono) = pick_installed_family(&font_system, mono_candidates) {
            font_system.db_mut().set_monospace_family(&mono);
        }

        Self {
            font_system,
            swash_cache: SwashCache::new(),
        }
    }
}

fn pick_installed_family(font_system: &FontSystem, candidates: &[&str]) -> Option<String> {
    for &candidate in candidates {
        if font_system
            .db()
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name == candidate))
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn shared_font_context() -> Rc<RefCell<SharedFontContext>> {
    thread_local! {
        static FONT_CTX: RefCell<Option<Weak<RefCell<SharedFontContext>>>> =
            const { RefCell::new(None) };
    }
    FONT_CTX.with(|cell| {
        if let Some(existing) = cell.borrow().as_ref().and_then(|w| w.upgrade()) {
            return existing;
        }
        let ctx = Rc::new(RefCell::new(SharedFontContext::new()));
        *cell.borrow_mut() = Some(Rc::downgrade(&ctx));
        ctx
    })
}

pub fn shared_text_engine_for_scale(scale: f32) -> Rc<CosmicTextEngine> {
    thread_local! {
        static SHARED_COSMIC_ENGINES: RefCell<HashMap<u32, Weak<CosmicTextEngine>>> =
            RefCell::new(HashMap::new());
    }
    let key = (scale.clamp(0.75, 4.0) * 100.0).round() as u32;
    SHARED_COSMIC_ENGINES.with(|map_cell| {
        let mut map = map_cell.borrow_mut();
        map.retain(|_, w| w.strong_count() > 0);
        if let Some(existing) = map.get(&key).and_then(|w| w.upgrade()) {
            return existing;
        }
        let engine = Rc::new(CosmicTextEngine::new((key as f32) / 100.0));
        map.insert(key, Rc::downgrade(&engine));
        engine
    })
}

struct CachedGlyph {
    start: usize,
    end: usize,
    x: f32,
    w: f32,
    is_rtl: bool,
    physical: cosmic_text::PhysicalGlyph,
}

struct ShapedLine {
    log_mw: i32,
    log_th: i32,
    glyphs: Vec<CachedGlyph>,
}

struct CosmicState {
    scale_factor: f32,
    measure_cache: HashMap<FontStyle, HashMap<String, (i32, i32)>>,
    shaped_cache: HashMap<FontStyle, HashMap<String, Rc<ShapedLine>>>,
}

pub struct CosmicTextEngine {
    fonts: Rc<RefCell<SharedFontContext>>,
    state: RefCell<CosmicState>,
}

impl CosmicTextEngine {
    fn new(scale_factor: f32) -> Self {
        Self {
            fonts: shared_font_context(),
            state: RefCell::new(CosmicState {
                scale_factor: scale_factor.clamp(0.75, 4.0),
                measure_cache: HashMap::new(),
                shaped_cache: HashMap::new(),
            }),
        }
    }

    pub fn trim_caches(&self) {
        let mut st = self.state.borrow_mut();
        st.shaped_cache.clear();
        if st.measure_cache.values().map(|m| m.len()).sum::<usize>() > 1024 {
            st.measure_cache.clear();
        }
    }

    fn style_spec(style: FontStyle) -> (bool, f32, Weight) {
        match style {
            FontStyle::UiRegular => (false, 14.5, Weight::NORMAL),
            FontStyle::UiBold => (false, 14.5, Weight::SEMIBOLD),
            FontStyle::UiSmall => (false, 13.5, Weight::NORMAL),
            FontStyle::CodeMono => (true, 15.0, Weight::NORMAL),
            FontStyle::CodeMonoBold => (true, 15.0, Weight::BOLD),
            FontStyle::ChatBody => (false, 14.0, Weight::NORMAL),
            FontStyle::ChatBodyBold => (false, 14.0, Weight::SEMIBOLD),
            FontStyle::ChatMeta => (false, 12.2, Weight::NORMAL),
            FontStyle::ChatMetaBold => (false, 12.2, Weight::SEMIBOLD),
            FontStyle::ChatCode => (true, 12.6, Weight::NORMAL),
            FontStyle::ChatCodeBold => (true, 12.6, Weight::SEMIBOLD),
            FontStyle::BadgeMicro => (false, 10.0, Weight::BOLD),
        }
    }

    fn get_or_shape_line(&self, text: &str, style: FontStyle) -> Rc<ShapedLine> {
        let scale = {
            let st = self.state.borrow();
            if let Some(by_text) = st.shaped_cache.get(&style) {
                if let Some(cached) = by_text.get(text) {
                    return Rc::clone(cached);
                }
            }
            st.scale_factor
        };

        let (is_mono, base_size, weight) = Self::style_spec(style);
        let phys_font_size = (base_size * scale).max(9.0);
        let phys_line_height = (phys_font_size * 1.32).ceil();
        let metrics = Metrics::new(phys_font_size, phys_line_height);

        let mut fonts = self.fonts.borrow_mut();
        let family = if is_mono {
            Family::Monospace
        } else {
            Family::SansSerif
        };
        let attrs = Attrs::new().family(family).weight(weight);

        let mut buffer = Buffer::new(&mut fonts.font_system, metrics);
        {
            let mut borrowed = buffer.borrow_with(&mut fonts.font_system);
            borrowed.set_wrap(Wrap::None);
            borrowed.set_size(None, None);
            borrowed.set_text(text, &attrs, Shaping::Advanced);
            borrowed.shape_until_scroll(false);
        }

        let mut phys_w = 0.0f32;
        let mut phys_h = phys_line_height;
        let mut glyphs = Vec::new();
        if let Some(run) = buffer.layout_runs().next() {
            phys_w = if run.line_w.is_finite() { run.line_w } else { 0.0 };
            phys_h = if run.line_height.is_finite() {
                run.line_height.max(1.0)
            } else {
                phys_line_height
            };
            let safe_line_y = if run.line_y.is_finite() { run.line_y } else { phys_font_size };
            glyphs.reserve(run.glyphs.len());
            for g in run.glyphs.iter() {
                if !g.x.is_finite() || !g.w.is_finite() || g.x.abs() > 1_000_000.0 || g.w.abs() > 1_000_000.0 {
                    continue;
                }
                glyphs.push(CachedGlyph {
                    start: g.start,
                    end: g.end,
                    x: g.x,
                    w: g.w,
                    is_rtl: g.level.is_rtl(),
                    physical: g.physical((0.0, safe_line_y), 1.0),
                });
            }
        }

        // Fallback metrics if a headless environment has zero installed system fonts.
        if glyphs.is_empty() && !text.is_empty() && phys_w <= 0.0 {
            let char_count = text.chars().count() as f32;
            let avg_ratio = if is_mono { 0.60 } else { 0.54 };
            phys_w = char_count * phys_font_size * avg_ratio;
        }

        let log_mw = if text.is_empty() {
            0
        } else {
            ((phys_w / scale).round() as i32).max(1)
        };
        let log_th = ((phys_h / scale).round() as i32).max(1);

        let shaped = Rc::new(ShapedLine {
            log_mw,
            log_th,
            glyphs,
        });

        if text.len() <= MAX_CACHED_TEXT_BYTES {
            let mut st = self.state.borrow_mut();
            let total_cached: usize = st.shaped_cache.values().map(|m| m.len()).sum();
            if total_cached >= SHAPED_CACHE_CAP {
                st.shaped_cache.clear();
            }
            st.shaped_cache
                .entry(style)
                .or_default()
                .insert(text.to_string(), Rc::clone(&shaped));
        }
        shaped
    }

    pub fn measure_text(&self, text: &str, style: FontStyle) -> (i32, i32) {
        if text.is_empty() {
            return (0, 18);
        }
        {
            let st = self.state.borrow();
            if let Some(by_text) = st.measure_cache.get(&style) {
                if let Some(&cached) = by_text.get(text) {
                    return cached;
                }
            }
        }
        let shaped = self.get_or_shape_line(text, style);
        let res = (shaped.log_mw, shaped.log_th);
        let mut st = self.state.borrow_mut();
        let total_cached: usize = st.measure_cache.values().map(|m| m.len()).sum();
        if total_cached >= MEASURE_CACHE_CAP {
            st.measure_cache.clear();
        }
        st.measure_cache
            .entry(style)
            .or_default()
            .insert(text.to_string(), res);
        res
    }

    pub fn hit_test_col(&self, line: &str, log_rel_x: i32, style: FontStyle) -> usize {
        if log_rel_x <= 0 || line.is_empty() {
            return 0;
        }
        let shaped = self.get_or_shape_line(line, style);
        let scale = self.state.borrow().scale_factor;
        let phys_x = (log_rel_x as f32) * scale;

        if shaped.glyphs.is_empty() {
            let char_count = line.chars().count();
            if char_count == 0 || shaped.log_mw <= 0 {
                return 0;
            }
            let approx = ((log_rel_x as f32 / shaped.log_mw as f32) * char_count as f32).round();
            return (approx as usize).min(char_count);
        }

        let mut byte_pos = line.len();
        let mut matched = false;
        for (idx, g) in shaped.glyphs.iter().enumerate() {
            if idx == 0 && ((!g.is_rtl && phys_x < g.x) || (g.is_rtl && phys_x > g.x + g.w)) {
                byte_pos = g.start;
                matched = true;
                break;
            }
            if phys_x >= g.x && phys_x <= g.x + g.w {
                let right_half = phys_x >= g.x + g.w * 0.5;
                byte_pos = if right_half != g.is_rtl {
                    g.end
                } else {
                    g.start
                };
                matched = true;
                break;
            }
        }
        if !matched {
            byte_pos = shaped
                .glyphs
                .last()
                .map(|g| if g.is_rtl { g.start } else { g.end })
                .unwrap_or(line.len());
        }

        let clamped = byte_pos.min(line.len());
        let mut valid = clamped;
        while valid > 0 && !line.is_char_boundary(valid) {
            valid -= 1;
        }
        line[..valid].chars().count()
    }

    pub fn draw_text(
        &self,
        pixmap: &mut Pixmap,
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
        let scale = self.state.borrow().scale_factor;
        let log_ph = ((pixmap.height() as f32) / scale).ceil() as i32;
        let clip = Rect::new(0, 0, (x + max_width).max(0), log_ph);
        let mw = self.draw_text_in_clip(pixmap, x, y, text, color, clip, style);
        x + mw.min(max_width)
    }

    pub fn draw_text_clipped(
        &self,
        pixmap: &mut Pixmap,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        clip_rect: Rect,
        style: FontStyle,
    ) -> i32 {
        if text.is_empty() || clip_rect.w <= 0 || clip_rect.h <= 0 {
            return x;
        }
        let mw = self.draw_text_in_clip(pixmap, x, y, text, color, clip_rect, style);
        x + mw
    }

    fn draw_text_in_clip(
        &self,
        pixmap: &mut Pixmap,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        clip_rect: Rect,
        style: FontStyle,
    ) -> i32 {
        let shaped = self.get_or_shape_line(text, style);
        let log_mw = shaped.log_mw;
        if shaped.glyphs.is_empty() {
            return log_mw;
        }

        let scale = self.state.borrow().scale_factor;
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;

        let phys_x = (x as f32 * scale).round() as i32;
        let phys_y = (y as f32 * scale).round() as i32;

        let clip_x0 = ((clip_rect.x as f32 * scale).floor() as i32).max(0);
        let clip_x1 = (((clip_rect.x + clip_rect.w) as f32 * scale).ceil() as i32).min(pw);
        let clip_y0 = ((clip_rect.y as f32 * scale).floor() as i32).max(0);
        let clip_y1 = (((clip_rect.y + clip_rect.h) as f32 * scale).ceil() as i32).min(ph);
        if clip_x1 <= clip_x0 || clip_y1 <= clip_y0 {
            return log_mw;
        }

        let fg_r = (color >> 16) & 0xFF;
        let fg_g = (color >> 8) & 0xFF;
        let fg_b = color & 0xFF;
        let pw_usize = pw as usize;
        let px = pixmap.pixels_mut();

        let mut fonts = self.fonts.borrow_mut();
        let SharedFontContext {
            font_system,
            swash_cache,
            ..
        } = &mut *fonts;

        for g in &shaped.glyphs {
            let gw_ceil = g.w.ceil().clamp(0.0, 1_000_000.0) as i32;
            let approx_gx0 = phys_x.saturating_add(g.physical.x).saturating_sub(4);
            let approx_gx1 = phys_x.saturating_add(g.physical.x).saturating_add(gw_ceil).saturating_add(8);
            if approx_gx1 <= clip_x0 || approx_gx0 >= clip_x1 {
                continue;
            }
            let Some(img) = swash_cache.get_image(font_system, g.physical.cache_key).as_ref()
            else {
                continue;
            };
            let gw = img.placement.width as i32;
            let gh = img.placement.height as i32;
            if gw <= 0 || gh <= 0 {
                continue;
            }

            let dst_x0 = phys_x + g.physical.x + img.placement.left;
            let dst_y0 = phys_y + g.physical.y - img.placement.top;
            let dst_x1 = dst_x0 + gw;
            let dst_y1 = dst_y0 + gh;

            let vx0 = dst_x0.max(clip_x0);
            let vx1 = dst_x1.min(clip_x1);
            let vy0 = dst_y0.max(clip_y0);
            let vy1 = dst_y1.min(clip_y1);
            if vx1 <= vx0 || vy1 <= vy0 {
                continue;
            }

            let gw_usize = gw as usize;
            let src_c0 = (vx0 - dst_x0) as usize;
            let cw = (vx1 - vx0) as usize;

            match img.content {
                SwashContent::Mask => {
                    for vy in vy0..vy1 {
                        let src_r = (vy - dst_y0) as usize;
                        let s_start = src_r * gw_usize + src_c0;
                        let mask_row = &img.data[s_start..s_start + cw];
                        let d_start = (vy as usize) * pw_usize + (vx0 as usize);
                        let dst_row = &mut px[d_start..d_start + cw];
                        for (dst, &cov) in dst_row.iter_mut().zip(mask_row.iter()) {
                            if cov == 0 {
                                continue;
                            }
                            let a = cov as u32;
                            let inv = 255 - a;
                            let nr = ((dst.red() as u32 * inv + fg_r * a + 127) / 255) as u8;
                            let ng = ((dst.green() as u32 * inv + fg_g * a + 127) / 255) as u8;
                            let nb = ((dst.blue() as u32 * inv + fg_b * a + 127) / 255) as u8;
                            if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                                *dst = p;
                            }
                        }
                    }
                }
                SwashContent::Color => {
                    for vy in vy0..vy1 {
                        let src_r = (vy - dst_y0) as usize;
                        let s_start = (src_r * gw_usize + src_c0) * 4;
                        let color_row = &img.data[s_start..s_start + cw * 4];
                        let d_start = (vy as usize) * pw_usize + (vx0 as usize);
                        let dst_row = &mut px[d_start..d_start + cw];
                        for (dst, rgba) in dst_row.iter_mut().zip(color_row.chunks_exact(4)) {
                            let a = rgba[3] as u32;
                            if a == 0 {
                                continue;
                            }
                            let inv = 255 - a;
                            let nr = ((dst.red() as u32 * inv + rgba[0] as u32 * a + 127) / 255)
                                as u8;
                            let ng = ((dst.green() as u32 * inv + rgba[1] as u32 * a + 127) / 255)
                                as u8;
                            let nb = ((dst.blue() as u32 * inv + rgba[2] as u32 * a + 127) / 255)
                                as u8;
                            if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                                *dst = p;
                            }
                        }
                    }
                }
                SwashContent::SubpixelMask => {
                    for vy in vy0..vy1 {
                        let src_r = (vy - dst_y0) as usize;
                        let s_start = (src_r * gw_usize + src_c0) * 3;
                        let sub_row = &img.data[s_start..s_start + cw * 3];
                        let d_start = (vy as usize) * pw_usize + (vx0 as usize);
                        let dst_row = &mut px[d_start..d_start + cw];
                        for (dst, rgb_cov) in dst_row.iter_mut().zip(sub_row.chunks_exact(3)) {
                            let ar = rgb_cov[0] as u32;
                            let ag = rgb_cov[1] as u32;
                            let ab = rgb_cov[2] as u32;
                            if ar == 0 && ag == 0 && ab == 0 {
                                continue;
                            }
                            let nr =
                                ((dst.red() as u32 * (255 - ar) + fg_r * ar + 127) / 255) as u8;
                            let ng =
                                ((dst.green() as u32 * (255 - ag) + fg_g * ag + 127) / 255) as u8;
                            let nb =
                                ((dst.blue() as u32 * (255 - ab) + fg_b * ab + 127) / 255) as u8;
                            if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                                *dst = p;
                            }
                        }
                    }
                }
            }
        }

        log_mw
    }
}
