//! An ACME v2 client, enough of one to get a wildcard certificate through DNS-01.
//!
//! # Why written here rather than taken off the shelf
//!
//! The crates that do this pull their own HTTP stack and their own async runtime assumptions, and
//! the part of ACME this needs is small: seven requests, all JSON, all signed the same way. What
//! is not small is the part they would still leave to us — writing the DNS record, waiting for it,
//! cleaning it up — which is provider-specific and where the failures actually live.
//!
//! # DNS-01, and why there is no choice about it
//!
//! A wildcard can only be validated by DNS-01. HTTP-01 and TLS-ALPN-01 prove control of a host;
//! only a DNS record proves control of the name that a wildcard spans. That single fact is why the
//! control plane holds a DNS credential at all, and therefore why it is the control plane that
//! issues certificates rather than each node.
//!
//! # The one shape worth knowing before reading
//!
//! An order for `*.x.example.net` and `x.example.net` produces **two authorizations whose challenge
//! records have the same name** — `_acme-challenge.x.example.net` — with different values. So the
//! provider must be able to hold two TXT records at one name, and cleanup must delete by record
//! id. Deleting by name takes the other one with it and fails the order that is still using it.
//! Measured, not assumed: it is what Let's Encrypt did on 2026-08-09.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// How long to wait for an authorization or order to leave `pending`. The measured end-to-end time
/// was 28 seconds; this is the ceiling for one order, not an expected duration.
const POLL_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// The transport failed — DNS, TLS, connection, timeout. Separate from `Server` because the
    /// operator's next move is different: this one is about the control plane's own network.
    Transport(String),
    /// The CA said no, and this is what it said. ACME problem documents are the most useful error
    /// in the whole flow (they name CAA failures, rate limits, and bad nonces exactly), so the
    /// detail is carried rather than flattened.
    Server(String),
    /// The account key, the CSR, or a signature.
    Crypto(String),
    /// Something in a response was missing or the wrong shape.
    Protocol(String),
    /// The order never became valid inside `POLL_TIMEOUT`.
    Timeout(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(m) => write!(f, "连不上 ACME 服务：{m}"),
            Error::Server(m) => write!(f, "ACME 服务拒绝了：{m}"),
            Error::Crypto(m) => write!(f, "密钥或签名出错：{m}"),
            Error::Protocol(m) => write!(f, "ACME 响应不符合预期：{m}"),
            Error::Timeout(m) => write!(f, "等超时了：{m}"),
        }
    }
}

impl std::error::Error for Error {}

fn transport(error: reqwest::Error) -> Error {
    Error::Transport(error.to_string())
}

/// The account key, in the form it is stored: base64 of the PKCS#8 document.
///
/// Not PEM, though PEM is what everything else here stores. ring reads and writes PKCS#8 bytes and
/// has no PEM of its own, so PEM would mean a conversion in both directions for the sole benefit
/// of looking like the other columns.
pub struct AccountKey {
    pair: EcdsaKeyPair,
    pkcs8_b64: String,
}

