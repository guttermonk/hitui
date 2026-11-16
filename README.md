# hitui

A terminal system information browser — a TUI replacement for
[hardinfo2](https://github.com/hardinfo2/hardinfo2).

Reads the kernel's own interfaces (`/proc`, `/sys`) rather than shelling out to
other tools, renders them in a two-pane browser, and can dump the whole report
as JSON for scripting.

**Status:** complete. All four of hardinfo2's modules are implemented — 39
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
cargo run -- --keys    # the resolved keybindings
cargo run -- --theme   # the resolved palette
cargo run -- -c ./try.toml --keys   # check a config before installing it
```

With Nix:

```sh
nix run .
nix develop            # cargo, clippy, rust-analyzer, plus pciutils/lm_sensors for cross-checking
nix flake check        # builds, runs the test suite, and lints with clippy -D warnings
```

A desktop entry is at [`assets/hitui.desktop`](assets/hitui.desktop), installed
by the Nix package; copy it to `~/.local/share/applications/` otherwise. It sets
`Terminal=true`, since a TUI launched without a tty exits immediately.

Its `Icon=hardinfo` is a name, not a file: icon names resolve through whatever
icon theme is in use, so this needs no hardinfo2 installed. Papirus and its
derivatives ship a `hardinfo` icon, and it is the closest match to what this is.
Breeze and current Adwaita ship none of the old freedesktop application names,
so there the entry falls back to a generic icon — change `Icon=` to taste, or to
`computer`, which is the one name every theme does carry.

## Keys

Defaults — all of them rebindable, see [Configuration](#configuration).

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
| `PgUp` / `PgDn`, `Ctrl-D` / `Ctrl-U` | scroll a screen |
| `r` | re-collect the current page |
| `?` | key help |
| `q`, `Ctrl-C` | quit |

## Configuration

`~/.config/hitui/config.toml` (or `$XDG_CONFIG_HOME/hitui/config.toml`). It is
optional; without it you get the table above and the terminal's own colours.
[`config.example.toml`](config.example.toml) is the annotated full set, every
value at its default — copy it and edit.

Unknown keys are errors rather than silent no-ops, and the config is parsed
before the terminal enters raw mode, so a typo reports itself legibly instead
of scrolling past a redraw.

### Themes

```toml
[theme]
preset = "gruvbox-dark"   # terminal (default), catppuccin-mocha,
                          # catppuccin-latte, gruvbox-dark, nord, dracula,
                          # one-dark, solarized-dark

[theme.colors]            # override any slot of the preset
border_focused = "#fabd2f"
accent = "bright-cyan"    # hex, or an ANSI colour name
```

The default preset is `terminal`: every slot resolves to `Reset`, so hitui
inherits whatever your terminal is themed with and nothing fights your
colourscheme. The fifteen slots are `fg`, `bg`, `dim`, `border`,
`border_focused`, `heading`, `accent`, `cursor_fg`, `cursor_bg`, `status_fg`,
`status_bg`, `gauge_track`, `ok`, `warn`, `danger`.

Under `terminal`, the three quiet slots — `dim`, `border`, `gauge_track` — are
drawn with the terminal's **dim attribute** on its own foreground rather than
in a colour. No colour works there without knowing the background: ANSI 8 is a
mid-grey on a dark theme and all but invisible on a light one (1.05:1 contrast
against rose-pine-dawn, where 3:1 is the floor for legible text), and ANSI 7 is
exactly what `reset` resolves to, so `dim` would come out the colour of the
text it sits behind. Giving any of the three a colour turns the attribute off
for that slot. A terminal that ignores SGR 2 draws them at normal weight —
unsubordinated but legible, which beats invisible.

Colour names are normalised the way ratatui normalises its own — lowercased,
spaces/hyphens/underscores dropped, `bright` read as `light`, `grey` as `gray`
— so `bright-red`, `brightred` and `light_red` are one spelling and a value
copied from another ratatui application means here what it meant there. Two
words are refused rather than guessed, because they parse in both vocabularies
with a different colour each:

| Word | Why | Write instead |
| --- | --- | --- |
| `white` | ANSI 7 to some tools, ANSI 15 to ratatui | `gray`, or `bright-white` |
| `bright-gray` | reads as ANSI 8; ratatui folds it to white at ANSI 15 | `dark-gray`, or `bright-white` |

`hitui --theme` prints the resolved result and marks which layer won for each
slot, which is the quick way to tell a bad hex code from a slot name you
misremembered. `hitui -c <path> --theme` does it for a config you have not
installed yet.

### Colour overlay

Tools that recolour a desktop from the current wallpaper — wallust, pywal —
re-extract a palette on every wallpaper change, and have no business owning the
file that also holds your keybindings. So hitui reads an optional second file
holding colours only:

```
$XDG_CACHE_HOME/wallust/colors-hitui.toml
~/.cache/wallust/colors-hitui.toml      # when XDG_CACHE_HOME is unset
$HITUI_COLORS_OVERLAY                   # overrides both
```

Its shape is a **top-level `[colors]` table** — not `[theme.colors]` — matching
every other file in such a template directory:

```toml
[colors]
dim = "#6c7086"
border = "#45475a"
```

Entries merge over `[theme.colors]` key by key, so an overlay naming only
`border` leaves the other fourteen alone. hitui is a fresh process every
launch, so the next run already has the new colours and there is no reload hook
to arrange.

A missing, unreadable or malformed overlay means "use the config", silently —
that is the normal state for any palette that is not wallpaper-derived, and it
also covers the file being half-written when hitui starts. A key that is *not*
a colour slot is dropped rather than fatal, unlike the same key in
`[theme.colors]`: a wallpaper must not be able to stop you reading your
hardware. It is not dropped quietly, though — `--theme` names it, because a
generated typo is permanent and nothing else would ever mention it.

The same seam, under `BLUETUI_COLORS_OVERLAY`, exists in
[bluetui](https://github.com/guttermonk/bluetui); the two are deliberately the
same shape so one generator can feed both.

### Layout

Pages whose fields are narrow are laid out in columns rather than one tall
strip. Memory Usage is 90 lines of 30-character rows; in one column it scrolls
while three quarters of the pane sits empty, and in three it fits on one
screen:

```
Overview                       All /proc/meminfo              All /proc/meminfo (cont.)
  Total             31.07 GiB    MemTotal          31.07 GiB     SecPageTables     0 B
  Used              8.00 GiB     MemFree           2.07 GiB      NFS_Unstable      0 B
  Available         23.07 GiB    MemAvailable      23.07 GiB     Bounce            0 B
```

Nothing is configured per page. Two measurements decide it:

- **A page is only split if it does not fit the screen in one column.** Columns
  are there to stop a page overflowing; cutting up a page that already fits
  costs readability and buys nothing. Resize the terminal and the columns come
  and go.
- **A column has to make the page shorter to be worth taking.** Each candidate
  count is laid out and the heights compared, which is what keeps pages of long
  values in one wide column — wrapping the CPU flag list or an environment
  variable into narrow columns makes the page *taller*, so they never qualify.

Breaks prefer section boundaries, so Sensors splits between chips rather than
through one. A section too tall to fit a column flows into the next, with its
heading repeated and marked `(cont.)`. Gauges set a floor on how narrow a
column may get, since a bar and its caption cannot wrap.

`[layout] max_columns` caps it; `1` keeps every page in a single column.

### Keybindings

Four layers, applied in order. `set` replaces an action's keys, `extra` adds to
them, `unbind` removes:

```toml
[keys.browser]
# Colemak-DH home row instead of hjkl
set = [
    ["MoveDown", "e"],
    ["MoveUp", "i"],
    ["FocusSidebar", "n"],
    ["FocusDetail", "o"],
]
extra = [["Refresh", "F5"]]
unbind = ["ctrl+d"]
```

`set` replaces only the *letter* keys for that action — the arrows survive, so
rebinding navigation can never leave you with no way to move. `extra` displaces
any binding it collides with, including a prefix of one. `unbind` is
prefix-based and applied last, so removing a prefix removes the sequences under
it.

`hitui --keys` prints what the layers actually produced, and names any action
that ended up unreachable — the one mistake these layers can make that is
otherwise silent. The footer and the `?` overlay are generated from the same
resolved keymap, so they describe your bindings rather than the defaults.

Sections are `[keys.browser]`, `[keys.search]` and `[keys.help]`, matching the
three modes.

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
| `src/config/` | config file, theme presets, palette resolution |
| `src/keys/` | key combos, actions, the four binding layers |
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

Optional tools in use or planned: `lspci`, `lsusb`, `smartctl`, `fwupdmgr`,
`busctl`, `vulkaninfo`, `glxinfo`, `dmidecode`.

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
- **Volumes render the stack as a stack.** A btrfs filesystem on a LUKS
  mapping on a partition on an NVMe drive is four layers, and an LV on a VG on
  a PV on a LUKS mapping is six; from either end that reads as a list of
  unrelated block devices. Read entirely from sysfs, which matters here more
  than elsewhere: `lvs` and `vgs` want `/dev/mapper/control` and report almost
  nothing as an ordinary user, while `/sys/block/*/dm/uuid` identifies the same
  volumes with no privilege at all. btrfs RAID profiles come from the
  block-group flag word rather than `btrfs filesystem df`, so a filesystem with
  data `single` and metadata `raid1` says which half survives one failure.
- **Repositories answer "verified how", not just "where from"**. A repository
  is a list of things the machine will install as root, and the managers all
  spell the check differently — `gpgcheck`, `gpg-verify`, `Signed-By`,
  `SigLevel`, `require-sigs`. They are normalised to one row, so an APT source
  carrying `trusted=yes` or a `.repo` with `gpgcheck=0` states it rather than
  leaving a blank. Read from the managers' own files: asking a package manager
  for a listing can send it to refresh metadata over the network, which opening
  a page must not do.
- **Bluetooth unpacks what BlueZ reports as numbers**: HCI version 8 as
  "Bluetooth 4.2", the class of device 7078156 as "Computer / Laptop" plus the
  services behind it, service UUIDs as profile names, and bluetoothd's own
  version out of the virtual modalias it hides in. The radio's rfkill state is
  read from the kernel, so a blocked adapter explains itself even when
  bluetoothd is not running.
- **Firmware says whether it is current**, not just which version is running:
  the installed build's release date, the newest build's name and date, and how
  many releases separate them. A version number alone cannot answer "am I
  behind", and it is how this machine's UEFI dbx turned out to be four
  revocation lists out of date. hardinfo2 reports the version and stops.

Nothing is outstanding beyond parity. Two things stay deliberately out of
scope; see [Not in scope](#not-in-scope) and the Benchmarks notes.

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
- [x] **Repositories** — every configured package source, and how each is verified
- [x] **Development** — toolchain versions, probing only what is installed
- [x] **Users** / **Groups** — `/etc/passwd`, `/etc/group`, primary groups resolved

### Devices

- [x] **Processor** — `/proc/cpuinfo`, cache and topology from `/sys`, CPUID for hypervisor identity
- [x] **Graphics Processors** — DRM cards, connectors, OpenGL renderer via `glxinfo`
- [x] **Monitors** — DRM connectors with full EDID decoding
- [x] **PCI Devices** — `/sys/bus/pci`, names via `lspci` when installed
- [x] **USB Devices** — `/sys/bus/usb` as a port tree, names via `lsusb` when needed
- [x] **Firmware** — UEFI, Secure Boot via efivarfs, TPM version and PCR banks,
      and firmware currency via `fwupdmgr`
- [x] **Bluetooth** — adapters, paired devices and radio blocks, via BlueZ
- [x] **Printers** — CUPS queues via `lpstat`
- [x] **Battery** — `/sys/class/power_supply`, including wear against design capacity
- [x] **Sensors** — `/sys/class/hwmon` (temperature, fan, voltage, current, power)
- [x] **Input Devices** — `/proc/bus/input/devices`, class and bound driver
- [x] **Storage** — `/sys/block`, partition use through LUKS/LVM, SMART via `smartctl`
- [x] **Volumes** — MD arrays, LVM groups, btrfs profiles, and the full stack beneath each
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
- **Pages use the width they are given.** hardinfo2 is a two-column name/value
  table however wide the window gets. Pages here are split into columns when
  that stops them scrolling — see [Layout](#layout).
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

## Licence

GPL-3.0-or-later; see [LICENSE](LICENSE).

hitui is a from-scratch reimplementation rather than a port — none of
hardinfo2's GPL-2.0 source is used, and the desktop entry reuses its icon by
*name* only, which is a string resolved through your icon theme and not
artwork carried in this repo. The licence was therefore a free choice, made to
match the sibling TUIs rather than to discharge an obligation.
