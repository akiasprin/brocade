use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use axum::Router;
use brocade_console::http::{
    admin_router_with_services, merged_router_with_wakes_and_realtime, with_console_branding,
    with_console_static, with_console_static_dir, BootstrapCredential, ConsoleServices,
};
use brocade_console::lifecycle::{
    serve_http_surfaces, ShutdownOutcome, ShutdownSignal, DEFAULT_SHUTDOWN_GRACE,
};
use brocade_store::PgStore;
use tokio::sync::Notify;

/// How often quotas are checked. Usage reports in 30-second windows, so anything denser has no new
/// data to look at; with the agent's 15-second fetch interval, going over to being cut off takes
/// about two minutes.
const QUOTA_TICK: Duration = Duration::from_secs(60);
/// Durable permission jobs normally arrive through a wake signal.  The timer covers a lost signal,
/// another process writing the row, and restart recovery.
const GRANTS_TICK: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if database_command().await? {
        return Ok(());
    }
    brocade_console::performance::Performance::initialize()?;
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
    let agent_bind: Option<SocketAddr> = match env::var("BROCADE_AGENT_BIND") {
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
    store.migrate().await?;
    let bootstrap_credential = match env::var("BROCADE_BOOTSTRAP_TOKEN") {
        Ok(secret) => Some(BootstrapCredential::new(&secret)?),
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => {
            return Err("BROCADE_BOOTSTRAP_TOKEN must be valid UTF-8".into());
        }
    };
    if !store.admin_auth_state().await?.initialized && bootstrap_credential.is_none() {
        eprintln!(
            "brocade-console has not been initialized; /auth/init is disabled until \
             BROCADE_BOOTSTRAP_TOKEN is configured"
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

    let listeners = brocade_console::socket_activation::open(admin_bind, agent_bind).await?;
    let admin_listener = listeners.admin;
    let agent_listener = listeners.agent;
    // Said out loud on every start: "embedded" is what the deployed binary should report, and
    // seeing a path here on a production machine is the whole explanation for a console that does
    // not match the API next to it.
    let ui = match &console_dist {
        Some(dir) => format!("from {dir}"),
        None => "embedded".to_owned(),
    };
    let listener_source = if listeners.socket_activated {
        "systemd socket activation"
    } else {
        "direct bind"
    };
    match agent_bind {
        Some(bind) => {
            eprintln!(
                "brocade-console admin listening on http://{admin_bind} \
                 ({listener_source}; console ui: {ui})"
            );
            eprintln!("brocade-console agent listening on http://{bind} ({listener_source})");
        }
        None => eprintln!(
            "brocade-console listening on http://{admin_bind} \
             (console + agent on one listener; {listener_source}; console ui: {ui})"
        ),
    }

    // Machine online/offline/IP-change events are always persisted. A generic webhook worker is
    // added when BROCADE_NOTIFICATION_WEBHOOK_URL is configured; its delivery state is durable.
    brocade_console::notifications::spawn(store.clone()).await;

    // Preserve recent raw telemetry; seal older history incrementally, never one large rewrite.
    // Skip missed ticks and bound the whole pass so catch-up cannot monopolise the database.
    let compactor = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match tokio::time::timeout(
                std::time::Duration::from_secs(20),
                compactor.compact_load_history(),
            )
            .await
            {
                Ok(Ok(0)) => {}
                Ok(Ok(rows)) => eprintln!("load: 将 {rows} 条历史窗口封存为分钟数据"),
                Ok(Err(error)) => eprintln!("load: 历史聚合失败：{error}"),
                Err(_) => eprintln!("load: 历史聚合达到本轮时间预算，下轮继续"),
            }
        }
    });

    // One capacity forecast per machine and UTC day, so a closed period can be checked against
    // what the forecast page said. The first pass after midnight records the day; hourly passes
    // only cover restarts and outages, since a day's first record stands.
    let forecaster = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match tokio::time::timeout(
                Duration::from_secs(60),
                forecaster.record_machine_traffic_forecasts(),
            )
            .await
            {
                Ok(Ok(0)) => {}
                Ok(Ok(rows)) => eprintln!("traffic: 记录 {rows} 台机器的当日流量预测"),
                Ok(Err(error)) => eprintln!("traffic: 记录流量预测失败：{error}"),
                Err(_) => eprintln!("traffic: 记录流量预测超时，下轮重试"),
            }
        }
    });

    // Each table owns its due time and bounded transaction. Backlogs retry every minute;
    // normally settled history runs hourly. One failure never skips another table.
    let pruner = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match pruner.run_data_retention().await {
                Ok(results) => {
                    for result in results {
                        if let Some(code) = result.error_code {
                            eprintln!(
                                "retention: {} 清理失败（{}），下轮重试",
                                result.table_name, code
                            );
                        } else if result.deleted_rows > 0 {
                            eprintln!(
                                "retention: {} 清理 {} 条过期记录",
                                result.table_name, result.deleted_rows
                            );
                        }
                    }
                }
                Err(error) => eprintln!("retention: 读取清理策略失败：{error}"),
            }
        }
    });

    // Initialization belongs to the server: closing the installation page cannot abandon it.
    let initialization_store = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(error) = initialization_store.process_node_initialization().await {
                eprintln!("node initialization: {error}");
            }
        }
    });

    // Permission automation.  The model write and its outbox row are one transaction in store;
    // this loop only turns queued rows into non-disruptive grants deployments. Once any row is
    // due it folds the whole queued batch into its newest revision, including backed-off rows;
    // an already-created deployment remains immutable and must settle before the next order.
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
    let user_traffic_store = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if let Err(error) = user_traffic_store.enforce_user_traffic_policies().await {
                eprintln!("user traffic: {error}");
            }
        }
    });

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
            match quota_store.enforce_user_service_periods().await {
                Ok(changed) if changed > 0 => {
                    eprintln!("user service: 已更新 {changed} 个用户的服务状态")
                }
                Ok(_) => {}
                Err(error) => eprintln!("user service: 有效期评估失败：{error}"),
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
            match quota_store.enforce_forwarding_quotas().await {
                Ok(outcome) if outcome.suspended > 0 || outcome.restored > 0 => eprintln!(
                    "forwarding quota: 暂停 {}、恢复 {} 个用户/机器配额，已创建自动化工单",
                    outcome.suspended, outcome.restored
                ),
                Ok(_) => {}
                Err(error) => eprintln!("forwarding quota: 自动化评估失败：{error}"),
            }
        }
    });

    // On a stop signal, finish the requests in hand before exiting. Without this layer a
    // `systemctl restart` landing at the instant of dispatch has a real cost: the transaction
    // claiming desired has committed while the response has not been written, so the agent
    // received nothing and the row already reads `dispatched`, and it waits out a full 15-minute
    // lease before it can claim work again (DISPATCH_LEASE_INTERVAL in `deployment.rs`).
    // Both listening surfaces, streaming responses, and upgraded connections share this signal.
    // The server lifecycle also starts its own hard deadline from the same transition, so a future
    // endpoint which forgets to observe it still cannot hold a systemd restart indefinitely.
    let shutdown = ShutdownSignal::new();
    let shutdown_trigger = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        eprintln!("brocade-console 收到停止信号，等手上的请求做完");
        shutdown_trigger.request();
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

    // External-tunnel checks originate here on the Console machine. Scheduling and run state are
    // durable in PostgreSQL; this process is only a leased executor, so restart and multi-instance
    // deployments cannot duplicate a check.
    brocade_console::tunnel_probe::spawn(store.clone());

    // Which of the two sources the front end comes from is the same decision on both listener
    // layouts, so it is made once here rather than at each of the two call sites — where the two
    // could drift into disagreeing.
    let branding_store = store.clone();
    let with_console = |router: Router| {
        with_console_branding(
            match &console_dist {
                Some(dir) => with_console_static_dir(router, dir),
                None => with_console_static(router),
            },
            branding_store.clone(),
        )
    };
    let admin_services =
        ConsoleServices::new(quota_wake, grants_wake, cert_wake, geoip, realtime.clone())
            .and_bootstrap_credential(bootstrap_credential)
            .and_shutdown(shutdown.clone());

    let outcome = match agent_listener {
        // Split: the console keeps the static fallback, the agent face is on its own.
        Some(agent_listener) => {
            let agent = admin_services
                .clone()
                .agent_router(store.clone(), agent_origin);
            let admin = with_console(admin_router_with_services(store.clone(), admin_services));
            serve_http_surfaces(
                (admin_listener, admin),
                Some((agent_listener, agent)),
                shutdown,
                DEFAULT_SHUTDOWN_GRACE,
            )
            .await?
        }
        // The default. The static fallback still goes on last, so it catches only what neither
        // face claimed — the agent's paths are real routes and win over it.
        None => {
            let router = with_console(merged_router_with_wakes_and_realtime(
                store,
                agent_origin,
                admin_services,
            ));
            serve_http_surfaces(
                (admin_listener, router),
                None,
                shutdown,
                DEFAULT_SHUTDOWN_GRACE,
            )
            .await?
        }
    };
    if outcome == ShutdownOutcome::DeadlineExceeded {
        eprintln!(
            "brocade-console 等待现有工作超过 {} 秒，结束剩余连接",
            DEFAULT_SHUTDOWN_GRACE.as_secs()
        );
    }
    Ok(())
}

/// Offline maintenance commands never create a missing database or start HTTP/background jobs.
async fn database_command() -> Result<bool, Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(false);
    }
    let migrate = match args.as_slice() {
        [command] if command == "database-check" => false,
        [command] if command == "database-migrate" => true,
        _ => return Err("usage: brocade-console [database-check|database-migrate]".into()),
    };
    let url = env::var("DATABASE_URL").map_err(|_| "DATABASE_URL must be set")?;
    let store = PgStore::connect(&url)
        .await
        .map_err(|_| "数据库连接失败；请检查原有数据库配置，不会创建新库")?;
    let check = store
        .check_database_upgrade()
        .await
        .map_err(|_| "无法读取数据库迁移历史")?;
    println!("{}", serde_json::to_string(&check)?);
    if !check.compatible {
        return Err("数据库不在支持的升级路径中，未执行迁移".into());
    }
    if migrate {
        store
            .migrate_database_schema()
            .await
            .map_err(|_| "数据库迁移失败；保留备份和迁移历史，请检查后重试")?;
    }
    Ok(true)
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
