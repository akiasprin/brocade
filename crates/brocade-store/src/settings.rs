use std::ops::RangeInclusive;

use brocade_core::{
    model::{
        ConnectionSettings, DisabledWireGuardLink, GeodataSettings, ModelSettings, NodeConnection,
        OverlaySettings, PortSettings, ProbeSettings, RealityClientPolicy, RealitySite,
    },
    text::{
        is_nonzero_host_port, is_reality_fingerprint, is_reality_server_name, normalize_host_port,
        parse_semver3,
    },
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

const MAX_REALITY_TIME_DIFF_MS: u64 = 24 * 60 * 60 * 1000;

/// The non-empty values flow may take. Xray-core's vless inbound accepts only `vless.XRV` and
/// the empty string, answering anything else with `unknown request flow`;
/// `xtls-rprx-vision-udp443` is a client-side variant truncated to XRV by the outbound before
/// it goes on the wire, so the server never sees that suffix (argued on
/// `model::DEFAULT_REALITY_FLOW`).
///
/// So this dropdown has two settings: on (the single value here) and off (`None`). A fresh
/// database defaults to on. Getting it wrong costs every client their connection, and by then
/// the wave has already shipped, hence blocking it as a 400 first.
pub(crate) const KNOWN_FLOWS: &[&str] = &["xtls-rprx-vision"];

pub(crate) fn validate_flow(value: Option<&str>, location: &str) -> Result<()> {
    if let Some(flow) = value {
        // The empty string is a value here, not an absence: it is "ship no flow". It has to pass,
        // because it is the only way to turn Vision off for one ingress rather than the fleet —
        // and XHTTP cannot run with Vision.
        if !flow.is_empty() && !KNOWN_FLOWS.contains(&flow) {
            return Err(StoreError::InvalidData(format!(
                "{location} 只能是 {}，收到 {flow}",
                KNOWN_FLOWS.join(" / ")
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSettingsResult {
    pub revision_id: u64,
    pub settings: ModelSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSnapshot {
    pub revision_id: u64,
    pub settings: ModelSettings,
}

const SETTINGS_SQL: &str = "SELECT current_revision,
            reality_min_client_ver,
            reality_max_client_ver,
            reality_max_time_diff_ms,
            reality_dest,
            reality_server_names,
            reality_fingerprint,
            reality_flow,
            overlay_keepalive_secs,
            overlay_mtu,
            overlay_disabled_links,
            port_ingress_base,
            port_anytls_base,
            port_hop_base,
            port_hy2_base,
            probe_endpoint_url,
            probe_timeout_secs,
            probe_interval_secs,
            geodata_cron,
            geodata_geoip_url,
            geodata_geosite_url,
            conn_idle_secs,
            conn_uplink_only_secs,
            conn_downlink_only_secs,
            conn_buffer_size_kb,
            conn_handshake_secs,
            stats_user_online
         FROM control_state
         WHERE id = TRUE";

pub async fn load_settings(pool: &PgPool) -> Result<ModelSettings> {
    settings_from_row(&sqlx::query(SETTINGS_SQL).fetch_one(pool).await?)
}

pub async fn load_settings_snapshot(pool: &PgPool) -> Result<SettingsSnapshot> {
    let row = sqlx::query(SETTINGS_SQL).fetch_one(pool).await?;
    let revision = row.try_get::<i64, _>("current_revision")?;
    Ok(SettingsSnapshot {
        revision_id: u64::try_from(revision).map_err(|_| {
            StoreError::InvalidData("control_state.current_revision is negative".to_owned())
        })?,
        settings: settings_from_row(&row)?,
    })
}

/// An in-transaction read. During a batch commit an earlier operation may have just changed
/// the global settings, and reading from the pool would see the old value.
pub(crate) async fn load_settings_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<ModelSettings> {
    settings_from_row(&sqlx::query(SETTINGS_SQL).fetch_one(&mut **tx).await?)
}

fn settings_from_row(row: &sqlx::postgres::PgRow) -> Result<ModelSettings> {
    let secs = |column: &str| -> Result<u32> {
        u32::try_from(row.try_get::<i32, _>(column)?)
            .map_err(|_| StoreError::InvalidData(format!("control_state.{column} 是负数")))
    };
    Ok(ModelSettings {
        connection: ConnectionSettings {
            conn_idle_secs: secs("conn_idle_secs")?,
            uplink_only_secs: secs("conn_uplink_only_secs")?,
            downlink_only_secs: secs("conn_downlink_only_secs")?,
            buffer_size_kb: row
                .try_get::<Option<i32>, _>("conn_buffer_size_kb")?
                .map(|kb| {
                    u32::try_from(kb).map_err(|_| {
                        StoreError::InvalidData(
                            "control_state.conn_buffer_size_kb 是负数".to_owned(),
                        )
                    })
                })
                .transpose()?,
            handshake_secs: secs("conn_handshake_secs")?,
        },
        stats_user_online: row.try_get("stats_user_online")?,
        reality_client: RealityClientPolicy {
            min_client_ver: row.try_get("reality_min_client_ver")?,
            max_client_ver: row.try_get("reality_max_client_ver")?,
            max_time_diff_ms: optional_u64(
                "control_state.reality_max_time_diff_ms",
                row.try_get("reality_max_time_diff_ms")?,
            )?,
        },
        reality_site: RealitySite {
            dest: row.try_get("reality_dest")?,
            server_names: crate::materialize::json_string_array(
                "control_state.reality_server_names",
                &row.try_get::<serde_json::Value, _>("reality_server_names")?,
            )?,
            fingerprint: row.try_get("reality_fingerprint")?,
            flow: row.try_get("reality_flow")?,
        },
        overlay: OverlaySettings {
            keepalive_secs: u16_column(
                "overlay_keepalive_secs",
                row.try_get("overlay_keepalive_secs")?,
            )?,
            mtu: u16_column("overlay_mtu", row.try_get("overlay_mtu")?)?,
            disabled_links: serde_json::from_value(
                row.try_get::<serde_json::Value, _>("overlay_disabled_links")?,
            )
            .map_err(|error| {
                StoreError::InvalidData(format!(
                    "control_state.overlay_disabled_links 格式错误: {error}"
                ))
            })?,
        },
        ports: PortSettings {
            ingress_base: u16_column("port_ingress_base", row.try_get("port_ingress_base")?)?,
            anytls_base: u16_column("port_anytls_base", row.try_get("port_anytls_base")?)?,
            hop_base: u16_column("port_hop_base", row.try_get("port_hop_base")?)?,
            hy2_base: u16_column("port_hy2_base", row.try_get("port_hy2_base")?)?,
        },
        probe: ProbeSettings {
            endpoint_url: row.try_get("probe_endpoint_url")?,
            timeout_secs: u16_column("probe_timeout_secs", row.try_get("probe_timeout_secs")?)?,
            interval_secs: u32_column("probe_interval_secs", row.try_get("probe_interval_secs")?)?,
        },
        geodata: GeodataSettings {
            cron: row.try_get("geodata_cron")?,
            geoip_url: row.try_get("geodata_geoip_url")?,
            geosite_url: row.try_get("geodata_geosite_url")?,
        },
    })
}

pub async fn update_settings(
    pool: &PgPool,
    actor: &AdminContext,
    settings: ModelSettings,
    expected_revision: Option<u64>,
) -> Result<UpdateSettingsResult> {
    let mut tx = pool.begin().await?;
    let previous = crate::console::lock_control_state(&mut tx).await?;
    if let Some(expected) = expected_revision {
        if expected != previous {
            return Err(StoreError::Unsupported(format!(
                "settings changed since revision {expected}; current revision is {previous}"
            )));
        }
    }
    let revision_id =
        crate::console::insert_revision(&mut tx, actor.operator_id(), "update global settings")
            .await?;
    let (settings, changed) = update_settings_tx(&mut tx, actor, settings).await?;
    let revision_id =
        crate::console::commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpdateSettingsResult {
        revision_id,
        settings,
    })
}

pub(crate) async fn update_settings_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &AdminContext,
    settings: ModelSettings,
) -> Result<(ModelSettings, bool)> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update global settings".to_owned(),
        ));
    }

    let settings = normalize_settings(settings);
    validate_backbone(&settings)?;
    validate_settings(&settings)?;

    // Storing the same thing again should not consume a revision number. The settings page
    // submits the whole form, and opening it, looking, and pressing save is commonplace. The
    // comparison is against normalized values, so a stray trailing space is not a change. The
    // global settings live entirely in that one control_state row, so this one comparison
    // covers everything the write touches.
    if load_settings_tx(tx).await? == settings {
        return Ok((settings, false));
    }

    sqlx::query(
        "UPDATE control_state
         SET reality_min_client_ver = $1,
             reality_max_client_ver = $2,
             reality_max_time_diff_ms = $3,
             reality_dest = $4,
             reality_server_names = $5,
             reality_fingerprint = $6,
             reality_flow = $7,
             overlay_keepalive_secs = $8,
             overlay_mtu = $9,
             port_ingress_base = $10,
             port_anytls_base = $11,
             port_hop_base = $12,
             port_hy2_base = $13,
             probe_endpoint_url = $14,
             probe_timeout_secs = $15,
             probe_interval_secs = $16,
             geodata_cron = $17,
             geodata_geoip_url = $18,
             geodata_geosite_url = $19,
             conn_idle_secs = $20,
             conn_uplink_only_secs = $21,
             conn_downlink_only_secs = $22,
             conn_buffer_size_kb = $23,
             conn_handshake_secs = $24,
             stats_user_online = $25,
             overlay_disabled_links = $26
         WHERE id = TRUE",
    )
    .bind(settings.reality_client.min_client_ver.as_deref())
    .bind(settings.reality_client.max_client_ver.as_deref())
    .bind(optional_i64(
        "settings.reality_client.max_time_diff_ms",
        settings.reality_client.max_time_diff_ms,
    )?)
    .bind(settings.reality_site.dest.as_deref())
    .bind(serde_json::to_value(&settings.reality_site.server_names)?)
    .bind(settings.reality_site.fingerprint.as_deref())
    .bind(settings.reality_site.flow.as_deref())
    .bind(i32::from(settings.overlay.keepalive_secs))
    .bind(i32::from(settings.overlay.mtu))
    .bind(i32::from(settings.ports.ingress_base))
    .bind(i32::from(settings.ports.anytls_base))
    .bind(i32::from(settings.ports.hop_base))
    .bind(i32::from(settings.ports.hy2_base))
    .bind(settings.probe.endpoint_url.as_str())
    .bind(i32::from(settings.probe.timeout_secs))
    .bind(i32::try_from(settings.probe.interval_secs).unwrap_or(i32::MAX))
    .bind(settings.geodata.cron.as_str())
    .bind(settings.geodata.geoip_url.as_str())
    .bind(settings.geodata.geosite_url.as_str())
    // i32 for every one: the columns are INTEGER, and a value large enough to overflow
    // would be a timeout measured in decades — clamping rather than failing keeps a typo
    // from taking the whole settings write down with it.
    .bind(i32::try_from(settings.connection.conn_idle_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(settings.connection.uplink_only_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(settings.connection.downlink_only_secs).unwrap_or(i32::MAX))
    .bind(
        settings
            .connection
            .buffer_size_kb
            .map(|kb| i32::try_from(kb).unwrap_or(i32::MAX)),
    )
    .bind(i32::try_from(settings.connection.handshake_secs).unwrap_or(i32::MAX))
    .bind(settings.stats_user_online)
    .bind(serde_json::to_value(&settings.overlay.disabled_links)?)
    .execute(&mut **tx)
    .await?;

    Ok((settings, true))
}

pub(crate) async fn set_wireguard_link_disabled_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &AdminContext,
    a: String,
    b: String,
    disabled: bool,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can disable WireGuard links".to_owned(),
        ));
    }
    let mut a = crate::input::required_text(a, "WireGuard 链路端点 a")?;
    let mut b = crate::input::required_text(b, "WireGuard 链路端点 b")?;
    if a == b {
        return Err(StoreError::InvalidData(
            "WireGuard 链路的两个端点不能是同一台机器".to_owned(),
        ));
    }
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }

    crate::console::ensure_node_exists_tx(tx, &a).await?;
    crate::console::ensure_node_exists_tx(tx, &b).await?;

    let mut settings = load_settings_tx(tx).await?;
    let link = DisabledWireGuardLink { a, b };
    if disabled {
        settings.overlay.disabled_links.push(link);
    } else {
        settings.overlay.disabled_links.retain(|item| item != &link);
    }
    Ok(update_settings_tx(tx, actor, settings).await?.1)
}

