//! Kernel hardening and access-control state.
//!
//! The sysctls here are the ones that decide what an unprivileged process can
//! see and do. Each is reported with what its value *means* rather than as a
//! bare integer: `kptr_restrict = 1` is not self-explanatory, and the whole
//! point of the page is to be read rather than decoded.
//!
//! Boot-time firmware state (Secure Boot, TPM) is on the Firmware page; this
//! one is about the running kernel.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Security;

impl Collector for Security {
    fn id(&self) -> &'static str {
        "security"
    }

    fn title(&self) -> &'static str {
        "Security"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        Ok(vec![
            access_control(),
            hardening(),
            modules(),
            virtualisation(),
        ])
    }
}

/// Which security modules are active, and the state of the big switches.
fn access_control() -> Node {
    let mut node = Node::group("Access control", vec![]);

    // The authoritative list of enabled LSMs, in invocation order.
    node.push(Node::field(
        "Security modules",
        sysfs::read("/sys/kernel/security/lsm")
            .map(|lsms| lsms.replace(',', ", "))
            .unwrap_or_else(|| "none reported".to_string()),
    ));

    // SELinux and AppArmor are the two that actually confine anything, and
    // being compiled in is not the same as being enforcing.
    node.push(Node::field(
        "SELinux",
        match sysfs::read("/sys/fs/selinux/enforce") {
            Some(v) if v.trim() == "1" => "enforcing".to_string(),
            Some(_) => "permissive".to_string(),
            None => "not enabled".to_string(),
        },
    ));
    node.push(Node::field(
        "AppArmor",
        if std::path::Path::new("/sys/kernel/security/apparmor").is_dir() {
            "enabled"
        } else {
            "not enabled"
        },
    ));

    // Lockdown restricts even root from reading kernel memory or loading
    // unsigned modules; it is absent unless compiled in.
    node.push(Node::field(
        "Kernel lockdown",
        sysfs::read("/sys/kernel/security/lockdown")
            // Reported as "[none] integrity confidentiality" with the active
            // mode bracketed.
            .map(|modes| bracketed(&modes))
            .unwrap_or_else(|| "not compiled in".to_string()),
    ));

    node.push(Node::field(
        "Ptrace scope",
        describe(
            "/proc/sys/kernel/yama/ptrace_scope",
            &[
                (0, "unrestricted; any process may trace any other"),
                (1, "restricted to descendants"),
                (2, "admin only"),
                (3, "no tracing at all"),
            ],
        ),
    ));
    node
}

/// Information-disclosure and exploit-mitigation sysctls.
fn hardening() -> Node {
    let mut node = Node::group("Hardening", vec![]);

    node.push(Node::field(
        "Address space randomisation",
        describe(
            "/proc/sys/kernel/randomize_va_space",
            &[
                (0, "disabled"),
                (1, "stack and mmap randomised"),
                (2, "full, including the heap"),
            ],
        ),
    ));
    node.push(Node::field(
        "Kernel pointer exposure",
        describe(
            "/proc/sys/kernel/kptr_restrict",
            &[
                (0, "addresses visible to everyone"),
                (1, "hidden from unprivileged users"),
                (2, "hidden from everyone"),
            ],
        ),
    ));
    node.push(Node::field(
        "Kernel log access",
        describe(
            "/proc/sys/kernel/dmesg_restrict",
            &[
                (0, "readable by everyone"),
                (1, "restricted to privileged users"),
            ],
        ),
    ));
    node.push(Node::field(
        "Unprivileged BPF",
        describe(
            "/proc/sys/kernel/unprivileged_bpf_disabled",
            &[
                (0, "permitted"),
                (1, "disabled permanently until reboot"),
                (2, "disabled"),
            ],
        ),
    ));
    node.push(Node::field(
        "Performance events",
        describe(
            "/proc/sys/kernel/perf_event_paranoid",
            &[
                (-1, "unrestricted"),
                (0, "raw tracepoints restricted"),
                (1, "CPU events restricted"),
                (2, "kernel profiling restricted"),
            ],
        ),
    ));
    node.push(Node::field(
        "Unprivileged user namespaces",
        describe(
            "/proc/sys/kernel/unprivileged_userns_clone",
            &[(0, "disabled"), (1, "permitted")],
        ),
    ));
    node
}

/// Module loading, which is the usual way code gets into a running kernel.
fn modules() -> Node {
    let mut node = Node::group("Module loading", vec![]);

    node.push(Node::field(
        "Signature enforcement",
        match sysfs::read("/sys/module/module/parameters/sig_enforce") {
            Some(v) if v.trim() == "Y" || v.trim() == "1" => {
                "enforced — unsigned modules are refused".to_string()
            }
            Some(_) => "not enforced — unsigned modules load".to_string(),
            None => "not configurable on this kernel".to_string(),
        },
    ));
    node.push(Node::field(
        "Loading disabled",
        describe(
            "/proc/sys/kernel/modules_disabled",
            &[
                (0, "no, modules may still be loaded"),
                (1, "yes, until reboot"),
            ],
        ),
    ));
    // The taint flags themselves are decoded on the Kernel Modules page, where
    // the modules that caused them are listed.
    node.push(Node::field(
        "Taint value",
        sysfs::read("/proc/sys/kernel/tainted")
            .map(|v| format!("{} — decoded on the Kernel Modules page", v.trim()))
            .unwrap_or_else(|| "unavailable".to_string()),
    ));
    node
}

