use super::*;

#[derive(Deserialize)]
pub(super) struct MatchQuery {
    domain: String,
    sets: String,
}

fn selectors(query: &MatchQuery) -> Result<Vec<String>, StoreError> {
    let sets: Vec<String> = serde_json::from_str(&query.sets)
        .map_err(|_| StoreError::InvalidData("geosite 分类列表格式无效".into()))?;
    if query.domain.is_empty()
        || query.domain.len() > 253
        || query.domain.parse::<IpAddr>().is_ok()
        || !query.domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || sets.is_empty()
        || sets.len() > 128
        || sets.iter().any(|set| {
            set.is_empty()
                || set.len() > 128
                || set.ends_with('@')
                || set.contains("@@")
                || !set
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.!@".contains(&b))
        })
    {
        return Err(StoreError::InvalidData(
            "请输入有效域名及 geosite 分类（每次最多 128 个）".into(),
        ));
    }
    Ok(sets)
}

pub(super) async fn matches(
    State(state): State<AppState>,
    Query(query): Query<MatchQuery>,
) -> ApiResult<Json<Value>> {
    let sets = selectors(&query)?;
    let settings = state.store.settings().await?;
    let matches = state
        .geosite
        .matches(&settings.geodata.geosite_url, &query.domain, &sets)
        .await
        .map_err(StoreError::Unavailable)?;
    Ok(Json(json!({ "matches": matches })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_lookup_inputs_without_accepting_urls_or_arbitrary_file_paths() {
        let query = |domain: &str, sets: Vec<&str>| MatchQuery {
            domain: domain.into(),
            sets: serde_json::to_string(&sets).unwrap(),
        };
        assert!(selectors(&query("openai.com", vec!["category-ai-!cn", "google@cn"])).is_ok());
        for domain in ["", "192.0.2.1", "a..b", "https://example.com", "-a.example"] {
            assert!(selectors(&query(domain, vec!["cn"])).is_err());
        }
        for set in ["", "../geosite.dat:cn", "google@@cn", "google@"] {
            assert!(selectors(&query("example.com", vec![set])).is_err());
        }
        assert!(selectors(&query("example.com", vec!["cn"; 129])).is_err());
    }
}
