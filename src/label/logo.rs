//! Logos: an image or a text stamp placed in a `logo` column of a template.
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
pub enum Logo {
    /// A raster image. It is scaled to the column height and binarized as the template says.
    Image(Arc<RgbaImage>),
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

    pub fn from_image(img: image::DynamicImage) -> Self {
        Logo::Image(Arc::new(img.to_rgba8()))
    }

    pub fn stamp(text: impl Into<String>) -> Self {
        Logo::Stamp(text.into())
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
        assert!(matches!(err, LabelError::Image { .. }), "{err:?}");
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
