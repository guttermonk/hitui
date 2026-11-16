//! hitui — a terminal system information browser.

mod app;
mod bench;
mod collect;
mod config;
mod keys;
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

    /// Print the resolved keybindings and exit.
    ///
    /// Shows what a config actually produced, and names any action that ended
    /// up with no key — which `base`, `set` and `unbind` can all cause and
    /// which is otherwise silent.
    #[arg(long)]
    keys: bool,

    /// Print the resolved theme colours and exit.
    #[arg(long)]
    theme: bool,

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

    // Loaded before anything draws, so a malformed config reports itself to a
    // normal terminal rather than to one already in raw mode.
    let config = config::Config::load()?;

    if cli.keys {
        return print_keys(&config);
    }
    if cli.theme {
        return print_theme(&config);
    }

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

    let mut app = App::with_config(&config);
    if let Some(first) = cli.categories.first() {
        app.select_page_by_id(first);
    }

    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

/// Show what the config's keybinding layers actually resolved to.
fn print_keys(config: &config::Config) -> Result<()> {
    let maps = config.keymaps();

    match config::Config::path() {
        Some(path) if path.is_file() => println!("config: {}", path.display()),
        Some(path) => println!(
            "config: {} (not present — showing defaults)",
            path.display()
        ),
        None => println!("config: no home directory — showing defaults"),
    }

    for (mode, keymap) in maps.iter() {
        println!("\n[{}]", mode.name());
        for action in keys::Action::ALL {
            let combos = keymap.combos_for(action);
            if combos.is_empty() {
                continue;
            }
            let rendered: Vec<String> = combos.iter().map(|c| c.to_string()).collect();
            println!(
                "  {:<18} {:<22} {}",
                rendered.join(", "),
                format!("{action:?}"),
                action.describe()
            );
        }
    }

    // The failure this subcommand exists for: a layer that silently left an
    // action with no key at all.
    let unreachable = maps.unreachable();
    if !unreachable.is_empty() {
        println!("\nunreachable in the browser — no key is bound to these:");
        for action in unreachable {
            println!("  {action:?}  ({})", action.describe());
        }
    }
    Ok(())
}

/// Show the resolved palette, so an override can be checked without squinting
/// at the terminal.
fn print_theme(config: &config::Config) -> Result<()> {
    let palette = config.palette()?;
    println!("preset: {:?}", config.theme.preset);

    for (slot, colour) in [
        ("fg", palette.fg),
        ("bg", palette.bg),
        ("dim", palette.dim),
        ("border", palette.border),
        ("border_focused", palette.border_focused),
        ("heading", palette.heading),
        ("accent", palette.accent),
        ("cursor_fg", palette.cursor_fg),
        ("cursor_bg", palette.cursor_bg),
        ("status_fg", palette.status_fg),
        ("status_bg", palette.status_bg),
        ("gauge_track", palette.gauge_track),
        ("ok", palette.ok),
        ("warn", palette.warn),
        ("danger", palette.danger),
    ] {
        let overridden = if config.theme.colors.contains_key(slot) {
            "  (overridden)"
        } else {
            ""
        };
        println!("  {slot:<16} {}{overridden}", describe_colour(colour));
    }
    Ok(())
}

fn describe_colour(colour: ratatui::style::Color) -> String {
    match colour {
        ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        ratatui::style::Color::Reset => "reset (the terminal's own)".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
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
