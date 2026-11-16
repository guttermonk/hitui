//! Collectors: one per page of the report.
//!
//! Pages are organised into the same four groups hardinfo2 uses, so anyone
//! coming from the GTK app finds things where they expect them. To add a page,
//! implement [`Collector`] in a new module and register it in [`registry`];
//! the sidebar and the JSON report both pick it up with no further changes.

use std::time::Duration;

use anyhow::Result;

use crate::model::Node;

pub mod accounts;
pub mod battery;
pub mod benchmarks;
pub mod bluetooth;
pub mod boots;
pub mod connections;
pub mod cpu;
pub mod development;
pub mod devicetree;
pub mod display;
pub mod dmi;
pub mod dns;
pub mod edid;
pub mod environment;
pub mod filesystems;
pub mod firmware;
pub mod fwupd;
pub mod gpu;
pub mod input;
pub mod interfaces;
pub mod locales;
pub mod memory;
pub mod memory_devices;
pub mod modules;
pub mod monitors;
pub mod netstat;
pub mod netutil;
pub mod os;
pub mod pci;
pub mod printers;
pub mod repos;
pub mod resources;
pub mod routes;
pub mod security;
pub mod sensors;
pub mod shares;
pub mod storage;
pub mod summary;
pub mod usb;
pub mod volumes;

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

    /// Label for an action the user can trigger with Enter, if this page has
    /// one. `None` — the default — means the page only reads.
    ///
    /// Exists for Benchmarks, which is the one page that *does* something
    /// rather than reporting something, and must never do it unasked.
    fn action(&self) -> Option<&'static str> {
        None
    }

    /// Perform that action. Must return promptly: anything long-running
    /// belongs on a worker thread, or the interface stops redrawing.
    fn activate(&mut self) {}

    /// Take whatever this collector needs from the config.
    ///
    /// Almost nothing does — a collector reports what it finds and has no
    /// settings — so the default is to ignore it. Threaded through rather
    /// than read from a global, for the same reason the palette is: a test
    /// has to be able to collect under a configuration of its choosing.
    fn configure(&mut self, _config: &crate::config::Config) {}
}

/// Every collector. Sidebar order is by [`Group::ALL`], then by registration
/// order within each group.
pub fn registry() -> Vec<Box<dyn Collector>> {
    let mut collectors: Vec<Box<dyn Collector>> = vec![
        Box::new(summary::Summary),
        Box::new(os::OperatingSystem),
        Box::new(memory::Memory),
        Box::new(filesystems::Filesystems),
        Box::new(modules::Modules),
        Box::new(boots::Boots),
        Box::new(development::Development),
        Box::new(repos::Repositories),
        Box::new(security::Security),
        Box::new(locales::Locales),
        Box::new(environment::Environment),
        Box::new(accounts::Users),
        Box::new(accounts::Groups),
        Box::new(display::Display),
        Box::new(cpu::Cpu),
        Box::new(pci::Pci),
        Box::new(usb::Usb),
        Box::new(storage::Storage),
        Box::new(volumes::Volumes),
        Box::new(dmi::Dmi),
        Box::new(firmware::Firmware),
        Box::new(gpu::Gpu),
        Box::new(monitors::Monitors),
        Box::new(input::Input),
        Box::new(bluetooth::Bluetooth),
        Box::new(printers::Printers),
        Box::new(memory_devices::MemoryDevices),
        Box::new(devicetree::DeviceTree),
        Box::new(resources::Resources),
        Box::new(sensors::Sensors),
        Box::new(battery::Battery),
        Box::new(interfaces::Interfaces),
        Box::new(connections::Connections),
        Box::new(routes::Routes),
        Box::new(routes::Arp),
        Box::new(dns::Dns),
        Box::new(netstat::NetStat),
        Box::new(shares::Shares),
        Box::new(benchmarks::Benchmarks::default()),
    ];
    collectors.sort_by_key(|c| Group::ALL.iter().position(|g| *g == c.group()));
    collectors
}
