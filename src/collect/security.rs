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
use crate::model::{Node, Severity, Value};
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
    let selinux = match sysfs::read("/sys/fs/selinux/enforce") {
        Some(v) if v.trim() == "1" => "enforcing",
        Some(_) => "permissive",
        None => "not enabled",
    };
    let apparmor = std::path::Path::new("/sys/kernel/security/apparmor").is_dir();
    node.push(Node::field("SELinux", selinux));
    node.push(Node::field(
        "AppArmor",
        if apparmor { "enabled" } else { "not enabled" },
    ));

    // Flagged together rather than separately, because neither is a setting
    // to turn on: choosing between SELinux and AppArmor is a decision about
    // how a system is built, and telling someone running one of them to also
    // run the other would be nonsense. What is worth saying is that nothing
    // at all is confining anything.
    if selinux == "not enabled" && !apparmor {
        node.push(Node::field(
            "Mandatory access control",
            Value::warn(
                "none — no LSM is confining processes",
                "enable AppArmor or SELinux; which one depends on the distribution",
            ),
        ));
    } else if selinux == "permissive" {
        node.push(Node::field(
            "Mandatory access control",
            Value::warn(
                "SELinux is loaded but permissive — denials are logged, not enforced",
                "setenforce 1, and set SELINUX=enforcing to make it persist",
            ),
        ));
    }

    // Lockdown restricts even root from reading kernel memory or loading
    // unsigned modules; it is absent unless compiled in.
    node.push(Node::field(
        "Kernel lockdown",
        match sysfs::read("/sys/kernel/security/lockdown") {
            // Reported as "[none] integrity confidentiality" with the active
            // mode bracketed.
            Some(modes) => {
                let active = bracketed(&modes);
                if active == "none" {
                    Value::warn(
                        "none — root can read kernel memory and load unsigned modules",
                        "boot with lockdown=integrity; confidentiality is stricter and \
                         also blocks reading kernel memory",
                    )
                } else {
                    Value::Text(active)
                }
            }
            // No advice: a kernel built without the feature cannot be
            // configured into having it.
            None => Value::Text("not compiled in".to_string()),
        },
    ));

    node.push(Node::field(
        "Ptrace scope",
        graded(
            "/proc/sys/kernel/yama/ptrace_scope",
            &[
                (0, "unrestricted; any process may trace any other"),
                (1, "restricted to descendants"),
                (2, "admin only"),
                (3, "no tracing at all"),
            ],
            1,
            Severity::Warn,
            "set kernel.yama.ptrace_scope = 1 so a process can only trace its own children",
        ),
    ));
    node
}

