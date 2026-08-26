//! Publishing and retracting the DNS-01 challenge records, on Cloudflare.
//!
//! # Why cleanup is only by record id
//!
//! One order for a wildcard and its bare name produces **two TXT records at one name** with
//! different values. Two consequences, both learned by running it rather than by reading:
//!
//! - Cleanup deletes by record id. Deleting by name removes the sibling too, and the order that is
//!   still using it fails with an error that points at DNS instead of at us.
//! - An order that dies between publishing and cleanup can leave records behind. They are safer
//!   left stale than swept by name: the same challenge name can be in use by another ACME client,
//!   and deleting records we did not create can break somebody else's live renewal.
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
/// The TXT record's TTL. Short because it lives for seconds; not shorter because Cloudflare's
/// floor for a non-automatic TTL is 60.
const CHALLENGE_TTL: u32 = 60;

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

/// A record this process created, and therefore is responsible for removing.
pub struct PublishedRecord {
    pub id: String,
    pub name: String,
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

impl Cloudflare {
    /// Looks up the zone by name, which doubles as the credential check: a token that cannot see
    /// the zone fails here, before an order has been opened and before any rate limit is spent.
    pub async fn connect(token: &str, domain: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
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

    /// Every TXT record currently at `name`.
    /// Points `name` at `ipv4` behind the proxy, creating or correcting the record, and removes
    /// any extras.
    ///
    /// Returns what it did, so the caller can say so once rather than every scan: this runs on a
    /// timer and a line per node per hour would bury the one time it mattered.
    ///
    /// Extras are deleted rather than left alone. Two A records at one name means half the clients
    /// resolve to a machine that is not there, which presents as "it works for some people" — the
    /// worst shape a fault can take.
    ///
    /// The proxy flag is reconciled alongside the address, and for the same reason the address is:
    /// a record written before this fleet proxied them is still exposing the machine's own
    /// address, and nothing else would ever come back to fix it. Which means the correcting branch
    /// now has two causes and has to say which one it acted on — a line reading "changed to the
    /// address it already had" is the sort of log that gets a working system taken apart.
    pub async fn ensure_a(&self, name: &str, ipv4: &str) -> Result<Option<String>> {
        let body: Value = self
            .http
            .get(format!("{API}/zones/{}/dns_records", self.zone_id))
            .query(&[("type", "A"), ("name", name)])
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        let existing: Vec<(String, String, bool)> = unwrap_envelope(body)?
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|record| {
                        Some((
                            record.get("id")?.as_str()?.to_owned(),
                            record.get("content")?.as_str()?.to_owned(),
                            // A record whose flag the API did not state is read as grey and
                            // corrected. Dropping it instead would leave it in `existing` as
                            // surplus and get it deleted, which is a worse answer to a field we
                            // merely failed to parse.
                            record
                                .get("proxied")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut note = None;
        // The single record this name is allowed to end up with: the address, behind the proxy.
        // Everything else here goes, a duplicate carrying the right address included — that one
        // used to be merely redundant, and is now the machine's own address answering alongside
        // the edge, which is the precise thing the proxy was turned on to stop.
        let mut keep = existing
            .iter()
            .find(|(_, content, proxied)| content == ipv4 && *proxied)
            .map(|(id, _, _)| id.clone());
        if keep.is_none() {
            match existing.first() {
                // Corrected in place rather than deleted and recreated: a gap between the two,
                // however short, is a name that resolves to nothing.
                Some((id, was, was_proxied)) => {
                    self.update_a(id, name, ipv4).await?;
                    keep = Some(id.clone());
                    note = Some(if was == ipv4 && !was_proxied {
                        format!("{name} 由直连改走橙云")
                    } else {
                        format!("{name} 从 {was} 改指 {ipv4}")
                    });
                }
                None => {
                    self.create_a(name, ipv4).await?;
                    note = Some(format!("{name} → {ipv4}"));
                }
            }
        }
        for (id, content, proxied) in existing
            .iter()
            .filter(|(id, _, _)| Some(id) != keep.as_ref())
        {
            self.delete(id).await?;
            // Said apart, because the two read as different faults to whoever finds the line: a
            // stray address is somebody else's record at our name, a stray direct one is our own
            // address that the proxy was supposed to have taken out of the answer.
            let what = match (content == ipv4, *proxied) {
                (true, false) => format!("多余的直连记录 {content}"),
                _ => format!("多余的 {content}"),
            };
            note = Some(match note {
                Some(said) => format!("{said}，并清掉{what}"),
                None => format!("{name} 清掉{what}"),
            });
        }
        Ok(note)
    }

    async fn create_a(&self, name: &str, ipv4: &str) -> Result<()> {
        let body: Value = self
            .http
            .post(format!("{API}/zones/{}/dns_records", self.zone_id))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "type": "A",
                "name": name,
                "content": ipv4,
                // Proxied, so the name answers with Cloudflare's edge rather than the machine.
                // Every issued certificate is published to Certificate Transparency, so
                // `<label>.<domain>` is public the moment it exists and anyone may ask what it
                // points at; grey-clouded, that question hands out the node's address. Orange
                // costs the data plane nothing — subscriptions carry the address itself, and this
                // name only ever travels as SNI, which no client resolves.
                //
                // What it does cost is the name in an address position: a projection host, a CDN
                // origin, or somebody's `curl` pointed at this name now reaches the edge, and the
                // edge speaks HTTP only. Neither is something this fleet sets up on its own.
                //
                // TTL is automatic because Cloudflare requires that of a proxied record. Nothing
                // is lost: the edge answers for the name, so the number would decide nothing.
                "ttl": 1,
                "proxied": true,
                "comment": "brocade node",
            }))
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        unwrap_envelope(body).map(|_| ())
    }

    async fn update_a(&self, record_id: &str, name: &str, ipv4: &str) -> Result<()> {
        let body: Value = self
            .http
            .patch(format!(
                "{API}/zones/{}/dns_records/{record_id}",
                self.zone_id
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "type": "A",
                "name": name,
                "content": ipv4,
                // Both fields restated rather than left to PATCH's merge: this is also the path
                // that turns an existing grey record orange, and a patch that names neither
                // leaves it exactly as it was. See `create_a` for why orange.
                "ttl": 1,
                "proxied": true,
            }))
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;
        unwrap_envelope(body).map(|_| ())
    }

    pub async fn publish(&self, name: &str, value: &str) -> Result<PublishedRecord> {
        let body: Value = self
            .http
            .post(format!("{API}/zones/{}/dns_records", self.zone_id))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "type": "TXT",
                "name": name,
                "content": value,
                "ttl": CHALLENGE_TTL,
                "comment": "brocade ACME dns-01",
            }))
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;

        let id = unwrap_envelope(body)?
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Api("建好了 TXT 但没给记录 id".to_owned()))?
            .to_owned();
        Ok(PublishedRecord {
            id,
            name: name.to_owned(),
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
}