/// Hardware virtualisation and passthrough readiness.
///
/// Not security state exactly, but it is the same question — what the kernel
/// will let a guest or a userspace driver reach — and nothing else surfaces
/// IOMMU grouping in a terminal.
fn virtualisation() -> Node {
    let mut node = Node::group("Virtualisation", vec![]);

    let groups = sysfs::list_dir("/sys/kernel/iommu_groups").len();
    node.push(Node::field(
        "IOMMU",
        if groups > 0 {
            format!("enabled — {groups} groups")
        } else {
            // Without an IOMMU there is no safe device passthrough, which is
            // the thing someone reading this page wants to know.
            "not enabled — no device passthrough without it".to_string()
        },
    ));

    // KVM presents itself as a character device once the module is loaded.
    node.push(Node::field(
        "KVM",
        if std::path::Path::new("/dev/kvm").exists() {
            "available"
        } else {
            "unavailable — module not loaded, or disabled in firmware"
        },
    ));

    // vmx/svm in the CPU flags means the silicon supports it; whether firmware
    // left it enabled is a separate question that /dev/kvm answers.
    if let Some(cpuinfo) = sysfs::read("/proc/cpuinfo") {
        let blocks = sysfs::parse_cpuinfo_blocks(&cpuinfo);
        if let Some(flags) = blocks.first().and_then(|b| sysfs::find(b, "flags")) {
            let extension = flags
                .split_whitespace()
                .find(|f| *f == "vmx" || *f == "svm");
            node.push(Node::field(
                "CPU extension",
                match extension {
                    Some("vmx") => "Intel VT-x",
                    Some("svm") => "AMD-V",
                    // The flag disappears entirely when firmware disables the
                    // extension, so absence does not mean the silicon lacks it.
                    _ => "none reported — absent, or disabled in firmware",
                },
            ));
        }
    }
    node
}

/// Read a numeric sysctl and say what the value means.
///
/// Reports the number alongside the explanation: the number is what you set,
/// the explanation is what you wanted to know.
fn describe(path: &str, meanings: &[(i64, &str)]) -> String {
    let Some(raw) = sysfs::read(path) else {
        return "not available on this kernel".to_string();
    };
    let Ok(value) = raw.trim().parse::<i64>() else {
        return raw.trim().to_string();
    };
    match meanings.iter().find(|(v, _)| *v == value) {
        Some((_, meaning)) => format!("{value} — {meaning}"),
        // An unmapped value is still worth printing; the kernel may have
        // gained a level since this table was written.
        None => format!("{value}"),
    }
}

/// Pull the active choice out of a `[none] integrity confidentiality` list.
fn bracketed(modes: &str) -> String {
    modes
        .split_whitespace()
        .find_map(|word| {
            word.strip_prefix('[')
                .and_then(|w| w.strip_suffix(']'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| modes.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_active_bracketed_mode() {
        assert_eq!(bracketed("[none] integrity confidentiality"), "none");
        assert_eq!(bracketed("none [integrity] confidentiality"), "integrity");
        // No brackets at all: pass the text through rather than inventing.
        assert_eq!(bracketed("integrity"), "integrity");
    }

    #[test]
    fn describes_known_values_and_passes_through_unknown_ones() {
        let meanings = &[(0, "off"), (1, "on")];

        // Real sysctl: ASLR is 2 on any normal kernel, which is mapped.
        let aslr = describe(
            "/proc/sys/kernel/randomize_va_space",
            &[(0, "disabled"), (1, "partial"), (2, "full")],
        );
        assert!(aslr.starts_with("2 — "), "{aslr}");

        // A path that cannot exist must say so, not report a wrong default.
        let missing = describe("/proc/sys/kernel/hitui_not_a_real_sysctl", meanings);
        assert_eq!(missing, "not available on this kernel");
    }

    #[test]
    fn a_missing_sysctl_never_reads_as_a_safe_default() {
        // The failure mode worth guarding: reporting "0 — disabled" for a
        // knob that simply is not present would be a false statement about
        // the system's configuration.
        let result = describe("/proc/sys/kernel/hitui_absent", &[(0, "disabled")]);
        assert!(!result.contains("disabled"), "{result}");
    }

    #[test]
    fn collects_every_section() {
        let nodes = Security.collect().expect("collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Access control",
                "Hardening",
                "Module loading",
                "Virtualisation"
            ]
        );
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
            // Every field must say something; a blank is indistinguishable
            // from a collector that silently failed.
            for field in &node.children {
                let value = field.value.as_ref().unwrap().to_string();
                assert!(!value.is_empty(), "{} has no value", field.label);
            }
        }
    }
}