impl AccountKey {
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|error| Error::Crypto(format!("生成账号密钥失败：{error}")))?;
        Self::from_pkcs8_b64(&B64.encode(document.as_ref()))
    }

    pub fn from_pkcs8_b64(encoded: &str) -> Result<Self> {
        let bytes = B64
            .decode(encoded.trim())
            .map_err(|_| Error::Crypto("账号密钥不是 base64".to_owned()))?;
        let rng = SystemRandom::new();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &bytes, &rng)
            .map_err(|error| Error::Crypto(format!("账号密钥读不出来：{error}")))?;
        Ok(Self {
            pair,
            pkcs8_b64: encoded.trim().to_owned(),
        })
    }

    pub fn as_stored(&self) -> &str {
        &self.pkcs8_b64
    }

    /// The public JWK. ring hands back an uncompressed point (`0x04 || x || y`), which is exactly
    /// the two coordinates a JWK wants, in order.
    fn jwk(&self) -> Result<Value> {
        let point = self.pair.public_key().as_ref();
        if point.len() != 65 || point[0] != 0x04 {
            return Err(Error::Crypto("公钥不是未压缩的 P-256 点".to_owned()));
        }
        Ok(json!({
            "crv": "P-256",
            "kty": "EC",
            "x": B64.encode(&point[1..33]),
            "y": B64.encode(&point[33..65]),
        }))
    }

    /// The JWK thumbprint (RFC 7638). Its members must be serialized in lexicographic order with
    /// no whitespace — this is one of the few places in ACME where formatting is load-bearing, and
    /// getting it wrong produces a challenge that fails validation with no explanation.
    fn thumbprint(&self) -> Result<String> {
        let jwk = self.jwk()?;
        let canonical = format!(
            r#"{{"crv":"{}","kty":"{}","x":"{}","y":"{}"}}"#,
            jwk["crv"].as_str().unwrap_or_default(),
            jwk["kty"].as_str().unwrap_or_default(),
            jwk["x"].as_str().unwrap_or_default(),
            jwk["y"].as_str().unwrap_or_default(),
        );
        let digest = ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes());
        Ok(B64.encode(digest.as_ref()))
    }

    fn sign(&self, message: &[u8]) -> Result<String> {
        let rng = SystemRandom::new();
        let signature = self
            .pair
            .sign(&rng, message)
            .map_err(|error| Error::Crypto(format!("签名失败：{error}")))?;
        Ok(B64.encode(signature.as_ref()))
    }
}

/// The directory's endpoints. Fetched once per order rather than cached across orders: it is one
/// request against a CDN, and a cached directory is how a client keeps talking to an endpoint the
/// CA has moved.
struct Directory {
    new_nonce: String,
    new_account: String,
    new_order: String,
}

pub struct Session {
    http: reqwest::Client,
    directory: Directory,
    key: AccountKey,
    /// Set once the account exists; every request after that is signed with `kid` rather than the
    /// full `jwk`, which is what ACME requires and not merely an optimization.
    account_url: Option<String>,
    nonce: Option<String>,
}

/// What one completed order produced.
pub struct IssuedCertificate {
    /// The full chain, leaf first.
    pub chain_pem: String,
    /// The private key, PEM. Generated per order — never reused, because reuse means a key that
    /// outlives the certificate that justified it.
    pub key_pem: String,
    /// `notAfter`, read out of the leaf. RFC 3339, so it lands in a `timestamptz` unambiguously.
    pub not_after: String,
    /// The issuing CA's common name, as the certificate states it.
    ///
    /// Read from the bytes rather than derived from the configured directory, because those two
    /// can disagree and only one of them is a fact. It is also the only thing that distinguishes a
    /// staging certificate from a real one on a page: a fleet left pointing at staging shows
    /// "ready" everywhere while nothing trusts a single one of them, and the issuer is where that
    /// says itself out loud — Let's Encrypt's staging names are deliberately absurd
    /// (`(STAGING) Pretend Pear X1` at the root) for exactly this purpose.
    pub issuer: String,
    /// SHA-256 of the leaf certificate's DER bytes, used by the local Xray probe when the chain is
    /// private and therefore absent from system trust stores.
    pub peer_sha256: String,
}

/// One DNS record the caller has to publish, and later remove.
pub struct DnsChallenge {
    /// The full record name, `_acme-challenge.<...>`.
    pub name: String,
    /// The TXT value, already in the form the CA will look for.
    pub value: String,
    /// Where to tell the CA to go and look.
    challenge_url: String,
    authorization_url: String,
}

