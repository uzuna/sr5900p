use crate::analyzer::analyze_tcp_data;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use argh::FromArgs;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::geometry::Point;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::Size;
use embedded_graphics::primitives::PrimitiveStyle;
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::primitives::StyledDrawable;
use embedded_graphics::text::Alignment;
use embedded_graphics::text::Baseline;
use embedded_graphics::text::Text;
use embedded_graphics::text::TextStyleBuilder;
use embedded_graphics::Drawable;
use image::Luma;
use qrcode::QrCode;
use sr5900p::label::display::TapeDisplay;
use sr5900p::label::mm_to_px;
use sr5900p::label::save_preview;
use sr5900p::label::template::Template;
use sr5900p::printer::detect_tape;
use sr5900p::printer::gen_tcp_data;
use sr5900p::printer::print_tcp_data;
use sr5900p::Tape;
use std::collections::HashMap;

fn determine_tape_width_px(args: &PrintArgs) -> Result<i32> {
    Ok(determine_tape(args)?.width_px())
}

fn determine_tape(args: &PrintArgs) -> Result<Tape> {
    let detected = if let Some(printer) = &args.printer {
        match detect_tape(printer) {
            Ok(t) => {
                eprintln!("Tape detected: {:?}", t);
                Some(t)
            }
            Err(e) => {
                eprintln!("{e}");
                None
            }
        }
    } else {
        None
    };
    let given = if let Some(mm) = args.width {
        Some(Tape::from_mm(mm)?)
    } else {
        None
    };
    Ok(match (given, detected) {
        (None, Some(w)) | (Some(w), None) => w,
        (Some(given), Some(detected)) => {
            if given != detected {
                eprintln!("Warning: {given:?} does not match with detected {detected:?}")
            }
            given
        }
        (None, None) => return Err(anyhow!("Please specify --width or --printer")),
    })
}

fn print_qr_text(args: &PrintArgs) -> Result<()> {
    let text = args.qr_text.as_ref().expect("Please specify --qr-text");
    let tape_width_px = determine_tape_width_px(args)? as usize;
    let qr_td = {
        let mut td = TapeDisplay::new(tape_width_px, tape_width_px);
        let tape_width_px = tape_width_px as u32;
        let code = QrCode::new(text).unwrap();
        let image = code
            .render::<Luma<u8>>()
            .max_dimensions(tape_width_px, tape_width_px)
            .build();
        let ofs_x = (tape_width_px - image.width()) / 2;
        let ofs_y = (tape_width_px - image.height()) / 2;
        for (x, y, p) in image.enumerate_pixels() {
            Rectangle::new(
                Point::new((x + ofs_x) as i32, (y + ofs_y) as i32),
                Size::new_equal(1),
            )
            .draw_styled(
                &PrimitiveStyle::with_fill(BinaryColor::from(p.0[0] == 0)),
                &mut td,
            )?;
        }
        image.save("qrcode.png").unwrap();
        td
    };
    let text_td = {
        let character_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let text_len = text.len();
        let margin_px = 4usize;
        let r = tape_width_px / (20 + margin_px);
        let mut td = TapeDisplay::new(10 * text_len + margin_px, 20 + margin_px);
        let tb = TextStyleBuilder::new();
        let ts = tb
            .alignment(Alignment::Center)
            .baseline(Baseline::Middle)
            .build();
        Text::with_text_style(text, td.bounding_box().center(), character_style, ts)
            .draw(&mut td)?;
        // magnify the td as much as possible to fit the parent
        td.scaled(r)
    };
    let mut td = TapeDisplay::new(qr_td.width + text_td.width, tape_width_px);
    td.overlay_or(&qr_td, 0, (td.height - qr_td.height) / 2);
    td.overlay_or(&text_td, qr_td.width, (td.height - text_td.height) / 2);
    print_td(args, &td)
}

fn print_qr_text_small(args: &PrintArgs) -> Result<()> {
    let text = args
        .qr_text_small
        .as_ref()
        .expect("Please specify --qr-text-small");
    let tape_width_px = determine_tape_width_px(args)? as usize;
    let qr_td = {
        let mut td = TapeDisplay::new(tape_width_px, tape_width_px);
        let tape_width_px = tape_width_px as u32;
        let code = QrCode::new(text).unwrap();
        let image = code
            .render::<Luma<u8>>()
            .max_dimensions(tape_width_px, tape_width_px)
            .build();
        let ofs_x = (tape_width_px - image.width()) / 2;
        let ofs_y = (tape_width_px - image.height()) / 2;
        for (x, y, p) in image.enumerate_pixels() {
            Rectangle::new(
                Point::new((x + ofs_x) as i32, (y + ofs_y) as i32),
                Size::new_equal(1),
            )
            .draw_styled(
                &PrimitiveStyle::with_fill(BinaryColor::from(p.0[0] == 0)),
                &mut td,
            )?;
        }
        image.save("qrcode.png").unwrap();
        td
    };
    let text_td = {
        let character_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let text_len = text.len();
        let margin_px = 4usize;
        let text_width = 10 * text_len + margin_px;
        let r = std::cmp::min(tape_width_px / text_width, 8);
        let mut td = TapeDisplay::new(text_width, 20 + margin_px);
        let tb = TextStyleBuilder::new();
        let ts = tb
            .alignment(Alignment::Center)
            .baseline(Baseline::Middle)
            .build();
        Text::with_text_style(text, td.bounding_box().center(), character_style, ts)
            .draw(&mut td)?;
        // magnify the td as much as possible to fit the parent
        td.rotated().scaled(r)
    };
    let mut td = TapeDisplay::new(qr_td.width * 9 / 10 + text_td.width, tape_width_px);
    td.overlay_or(&qr_td, 0, (td.height - qr_td.height) / 2);
    td.overlay_or(
        &text_td,
        qr_td.width * 9 / 10,
        (td.height - text_td.height) / 2,
    );
    print_td(args, &td)
}

