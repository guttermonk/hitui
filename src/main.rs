//! hitui — a terminal system information browser.

mod app;
mod bench;
mod collect;
mod model;
mod probe;
mod sysfs;
mod ui;

use anyhow::{Result, bail};
use clap::Parser;
use serde_json::{Map, Value as Json};

use crate::app::App;

#[derive(Parser)]
#[command(
    name = "hitui",
    version,
    about = "Browse system information in the terminal",
    after_help = "With no arguments, hitui starts the interactive browser."
)]
struct Cli {
    /// Print a JSON report to stdout instead of starting the browser
    #[arg(long)]
    json: bool,

    /// List the available categories and exit
    #[arg(long)]
    list: bool,

    /// Run the benchmark suite and include its results in the report.
    ///
    /// Off by default because it takes about fifteen seconds and loads every
    /// core; nothing is benchmarked unless this is passed.
    #[arg(long)]
    bench: bool,

    /// Categories to report on, by id (default: all)
    #[arg(value_name = "CATEGORY")]
    categories: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        for collector in collect::registry() {
            println!("{:<12} {}", collector.id(), collector.title());
        }
        return Ok(());
    }

    // Reject unknown ids up front: silently reporting on nothing is worse than
    // saying which name was wrong.
    let known: Vec<&'static str> = collect::registry().iter().map(|c| c.id()).collect();
    if let Some(unknown) = cli.categories.iter().find(|c| !known.contains(&c.as_str())) {
        bail!("unknown category `{unknown}` (known: {})", known.join(", "));
    }

    if cli.json {
        return print_json(&cli.categories, cli.bench);
    }
    if cli.bench {
        bail!("--bench only applies to --json; in the browser, open Benchmarks and press Enter");
    }

    let mut app = App::new();
    if let Some(first) = cli.categories.first() {
        app.select_page_by_id(first);
    }

    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

fn print_json(filter: &[String], bench: bool) -> Result<()> {
    let mut report = Map::new();
    for mut collector in collect::registry() {
        if !filter.is_empty() && !filter.iter().any(|f| f == collector.id()) {
            continue;
        }
        // The benchmarks page reads shared state that only the interactive
        // runner fills, so a scripted run has to do the work itself.
        if bench && collector.id() == "benchmarks" {
            let nodes = collect::benchmarks::run_now();
            report.insert(collector.id().to_string(), serde_json::to_value(nodes)?);
            continue;
        }
        // A failing collector is reported in-band rather than aborting the
        // whole report, so one unreadable subsystem does not cost you the rest.
        let value = match collector.collect() {
            Ok(nodes) => serde_json::to_value(nodes)?,
            Err(err) => serde_json::json!({ "error": format!("{err:#}") }),
        };
        report.insert(collector.id().to_string(), value);
    }
    println!("{}", serde_json::to_string_pretty(&Json::Object(report))?);
    Ok(())
}
