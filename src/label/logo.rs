//! Logos: an image (raster or SVG) or a text stamp placed in a `logo` column of a template.
//!
//! The application supplies a [`Logo`] per label, either by a key registered on the
//! [`Renderer`](super::Renderer) or inline through [`LabelData`](super::LabelData).
//! A template can name a default logo used when the label does not supply one.
use crate::label::display::TapeDisplay;
use crate::label::error::LabelError;
use crate::label::mm_to_px;
use ab_glyph::FontVec;
use image::RgbaImage;
use serde::Deserialize;
use std::sync::Arc;

/// A logo supplied by the application.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Logo {
    /// A raster image. It is scaled to the column height and binarized as the template says.
    Image(Arc<RgbaImage>),
    /// A vector image, drawn at the column height before it is binarized, so it stays sharp
    /// on every tape width.
    #[cfg(feature = "svg")]
    Svg(Arc<resvg::usvg::Tree>),
    /// Text drawn with the template's stamp style, e.g. a department name.
    Stamp(String),
}

impl Logo {
    /// Decode an image file (PNG, or any format the `image` crate is built with).
    pub fn from_image_bytes(bytes: &[u8]) -> Result<Self, LabelError> {
        let img = image::load_from_memory(bytes).map_err(|e| LabelError::Image {
            path: "<memory>".into(),
            reason: e.to_string(),
        })?;
        Ok(Self::from_image(img))
    }

    /// Parse an SVG. Text in the SVG is drawn with the system fonts, which are looked up
    /// here, so register SVG logos once rather than per label. Images that refer to files
    /// (`<image href="...">`) are not read; embedded `data:` images are. Use
    /// [`Logo::from_file`] for SVGs that refer to files next to them.
    #[cfg(feature = "svg")]
    pub fn from_svg(bytes: &[u8]) -> Result<Self, LabelError> {
        Ok(parse_svg(bytes, None)?.into())
    }

    /// Read an image file; `.svg` files are parsed as SVG (relative references resolve against
    /// the file's directory), anything else as a raster image.
    pub fn from_file(path: &std::path::Path) -> Result<Self, LabelError> {
        Ok(load_picture(path)?.into())
    }

    /// Split into the two ways a logo is drawn.
    pub(crate) fn kind(&self) -> LogoKind<'_> {
        match self {
            Logo::Image(img) => LogoKind::Picture(Picture::Raster(img.clone())),
            #[cfg(feature = "svg")]
            Logo::Svg(tree) => LogoKind::Picture(Picture::Svg(tree.clone())),
            Logo::Stamp(text) => LogoKind::Stamp(text),
        }
    }

    pub fn from_image(img: image::DynamicImage) -> Self {
        Logo::Image(Arc::new(img.to_rgba8()))
    }

    pub fn stamp(text: impl Into<String>) -> Self {
        Logo::Stamp(text.into())
    }
}

pub(crate) enum LogoKind<'a> {
    Picture(Picture),
    Stamp(&'a str),
}

/// An image logo, raster or SVG; never a stamp.
#[derive(Clone, Debug)]
pub(crate) enum Picture {
    Raster(Arc<RgbaImage>),
    #[cfg(feature = "svg")]
    Svg(Arc<resvg::usvg::Tree>),
}

impl From<Picture> for Logo {
    fn from(p: Picture) -> Self {
        match p {
            Picture::Raster(img) => Logo::Image(img),
            #[cfg(feature = "svg")]
            Picture::Svg(tree) => Logo::Svg(tree),
        }
    }
}

/// Read an image file; `.svg` by extension is parsed as SVG.
pub(crate) fn load_picture(path: &std::path::Path) -> Result<Picture, LabelError> {
    let bytes = std::fs::read(path).map_err(|source| LabelError::Io {
        what: "image",
        path: path.to_path_buf(),
        source,
    })?;
    let is_svg = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    let picture = if is_svg {
        #[cfg(feature = "svg")]
        {
            parse_svg(&bytes, path.parent())
        }
        #[cfg(not(feature = "svg"))]
        {
            Err(LabelError::Image {
                path: path.to_path_buf(),
                reason: "SVG support is not built in (cargo feature `svg`)".to_string(),
            })
        }
    } else {
        image::load_from_memory(&bytes)
            .map(|img| Picture::Raster(Arc::new(img.to_rgba8())))
            .map_err(|e| LabelError::Image {
                path: "<memory>".into(),
                reason: e.to_string(),
            })
    };
    picture.map_err(|e| match e {
        LabelError::Image { reason, .. } => LabelError::Image {
            path: path.to_path_buf(),
            reason,
        },
        e => e,
    })
}

