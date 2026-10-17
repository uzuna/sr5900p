//! Declarative label templates.
//!
//! A template is a TOML file that lays out columns from left to right.
//! Each column fills the printable height of the tape.
//!
//! ```toml
//! gap_mm = 1.5
//!
//! [fonts]
//! bold = { path = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc" }
//!
//! [[columns]]
//! type = "text"
//! lines = [
//!   { text = "{dept}", font = "bold", size = 0.4 },
//!   { text = "{shelf_id}", font = "bold", size = 0.55 },
//! ]
//!
//! [[columns]]
//! type = "qr"
//! data = "{shelf_id}"
//! ```
//!
//! `{name}` in `text` / `data` is replaced with the field of the same name.
//! `size` is a fraction of the printable height.
use crate::display::TapeDisplay;
use crate::Tape;
use ab_glyph::point;
use ab_glyph::Font;
use ab_glyph::FontVec;
use ab_glyph::GlyphId;
use ab_glyph::PxScale;
use ab_glyph::ScaleFont;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use image::Luma;
use qrcode::QrCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

#[derive(Deserialize, Debug)]
pub struct Template {
    #[serde(default = "default_gap_mm")]
    pub gap_mm: f32,
    #[serde(default)]
    pub fonts: HashMap<String, FontSpec>,
    pub columns: Vec<Column>,
    /// directory that relative paths (fonts, images) are resolved against
    #[serde(skip)]
    pub base_dir: PathBuf,
}

fn default_gap_mm() -> f32 {
    1.5
}

fn default_size() -> f32 {
    1.0
}

#[derive(Deserialize, Debug)]
pub struct FontSpec {
    pub path: String,
    /// face index in a font collection (.ttc)
    #[serde(default)]
    pub index: u32,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Column {
    Text {
        lines: Vec<Line>,
    },
    Qr {
        data: String,
        #[serde(default = "default_size")]
        size: f32,
    },
    Image {
        path: String,
        #[serde(default = "default_size")]
        size: f32,
    },
}

#[derive(Deserialize, Debug)]
pub struct Line {
    pub text: String,
    pub font: String,
    pub size: f32,
}

impl Template {
    pub fn from_toml_str(s: &str, base_dir: impl Into<PathBuf>) -> Result<Self> {
        let mut t: Template = toml::from_str(s).context("Failed to parse template")?;
        t.base_dir = base_dir.into();
        Ok(t)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read template {}", path.display()))?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml_str(&s, base_dir)
    }

    /// Render the label for the given tape. The result is `tape.width_px()` px high.
    pub fn render(&self, fields: &HashMap<String, String>, tape: Tape) -> Result<TapeDisplay> {
        let h = tape.width_px() as usize;
        let mut fonts = HashMap::new();
        for (name, spec) in &self.fonts {
            fonts.insert(name.as_str(), load_font(&self.resolve(&spec.path), spec.index)?);
        }
        let mut parts = Vec::new();
        for c in &self.columns {
            parts.push(match c {
                Column::Text { lines } => {
                    let mut rendered = Vec::new();
                    for l in lines {
                        let font = fonts
                            .get(l.font.as_str())
                            .ok_or_else(|| anyhow!("Font '{}' is not defined in [fonts]", l.font))?;
                        let text = expand(&l.text, fields)?;
                        rendered.push(render_text(font, &text, frac_px(h, l.size)));
                    }
                    stack_vertical(&rendered, h)
                }
                Column::Qr { data, size } => {
                    let qr = render_qr(&expand(data, fields)?, frac_px(h, *size))?;
                    center_vertical(&qr, h)
                }
                Column::Image { path, size } => {
                    let img = render_image(&self.resolve(path), frac_px(h, *size))?;
                    center_vertical(&img, h)
                }
            });
        }
        let gap = crate::print::mm_to_px(self.gap_mm) as usize;
        let width = parts.iter().map(|p| p.width).sum::<usize>() + gap * parts.len().saturating_sub(1);
        let mut td = TapeDisplay::new(width, h);
        let mut x = 0;
        for p in &parts {
            td.overlay_or(p, x, 0);
            x += p.width + gap;
        }
        Ok(td)
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir.join(p)
        }
    }
}

