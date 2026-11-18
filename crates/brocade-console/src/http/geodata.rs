use super::*;
use brocade_core::{
    model::ModelSnapshot,
    rule_validation::{atoms, issue},
};
use std::collections::BTreeSet;

#[derive(Deserialize)]
pub(super) struct CatalogQuery {
    kind: String,
    source: Option<String>,
}
#[derive(Default, Deserialize)]
pub(super) struct PreviewQuery {
    source: Option<String>,
    #[serde(default)]
    attr: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
    #[serde(default)]
    target: String,
}
fn source<'a>(settings: &'a ModelSettings, kind: &str) -> Result<&'a str, StoreError> {
    match kind {
        "geosite" => Ok(&settings.geodata.geosite_url),
        "geoip" => Ok(&settings.geodata.geoip_url),
        _ => Err(StoreError::InvalidData("规则集类型无效".into())),
    }
}
pub(super) async fn catalog(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    if query.source.is_some() {
        require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    }
    let settings = state.store.settings().await?;
    let source = query
        .source
        .as_deref()
        .unwrap_or(source(&settings, &query.kind)?);
    if source.len() > 2048 || !(source.starts_with("https://") || source.starts_with("http://")) {
        return Err(StoreError::InvalidData("规则集来源无效".into()).into());
    }
    let db = state
        .geodata
        .database(&query.kind, source)
        .await
        .map_err(StoreError::Unavailable)?;
    let snapshot = state.store.redacted_snapshot(&admin, None).await?;
    let mut references = vec![];
    fn collect(selector: &Value, kind: &str, location: &str, rule: usize, output: &mut Vec<Value>) {
        if selector["t"] == kind {
            if let Some(values) = selector["v"].as_array() {
                for value in values {
                    output.push(json!({"selector":value,"location":location,"rule":rule}));
                }
            }
        }
        if selector["t"] == "all" {
            if let Some(values) = selector["v"].as_array() {
                for value in values {
                    collect(value, kind, location, rule, output);
                }
            }
        }
    }
    if let Some(apps) = snapshot.snapshot["apps"].as_array() {
        for app in apps {
            if let Some(steps) = app["steps"].as_array() {
                for step in steps {
                    let location = format!(
                        "{}/{}/{}",
                        app["id"].as_str().unwrap_or_default(),
                        step["chain"].as_str().unwrap_or_default(),
                        step["node"].as_str().unwrap_or_default()
                    );
                    if let Some(rules) = step["rules"].as_array() {
                        for (index, rule) in rules.iter().enumerate() {
                            collect(&rule["m"], &query.kind, &location, index, &mut references);
                        }
                    }
                }
            }
        }
    }
    if let Some(policies) = snapshot.snapshot["node_egress_dns"].as_array() {
        for policy in policies {
            collect(
                &policy["selector"],
                &query.kind,
                &format!("dns/{}", policy["node"].as_str().unwrap_or_default()),
                policy["position"].as_u64().unwrap_or_default() as usize,
                &mut references,
            );
        }
    }
    let mut value = serde_json::to_value(db.catalog(source)).map_err(StoreError::from)?;
    value["references"] = json!(references);
    Ok(Json(value))
}
pub(super) async fn preview(
    State(state): State<AppState>,
    Path((kind, code)): Path<(String, String)>,
    Query(query): Query<PreviewQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    if query.q.len() > 256
        || query.target.len() > 253
        || query.attr.len() > 128
        || query.offset > 1_000_000
    {
        return Err(StoreError::InvalidData("查询范围无效".into()).into());
    }
    let settings = state.store.settings().await?;
    if query.source.is_some() {
        require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    }
    let selected_source = query.source.as_deref().unwrap_or(source(&settings, &kind)?);
    if selected_source.len() > 2048
        || !(selected_source.starts_with("https://") || selected_source.starts_with("http://"))
    {
        return Err(StoreError::InvalidData("规则集来源无效".into()).into());
    }
    let db = state
        .geodata
        .database(&kind, selected_source)
        .await
        .map_err(StoreError::Unavailable)?;
    let page = db
        .page(
            &code,
            &query.attr,
            &query.q,
            query.offset,
            query.limit.unwrap_or(100).clamp(1, 200),
            &query.target,
        )
        .map_err(StoreError::InvalidData)?;
    Ok(Json(serde_json::to_value(page).map_err(StoreError::from)?))
}

pub(super) fn touched(ops: &[brocade_store::ModelOp]) -> BTreeSet<String> {
    ops.iter()
        .filter_map(|op| match op {
            brocade_store::ModelOp::PutStep {
                app_id,
                chain_id,
                node_id,
                ..
            } => Some(format!("{app_id}/{chain_id}/{node_id}")),
            brocade_store::ModelOp::SetNodeEgressDns { node_id, .. }
            | brocade_store::ModelOp::ReorderNodeEgressDns { node_id, .. } => {
                Some(format!("dns/{node_id}"))
            }
            brocade_store::ModelOp::UpdateSettings { .. } => Some("settings".into()),
            _ => None,
        })
        .collect()
}