#[cfg(feature = "svg")]
fn parse_svg(bytes: &[u8], resources_dir: Option<&std::path::Path>) -> Result<Picture, LabelError> {
    let mut opt = resvg::usvg::Options {
        resources_dir: resources_dir.map(std::path::Path::to_path_buf),
        ..Default::default()
    };
    if resources_dir.is_none() {
        // SVG bytes from memory have no directory of their own; without this, usvg would read
        // files named by `<image href>` relative to the process's working directory
        opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
    }
    let db = opt.fontdb_mut();
    db.load_system_fonts();
    fix_generic_families(db);
    let tree = resvg::usvg::Tree::from_data(bytes, &opt).map_err(|e| LabelError::Image {
        path: "<svg>".into(),
        reason: e.to_string(),
    })?;
    Ok(Picture::Svg(Arc::new(tree)))
}

/// fontconfig may name a generic family (sans-serif, ...) that is not installed, and text in
/// that family would silently disappear; point such generics at an installed face instead.
#[cfg(feature = "svg")]
fn fix_generic_families(db: &mut resvg::usvg::fontdb::Database) {
    use resvg::usvg::fontdb::{Family, Query};
    let fallback = ["Noto Sans CJK JP", "Noto Sans", "DejaVu Sans"]
        .iter()
        .find(|f| {
            db.query(&Query {
                families: &[Family::Name(f)],
                ..Default::default()
            })
            .is_some()
        })
        .map(|f| f.to_string())
        .or_else(|| {
            db.faces()
                .next()
                .and_then(|f| f.families.first())
                .map(|f| f.0.clone())
        });
    if let Some(fallback) = fallback {
        let missing = |db: &resvg::usvg::fontdb::Database, g: Family| {
            db.query(&Query {
                families: &[g],
                ..Default::default()
            })
            .is_none()
        };
        if missing(db, Family::Serif) {
            db.set_serif_family(fallback.clone());
        }
        if missing(db, Family::SansSerif) {
            db.set_sans_serif_family(fallback.clone());
        }
        if missing(db, Family::Cursive) {
            db.set_cursive_family(fallback.clone());
        }
        if missing(db, Family::Fantasy) {
            db.set_fantasy_family(fallback.clone());
        }
        if missing(db, Family::Monospace) {
            db.set_monospace_family(fallback);
        }
    }
}

/// How a raster logo becomes black and white.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Binarize {
    /// pixels darker than the level are printed (0-255)
    Threshold(u8),
    /// Floyd-Steinberg error diffusion; keeps shading of photos and gradients
    Dither,
}

impl Default for Binarize {
    fn default() -> Self {
        Binarize::Threshold(128)
    }
}

/// How a text logo is drawn.
#[derive(Deserialize, Debug, Clone)]
pub struct StampStyle {
    /// a font name from the template's `[fonts]`
    pub font: String,
    /// stack the characters top to bottom
    #[serde(default)]
    pub vertical: bool,
    /// white text on a black rounded box
    #[serde(default)]
    pub invert: bool,
    /// space between the text and the edge of the box
    #[serde(default = "default_padding_mm")]
    pub padding_mm: f32,
    /// smallest box width as a fraction of the height (1.0 = square); the text is centred
    #[serde(default)]
    pub min_width_ratio: f32,
}

fn default_padding_mm() -> f32 {
    0.5
}

/// The logo a template uses when the label supplies none.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum DefaultLogo {
    /// a stamp with this text
    Text { text: String },
    /// an image file, relative to the template
    Path { path: String },
    /// a logo registered on the renderer
    Key { key: String },
}

/// Scale the image to height_px (keeping aspect) and binarize it.
/// Transparent pixels count as white.
pub(crate) fn render_raster(
    img: &RgbaImage,
    height_px: usize,
    binarize: Binarize,
    invert: bool,
) -> Result<TapeDisplay, LabelError> {
    if img.width() == 0 || img.height() == 0 {
        return Err(LabelError::Image {
            path: "<logo>".into(),
            reason: format!("the image is {}x{} px", img.width(), img.height()),
        });
    }
    // at least 1 px wide, even for an extremely tall image
    let width_px = ((img.width() as usize * height_px + img.height() as usize / 2)
        / img.height() as usize)
        .max(1);
    // composite over white before scaling: scaling straight RGBA would let the hidden colour
    // of transparent pixels bleed into their opaque neighbours
    // kept as f32 so that the threshold compares the exact luma, not a rounded one;
    // `image` treats f32 samples as 0.0..=1.0 and clamps to it while resizing
    let flat = image::ImageBuffer::<image::Luma<f32>, Vec<f32>>::from_fn(
        img.width(),
        img.height(),
        |x, y| {
            let [r, g, b, a] = img.get_pixel(x, y).0;
            let luma = (r as f32 * 299.0 + g as f32 * 587.0 + b as f32 * 114.0) / 1000.0;
            let a = a as f32 / 255.0;
            image::Luma([(luma * a + 255.0 * (1.0 - a)) / 255.0])
        },
    );
    let flat = image::imageops::resize(
        &flat,
        width_px as u32,
        height_px as u32,
        image::imageops::FilterType::Triangle,
    );
    // 0.0 = black .. 255.0 = white
    let mut luma: Vec<f32> = flat.pixels().map(|p| p.0[0] * 255.0).collect();
    let mut td = TapeDisplay::new(width_px, height_px);
    for y in 0..height_px {
        for x in 0..width_px {
            let i = y * width_px + x;
            let black = match binarize {
                Binarize::Threshold(level) => luma[i] < level as f32,
                Binarize::Dither => {
                    let old = luma[i];
                    let black = old < 128.0;
                    let err = old - if black { 0.0 } else { 255.0 };
                    let mut spread = |dx: isize, dy: usize, w: f32| {
                        let nx = x as isize + dx;
                        if nx >= 0 && (nx as usize) < width_px && y + dy < height_px {
                            luma[(y + dy) * width_px + nx as usize] += err * w;
                        }
                    };
                    spread(1, 0, 7.0 / 16.0);
                    spread(-1, 1, 3.0 / 16.0);
                    spread(0, 1, 5.0 / 16.0);
                    spread(1, 1, 1.0 / 16.0);
                    black
                }
            };
            td.set_pixel(x, y, black != invert);
        }
    }
    Ok(td)
}

