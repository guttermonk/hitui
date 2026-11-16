# hitui

A terminal system information browser — a TUI replacement for
[hardinfo2](https://github.com/hardinfo2/hardinfo2).

Reads the kernel's own interfaces (`/proc`, `/sys`) rather than shelling out to
other tools, renders them in a two-pane browser, and can dump the whole report
as JSON for scripting.

**Status:** complete. All four of hardinfo2's modules are implemented — 36
pages across Computer, Devices, Network and Benchmarks. Several go further than
the original; see [Pages](#pages).

## Build and run

```sh
cargo run              # interactive browser
cargo run -- --list    # available pages
cargo run -- --json    # whole report as JSON
cargo run -- --json cpu
cargo run -- cpu       # open the browser on a specific page
cargo run -- --json --bench   # include a benchmark run in the report
```

With Nix:

```sh
nix run .
nix develop            # cargo, clippy, rust-analyzer, plus pciutils/lm_sensors for cross-checking
nix flake check        # builds, runs the test suite, and lints with clippy -D warnings
```

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | move in the sidebar, or scroll the detail pane |
| `h` / `l`, `←` / `→` | switch pane |
| `Tab` | toggle pane focus |
| `1`–`9` | jump straight to a page |
| `/` | filter fields on the current page |
| `Enter` | run the current page's action, where it has one (Benchmarks) |
| `Esc` | clear the filter, or quit |
| `g` / `G`, `Home` / `End` | top / bottom |
| `PgUp` / `PgDn` | scroll a screen |
| `r` | re-collect the current page |
| `?` | key help |
| `q`, `Ctrl-C` | quit |

## Architecture

Collectors never touch the terminal. Each one returns a tree of `Node`s —
label, optional typed `Value`, children — and both the TUI and the JSON report
consume that tree generically. Adding a page is one file plus one line:

```rust
// src/collect/battery.rs
impl Collector for Battery {
    fn id(&self) -> &'static str { "battery" }       // JSON key, CLI name
    fn title(&self) -> &'static str { "Battery" }    // sidebar entry
    fn group(&self) -> Group { Group::Devices }      // sidebar grouping
    fn collect(&mut self) -> Result<Vec<Node>> { … }
    fn refresh_interval(&self) -> Option<Duration> { … }  // None = static
}
```

Register it in `collect::registry()` and it appears in the sidebar, the JSON
report, `--list`, and the digit shortcuts with no UI changes.

Values are typed (`Bytes`, `Hertz`, `Percent`, `Gauge`, …) rather than
pre-formatted, so the JSON stays machine-readable and unit formatting is purely
a display concern. Missing data is modelled as missing — `Node::push_opt` drops
a field rather than printing "unknown", since which `/sys` files exist varies by
kernel config, hardware, and privilege.

Gauges carry a `Polarity`, because a percentage alone cannot be coloured: 95 %
disk used and 95 % battery remaining are the same number meaning opposite
things. `Value::gauge` measures consumption (disk used, SSD endurance spent) and
reddens as it fills; `Value::reserve_gauge` measures a reserve (battery charge,
SSD spare blocks, capacity against design) and reddens as it empties.

| File | Role |
| --- | --- |
| `src/model.rs` | `Node` / `Value`, unit formatting |
| `src/sysfs.rs` | `/proc` and `/sys` parsing helpers |
| `src/probe.rs` | optional external tools — discovery, timeout, capture |
| `src/collect/` | one module per page |
| `src/app.rs` | state, key handling, row flattening, field filter |
| `src/ui.rs` | rendering — knows nothing about data sources |

### Where data comes from

hitui reads the kernel's own interfaces for everything it can: `/proc`, `/sys`,
and syscalls. That keeps it a single binary with no runtime dependencies, which
matters when you are using it to work out why a machine is broken.

A few facts are genuinely not there to be read. Drive health lives behind ATA
pass-through, GPU renderer strings behind a GL context, and vendor *names* for
PCI and USB devices in a `pci.ids` table that many systems — NixOS among them —
do not ship at any fixed path. For those, hitui parses the output of the
established tool **if it happens to be installed**, and labels the section with
where the data came from:

```
0000:00:02.0  VGA compatible controller  via lspci
  Vendor      Intel Corporation [8086]
  Device      Kaby Lake-DT GT2 [HD Graphics P630] [591d]
```

Three rules hold this together:

1. **No tool is required.** Every collector produces a useful page from kernel
   interfaces alone. Without `lspci`, the PCI page still lists all 24 devices
   with their numeric IDs and decoded base classes — it just says
   `Device names: unavailable — install pciutils for vendor and device names`
   rather than silently showing blanks.
2. **Provenance is recorded, not hidden.** `Node::from("lspci")` tags a section;
   the TUI renders it beside the heading and `--json` emits it as `"source"`.
   A number you read yourself and a number you scraped are not the same kind of
   fact.
3. **A tool that hangs is a tool that gets killed.** `probe::run` drains stdout
   on a separate thread and kills the child after three seconds. `smartctl`
   against a dying disk can block indefinitely, and a page missing a column
   beats a page that never draws.

Optional tools in use or planned: `lspci`, `lsusb`, `smartctl`, `vulkaninfo`,
`glxinfo`, `dmidecode`.

Prefer a tool's structured output over its human-readable report where one
exists. `smartctl --json` is parsed as JSON rather than scraped, which also
exposes its failures properly: a permission error arrives *inside* the JSON as
a message with `severity: "error"`, not as empty output, so the result has to
be inspected rather than trusted. That is how the Storage page can distinguish
"SMART needs root" from "this drive reports no health data".

## Pages

Grouped as hardinfo2 groups them, so anyone coming from the GTK app finds
things where they expect. Notes name the data source each page needs.

Where hitui goes further than hardinfo2:

- **Memory Devices decodes SPD directly**, so part numbers, rank geometry,
  speed grade, ECC width and manufacture date all work **without root**.
  hardinfo2 reads this through `dmidecode`, which cannot.
- **Storage follows a partition to its real use.** An encrypted root reads as
  `LUKS2 → luks-…, mounted at /, /home`, not the technically-true but useless
  "not mounted"; encrypted swap is named as swap.
- **USB is a port tree**, nesting each device under its hub, with every
  interface's class and bound driver. `lsusb` needs `-t` for the topology and
  then loses the names.
- **Monitors decode the full EDID** — manufacturer, preferred mode, physical
  size, pixel density, manufacture date.
- **Values that only mean something decoded, are decoded**: kernel taint
  `4097` as "proprietary module loaded, out-of-tree module loaded"; hardening
  sysctls as "1 — hidden from unprivileged users"; TCP state `0A` as `LISTEN`.
- **Security** covers hardening sysctls, lockdown, module signing and
  IOMMU/passthrough readiness, which hardinfo2 does not report.

Still planned beyond parity: bluetooth, package repositories, RAID/LVM volume
topology.

Two deliberate omissions:

**Module descriptions.** They live in the `.ko` files, readable only by
`modinfo`, and the cost is decompressing every module on disk — batching all
213 modules of a running kernel into one call still measured 32 seconds. That
is a page that hangs. Adding them back needs background collection or an
on-disk cache, not a longer timeout.

**Environment variable values that look like credentials** are replaced with
their length. hardinfo2 prints everything, but `--json` output is meant to be
pasteable into a bug report, and an environment routinely holds API tokens.
"Set, and 40 characters long" answers the debugging question without putting
the secret in a paste.

### Computer

- [x] **Summary** — machine identity, installed hardware, capacity
- [x] **Operating System** — `/etc/os-release`, kernel, init, container detection
- [x] **Security** — LSMs, lockdown, hardening sysctls, module signing, IOMMU/KVM
- [x] **Kernel Modules** — `/proc/modules`, `/sys/module`, decoded taint flags
- [x] **Boots** — the journal’s boot records via `journalctl`
- [x] **Languages** — active locale, system default, installed locales
- [x] **Memory Usage** — `/proc/meminfo`, `/proc/sys/vm`
- [x] **Filesystems** — `/proc/self/mounts`, `statvfs`
- [x] **Display** — session type, compositor, active outputs, backlight
- [x] **Environment Variables** — with credential-shaped values redacted
- [x] **Development** — toolchain versions, probing only what is installed
- [x] **Users** / **Groups** — `/etc/passwd`, `/etc/group`, primary groups resolved

### Devices

- [x] **Processor** — `/proc/cpuinfo`, cache and topology from `/sys`, CPUID for hypervisor identity
- [x] **Graphics Processors** — DRM cards, connectors, OpenGL renderer via `glxinfo`
- [x] **Monitors** — DRM connectors with full EDID decoding
- [x] **PCI Devices** — `/sys/bus/pci`, names via `lspci` when installed
- [x] **USB Devices** — `/sys/bus/usb` as a port tree, names via `lsusb` when needed
- [x] **Firmware** — UEFI, Secure Boot via efivarfs, TPM version and PCR banks
- [x] **Printers** — CUPS queues via `lpstat`
- [x] **Battery** — `/sys/class/power_supply`, including wear against design capacity
- [x] **Sensors** — `/sys/class/hwmon` (temperature, fan, voltage, current, power)
- [x] **Input Devices** — `/proc/bus/input/devices`, class and bound driver
- [x] **Storage** — `/sys/block`, partition use through LUKS/LVM, SMART via `smartctl`
- [x] **System DMI** — SMBIOS identity tables from `/sys/class/dmi/id`
- [x] **Memory Devices** — SPD decoded from EEPROM, unprivileged
- [x] **Device Tree** — `/proc/device-tree`, distinguishing a real tree from an EFI stub
- [x] **Resources** — interrupts, I/O ports, memory ranges, DMA

### Network

- [x] **Interfaces** — `/sys/class/net`, addresses via `ip -j` with an IPv6 fallback
- [x] **IP Connections** — `/proc/net/{tcp,udp}`, grouped by socket state
- [x] **Routing Table** / **ARP Table** — `/proc/net/route`, `ipv6_route`, `arp`
- [x] **DNS Servers** — `resolv.conf`, with stub-resolver detection
- [x] **Statistics** — `/proc/net/dev` and per-protocol counters from `snmp`
- [x] **Shared Directories** — NFS exports and Samba shares

### Benchmarks

Nothing runs until you ask. Open the page and press Enter, or use
`hitui --json --bench` for a scripted run. Results are in real physical units
rather than scores, best of three passes each, and the suite takes about
eighteen seconds.

- [x] **Processor** — Blowfish and SHA-256 (single thread and all cores), Zlib round trip, recursive Fibonacci, N-Queens
- [x] **Floating point** — radix-2 FFT in MFLOP/s, sphere raytracing
- [x] **Memory** — copy bandwidth, and latency at each level of the cache hierarchy
- [x] **Storage** — sequential write with fsync
- [ ] GPU throughput — needs a graphics context a terminal cannot create
- [ ] Shared result comparison — would mean uploading your hardware profile to a third party

## Deviations

Places where hitui deliberately departs from hardinfo2:

- **Field search.** `/` filters fields on the current page, keeping enclosing
  sections for context. hardinfo2 has no search at all, and a CPU flag list is
  180 entries long.
- **JSON reports.** `--json` emits the same tree the TUI renders, so hitui is
  usable from scripts. hardinfo2 exports HTML and plain text intended for
  reading, not parsing.
- **Benchmarks will not auto-run on page open.** In hardinfo2, selecting a
  benchmark page starts it. Here it will require an explicit keypress — opening
  a page should never peg your CPU for thirty seconds.
- **No root required, and no silent degradation.** Pages that need privilege
  for part of their data show what they can and say what they could not read.

## Not in scope

**hitui is not a system monitor.** It answers "what is this machine and what
state is it in", not "what is it doing right now". No utilisation gauges, no
load history, no graphs, no per-process view — use btop, htop, or tuimon for
that. Benchmarks are in scope, because identifying hardware includes measuring
what it can do.

The line is not "nothing live". A few figures are read fresh on every tick
because their current value *is* the fact you came for: memory in use, disk
free space, sensor readings, battery charge, link state. hardinfo2 refreshes
its Summary memory figure every second for the same reason. What's excluded is
*history* and *rates* — the things a monitor exists to show.
