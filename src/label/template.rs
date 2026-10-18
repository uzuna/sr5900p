//! Declarative label templates.
//!
//! A template is a TOML file that lays out columns from left to right.
//! Each column fills the printable height of the tape.
//!
//! ```toml
//! gap_mm = 1.5
//! max_length_mm = 40
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
//! Column types: `text`, `qr`, `datamatrix` (`shape = "square"` or `"rectangle"`) and `image`.
//! `code128` draws a 1D barcode; `module_mm` sets the narrowest bar (default 0.3 mm).
//! `logo` draws a logo chosen per label, an image or a text stamp (see [`crate::label::logo`]).
//! `{name}` in `text` / `data` and in a logo's `default = { text = ... }` is replaced with the
//! field of the same name.
//! `size` is a fraction of the printable height.
//! When the rendered label is longer than `max_length_mm`, rendering fails
//! instead of printing a label that does not fit.
use crate::label::display::TapeDisplay;
use crate::label::error::CodeKind;
use crate::label::error::LabelError;
use crate::label::logo::load_picture;
use crate::label::logo::render_picture;
use crate::label::logo::render_stamp;
use crate::label::logo::Binarize;
use crate::label::logo::DefaultLogo;
use crate::label::logo::Logo;
use crate::label::logo::LogoKind;
use crate::label::logo::Picture;
use crate::label::logo::StampStyle;
use crate::label::px_to_mm;
use crate::label::Label;
use crate::label::LabelData;
use crate::label::LogoChoice;
use crate::Tape;
use ab_glyph::point;
use ab_glyph::Font;
use ab_glyph::FontVec;
use ab_glyph::GlyphId;
use ab_glyph::PxScale;
use ab_glyph::ScaleFont;
use barcoders::sym::code128::Code128;
use datamatrix::DataMatrix;
use datamatrix::SymbolList;
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
    /// upper limit of the label length (the printer's own margins are not included)
    #[serde(default)]
    pub max_length_mm: Option<f32>,
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
    /// An image or a text stamp chosen per label; see [`crate::label::logo`].
    Logo {
        /// name under which a label supplies the logo (`LabelData::logo`)
        #[serde(default)]
        slot: Option<String>,
        /// used when the label supplies none
        #[serde(default)]
        default: Option<DefaultLogo>,
        #[serde(default = "default_size")]
        size: f32,
        #[serde(default)]
        binarize: Binarize,
        /// swap black and white of image logos
        #[serde(default)]
        invert: bool,
        /// how text logos are drawn; required when a text logo can reach this column
        #[serde(default)]
        stamp: Option<StampStyle>,
    },
    Datamatrix {
        data: String,
        #[serde(default = "default_size")]
        size: f32,
        #[serde(default)]
        shape: DatamatrixShape,
    },
    Code128 {
        data: String,
        #[serde(default = "default_size")]
        size: f32,
        /// width of the narrowest bar
        #[serde(default = "default_module_mm")]
        module_mm: f32,
    },
}

fn default_module_mm() -> f32 {
    0.3
}

#[derive(Deserialize, Debug, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DatamatrixShape {
    #[default]
    Square,
    /// wider than high; fits narrow tapes with less height
    Rectangle,
}

#[derive(Deserialize, Debug)]
pub struct Line {
    pub text: String,
    pub font: String,
    pub size: f32,
}

impl Template {
    pub fn from_toml_str(s: &str, base_dir: impl Into<PathBuf>) -> Result<Self, LabelError> {
        let mut t: Template =
            toml::from_str(s).map_err(|e| LabelError::InvalidTemplate(e.to_string()))?;
        t.base_dir = base_dir.into();
        Ok(t)
    }

