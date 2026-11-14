use super::*;
use brocade_core::format::yaml::RulePreferences;
use brocade_store::clash_rules;

fn actor() -> ApiResult<AdminContext> {
    request_admin()
        .map(|admin| AdminContext::from_authenticated(&admin))
        .ok_or(ApiError::Unauthorized)
}
pub(super) fn allowed(method: &axum::http::Method, path: &str) -> bool {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["users", tenant, user, "clash-rules"] if !tenant.is_empty() && !user.is_empty() => {
            matches!(*method, axum::http::Method::GET | axum::http::Method::PUT)
        }
        ["users", tenant, user, "clash-rules", "preview" | "probe"]
            if !tenant.is_empty() && !user.is_empty() =>
        {
            *method == axum::http::Method::POST
        }
        _ => false,
    }
}
pub(super) async fn info(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
) -> ApiResult<Response> {
    Ok(subscription_no_store(
        Json(state.store.clash_rules(&actor()?, &tenant, &user).await?).into_response(),
    ))
}
pub(super) async fn save(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
    Json(write): Json<clash_rules::Write>,
) -> ApiResult<Response> {
    Ok(subscription_no_store(
        Json(
            state
                .store
                .save_clash_rules(&actor()?, &tenant, &user, write)
                .await?,
        )
        .into_response(),
    ))
}
pub(super) async fn preview(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
    Json(rules): Json<RulePreferences>,
) -> ApiResult<Response> {
    Ok(subscription_no_store(
        Json(
            json!({"yaml":state.store.preview_clash_rules(&actor()?,&tenant,&user,&rules).await?}),
        )
        .into_response(),
    ))
}

#[derive(Deserialize)]
pub(super) struct Probe {
    rules: RulePreferences,
    kind: brocade_core::format::yaml::RuleKind,
    value: String,
}
#[derive(Serialize)]
struct Hit {
    layer: &'static str,
    id: String,
    target: String,
    via: String,
}
// Only fixed, compiler-owned source URLs are fetched. Lookup inputs never leave the Console.
static SOURCES: std::sync::LazyLock<tokio::sync::Mutex<HashMap<String, Arc<Vec<String>>>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));
async fn source(id: &str, ip: bool) -> ApiResult<Option<Arc<Vec<String>>>> {
    let Some(url) = brocade_core::format::yaml::rule_source_url(id, ip) else {
        return Ok(None);
    };
    let mut cache = SOURCES.lock().await;
    if let Some(source) = cache.get(&url) {
        return Ok(Some(source.clone()));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| StoreError::Unavailable("规则集连接未就绪".into()))?;
    let mut response = client
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|_| StoreError::Unavailable("规则集暂时无法加载，请稍后重试".into()))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| StoreError::Unavailable("规则集读取失败，请稍后重试".into()))?
    {
        if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
            return Err(StoreError::Unavailable("规则集超出加载限制".into()).into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| StoreError::Unavailable("规则集内容无法识别".into()))?;
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    if lines.len() > 1_000_000 || lines.iter().any(|line| line.len() > 512) {
        return Err(StoreError::Unavailable("规则集条目超出加载限制".into()).into());
    }
    let total: usize = cache
        .values()
        .flat_map(|source| source.iter())
        .map(|line| line.len() + 32)
        .sum();
    if total + lines.iter().map(|line| line.len() + 32).sum::<usize>() > 64 * 1024 * 1024 {
        return Err(StoreError::Unavailable("规则集缓存已满，请重启 Console 后重试".into()).into());
    }
    let lines = Arc::new(lines);
    cache.insert(url, lines.clone());
    Ok(Some(lines))
}
async fn run_probe(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
    Json(input): Json<Probe>,
) -> ApiResult<Response> {
    use brocade_core::format::yaml::{self, RuleKind};
    let view = state.store.clash_rules(&actor()?, &tenant, &user).await?;
    input
        .rules
        .validate(&view.catalog)
        .map_err(StoreError::InvalidData)?;
    if input.value.is_empty()
        || input.value.len() > 253
        || input.value.contains([',', '\n', '\r', '\0'])
        || matches!(input.kind, RuleKind::Keyword)
    {
        return Err(StoreError::InvalidData("请输入域名、IP 或进程名".into()).into());
    }
    if matches!(input.kind, RuleKind::Ip) && input.value.parse::<IpAddr>().is_err() {
        return Err(StoreError::InvalidData("IP 格式无效".into()).into());
    }
    for rule in &input.rules.custom {
        if yaml::custom_rule_matches(rule, &input.kind, &input.value) {
            return Ok(subscription_no_store(
                Json(Hit {
                    layer: "custom",
                    id: rule.id.clone(),
                    target: rule.target.clone(),
                    via: rule.value.clone(),
                })
                .into_response(),
            ));
        }
    }
    if !matches!(input.kind, RuleKind::Process) {
        for module in yaml::routing_module_order(&input.rules, &view.catalog) {
            let preference = input.rules.modules.get(&module.id);
            if preference.is_some_and(|s| !s.on) {
                continue;
            }
            for id in &module.sets {
                if let Some(source) = source(id, matches!(input.kind, RuleKind::Ip)).await? {
                    if source.iter().any(|pattern| {
                        yaml::source_rule_matches(pattern, &input.kind, &input.value)
                    }) {
                        return Ok(subscription_no_store(
                            Json(Hit {
                                layer: "module",
                                id: module.id.clone(),
                                target: preference
                                    .map_or_else(|| module.standard.clone(), |s| s.target.clone()),
                                via: format!("RULE-SET,{id}"),
                            })
                            .into_response(),
                        ));
                    }
                }
            }
        }
        if let Some(source) = source("cn", matches!(input.kind, RuleKind::Ip)).await? {
            if source
                .iter()
                .any(|pattern| yaml::source_rule_matches(pattern, &input.kind, &input.value))
            {
                return Ok(subscription_no_store(
                    Json(Hit {
                        layer: "module",
                        id: "cn".into(),
                        target: input
                            .rules
                            .modules
                            .get("cn")
                            .map_or_else(|| "direct".into(), |s| s.target.clone()),
                        via: "RULE-SET,cn".into(),
                    })
                    .into_response(),
                ));
            }
        }
    }
    Ok(subscription_no_store(
        Json(Hit {
            layer: "final",
            id: "final".into(),
            target: input.rules.final_target,
            via: "MATCH".into(),
        })
        .into_response(),
    ))
}