/// Information-disclosure and exploit-mitigation sysctls.
fn hardening() -> Node {
    let mut node = Node::group("Hardening", vec![]);

    node.push(Node::field(
        "Address space randomisation",
        graded(
            "/proc/sys/kernel/randomize_va_space",
            &[
                (0, "disabled"),
                (1, "stack and mmap randomised"),
                (2, "full, including the heap"),
            ],
            2,
            Severity::Danger,
            "set kernel.randomize_va_space = 2; anything less leaves part of the \
             address space predictable",
        ),
    ));
    node.push(Node::field(
        "Kernel pointer exposure",
        graded(
            "/proc/sys/kernel/kptr_restrict",
            &[
                (0, "addresses visible to everyone"),
                (1, "hidden from unprivileged users"),
                (2, "hidden from everyone"),
            ],
            1,
            Severity::Warn,
            "set kernel.kptr_restrict = 1; kernel addresses in /proc defeat KASLR \
             for anything that can read them",
        ),
    ));
    node.push(Node::field(
        "Kernel log access",
        graded(
            "/proc/sys/kernel/dmesg_restrict",
            &[
                (0, "readable by everyone"),
                (1, "restricted to privileged users"),
            ],
            1,
            Severity::Warn,
            "set kernel.dmesg_restrict = 1; the ring buffer carries addresses and \
             hardware detail",
        ),
    ));
    node.push(Node::field(
        "Unprivileged BPF",
        graded(
            "/proc/sys/kernel/unprivileged_bpf_disabled",
            &[
                (0, "permitted"),
                (1, "disabled permanently until reboot"),
                (2, "disabled"),
            ],
            1,
            Severity::Danger,
            "set kernel.unprivileged_bpf_disabled = 2; unprivileged BPF has been a \
             recurring local privilege escalation surface",
        ),
    ));
    node.push(Node::field(
        "Performance events",
        graded(
            "/proc/sys/kernel/perf_event_paranoid",
            &[
                (-1, "unrestricted"),
                (0, "raw tracepoints restricted"),
                (1, "CPU events restricted"),
                (2, "kernel profiling restricted"),
            ],
            2,
            Severity::Warn,
            "set kernel.perf_event_paranoid = 2; lower values let unprivileged \
             processes sample the kernel",
        ),
    ));
    // Reported and deliberately not graded. Unprivileged user namespaces are
    // an attack surface and also what Flatpak, rootless podman, bubblewrap
    // and Nix's own build sandbox are built on, so "set this to 0" is advice
    // that breaks a working desktop. Which way it should go depends on what
    // the machine is for, and this page does not know that.
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
    // Not graded either: kernel.modules_disabled is a one-way switch for the
    // rest of the boot, and a machine that cannot load a module later cannot
    // hotplug hardware or switch to a new NixOS generation. Worth knowing,
    // not worth recommending.
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
            Value::Text(format!("enabled — {groups} groups"))
        } else {
            // Without an IOMMU there is no safe device passthrough, and
            // nothing stops a plugged-in PCIe or Thunderbolt device from
            // reading memory directly — which is a live concern on any
            // machine with an external port wired to the PCIe bus.
            Value::warn(
                "not enabled — devices can reach memory directly, and passthrough is unsafe",
                "boot with intel_iommu=on (or amd_iommu=on) for DMA protection",
            )
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
/// A sysctl with a recommended floor, reported with a verdict when it is not
/// met.
///
/// Every setting this is used for runs "higher is more restrictive", so the
/// verdict is whether the running value reaches the floor. The severity is
/// given per setting rather than derived from the number, because the number
/// says nothing about how much it matters: a readable kernel log leaks
/// addresses, and unprivileged BPF has been a repeated route to root, and
/// those are not the same finding.
///
/// A sysctl that does not exist is not a finding. The kernel may not have the
/// knob, and a recommendation to set something that cannot be set is worse
/// than silence.
fn graded(
    path: &str,
    meanings: &[(i64, &str)],
    least: i64,
    severity: Severity,
    advice: &str,
) -> Value {
    let described = describe(path, meanings);
    let Some(value) = sysfs::read(path).and_then(|raw| raw.trim().parse::<i64>().ok()) else {
        return Value::Text(described);
    };
    if value >= least {
        return Value::Text(described);
    }
    match severity {
        Severity::Warn => Value::warn(described, advice),
        Severity::Danger => Value::danger(described, advice),
    }
}

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
    fn a_setting_that_meets_its_floor_carries_no_verdict() {
        // Colour on this page has to mean "act on this". A correct value that
        // came back as a green verdict would make the page a wall of colour
        // in which the two rows that matter disappear.
        //
        // ASLR is 2 on any normal kernel, so this also asserts against the
        // real machine rather than only against a fixture.
        let value = graded(
            "/proc/sys/kernel/randomize_va_space",
            &[(2, "full")],
            2,
            Severity::Danger,
            "should not be reached",
        );
        assert!(
            matches!(value, Value::Text(_)),
            "a value at its floor is plain text"
        );
    }

    #[test]
    fn a_missing_sysctl_is_not_a_finding() {
        // The kernel may simply not have the knob, and recommending a setting
        // that cannot be set is worse than silence.
        let value = graded(
            "/proc/sys/kernel/hitui_not_a_real_sysctl",
            &[(1, "on")],
            1,
            Severity::Danger,
            "should not be reached",
        );
        match value {
            Value::Text(text) => assert_eq!(text, "not available on this kernel"),
            other => panic!("expected plain text, got {other:?}"),
        }
    }

    #[test]
    fn a_setting_below_its_floor_is_flagged_with_what_to_set() {
        // Driven through a real sysctl that is certain to be below an absurd
        // floor, so the grading path is exercised without inventing a file.
        let value = graded(
            "/proc/sys/kernel/randomize_va_space",
            &[(2, "full, including the heap")],
            99,
            Severity::Warn,
            "set kernel.randomize_va_space = 99",
        );
        match value {
            Value::Verdict {
                value,
                severity,
                advice,
            } => {
                assert_eq!(severity, Severity::Warn);
                // The reported value still says what it *is*, decoded.
                assert!(value.starts_with("2 — "), "{value}");
                // And the advice says what to do, not merely that something
                // is wrong.
                let advice = advice.expect("a floor miss always carries advice");
                assert!(advice.contains("kernel.randomize_va_space"), "{advice}");
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    #[test]
    fn every_recommendation_names_something_to_change() {
        // A flag with no actionable fix is a complaint. Advice is allowed to
        // be absent, but when present it has to name a setting, a boot
        // parameter or a command.
        let nodes = Security.collect().expect("collection failed");
        let mut verdicts = 0;
        for section in &nodes {
            for field in &section.children {
                let Some(Value::Verdict {
                    value,
                    advice,
                    severity,
                }) = field.value.as_ref()
                else {
                    continue;
                };
                verdicts += 1;
                // The value is still reported, not replaced by the complaint.
                assert!(!value.is_empty(), "{} has an empty value", field.label);
                assert!(matches!(severity, Severity::Warn | Severity::Danger));
                if let Some(advice) = advice {
                    assert!(
                        advice.contains('=')
                            || advice.contains("enable")
                            || advice.contains("boot with")
                            || advice.contains("setenforce"),
                        "{}: advice names nothing to change: {advice}",
                        field.label
                    );
                }
            }
        }
        // Not asserted to be non-zero: a well-configured machine should have
        // no findings, and that is the point rather than a broken test.
        let _ = verdicts;
    }

    #[test]
    fn the_settings_that_depend_on_what_the_machine_is_for_are_not_graded() {
        // Unprivileged user namespaces are an attack surface and also what
        // Flatpak, rootless podman, bubblewrap and Nix's build sandbox are
        // built on. kernel.modules_disabled is a one-way switch that stops a
        // machine hotplugging hardware or switching generations. Both are
        // worth reporting and neither is ours to recommend.
        let nodes = Security.collect().expect("collection failed");
        for label in ["Unprivileged user namespaces", "Loading disabled"] {
            let field = nodes
                .iter()
                .flat_map(|section| section.children.iter())
                .find(|field| field.label == label)
                .unwrap_or_else(|| panic!("{label} is not reported"));
            assert!(
                !matches!(field.value, Some(Value::Verdict { .. })),
                "{label} must be reported without a recommendation"
            );
        }
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
