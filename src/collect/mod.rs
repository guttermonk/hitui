//! Collectors: one per page of the report.
//!
//! Pages are organised into the same four groups hardinfo2 uses, so anyone
//! coming from the GTK app finds things where they expect them. To add a page,
//! implement [`Collector`] in a new module and register it in [`registry`];
//! the sidebar and the JSON report both pick it up with no further changes.

use std::time::Duration;

use anyhow::Result;

use crate::model::Node;

pub mod battery;
pub mod cpu;
pub mod dmi;
pub mod filesystems;
pub mod firmware;
pub mod interfaces;
pub mod memory;
pub mod modules;
pub mod pci;
pub mod security;
pub mod sensors;
pub mod storage;
pub mod summary;
pub mod usb;

/// Top-level sidebar grouping, mirroring hardinfo2's modules.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Group {
    Computer,
    Devices,
    Network,
    Benchmarks,
}

impl Group {
    /// Display order in the sidebar.
    pub const ALL: [Group; 4] = [
        Group::Computer,
        Group::Devices,
        Group::Network,
        Group::Benchmarks,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Computer => "Computer",
            Group::Devices => "Devices",
            Group::Network => "Network",
            Group::Benchmarks => "Benchmarks",
        }
    }
}

/// A source of system information.
pub trait Collector {
    /// Stable machine-readable key. Used as the JSON object key and for
    /// `--json <ID>`, so treat it as part of the interface.
    fn id(&self) -> &'static str;

    /// Human-readable name, shown in the sidebar.
    fn title(&self) -> &'static str;

    /// Which sidebar group this page belongs under.
    fn group(&self) -> Group;

    /// Gather the current state.
    ///
    /// Takes `&mut self` so collectors that need to diff against a previous
    /// sample (CPU utilisation from /proc/stat, network counters) can keep
    /// state between calls.
    fn collect(&mut self) -> Result<Vec<Node>>;

    /// How often to re-collect while the page is open.
    ///
    /// `None` — the default — means the data is static and is only re-read when
    /// the user asks for a refresh.
    fn refresh_interval(&self) -> Option<Duration> {
        None
    }
}

/// Every collector. Sidebar order is by [`Group::ALL`], then by registration
/// order within each group.
pub fn registry() -> Vec<Box<dyn Collector>> {
    let mut collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(summary::Summary),
        Box::new(memory::Memory),
        Box::new(filesystems::Filesystems),
        Box::new(modules::Modules),
        Box::new(security::Security),
        Box::new(cpu::Cpu),
        Box::new(pci::Pci),
        Box::new(usb::Usb),
        Box::new(storage::Storage),
        Box::new(dmi::Dmi),
        Box::new(firmware::Firmware),
        Box::new(sensors::Sensors),
        Box::new(battery::Battery),
        Box::new(interfaces::Interfaces),
    ];
    collectors.sort_by_key(|c| Group::ALL.iter().position(|g| *g == c.group()));
    collectors
}
