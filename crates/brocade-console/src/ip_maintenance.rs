//! Mirror only the upstream's published data, never its executable scripts.
use brocade_deployment::ip_maintenance::data;
use brocade_store::PgStore;
use std::{collections::BTreeMap, time::Duration};

const SOURCE: &str = "https://raw.githubusercontent.com/hotyue/IP-Sentinel";

pub fn spawn(store: PgStore, shutdown: crate::lifecycle::ShutdownSignal) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _=shutdown.requested()=>break,
                result=refresh(&store)=>if let Err(error)=result {eprintln!("ip-maintenance-data: {error}");},
            }
            tokio::select! {_=shutdown.requested()=>break,_=tokio::time::sleep(Duration::from_secs(60))=>{}}
        }
    });
}
async fn download(client: &reqwest::Client, url: &str, maximum: usize) -> Result<String, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "maintenance_data_network")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err("maintenance_data_http_status".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "maintenance_data_body")?
    {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err("maintenance_data_size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "maintenance_data_utf8".into())
}
fn words(text: &str) -> Result<Vec<String>, String> {
    let words = text
        .lines()
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if words.is_empty()
        || words.len() > 100
        || words
            .iter()
            .any(|word| word.len() > 2048 || word.chars().any(char::is_control))
    {
        return Err("maintenance_keywords_invalid".into());
    }
    Ok(words)
}
async fn refresh(store: &PgStore) -> Result<(), String> {
    let claim = store
        .claim_ip_maintenance_refresh()
        .await
        .map_err(|_| "maintenance_data_lease")?;
    let Some((generation, ids)) = claim else {
        return Ok(());
    };
    let work = tokio::time::timeout(
        Duration::from_secs(1800),
        refresh_claimed(store, &ids, generation),
    )
    .await
    .map_err(|_| "maintenance_refresh_deadline".to_owned())
    .and_then(|result| result);
    store
        .finish_ip_maintenance_refresh(generation, work.is_err())
        .await
        .map_err(|_| "maintenance_data_schedule")?;
    work
}
async fn refresh_claimed(store: &PgStore, ids: &[String], generation: u64) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("brocade-maintenance-data/1")
        .build()
        .map_err(|_| "maintenance_data_client")?;
    let commit_json = download(
        &client,
        "https://api.github.com/repos/hotyue/IP-Sentinel/commits/main",
        128 * 1024,
    )
    .await?;
    let commit: serde_json::Value =
        serde_json::from_str(&commit_json).map_err(|_| "maintenance_commit_json")?;
    let commit = commit
        .get("sha")
        .and_then(|v| v.as_str())
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("maintenance_commit_identity")?;
    let source = format!("{SOURCE}/{commit}/data");
    let text = download(&client, &format!("{source}/user_agents.txt"), 1024 * 1024).await?;
    let agents = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut assets = vec![(
        "agents".to_owned(),
        serde_json::to_value(agents).map_err(|_| "maintenance_agents_encoding")?,
    )];
    let mut keywords = BTreeMap::<String, Vec<String>>::new();
    for id in ids {
        let seed = data::region(id).ok_or("maintenance_region_unknown")?;
        if !keywords.contains_key(&seed.country) {
            let text = download(
                &client,
                &format!("{source}/keywords/kw_{}.txt", seed.country),
                256 * 1024,
            )
            .await?;
            keywords.insert(seed.country.clone(), words(&text)?);
        }
        let json = download(&client, &format!("{source}/regions/{id}.json"), 512 * 1024).await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&json).map_err(|_| "maintenance_region_json")?;
        let sites = serde_json::from_value::<Vec<String>>(
            parsed
                .pointer("/trust_module/white_urls")
                .ok_or("maintenance_sites_missing")?
                .clone(),
        )
        .map_err(|_| "maintenance_sites_invalid")?;
        let mut region = seed.clone();
        region.keywords = keywords
            .get(&seed.country)
            .ok_or("maintenance_keywords_missing")?
            .clone();
        region.sites = sites;
        assets.push((
            format!("region:{id}"),
            serde_json::to_value(region).map_err(|_| "maintenance_region_encoding")?,
        ));
    }
    store
        .publish_ip_maintenance_assets(assets, commit, generation)
        .await
        .map_err(|_| "maintenance_data_publish".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hot_words_are_bounded_without_hiding_bad_payloads() {
        assert_eq!(
            words("地域新闻\n\n本地交通\n").unwrap(),
            vec!["地域新闻", "本地交通"]
        );
        assert!(words("").is_err());
        assert!(words(&"x\n".repeat(101)).is_err());
        assert!(words("bad\u{0000}word").is_err());
    }
}
