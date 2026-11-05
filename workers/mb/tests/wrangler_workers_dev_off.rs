//! The mb Worker is reachable only through the ingest `MB_API` service
//! binding, so the binding map must never expose it on its public
//! `*.workers.dev` subdomain (wrangler defaults it to on).
//! `workers_dev = false` is pinned at the top level and in every named
//! environment of the real `wrangler.toml`.

const CONFIG: &str = include_str!("../wrangler.toml");

/// Top-level keys end at the first table header; a `workers_dev` line under
/// any `[...]` table would not configure the Worker itself.
fn top_level_value(key: &str) -> Option<&'static str> {
    CONFIG
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim())
        })
}

/// Value of `workers_dev` inside one `[env.<name>]` section (its own keys
/// end at the next table header).
fn env_workers_dev(env_table: &str) -> Option<&'static str> {
    let start = CONFIG.find(env_table)?;
    let rest = &CONFIG[start + env_table.len()..];
    let mut value = None;
    for line in rest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == "workers_dev" {
                value = Some(v.split('#').next().unwrap_or("").trim());
            }
        }
    }
    value
}

#[test]
fn workers_dev_subdomain_is_disabled_at_top_level() {
    assert_eq!(top_level_value("workers_dev"), Some("false"));
}

#[test]
fn workers_dev_subdomain_is_disabled_in_every_env() {
    for env_table in ["[env.staging]\n", "[env.production]\n"] {
        assert_eq!(
            env_workers_dev(env_table),
            Some("false"),
            "{env_table:?} must set workers_dev = false explicitly (no inherited defaults)"
        );
    }
}

#[test]
fn no_table_reenables_workers_dev() {
    let enabled = CONFIG
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(k, _)| k.trim() == "workers_dev")
        .any(|(_, v)| v.split('#').next().unwrap_or("").trim() != "false");
    assert!(!enabled, "every workers_dev entry must be false");
}
