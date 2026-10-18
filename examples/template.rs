//! Render a label from a template as a library user would.
//! cargo run --example template -- <template.toml> <tape mm> key=value...
use anyhow::Context;
use anyhow::Result;
use sr5900p::label::save_preview;
use sr5900p::label::template::Template;
use sr5900p::Tape;
use std::collections::HashMap;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let template = Template::load(args.next().context("template path")?)?;
    let tape = Tape::from_mm(args.next().context("tape mm")?.parse()?)?;
    let fields: HashMap<String, String> = args
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    let td = template.render(&fields, tape)?;
    save_preview("example.png", &td)?;
    // sr5900p::printer::Printer::new("192.168.11.16".parse()?).print(&td)?; // to print
    println!("{} x {} px", td.width, td.height);
    Ok(())
}