/// The database has a CHECK constraint that would surface an out-of-range value as a 500;
/// this blocks it as a 400 first and states the range.
fn validate_backbone(settings: &ModelSettings) -> Result<()> {
    if settings.overlay.keepalive_secs == 0 {
        return Err(StoreError::InvalidData(
            "overlay.keepalive_secs 必须大于 0（单向拨号时靠它维持 NAT 映射）".to_owned(),
        ));
    }
    // The floor is not IPv6's 1280: the overlay is IPv4 only, and effective tunnel MTUs below
    // 1280 genuinely occur on cross-border links. 1000 exists only to catch obvious
    // misconfiguration.
    if !(1000..=9000).contains(&settings.overlay.mtu) {
        return Err(StoreError::InvalidData(format!(
            "overlay.mtu 必须在 1000 到 9000 之间，收到 {}",
            settings.overlay.mtu
        )));
    }
    // A port base is where the upward search starts, so 0 is meaningless (not a bindable port)
    // and so is 65535 (nothing left to search). The database has the same CHECK; this blocks it
    // as a 400 first and says so.
    for (what, port) in [
        ("ports.ingress_base", settings.ports.ingress_base),
        ("ports.anytls_base", settings.ports.anytls_base),
        ("ports.hop_base", settings.ports.hop_base),
        ("ports.hy2_base", settings.ports.hy2_base),
    ] {
        if port == 0 || port == u16::MAX {
            return Err(StoreError::InvalidData(format!(
                "{what} 必须在 1 到 65534 之间，收到 {port}"
            )));
        }
    }
    Ok(())
}