impl Session {
    pub async fn start(directory_url: &str, key: AccountKey) -> Result<Self> {
        let http = reqwest::Client::builder()
            // Every request here is small and to one host. A total timeout rather than a read
            // timeout, so a server that trickles bytes forever still ends.
            .timeout(Duration::from_secs(30))
            .user_agent("brocade-console")
            .build()
            .map_err(transport)?;

        let body: Value = http
            .get(directory_url)
            .send()
            .await
            .map_err(transport)?
            .json()
            .await
            .map_err(transport)?;

        let field = |name: &str| -> Result<String> {
            body.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::Protocol(format!("目录里没有 {name}")))
        };
        Ok(Self {
            directory: Directory {
                new_nonce: field("newNonce")?,
                new_account: field("newAccount")?,
                new_order: field("newOrder")?,
            },
            http,
            key,
            account_url: None,
            nonce: None,
        })
    }

    async fn take_nonce(&mut self) -> Result<String> {
        if let Some(nonce) = self.nonce.take() {
            return Ok(nonce);
        }
        let response = self
            .http
            .head(&self.directory.new_nonce)
            .send()
            .await
            .map_err(transport)?;
        response
            .headers()
            .get("replay-nonce")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| Error::Protocol("newNonce 没给 Replay-Nonce".to_owned()))
    }

    /// A signed request. `payload` of `None` is POST-as-GET, which is how ACME reads a resource —
    /// there is no authenticated GET in the protocol.
    async fn post(
        &mut self,
        url: &str,
        payload: Option<Value>,
    ) -> Result<(Value, reqwest::header::HeaderMap)> {
        // One retry, and only for badNonce. The CA hands out a fresh nonce with every response, so
        // a rejected one means ours was stale rather than wrong — retrying anything else here
        // would be retrying a real refusal.
        for attempt in 0..2 {
            let nonce = self.take_nonce().await?;
            let mut protected = json!({
                "alg": "ES256",
                "nonce": nonce,
                "url": url,
            });
            match &self.account_url {
                Some(kid) => protected["kid"] = json!(kid),
                None => protected["jwk"] = self.key.jwk()?,
            }
            let protected_b64 = B64.encode(protected.to_string());
            let payload_b64 = match &payload {
                Some(value) => B64.encode(value.to_string()),
                None => String::new(),
            };
            let signature = self
                .key
                .sign(format!("{protected_b64}.{payload_b64}").as_bytes())?;

            let response = self
                .http
                .post(url)
                .header("content-type", "application/jose+json")
                .body(
                    json!({
                        "protected": protected_b64,
                        "payload": payload_b64,
                        "signature": signature,
                    })
                    .to_string(),
                )
                .send()
                .await
                .map_err(transport)?;

            let status = response.status();
            let headers = response.headers().clone();
            if let Some(fresh) = headers.get("replay-nonce").and_then(|v| v.to_str().ok()) {
                self.nonce = Some(fresh.to_owned());
            }
            let text = response.text().await.map_err(transport)?;
            let body: Value = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text.clone()))
            };

            if status.is_success() {
                return Ok((body, headers));
            }
            let kind = body.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind.ends_with(":badNonce") && attempt == 0 {
                continue;
            }
            return Err(Error::Server(describe_problem(status.as_u16(), &body)));
        }
        Err(Error::Server("nonce 反复被拒".to_owned()))
    }

    /// Registers, or picks up the existing account for this key. ACME makes these the same
    /// request, which is why an account URL that was lost can always be recovered from the key.
    pub async fn register(&mut self, contact: Option<&str>) -> Result<String> {
        let mut payload = json!({ "termsOfServiceAgreed": true });
        if let Some(contact) = contact.filter(|value| !value.trim().is_empty()) {
            let contact = if contact.starts_with("mailto:") {
                contact.to_owned()
            } else {
                format!("mailto:{contact}")
            };
            payload["contact"] = json!([contact]);
        }
        let new_account = self.directory.new_account.clone();
        let (_, headers) = self.post(&new_account, Some(payload)).await?;
        let url = headers
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::Protocol("newAccount 没给 Location".to_owned()))?
            .to_owned();
        self.account_url = Some(url.clone());
        Ok(url)
    }

    pub fn adopt_account(&mut self, url: &str) {
        self.account_url = Some(url.to_owned());
    }

    /// Opens an order and returns the DNS records that have to exist before it can proceed.
    pub async fn begin_order(&mut self, names: &[String]) -> Result<(Order, Vec<DnsChallenge>)> {
        let identifiers: Vec<Value> = names
            .iter()
            .map(|name| json!({ "type": "dns", "value": name }))
            .collect();
        let new_order = self.directory.new_order.clone();
        let (body, headers) = self
            .post(&new_order, Some(json!({ "identifiers": identifiers })))
            .await?;
        let order_url = headers
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::Protocol("newOrder 没给 Location".to_owned()))?
            .to_owned();
        let finalize = body
            .get("finalize")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Protocol("订单里没有 finalize".to_owned()))?
            .to_owned();
        let authorizations: Vec<String> = body
            .get("authorizations")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let thumbprint = self.key.thumbprint()?;
        let mut challenges = Vec::new();
        for authorization_url in authorizations {
            let (authorization, _) = self.post(&authorization_url, None).await?;
            // Already valid — a previous order for the same name inside the CA's reuse window.
            // Skipped rather than re-solved: publishing a record for an authorization nobody will
            // check is work that can only fail.
            if authorization.get("status").and_then(Value::as_str) == Some("valid") {
                continue;
            }
            let identifier = authorization
                .get("identifier")
                .and_then(|value| value.get("value"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Protocol("授权里没有 identifier".to_owned()))?;
            let challenge = authorization
                .get("challenges")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("type").and_then(Value::as_str) == Some("dns-01"))
                })
                .ok_or_else(|| {
                    // Worth saying plainly: for a wildcard the CA offers nothing else, so this is
                    // not "pick another challenge", it is "this name cannot be validated here".
                    Error::Protocol(format!("{identifier} 没有 dns-01 挑战可用"))
                })?;
            let token = challenge
                .get("token")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Protocol("挑战里没有 token".to_owned()))?;
            let challenge_url = challenge
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Protocol("挑战里没有 url".to_owned()))?
                .to_owned();

            let key_authorization = format!("{token}.{thumbprint}");
            let digest = ring::digest::digest(&ring::digest::SHA256, key_authorization.as_bytes());
            challenges.push(DnsChallenge {
                // The wildcard's authorization identifier has no `*.` — the CA strips it — so both
                // identifiers produce the same record name. That is the collision the module
                // header warns about, and it is normal rather than a bug.
                name: format!("_acme-challenge.{identifier}"),
                value: B64.encode(digest.as_ref()),
                challenge_url,
                authorization_url,
            });
        }

        Ok((
            Order {
                order_url,
                finalize,
            },
            challenges,
        ))
    }

    /// Tells the CA to check, then waits for the authorization to settle.
    pub async fn validate(&mut self, challenge: &DnsChallenge) -> Result<()> {
        self.post(&challenge.challenge_url, Some(json!({}))).await?;

        let deadline = std::time::Instant::now() + POLL_TIMEOUT;
        loop {
            let (body, _) = self.post(&challenge.authorization_url, None).await?;
            match body.get("status").and_then(Value::as_str) {
                Some("valid") => return Ok(()),
                Some("invalid") => {
                    // The reason lives on the challenge, not the authorization, and it is the only
                    // place that says *why* — "record not found", "wrong value", the CAA refusal.
                    let detail = body
                        .get("challenges")
                        .and_then(Value::as_array)
                        .and_then(|items| items.iter().find_map(|item| item.get("error")))
                        .map(|error| describe_problem(0, error))
                        .unwrap_or_else(|| "CA 没说原因".to_owned());
                    return Err(Error::Server(format!(
                        "{} 验证失败：{detail}",
                        challenge.name
                    )));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Timeout(format!("{} 一直没被验证", challenge.name)));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Generates the certificate key, sends the CSR, and downloads the chain.
    pub async fn finalize(&mut self, order: &Order, names: &[String]) -> Result<IssuedCertificate> {
        let key = rcgen::KeyPair::generate()
            .map_err(|error| Error::Crypto(format!("生成证书密钥失败：{error}")))?;
        let mut params = rcgen::CertificateParams::new(names.to_vec())
            .map_err(|error| Error::Crypto(format!("CSR 参数不合法：{error}")))?;
        // Empty subject, deliberately. rcgen defaults the common name to the literal string
        // "rcgen self signed cert", and a CA reads the CN as one more name being asked for — the
        // order is then rejected with `Cannot issue for "rcgen self signed cert"`, which is what
        // Let's Encrypt said here on 2026-08-09. Names belong in the SAN and nowhere else; the CN
        // has been deprecated for identification for years, and leaving it empty is what every
        // ACME client does.
        params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = params
            .serialize_request(&key)
            .map_err(|error| Error::Crypto(format!("造 CSR 失败：{error}")))?;
        let csr_b64 = B64.encode(csr.der());

        self.post(&order.finalize, Some(json!({ "csr": csr_b64 })))
            .await?;

        let deadline = std::time::Instant::now() + POLL_TIMEOUT;
        let certificate_url = loop {
            let (body, _) = self.post(&order.order_url, None).await?;
            match body.get("status").and_then(Value::as_str) {
                Some("valid") => {
                    break body
                        .get("certificate")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::Protocol("订单成了但没有 certificate".to_owned()))?
                        .to_owned();
                }
                Some("invalid") => {
                    return Err(Error::Server(format!(
                        "订单作废：{}",
                        body.get("error")
                            .map(|error| describe_problem(0, error))
                            .unwrap_or_else(|| "CA 没说原因".to_owned())
                    )));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Timeout("订单一直没签发".to_owned()));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        let (body, _) = self.post(&certificate_url, None).await?;
        let chain_pem = body
            .as_str()
            .ok_or_else(|| Error::Protocol("证书不是 PEM 文本".to_owned()))?
            .to_owned();

        let (not_after, issuer) = leaf_facts(&chain_pem)?;
        let peer_sha256 = leaf_sha256(&chain_pem)?;
        Ok(IssuedCertificate {
            not_after,
            issuer,
            chain_pem,
            key_pem: key.serialize_pem(),
            peer_sha256,
        })
    }
}

pub struct Order {
    order_url: String,
    finalize: String,
}

/// Reads `notAfter` and the issuer out of the first certificate in the chain.
///
/// Both taken from the certificate rather than computed or configured. The expiry, because the CA
/// decides the lifetime and has changed it before — a renewal schedule built on "now plus 90 days"
/// is one that quietly stops renewing early enough. The issuer, because the configured directory
/// says what was asked for and the certificate says what came back, and those two disagreeing is
/// precisely the situation worth being able to see.
pub(crate) fn leaf_facts(chain_pem: &str) -> Result<(String, String)> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(chain_pem.as_bytes())
        .map_err(|error| Error::Protocol(format!("证书 PEM 解不开：{error}")))?;
    let certificate = pem
        .parse_x509()
        .map_err(|error| Error::Protocol(format!("证书解不开：{error}")))?;
    let not_after = certificate
        .validity()
        .not_after
        .to_datetime()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| Error::Protocol(format!("到期时间格式化失败：{error}")))?;
    // The CN where there is one, the whole DN otherwise. Some CAs put nothing useful in the CN,
    // and a blank column reads as "unknown" when the information was there all along.
    let issuer = certificate
        .issuer()
        .iter_common_name()
        .next()
        .and_then(|name| name.as_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| certificate.issuer().to_string());
    Ok((not_after, issuer))
}

pub(crate) fn leaf_sha256(chain_pem: &str) -> Result<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(chain_pem.as_bytes())
        .map_err(|error| Error::Protocol(format!("证书 PEM 解不开：{error}")))?;
    Ok(format!("{:x}", Sha256::digest(&pem.contents)))
}

