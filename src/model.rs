//! The data model every collector produces.
//!
//! Collectors never touch the terminal. They return a tree of [`Node`]s, which
//! the TUI renders generically and `--json` serialises verbatim. Adding a page
//! means adding a collector, not touching the UI.

use std::fmt;

use serde::Serialize;

/// One labelled row in a report: either a leaf field, or a group with children.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Where this came from, when it was not the kernel.
    ///
    /// Only set on nodes whose data came from an external tool, so a reader can
    /// tell a figure hitui read itself from one it parsed out of `smartctl`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    /// A leaf: `label: value`.
    pub fn field(label: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            label: label.into(),
            value: Some(value.into()),
            source: None,
            children: Vec::new(),
        }
    }

    /// A heading with nested nodes under it.
    pub fn group(label: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            label: label.into(),
            value: None,
            source: None,
            children,
        }
    }

    /// Record that this node's data came from an external tool.
    pub fn from(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Append a leaf, skipping it entirely when the value is absent.
    ///
    /// Most of /proc and /sys is optional depending on kernel config and
    /// hardware, so collectors lean on this to stay quiet about what they
    /// could not read rather than printing a wall of "unknown".
    pub fn push_opt(&mut self, label: impl Into<String>, value: Option<impl Into<Value>>) {
        if let Some(v) = value {
            self.children.push(Node::field(label, v));
        }
    }

    pub fn push(&mut self, node: Node) {
        self.children.push(node);
    }

    pub fn is_group(&self) -> bool {
        self.value.is_none()
    }
}

/// A typed field value.
///
/// Typing these rather than pre-formatting strings keeps the JSON report
/// machine-readable and leaves unit formatting as a display concern.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum Value {
    Text(String),
    Int(i64),
    /// Raw byte count, rendered with binary prefixes.
    Bytes(u64),
    /// Raw hertz, rendered with decimal prefixes.
    Hertz(u64),
    Percent(f64),
    Flag(bool),
    List(Vec<String>),
    /// A proportion to draw as a bar, with an optional caption spelling out the
    /// absolute figures behind it ("7.74 GiB of 31.07 GiB").
    Gauge {
        percent: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        polarity: Polarity,
    },
}

/// Which end of a gauge is the bad end.
///
/// Without this a gauge cannot be coloured: 95 % disk used and 95 % battery
/// remaining are the same number meaning opposite things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    /// High is bad — consumption and wear: disk used, memory in use, SSD
    /// endurance spent.
    HighIsBad,
    /// High is good — reserves and condition: battery charge, spare blocks,
    /// capacity against design.
    HighIsGood,
}

impl Value {
    /// A gauge measuring consumption, where a full bar is bad.
    pub fn gauge(percent: f64, label: impl Into<String>) -> Self {
        Value::Gauge {
            percent,
            label: Some(label.into()),
            polarity: Polarity::HighIsBad,
        }
    }

    /// A gauge measuring a reserve, where an empty bar is bad.
    ///
    /// Battery charge, SSD spare blocks, capacity against design — all read
    /// the opposite way round from usage.
    pub fn reserve_gauge(percent: f64, label: impl Into<String>) -> Self {
        Value::Gauge {
            percent,
            label: Some(label.into()),
            polarity: Polarity::HighIsGood,
        }
    }

    /// A gauge showing `used` of `total`, captioned with both byte counts.
    pub fn bytes_gauge(used: u64, total: u64) -> Self {
        let percent = if total == 0 {
            0.0
        } else {
            used as f64 / total as f64 * 100.0
        };
        Value::gauge(
            percent,
            format!("{} of {}", format_bytes(used), format_bytes(total)),
        )
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.to_string())
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}

impl From<u64> for Value {
    fn from(n: u64) -> Self {
        Value::Int(n as i64)
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::Int(n as i64)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Flag(b)
    }
}

impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Value::List(v)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(s) => f.write_str(s),
            Value::Int(n) => write!(f, "{n}"),
            Value::Bytes(b) => f.write_str(&format_bytes(*b)),
            Value::Hertz(hz) => f.write_str(&format_hertz(*hz)),
            Value::Percent(p) => write!(f, "{p:.1} %"),
            Value::Flag(true) => f.write_str("yes"),
            Value::Flag(false) => f.write_str("no"),
            Value::List(items) => f.write_str(&items.join(" ")),
            // Text fallbacks, used by the JSON report and by any renderer that
            // does not draw these as graphics.
            Value::Gauge {
                percent,
                label: Some(label),
                ..
            } => write!(f, "{percent:.1} % · {label}"),
            Value::Gauge {
                percent,
                label: None,
                ..
            } => write!(f, "{percent:.1} %"),
        }
    }
}

/// Spell out a duration the way an uptime readout should: coarse units only.
pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days} day{}", plural(days)));
    }
    if hours > 0 {
        parts.push(format!("{hours} hour{}", plural(hours)));
    }
    // Show minutes unless the uptime is long enough that they are noise.
    if days == 0 && (minutes > 0 || parts.is_empty()) {
        parts.push(format!("{minutes} minute{}", plural(minutes)));
    }
    parts.join(", ")
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Binary prefixes, as every other system tool reports memory and storage.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Decimal prefixes, which is how clock speeds are conventionally quoted.
pub fn format_hertz(hz: u64) -> String {
    const UNITS: [&str; 4] = ["Hz", "kHz", "MHz", "GHz"];
    let mut value = hz as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{hz} Hz")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_prefixes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(16 * 1024 * 1024 * 1024), "16.00 GiB");
    }

    #[test]
    fn hertz_uses_decimal_prefixes() {
        assert_eq!(format_hertz(800), "800 Hz");
        assert_eq!(format_hertz(3_400_000_000), "3.40 GHz");
    }

    #[test]
    fn bytes_gauge_computes_its_own_percentage() {
        let Value::Gauge { percent, label, .. } = Value::bytes_gauge(512, 1024) else {
            panic!("expected a gauge");
        };
        assert_eq!(percent, 50.0);
        assert_eq!(label.unwrap(), "512 B of 1.00 KiB");
    }

    #[test]
    fn bytes_gauge_survives_a_zero_total() {
        // Machines with no swap report SwapTotal: 0.
        let Value::Gauge { percent, .. } = Value::bytes_gauge(0, 0) else {
            panic!("expected a gauge");
        };
        assert_eq!(percent, 0.0);
    }

    #[test]
    fn durations_drop_units_that_would_be_noise() {
        assert_eq!(format_duration(0), "0 minutes");
        assert_eq!(format_duration(60), "1 minute");
        assert_eq!(format_duration(10_157), "2 hours, 49 minutes");
        assert_eq!(format_duration(90_000), "1 day, 1 hour");
    }

    #[test]
    fn push_opt_skips_missing_values() {
        let mut node = Node::group("Test", vec![]);
        node.push_opt("present", Some("here"));
        node.push_opt("absent", None::<&str>);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].label, "present");
    }
}