fn u32_column(location: &str, value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

fn u16_column(location: &str, value: i32) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

fn normalize_settings(settings: ModelSettings) -> ModelSettings {
    let OverlaySettings {
        keepalive_secs,
        mtu,
        disabled_links,
    } = settings.overlay;
    let mut disabled_links = disabled_links
        .into_iter()
        .map(|link| {
            let mut a = link.a.trim().to_owned();
            let mut b = link.b.trim().to_owned();
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            DisabledWireGuardLink { a, b }
        })
        .collect::<Vec<_>>();
    disabled_links.sort();
    disabled_links.dedup();

    ModelSettings {
        // Numbers, with nothing to trim or case-fold; they pass through untouched.
        connection: settings.connection,
        stats_user_online: settings.stats_user_online,
        reality_client: RealityClientPolicy {
            min_client_ver: normalize_optional_text(settings.reality_client.min_client_ver),
            max_client_ver: normalize_optional_text(settings.reality_client.max_client_ver),
            max_time_diff_ms: settings.reality_client.max_time_diff_ms,
        },
        reality_site: RealitySite {
            dest: normalize_optional_text(settings.reality_site.dest)
                .map(|dest| normalize_host_port(&dest)),
            server_names: settings
                .reality_site
                .server_names
                .into_iter()
                .filter_map(normalize_optional_text_owned)
                .collect(),
            fingerprint: normalize_optional_text(settings.reality_site.fingerprint),
            flow: normalize_optional_text(settings.reality_site.flow),
        },
        overlay: OverlaySettings {
            keepalive_secs,
            mtu,
            disabled_links,
        },
        ports: settings.ports,
        probe: ProbeSettings {
            endpoint_url: settings.probe.endpoint_url.trim().to_owned(),
            ..settings.probe
        },
        geodata: GeodataSettings {
            cron: settings.geodata.cron.trim().to_owned(),
            geoip_url: settings.geodata.geoip_url.trim().to_owned(),
            geosite_url: settings.geodata.geosite_url.trim().to_owned(),
        },
    }
}

fn normalize_optional_text_owned(value: String) -> Option<String> {
    normalize_optional_text(Some(value))
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_owned())
        }
    })
}

