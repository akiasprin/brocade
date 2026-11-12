//! Native Sub-Store client and bounded persistent worker. No remote response body or URL is logged.
use brocade_store::{subscription_connector as db, AdminContext, PgStore, StoreError};
use reqwest::{Client, Method, Url};
use serde_json::{json, Value};
use sqlx::Row;
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, StoreError>;
fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData(message.into())
}
#[derive(Clone, Debug)]
struct Failure {
    message: &'static str,
    http: Option<u16>,
    retry: bool,
}
impl Failure {
    fn new(message: &'static str) -> Self {
        Self {
            message,
            http: None,
            retry: false,
        }
    }
}

pub fn validate_endpoint(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| invalid("请输入完整的 http(s) 后端地址"))?;
    if value.len() > 4096
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "后端地址不能含账号、查询参数或片段；填写 api= 后的完整地址",
        ));
    }
    if let Some(host) = url
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
    {
        if !allowed_ip(host) {
            return Err(invalid("后端地址不可使用链路本地、广播或多播地址"));
        }
    }
    Ok(url)
}
fn allowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !v.is_link_local() && !v.is_multicast() && !v.is_unspecified() && !v.is_broadcast()
        }
        IpAddr::V6(v) => {
            !v.is_unicast_link_local()
                && !v.is_multicast()
                && !v.is_unspecified()
                && v.to_ipv4_mapped().is_none_or(|v| allowed_ip(v.into()))
        }
    }
}
struct Native {
    client: Client,
    base: Url,
    auth: db::Credentials,
}
impl Native {
    async fn new(auth: db::Credentials) -> std::result::Result<Self, Failure> {
        let base = validate_endpoint(&auth.endpoint).map_err(|_| Failure::new("后端地址无效"))?;
        let host = base
            .host_str()
            .ok_or_else(|| Failure::new("后端地址无效"))?
            .trim_matches(['[', ']']);
        let port = base
            .port_or_known_default()
            .ok_or_else(|| Failure::new("后端地址无效"))?;
        let addresses = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| Failure {
            message: "后端 DNS 超时",
            http: None,
            retry: true,
        })?
        .map_err(|_| Failure {
            message: "后端 DNS 查询失败",
            http: None,
            retry: true,
        })?
        .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|a| !allowed_ip(a.ip())) {
            return Err(Failure::new("后端 DNS 指向不允许的地址"));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| Failure::new("无法初始化后端连接"))?;
        Ok(Self { client, base, auth })
    }
    fn url(&self, path: &str) -> Url {
        let mut u = self.base.clone();
        u.set_path(&format!(
            "{}/{}",
            self.base.path().trim_end_matches('/'),
            path
        ));
        u
    }
    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
    ) -> std::result::Result<(u16, Value), Failure> {
        let mut req = self.client.request(method, url);
        if !self.auth.login.is_empty() {
            req = req.basic_auth(&self.auth.login, Some(&self.auth.password));
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let mut response = req.send().await.map_err(|e| Failure {
            message: if e.is_timeout() {
                "连接 Sub-Store 超时"
            } else if e.is_connect() {
                "无法连接 Sub-Store（检查网络、TLS 和地址）"
            } else {
                "Sub-Store 请求失败"
            },
            http: None,
            retry: true,
        })?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(Failure {
                message: match status {
                    401 | 403 => "Sub-Store 认证失败",
                    404 => "Sub-Store 接口不存在",
                    429 => "Sub-Store 请求过于频繁",
                    _ => "Sub-Store 返回错误",
                },
                http: Some(status),
                retry: status >= 500 || status == 429,
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure {
            message: "读取 Sub-Store 响应失败",
            http: Some(status),
            retry: true,
        })? {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(Failure::new("Sub-Store 响应超过 4 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Failure::new("Sub-Store 响应不是有效 JSON"))?;
        if value.get("status").and_then(Value::as_str) == Some("failed")
            || value.get("status").and_then(Value::as_str) == Some("error")
        {
            return Err(Failure {
                message: "Sub-Store 拒绝操作",
                http: Some(status),
                retry: false,
            });
        }
        Ok((status, value))
    }
    async fn source(&self, name: &str) -> std::result::Result<Option<Value>, Failure> {
        let (_, body) = self
            .request(Method::GET, self.url("api/subs"), None)
            .await?;
        let all = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Failure::new("无法识别 Sub-Store 订阅接口"))?;
        Ok(all
            .iter()
            .find(|s| s.get("name").and_then(Value::as_str) == Some(name))
            .cloned())
    }
}
/// Tests are read-only and use unsaved credentials without persisting or echoing them.
pub async fn test(auth: db::Credentials) -> Result<()> {
    let n = Native::new(auth).await.map_err(|e| invalid(e.message))?;
    n.source("__brocade_connectivity_only__")
        .await
        .map_err(|e| invalid(e.message))?;
    Ok(())
}
pub fn spawn(store: PgStore, origin: String, shutdown: crate::lifecycle::ShutdownSignal) {
    for lane in 0..4 {
        let store = store.clone();
        let origin = origin.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let Ok(nonce) = now_nonce() else {
                tracing::error!("connector entropy unavailable");
                return;
            };
            let owner = format!("{}-{lane}-{nonce}", std::process::id());
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut schedule = 0u64;
            loop {
                tokio::select! {_=shutdown.requested()=>break,_=tick.tick()=>{}}
                if lane == 0 && schedule.is_multiple_of(15) {
                    // Bounded scheduling and retention; unavailable stores get another pass.
                    if let Ok(due) = db::due(store.pool()).await {
                        for (tenant, user) in due {
                            let _ = db::enqueue(
                                store.pool(),
                                &AdminContext::system_admin("connector-worker"),
                                &tenant,
                                &user,
                                "inspect",
                                None,
                                "定时核验",
                            )
                            .await;
                        }
                    }
                    if let Err(_error) = db::prune(store.pool()).await {
                        tracing::warn!("connector retention failed");
                    }
                }
                schedule += 1;
                match db::claim(store.pool(), &owner).await {
                    Ok(Some((tenant, user, run))) => {
                        // Cancellation drops the dedicated lock connection; lease recovery resumes later.
                        tokio::select! {_=shutdown.requested()=>break,r=execute(&store,&origin,&owner,&tenant,&user,run)=>{if r.is_err(){tracing::warn!("connector task persistence failed");}}}
                    }
                    Ok(None) => {}
                    Err(_) => tracing::warn!("connector task claim failed"),
                }
            }
        });
    }
}
fn now_nonce() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(brocade_core::hash::hex_lower(&bytes))
}
async fn event(store: &PgStore, owner: &str, r: &mut db::Run, name: &str, cp: usize) -> Result<()> {
    if let Some(a) = r.attempts.last_mut() {
        a.events.push(db::Event {
            name: name.into(),
            checkpoint: cp,
            state: "running".into(),
            ms: None,
            http: None,
            detail: None,
        });
    }
    if !db::progress(store.pool(), owner, r).await? {
        return Err(invalid("task canceled"));
    }
    Ok(())
}
async fn done(
    store: &PgStore,
    owner: &str,
    r: &mut db::Run,
    started: Instant,
    http: Option<u16>,
) -> Result<()> {
    if let Some(e) = r.attempts.last_mut().and_then(|a| a.events.last_mut()) {
        e.state = "done".into();
        e.ms = Some(started.elapsed().as_millis() as u64);
        e.http = http;
    }
    if !db::progress(store.pool(), owner, r).await? {
        return Err(invalid("task canceled"));
    }
    Ok(())
}
/// Serialized with configuration mutations by the same per-user database advisory lock.
pub async fn execute(
    store: &PgStore,
    fallback_origin: &str,
    owner: &str,
    tenant: &str,
    user: &str,
    mut run: db::Run,
) -> Result<()> {
    let mut lock = store.pool().acquire().await?;
    lock.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended(jsonb_build_array($1::text,$2::text)::text,72117))").bind(tenant).bind(user).execute(&mut *lock).await?;
    let config=sqlx::query("SELECT version,remote_name,metadata FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2 AND sealed IS NOT NULL").bind(tenant).bind(user).fetch_optional(store.pool()).await?;
    let Some(c) = config else {
        return db::finish(store.pool(), owner, &run, "superseded", false).await;
    };
    if c.get::<i64, _>("version") != run.config_version {
        return db::finish(store.pool(), owner, &run, "superseded", false).await;
    }
    if run.attempts.len() >= 3 {
        run.error = Some("任务恢复达到三次上限，请检查后手动重试".into());
        return db::finish(store.pool(), owner, &run, "failed", false).await;
    }
    for a in &mut run.attempts {
        if a.status == "running" {
            a.status = "interrupted".into();
            for e in &mut a.events {
                if e.state == "running" {
                    e.state = "interrupted".into();
                }
            }
        }
    }
    run.error = None;
    run.attempts.push(db::Attempt {
        number: run.attempts.len() + 1,
        status: "running".into(),
        events: vec![],
    });
    db::progress(store.pool(), owner, &run).await?;
    let remote: String = c.try_get("remote_name")?;
    let metadata: Value = c.try_get("metadata")?;
    let credentials = db::credentials(store.pool(), tenant, user)
        .await?
        .ok_or_else(|| invalid("connector removed"))?;
    let result = perform(
        store,
        fallback_origin,
        owner,
        tenant,
        user,
        &remote,
        &metadata,
        credentials,
        &mut run,
    )
    .await;
    match result {
        Ok(()) => {
            if let Some(a) = run.attempts.last_mut() {
                a.status = "succeeded".into();
            }
            db::finish(store.pool(), owner, &run, "succeeded", false).await?;
        }
        Err(failure) => {
            let registered = run.attempts.last().is_some_and(|a| {
                a.events
                    .iter()
                    .any(|e| e.checkpoint == 0 && e.state == "done" && e.name == "读回来源")
            });
            run.error = Some(failure.message.into());
            if let Some(a) = run.attempts.last_mut() {
                a.status = "failed".into();
                if let Some(e) = a.events.last_mut() {
                    if e.state == "running" {
                        e.state = "failed".into();
                        e.http = failure.http;
                        e.detail = Some(failure.message.into());
                    }
                }
            }
            let retry = failure.retry && run.attempts.len() < 3;
            db::finish(
                store.pool(),
                owner,
                &run,
                if retry {
                    "retry_wait"
                } else if registered {
                    "partial"
                } else {
                    "failed"
                },
                retry,
            )
            .await?;
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn perform(
    store: &PgStore,
    fallback: &str,
    owner: &str,
    tenant: &str,
    user: &str,
    remote: &str,
    metadata: &Value,
    credentials: db::Credentials,
    r: &mut db::Run,
) -> std::result::Result<(), Failure> {
    macro_rules! persist {
        ($e:expr) => {
            $e.map_err(|_| Failure::new("任务已停止或状态保存失败"))?
        };
    }
    if r.kind == "cleanup" {
        persist!(event(store, owner, r, "核验远端来源归属", 0).await);
        let n = Native::new(credentials).await?;
        let old = n.source(remote).await?;
        if old
            .as_ref()
            .is_some_and(|v| v.get("_brocadeOwner").and_then(Value::as_str) != Some(remote))
        {
            return Err(Failure::new("远端来源归属不符，未删除"));
        }
        persist!(event(store, owner, r, "清理远端来源", 0).await);
        let start = Instant::now();
        let http = if old.is_some() {
            Some(
                n.request(Method::DELETE, n.url(&format!("api/sub/{remote}")), None)
                    .await?
                    .0,
            )
        } else {
            None
        };
        persist!(done(store, owner, r, start, http).await);
        persist!(sqlx::query("UPDATE subscription_connectors SET registered=false,verified=false,auto=false,metadata=jsonb_set(metadata,'{auto}','false'),last_confirmed=NULL WHERE tenant_id=$1 AND user_id=$2 AND version=$3").bind(tenant).bind(user).bind(r.config_version).execute(store.pool()).await.map_err(StoreError::from));
        return Ok(());
    }
    persist!(event(store, owner, r, "核对生效订阅", 0).await);
    let start = Instant::now();
    let admin = AdminContext::system_admin("connector-worker");
    let subscription = store
        .clash_subscription_for_user(&admin, tenant, user)
        .await
        .map_err(|_| Failure::new("用户订阅尚未生效、已停用或正在发布"))?;
    r.source_version = Some(subscription.revision as i64);
    let settings = store
        .distribution()
        .await
        .map_err(|_| Failure::new("无法读取订阅入口"))?;
    let origin = std::env::var("BROCADE_SUBSCRIPTION_PUBLIC_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or(settings.agent_public_url)
        .or_else(|| std::env::var("BROCADE_AGENT_PUBLIC_URL").ok())
        .unwrap_or_else(|| fallback.to_owned());
    let origin = origin.trim_end_matches('/');
    if validate_endpoint(origin).is_err() {
        return Err(Failure::new("订阅公开入口未配置"));
    }
    let source_url = format!("{origin}/sub/v1/{}/clash.yaml", subscription.uuid);
    persist!(done(store, owner, r, start, None).await);
    persist!(event(store, owner, r, "连接 Sub-Store", 0).await);
    let start = Instant::now();
    let n = Native::new(credentials).await?;
    let old = n.source(remote).await?;
    if old
        .as_ref()
        .is_some_and(|v| v.get("_brocadeOwner").and_then(Value::as_str) != Some(remote))
    {
        return Err(Failure::new("远端来源同名但归属不符，未覆盖"));
    }
    persist!(done(store, owner, r, start, Some(200)).await);
    if r.kind == "verify" && old.is_none() {
        return Err(Failure::new("远端来源不存在，请先同步"));
    }
    let correct = old.as_ref().is_some_and(|v| {
        v.get("url").and_then(Value::as_str) == Some(&source_url)
            && v.get("source").and_then(Value::as_str) == Some("remote")
            && v.get("displayName") == metadata.get("title")
    });
    if r.kind != "verify" && !correct {
        persist!(
            event(
                store,
                owner,
                r,
                if old.is_some() {
                    "更新来源地址"
                } else {
                    "创建来源"
                },
                0
            )
            .await
        );
        let start = Instant::now();
        let patch = json!({"url":source_url,"source":"remote","displayName":metadata["title"]});
        let (method, path, body) = if old.is_some() {
            (Method::PATCH, format!("api/sub/{remote}"), patch)
        } else {
            let mut b = patch;
            b["name"] = json!(remote);
            b["_brocadeOwner"] = json!(remote);
            (Method::POST, "api/subs".into(), b)
        };
        let (http, _) = n.request(method, n.url(&path), Some(body)).await?;
        persist!(done(store, owner, r, start, Some(http)).await);
    }
    persist!(event(store, owner, r, "读回来源", 0).await);
    let start = Instant::now();
    let check = n
        .source(remote)
        .await?
        .ok_or_else(|| Failure::new("来源写入后未找到，等待重新核验"))?;
    if check.get("_brocadeOwner").and_then(Value::as_str) != Some(remote)
        || check.get("url").and_then(Value::as_str) != Some(&source_url)
        || check.get("source").and_then(Value::as_str) != Some("remote")
    {
        return Err(Failure::new("远端来源已变化，未确认登记"));
    }
    persist!(sqlx::query("UPDATE subscription_connectors SET registered=true,verified=false WHERE tenant_id=$1 AND user_id=$2 AND version=$3").bind(tenant).bind(user).bind(r.config_version).execute(store.pool()).await.map_err(StoreError::from));
    persist!(done(store, owner, r, start, Some(200)).await);
    persist!(event(store, owner, r, "主动拉取订阅", 1).await);
    let start = Instant::now();
    let probe = now_nonce().map_err(|_| Failure::new("无法生成拉取核验标识"))?;
    persist!(sqlx::query("UPDATE subscription_connector_runs SET probe_token=$2,observed_revision=NULL,observed_fingerprint=NULL WHERE id=$1 AND lease_owner=$3").bind(r.id).bind(&probe).bind(owner).execute(store.pool()).await.map_err(StoreError::from));
    let mut download = n.url(&format!("download/{remote}"));
    download
        .query_pairs_mut()
        .append_pair("target", "JSON")
        .append_pair("noCache", "true")
        .append_pair("noFlow", "true")
        .append_pair("ignoreFailedRemoteSub", "false")
        .append_pair("url", &format!("{source_url}?brocade_probe={probe}"));
    let (download_http, nodes) = n.request(Method::GET, download, None).await?;
    let observed: Option<String> = persist!(sqlx::query_scalar(
        "SELECT observed_fingerprint FROM subscription_connector_runs WHERE id=$1"
    )
    .bind(r.id)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::from));
    let expected = brocade_store::secrets::subscription_fingerprint(
        tenant,
        user,
        &subscription.uuid,
        &subscription.content,
    )
    .map_err(|_| Failure::new("无法核验订阅版本"))?;
    if observed.as_deref() != Some(&expected) {
        return Err(Failure::new("转换返回但未收到当前订阅拉取回执"));
    }
    persist!(done(store, owner, r, start, Some(download_http)).await);
    persist!(event(store, owner, r, "验证转换结果", 2).await);
    let start = Instant::now();
    let array = nodes
        .as_array()
        .ok_or_else(|| Failure::new("Sub-Store 转换未返回节点列表"))?;
    r.output_count = Some(array.len());
    // A newer serving generation makes the receipt historical evidence, never current success.
    let current = store
        .clash_subscription_for_user(&admin, tenant, user)
        .await
        .map_err(|_| Failure::new("用户订阅在核验期间发生变化"))?;
    if current.revision != subscription.revision
        || current.uuid != subscription.uuid
        || current.content != subscription.content
    {
        return Err(Failure::new("用户订阅有新版本，请重新同步"));
    }
    persist!(sqlx::query("UPDATE subscription_connectors SET verified=true,last_confirmed=now(),source_version=$4,source_fingerprint=$5,next_check=now()+interval '5 minutes' WHERE tenant_id=$1 AND user_id=$2 AND version=$3").bind(tenant).bind(user).bind(r.config_version).bind(r.source_version).bind(&expected).execute(store.pool()).await.map_err(StoreError::from));
    persist!(done(store, owner, r, start, None).await);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_are_validated_without_reflecting_secrets() {
        for s in [
            "file:///etc/passwd",
            "https://u:p@host/secret",
            "http://169.254.169.254/",
            "https://host/path?key=secret",
            "https://host/#secret",
            "http://[fe80::1]/",
        ] {
            assert!(validate_endpoint(s).is_err());
        }
        for s in [
            "http://127.0.0.1:3001/private",
            "https://sub.example/private",
            "http://192.168.1.2:3001/",
        ] {
            assert!(validate_endpoint(s).is_ok());
        }
        assert!(!allowed_ip("::ffff:169.254.169.254".parse().unwrap()));
    }
}
