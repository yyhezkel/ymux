//! Phase 68.A — add-on framework: manifest schema + built-in registry.
//!
//! An "add-on" is anything ymux installs on a remote server: the CLI
//! binary, the tmux config, the Claude hooks, and (new in Phase 68) the
//! `ymux-insights` daemon. Before 68 each had its own bespoke installer;
//! this crate gives them ONE shape so the desktop `AddonManager` can
//! install / update / remove / detect any of them uniformly, and the
//! Settings → Add-ons table + the wizards can drive them generically.
//!
//! Pure data (no IO) — the desktop's `AddonManager` owns the SSH side and
//! the `Builtin` routine dispatch; this crate just declares *what* each
//! add-on is.

use serde::{Deserialize, Serialize};

macro_rules! addon_type {
    ($item:item) => {
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(export, export_to = "../../../../src/bindings/"))]
        $item
    };
}

addon_type! {
    /// How one lifecycle step (install / uninstall / update / detect) is
    /// performed for an add-on.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum AddonAction {
        /// Run this shell snippet on the remote via `ssh_exec`. The
        /// AddonManager substitutes `${YMUX_BIN}` (the remote CLI path)
        /// and `${REMOTE_HOME}` before exec. For `detect`, the snippet
        /// prints the installed version to stdout (empty / non-zero = not
        /// installed).
        Shell { script: String },
        /// Call a built-in Rust routine by name — for add-ons that need
        /// SFTP upload or structured settings.json edits rather than a
        /// plain shell line (cli / tmux-conf / hooks). Dispatched by the
        /// desktop AddonManager.
        Builtin { routine: String },
        /// Nothing to do (e.g. an add-on with no separate update step).
        Noop,
    }
}

addon_type! {
    /// Static declaration of one add-on. Built-in add-ons ship their
    /// manifest compiled in ([`builtin_registry`]); the schema also
    /// serialises so a future community-add-ons directory can drop JSON.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct AddonManifest {
        /// Stable key: "ymux-cli" | "tmux-conf" | "hooks" | "insights".
        pub id: String,
        pub name: String,
        pub description: String,
        /// Version this desktop build ships / can install.
        pub version: String,
        /// Other add-on ids that must be installed first.
        #[serde(default)]
        pub dependencies: Vec<String>,
        pub install: AddonAction,
        pub uninstall: AddonAction,
        #[serde(default = "noop_action")]
        pub update: AddonAction,
        /// Prints the installed version (empty / non-zero ⇒ absent).
        pub detect: AddonAction,
        /// Needs sudo to install (drives the UI warning + wizard gating).
        #[serde(default)]
        pub needs_sudo: bool,
    }
}

addon_type! {
    /// Runtime status of an add-on on a given workspace (vs the static
    /// manifest). Returned by `addon_list` to the Settings table / wizard.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct AddonStatus {
        pub id: String,
        pub installed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub installed_version: Option<String>,
        pub available_version: String,
        pub update_available: bool,
        /// An install/update/remove is in flight for this (workspace, id).
        pub busy: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub last_error: Option<String>,
    }
}

fn noop_action() -> AddonAction {
    AddonAction::Noop
}

/// Built-in routine names dispatched by the desktop AddonManager (68.B
/// wires these to the existing bootstrap / setup-hooks code). Kept as
/// consts so the manifest and the dispatcher can't drift on a typo.
pub mod routines {
    pub const CLI_INSTALL: &str = "cli_install";
    pub const CLI_DETECT: &str = "cli_detect";
    pub const TMUX_CONF_INSTALL: &str = "tmux_conf_install";
    pub const TMUX_CONF_DETECT: &str = "tmux_conf_detect";
    pub const HOOKS_INSTALL: &str = "hooks_install";
    pub const HOOKS_UNINSTALL: &str = "hooks_uninstall";
    pub const HOOKS_DETECT: &str = "hooks_detect";
    pub const INSIGHTS_INSTALL: &str = "insights_install";
    pub const INSIGHTS_UNINSTALL: &str = "insights_uninstall";
    pub const INSIGHTS_DETECT: &str = "insights_detect";
    // Phase 70 — nginx reverse proxy + Let's Encrypt (Cloudflare DNS-01).
    // install needs params (domain + cf_token), so it's driven by the
    // mobile_pairing_init command, not generic addon_install.
    pub const NGINX_PROXY_INSTALL: &str = "nginx_proxy_install";
    pub const NGINX_PROXY_UNINSTALL: &str = "nginx_proxy_uninstall";
    pub const NGINX_PROXY_DETECT: &str = "nginx_proxy_detect";
}