/// Draw an image or SVG logo height_px high, black and white.
pub(crate) fn render_picture(
    picture: &Picture,
    height_px: usize,
    binarize: Binarize,
    invert: bool,
) -> Result<TapeDisplay, LabelError> {
    match picture {
        Picture::Raster(img) => render_raster(img, height_px, binarize, invert),
        #[cfg(feature = "svg")]
        Picture::Svg(tree) => render_raster(
            &rasterize_svg(tree, height_px)?,
            height_px,
            binarize,
            invert,
        ),
    }
}

/// Draw the SVG at height_px (keeping aspect) as straight (not premultiplied) RGBA.
#[cfg(feature = "svg")]
fn rasterize_svg(tree: &resvg::usvg::Tree, height_px: usize) -> Result<RgbaImage, LabelError> {
    let size = tree.size();
    let scale = height_px as f32 / size.height();
    // at least 1 px wide so that a tall narrow logo still renders; the horizontal scale
    // follows the final width, so a widened logo is stretched to fill it (as raster images are)
    let width_px = ((size.width() * scale).round() as u32).max(1);
    let scale_x = width_px as f32 / size.width();
    let pixmap = (width_px > 0 && height_px > 0)
        .then(|| resvg::tiny_skia::Pixmap::new(width_px, height_px as u32))
        .flatten();
    let Some(mut pixmap) = pixmap else {
        return Err(LabelError::Image {
            path: "<svg>".into(),
            reason: format!("cannot draw {}x{} px", width_px, height_px),
        });
    };
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale_x, scale),
        &mut pixmap.as_mut(),
    );
    let data = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(RgbaImage::from_raw(width_px, height_px as u32, data).expect("size matches the pixmap"))
}

/// Draw text as a stamp that is height_px high.
pub(crate) fn render_stamp(
    font: &FontVec,
    text: &str,
    height_px: usize,
    style: &StampStyle,
) -> Result<TapeDisplay, LabelError> {
    let pad = mm_to_px(style.padding_mm).max(0) as usize;
    let inner = height_px.saturating_sub(2 * pad);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || inner == 0 {
        return Err(LabelError::InvalidTemplate(format!(
            "stamp {text:?} does not fit in {height_px} px with {} mm padding",
            style.padding_mm
        )));
    }
    // (glyph bitmap, x offset, y offset) inside the padded box
    let mut pieces = Vec::new();
    let content_w;
    if style.vertical {
        // each character gets a square cell and is scaled so that its ink fills it
        let cell = inner / chars.len();
        if cell == 0 {
            return Err(LabelError::InvalidTemplate(format!(
                "vertical stamp {text:?} has more characters than {inner} px can hold"
            )));
        }
        let glyphs: Vec<TapeDisplay> = chars
            .iter()
            .map(|c| fit_ink(font, &c.to_string(), cell, cell))
            .collect();
        content_w = glyphs.iter().map(|g| g.width).max().unwrap_or(0);
        for (i, g) in glyphs.into_iter().enumerate() {
            let x = (content_w - g.width) / 2;
            let y = i * cell + (cell - g.height) / 2;
            pieces.push((g, x, y));
        }
    } else {
        let line = fit_ink(font, text, usize::MAX, inner);
        content_w = line.width;
        let y = (inner - line.height) / 2;
        pieces.push((line, 0, y));
    }
    let width = (content_w + 2 * pad).max((height_px as f32 * style.min_width_ratio) as usize);
    let left = (width - content_w) / 2;
    let mut td = TapeDisplay::new(width, height_px);
    if style.invert {
        fill_rounded_box(&mut td, pad.max(1));
    }
    for (g, x, y) in pieces {
        for gy in 0..g.height {
            for gx in 0..g.width {
                if g.get_pixel(gx, gy) {
                    td.set_pixel(left + x + gx, pad + y + gy, !style.invert);
                }
            }
        }
    }
    Ok(td)
}

