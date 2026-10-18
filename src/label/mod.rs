//! Rendering labels into 1-bit bitmaps.
//!
//! ```no_run
//! # fn main() -> Result<(), sr5900p::label::error::LabelError> {
//! use sr5900p::label::{template::Template, LabelData, Renderer};
//! use sr5900p::Tape;
//!
//! // load the template and its fonts once
//! let renderer = Renderer::new(Template::load("templates/shelf.toml")?)?;
//! let label = renderer.render(
//!     &LabelData::new().text("dept", "開発部").text("shelf_id", "SH-A03"),
//!     Tape::W12,
//! )?;
//! println!("{:.1} mm", label.length_mm());
//! std::fs::write("preview.png", label.to_png()?).unwrap();
//! # Ok(())
//! # }
//! ```
pub mod display;
pub mod error;
pub mod logo;
pub mod template;

use crate::Tape;
use display::TapeDisplay;
use error::LabelError;
use std::collections::HashMap;

pub use template::Renderer;

/// print resolution of the SR5900P head
const DPI: f32 = 360.0;

pub fn mm_to_px(mm: f32) -> i32 {
    const MM_TO_INCH: f32 = 10.0 / 254.0;
    (mm * DPI * MM_TO_INCH).floor() as i32
}

pub fn px_to_mm(px: usize) -> f32 {
    px as f32 * 25.4 / DPI
}

/// Values for the `{name}` placeholders of a template.
#[derive(Clone, Debug, Default)]
pub struct LabelData {
    pub(crate) texts: HashMap<String, String>,
    pub(crate) logos: HashMap<String, LogoChoice>,
}

#[derive(Clone, Debug)]
pub(crate) enum LogoChoice {
    Key(String),
    Inline(logo::Logo),
}

impl LabelData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.texts.insert(name.into(), value.into());
        self
    }

    /// Use the logo registered on the renderer under `key` for the logo column `slot`.
    pub fn logo(mut self, slot: impl Into<String>, key: impl Into<String>) -> Self {
        self.logos.insert(slot.into(), LogoChoice::Key(key.into()));
        self
    }

    /// Use this logo for the logo column `slot`, without registering it.
    pub fn logo_inline(mut self, slot: impl Into<String>, logo: logo::Logo) -> Self {
        self.logos.insert(slot.into(), LogoChoice::Inline(logo));
        self
    }
}

/// A rendered label: a bitmap whose height matches the tape it was rendered for.
#[derive(Clone, Debug)]
pub struct Label {
    bitmap: TapeDisplay,
    tape: Tape,
}

impl Label {
    pub fn new(bitmap: TapeDisplay, tape: Tape) -> Result<Self, LabelError> {
        if bitmap.height != tape.width_px() as usize {
            return Err(LabelError::HeightMismatch {
                height: bitmap.height,
                tape,
            });
        }
        Ok(Self { bitmap, tape })
    }

    pub fn tape(&self) -> Tape {
        self.tape
    }

    pub fn bitmap(&self) -> &TapeDisplay {
        &self.bitmap
    }

    /// Length along the tape, excluding the margins the printer adds.
    pub fn length_mm(&self) -> f32 {
        px_to_mm(self.bitmap.width)
    }

    /// The label as a PNG image (black on white).
    pub fn to_png(&self) -> Result<Vec<u8>, LabelError> {
        let td = &self.bitmap;
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, td.width as u32, td.height as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_source_gamma(png::ScaledFloat::from_scaled(45455)); // 1.0 / 2.2, scaled by 100000
            encoder.set_source_gamma(png::ScaledFloat::new(1.0 / 2.2)); // 1.0 / 2.2, unscaled, but rounded
            let source_chromaticities = png::SourceChromaticities::new(
                // Using unscaled instantiation here
                (0.31270, 0.32900),
                (0.64000, 0.33000),
                (0.30000, 0.60000),
                (0.15000, 0.06000),
            );
            encoder.set_source_chromaticities(source_chromaticities);
            let mut writer = encoder
                .write_header()
                .map_err(|e| LabelError::Png(e.to_string()))?;
            let data: Vec<u8> = td
                .framebuffer
                .iter()
                .flat_map(|row| row.iter())
                .flat_map(|c| {
                    // data will be [RGBARGBA...]
                    if *c {
                        [0, 0, 0, 255]
                    } else {
                        [255, 255, 255, 255]
                    }
                })
                .collect();
            writer
                .write_image_data(&data)
                .map_err(|e| LabelError::Png(e.to_string()))?;
            writer
                .finish()
                .map_err(|e| LabelError::Png(e.to_string()))?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_new_rejects_a_bitmap_of_another_height() {
        let err = Label::new(TapeDisplay::new(10, 143), Tape::W12).expect_err("should be rejected");
        assert!(matches!(
            err,
            LabelError::HeightMismatch {
                height: 143,
                tape: Tape::W12
            }
        ));
        assert!(Label::new(TapeDisplay::new(10, 144), Tape::W12).is_ok());
    }

    #[test]
    fn length_mm_is_the_width_at_360_dpi() {
        let label = Label::new(TapeDisplay::new(360, 144), Tape::W12).unwrap();
        assert_eq!(label.length_mm(), 25.4);
    }

    #[test]
    fn to_png_decodes_back_to_the_same_pixels() {
        let mut td = TapeDisplay::new(3, 144);
        td.set_pixel(0, 0, true);
        td.set_pixel(2, 143, true);
        let png = Label::new(td.clone(), Tape::W12).unwrap().to_png().unwrap();
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!((img.width(), img.height()), (3, 144));
        for (x, y, p) in img.enumerate_pixels() {
            assert_eq!(
                p.0[0] == 0,
                td.get_pixel(x as usize, y as usize),
                "({x}, {y})"
            );
        }
    }
}