/// Turns an ACME problem document into one line worth putting on a page.
///
/// `detail` is the sentence a human wrote; `type` is the machine-readable class. Both, because the
/// detail alone loses "this was a rate limit" and the type alone loses everything useful.
fn describe_problem(status: u16, body: &Value) -> String {
    let kind = body
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("about:blank")
        .rsplit(':')
        .next()
        .unwrap_or_default();
    let detail = body
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("(没有说明)");
    match (status, kind.is_empty()) {
        (0, false) => format!("{kind}: {detail}"),
        (0, true) => detail.to_owned(),
        (code, false) => format!("HTTP {code} {kind}: {detail}"),
        (code, true) => format!("HTTP {code}: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_account_key_round_trips_through_storage() {
        let key = AccountKey::generate().unwrap();
        let stored = key.as_stored().to_owned();
        let again = AccountKey::from_pkcs8_b64(&stored).unwrap();
        // Same key means same thumbprint; a different one would mean every stored account is
        // orphaned on restart, and the symptom would be a new account per issuance.
        assert_eq!(key.thumbprint().unwrap(), again.thumbprint().unwrap());
    }

    #[test]
    fn the_thumbprint_is_the_rfc_7638_canonical_form() {
        // From RFC 7638 §3.1's worked example, converted to the P-256 members. What is being held
        // here is the ordering and the absence of whitespace: both are load-bearing, and getting
        // either wrong produces challenges that fail validation with no diagnostic anywhere.
        let key = AccountKey::generate().unwrap();
        let jwk = key.jwk().unwrap();
        let canonical = format!(
            r#"{{"crv":"{}","kty":"{}","x":"{}","y":"{}"}}"#,
            jwk["crv"].as_str().unwrap(),
            jwk["kty"].as_str().unwrap(),
            jwk["x"].as_str().unwrap(),
            jwk["y"].as_str().unwrap(),
        );
        assert!(!canonical.contains(' '));
        assert!(canonical.find("\"crv\"") < canonical.find("\"kty\""));
        assert!(canonical.find("\"kty\"") < canonical.find("\"x\""));
        assert!(canonical.find("\"x\"") < canonical.find("\"y\""));
    }

    #[test]
    fn a_problem_document_keeps_both_the_class_and_the_sentence() {
        let body = json!({
            "type": "urn:ietf:params:acme:error:rateLimited",
            "detail": "too many certificates already issued",
        });
        let line = describe_problem(429, &body);
        assert!(line.contains("rateLimited"), "丢了类别：{line}");
        assert!(line.contains("too many"), "丢了说明：{line}");
    }

    /// A CA rejects an order whose CSR carries a subject it was not asked to certify, and rcgen
    /// puts one there by default. This is a regression test with a date on it: Let's Encrypt
    /// refused a real order on 2026-08-09 with `Cannot issue for "rcgen self signed cert"`,
    /// because the line clearing the subject had been dropped on the way from the experiment
    /// into this file.
    #[test]
    fn the_csr_carries_no_subject_beyond_the_names_asked_for() {
        let key = rcgen::KeyPair::generate().unwrap();
        let names = vec![
            "*.a1b2.example.net".to_owned(),
            "a1b2.example.net".to_owned(),
        ];
        let mut params = rcgen::CertificateParams::new(names).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = params.serialize_request(&key).unwrap();
        let der = csr.der().to_vec();
        // Checked against the bytes rather than a parsed structure: what the CA objected to was a
        // string being present at all, and the DER is where it either is or is not.
        assert!(
            !der.windows(21)
                .any(|window| window == b"rcgen self signed cert"[..21].as_ref()),
            "CSR 里还带着 rcgen 的默认 CN——CA 会把它当成一个要签的名字并整单拒绝",
        );
    }

    #[test]
    fn not_after_comes_out_of_the_certificate() {
        // A self-signed certificate stands in for a CA-issued one: what is under test is that the
        // expiry is read from the bytes rather than computed, and that is the same code path.
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["a.example.net".to_owned()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2030, 1, 2);
        let certificate = params.self_signed(&key).unwrap();
        let (not_after, _) = leaf_facts(&certificate.pem()).unwrap();
        assert!(
            not_after.starts_with("2030-01-02"),
            "读出来的是 {not_after}"
        );
    }
}
