mod analyzer;
mod cli;

use analyzer::analyze_tcp_data;
use anyhow::Result;
use argh::FromArgs;
use cli::do_print;
use cli::PrintArgs;
use std::fs;

#[derive(FromArgs, PartialEq, Debug)]
/// Analyze the packet captures
#[argh(subcommand, name = "analyze")]
struct AnalyzeArgs {
    /// the raw dump of the TCP stream while printing
    #[argh(option)]
    tcp_data: String,
}
fn do_analyze(dump_file: &str) -> Result<()> {
    let data = fs::read(dump_file)?;
    analyze_tcp_data(&data)
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum ArgsSubCommand {
    Analyze(AnalyzeArgs),
    Print(PrintArgs),
}
#[derive(Debug, FromArgs)]
/// Reach new heights.
struct Args {
    #[argh(subcommand)]
    nested: ArgsSubCommand,
}

fn main() -> Result<()> {
    // the library reports progress through `log`; show info and above as plain lines
    // so that CLI users still see it (RUST_LOG=debug adds the status read before printing)
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();
    let args: Args = argh::from_env();
    println!("{:?}", args);
    match args.nested {
        ArgsSubCommand::Analyze(args) => do_analyze(&args.tcp_data),
        ArgsSubCommand::Print(args) => do_print(&args),
    }
}