pub(super) async fn validate(
    state: &AppState,
    before: ModelSnapshot,
    after: ModelSnapshot,
    touched: BTreeSet<String>,
) -> Result<(), StoreError> {
    crate::geodata::validate(&state.geodata, &before, &after, &touched).await?;
    // Use the exact node engine, in a minimal synthetic configuration. No node configurations,
    // geodata files, credentials or publish-time preflight are involved.
    let mut regexes = vec![];
    for (location, rules) in crate::geodata::selectors(&after) {
        if !touched.contains(&location) {
            continue;
        }
        for (i, rule) in rules.iter().enumerate() {
            for (kind, value) in atoms(rule) {
                if kind == "domain_regex" {
                    regexes.push((location.clone(), i, value.to_owned()));
                }
            }
        }
    }
    if touched.contains("settings") || before.settings.geodata.cron != after.settings.geodata.cron {
        let cron = after.settings.geodata.cron.clone();
        if !engine_check(
            json!({"geodata":{"cron":cron}, "outbounds":[{"tag":"direct","protocol":"freedom"}]}),
        )
        .await?
        {
            let mut error = issue("settings", None, "", "cron 无效");
            error.field = "geodata.cron".into();
            return Err(StoreError::RuleValidation(vec![error]));
        }
    }
    if !regexes.is_empty() {
        let config = |values: Vec<String>| json!({"outbounds":[{"tag":"direct","protocol":"freedom"}], "routing":{"rules":[{"type":"field","domain":values,"outboundTag":"direct"}]}});
        if !engine_check(config(
            regexes
                .iter()
                .map(|(_, _, v)| format!("regexp:{v}"))
                .collect(),
        ))
        .await?
        {
            let mut issues = vec![];
            for (location, i, value) in regexes {
                if !engine_check(config(vec![format!("regexp:{value}")])).await? {
                    issues.push(issue(&location, Some(i), &value, "RE2 正则无效"));
                }
            }
            if !issues.is_empty() {
                return Err(StoreError::RuleValidation(issues));
            }
        }
    }
    Ok(())
}

async fn engine_check(config: Value) -> Result<bool, StoreError> {
    use tokio::io::AsyncWriteExt;
    // A private ephemeral cache avoids assuming the Console's working directory is writable.
    let root =
        tempfile::tempdir().map_err(|_| StoreError::Unavailable("Xray 校验未就绪".into()))?;
    let path = crate::embedded_xray::prepare(root.path(), std::env::consts::ARCH)
        .map_err(StoreError::Unavailable)?;
    let mut child = tokio::process::Command::new(path)
        .args(["run", "-test", "-format", "json", "-config", "stdin:"])
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| StoreError::Unavailable("Xray 校验无法启动".into()))?;
    let bytes = serde_json::to_vec(&config)?;
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&bytes)
            .await?;
        child.wait().await
    })
    .await;
    match result {
        Ok(Ok(status)) => Ok(status.success()),
        _ => {
            let _ = child.kill().await;
            Err(StoreError::Unavailable("Xray 校验失败".into()))
        }
    }
}

pub(super) async fn validate_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DraftRequest>,
) -> ApiResult<Json<Value>> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let before = state.store.materialize_snapshot(None).await?;
    let touched = touched(&request.ops);
    let after = state
        .store
        .draft_validation_snapshot(&admin, request.ops)
        .await?;
    validate(&state, before, after, touched).await?;
    Ok(Json(json!({"valid":true})))
}

#[derive(Deserialize)]
pub(super) struct RelationsRequest {
    kind: String,
    values: Vec<String>,
}
pub(super) async fn relations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RelationsRequest>,
) -> ApiResult<Json<Value>> {
    require_admin_context(&state, &headers, AdminPermission::Read).await?;
    if request.values.len() > 128 || request.values.iter().any(|v| v.len() > 256) {
        return Err(StoreError::InvalidData("查询范围无效".into()).into());
    }
    let settings = state.store.settings().await?;
    let database = state
        .geodata
        .database(&request.kind, source(&settings, &request.kind)?)
        .await
        .map_err(StoreError::Unavailable)?;
    let value = tokio::task::spawn_blocking(move || database.relations(&request.values))
        .await
        .map_err(|_| StoreError::Unavailable("规则集检查失败".into()))?;
    Ok(Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn embedded_engine_parses_the_node_cron_and_re2_grammar() {
        for cron in ["CRON_TZ=Asia/Shanghai 30 6 * * *", "@daily", "@every 12h"] {
            assert!(
                engine_check(json!({"geodata":{"cron":cron},"outbounds":[{"protocol":"freedom"}]}))
                    .await
                    .unwrap(),
                "{cron}"
            );
        }
        for cron in ["61 * * * *", "CRON_TZ=Unknown/Nowhere @daily"] {
            assert!(
                !engine_check(
                    json!({"geodata":{"cron":cron},"outbounds":[{"protocol":"freedom"}]})
                )
                .await
                .unwrap(),
                "{cron}"
            );
        }
        for (regex, valid) in [
            (r"\Qexample.com\E", true),
            ("(?=example)", false),
            ("[", false),
        ] {
            assert_eq!(engine_check(json!({"outbounds":[{"tag":"direct","protocol":"freedom"}],"routing":{"rules":[{"type":"field","domain":[format!("regexp:{regex}")],"outboundTag":"direct"}]}})).await.unwrap(),valid,"{regex}");
        }
    }
}