/// Render text so that its ink (not its line box) is as large as fits in max_w x max_h.
fn fit_ink(font: &FontVec, text: &str, max_w: usize, max_h: usize) -> TapeDisplay {
    // the ink of CJK glyphs is roughly the em square, which is smaller than the line box
    // ab_glyph sizes text by; draw large, crop to the ink, then scale down to fit
    const PROBE_PX: usize = 256;
    let probe = crop_to_ink(&super::template::render_ink(font, text, PROBE_PX));
    if probe.width == 0 || probe.height == 0 {
        return TapeDisplay::new(0, 0);
    }
    let scale = (max_h as f32 / probe.height as f32).min(max_w as f32 / probe.width as f32);
    let mut line_px = ((PROBE_PX as f32 * scale).floor() as usize).max(1);
    // rasterizing at the scaled size can round the ink 1 px larger than the probe predicts;
    // shrink until it fits, because the caller centres it with unsigned arithmetic
    loop {
        let ink = crop_to_ink(&super::template::render_ink(font, text, line_px));
        if ink.width <= max_w && ink.height <= max_h {
            return ink;
        }
        if line_px == 1 {
            // nothing this font draws fits; an empty glyph keeps the bounds guarantee
            return TapeDisplay::new(0, 0);
        }
        line_px -= 1;
    }
}