fn validate_settings(settings: &ModelSettings) -> Result<()> {
    if let Some(link) = settings
        .overlay
        .disabled_links
        .iter()
        .find(|link| link.a.is_empty() || link.b.is_empty() || link.a == link.b)
    {
        return Err(StoreError::InvalidData(format!(
            "overlay.disabled_links 必须引用两台不同的机器，收到 {:?} 与 {:?}",
            link.a, link.b
        )));
    }

    let policy = &settings.reality_client;
    let min = validate_version(
        policy.min_client_ver.as_deref(),
        "settings.reality_client.min_client_ver",
    )?;
    let max = validate_version(
        policy.max_client_ver.as_deref(),
        "settings.reality_client.max_client_ver",
    )?;

    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err(StoreError::InvalidData(
                "settings.reality_client.min_client_ver cannot be greater than max_client_ver"
                    .to_owned(),
            ));
        }
    }

    if policy
        .max_time_diff_ms
        .is_some_and(|value| value > MAX_REALITY_TIME_DIFF_MS)
    {
        return Err(StoreError::InvalidData(format!(
            "settings.reality_client.max_time_diff_ms cannot exceed {MAX_REALITY_TIME_DIFF_MS}"
        )));
    }

    // dest must be host:port — REALITY forwards the handshake to that real site, and getting it
    // wrong presents as the handshake failing outright.
    if let Some(dest) = settings.reality_site.dest.as_deref() {
        if !is_nonzero_host_port(dest) {
            return Err(StoreError::InvalidData(
                "settings.reality_site.dest must look like example.com:443".to_owned(),
            ));
        }
    }
    // A dest requires server_names: the client's SNI takes that, not dest's hostname
    if settings.reality_site.dest.is_some() && settings.reality_site.server_names.is_empty() {
        return Err(StoreError::InvalidData(
            "settings.reality_site.server_names must not be empty when dest is set".to_owned(),
        ));
    }
    if settings.reality_site.dest.is_none() && !settings.reality_site.server_names.is_empty() {
        return Err(StoreError::InvalidData(
            "settings.reality_site.server_names requires a configured dest".to_owned(),
        ));
    }
    if let Some(server_name) = settings
        .reality_site
        .server_names
        .iter()
        .find(|server_name| !is_reality_server_name(server_name))
    {
        return Err(StoreError::InvalidData(format!(
            "settings.reality_site.server_names contains invalid name {server_name:?}"
        )));
    }
    if settings
        .reality_site
        .fingerprint
        .as_deref()
        .is_some_and(|fingerprint| !is_reality_fingerprint(fingerprint))
    {
        return Err(StoreError::InvalidData(
            "settings.reality_site.fingerprint is unsupported by the pinned Xray REALITY client"
                .to_owned(),
        ));
    }

    validate_flow(
        settings.reality_site.flow.as_deref(),
        "settings.reality_site.flow",
    )?;

    validate_geodata(&settings.geodata)?;
    validate_connection_settings(&settings.connection)?;

    Ok(())
}