/// Stable add-on ids.
pub mod ids {
    pub const CLI: &str = "ymux-cli";
    pub const TMUX_CONF: &str = "tmux-conf";
    pub const HOOKS: &str = "hooks";
    pub const INSIGHTS: &str = "insights";
    pub const NGINX_PROXY: &str = "nginx-proxy";
}

/// Version of the `nginx-proxy` add-on (the installer logic, not nginx itself).
pub const NGINX_PROXY_VERSION: &str = "1.0.0";

/// The version the `insights` daemon add-on installs. MUST track the embedded
/// daemon's `Version` const — Phase 77 renamed the daemon to `ymux-server`
/// (app/src-tauri/server/internal/core, `core.Version`). The desktop's update
/// check compares the remote's `ymux-server --version` (falling back to the
/// legacy `ymux-insights` symlink) against this. Major 2 = the API-stability
/// guarantee; existing 1.2.x installs are offered the 2.0.0 upgrade.
pub const INSIGHTS_VERSION: &str = "2.4.0";

/// The add-ons ymux knows about, in dependency-friendly order
/// (ymux-cli first — everything else needs the remote CLI present).
pub fn builtin_registry() -> Vec<AddonManifest> {
    vec![
        AddonManifest {
            id: ids::CLI.into(),
            name: "ymux CLI".into(),
            description: "The ymux remote CLI (RPC bridge, port-watch, hooks).".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            dependencies: vec![],
            install: AddonAction::Builtin {
                routine: routines::CLI_INSTALL.into(),
            },
            // Removing the CLI would break every other add-on; treat as no-op.
            uninstall: AddonAction::Noop,
            update: AddonAction::Builtin {
                routine: routines::CLI_INSTALL.into(),
            },
            detect: AddonAction::Builtin {
                routine: routines::CLI_DETECT.into(),
            },
            needs_sudo: false,
        },
        AddonManifest {
            id: ids::TMUX_CONF.into(),
            name: "tmux config".into(),
            description: "Scrollback-friendly ~/.ymux/tmux.conf.".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            dependencies: vec![ids::CLI.into()],
            install: AddonAction::Builtin {
                routine: routines::TMUX_CONF_INSTALL.into(),
            },
            uninstall: AddonAction::Shell {
                script: "rm -f \"${REMOTE_HOME}/.ymux/tmux.conf\"".into(),
            },
            update: AddonAction::Builtin {
                routine: routines::TMUX_CONF_INSTALL.into(),
            },
            detect: AddonAction::Builtin {
                routine: routines::TMUX_CONF_DETECT.into(),
            },
            needs_sudo: false,
        },
        AddonManifest {
            id: ids::HOOKS.into(),
            name: "Claude Code Hooks".into(),
            description: "Route Claude Code permission requests through ymux.".into(),
            version: "1.1.0".into(),
            dependencies: vec![ids::CLI.into()],
            install: AddonAction::Builtin {
                routine: routines::HOOKS_INSTALL.into(),
            },
            uninstall: AddonAction::Builtin {
                routine: routines::HOOKS_UNINSTALL.into(),
            },
            update: AddonAction::Builtin {
                routine: routines::HOOKS_INSTALL.into(),
            },
            detect: AddonAction::Builtin {
                routine: routines::HOOKS_DETECT.into(),
            },
            needs_sudo: false,
        },
        AddonManifest {
            id: ids::INSIGHTS.into(),
            name: "Server Insights".into(),
            description: "Lightweight metrics daemon (CPU/RAM/disk/net/Docker).".into(),
            version: INSIGHTS_VERSION.into(),
            dependencies: vec![ids::CLI.into()],
            // 68.C: the desktop AddonManager SFTP-uploads the bundled
            // daemon binary (arch-matched) + starts it; detect asks the
            // installed daemon for its version.
            install: AddonAction::Builtin {
                routine: routines::INSIGHTS_INSTALL.into(),
            },
            uninstall: AddonAction::Builtin {
                routine: routines::INSIGHTS_UNINSTALL.into(),
            },
            update: AddonAction::Builtin {
                routine: routines::INSIGHTS_INSTALL.into(),
            },
            detect: AddonAction::Builtin {
                routine: routines::INSIGHTS_DETECT.into(),
            },
            // Runs as a `systemd --user` service (no sudo needed); falls
            // back to nohup when user-lingering isn't available.
            needs_sudo: false,
        },
        AddonManifest {
            id: ids::NGINX_PROXY.into(),
            name: "Mobile Proxy (nginx + TLS)".into(),
            description: "Public nginx reverse proxy with a Let's Encrypt cert \
                          (Cloudflare DNS) so mobile devices can reach the daemon."
                .into(),
            version: NGINX_PROXY_VERSION.into(),
            dependencies: vec![],
            // install needs params (domain + CF token) → driven by
            // mobile_pairing_init, not generic addon_install. The Builtin
            // routine errors with that guidance if called paramless.
            install: AddonAction::Builtin {
                routine: routines::NGINX_PROXY_INSTALL.into(),
            },
            uninstall: AddonAction::Builtin {
                routine: routines::NGINX_PROXY_UNINSTALL.into(),
            },
            update: AddonAction::Builtin {
                routine: routines::NGINX_PROXY_INSTALL.into(),
            },
            detect: AddonAction::Builtin {
                routine: routines::NGINX_PROXY_DETECT.into(),
            },
            // nginx/apt/certbot require root (run-as-root or NOPASSWD sudo).
            needs_sudo: true,
        },
    ]
}

