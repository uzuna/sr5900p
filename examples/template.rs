//! Render a label from a template as a library user would.
//! cargo run --example template -- <template.toml> <tape mm> key=value...
use anyhow::Context;
use anyhow::Result;
use sr5900p::label::template::Template;
use sr5900p::label::LabelData;
use sr5900p::label::Renderer;
use sr5900p::Tape;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let renderer = Renderer::new(Template::load(args.next().context("template path")?)?)?;
    let tape = Tape::from_mm(args.next().context("tape mm")?.parse()?)?;
    let data = args
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .fold(LabelData::new(), |d, (k, v)| d.text(k, v));
    let label = renderer.render(&data, tape)?;
    std::fs::write("example.png", label.to_png()?)?;
    // sr5900p::printer::Printer::new("192.168.11.16".parse()?).print(&label)?; // to print
    println!("{:.1} mm", label.length_mm());
    Ok(())
}
