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

use std::path::{Path, PathBuf};

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
    /// Read this config file instead of the one under $XDG_CONFIG_HOME.
    ///
    /// For checking a config before installing it — `--keys` and `--theme`
    /// against an arbitrary file. A path given here must exist: falling back
    /// to the defaults would answer a question about a different config than
    /// the one asked about.
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,

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
    let config = match cli.config.as_deref() {
        Some(path) => config::Config::from_file(path)?,
        None => config::Config::load()?,
    };

    if cli.keys {
        return print_keys(&config, cli.config.as_deref());
    }
    if cli.theme {
        return print_theme(&config, cli.config.as_deref());
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
fn print_keys(config: &config::Config, source: Option<&Path>) -> Result<()> {
    let maps = config.keymaps();
    print_source(source);

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

/// Where the config being reported on came from.
fn print_source(source: Option<&Path>) {
    match source {
        Some(path) => println!("config: {}", path.display()),
        None => match config::Config::path() {
            Some(path) if path.is_file() => println!("config: {}", path.display()),
            Some(path) => println!(
                "config: {} (not present — showing defaults)",
                path.display()
            ),
            None => println!("config: no home directory — showing defaults"),
        },
    }
}

/// Show the resolved palette, so an override can be checked without squinting
/// at the terminal.
fn print_theme(config: &config::Config, source: Option<&Path>) -> Result<()> {
    let palette = config.palette()?;
    print_source(source);
    print_overlay(config);
    println!("preset: {:?}", config.theme.preset);

    let faint = palette.faint;
    for (slot, colour, faint) in [
        ("fg", palette.fg, false),
        ("bg", palette.bg, false),
        ("dim", palette.dim, faint.dim),
        ("border", palette.border, faint.border),
        ("border_focused", palette.border_focused, false),
        ("heading", palette.heading, false),
        ("accent", palette.accent, false),
        ("cursor_fg", palette.cursor_fg, false),
        ("cursor_bg", palette.cursor_bg, false),
        ("status_fg", palette.status_fg, false),
        ("status_bg", palette.status_bg, false),
        ("gauge_track", palette.gauge_track, faint.gauge_track),
        ("ok", palette.ok, false),
        ("warn", palette.warn, false),
        ("danger", palette.danger, false),
    ] {
        // Which layer won, since three can set the same slot: the preset, the
        // config's [theme.colors], and a colour overlay on top of that.
        let from = if config
            .overlay
            .as_ref()
            .is_some_and(|overlay| overlay.applied.iter().any(|key| key == slot))
        {
            "  (from the overlay)"
        } else if config.theme.colors.contains_key(slot) {
            "  (overridden)"
        } else {
            ""
        };
        // A faint slot has no colour of its own, so printing only the colour
        // would say "reset" and leave the reader wondering why it looks
        // different from fg.
        let attribute = if faint { " + dim attribute" } else { "" };
        println!("  {slot:<16} {}{attribute}{from}", describe_colour(colour));
    }
    Ok(())
}

/// What the colour overlay contributed, and where it was looked for.
///
/// Reported even when there is none, because the first question about a
/// generated overlay that is not taking effect is whether it is in the place
/// hitui actually reads.
fn print_overlay(config: &config::Config) {
    match (&config.overlay, config::Config::overlay_path()) {
        (Some(overlay), _) => {
            println!("overlay: {}", overlay.path.display());
            if !overlay.applied.is_empty() {
                println!("  applied         {}", overlay.applied.join(", "));
            }
            // Named rather than silently dropped: a generated typo is
            // permanent, and nothing else would ever mention it.
            if !overlay.ignored.is_empty() {
                println!(
                    "  not colour slots, ignored: {}",
                    overlay.ignored.join(", ")
                );
            }
        }
        (None, Some(path)) => println!("overlay: none at {}", path.display()),
        (None, None) => println!("overlay: none (no cache directory)"),
    }
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

#[cfg(test)]
mod tests {
    /// The desktop entry names the binary and the icon, and both are strings
    /// that can rot without anything failing to build. `nix flake check` runs
    /// desktop-file-validate over the same file for the spec-level checks;
    /// this is the part only the crate knows.
    #[test]
    fn the_desktop_entry_matches_the_binary() {
        let entry =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/hitui.desktop"))
                .expect("assets/hitui.desktop is present");

        let value = |key: &str| -> Option<String> {
            entry
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .map(str::to_string)
        };

        assert_eq!(value("Exec").as_deref(), Some(env!("CARGO_PKG_NAME")));
        assert_eq!(value("TryExec").as_deref(), Some(env!("CARGO_PKG_NAME")));
        assert_eq!(value("Type").as_deref(), Some("Application"));
        // A TUI launched without a terminal exits instantly, so this is the
        // one key whose absence breaks the entry outright.
        assert_eq!(value("Terminal").as_deref(), Some("true"));
        assert!(value("Icon").is_some_and(|icon| !icon.is_empty()));
    }
}
