//! The ingest routes trust a self-asserted `user_id` until callers are
//! authenticated, so the binding map must never expose the Worker on its
//! public `*.workers.dev` subdomain (wrangler defaults it to on).

const EXAMPLE: &str = include_str!("../wrangler.toml.example");

/// Top-level keys end at the first table header; a `workers_dev` line under
/// any `[...]` table would not configure the Worker itself.
fn top_level_value(key: &str) -> Option<&'static str> {
    EXAMPLE
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim())
        })
}

#[test]
fn workers_dev_subdomain_is_disabled_at_top_level() {
    assert_eq!(top_level_value("workers_dev"), Some("false"));
}

#[test]
fn no_table_reenables_workers_dev() {
    let enabled = EXAMPLE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(k, _)| k.trim() == "workers_dev")
        .any(|(_, v)| v.split('#').next().unwrap_or("").trim() != "false");
    assert!(!enabled, "every workers_dev entry must be false");
}