/// What the four timeouts and the buffer are allowed to be. `handshake` is here too even
/// though no machine can override it — the range belongs to the field, not to the page it
/// is edited on.
///
/// The lower bounds mark where a value stops meaning what it says, not where it stops being
/// wise. `connIdle` at 0 reaps a connection the moment it falls quiet, so 0 is refused;
/// `uplinkOnly` at 0 legitimately means "close as soon as the other direction has", which is
/// a choice somebody may want, so 0 is allowed. Nothing in between is judged: an operator who
/// wants a 15-second idle timeout knows something about their fleet that this file does not.
///
/// The upper bounds are only there so that a slipped keystroke is refused where it is typed
/// rather than three layers down. 64 MB per connection is already past any real answer.
const CONN_IDLE_RANGE: RangeInclusive<u32> = 10..=86_400;
const CONN_HALF_CLOSE_RANGE: RangeInclusive<u32> = 0..=3_600;
const CONN_BUFFER_KB_RANGE: RangeInclusive<u32> = 0..=65_536;
const CONN_HANDSHAKE_RANGE: RangeInclusive<u32> = 1..=600;

/// One table for both pages. A bound enforced on the global default but not on a machine's
/// override is a bound an operator walks around by typing the number on the other screen.
pub(crate) fn validate_node_connection(connection: &NodeConnection) -> Result<()> {
    conn_in_range(
        "connection.conn_idle_secs",
        connection.conn_idle_secs,
        &CONN_IDLE_RANGE,
    )?;
    conn_in_range(
        "connection.conn_uplink_only_secs",
        connection.uplink_only_secs,
        &CONN_HALF_CLOSE_RANGE,
    )?;
    conn_in_range(
        "connection.conn_downlink_only_secs",
        connection.downlink_only_secs,
        &CONN_HALF_CLOSE_RANGE,
    )?;
    conn_in_range(
        "connection.conn_buffer_size_kb",
        connection.buffer_size_kb,
        &CONN_BUFFER_KB_RANGE,
    )
}