/// Look up a manifest by id.
pub fn manifest_for(id: &str) -> Option<AddonManifest> {
    builtin_registry().into_iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_ids_unique() {
        let reg = builtin_registry();
        let mut seen = std::collections::HashSet::new();
        for m in &reg {
            assert!(seen.insert(m.id.clone()), "duplicate id {}", m.id);
        }
        for id in [ids::CLI, ids::TMUX_CONF, ids::HOOKS, ids::INSIGHTS] {
            assert!(manifest_for(id).is_some(), "missing {id}");
        }
    }

    #[test]
    fn dependencies_reference_real_addons() {
        let reg = builtin_registry();
        let ids: std::collections::HashSet<_> = reg.iter().map(|m| m.id.clone()).collect();
        for m in &reg {
            for dep in &m.dependencies {
                assert!(ids.contains(dep), "{} depends on unknown {}", m.id, dep);
            }
        }
    }

    #[test]
    fn action_tagged_round_trip() {
        let a = AddonAction::Shell {
            script: "echo hi".into(),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["kind"], "shell");
        let back: AddonAction = serde_json::from_value(v).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn manifest_update_defaults_to_noop() {
        // A manifest JSON without `update` loads with Noop.
        let json = serde_json::json!({
            "id": "x", "name": "X", "description": "", "version": "1",
            "install": { "kind": "noop" },
            "uninstall": { "kind": "noop" },
            "detect": { "kind": "noop" }
        });
        let m: AddonManifest = serde_json::from_value(json).unwrap();
        assert_eq!(m.update, AddonAction::Noop);
        assert!(!m.needs_sudo);
        assert!(m.dependencies.is_empty());
    }
}