pub(super) fn is_for_self(
    admin: &AuthenticatedAdmin,
    method: &axum::http::Method,
    path: &str,
) -> bool {
    let Some(user) = admin.self_user.as_ref() else {
        return false;
    };
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    allowed(method, path)
        && parts.get(1) == Some(&user.tenant_id.as_str())
        && parts.get(2) == Some(&user.user_id.as_str())
}

pub(super) async fn probe(
    state: State<AppState>,
    path: Path<(String, String)>,
    input: Json<Probe>,
) -> ApiResult<Response> {
    tokio::time::timeout(Duration::from_secs(30), run_probe(state, path, input))
        .await
        .map_err(|_| StoreError::Unavailable("规则匹配超时，请稍后重试".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_are_exact_and_self_unmasking_requires_the_matching_user() {
        use axum::http::Method;
        let path = "/users/example/alice/clash-rules";
        assert!(allowed(&Method::GET, path));
        assert!(allowed(&Method::PUT, path));
        assert!(!allowed(&Method::DELETE, path));
        assert!(allowed(&Method::POST, &format!("{path}/probe")));
        assert!(!allowed(&Method::POST, &format!("{path}/probe/more")));
        let admin = AuthenticatedAdmin {
            operator_id: "self".into(),
            role: AdminRole::User,
            tenant_scope: Some("example".into()),
            token_prefix: None,
            self_user: Some(brocade_store::AuthenticatedUser {
                tenant_id: "example".into(),
                user_id: "alice".into(),
            }),
        };
        assert!(is_for_self(&admin, &Method::GET, path));
        assert!(!is_for_self(
            &admin,
            &Method::GET,
            "/users/example/bob/clash-rules"
        ));
    }
}