    /// Names of the logo columns' slots, i.e. what `LabelData::logo` can fill.
    pub fn logo_slots(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().filter_map(|c| match c {
            Column::Logo { slot, .. } => slot.as_deref(),
            _ => None,
        })
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, LabelError> {
        let path = path.as_ref();
        let s = std::fs::read_to_string(path).map_err(|source| LabelError::Io {
            what: "template",
            path: path.to_path_buf(),
            source,
        })?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml_str(&s, base_dir)
    }

    /// Render one label with fonts that are already loaded.
    /// The result is `tape.width_px()` px high.
    fn render_with(
        &self,
        loaded: &Loaded,
        data: &LabelData,
        tape: Tape,
    ) -> Result<TapeDisplay, LabelError> {
        let fonts = &loaded.fonts;
        let fields = &data.texts;
        let h = tape.width_px() as usize;
        let mut parts = Vec::new();
        for c in &self.columns {
            parts.push(match c {
                Column::Text { lines } => {
                    let mut rendered = Vec::new();
                    for l in lines {
                        let font = fonts
                            .get(l.font.as_str())
                            .ok_or_else(|| LabelError::UnknownFont(l.font.clone()))?;
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
                    let img = &loaded.images[&self.resolve(path)];
                    let td = render_picture(img, frac_px(h, *size), Binarize::default(), false)?;
                    center_vertical(&td, h)
                }
                Column::Logo {
                    slot,
                    default,
                    size,
                    binarize,
                    invert,
                    stamp,
                } => {
                    let logo = self.choose_logo(loaded, data, slot.as_deref(), default.as_ref())?;
                    let px = frac_px(h, *size);
                    let td = match logo.kind() {
                        LogoKind::Picture(picture) => {
                            render_picture(&picture, px, *binarize, *invert)?
                        }
                        LogoKind::Stamp(text) => {
                            let style = stamp.as_ref().ok_or_else(|| {
                                LabelError::InvalidTemplate(
                                    "a text logo needs `stamp` on its logo column".to_string(),
                                )
                            })?;
                            render_stamp(&fonts[&style.font], text, px, style)?
                        }
                    };
                    center_vertical(&td, h)
                }
                Column::Code128 {
                    data,
                    size,
                    module_mm,
                } => {
                    let module = crate::label::mm_to_px(*module_mm).max(1) as usize;
                    let bc = render_code128(&expand(data, fields)?, frac_px(h, *size), module)?;
                    center_vertical(&bc, h)
                }
                Column::Datamatrix { data, size, shape } => {
                    let dm = render_datamatrix(&expand(data, fields)?, frac_px(h, *size), *shape)?;
                    center_vertical(&dm, h)
                }
            });
        }
        let gap = crate::label::mm_to_px(self.gap_mm) as usize;
        let width =
            parts.iter().map(|p| p.width).sum::<usize>() + gap * parts.len().saturating_sub(1);
        if let Some(max_mm) = self.max_length_mm {
            if width > crate::label::mm_to_px(max_mm) as usize {
                return Err(LabelError::TooLong {
                    length_mm: px_to_mm(width),
                    max_mm,
                });
            }
        }
        let mut td = TapeDisplay::new(width, h);
        let mut x = 0;
        for p in &parts {
            td.overlay_or(p, x, 0);
            x += p.width + gap;
        }
        Ok(td)
    }

    fn choose_logo(
        &self,
        loaded: &Loaded,
        data: &LabelData,
        slot: Option<&str>,
        default: Option<&DefaultLogo>,
    ) -> Result<Logo, LabelError> {
        let registered = |key: &str| {
            loaded
                .logos
                .get(key)
                .cloned()
                .ok_or_else(|| LabelError::UnknownLogo(key.to_string()))
        };
        match slot.and_then(|s| data.logos.get(s)) {
            Some(LogoChoice::Inline(logo)) => Ok(logo.clone()),
            Some(LogoChoice::Key(key)) => registered(key),
            None => match default {
                Some(DefaultLogo::Text { text }) => Ok(Logo::Stamp(expand(text, &data.texts)?)),
                Some(DefaultLogo::Path { path }) => {
                    Ok(loaded.images[&self.resolve(path)].clone().into())
                }
                Some(DefaultLogo::Key { key }) => registered(key),
                None => Err(LabelError::MissingLogo(
                    slot.unwrap_or_default().to_string(),
                )),
            },
        }
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

/// Renders labels from one template. Fonts are loaded once when the renderer is created.
pub struct Renderer {
    template: Template,
    loaded: Loaded,
}

/// Everything a render reads besides the template, loaded before the first render.
struct Loaded {
    fonts: HashMap<String, FontVec>,
    /// image files (raster or SVG) named by the template, by resolved path
    images: HashMap<PathBuf, Picture>,
    /// logos registered by the application
    logos: HashMap<String, Logo>,
}

impl Renderer {
    pub fn new(template: Template) -> Result<Self, LabelError> {
        let mut fonts = HashMap::new();
        for (name, spec) in &template.fonts {
            fonts.insert(
                name.clone(),
                load_font(&template.resolve(&spec.path), spec.index)?,
            );
        }
        let mut images = HashMap::new();
        // reject a misspelled font name or a logo column that can never draw now,
        // rather than on the first render
        for c in &template.columns {
            match c {
                Column::Text { lines } => {
                    if let Some(l) = lines.iter().find(|l| !fonts.contains_key(&l.font)) {
                        return Err(LabelError::UnknownFont(l.font.clone()));
                    }
                }
                Column::Image { path, .. } => {
                    let path = template.resolve(path);
                    let img = load_picture(&path)?;
                    images.insert(path, img);
                }
                Column::Logo {
                    slot,
                    default,
                    stamp,
                    ..
                } => {
                    if slot.is_none() && default.is_none() {
                        return Err(LabelError::InvalidTemplate(
                            "a logo column needs `slot` or `default`".to_string(),
                        ));
                    }
                    if let Some(style) = stamp {
                        if !fonts.contains_key(&style.font) {
                            return Err(LabelError::UnknownFont(style.font.clone()));
                        }
                    }
                    match default {
                        Some(DefaultLogo::Text { .. }) if stamp.is_none() => {
                            return Err(LabelError::InvalidTemplate(
                                "a text default logo needs `stamp` on its logo column".to_string(),
                            ));
                        }
                        Some(DefaultLogo::Path { path }) => {
                            let path = template.resolve(path);
                            let img = load_picture(&path)?;
                            images.insert(path, img);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(Self {
            template,
            loaded: Loaded {
                fonts,
                images,
                logos: HashMap::new(),
            },
        })
    }

    /// Make a logo available to labels and to `default = { key = ... }` under this key.
    pub fn register_logo(&mut self, key: impl Into<String>, logo: Logo) {
        self.loaded.logos.insert(key.into(), logo);
    }

    pub fn template(&self) -> &Template {
        &self.template
    }

    pub fn render(&self, data: &LabelData, tape: Tape) -> Result<Label, LabelError> {
        let bitmap = self.template.render_with(&self.loaded, data, tape)?;
        Label::new(bitmap, tape)
    }
}

fn frac_px(h: usize, frac: f32) -> usize {
    (h as f32 * frac).floor() as usize
}

/// Replace `{name}` with fields[name]. An undefined name is an error.
fn expand(template: &str, fields: &HashMap<String, String>) -> Result<String, LabelError> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find('}')
            .ok_or_else(|| LabelError::UnclosedPlaceholder(template.to_string()))?
            + start;
        let name = &rest[start + 1..end];
        let v = fields
            .get(name)
            .ok_or_else(|| LabelError::MissingField(name.to_string()))?;
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

fn load_font(path: &Path, index: u32) -> Result<FontVec, LabelError> {
    let data = std::fs::read(path).map_err(|source| LabelError::Io {
        what: "font",
        path: path.to_path_buf(),
        source,
    })?;
    FontVec::try_from_vec_and_index(data, index).map_err(|e| LabelError::Font {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

/// Lay out one line of text whose line height (ascent - descent) is line_px.
/// The bitmap covers both the line box (advances x line height) and every glyph outline,
/// so overhanging glyphs are kept. Also returns where the line box is in it:
/// (left, top, width).
fn rasterize_line(
    font: &FontVec,
    text: &str,
    line_px: usize,
) -> (TapeDisplay, usize, usize, usize) {
    // ab_glyph scales so that ascent - descent == PxScale
    let scale = PxScale::from(line_px as f32);
    let sf = font.as_scaled(scale);
    let mut outlines = Vec::new();
    let mut caret = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for c in text.chars() {
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        let g = id.with_scale_and_position(scale, point(caret, sf.ascent()));
        if let Some(og) = font.outline_glyph(g) {
            outlines.push(og);
        }
        caret += sf.h_advance(id);
        prev = Some(id);
    }
    let (mut min_x, mut min_y) = (0i32, 0i32);
    let (mut max_x, mut max_y) = (caret.ceil() as i32, line_px as i32);
    for og in &outlines {
        let b = og.px_bounds();
        min_x = min_x.min(b.min.x.floor() as i32);
        min_y = min_y.min(b.min.y.floor() as i32);
        max_x = max_x.max(b.max.x.ceil() as i32);
        max_y = max_y.max(b.max.y.ceil() as i32);
    }
    let mut td = TapeDisplay::new((max_x - min_x) as usize, (max_y - min_y) as usize);
    for og in &outlines {
        let b = og.px_bounds();
        og.draw(|x, y, cov| {
            if cov >= 0.5 {
                let px = b.min.x as i32 + x as i32 - min_x;
                let py = b.min.y as i32 + y as i32 - min_y;
                td.set_pixel(px as usize, py as usize, true);
            }
        });
    }
    let box_width = caret.ceil() as usize;
    (td, (-min_x) as usize, (-min_y) as usize, box_width)
}

/// One line of a text column, cut to its line box (advances x line height): every line
/// keeps its origin at x = 0, so the lines of a column stay left-aligned even when a
/// glyph such as `j` overhangs its advance.
pub(crate) fn render_text(font: &FontVec, text: &str, line_px: usize) -> TapeDisplay {
    let (full, left, top, width) = rasterize_line(font, text, line_px);
    let mut td = TapeDisplay::new(width, line_px);
    for y in 0..line_px {
        for x in 0..width {
            td.set_pixel(x, y, full.get_pixel(left + x, top + y));
        }
    }
    td
}

/// All the ink of one line of text, including parts above or below the line box.
pub(crate) fn render_ink(font: &FontVec, text: &str, line_px: usize) -> TapeDisplay {
    rasterize_line(font, text, line_px).0
}

fn render_qr(text: &str, size_px: usize) -> Result<TapeDisplay, LabelError> {
    let mut td = TapeDisplay::new(size_px, size_px);
    let size_px = size_px as u32;
    let code = QrCode::new(text).map_err(|e| LabelError::Encode {
        kind: CodeKind::Qr,
        reason: e.to_string(),
    })?;
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

/// Render a Code128 barcode (character set B) with 10-module quiet zones on both sides.
fn render_code128(
    text: &str,
    height_px: usize,
    module_px: usize,
) -> Result<TapeDisplay, LabelError> {
    const QUIET_ZONE: usize = 10;
    let bars = Code128::new(format!("\u{0181}{text}"))
        .map_err(|e| LabelError::Encode {
            kind: CodeKind::Code128,
            reason: format!("{text:?}: {e}"),
        })?
        .encode();
    let mut td = TapeDisplay::new((bars.len() + 2 * QUIET_ZONE) * module_px, height_px);
    for (i, bar) in bars.iter().enumerate() {
        if *bar == 1 {
            for dx in 0..module_px {
                for y in 0..height_px {
                    td.set_pixel((QUIET_ZONE + i) * module_px + dx, y, true);
                }
            }
        }
    }
    Ok(td)
}

/// Render a Data Matrix whose height is at most height_px, with whole-pixel modules.
fn render_datamatrix(
    text: &str,
    height_px: usize,
    shape: DatamatrixShape,
) -> Result<TapeDisplay, LabelError> {
    let symbols = match shape {
        DatamatrixShape::Square => SymbolList::default().enforce_square(),
        DatamatrixShape::Rectangle => SymbolList::default().enforce_rectangular(),
    };
    let bitmap = DataMatrix::encode_str(text, symbols)
        .map_err(|e| LabelError::Encode {
            kind: CodeKind::DataMatrix,
            reason: format!("{e:?}"),
        })?
        .bitmap();
    let module = height_px / bitmap.height();
    if module == 0 {
        return Err(LabelError::Encode {
            kind: CodeKind::DataMatrix,
            reason: format!(
                "needs {} px but only {height_px} px is available",
                bitmap.height()
            ),
        });
    }
    let mut td = TapeDisplay::new(bitmap.width() * module, bitmap.height() * module);
    for (x, y) in bitmap.pixels() {
        for dy in 0..module {
            for dx in 0..module {
                td.set_pixel(x * module + dx, y * module + dy, true);
            }
        }
    }
    Ok(td)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOLD: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc";

    fn fields(kv: &[(&str, &str)]) -> HashMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    trait RenderExt {
        fn render(
            self,
            fields: &HashMap<String, String>,
            tape: Tape,
        ) -> Result<TapeDisplay, LabelError>;
    }

    impl RenderExt for Template {
        fn render(
            self,
            fields: &HashMap<String, String>,
            tape: Tape,
        ) -> Result<TapeDisplay, LabelError> {
            let data = LabelData {
                texts: fields.clone(),
                ..Default::default()
            };
            Ok(Renderer::new(self)?.render(&data, tape)?.bitmap().clone())
        }
    }

    fn template(body: &str) -> Template {
        // [fonts] goes last so that top-level keys in body stay top-level
        let s = format!("{body}\n[fonts]\nbold = {{ path = \"{BOLD}\" }}\n");
        Template::from_toml_str(&s, ".").unwrap()
    }

    #[test]
    fn expand_replaces_fields() {
        let f = fields(&[("a", "1"), ("b", "xy")]);
        assert_eq!(expand("{a}-{b}!", &f).unwrap(), "1-xy!");
        assert_eq!(expand("plain", &f).unwrap(), "plain");
    }

    #[test]
    fn expand_rejects_missing_field_and_unclosed_brace() {
        let f = fields(&[("a", "1")]);
        assert!(expand("{a}{b}", &f)
            .unwrap_err()
            .to_string()
            .contains("'b'"));
        assert!(expand("{a", &f).is_err());
    }

    #[test]
    fn render_fills_tape_height_and_lays_out_columns_with_gap() {
        let t = template(
            r#"
gap_mm = 2.0
[[columns]]
type = "text"
lines = [{ text = "{id}", font = "bold", size = 0.5 }]
[[columns]]
type = "qr"
data = "{id}"
"#,
        );
        let f = fields(&[("id", "SH-A03")]);
        let td = t.render(&f, Tape::W12).unwrap();
        let h = Tape::W12.width_px() as usize;
        assert_eq!(td.height, h);

        let text_only = template(
            r#"
[[columns]]
type = "text"
lines = [{ text = "{id}", font = "bold", size = 0.5 }]
"#,
        )
        .render(&f, Tape::W12)
        .unwrap();
        // text + gap + QR (a square of the full height)
        let gap = crate::label::mm_to_px(2.0) as usize;
        assert_eq!(td.width, text_only.width + gap + h);
        // the gap is blank and the QR column is placed right after it
        let qr = render_qr("SH-A03", h).unwrap();
        for y in 0..h {
            for x in 0..gap {
                assert!(!td.get_pixel(text_only.width + x, y), "gap at ({x}, {y})");
            }
            for x in 0..h {
                let got = td.get_pixel(text_only.width + gap + x, y);
                assert_eq!(got, qr.get_pixel(x, y), "QR at ({x}, {y})");
            }
        }
    }

    #[test]
    fn render_rejects_label_longer_than_max_length() {
        let body = r#"
[[columns]]
type = "text"
lines = [{ text = "{name}", font = "bold", size = 1.0 }]
"#;
        let f = fields(&[("name", "ABCDEFGHIJ")]);
        let len = template(body).render(&f, Tape::W12).unwrap().width;
        let len_mm = px_to_mm(len);

        let fits = template(&format!("max_length_mm = {}\n{body}", len_mm + 1.0));
        assert_eq!(fits.render(&f, Tape::W12).unwrap().width, len);

        let too_short = template(&format!("max_length_mm = {}\n{body}", len_mm - 1.0));
        let err = too_short
            .render(&f, Tape::W12)
            .expect_err("should be rejected")
            .to_string();
        assert!(err.contains("exceeds max_length_mm"), "{err}");
    }

    #[test]
    fn glyphs_that_overhang_the_line_box_are_kept_whole() {
        // ink outside the advance box or above/below the line used to be clipped
        let font = load_font(Path::new(BOLD), 0).unwrap();
        let line_px = 60;
        let scale = PxScale::from(line_px as f32);
        let mut overhanging = 0;
        for c in ['j', 'f', 'g', 'Å', 'ʃ', '/', '_', 'Ç', 'ÿ', '貸'] {
            let g = font
                .as_scaled(scale)
                .glyph_id(c)
                .with_scale_and_position(scale, point(0.0, font.as_scaled(scale).ascent()));
            let og = font.outline_glyph(g).unwrap();
            let mut expected = 0;
            og.draw(|_, _, cov| expected += (cov >= 0.5) as usize);
            let b = og.px_bounds();
            let advance = font.as_scaled(scale).h_advance(font.glyph_id(c));
            if b.min.x < 0.0 || b.min.y < 0.0 || b.max.x > advance || b.max.y > line_px as f32 {
                overhanging += 1;
            }
            let ink = |td: &TapeDisplay| td.framebuffer.iter().flatten().filter(|p| **p).count();
            assert_eq!(
                ink(&render_ink(&font, &c.to_string(), line_px)),
                expected,
                "{c}"
            );
            // a text column keeps only what lies inside the line box (advance x line height)
            let mut within_line = 0;
            og.draw(|x, y, cov| {
                let px = b.min.x as i32 + x as i32;
                let py = b.min.y as i32 + y as i32;
                within_line += (cov >= 0.5
                    && (0..advance.ceil() as i32).contains(&px)
                    && (0..line_px as i32).contains(&py)) as usize;
            });
            assert_eq!(
                ink(&render_text(&font, &c.to_string(), line_px)),
                within_line,
                "{c}"
            );
        }
        // the set must contain glyphs that actually overhang, or it proves nothing
        assert!(overhanging >= 3, "only {overhanging} glyphs overhang");
    }

    #[test]
    fn renderer_keeps_fonts_loaded_after_the_file_is_gone() {
        // the renderer must not read font files per render
        let dir = std::env::temp_dir().join(format!("sr5900p-font-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let font = dir.join("bold.ttc");
        std::fs::copy(BOLD, &font).unwrap();
        let t = Template::from_toml_str(
            &format!(
                r#"
[[columns]]
type = "text"
lines = [{{ text = "{{id}}", font = "bold", size = 0.5 }}]
[fonts]
bold = {{ path = "{}" }}
"#,
                font.display()
            ),
            ".",
        )
        .unwrap();
        let renderer = Renderer::new(t).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        for id in ["A-1", "B-22"] {
            let label = renderer
                .render(&LabelData::new().text("id", id), Tape::W12)
                .unwrap();
            assert_eq!(label.tape(), Tape::W12);
            assert!(label.length_mm() > 0.0);
        }
    }

    #[test]
    fn renderer_new_rejects_undefined_font() {
        // the misspelling must surface when the renderer is created, before any label is rendered
        let t = template(
            r#"
[[columns]]
type = "text"
lines = [{ text = "x", font = "nope", size = 0.5 }]
"#,
        );
        let err = Renderer::new(t).err().expect("should be rejected");
        assert!(
            matches!(&err, LabelError::UnknownFont(name) if name == "nope"),
            "{err:?}"
        );
    }

    #[test]
    fn render_rejects_undefined_font() {
        let t = template(
            r#"
[[columns]]
type = "text"
lines = [{ text = "x", font = "nope", size = 0.5 }]
"#,
        );
        let err = t
            .render(&fields(&[]), Tape::W12)
            .expect_err("should be rejected")
            .to_string();
        assert!(err.contains("'nope'"), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_column_type() {
        assert!(Template::from_toml_str("[[columns]]\ntype = \"barcode\"\n", ".").is_err());
    }

    #[test]
    fn render_datamatrix_uses_whole_pixel_modules_within_height() {
        let h = Tape::W12.width_px() as usize;
        let sq = render_datamatrix("SH-A03", h, DatamatrixShape::Square).unwrap();
        assert_eq!(sq.width, sq.height);
        assert!(sq.height <= h);
        // the largest whole-pixel module that fits the symbol into h
        let n = DataMatrix::encode_str("SH-A03", SymbolList::default().enforce_square())
            .unwrap()
            .bitmap()
            .height();
        assert_eq!(sq.height, h / n * n);
        // the L-shaped finder: left column and bottom row are all dark
        assert!((0..sq.height).all(|y| sq.get_pixel(0, y)));
        assert!((0..sq.width).all(|x| sq.get_pixel(x, sq.height - 1)));

        let rect = render_datamatrix("SH-A03", h, DatamatrixShape::Rectangle).unwrap();
        assert!(rect.width > rect.height);
        assert!(rect.height <= h);
    }

    #[test]
    fn render_datamatrix_rejects_too_small_height() {
        let err = render_datamatrix("SH-A03", 5, DatamatrixShape::Square)
            .expect_err("should be rejected");
        assert!(err.to_string().contains("only 5 px"), "{err}");
    }

    #[test]
    fn render_code128_has_quiet_zones_and_full_height_bars() {
        let module = 4;
        let bars = Code128::new("\u{0181}SH-A03").unwrap().encode();
        let td = render_code128("SH-A03", 100, module).unwrap();
        assert_eq!(td.width, (bars.len() + 20) * module);
        assert_eq!(td.height, 100);
        // 10-module quiet zones on both sides are blank
        for y in 0..td.height {
            for x in 0..10 * module {
                assert!(!td.get_pixel(x, y));
                assert!(!td.get_pixel(td.width - 1 - x, y));
            }
        }
        // each module column matches the encoded bar over the whole height
        for (i, bar) in bars.iter().enumerate() {
            let x = (10 + i) * module;
            assert!((0..td.height).all(|y| td.get_pixel(x, y) == (*bar == 1)));
        }
    }

    #[test]
    fn render_code128_rejects_non_ascii() {
        assert!(render_code128("棚", 100, 4).is_err());
    }

    #[test]
    fn render_image_binarizes_and_keeps_aspect() {
        // 4x2 image: left half black, right half white; transparent black stays blank
        let dir = std::env::temp_dir().join(format!("sr5900p-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("img.png");
        let mut img = image::RgbaImage::new(4, 2);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = match x {
                0 => image::Rgba([0, 0, 0, 255]),
                1 => image::Rgba([0, 0, 0, 0]),
                _ => image::Rgba([255, 255, 255, 255]),
            };
        }
        img.save(&path).unwrap();
        let td =
            render_picture(&load_picture(&path).unwrap(), 2, Binarize::default(), false).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((td.width, td.height), (4, 2));
        assert!(td.get_pixel(0, 0));
        assert!(!td.get_pixel(1, 0));
        assert!(!td.get_pixel(2, 0));
    }
}
