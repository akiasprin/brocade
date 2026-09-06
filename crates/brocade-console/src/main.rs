use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use axum::Router;
use brocade_console::http::{
    admin_router_with_wakes_and_realtime, agent_router_with_origin_and_realtime,
    merged_router_with_wakes_and_realtime, with_console_static, with_console_static_dir,
};
use brocade_store::PgStore;
use tokio::sync::Notify;

/// How long raw readings are retained. The difference needs only the most recent one, and a week
/// is so that accounts can be reconciled after an incident.
const USAGE_READING_RETAIN_DAYS: u32 = 7;

/// How long telemetry is retained. A week for the same reason as above — long enough to look back
/// at an incident after the weekend — but the resemblance stops there: usage_readings are kept so
/// that money can be re-derived, while these are the finished article and worth nothing once old.
///
/// This is the first knob to turn if the tables get heavy. Shortening it costs history; shortening
/// the sampling interval instead would cost the resolution that makes a CPU spike visible at all,
/// which is the one thing this data is for.
const LOAD_SAMPLE_RETAIN_DAYS: u32 = 7;

/// How often quotas are checked. Usage reports in 30-second windows, so anything denser has no new
/// data to look at; with the agent's 15-second fetch interval, going over to being cut off takes
/// about two minutes.
const QUOTA_TICK: Duration = Duration::from_secs(60);
/// Durable permission jobs normally arrive through a wake signal.  The timer covers a lost signal,
/// another process writing the row, and restart recovery.
const GRANTS_TICK: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        env::var("DATABASE_URL").map_err(|_| "DATABASE_URL must be set for brocade-console")?;
    let admin_bind: SocketAddr = env::var("BROCADE_ADMIN_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()?;
    // Absent means one listener carrying both faces. Splitting them costs a second open port, a
    // second reverse-proxy entry and a second certificate, and it buys nothing on its own — the
    // two route tables do not overlap. It buys something only when the deployment wants to treat
    // them differently (an allow-list in front of the console while the machines reach the agent
    // face), and that is a decision the operator makes, so it waits to be asked for.
    //
    // `BROCADE_BIND` is the older spelling of the same request and still counts as asking.
    let agent_bind: Option<SocketAddr> =
        match env::var("BROCADE_AGENT_BIND").or_else(|_| env::var("BROCADE_BIND")) {
            Ok(value) => Some(value.parse()?),
            Err(_) => None,
        };

    // A first start otherwise stops on a database nobody has created yet, which is a step with no
    // decision in it: the name is already in DATABASE_URL, and the process is about to create every
    // table inside it anyway. Creating the database is the same act as `migrate()` one level out.
    //
    // Said out loud, because the condition it acts on — "the server has no such database" — is also
    // what a typo in DATABASE_URL looks like. Silently, this would create the typo, migrate it, and
    // serve an empty console that looks perfectly healthy, which reads as "the records are gone".
    // One line here turns that into something visible before the first request.
    if let Some(created) = PgStore::create_database_if_absent(&database_url).await? {
        eprintln!(
            "brocade-console created the database {created:?} — the server did not have it. \
             If that name is not the one you meant, stop now and check DATABASE_URL: \
             what comes up next will be empty."
        );
    }
    let store = PgStore::connect(&database_url).await?;
    let default_warps = store.migrate().await?;
    if default_warps > 0 {
        eprintln!(
            "brocade-console created {default_warps} missing tenant default WARP resource(s)"
        );
    }
    if store.ensure_default_app_group().await? {
        eprintln!("brocade-console created the line group 默认分组");
    }
    let default_certificates = store.ensure_default_self_signed_pool().await?;
    if default_certificates > 0 {
        eprintln!(
            "brocade-console created 默认自签证书组 with {default_certificates} self-signed certificates"
        );
    }
    // Only the policy is durable. The service below is deliberately created once and shared by
    // both listener faces; creating one per router would leave the browser leasing one instance
    // while the Agent connected to another, and no samples would ever start.
    let realtime =
        brocade_console::realtime::RealtimeService::new(store.realtime_telemetry_policy().await?);

    // The console front end normally rides inside this binary (argued at the top of build.rs).
    // Set to a directory, this serves that directory instead — for iterating on the front end
    // without recompiling the control plane, or for patching the UI on a machine where rebuilding
    // is not an option. Nothing to configure otherwise, which is the point.
    let console_dist = env::var("BROCADE_CONSOLE_DIST")
        .ok()
        .filter(|dir| !dir.trim().is_empty());

    // What goes into the enrolment command when BROCADE_AGENT_PUBLIC_URL is unset. It has to name
    // the address the agent face actually ended up on, which depends on whether it was split off.
    let agent_origin = format!("http://{}", agent_bind.unwrap_or(admin_bind));

    let admin_listener = tokio::net::TcpListener::bind(admin_bind).await?;
    let agent_listener = match agent_bind {
        Some(bind) => Some(tokio::net::TcpListener::bind(bind).await?),
        None => None,
    };
    // Said out loud on every start: "embedded" is what the deployed binary should report, and
    // seeing a path here on a production machine is the whole explanation for a console that does
    // not match the API next to it.
    let ui = match &console_dist {
        Some(dir) => format!("from {dir}"),
        None => "embedded".to_owned(),
    };
    match agent_bind {
        Some(bind) => {
            eprintln!("brocade-console admin listening on http://{admin_bind} (console ui: {ui})");
            eprintln!("brocade-console agent listening on http://{bind}");
        }
        None => eprintln!(
            "brocade-console listening on http://{admin_bind} \
             (console + agent on one listener; console ui: {ui})"
        ),
    }

    // Retention cleanup. The control plane had no background loop before this one —
    // usage_readings appends a row per label every 30 seconds and never reclaims, so without
    // someone clearing it, it does not last long.
    //
    // Once an hour suffices (retention is measured in days), DELETE is idempotent, and several
    // instances running at once do not fight. Failure logs without exiting: an uncleanable table
    // is a disk problem and should not take the whole control plane down.
    let pruner = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match pruner.prune_usage_readings(USAGE_READING_RETAIN_DAYS).await {
                Ok(0) => {}
                Ok(rows) => eprintln!("usage: 清掉 {rows} 条过期读数"),
                Err(error) => eprintln!("usage: 清理读数失败：{error}"),
            }
            match pruner.prune_load_samples(LOAD_SAMPLE_RETAIN_DAYS).await {
                Ok(0) => {}
                Ok(rows) => eprintln!("load: 清掉 {rows} 条过期遥测"),
                Err(error) => eprintln!("load: 清理遥测失败：{error}"),
            }
        }
    });

    // Permission automation.  The model write and its outbox row are one transaction in store;
    // this loop only turns queued rows into non-disruptive grants deployments.  It folds all due
    // rows into the newest revision, while an already-created deployment remains immutable.
    let grants_wake = Arc::new(Notify::new());
    let grants_store = store.clone();
    let grants_signal = grants_wake.clone();
    tokio::spawn(async move {
        let mut last_waiting = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(GRANTS_TICK) => {}
                _ = grants_signal.notified() => {}
            }
            match grants_store.process_grant_automation().await {
                Ok(outcome) => {
                    if let Some(id) = outcome.deployment_id {
                        last_waiting = None;
                        eprintln!(
                            "grants: 合并 {} 次权限变更，已创建自动化授权单 #{id}（修订 {}）",
                            outcome.merged_jobs,
                            outcome.revision_id.unwrap_or_default()
                        );
                    } else if let Some(waiting) = outcome.waiting {
                        if outcome.merged_jobs > 0
                            && last_waiting.as_deref() != Some(waiting.as_str())
                        {
                            eprintln!("grants: {waiting}");
                        }
                        last_waiting = Some(waiting);
                    } else {
                        last_waiting = None;
                    }
                }
                Err(error) => eprintln!("grants: 自动化队列处理失败：{error}"),
            }
        }
    });

    // Quota enforcement. Unlike the cleanup loop above this one has side effects: it stamps
    // revisions and releases. Three constraints are fixed in store's quota.rs rather than here —
    // a round composes into one revision; it releases only where the plan is nothing but
    // SyncGrants (pushing grants and never configuration, so never a destructive release); and
    // being blocked leaves only a log line. Failure does not exit: a database hiccup or a
    // collision with somebody else's release is simply retried next round under the same
    // idempotency key.
    let quota_wake = Arc::new(Notify::new());
    let quota_store = store.clone();
    let quota_signal = quota_wake.clone();
    tokio::spawn(async move {
        loop {
            // Two triggers: the timer, and somebody having just changed a quota. A quota change
            // must take effect at once — lowering one cuts people off and raising one restores
            // them, and having somebody watch the UI for a minute is indefensible.
            tokio::select! {
                _ = tokio::time::sleep(QUOTA_TICK) => {}
                _ = quota_signal.notified() => {}
            }
            match quota_store.enforce_quotas().await {
                Ok(outcome) => {
                    if outcome.suspended > 0 || outcome.restored > 0 {
                        eprintln!(
                            "quota: 撤 {} 条、恢复 {} 条授权，修订 {}",
                            outcome.suspended, outcome.restored, outcome.revision_id
                        );
                    }
                    if let Some(id) = outcome.deployment_id {
                        eprintln!("quota: 已发布权限单 #{id}");
                    }
                    if !outcome.deferred.is_empty() {
                        eprintln!(
                            "quota: {} 的名单要等配置单先落地（这几台还欠着未发布的配置改动）",
                            outcome.deferred.join("、")
                        );
                    }
                }
                Err(error) => eprintln!("quota: 这一轮失败：{error}"),
            }
        }
    });

    // On a stop signal, finish the requests in hand before exiting. Without this layer a
    // `systemctl restart` landing at the instant of dispatch has a real cost: the transaction
    // claiming desired has committed while the response has not been written, so the agent
    // received nothing and the row already reads `dispatched`, and it waits out a full 15-minute
    // lease before it can claim work again (DISPATCH_LEASE_INTERVAL in `deployment.rs`).
    // Both listening surfaces need the signal, hence a watch broadcast rather than passing one
    // future twice.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        eprintln!("brocade-console 收到停止信号，等手上的请求做完");
        let _ = shutdown_tx.send(true);
    });

    // Certificate issuance. Started before the listeners so that a control plane coming up with
    // certificates already configured begins catching up on renewals without waiting for somebody
    // to open the page.
    //
    // Failure here is not fatal and cannot be: an ACME server being unreachable, or a DNS
    // credential having expired, must not stop the control plane from serving a fleet that is
    // otherwise running fine. The consequences land in the console instead, per node.
    let cert_wake = brocade_console::certs::spawn(store.clone());

    // The machine list's country flags. Started here rather than looked up inside the request,
    // where a cancelled request used to strand the retry state for an hour with nothing in the
    // journal to say so — see the module for the whole story. It reads the copy on disk first, so
    // a restart shows countries before the download finishes, and before it is even attempted.
    let geoip = brocade_console::geoip::spawn(store.clone());

    // Which of the two sources the front end comes from is the same decision on both listener
    // layouts, so it is made once here rather than at each of the two call sites — where the two
    // could drift into disagreeing.
    let with_console = |router: Router| match &console_dist {
        Some(dir) => with_console_static_dir(router, dir),
        None => with_console_static(router),
    };

    match agent_listener {
        // Split: the console keeps the static fallback, the agent face is on its own.
        Some(agent_listener) => {
            let admin = axum::serve(
                admin_listener,
                with_console(admin_router_with_wakes_and_realtime(
                    store.clone(),
                    quota_wake,
                    grants_wake,
                    cert_wake,
                    geoip,
                    realtime.clone(),
                )),
            )
            .with_graceful_shutdown(shutdown_when(shutdown_rx.clone()));
            let agent = axum::serve(
                agent_listener,
                agent_router_with_origin_and_realtime(store, agent_origin, realtime),
            )
            .with_graceful_shutdown(shutdown_when(shutdown_rx));
            tokio::try_join!(admin, agent)?;
        }
        // The default. The static fallback still goes on last, so it catches only what neither
        // face claimed — the agent's paths are real routes and win over it.
        None => {
            axum::serve(
                admin_listener,
                with_console(merged_router_with_wakes_and_realtime(
                    store,
                    quota_wake,
                    grants_wake,
                    cert_wake,
                    geoip,
                    agent_origin,
                    realtime,
                )),
            )
            .with_graceful_shutdown(shutdown_when(shutdown_rx))
            .await?;
        }
    }
    Ok(())
}

/// Both SIGTERM (what systemd stops a service with) and Ctrl-C count.
///
/// SIGINT alone is not enough: production runs under systemd, `systemctl restart` sends SIGTERM,
/// and an unhandled SIGTERM's default action is immediate termination — so graceful shutdown would
/// be absent exactly where it is needed most.
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            // A handler that cannot be installed is treated as that path not existing, rather
            // than dragging the whole shutdown chain down
            Err(error) => {
                eprintln!("装 SIGTERM 处理器失败，只剩 Ctrl-C 那一路：{error}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
}

async fn shutdown_when(mut rx: tokio::sync::watch::Receiver<bool>) {
    // The signal may arrive before `with_graceful_shutdown` receives this future, so the current
    // value is checked first and changes awaited after — awaiting `changed()` alone misses that
    // one, and the slower of the two surfaces never closes.
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}