fn validate_connection_settings(connection: &ConnectionSettings) -> Result<()> {
    conn_in_range(
        "settings.connection.conn_idle_secs",
        Some(connection.conn_idle_secs),
        &CONN_IDLE_RANGE,
    )?;
    conn_in_range(
        "settings.connection.uplink_only_secs",
        Some(connection.uplink_only_secs),
        &CONN_HALF_CLOSE_RANGE,
    )?;
    conn_in_range(
        "settings.connection.downlink_only_secs",
        Some(connection.downlink_only_secs),
        &CONN_HALF_CLOSE_RANGE,
    )?;
    conn_in_range(
        "settings.connection.buffer_size_kb",
        connection.buffer_size_kb,
        &CONN_BUFFER_KB_RANGE,
    )?;
    conn_in_range(
        "settings.connection.handshake_secs",
        Some(connection.handshake_secs),
        &CONN_HANDSHAKE_RANGE,
    )
}

/// `None` passes: on a machine it means "take the global default", and in the settings it
/// means "write no key and let xray decide". Neither is a number to be judged.
fn conn_in_range(location: &str, value: Option<u32>, range: &RangeInclusive<u32>) -> Result<()> {
    match value {
        Some(value) if !range.contains(&value) => Err(StoreError::InvalidData(format!(
            "{location} 要在 {} 到 {} 之间，收到 {value}",
            range.start(),
            range.end()
        ))),
        _ => Ok(()),
    }
}

/// The database has the same CHECK; this blocks it as a 400 first and states the shape.
///
/// It checks only the shape, not whether the cron expression itself is valid. That is judged
/// by xray's cron library, and reproducing a parser here means two implementations that
/// eventually disagree — the right number of fields passes, and getting it wrong costs that
/// machine its updates and nothing else.
fn validate_geodata(settings: &GeodataSettings) -> Result<()> {
    // A `CRON_TZ=` / `TZ=` prefix is allowed (robfig/cron v3's extension) and is stripped
    // before counting fields. Whether the prefix itself is valid (whether the zone name is
    // known) is judged by xray's `time.LoadLocation`.
    let spec = settings.cron.trim();
    let fields = spec
        .strip_prefix("CRON_TZ=")
        .or_else(|| spec.strip_prefix("TZ="))
        .and_then(|rest| rest.split_once(char::is_whitespace))
        .map_or(spec, |(_zone, rest)| rest);
    if fields.split_whitespace().count() != 5 {
        return Err(StoreError::InvalidData(format!(
            "settings.geodata.cron 要五段（分 时 日 月 周），可选前缀 CRON_TZ=<时区>；收到 {:?}",
            settings.cron
        )));
    }
    for (location, url) in [
        ("settings.geodata.geoip_url", &settings.geoip_url),
        ("settings.geodata.geosite_url", &settings.geosite_url),
    ] {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(StoreError::InvalidData(format!(
                "{location} 必须是 http:// 或 https:// 开头，收到 {url:?}"
            )));
        }
    }
    Ok(())
}

fn validate_version(value: Option<&str>, location: &str) -> Result<Option<(u64, u64, u64)>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let version = parse_semver3(value)
        .ok_or_else(|| StoreError::InvalidData(format!("{location} must use x.y.z format")))?;
    if [version.0, version.1, version.2]
        .into_iter()
        .any(|part| part > 255)
    {
        return Err(StoreError::InvalidData(format!(
            "{location} components must be between 0 and 255"
        )));
    }
    Ok(Some(version))
}

fn optional_u64(location: &str, value: Option<i64>) -> Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
        })
        .transpose()
}

fn optional_i64(location: &str, value: Option<u64>) -> Result<Option<i64>> {
    value
        .map(|value| {
            i64::try_from(value)
                .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
        })
        .transpose()
}
