//! Publishing and retracting the DNS-01 challenge records, on Cloudflare.
//!
//! # Why cleanup is only by record id
//!
//! One order for a wildcard and its bare name produces **two TXT records at one name** with
//! different values. Two consequences, both learned by running it rather than by reading:
//!
//! - Cleanup deletes by record id. Deleting by name removes the sibling too, and the order that is
//!   still using it fails with an error that points at DNS instead of at us.
//! - An order that dies between publishing and cleanup can leave records behind. Records created
//!   here carry an exact Brocade comment, so the next scan can remove only those records by id
//!   without touching another ACME client's TXT value at the same name.
//!
//! Publishing is also idempotent. Let's Encrypt may reuse a still-pending authorization after a
//! retry, which gives the retry the same TXT name and value. Cloudflare answers a second POST with
//! code 81058 (`An identical record already exists`); finding and reusing the exact value avoids
//! turning a harmless retry into a failed certificate order.
//!
//! # Why the wait can give up without failing
//!
//! Waiting is worth something: asking the CA to look before the record is there wastes one of the
//! challenge's few attempts. But the thing being waited on and the thing the CA reads are not the
//! same. The CA queries the zone's authoritative servers — which on Cloudflare is Cloudflare, the
//! same edge that just answered the API call — while this waits on a public *caching* resolver
//! that sits in nobody's validation path.
//!
//! So a resolver that has not caught up is not evidence that the CA cannot see the record, and it
//! must not be allowed to fail the order. It was, once: a node failed with "180s 内没生效" while
//! the record was in fact present, and the CA would have validated it. Now the wait returns
//! whether it saw it, the caller proceeds either way, and the CA — which is authoritative about
//! its own view — gives the real answer.
//!
//! Measured on 2026-08-09: 9 seconds from a cold name, 18 with a negative answer deliberately
//! cached first. The ceiling below is far above both, because its job is to bound the wait rather
//! than to decide anything.

use std::time::Duration;

use serde_json::Value;

const API: &str = "https://api.cloudflare.com/client/v4";
/// Resolved through DoH rather than a resolver on the machine: the control plane may sit where
/// port 53 is blocked, and this check must not be the reason a certificate cannot be issued.
const DOH: &str = "https://cloudflare-dns.com/dns-query";

const PROPAGATION_TIMEOUT: Duration = Duration::from_secs(60);
const PROPAGATION_INTERVAL: Duration = Duration::from_secs(3);
const RECORDS_PER_PAGE: u64 = 5_000;
const MAX_RECORD_PAGES: u64 = 1_000;
/// The TXT record's TTL. Short because it lives for seconds; not shorter because Cloudflare's
/// floor for a non-automatic TTL is 60.
const CHALLENGE_TTL: u32 = 60;
const ACME_RECORD_COMMENT: &str = "brocade ACME dns-01";
/// Older consoles kept an orange-clouded A record for every certificate SNI. Subscriptions dial
/// the machine IP directly and carry the name only as TLS SNI, so those records have no runtime
/// consumer. The marker remains here solely so a newer console can remove what an older one made.
const LEGACY_NODE_RECORD_COMMENT: &str = "brocade node";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Transport(String),
    /// The provider refused. Carries what it said, because the two failures that actually happen —
    /// an expired token and a zone the token cannot see — are indistinguishable without it.
    Api(String),
    Timeout(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(m) => write!(f, "连不上 Cloudflare：{m}"),
            Error::Api(m) => write!(f, "Cloudflare 拒绝了：{m}"),
            Error::Timeout(m) => write!(f, "等 DNS 生效超时：{m}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Cloudflare {
    http: reqwest::Client,
    token: String,
    zone_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DnsRecord {
    id: String,
    name: String,
    content: String,
    comment: Option<String>,
}

/// A challenge record used by this order. `delete_after_use` is false when an exact value exists
/// without Brocade's ownership marker: it is valid proof and can be reused, but is not ours to
/// remove.
pub struct PublishedRecord {
    pub id: String,
    pub name: String,
    pub delete_after_use: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ManagedCleanup {
    pub legacy_address_records: usize,
    pub challenge_records: usize,
    pub failures: Vec<String>,
}

fn transport(error: reqwest::Error) -> Error {
    Error::Transport(error.to_string())
}

/// Cloudflare wraps everything in `{success, errors, result}`; a failed call is still HTTP 200 in
/// some cases, so the envelope is what decides.
fn unwrap_envelope(body: Value) -> Result<Value> {
    if body.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(body.get("result").cloned().unwrap_or(Value::Null));
    }
    let detail = body
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    let message = error.get("message").and_then(Value::as_str)?;
                    let code = error
                        .get("code")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    Some(format!("{message}（code {code}）"))
                })
                .collect::<Vec<_>>()
                .join("；")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "没有给出原因".to_owned());
    Err(Error::Api(detail))
}

