//! Rendering labels into 1-bit bitmaps.
pub mod display;
pub mod error;
pub mod template;

use anyhow::Result;
use display::TapeDisplay;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

pub fn mm_to_px(mm: f32) -> i32 {
    const DPI: f32 = 360.0;
    const MM_TO_INCH: f32 = 10.0 / 254.0;
    (mm * DPI * MM_TO_INCH).floor() as i32
}

/// Write the label as a PNG image.
pub fn save_preview(path: impl AsRef<Path>, td: &TapeDisplay) -> Result<()> {
    let file = File::create(path)?;
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, td.width as u32, td.height as u32);
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
    let mut writer = encoder.write_header()?;
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
    writer.write_image_data(&data)?;
    Ok(())
}