fn frac_px(h: usize, frac: f32) -> usize {
    (h as f32 * frac).floor() as usize
}

/// Replace `{name}` with fields[name]. An undefined name is an error.
fn expand(template: &str, fields: &HashMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find('}')
            .ok_or_else(|| anyhow!("Unclosed '{{' in {template:?}"))?
            + start;
        let name = &rest[start + 1..end];
        let v = fields
            .get(name)
            .ok_or_else(|| anyhow!("Field '{name}' is required by the template"))?;
        out.push_str(v);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn stack_vertical(parts: &[TapeDisplay], h: usize) -> TapeDisplay {
    let w = parts.iter().map(|p| p.width).max().unwrap_or(0);
    let mut td = TapeDisplay::new(w, h);
    let mut y = 0;
    for p in parts {
        td.overlay_or(p, 0, y);
        y += p.height;
    }
    td
}

fn center_vertical(part: &TapeDisplay, h: usize) -> TapeDisplay {
    let mut td = TapeDisplay::new(part.width, h);
    td.overlay_or(part, 0, h.saturating_sub(part.height) / 2);
    td
}

fn load_font(path: &Path, index: u32) -> Result<FontVec> {
    let data = std::fs::read(path).with_context(|| format!("Failed to read font {}", path.display()))?;
    FontVec::try_from_vec_and_index(data, index)
        .map_err(|e| anyhow!("Failed to load font {}: {e}", path.display()))
}

/// Render a single line of text whose line height (ascent - descent) is line_px.
fn render_text(font: &FontVec, text: &str, line_px: usize) -> TapeDisplay {
    // ab_glyph scales so that ascent - descent == PxScale
    let scale = PxScale::from(line_px as f32);
    let sf = font.as_scaled(scale);
    let mut glyphs = Vec::new();
    let mut caret = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for c in text.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(scale, point(caret, sf.ascent())));
        caret += sf.h_advance(id);
        prev = Some(id);
    }
    let mut td = TapeDisplay::new(caret.ceil() as usize, line_px);
    for g in glyphs {
        if let Some(og) = font.outline_glyph(g) {
            let b = og.px_bounds();
            og.draw(|x, y, cov| {
                let px = b.min.x as i32 + x as i32;
                let py = b.min.y as i32 + y as i32;
                if cov >= 0.5 && px >= 0 && py >= 0 {
                    td.set_pixel(px as usize, py as usize, true);
                }
            });
        }
    }
    td
}

fn render_qr(text: &str, size_px: usize) -> Result<TapeDisplay> {
    let mut td = TapeDisplay::new(size_px, size_px);
    let size_px = size_px as u32;
    let code = QrCode::new(text).context("Failed to generate a QR code")?;
    let image = code
        .render::<Luma<u8>>()
        .quiet_zone(false)
        .max_dimensions(size_px, size_px)
        .build();
    let ofs_x = (size_px - image.width()) / 2;
    let ofs_y = (size_px - image.height()) / 2;
    for (x, y, p) in image.enumerate_pixels() {
        td.set_pixel((x + ofs_x) as usize, (y + ofs_y) as usize, p.0[0] == 0);
    }
    Ok(td)
}

/// Scale the image to height_px (keeping aspect) and binarize it.
/// Dark opaque pixels are printed; transparent pixels are left blank.
fn render_image(path: &Path, height_px: usize) -> Result<TapeDisplay> {
    let img = image::open(path)
        .with_context(|| format!("Failed to open image {}", path.display()))?
        .to_rgba8();
    let width_px = (img.width() as usize * height_px + img.height() as usize / 2) / img.height() as usize;
    let img = image::imageops::resize(
        &img,
        width_px as u32,
        height_px as u32,
        image::imageops::FilterType::Triangle,
    );
    let mut td = TapeDisplay::new(width_px, height_px);
    for (x, y, p) in img.enumerate_pixels() {
        let [r, g, b, a] = p.0;
        let luma = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
        td.set_pixel(x as usize, y as usize, a >= 128 && luma < 128);
    }
    Ok(td)
}