fn crop_to_ink(td: &TapeDisplay) -> TapeDisplay {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..td.height {
        for x in 0..td.width {
            if td.get_pixel(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == usize::MAX {
        return TapeDisplay::new(0, 0);
    }
    let mut out = TapeDisplay::new(x1 - x0, y1 - y0);
    for y in y0..y1 {
        for x in x0..x1 {
            out.set_pixel(x - x0, y - y0, td.get_pixel(x, y));
        }
    }
    out
}

fn fill_rounded_box(td: &mut TapeDisplay, radius: usize) {
    let (w, h) = (td.width, td.height);
    let r = radius.min(w / 2).min(h / 2) as f32;
    for y in 0..h {
        for x in 0..w {
            // distance from the nearest corner circle's centre, only inside the corner squares
            let cx = if (x as f32) < r {
                r - 0.5
            } else if (x as f32) >= w as f32 - r {
                w as f32 - r - 0.5
            } else {
                x as f32
            };
            let cy = if (y as f32) < r {
                r - 0.5
            } else if (y as f32) >= h as f32 - r {
                h as f32 - r - 0.5
            } else {
                y as f32
            };
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            td.set_pixel(x, y, dx * dx + dy * dy <= r * r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::template::Template;
    use crate::label::LabelData;
    use crate::label::Renderer;
    use crate::Tape;

    const BLACK: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Black.ttc";

    fn renderer(column: &str) -> Result<Renderer, LabelError> {
        let s = format!("[[columns]]\n{column}\n[fonts]\nblack = {{ path = \"{BLACK}\" }}\n");
        Renderer::new(Template::from_toml_str(&s, ".")?)
    }

    /// 4x2 image: left half black, right half white
    fn half_black() -> Logo {
        let mut img = RgbaImage::new(4, 2);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 2 {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            };
        }
        Logo::Image(Arc::new(img))
    }

    fn render(r: &Renderer, data: LabelData) -> Result<TapeDisplay, LabelError> {
        Ok(r.render(&data, Tape::W12)?.bitmap().clone())
    }

    const LOGO_COLUMN: &str = r#"type = "logo"
slot = "logo"
stamp = { font = "black", vertical = true, invert = true }"#;

    #[test]
    fn logo_comes_from_inline_then_key_then_default() {
        let mut r = renderer(&format!("{LOGO_COLUMN}\ndefault = {{ text = \"貸出\" }}")).unwrap();
        r.register_logo("dept", half_black());
        let h = Tape::W12.width_px() as usize;
        // an image logo keeps its 2:1 aspect at the full height
        let by_key = render(&r, LabelData::new().logo("logo", "dept")).unwrap();
        assert_eq!(by_key.width, h * 2);
        let inline = render(
            &r,
            LabelData::new().logo_inline("logo", Logo::stamp("総務")),
        )
        .unwrap();
        let default = render(&r, LabelData::new()).unwrap();
        // both are stamps; different text gives a different picture
        assert_ne!(inline.framebuffer, default.framebuffer);
        assert_ne!(inline.width, by_key.width);
    }

    #[test]
    fn unknown_key_and_missing_logo_are_errors() {
        let r = renderer(LOGO_COLUMN).unwrap();
        let err = render(&r, LabelData::new().logo("logo", "nope")).unwrap_err();
        assert!(
            matches!(&err, LabelError::UnknownLogo(k) if k == "nope"),
            "{err:?}"
        );
        let err = render(&r, LabelData::new()).unwrap_err();
        assert!(
            matches!(&err, LabelError::MissingLogo(s) if s == "logo"),
            "{err:?}"
        );
    }

    #[test]
    fn default_key_uses_a_registered_logo() {
        let mut r = renderer("type = \"logo\"\ndefault = { key = \"company\" }").unwrap();
        assert!(matches!(
            render(&r, LabelData::new()),
            Err(LabelError::UnknownLogo(_))
        ));
        r.register_logo("company", half_black());
        assert!(render(&r, LabelData::new()).is_ok());
    }

    #[test]
    fn templates_that_cannot_draw_are_rejected_when_the_renderer_is_created() {
        let cases = [
            ("type = \"logo\"", "needs `slot` or `default`"),
            (
                "type = \"logo\"\ndefault = { text = \"x\" }",
                "text default logo needs `stamp`",
            ),
            (
                "type = \"logo\"\nslot = \"l\"\nstamp = { font = \"nope\" }",
                "font 'nope'",
            ),
        ];
        for (column, expected) in cases {
            let err = renderer(column).err().expect(column);
            assert!(err.to_string().contains(expected), "{column}: {err}");
        }
    }

    #[test]
    fn stamp_without_style_is_an_error_at_render() {
        let r = renderer("type = \"logo\"\nslot = \"l\"").unwrap();
        let err = render(&r, LabelData::new().logo_inline("l", Logo::stamp("x"))).unwrap_err();
        assert!(matches!(err, LabelError::InvalidTemplate(_)), "{err:?}");
    }

    #[test]
    fn raster_threshold_invert_and_transparency() {
        let mut img = RgbaImage::new(4, 1);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 255])); // black
        img.put_pixel(1, 0, image::Rgba([100, 100, 100, 255])); // dark grey
        img.put_pixel(2, 0, image::Rgba([200, 200, 200, 255])); // light grey
        img.put_pixel(3, 0, image::Rgba([0, 0, 0, 0])); // transparent counts as white
        let row = |td: &TapeDisplay| (0..4).map(|x| td.get_pixel(x, 0)).collect::<Vec<_>>();
        let cases = [
            (
                Binarize::Threshold(128),
                false,
                vec![true, true, false, false],
            ),
            (
                Binarize::Threshold(50),
                false,
                vec![true, false, false, false],
            ),
            (
                Binarize::Threshold(128),
                true,
                vec![false, false, true, true],
            ),
        ];
        for (binarize, invert, expected) in cases {
            let td = render_raster(&img, 1, binarize, invert).unwrap();
            assert_eq!(row(&td), expected, "{binarize:?} invert={invert}");
        }
    }

    #[test]
    fn hidden_colour_of_transparent_pixels_does_not_change_the_result() {
        // the two images look the same; only the RGB under alpha 0 differs
        let make = |hidden: u8| {
            RgbaImage::from_fn(8, 8, |x, _| {
                if x < 4 {
                    image::Rgba([0, 0, 0, 255])
                } else {
                    image::Rgba([hidden, hidden, hidden, 0])
                }
            })
        };
        for (binarize, h) in [
            (Binarize::Threshold(128), 3),
            (Binarize::Threshold(200), 5),
            (Binarize::Dither, 5),
        ] {
            let a = render_raster(&make(0), h, binarize, false).unwrap();
            let b = render_raster(&make(255), h, binarize, false).unwrap();
            assert_eq!(a.framebuffer, b.framebuffer, "{binarize:?} h={h}");
        }
    }

    #[test]
    fn threshold_compares_the_exact_luma() {
        // luma 127.544 is darker than 128 and must print; rounding it to 128 would not.
        // Heights 2 (no resize), 1 (shrink) and 5 (enlarge) cover both resize paths.
        let dark = RgbaImage::from_pixel(2, 2, image::Rgba([128, 128, 124, 255]));
        let light = RgbaImage::from_pixel(2, 2, image::Rgba([129, 129, 129, 255]));
        let all = |img: &RgbaImage, h: usize| {
            let td = render_raster(img, h, Binarize::Threshold(128), false).unwrap();
            td.framebuffer.iter().flatten().filter(|p| **p).count()
        };
        for h in [2, 1, 5] {
            assert_eq!(all(&dark, h), h * h, "dark h={h}");
            assert_eq!(all(&light, h), 0, "light h={h}");
        }
    }

    #[test]
    fn white_stays_white_and_black_stays_black_when_resized() {
        let white = RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
        let black = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
        for binarize in [Binarize::Threshold(128), Binarize::Dither] {
            for h in [4, 16] {
                let w = render_raster(&white, h, binarize, false).unwrap();
                let b = render_raster(&black, h, binarize, false).unwrap();
                let ink =
                    |td: &TapeDisplay| td.framebuffer.iter().flatten().filter(|p| **p).count();
                assert_eq!(ink(&w), 0, "white {binarize:?} h={h}");
                assert_eq!(ink(&b), h * h, "black {binarize:?} h={h}");
            }
        }
    }

    #[test]
    fn extremely_tall_image_is_still_one_pixel_wide() {
        let img = RgbaImage::from_pixel(1, 1000, image::Rgba([0, 0, 0, 255]));
        let td = render_raster(&img, 40, Binarize::default(), false).unwrap();
        assert_eq!((td.width, td.height), (1, 40));
        assert!(td.get_pixel(0, 20));
    }

    #[test]
    fn dither_keeps_the_average_shade_of_a_flat_grey() {
        // a flat 50 % grey thresholds to all white but dithers to about half black
        let img = RgbaImage::from_pixel(40, 40, image::Rgba([128, 128, 128, 255]));
        let black = |td: &TapeDisplay| td.framebuffer.iter().flatten().filter(|p| **p).count();
        assert_eq!(
            black(&render_raster(&img, 40, Binarize::Threshold(128), false).unwrap()),
            0
        );
        let dithered = black(&render_raster(&img, 40, Binarize::Dither, false).unwrap());
        assert!((700..=900).contains(&dithered), "{dithered} of 1600");
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "slow in debug builds; CI runs it with --release"
    )]
    fn stamp_fits_its_box_for_every_height() {
        // the ink of some glyphs rounded 1 px over the cell and broke the centring arithmetic
        let font = FontVec::try_from_vec_and_index(std::fs::read(BLACK).unwrap(), 0).unwrap();
        for vertical in [false, true] {
            let style = StampStyle {
                font: "black".into(),
                vertical,
                invert: true,
                padding_mm: 0.5,
                min_width_ratio: 0.0,
            };
            for text in ["口", "-", "貸出"] {
                // from the printable height of the narrowest tape (4 mm, 40 px)
                for h in 40..400 {
                    let td = render_stamp(&font, text, h, &style).unwrap();
                    assert_eq!(td.height, h, "{text:?} vertical={vertical} h={h}");
                    // the white text stays inside the padded box
                    let pad = mm_to_px(0.5) as usize;
                    let white_inside = (pad..h - pad)
                        .flat_map(|y| (pad..td.width - pad).map(move |x| (x, y)))
                        .filter(|&(x, y)| !td.get_pixel(x, y))
                        .count();
                    assert!(white_inside > 0, "{text:?} vertical={vertical} h={h}");
                }
            }
        }
    }

    #[test]
    fn vertical_stamp_with_more_characters_than_pixels_is_an_error() {
        let style = StampStyle {
            font: "black".into(),
            vertical: true,
            invert: false,
            padding_mm: 0.0,
            min_width_ratio: 0.0,
        };
        let font = FontVec::try_from_vec_and_index(std::fs::read(BLACK).unwrap(), 0).unwrap();
        let err = render_stamp(&font, &"貸".repeat(41), 40, &style).unwrap_err();
        assert!(matches!(err, LabelError::InvalidTemplate(_)), "{err:?}");
        assert!(render_stamp(&font, &"貸".repeat(4), 40, &style).is_ok());
    }

    #[test]
    fn empty_image_logo_is_an_error() {
        let r = renderer("type = \"logo\"\nslot = \"l\"").unwrap();
        let empty = Logo::Image(Arc::new(RgbaImage::new(0, 0)));
        let err = render(&r, LabelData::new().logo_inline("l", empty)).unwrap_err();
        assert!(matches!(err, LabelError::Image { .. }), "{err:?}");
    }

    #[test]
    fn default_text_is_expanded_from_the_fields() {
        let r = renderer(&format!(
            "{LOGO_COLUMN}\ndefault = {{ text = \"{{dept}}\" }}"
        ))
        .unwrap();
        let a = render(&r, LabelData::new().text("dept", "総務")).unwrap();
        let b = render(&r, LabelData::new().text("dept", "開発")).unwrap();
        assert_ne!(a.framebuffer, b.framebuffer);
        let err = render(&r, LabelData::new()).unwrap_err();
        assert!(
            matches!(&err, LabelError::MissingField(f) if f == "dept"),
            "{err:?}"
        );
    }

    #[test]
    fn template_images_are_loaded_when_the_renderer_is_created() {
        let dir = std::env::temp_dir().join(format!("sr5900p-logo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // separate files, so that loading one cannot stand in for loading the other
        let (image, logo) = (dir.join("image.png"), dir.join("logo.png"));
        for p in [&image, &logo] {
            RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 0, 255]))
                .save(p)
                .unwrap();
        }
        let body = format!(
            "[[columns]]\ntype = \"image\"\npath = \"{}\"\n\
             [[columns]]\ntype = \"logo\"\ndefault = {{ path = \"{}\" }}\n",
            image.display(),
            logo.display()
        );
        let r = Renderer::new(Template::from_toml_str(&body, ".").unwrap()).unwrap();
        // the files are not read again when rendering
        std::fs::remove_dir_all(&dir).unwrap();
        let td = render(&r, LabelData::new()).unwrap();
        let h = Tape::W12.width_px() as usize;
        let gap = mm_to_px(1.5) as usize;
        // two 2:1 images, fully black, side by side
        assert_eq!(td.width, 2 * h + gap + 2 * h);
        assert!(td.get_pixel(0, 0) && td.get_pixel(td.width - 1, h - 1));
        // a missing file is reported when the renderer is created
        let err = Renderer::new(Template::from_toml_str(&body, ".").unwrap())
            .err()
            .expect("missing image");
        assert!(
            matches!(err, LabelError::Io { what: "image", .. }),
            "{err:?}"
        );
    }

    #[cfg(feature = "svg")]
    /// 20x10 SVG: left half black, right half white
    const HALF_BLACK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10">
<rect x="0" y="0" width="10" height="10" fill="black"/>
<rect x="10" y="0" width="10" height="10" fill="white"/>
</svg>"#;

    #[cfg(feature = "svg")]
    #[test]
    fn svg_logo_is_drawn_at_the_column_height() {
        let Picture::Svg(tree) = parse_svg(HALF_BLACK_SVG.as_bytes(), None).unwrap() else {
            unreachable!()
        };
        let picture = Picture::Svg(tree.clone());
        for h in [40, 144, 368] {
            // drawn as a vector at the target size, not drawn small and then enlarged
            let raster = rasterize_svg(&tree, h).unwrap();
            assert_eq!((raster.width(), raster.height()), (2 * h as u32, h as u32));
            let td = render_picture(&picture, h, Binarize::default(), false).unwrap();
            // 2:1 aspect kept, the boundary sits in the middle at every height
            assert_eq!((td.width, td.height), (h * 2, h), "h={h}");
            for y in [0, h / 2, h - 1] {
                assert!(td.get_pixel(0, y) && td.get_pixel(h - 2, y), "h={h} y={y}");
                assert!(
                    !td.get_pixel(h + 1, y) && !td.get_pixel(2 * h - 1, y),
                    "h={h} y={y}"
                );
            }
        }
    }

    #[cfg(feature = "svg")]
    #[test]
    fn extremely_tall_svg_is_still_one_pixel_wide() {
        // 1x1000 at 40 px high would cover 0.04 px; it must be stretched to fill its 1 px column
        for w in [1, 5, 10] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="1000">
<rect width="{w}" height="1000" fill="black"/></svg>"#
            );
            let picture = parse_svg(svg.as_bytes(), None).unwrap();
            let td = render_picture(&picture, 40, Binarize::default(), false).unwrap();
            assert_eq!((td.width, td.height), (1, 40), "w={w}");
            assert!((0..40).all(|y| td.get_pixel(0, y)), "w={w}");
        }
    }

    #[cfg(feature = "svg")]
    #[test]
    fn invalid_svg_is_an_error() {
        let err = Logo::from_svg(b"<svg").unwrap_err();
        assert!(matches!(err, LabelError::Image { .. }), "{err:?}");
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_file_is_loaded_by_extension() {
        let dir = std::env::temp_dir().join(format!("sr5900p-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = dir.join("logo.svg");
        std::fs::write(&svg, HALF_BLACK_SVG).unwrap();
        let body = format!(
            "[[columns]]\ntype = \"logo\"\ndefault = {{ path = \"{}\" }}\n",
            svg.display()
        );
        let r = Renderer::new(Template::from_toml_str(&body, ".").unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let td = render(&r, LabelData::new()).unwrap();
        let h = Tape::W12.width_px() as usize;
        assert_eq!(td.width, 2 * h);
        assert!(td.get_pixel(0, 0) && !td.get_pixel(2 * h - 1, 0));
    }

    #[cfg(not(feature = "svg"))]
    #[test]
    fn svg_file_without_the_svg_feature_is_an_error() {
        let dir = std::env::temp_dir().join(format!("sr5900p-nosvg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = dir.join("logo.svg");
        std::fs::write(&svg, "<svg/>").unwrap();
        let err = load_picture(&svg).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            matches!(&err, LabelError::Image { reason, .. } if reason.contains("`svg`")),
            "{err:?}"
        );
    }

    #[cfg(feature = "svg")]
    fn svg_ink(svg: &str, h: usize) -> usize {
        let picture = parse_svg(svg.as_bytes(), None).unwrap();
        let td = render_picture(&picture, h, Binarize::default(), false).unwrap();
        td.framebuffer.iter().flatten().filter(|p| **p).count()
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_text_in_a_generic_family_is_drawn() {
        // fontconfig can name a generic family that is not installed; the text then vanished
        for family in ["", "font-family=\"sans-serif\"", "font-family=\"serif\""] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20">
<text x="2" y="16" font-size="16" {family}>ABC</text></svg>"#
            );
            assert!(svg_ink(&svg, 40) > 50, "{family:?}");
        }
    }

    #[cfg(feature = "svg")]
    #[test]
    fn generic_families_that_are_not_installed_are_pointed_at_an_installed_face() {
        // independent of the machine's fontconfig: a database with one font whose generic
        // families name fonts that do not exist
        use resvg::usvg::fontdb::{Database, Family, Query};
        let mut db = Database::new();
        db.load_font_file(BLACK).unwrap();
        db.set_serif_family("No Such Serif");
        db.set_sans_serif_family("No Such Sans");
        // resolvable but not the fallback, so leaving it alone is observable
        db.set_monospace_family("Noto Sans CJK KR");
        let resolves = |db: &Database, g: Family| {
            db.query(&Query {
                families: &[g],
                ..Default::default()
            })
            .is_some()
        };
        assert!(!resolves(&db, Family::Serif) && !resolves(&db, Family::SansSerif));
        fix_generic_families(&mut db);
        for g in [
            Family::Serif,
            Family::SansSerif,
            Family::Cursive,
            Family::Fantasy,
            Family::Monospace,
        ] {
            assert!(resolves(&db, g), "{g:?}");
        }
        assert_eq!(db.family_name(&Family::Monospace), "Noto Sans CJK KR");
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_semi_transparent_colour_is_un_premultiplied() {
        // half-transparent white must stay white (straight 255), not turn into premultiplied grey
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
<rect width="10" height="10" fill="white" fill-opacity="0.5"/></svg>"#;
        let Picture::Svg(tree) = parse_svg(svg.as_bytes(), None).unwrap() else {
            unreachable!()
        };
        let p = rasterize_svg(&tree, 10).unwrap().get_pixel(5, 5).0;
        assert_eq!(&p[..3], &[255, 255, 255], "{p:?}");
        assert!((120..=135).contains(&p[3]), "{p:?}");
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_from_memory_does_not_read_files() {
        // a file in the working directory (the crate root while testing) must not be picked up
        let name = format!("sr5900p-cwd-{}.png", std::process::id());
        RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 255]))
            .save(&name)
            .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
<image href="{name}" width="10" height="10"/></svg>"#
        );
        let ink = svg_ink(&svg, 40);
        std::fs::remove_file(&name).unwrap();
        assert_eq!(ink, 0);
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_from_memory_still_draws_embedded_data_images() {
        // a 10x10 black PNG embedded as a data: URL
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
<image href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAoAAAAKCAYAAACNMs+9AAAAGElEQVR4nGNkYGD4z0AEYCJG0ahC6ikEAKYXARPmwrp4AAAAAElFTkSuQmCC" width="10" height="10"/></svg>"#;
        assert_eq!(svg_ink(svg, 40), 40 * 40);
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_relative_image_resolves_next_to_the_svg_file() {
        let dir = std::env::temp_dir().join(format!("sr5900p-svgref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 255]))
            .save(dir.join("icon.png"))
            .unwrap();
        let svg = dir.join("logo.svg");
        std::fs::write(
            &svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
<image href="icon.png" width="10" height="10"/></svg>"#,
        )
        .unwrap();
        // the tests run in the crate root, so a cwd-relative lookup would miss icon.png
        let picture = load_picture(&svg).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let td = render_picture(&picture, 40, Binarize::default(), false).unwrap();
        assert!(td.get_pixel(20, 20));
    }

    #[test]
    fn inverted_stamp_is_white_text_on_a_black_box() {
        let style = StampStyle {
            font: "black".into(),
            vertical: false,
            invert: true,
            padding_mm: 0.5,
            min_width_ratio: 0.0,
        };
        let font = FontVec::try_from_vec_and_index(std::fs::read(BLACK).unwrap(), 0).unwrap();
        let td = render_stamp(&font, "総務", 144, &style).unwrap();
        let pad = mm_to_px(0.5) as usize;
        // the padding band inside the rounded corners is solid black
        assert!(td.get_pixel(td.width / 2, pad / 2));
        assert!(td.get_pixel(td.width / 2, td.height - 1 - pad / 2));
        // the corners are cut
        assert!(!td.get_pixel(0, 0));
        // some white text inside the box
        let inside_white = (pad..td.height - pad)
            .flat_map(|y| (pad..td.width - pad).map(move |x| (x, y)))
            .filter(|&(x, y)| !td.get_pixel(x, y))
            .count();
        assert!(inside_white > 0);
    }
}