fn parse_records(result: &Value) -> Result<Vec<DnsRecord>> {
    let records = result
        .as_array()
        .ok_or_else(|| Error::Api("DNS 记录列表不是数组".to_owned()))?;
    records
        .iter()
        .map(|record| {
            let field = |name: &str| {
                record
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Api(format!("DNS 记录缺少 {name}")))
            };
            Ok(DnsRecord {
                id: field("id")?,
                name: field("name")?,
                content: field("content")?,
                comment: record
                    .get("comment")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

fn reusable_challenge(records: &[DnsRecord], value: &str) -> Option<PublishedRecord> {
    records
        .iter()
        .find(|record| record.content == value)
        .map(|record| PublishedRecord {
            id: record.id.clone(),
            name: record.name.clone(),
            delete_after_use: record
                .comment
                .as_deref()
                .is_some_and(|comment| comment.eq_ignore_ascii_case(ACME_RECORD_COMMENT)),
        })
}

impl Cloudflare {
    /// Looks up the zone by name, which doubles as the credential check: a token that cannot see
    /// the zone fails here, before an order has been opened and before any rate limit is spent.
    pub async fn connect(token: &str, domain: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(20))
            .user_agent("brocade-console")
            .build()
            .map_err(transport)?;

        let body: Value = http
            .get(format!("{API}/zones"))
            .query(&[("name", domain)])
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;

        let zones = unwrap_envelope(body)?;
        let zone_id = zones
            .as_array()
            .and_then(|items| items.first())
            .and_then(|zone| zone.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                // Said as a question about the token rather than about the domain: the domain is
                // what the operator typed and is usually right; what is usually wrong is that the
                // token was scoped to a different zone, or to none.
                Error::Api(format!(
                    "这个 token 看不到 {domain} 这个 zone——检查 token 的 Zone Resources 是不是选了它，\
                     以及有没有 Zone:Read 权限"
                ))
            })?
            .to_owned();

        Ok(Self {
            http,
            token: token.to_owned(),
            zone_id,
        })
    }

    async fn records(&self, record_type: &str, name: Option<&str>) -> Result<Vec<DnsRecord>> {
        let mut all = Vec::new();
        let mut page = 1_u64;
        loop {
            let mut query = vec![
                ("type", record_type.to_owned()),
                ("page", page.to_string()),
                ("per_page", RECORDS_PER_PAGE.to_string()),
            ];
            if let Some(name) = name {
                query.push(("name", name.to_owned()));
            }
            let body: Value = self
                .http
                .get(format!("{API}/zones/{}/dns_records", self.zone_id))
                .query(&query)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(transport)?
                .json()
                .await
                .map_err(transport)?;
            let total_pages = body
                .pointer("/result_info/total_pages")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if total_pages > MAX_RECORD_PAGES {
                return Err(Error::Api(format!(
                    "DNS 记录列表有 {total_pages} 页，超过安全上限 {MAX_RECORD_PAGES}"
                )));
            }
            all.extend(parse_records(&unwrap_envelope(body)?)?);
            if page >= total_pages {
                return Ok(all);
            }
            page += 1;
        }
    }

    /// Removes only records carrying the exact comments used by Brocade. A records are leftovers
    /// from the retired SNI-address reconciliation; TXT records are abandoned ACME challenges.
    /// Records owned by a person or another ACME client are never selected.
    pub async fn cleanup_managed_records(&self) -> Result<ManagedCleanup> {
        let mut managed = Vec::new();
        managed.extend(
            self.records("A", None)
                .await?
                .into_iter()
                .filter(|record| {
                    record.comment.as_deref().is_some_and(|comment| {
                        comment.eq_ignore_ascii_case(LEGACY_NODE_RECORD_COMMENT)
                    })
                })
                .map(|record| (record, false)),
        );
        managed.extend(
            self.records("TXT", None)
                .await?
                .into_iter()
                .filter(|record| {
                    record
                        .comment
                        .as_deref()
                        .is_some_and(|comment| comment.eq_ignore_ascii_case(ACME_RECORD_COMMENT))
                })
                .map(|record| (record, true)),
        );

        let mut cleanup = ManagedCleanup::default();
        for (record, challenge) in managed {
            match self.delete(&record.id).await {
                Ok(()) if challenge => cleanup.challenge_records += 1,
                Ok(()) => cleanup.legacy_address_records += 1,
                Err(error) => cleanup.failures.push(format!("{}：{error}", record.name)),
            }
        }
        Ok(cleanup)
    }

    pub async fn publish(&self, name: &str, value: &str) -> Result<PublishedRecord> {
        if let Some(existing) = reusable_challenge(&self.records("TXT", Some(name)).await?, value) {
            return Ok(existing);
        }
        let body: Value = self
            .http
            .post(format!("{API}/zones/{}/dns_records", self.zone_id))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "type": "TXT",
                "name": name,
                "content": value,
                "ttl": CHALLENGE_TTL,
                "comment": ACME_RECORD_COMMENT,
            }))
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;

        let created = match unwrap_envelope(body) {
            Ok(created) => created,
            Err(error) => {
                // Close the race between the preflight GET and POST. It also makes a retry safe
                // against providers which create a record but lose the create response.
                if let Some(existing) =
                    reusable_challenge(&self.records("TXT", Some(name)).await?, value)
                {
                    return Ok(existing);
                }
                return Err(error);
            }
        };
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Api("建好了 TXT 但没给记录 id".to_owned()))?
            .to_owned();
        Ok(PublishedRecord {
            id,
            name: name.to_owned(),
            delete_after_use: true,
        })
    }

    /// Deletes one record by id.
    pub async fn delete(&self, record_id: &str) -> Result<()> {
        let body: Value = self
            .http
            .delete(format!(
                "{API}/zones/{}/dns_records/{record_id}",
                self.zone_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        unwrap_envelope(body).map(|_| ())
    }

    /// Waits for every published value to become visible, and reports whether it did.
    ///
    /// `Some(elapsed)` means a resolver confirmed all of them; `None` means the ceiling was
    /// reached without confirmation, which is a reason to note the delay and carry on — see the
    /// module header for why this cannot be a failure.
    ///
    /// All values together rather than one at a time: the records at one name propagate as a
    /// single change, and checking them separately would report success as soon as the first
    /// appeared.
    pub async fn await_propagation(&self, name: &str, values: &[String]) -> Option<Duration> {
        let started = std::time::Instant::now();
        let deadline = started + PROPAGATION_TIMEOUT;
        loop {
            // A resolver hiccup and "not there yet" are the same thing to this loop: both mean
            // keep waiting until the ceiling decides.
            if let Ok(seen) = self.resolve_txt(name).await {
                if values
                    .iter()
                    .all(|value| seen.iter().any(|item| item == value))
                {
                    return Some(started.elapsed());
                }
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(PROPAGATION_INTERVAL).await;
        }
    }

    async fn resolve_txt(&self, name: &str) -> Result<Vec<String>> {
        let body: Value = self
            .http
            .get(DOH)
            .query(&[("name", name), ("type", "TXT")])
            .header("accept", "application/dns-json")
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        Ok(body
            .get("Answer")
            .and_then(Value::as_array)
            .map(|answers| {
                answers
                    .iter()
                    .filter_map(|answer| answer.get("data").and_then(Value::as_str))
                    // DoH hands back TXT data quoted; the challenge value is what is inside.
                    .map(|data| data.trim_matches('"').to_owned())
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(id: &str, name: &str, content: &str, comment: Option<&str>) -> DnsRecord {
        DnsRecord {
            id: id.to_owned(),
            name: name.to_owned(),
            content: content.to_owned(),
            comment: comment.map(str::to_owned),
        }
    }

    #[test]
    fn a_failed_envelope_carries_what_cloudflare_said() {
        let body = json!({
            "success": false,
            "errors": [{ "code": 10000, "message": "Authentication error" }],
        });
        let error = unwrap_envelope(body).unwrap_err().to_string();
        // Both halves matter: the sentence is what a person reads, the code is what they search.
        assert!(error.contains("Authentication error"), "{error}");
        assert!(error.contains("10000"), "{error}");
    }

    #[test]
    fn a_successful_envelope_yields_the_result() {
        let body = json!({ "success": true, "result": { "id": "abc" } });
        assert_eq!(unwrap_envelope(body).unwrap()["id"], "abc");
    }

    #[test]
    fn an_envelope_with_no_errors_still_says_something() {
        // Cloudflare has returned `success: false` with an empty error list. "没有给出原因" is a
        // worse message than a real one and a much better one than an empty string.
        let error = unwrap_envelope(json!({ "success": false, "errors": [] }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("没有给出原因"), "{error}");
    }

    #[test]
    fn an_identical_brocade_challenge_is_reused_and_removed_afterwards() {
        let existing = vec![record(
            "txt-1",
            "_acme-challenge.edge.example.com",
            "proof",
            Some(ACME_RECORD_COMMENT),
        )];
        let published = reusable_challenge(&existing, "proof").expect("same proof is reusable");
        assert_eq!(published.id, "txt-1");
        assert!(published.delete_after_use);
    }

    #[test]
    fn an_identical_unowned_challenge_is_reused_but_not_deleted() {
        let existing = vec![record(
            "txt-external",
            "_acme-challenge.edge.example.com",
            "proof",
            Some("managed elsewhere"),
        )];
        let published = reusable_challenge(&existing, "proof").expect("same proof is reusable");
        assert_eq!(published.id, "txt-external");
        assert!(!published.delete_after_use);
    }

    #[test]
    fn a_different_challenge_value_does_not_hide_a_new_record() {
        let existing = vec![record(
            "txt-old",
            "_acme-challenge.edge.example.com",
            "old-proof",
            Some(ACME_RECORD_COMMENT),
        )];
        assert!(reusable_challenge(&existing, "new-proof").is_none());
    }

    #[test]
    fn record_lists_require_identity_name_and_content() {
        let parsed = parse_records(&json!([{
            "id": "record-1",
            "name": "edge.example.com",
            "content": "192.0.2.1",
            "comment": LEGACY_NODE_RECORD_COMMENT
        }]))
        .unwrap();
        assert_eq!(
            parsed,
            vec![record(
                "record-1",
                "edge.example.com",
                "192.0.2.1",
                Some(LEGACY_NODE_RECORD_COMMENT)
            )]
        );
        assert!(parse_records(&json!([{"name": "missing-id.example.com"}])).is_err());
    }
}
