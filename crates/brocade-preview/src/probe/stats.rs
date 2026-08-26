use std::collections::BTreeMap;

use serde_json::Value;

use crate::{config::Config, docker::docker_output, error::PreviewError};

#[derive(Debug, Clone)]
pub(crate) struct UserStats {
    pub(crate) total_bytes: u64,
    pub(crate) labels: BTreeMap<String, u64>,
}

/// Read per-user traffic counters from the xray inside a node container.
///
/// The api port is read from the converged xray.json, falling back to the default port where it
/// cannot be.
pub(crate) async fn read_user_stats(
    config: &Config,
    container: &str,
    user_stat_prefix: &str,
) -> Result<UserStats, PreviewError> {
    let script = r#"PORT=$(jq -r '.inbounds[]? | select(.tag=="api") | .port' /var/lib/brocade-agent/xray.json 2>/dev/null | head -n 1)
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then PORT=10085; fi
xray api statsquery --server=127.0.0.1:$PORT -pattern 'user>>>'
"#;
    let output = docker_output(config, &["exec", container, "sh", "-lc", script]).await?;
    parse_user_stats(&output, user_stat_prefix)
        .map_err(|error| PreviewError::internal(format!("parse xray user stats: {error}")))
}

/// Keep only the users matching the prefix, summing uplink and downlink together.
fn parse_user_stats(output: &str, user_stat_prefix: &str) -> Result<UserStats, String> {
    let value: Value = serde_json::from_str(output.trim()).map_err(|error| error.to_string())?;
    let stats = value
        .get("stat")
        .or_else(|| value.get("stats"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut labels = BTreeMap::<String, u64>::new();

    for stat in stats {
        let Some(name) = stat.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = stat.get("value").and_then(Value::as_u64) else {
            continue;
        };
        let Some((label, direction)) = parse_user_stat_name(name) else {
            continue;
        };
        if !label.starts_with(user_stat_prefix) {
            continue;
        }
        if !matches!(direction, "uplink" | "downlink") {
            continue;
        }
        *labels.entry(label.to_owned()).or_default() += value;
    }

    Ok(UserStats {
        total_bytes: labels.values().copied().sum(),
        labels,
    })
}

/// An xray counter name reads `user>>><label>>>>traffic>>>uplink`.
fn parse_user_stat_name(name: &str) -> Option<(&str, &str)> {
    let parts = name.split(">>>").collect::<Vec<_>>();
    match parts[..] {
        ["user", label, "traffic", direction] => Some((label, direction)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_stats_filters_and_sums_target_user_labels() {
        let stats = parse_user_stats(
            r#"{
                "stat": [
                    { "name": "user>>>alice@platform.acme#i-hk>>>traffic>>>uplink", "value": 12 },
                    { "name": "user>>>alice@platform.acme#i-hk>>>traffic>>>downlink", "value": 34 },
                    { "name": "user>>>bob@platform.acme#i-hk>>>traffic>>>uplink", "value": 999 },
                    { "name": "inbound>>>in:app/i-hk>>>traffic>>>uplink", "value": 999 }
                ]
            }"#,
            "alice@platform.acme#",
        )
        .unwrap();

        assert_eq!(stats.total_bytes, 46);
        assert_eq!(stats.labels.len(), 1);
        assert_eq!(
            stats.labels.get("alice@platform.acme#i-hk").copied(),
            Some(46)
        );
    }

    #[test]
    fn parse_user_stats_accepts_legacy_stats_field() {
        let stats = parse_user_stats(
            r#"{
                "stats": [
                    { "name": "user>>>alice@platform.acme#i-us>>>traffic>>>downlink", "value": 5 }
                ]
            }"#,
            "alice@platform.acme#",
        )
        .unwrap();

        assert_eq!(stats.total_bytes, 5);
        assert_eq!(
            stats.labels.get("alice@platform.acme#i-us").copied(),
            Some(5)
        );
    }

    #[test]
    fn parse_user_stat_name_rejects_counters_that_are_not_user_traffic() {
        assert_eq!(
            parse_user_stat_name("user>>>alice#i-hk>>>traffic>>>uplink"),
            Some(("alice#i-hk", "uplink"))
        );
        assert_eq!(
            parse_user_stat_name("inbound>>>in>>>traffic>>>uplink"),
            None
        );
        assert_eq!(parse_user_stat_name("user>>>alice>>>traffic"), None);
    }
}