fn print_template(args: &PrintArgs) -> Result<()> {
    let path = args
        .template
        .as_ref()
        .context("Please specify --template")?;
    let template = Template::load(path)?;
    let mut fields = HashMap::new();
    for f in &args.field {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| anyhow!("--field must be key=value: {f:?}"))?;
        fields.insert(k.to_string(), v.to_string());
    }
    let tape = determine_tape(args)?;
    let td = template.render(&fields, tape)?;
    print_td(args, &td)
}

fn print_td(args: &PrintArgs, td: &TapeDisplay) -> Result<()> {
    save_preview("preview.png", td)?;

    let tcp_data = gen_tcp_data(td)?;

    if !args.dry_run {
        print_tcp_data(
            args.printer.as_ref().context("Please specify --printer")?,
            &tcp_data,
        )
    } else {
        analyze_tcp_data(&tcp_data)?;
        Ok(())
    }
}

fn print_test_pattern(args: &PrintArgs) -> Result<()> {
    let tape_width_px = determine_tape_width_px(args)?;
    // td represents a tape segment
    let mut td = TapeDisplay::new(mm_to_px(40.0) as usize, tape_width_px as usize);
    // 1mm outline
    Rectangle::new(
        Point::new(0, 0),
        Size {
            width: td.width as u32,
            height: td.height as u32,
        },
    )
    .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::from(true)), &mut td)?;
    Rectangle::new(
        Point::new(mm_to_px(1.0), mm_to_px(1.0)),
        Size {
            width: td.width as u32 - mm_to_px(2.0) as u32,
            height: td.height as u32 - mm_to_px(2.0) as u32,
        },
    )
    .draw_styled(
        &PrimitiveStyle::with_fill(BinaryColor::from(false)),
        &mut td,
    )?;
    // 0.5mm squares, at 1mm cells, from the print origin
    for y_mm in 0..40 {
        for x_mm in 0..40 {
            Rectangle::new(
                Point::new(mm_to_px(x_mm as f32), mm_to_px(y_mm as f32)),
                Size::new_equal(mm_to_px(0.5) as u32),
            )
            .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::from(true)), &mut td)?;
        }
    }
    // 1cm square
    Rectangle::new(
        Point::new(0, 0),
        Size {
            width: mm_to_px(10.0) as u32,
            height: mm_to_px(10.0) as u32,
        },
    )
    .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::from(true)), &mut td)?;
    // 0.5cm square, at the center of the previous one
    Rectangle::new(
        Point::new(mm_to_px(2.5), mm_to_px(2.5)),
        Size {
            width: mm_to_px(5.0) as u32,
            height: mm_to_px(5.0) as u32,
        },
    )
    .draw_styled(
        &PrimitiveStyle::with_fill(BinaryColor::from(false)),
        &mut td,
    )?;
    let text_td = {
        let text = "Ag";
        let character_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let text_len = text.len();
        let margin_px = 4;
        let r = (td.height / (20 + margin_px)) as i32;
        let mut td = TapeDisplay::new(10 * text_len + margin_px, 20 + margin_px);
        // 1px outline (in text td)
        Rectangle::new(
            Point::new(0, 0),
            Size {
                width: td.width as u32,
                height: td.height as u32,
            },
        )
        .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::from(true)), &mut td)?;
        Rectangle::new(
            Point::new(1, 1),
            Size {
                width: td.width as u32 - 2,
                height: td.height as u32 - 2,
            },
        )
        .draw_styled(
            &PrimitiveStyle::with_fill(BinaryColor::from(false)),
            &mut td,
        )?;
        let tb = TextStyleBuilder::new();
        let ts = tb
            .alignment(Alignment::Center)
            .baseline(Baseline::Middle)
            .build();
        Text::with_text_style(text, td.bounding_box().center(), character_style, ts)
            .draw(&mut td)?;
        // magnify the td as much as possible to fit the parent
        td.scaled(r as usize)
    };
    td.overlay_or(
        &text_td,
        (td.width - text_td.width) / 2,
        (td.height - text_td.height) / 2,
    );
    print_td(args, &td)
}

#[derive(FromArgs, PartialEq, Debug)]
/// Print something
#[argh(subcommand, name = "print")]
pub struct PrintArgs {
    /// generate a label for a mac addr
    #[argh(option)]
    mac_addr: Option<String>,
    /// generate a label for a QR code with text
    #[argh(option)]
    qr_text: Option<String>,
    /// generate a label for a QR code with text
    #[argh(option)]
    qr_text_small: Option<String>,
    /// tape width in mm (default: auto)
    #[argh(option)]
    width: Option<usize>,
    /// do not print (just generate and analyze)
    #[argh(switch)]
    dry_run: bool,
    /// the raw dump of the TCP stream while printing
    #[argh(option)]
    tcp_data: Option<String>,
    /// print a test pattern
    #[argh(switch)]
    test_pattern: bool,
    /// an IPv4 address for the printer
    #[argh(option)]
    printer: Option<String>,
    /// label template file (TOML)
    #[argh(option)]
    template: Option<String>,
    /// template field as key=value (repeatable)
    #[argh(option)]
    field: Vec<String>,
}
pub fn do_print(args: &PrintArgs) -> Result<()> {
    if args.test_pattern {
        print_test_pattern(args)
    } else if args.qr_text.is_some() {
        print_qr_text(args)
    } else if args.qr_text_small.is_some() {
        print_qr_text_small(args)
    } else if args.template.is_some() {
        print_template(args)
    } else {
        Err(anyhow!("Please specify a print command"))
    }
}
