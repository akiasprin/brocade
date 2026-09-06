//! Node TLS certificates: which domain they are issued under, and where each node's stands.
//!
//! Alongside `distribution.rs` rather than `settings.rs`, for its reason: none of this compiles
//! into an artifact, so writing it stamps no revision and travels through no release. A
//! certificate is not a decision about what the fleet should look like — it is a fact with an
//! expiry date, and recompiling an old revision must not drag along the certificate that happened
//! to be current when it was written.
//!
//! # Two secrets, and why they never leave this module in the clear
//!
//! The DNS credential and every node private key are sealed (`secrets.rs`). The public types here
//! carry `has_credential` and never the credential; reading the real values goes through
//! [`domain_secrets`] and [`node_key`], which exist for the issuing worker and for
//! [`cert_delta_for_node`], the desired-response check — and nothing else. The console API has
//! no route that returns either, and that is the property to preserve when adding one: a
//! certificate's private key is the machine's identity for as long as the certificate lives, and
//! the DNS credential can rewrite every record in the domain.
//!
//! # Why the label is random
//!
//! Every publicly-trusted certificate is published to Certificate Transparency, so the names are
//! public whatever they are. A readable label (`hk-01.edge.example.net`) would put the fleet's
//! topology in that public log; a random one still shows that a node exists but not what it is.
//! Nothing here pretends the label is a secret — it is only prevented from being a description.

use std::collections::BTreeSet;

use brocade_core::hash::{hex_lower, sha256_hex};
use brocade_deployment::protocol::NodeCertificateMaterial;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::secrets::{self, CTX_ACME_ACCOUNT, CTX_CERT_KEY, CTX_DNS_CREDENTIAL};
use crate::{AdminContext, Result, StoreError};

/// Let's Encrypt's production directory. Named here so the console's default and the operator's
/// choice are the same string.
pub const ACME_LETSENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Its staging directory. Different account, far looser limits, certificates that nothing trusts.
pub const ACME_LETSENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
/// Stored in the existing directory column so a certificate's requested authority remains one
/// comparable value. It is not a URL and is admitted explicitly by the database constraint.
pub const SELF_SIGNED_DIRECTORY: &str = "self-signed";
/// Bytes of randomness in a label. Eight hex characters — enough that labels do not collide within
/// a fleet, short enough to read out over a call.
const LABEL_BYTES: usize = 4;
const CERTIFICATE_SCAN_ADVISORY_KEY: i64 = 0x4252_4f43_4345_5254;
pub const DEFAULT_SELF_SIGNED_GROUP_NAME: &str = "默认组";
pub const SELF_SIGNED_INITIAL_POOL_SIZE: usize = 5;
pub const SELF_SIGNED_MAX_POOL_SIZE: i64 = 10;

/// A transaction whose only job is to hold the fleet-wide issuance lock. Dropping it rolls the
/// transaction back and releases the lock, including on task cancellation or an early return.
pub struct CertificateScanLock {
    _transaction: Transaction<'static, Postgres>,
}

pub async fn try_certificate_scan_lock(pool: &PgPool) -> Result<Option<CertificateScanLock>> {
    let mut transaction = pool.begin().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(CERTIFICATE_SCAN_ADVISORY_KEY)
        .fetch_one(&mut *transaction)
        .await?;
    Ok(acquired.then_some(CertificateScanLock {
        _transaction: transaction,
    }))
}

/// A domain the fleet issues node certificates under.
///
/// The console offers one. The table allows several because the cost of splitting later is not the
/// schema but the re-issuance: a node's certificate name is the SNI its clients send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertDomain {
    pub id: String,
    pub domain: String,
    pub dns_provider: String,
    pub acme_directory: String,
    #[serde(default)]
    pub acme_contact: Option<String>,
    pub renew_before_days: i32,
    /// Whether a DNS credential is stored — never the credential. The console needs to render
    /// "configured" without being able to read it back.
    pub has_credential: bool,
    /// Whether an ACME account has been registered against this directory yet. Absent is normal
    /// before the first issuance; the worker registers one and records it.
    pub has_account: bool,
    pub signing_method: CertificateSigningMethod,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CertificateSigningMethod {
    #[default]
    PublicCa,
    SelfSigned,
}

impl CertificateSigningMethod {
    pub fn from_directory(directory: &str) -> Self {
        if directory == SELF_SIGNED_DIRECTORY {
            Self::SelfSigned
        } else {
            Self::PublicCa
        }
    }
}

/// What a caller may set. Separate from [`CertDomain`] because the credential is write-only:
/// sending `None` keeps whatever is stored, which is what lets the settings form be saved without
/// re-typing a secret it cannot display.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertDomainInput {
    pub domain: String,
    #[serde(default)]
    pub signing_method: CertificateSigningMethod,
    #[serde(default)]
    pub dns_credential: Option<String>,
    #[serde(default)]
    pub acme_directory: Option<String>,
    #[serde(default)]
    pub acme_contact: Option<String>,
    #[serde(default)]
    pub renew_before_days: Option<i32>,
}

/// A certificate group as the console shows it. No key, ever.
///
/// The group is the unit an operator manages: machines join it, certificates roll inside it, and
/// its `label` is the SNI every one of those machines presents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertGroup {
    pub id: String,
    pub domain: String,
    pub label: String,
    pub name: String,
    /// The group created with a fresh installation. Its visible name and existence are stable.
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub note: Option<String>,
    pub status: String,
    /// The two names in the certificate. Both, because a wildcard covers one label and does not
    /// cover itself — `*.a.example.net` does not match `a.example.net`.
    pub names: Vec<String>,
    /// Machines drawing their certificate from this group.
    pub nodes: Vec<String>,
    /// Every certificate held for this group: the serving one first, then spares.
    pub certificates: Vec<GroupCertificate>,
}

/// One certificate inside a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCertificate {
    pub id: String,
    /// `pending`, `ready` (a spare), `serving`, `superseded`, or `failed`.
    pub status: String,
    /// `renewal` (the scan asked for it), `spare` (an operator asked for it), or `bootstrap`
    /// (one of the five leaves a fresh installation starts with).
    pub origin: String,
    /// The authority that actually issued this row, frozen when it was stored. It can differ from
    /// the domain's current setting while old leaves remain in the client trust set.
    pub signing_method: CertificateSigningMethod,
    /// Whose signature it carries, as the certificate itself states it. `None` before the first
    /// issuance. This is the one field that tells a staging certificate from a real one, and it
    /// cannot be derived from the configured directory — that says what was asked for.
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub issued_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Lowercase hex sha256 of the certificate, which is what a node reports holding. `None`
    /// until the certificate exists.
    #[serde(default)]
    pub sha256: Option<String>,
    pub attempts: i32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_attempt_at: Option<String>,
}

/// What one machine holds, judged against what its group is serving.
///
/// Per machine rather than per certificate, because ten machines in one group share one
/// certificate and have ten independent answers about whether they have it — which is precisely
/// what has to be visible while a roll is in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCertificateState {
    pub node_id: String,
    pub label_id: String,
    pub group_name: String,
    /// The bare name, which is the `sni` this machine's subscriptions carry.
    pub certificate_name: String,
    /// `unknown` (never reported — an agent too old to manage certificates), `absent` (it looked
    /// and there is nothing), `current`, or `stale` (it holds one, but not the one this group
    /// serves — expected for up to an hour after a roll, since xray reloads on its own timer).
    pub on_disk: String,
    #[serde(default)]
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServingCertificateProfile {
    pub name: String,
    /// Pinning remains active while any retained, issued certificate in the group is self-signed.
    /// A later public certificate therefore overlaps the old self-signed leaf until an operator
    /// explicitly deletes the latter.
    pub requires_pinning: bool,
    pub trusted_peer_sha256: Vec<String>,
}

/// A unit of work for the issuing worker: one group that needs a certificate now.
///
/// Keyed on the group rather than the machine, which is the entire point of grouping — Let's
/// Encrypt counts certificates, and ten machines sharing a group cost one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateOrder {
    /// The `certificates` row this order fills in. Created before the CA is contacted so that a
    /// crash mid-issuance leaves a row saying an attempt happened, not a silent gap.
    pub certificate_id: String,
    pub label_id: String,
    pub domain_id: String,
    pub domain: String,
    pub label: String,
    /// Exact SNI used by self-signed groups. Public-CA groups keep the legacy wildcard pair.
    pub certificate_name: Option<String>,
    pub acme_directory: String,
    pub acme_contact: Option<String>,
    /// True where the group already serves a certificate and this one will replace it. Only used
    /// for wording — the work is identical either way, which is deliberate: a renewal that behaves
    /// differently from a first issuance is a path exercised eight times less often.
    pub renewal: bool,
}

/// The immutable facts produced by one certificate order. Keeping the directory beside the bytes
/// makes it impossible for the write path to relabel an in-flight result with a newer setting.
pub struct IssuedCertificate<'a> {
    pub certificate_id: &'a str,
    pub acme_directory: &'a str,
    pub cert_pem: &'a str,
    pub key_pem: &'a str,
    pub not_after: &'a str,
    pub issuer: &'a str,
    pub peer_sha256: &'a str,
}

impl CertificateOrder {
    /// The names to ask for, wildcard first. Order is fixed so that two runs asking for the same
    /// thing produce the same request.
    pub fn names(&self) -> Vec<String> {
        names_of(&self.label, &self.domain, self.certificate_name.as_deref())
    }
}

fn names_of(label: &str, domain: &str, certificate_name: Option<&str>) -> Vec<String> {
    match certificate_name {
        Some(name) => vec![name.to_owned()],
        None => vec![format!("*.{label}.{domain}"), format!("{label}.{domain}")],
    }
}

fn certificate_name_of(label: &str, domain: &str, certificate_name: Option<&str>) -> String {
    certificate_name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{label}.{domain}"))
}

fn require_system_admin(actor: &AdminContext, what: &str) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(format!(
            "only system-admin can {what}"
        )));
    }
    Ok(())
}

fn normalize_domain(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// A fresh label. Collisions inside one domain are caught by the unique constraint rather than by
/// checking first — the check would be a race, the constraint is not.
fn generate_label() -> Result<String> {
    let mut bytes = [0u8; LABEL_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(hex_lower(&bytes))
}

/// A row id. Wider than a label because it is never read aloud and never appears in a name — it
/// only has to not collide.
fn generate_id() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(hex_lower(&bytes))
}

/// A plausible private hostname which can never collide with a real public site. RFC 2606
/// reserves `.test`; the two words make the value readable while the random suffix makes each
/// installation and manually-created self-signed group distinct.
fn generate_reserved_certificate_name() -> Result<String> {
    const FIRST: &[&str] = &[
        "amber",
        "arcadia",
        "cedar",
        "coral",
        "harbor",
        "lumen",
        "northstar",
        "silver",
        "solace",
        "verdant",
        "velora",
        "willow",
    ];
    const SECOND: &[&str] = &[
        "bridge", "cloud", "edge", "gateway", "network", "relay", "services", "systems",
    ];
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "{}-{}-{}.test",
        FIRST[usize::from(bytes[0]) % FIRST.len()],
        SECOND[usize::from(bytes[1]) % SECOND.len()],
        hex_lower(&bytes[2..])
    ))
}

fn normalize_certificate_name(raw: &str) -> Result<String> {
    let name = normalize_domain(raw);
    let valid = name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
        });
    if !valid {
        return Err(StoreError::InvalidData(
            "自定义证书域名不是有效的完整域名".to_owned(),
        ));
    }
    Ok(name)
}

/// Creates the private certificate pool for a genuinely fresh installation. Existing certificate
/// configuration is left byte-for-byte alone: changing its SNI would invalidate subscriptions
/// which may already be in circulation.
pub async fn ensure_default_self_signed_pool(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(CERTIFICATE_SCAN_ADVISORY_KEY)
        .execute(&mut *tx)
        .await?;
    let configured: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cert_domains)")
        .fetch_one(&mut *tx)
        .await?;
    if configured {
        tx.commit().await?;
        return Ok(0);
    }

    let certificate_name = generate_reserved_certificate_name()?;
    let group_id = generate_id()?;
    let label = generate_label()?;
    sqlx::query(
        "INSERT INTO cert_domains
             (id, domain, dns_provider, acme_directory, renew_before_days)
         VALUES ($1, $1, 'cloudflare', 'self-signed', 30)",
    )
    .bind(&certificate_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO cert_labels
             (id, domain_id, label, name, note, certificate_name, is_default)
         VALUES ($1, $2, $3, $4, '新安装自动创建的自签证书池', $2, TRUE)",
    )
    .bind(&group_id)
    .bind(&certificate_name)
    .bind(&label)
    .bind(DEFAULT_SELF_SIGNED_GROUP_NAME)
    .execute(&mut *tx)
    .await?;
    for _ in 0..SELF_SIGNED_INITIAL_POOL_SIZE {
        sqlx::query(
            "INSERT INTO certificates (id, label_id, origin)
             VALUES ($1, $2, 'bootstrap')",
        )
        .bind(generate_id()?)
        .bind(&group_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(SELF_SIGNED_INITIAL_POOL_SIZE)
}

pub async fn list_cert_domains(pool: &PgPool) -> Result<Vec<CertDomain>> {
    let rows = sqlx::query(
        "SELECT id, domain, dns_provider, acme_directory, acme_contact, renew_before_days,
                (dns_credential_sealed IS NOT NULL) AS has_credential,
                (acme_account_key_sealed IS NOT NULL) AS has_account
           FROM cert_domains
          ORDER BY domain",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let directory: String = row.try_get("acme_directory")?;
            Ok(CertDomain {
                id: row.try_get("id")?,
                domain: row.try_get("domain")?,
                dns_provider: row.try_get("dns_provider")?,
                signing_method: CertificateSigningMethod::from_directory(&directory),
                acme_directory: directory,
                acme_contact: row.try_get("acme_contact")?,
                renew_before_days: row.try_get("renew_before_days")?,
                has_credential: row.try_get("has_credential")?,
                has_account: row.try_get("has_account")?,
            })
        })
        .collect()
}

/// Creates or updates the domain. Keyed on the domain name rather than an id the caller invents,
/// because the console offers one domain and re-saving the form must not silently make a second.
pub async fn upsert_cert_domain(
    pool: &PgPool,
    actor: &AdminContext,
    input: CertDomainInput,
) -> Result<CertDomain> {
    require_system_admin(actor, "manage certificate domains")?;

    let directory = match input.signing_method {
        CertificateSigningMethod::SelfSigned => SELF_SIGNED_DIRECTORY.to_owned(),
        CertificateSigningMethod::PublicCa => input
            .acme_directory
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                StoreError::InvalidData(
                    "certificate directory is required for ACME issuance".to_owned(),
                )
            })?
            .to_owned(),
    };
    let domain = match (input.signing_method, normalize_domain(&input.domain)) {
        (CertificateSigningMethod::SelfSigned, domain) if domain.is_empty() => {
            generate_reserved_certificate_name()?
        }
        (_, domain) if domain.is_empty() => {
            return Err(StoreError::InvalidData(
                "certificate domain is required".to_owned(),
            ));
        }
        (_, domain) => domain,
    };
    // A label is added beneath this, so a bare TLD or a name with no dot cannot work. Refused here
    // with a sentence rather than left to the CHECK constraint, whose message names a regex.
    if !domain.contains('.') {
        return Err(StoreError::InvalidData(format!(
            "{domain} is not a domain the fleet can issue under — it needs at least one dot"
        )));
    }
    if input.signing_method == CertificateSigningMethod::PublicCa
        && !directory.starts_with("https://")
    {
        return Err(StoreError::InvalidData(
            "the ACME directory must be an https URL".to_owned(),
        ));
    }
    let renew_before = input.renew_before_days.unwrap_or(30);
    if !(1..=89).contains(&renew_before) {
        return Err(StoreError::InvalidData(
            "renew_before_days must be between 1 and 89".to_owned(),
        ));
    }

    // Sealed before the transaction so that a missing BROCADE_SECRET_KEY fails the save outright
    // rather than writing the rest of the row and leaving a domain with no credential.
    let sealed = match input.dns_credential.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => Some(secrets::seal(CTX_DNS_CREDENTIAL, value)?),
        _ => None,
    };

    // Changing the directory invalidates the account: staging and production accounts are not
    // interchangeable, and keeping the old one would have the worker present a staging account to
    // production and fail in a way that reads as a permissions problem.
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "INSERT INTO cert_domains (id, domain, dns_provider, dns_credential_sealed,
                                   acme_directory, acme_contact, renew_before_days)
         VALUES ($1, $1, 'cloudflare', $2, $3, $4, $5)
         ON CONFLICT (domain) DO UPDATE SET
             dns_credential_sealed = COALESCE(EXCLUDED.dns_credential_sealed,
                                              cert_domains.dns_credential_sealed),
             acme_contact = EXCLUDED.acme_contact,
             renew_before_days = EXCLUDED.renew_before_days,
             acme_directory = EXCLUDED.acme_directory,
             acme_account_key_sealed = CASE
                 WHEN cert_domains.acme_directory = EXCLUDED.acme_directory
                 THEN cert_domains.acme_account_key_sealed ELSE NULL END,
             acme_account_url = CASE
                 WHEN cert_domains.acme_directory = EXCLUDED.acme_directory
                 THEN cert_domains.acme_account_url ELSE NULL END
         RETURNING id, domain, dns_provider, acme_directory, acme_contact, renew_before_days,
                   (dns_credential_sealed IS NOT NULL) AS has_credential,
                   (acme_account_key_sealed IS NOT NULL) AS has_account",
    )
    .bind(&domain)
    .bind(sealed.as_deref())
    .bind(&directory)
    .bind(
        input
            .acme_contact
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(renew_before)
    .fetch_one(&mut *tx)
    .await?;

    let domain_id: String = row.try_get("id")?;
    tx.commit().await?;

    let directory: String = row.try_get("acme_directory")?;
    Ok(CertDomain {
        id: domain_id,
        domain: row.try_get("domain")?,
        dns_provider: row.try_get("dns_provider")?,
        signing_method: CertificateSigningMethod::from_directory(&directory),
        acme_directory: directory,
        acme_contact: row.try_get("acme_contact")?,
        renew_before_days: row.try_get("renew_before_days")?,
        has_credential: row.try_get("has_credential")?,
        has_account: row.try_get("has_account")?,
    })
}

/// The DNS credential and ACME account for one domain, decrypted. For the issuing worker.
pub async fn domain_secrets(
    pool: &PgPool,
    domain_id: &str,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let row = sqlx::query(
        "SELECT dns_credential_sealed, acme_account_key_sealed, acme_account_url
           FROM cert_domains WHERE id = $1",
    )
    .bind(domain_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("certificate domain {domain_id}")))?;

    let credential: Option<String> = row.try_get("dns_credential_sealed")?;
    let account: Option<String> = row.try_get("acme_account_key_sealed")?;
    let url: Option<String> = row.try_get("acme_account_url")?;
    Ok((
        credential
            .map(|v| secrets::open(CTX_DNS_CREDENTIAL, &v))
            .transpose()?,
        account
            .map(|v| secrets::open(CTX_ACME_ACCOUNT, &v))
            .transpose()?,
        url,
    ))
}

/// Records the ACME account the worker registered, so the next issuance reuses it. Reusing matters
/// for more than tidiness: a new account per issuance is its own rate limit, and one account is
/// what lets orders under this domain be counted in one place.
pub async fn save_acme_account(
    pool: &PgPool,
    domain_id: &str,
    account_key_pem: &str,
    account_url: &str,
) -> Result<()> {
    let sealed = secrets::seal(CTX_ACME_ACCOUNT, account_key_pem)?;
    sqlx::query(
        "UPDATE cert_domains SET acme_account_key_sealed = $2, acme_account_url = $3 WHERE id = $1",
    )
    .bind(domain_id)
    .bind(sealed)
    .bind(account_url)
    .execute(pool)
    .await?;
    Ok(())
}

/// Creates a group. The name is what an operator will pick it by, so it is required.
pub async fn create_cert_label(
    pool: &PgPool,
    actor: &AdminContext,
    domain_id: &str,
    name: &str,
    note: Option<&str>,
) -> Result<String> {
    create_cert_label_inner(pool, actor, domain_id, name, note, None, false).await
}

/// Creates a group from the console. Self-signed groups get one exact synthetic SNI by default;
/// a custom exact SNI is accepted only on this explicit manual-create path.
pub async fn create_cert_label_with_certificate_name(
    pool: &PgPool,
    actor: &AdminContext,
    domain_id: &str,
    name: &str,
    note: Option<&str>,
    certificate_name: Option<&str>,
) -> Result<String> {
    create_cert_label_inner(pool, actor, domain_id, name, note, certificate_name, true).await
}

async fn create_cert_label_inner(
    pool: &PgPool,
    actor: &AdminContext,
    domain_id: &str,
    name: &str,
    note: Option<&str>,
    certificate_name: Option<&str>,
    synthesize_self_signed_name: bool,
) -> Result<String> {
    require_system_admin(actor, "create a certificate group")?;
    let name = name.trim();
    if name.is_empty() {
        return Err(StoreError::InvalidData(
            "certificate group name is required".to_owned(),
        ));
    }
    let directory: String =
        sqlx::query_scalar("SELECT acme_directory FROM cert_domains WHERE id = $1")
            .bind(domain_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| StoreError::InvalidData(format!("没有这个证书域：{domain_id}")))?;
    let certificate_name = if directory == SELF_SIGNED_DIRECTORY && synthesize_self_signed_name {
        Some(
            match certificate_name
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(value) => normalize_certificate_name(value)?,
                None => generate_reserved_certificate_name()?,
            },
        )
    } else if certificate_name.is_some_and(|value| !value.trim().is_empty()) {
        return Err(StoreError::InvalidData(
            "只有自签证书组可以指定独立证书域名".to_owned(),
        ));
    } else {
        None
    };
    let id = generate_id()?;
    for _ in 0..4 {
        let label = generate_label()?;
        let result = sqlx::query(
            "INSERT INTO cert_labels (id, domain_id, label, name, note, certificate_name)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(domain_id)
        .bind(&label)
        .bind(name)
        .bind(note.map(str::trim).filter(|note| !note.is_empty()))
        .bind(&certificate_name)
        .execute(pool)
        .await;
        match result {
            Ok(_) => return Ok(id),
            // Two unique constraints can fire here and they need opposite answers: a label
            // collision is ours to retry, a name collision is the operator's to fix.
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
                if error
                    .constraint()
                    .is_some_and(|name| name == "cert_labels_name_key")
                {
                    return Err(StoreError::InvalidData(format!("证书组名 {name} 已经有了")));
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(StoreError::InvalidData(
        "could not allocate a label for this certificate group".to_owned(),
    ))
}

/// Renames a group or edits its note. The default group's name is fixed; its note is not.
pub async fn update_cert_label(
    pool: &PgPool,
    actor: &AdminContext,
    id: &str,
    name: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    require_system_admin(actor, "edit a certificate group")?;
    if let Some(name) = name {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::InvalidData(
                "certificate group name is required".to_owned(),
            ));
        }
        let is_default: bool =
            sqlx::query_scalar("SELECT is_default FROM cert_labels WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?
                .ok_or_else(|| StoreError::InvalidData(format!("没有这个证书组：{id}")))?;
        if is_default && name != DEFAULT_SELF_SIGNED_GROUP_NAME {
            return Err(StoreError::InvalidData("默认组不能改名".to_owned()));
        }
        sqlx::query("UPDATE cert_labels SET name = $2 WHERE id = $1")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .map_err(|error| match error {
                sqlx::Error::Database(error) if error.code().as_deref() == Some("23505") => {
                    StoreError::InvalidData(format!("证书组名 {name} 已经有了"))
                }
                error => error.into(),
            })?;
    }
    if let Some(note) = note {
        let note = note.trim();
        sqlx::query("UPDATE cert_labels SET note = $2 WHERE id = $1")
            .bind(id)
            .bind((!note.is_empty()).then_some(note))
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Deletes a group. Refused while machines still draw from it — the machines would silently lose
/// their SNI, and the first anyone would hear of it is a compile error on the next release.
pub async fn delete_cert_label(pool: &PgPool, actor: &AdminContext, id: &str) -> Result<()> {
    require_system_admin(actor, "delete a certificate group")?;
    let is_default: bool = sqlx::query_scalar("SELECT is_default FROM cert_labels WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| StoreError::InvalidData(format!("没有这个证书组：{id}")))?;
    if is_default {
        return Err(StoreError::InvalidData("默认组不能删除".to_owned()));
    }
    let using: i64 = sqlx::query("SELECT count(*) AS n FROM node_cert_label WHERE label_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?
        .try_get("n")?;
    if using != 0 {
        return Err(StoreError::InvalidData(format!(
            "还有 {using} 台机器在用这个组，先把它们改到别的组"
        )));
    }
    sqlx::query("DELETE FROM cert_labels WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Points a machine at a group, inside the caller's transaction.
///
/// `None` means no group, which is a legitimate resting state rather than a gap to fill: a fleet
/// running REALITY against external sites needs no certificate at all, and there is no group that
/// would be the right guess for a machine whose operator named none — picking one would decide
/// which machines are seen together on their behalf.
///
/// A machine without a group has no `certificate_name`, and that is exactly the state
/// `ingress.tls-no-certificate` refuses to publish a TLS or Hysteria 2 ingress in.
pub async fn assign_node_label(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label_id: Option<&str>,
) -> Result<()> {
    let Some(label_id) = label_id else {
        sqlx::query("DELETE FROM node_cert_label WHERE node_id = $1")
            .bind(node_id)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO node_cert_label (node_id, label_id) VALUES ($1, $2)
         ON CONFLICT (node_id) DO UPDATE SET label_id = EXCLUDED.label_id, assigned_at = now()",
    )
    .bind(node_id)
    .bind(label_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(inner) if inner.code().as_deref() == Some("23503") => {
            StoreError::InvalidData(format!("没有这个证书组：{label_id}"))
        }
        error => error.into(),
    })?;
    Ok(())
}

/// Moves a machine to another group.
///
/// This changes the machine's SNI. Every subscription already handed out for its TLS and Hysteria
/// 2 ingresses names the old group and stops working — the console says so before calling this,
/// which is why nothing is checked here beyond the group existing.
pub async fn set_node_label(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    label_id: Option<&str>,
) -> Result<()> {
    require_system_admin(actor, "move a machine to another certificate group")?;
    let mut tx = pool.begin().await?;
    assign_node_label(&mut tx, node_id, label_id).await?;
    tx.commit().await?;
    Ok(())
}

/// One group's name and where it should point.
///
/// The A record is not what carries traffic — a subscription dials the machine's address and
/// merely presents this name as SNI. It exists so the name resolves at all: a certificate name
/// that resolves to nothing is a signal in itself, and an active prober asking for it should find
/// an ordinary site.
///
/// A group spanning several machines still gets one record. Which machine it points at is picked
/// deterministically (lowest node id) so that repeated scans do not flap the record between
/// members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateDnsTarget {
    pub label_id: String,
    /// The bare name, `<label>.<domain>` — the one the wildcard does not cover.
    pub name: String,
    pub ipv4: String,
}

pub async fn certificate_dns_targets(pool: &PgPool) -> Result<Vec<CertificateDnsTarget>> {
    let rows = sqlx::query(
        "SELECT l.id AS label_id, l.label, l.certificate_name, d.domain,
                (SELECT n.public_ipv4
                   FROM node_cert_label m
                   JOIN nodes n ON n.id = m.node_id
                  WHERE m.label_id = l.id AND n.public_ipv4 IS NOT NULL
                    AND n.retired_at IS NULL
                  ORDER BY n.id
                  LIMIT 1) AS ipv4
           FROM cert_labels l
           JOIN cert_domains d ON d.id = l.domain_id
          WHERE l.status = 'active'
            AND d.acme_directory <> 'self-signed'
            AND EXISTS (SELECT 1 FROM certificates c
                         WHERE c.label_id = l.id AND c.status = 'serving')
          ORDER BY l.id",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .filter_map(|row| {
            let ipv4: Option<String> = row.try_get("ipv4").ok()?;
            let ipv4 = ipv4?;
            let label: String = row.try_get("label").ok()?;
            let domain: String = row.try_get("domain").ok()?;
            let certificate_name: Option<String> = row.try_get("certificate_name").ok()?;
            let label_id: String = row.try_get("label_id").ok()?;
            Some(Ok(CertificateDnsTarget {
                label_id,
                name: certificate_name_of(&label, &domain, certificate_name.as_deref()),
                ipv4,
            }))
        })
        .collect()
}

/// Every group with its certificates and members. What the console's certificate page shows.
pub async fn list_cert_groups(pool: &PgPool, actor: &AdminContext) -> Result<Vec<CertGroup>> {
    require_system_admin(actor, "view certificate status")?;
    let groups = sqlx::query(
        "SELECT l.id, l.label, l.name, l.note, l.status, l.certificate_name, l.is_default,
                d.domain,
                d.acme_directory AS configured_directory
           FROM cert_labels l
           JOIN cert_domains d ON d.id = l.domain_id
          ORDER BY l.name",
    )
    .fetch_all(pool)
    .await?;

    // Two more queries rather than one join per group: the page shows every group, and a join
    // would multiply rows by certificates by members and be taken apart again here anyway.
    let members =
        sqlx::query("SELECT label_id, node_id FROM node_cert_label ORDER BY label_id, node_id")
            .fetch_all(pool)
            .await?;
    let certs = sqlx::query(
        // The digest is recomputed from `cert_pem` on every read: storing it would be a second
        // copy of a fact already in the row, and the two would eventually disagree.
        "SELECT id, label_id, status, origin, issuer, attempts, last_error, acme_directory,
                issued_at::text AS issued_at, expires_at::text AS expires_at,
                last_attempt_at::text AS last_attempt_at,
                CASE WHEN cert_pem IS NULL THEN NULL
                     ELSE encode(sha256(cert_pem::bytea), 'hex') END AS sha256
           FROM certificates
          ORDER BY label_id,
                   CASE status WHEN 'serving' THEN 0 WHEN 'ready' THEN 1 WHEN 'pending' THEN 2
                               WHEN 'failed' THEN 3 ELSE 4 END,
                   expires_at NULLS LAST",
    )
    .fetch_all(pool)
    .await?;

    groups
        .iter()
        .map(|row| {
            let id: String = row.try_get("id")?;
            let label: String = row.try_get("label")?;
            let domain: String = row.try_get("domain")?;
            let certificate_name: Option<String> = row.try_get("certificate_name")?;
            let configured_directory: String = row.try_get("configured_directory")?;
            Ok(CertGroup {
                names: names_of(&label, &domain, certificate_name.as_deref()),
                nodes: members
                    .iter()
                    .filter(|member| {
                        member.try_get::<String, _>("label_id").ok().as_deref() == Some(id.as_str())
                    })
                    .map(|member| member.try_get("node_id"))
                    .collect::<std::result::Result<_, _>>()?,
                certificates: certs
                    .iter()
                    .filter(|cert| {
                        cert.try_get::<String, _>("label_id").ok().as_deref() == Some(id.as_str())
                    })
                    .map(|cert| {
                        // Pending and failed rows do not have an issuing directory yet. They still
                        // belong on the settings page, using the group's currently configured
                        // method until an issued certificate freezes the actual authority here.
                        let issued_directory: Option<String> = cert.try_get("acme_directory")?;
                        let directory =
                            issued_directory.as_deref().unwrap_or(&configured_directory);
                        Ok(GroupCertificate {
                            id: cert.try_get("id")?,
                            status: cert.try_get("status")?,
                            origin: cert.try_get("origin")?,
                            signing_method: CertificateSigningMethod::from_directory(directory),
                            issuer: cert.try_get("issuer")?,
                            issued_at: cert.try_get("issued_at")?,
                            expires_at: cert.try_get("expires_at")?,
                            sha256: cert.try_get("sha256")?,
                            attempts: cert.try_get("attempts")?,
                            last_error: cert.try_get("last_error")?,
                            last_attempt_at: cert.try_get("last_attempt_at")?,
                        })
                    })
                    .collect::<Result<_>>()?,
                id,
                label,
                domain,
                name: row.try_get("name")?,
                is_default: row.try_get("is_default")?,
                note: row.try_get("note")?,
                status: row.try_get("status")?,
            })
        })
        .collect()
}

/// What each machine holds, judged against what its group serves.
pub async fn list_node_certificate_state(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<Vec<NodeCertificateState>> {
    require_system_admin(actor, "view certificate status")?;
    let rows = sqlx::query(
        // `on_disk` is decided here rather than in the console, so that the comparison and the
        // bytes being compared never travel apart.
        "SELECT m.node_id, m.label_id, l.name AS group_name, l.label, l.certificate_name,
                d.domain,
                s.observed_at::text AS observed_at,
                CASE
                    WHEN s.observed_state IS NULL THEN 'unknown'
                    WHEN s.observed_state = 'absent' THEN 'absent'
                    WHEN s.observed_sha256 = (SELECT encode(sha256(c.cert_pem::bytea), 'hex')
                                                FROM certificates c
                                               WHERE c.label_id = m.label_id
                                                 AND c.status = 'serving') THEN 'current'
                    ELSE 'stale'
                END AS on_disk
           FROM node_cert_label m
           JOIN cert_labels l ON l.id = m.label_id
           JOIN cert_domains d ON d.id = l.domain_id
           LEFT JOIN node_cert_state s ON s.node_id = m.node_id
          ORDER BY m.node_id",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let label: String = row.try_get("label")?;
            let domain: String = row.try_get("domain")?;
            let certificate_name: Option<String> = row.try_get("certificate_name")?;
            Ok(NodeCertificateState {
                node_id: row.try_get("node_id")?,
                label_id: row.try_get("label_id")?,
                group_name: row.try_get("group_name")?,
                certificate_name: certificate_name_of(&label, &domain, certificate_name.as_deref()),
                on_disk: row.try_get("on_disk")?,
                observed_at: row.try_get("observed_at")?,
            })
        })
        .collect()
}

/// Names whose retained trust set still contains a self-signed certificate. This deliberately
/// outlives the serving row: changing the node to a public certificate does not silently tighten
/// clients before the operator removes the old leaf from the database.
pub(crate) async fn self_signed_certificate_names(pool: &PgPool) -> Result<BTreeSet<String>> {
    let rows = sqlx::query(
        "SELECT l.label, l.certificate_name, d.domain
           FROM cert_labels l
           JOIN cert_domains d ON d.id = l.domain_id
           JOIN certificates c ON c.label_id = l.id AND c.status = 'serving'
          WHERE EXISTS (
                SELECT 1 FROM certificates trusted
                 WHERE trusted.label_id = l.id
                   AND trusted.status IN ('ready', 'serving', 'superseded')
                   AND trusted.acme_directory = 'self-signed'
                   AND trusted.peer_sha256 IS NOT NULL
          )",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let label: String = row.try_get("label")?;
            let domain: String = row.try_get("domain")?;
            let certificate_name: Option<String> = row.try_get("certificate_name")?;
            Ok(certificate_name_of(
                &label,
                &domain,
                certificate_name.as_deref(),
            ))
        })
        .collect()
}

pub(crate) async fn serving_certificate_profile_for_node(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<ServingCertificateProfile>> {
    let row = sqlx::query(
        "SELECT l.label, l.certificate_name, d.domain,
                EXISTS (
                    SELECT 1 FROM certificates trusted
                     WHERE trusted.label_id = l.id
                       AND trusted.status IN ('ready', 'serving', 'superseded')
                       AND trusted.acme_directory = 'self-signed'
                       AND trusted.peer_sha256 IS NOT NULL
                ) AS requires_pinning,
                ARRAY(
                    SELECT trusted.peer_sha256
                      FROM certificates trusted
                     WHERE trusted.label_id = l.id
                       AND trusted.status IN ('ready', 'serving', 'superseded')
                       AND trusted.peer_sha256 IS NOT NULL
                     ORDER BY trusted.issued_at, trusted.id
                ) AS trusted_peer_sha256
           FROM node_cert_label m
           JOIN cert_labels l ON l.id = m.label_id
           JOIN cert_domains d ON d.id = l.domain_id
           JOIN certificates c ON c.label_id = l.id AND c.status = 'serving'
          WHERE m.node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(ServingCertificateProfile {
            name: certificate_name_of(
                &row.try_get::<String, _>("label")?,
                &row.try_get::<String, _>("domain")?,
                row.try_get::<Option<String>, _>("certificate_name")?
                    .as_deref(),
            ),
            requires_pinning: row.try_get("requires_pinning")?,
            trusted_peer_sha256: row.try_get("trusted_peer_sha256")?,
        })
    })
    .transpose()
}

/// Asks for one more certificate in a group, to be held as a spare.
///
/// Manual because the alternative — keeping spares topped up automatically — spends the quota that
/// caps everything else here: five certificates per identical name set per seven days, fifty per
/// registered domain. A spare is worth one of those only when somebody has a reason.
pub async fn request_spare_certificate(
    pool: &PgPool,
    actor: &AdminContext,
    label_id: &str,
) -> Result<String> {
    require_system_admin(actor, "request a spare certificate")?;
    let mut tx = pool.begin().await?;
    let directory: String = sqlx::query_scalar(
        "SELECT d.acme_directory
           FROM cert_labels l
           JOIN cert_domains d ON d.id = l.domain_id
          WHERE l.id = $1
          FOR UPDATE OF l",
    )
    .bind(label_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData(format!("没有这个证书组：{label_id}")))?;
    let count: i64 = if directory == SELF_SIGNED_DIRECTORY {
        sqlx::query_scalar("SELECT count(*) FROM certificates WHERE label_id = $1")
            .bind(label_id)
            .fetch_one(&mut *tx)
            .await?
    } else {
        sqlx::query_scalar(
            "SELECT count(*) FROM certificates
              WHERE label_id = $1 AND status IN ('pending', 'ready')",
        )
        .bind(label_id)
        .fetch_one(&mut *tx)
        .await?
    };
    if directory == SELF_SIGNED_DIRECTORY && count >= SELF_SIGNED_MAX_POOL_SIZE {
        return Err(StoreError::InvalidData(format!(
            "自签证书池最多保留 {SELF_SIGNED_MAX_POOL_SIZE} 张，请先删除一张待命或失败的证书"
        )));
    }
    // ACME groups retain their quota-aware two-spare limit.
    if directory != SELF_SIGNED_DIRECTORY && count >= 2 {
        return Err(StoreError::InvalidData(
            "这个组已经有两张备用了，再多也用不上，而每张都占掉每周五张的额度".to_owned(),
        ));
    }
    let id = generate_id()?;
    sqlx::query("INSERT INTO certificates (id, label_id, origin) VALUES ($1, $2, 'spare')")
        .bind(&id)
        .bind(label_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(inner) if inner.code().as_deref() == Some("23503") => {
                StoreError::InvalidData(format!("没有这个证书组：{label_id}"))
            }
            error => error.into(),
        })?;
    tx.commit().await?;
    Ok(id)
}

/// Makes a held certificate the one the group's machines present.
///
/// The swap is one statement per row inside one transaction, and the partial unique index is what
/// actually guarantees a group never has two serving certificates — a crash between the two writes
/// cannot leave that state behind.
pub async fn promote_certificate(
    pool: &PgPool,
    actor: &AdminContext,
    certificate_id: &str,
) -> Result<()> {
    require_system_admin(actor, "roll a certificate")?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT c.label_id, c.status, d.acme_directory
           FROM certificates c
           JOIN cert_labels l ON l.id = c.label_id
           JOIN cert_domains d ON d.id = l.domain_id
          WHERE c.id = $1
          FOR UPDATE OF c, l",
    )
    .bind(certificate_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData(format!("没有这张证书：{certificate_id}")))?;
    let label_id: String = row.try_get("label_id")?;
    let status: String = row.try_get("status")?;
    let directory: String = row.try_get("acme_directory")?;
    if status == "serving" {
        return Ok(());
    }
    if status != "ready" {
        return Err(StoreError::InvalidData(format!(
            "这张证书是 {status}，还不能启用——只有已签发待命的（ready）可以"
        )));
    }
    let previous_status = if directory == SELF_SIGNED_DIRECTORY {
        "ready"
    } else {
        "superseded"
    };
    sqlx::query(
        "UPDATE certificates SET status = $2
          WHERE label_id = $1 AND status = 'serving'",
    )
    .bind(&label_id)
    .bind(previous_status)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE certificates SET status = 'serving' WHERE id = $1")
        .bind(certificate_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes one certificate from the retained client trust set. The certificate currently served
/// by the group cannot be deleted: switch the group first, then remove the old row once every
/// client that still needs it has refreshed.
pub async fn delete_certificate(
    pool: &PgPool,
    actor: &AdminContext,
    certificate_id: &str,
) -> Result<()> {
    require_system_admin(actor, "delete a certificate")?;
    let mut tx = pool.begin().await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM certificates WHERE id = $1 FOR UPDATE")
            .bind(certificate_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::InvalidData(format!("没有这张证书：{certificate_id}")))?;
    if status == "serving" {
        return Err(StoreError::InvalidData(
            "正在由节点使用的证书不能删除；请先启用另一张证书".to_owned(),
        ));
    }
    sqlx::query("DELETE FROM certificates WHERE id = $1")
        .bind(certificate_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// What needs issuing or renewing now, one row per group.
///
/// `retry_after_minutes` keeps a failing group from being retried every scan: a wrong credential
/// fails identically however often it is tried, and each try spends a rate limit.
pub async fn certificates_due(
    pool: &PgPool,
    retry_after_minutes: i32,
) -> Result<Vec<CertificateOrder>> {
    // Groups holding nothing usable get a row created for them first, so that the query below has
    // something to return. Doing it here rather than in the scan keeps "what is due" a single
    // answer instead of two half-answers.
    //
    // The id comes from Rust rather than from SQL: the obvious `gen_random_bytes` needs pgcrypto,
    // which a stock PostgreSQL does not have, and the failure is at runtime on a database nobody
    // was looking at.
    let starved: Vec<String> = sqlx::query(
        "SELECT l.id FROM cert_labels l
          WHERE l.status = 'active'
            AND NOT EXISTS (SELECT 1 FROM certificates c
                             WHERE c.label_id = l.id
                               AND c.status IN ('pending', 'ready', 'serving'))",
    )
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| row.try_get("id"))
    .collect::<std::result::Result<_, _>>()?;
    for label_id in starved {
        sqlx::query("INSERT INTO certificates (id, label_id) VALUES ($1, $2)")
            .bind(generate_id()?)
            .bind(&label_id)
            .execute(pool)
            .await?;
    }

    // A renewal is a new certificate, not new bytes written over the serving row. Keeping both
    // rows is what lets clients trust the old and new leaves at the same time while machines pick
    // up the replacement on their own schedules. Failed renewal rows retry in place; creating a
    // fresh row for every scan would spend the CA's quota while evading the retry backoff.
    let renewals: Vec<String> = sqlx::query(
        "SELECT l.id
           FROM cert_labels l
           JOIN cert_domains d ON d.id = l.domain_id
           JOIN certificates serving
             ON serving.label_id = l.id AND serving.status = 'serving'
          WHERE l.status = 'active'
            AND (d.acme_directory = 'self-signed' OR d.dns_credential_sealed IS NOT NULL)
            AND (serving.expires_at < now() + make_interval(days => d.renew_before_days)
                 OR serving.acme_directory IS DISTINCT FROM d.acme_directory)
            -- A self-signed pool has a hard ten-row ceiling. At that size there is already at
            -- least one reusable standby leaf; the operator can delete one before asking the
            -- scanner to create another. Public-CA rollover keeps its separate quota rules.
            AND (d.acme_directory <> 'self-signed' OR
                 (SELECT count(*) FROM certificates held WHERE held.label_id = l.id) < $1)
            AND NOT EXISTS (
                SELECT 1 FROM certificates pending
                 WHERE pending.label_id = l.id
                   AND pending.origin = 'renewal'
                   AND pending.status IN ('pending', 'ready', 'failed')
            )",
    )
    .bind(SELF_SIGNED_MAX_POOL_SIZE)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| row.try_get("id"))
    .collect::<std::result::Result<_, _>>()?;
    for label_id in renewals {
        sqlx::query("INSERT INTO certificates (id, label_id, origin) VALUES ($1, $2, 'renewal')")
            .bind(generate_id()?)
            .bind(&label_id)
            .execute(pool)
            .await?;
    }

    let rows = sqlx::query(
        "SELECT c.id AS certificate_id, c.label_id, l.domain_id, l.label, l.certificate_name,
                d.domain, d.acme_directory, d.acme_contact,
                EXISTS (SELECT 1 FROM certificates s
                         WHERE s.label_id = c.label_id AND s.status = 'serving') AS renewal
           FROM certificates c
           JOIN cert_labels l ON l.id = c.label_id
           JOIN cert_domains d ON d.id = l.domain_id
          -- Public issuance needs a DNS credential to prove the name. Direct self-signing does not.
          WHERE (d.acme_directory = 'self-signed' OR d.dns_credential_sealed IS NOT NULL)
            AND l.status = 'active'
            -- Fresh, manually requested and renewal rows all use one issuance path. A serving row
            -- is never selected here: the block above creates a separate renewal row for it.
            AND c.status IN ('pending', 'failed')
            AND (c.last_attempt_at IS NULL
                 OR c.last_attempt_at < now() - make_interval(mins => $1))
          ORDER BY c.expires_at NULLS FIRST",
    )
    .bind(retry_after_minutes)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(CertificateOrder {
                certificate_id: row.try_get("certificate_id")?,
                label_id: row.try_get("label_id")?,
                domain_id: row.try_get("domain_id")?,
                domain: row.try_get("domain")?,
                label: row.try_get("label")?,
                certificate_name: row.try_get("certificate_name")?,
                acme_directory: row.try_get("acme_directory")?,
                acme_contact: row.try_get("acme_contact")?,
                renewal: row.try_get("renewal")?,
            })
        })
        .collect()
}

/// Stores a freshly issued certificate. `not_after` is RFC3339 as the CA stated it, not a value
/// computed here — the expiry that matters is the one in the certificate. Returns `false` without
/// storing or promoting when the domain changed signing method while this order was in flight; the
/// row is made immediately due again so the next scan uses the new setting.
///
/// What happens next is decided by the row's `origin`, in one transaction with the write:
///
/// - a **renewal** takes over immediately, superseding whatever the group was serving. Waiting for
///   somebody to activate it is how a certificate expires with its replacement already in the
///   database, and the failure lands on every machine in the group at once.
/// - a **spare** waits in `ready`. Choosing the moment is the entire reason an operator asked for
///   one early.
///
/// Either way a group that is serving nothing starts serving this one: there is nothing to keep
/// and nothing to choose between.
///
/// The takeover does not interrupt anything. Both certificates are valid, both carry the same
/// names, and the machines pick up the new bytes on their own schedule — the agent within ten
/// minutes, xray within an hour of that. Nothing is republished, because the SNI has not changed.
pub async fn record_certificate(pool: &PgPool, issued: IssuedCertificate<'_>) -> Result<bool> {
    let mut tx = pool.begin().await?;

    // Lock both the order and its domain so this comparison and the promotion have one
    // linearization point with `upsert_cert_domain`. An ACME order can take long enough for an
    // operator to change CA (or switch to self-signing) while it is in flight; the certificate
    // proves which directory actually issued it, not whichever value happens to be current now.
    let row = sqlx::query(
        "SELECT c.label_id, c.origin, d.acme_directory AS current_directory
           FROM certificates c
           JOIN cert_labels l ON l.id = c.label_id
           JOIN cert_domains d ON d.id = l.domain_id
          WHERE c.id = $1
          FOR UPDATE OF c, d",
    )
    .bind(issued.certificate_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData(format!("没有这张证书：{}", issued.certificate_id)))?;
    let label_id: String = row.try_get("label_id")?;
    let origin: String = row.try_get("origin")?;
    let current_directory: String = row.try_get("current_directory")?;
    if current_directory != issued.acme_directory {
        sqlx::query("UPDATE certificates SET last_attempt_at = NULL WHERE id = $1")
            .bind(issued.certificate_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(false);
    }

    let sealed = secrets::seal(CTX_CERT_KEY, issued.key_pem)?;

    let serving: Option<String> = sqlx::query(
        "SELECT id FROM certificates
          WHERE label_id = $1 AND status = 'serving' AND id <> $2
          FOR UPDATE",
    )
    .bind(&label_id)
    .bind(issued.certificate_id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|row| row.try_get("id"))
    .transpose()?;

    let takes_over = serving.is_none() || origin == "renewal";
    if takes_over {
        if let Some(previous) = &serving {
            let previous_status = if current_directory == SELF_SIGNED_DIRECTORY {
                "ready"
            } else {
                "superseded"
            };
            sqlx::query("UPDATE certificates SET status = $2 WHERE id = $1")
                .bind(previous)
                .bind(previous_status)
                .execute(&mut *tx)
                .await?;
        }
    }

    sqlx::query(
        "UPDATE certificates
            SET cert_pem = $2, key_pem_sealed = $3, expires_at = $4::timestamptz, issuer = $5,
                peer_sha256 = $6,
                acme_directory = $7,
                issued_at = now(), attempts = 0, last_error = NULL, last_attempt_at = now(),
                status = $8
          WHERE id = $1",
    )
    .bind(issued.certificate_id)
    .bind(issued.cert_pem)
    .bind(sealed)
    .bind(issued.not_after)
    .bind(issued.issuer)
    .bind(issued.peer_sha256)
    .bind(issued.acme_directory)
    .bind(if takes_over { "serving" } else { "ready" })
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

/// Records what a node says it is holding.
///
/// Written on every runtime report, which is frequent, so it is one upsert with no read first.
pub async fn record_observation(
    pool: &PgPool,
    node_id: &str,
    state: &str,
    sha256: Option<&str>,
) -> Result<()> {
    record_observation_at(pool, node_id, state, sha256, None).await
}

pub async fn record_observation_at(
    pool: &PgPool,
    node_id: &str,
    state: &str,
    sha256: Option<&str>,
    observed_at_unix_secs: Option<i64>,
) -> Result<()> {
    if observed_at_unix_secs.is_some_and(|at| at <= 0) {
        return Err(StoreError::InvalidData(
            "certificate observation timestamp must be positive unix seconds".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO node_cert_state (node_id, observed_state, observed_sha256, observed_at)
         VALUES ($1, $2, $3, COALESCE(to_timestamp($4), now()))
         ON CONFLICT (node_id) DO UPDATE
            SET observed_state = EXCLUDED.observed_state,
                observed_sha256 = EXCLUDED.observed_sha256,
                observed_at = EXCLUDED.observed_at
          WHERE node_cert_state.observed_at <= EXCLUDED.observed_at",
    )
    .bind(node_id)
    .bind(state)
    .bind(sha256)
    .bind(observed_at_unix_secs.map(|at| at as f64))
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks an attempt as started, before anything is asked of the CA.
///
/// The backoff in `certificates_due` reads `last_attempt_at`, and until this existed that column
/// was only written once an attempt *finished*. A process that died partway through — or was
/// restarted during one — therefore looked like it had never tried, and the next scan started
/// over immediately. That is a rate limit waiting to be spent: Let's Encrypt allows five
/// certificates a week for one set of names, and five restarts in a crash loop is not a lot.
pub async fn record_certificate_attempt(pool: &PgPool, certificate_id: &str) -> Result<()> {
    sqlx::query("UPDATE certificates SET last_attempt_at = now() WHERE id = $1")
        .bind(certificate_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Records a failed attempt without touching whatever the group is already serving.
///
/// Failing is not the same as having nothing: a group whose renewal fails still serves the
/// certificate it has, for as many days as are left. This row is the one that failed, and it is
/// the only one touched.
pub async fn record_certificate_failure(
    pool: &PgPool,
    certificate_id: &str,
    error: &str,
) -> Result<()> {
    // Truncated because this is written from an error path and ends up on a page. An ACME server
    // can return a very long problem document, and the first line is the part anybody reads.
    let message: String = error.chars().take(500).collect();
    sqlx::query(
        "UPDATE certificates
            SET status = CASE WHEN status = 'serving' THEN 'serving' ELSE 'failed' END,
                attempts = attempts + 1, last_error = $2, last_attempt_at = now()
          WHERE id = $1",
    )
    .bind(certificate_id)
    .bind(message)
    .execute(pool)
    .await?;
    Ok(())
}

/// The certificate a node should be holding: its group's serving one, names included.
pub async fn node_key(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<(Vec<String>, String, String)>> {
    let Some(row) = sqlx::query(
        "SELECT l.label, l.certificate_name, d.domain, c.cert_pem, c.key_pem_sealed
           FROM node_cert_label m
           JOIN cert_labels l ON l.id = m.label_id
           JOIN cert_domains d ON d.id = l.domain_id
           JOIN certificates c ON c.label_id = l.id AND c.status = 'serving'
          WHERE m.node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let label: String = row.try_get("label")?;
    let domain: String = row.try_get("domain")?;
    let certificate_name: Option<String> = row.try_get("certificate_name")?;
    let cert_pem: Option<String> = row.try_get("cert_pem")?;
    let sealed: Option<String> = row.try_get("key_pem_sealed")?;
    // A serving row always has both by CHECK constraint. Treated as "nothing to hand out" rather
    // than an error so that one malformed row cannot stop a deployment.
    let (Some(cert_pem), Some(sealed)) = (cert_pem, sealed) else {
        return Ok(None);
    };
    Ok(Some((
        names_of(&label, &domain, certificate_name.as_deref()),
        cert_pem,
        secrets::open(CTX_CERT_KEY, &sealed)?,
    )))
}

/// What the desired response owes this node on the certificate dimension.
///
/// The control plane holds the serving certificate and the node reports the sha256 of the
/// certificate file it actually holds, so the comparison needs no round trip: a matching sha
/// means the node already has the serving certificate and nothing is owed. Everything else —
/// absent, stale, or never reported — is owed the material, which is how a renewal reaches a
/// converged machine: the serving row changes, the reported sha stops matching, and the next
/// desired poll carries the new certificate.
///
/// `None` also when the node has no serving certificate: there is nothing to expect, and the
/// node is converged on this dimension. Errors are for the caller to degrade, not to propagate
/// — this runs on the desired poll path, where one malformed row must not block a deployment.
pub async fn cert_delta_for_node(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<NodeCertificateMaterial>> {
    let Some((names, cert_pem, key_pem)) = node_key(pool, node_id).await? else {
        return Ok(None);
    };
    // Two kinds of absence are equivalent here: the node has never reported (no row), or it
    // explicitly reported that the certificate is absent (a row whose observed_sha256 is NULL).
    // Decode the nullable column first and then flatten the optional row; asking sqlx for String
    // makes the second, normal state fail as an unexpected NULL on every agent poll.
    let observed: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT observed_sha256 FROM node_cert_state WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    if observed.as_deref() == Some(sha256_hex(cert_pem.as_bytes()).as_str()) {
        return Ok(None);
    }
    Ok(Some(NodeCertificateMaterial {
        names,
        cert_pem,
        key_pem,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_asks_for_the_wildcard_and_the_bare_name() {
        let order = CertificateOrder {
            certificate_id: "c1".to_owned(),
            label_id: "l1".to_owned(),
            domain_id: "example.net".to_owned(),
            domain: "example.net".to_owned(),
            label: "a1b2c3d4".to_owned(),
            certificate_name: None,
            acme_directory: ACME_LETSENCRYPT_STAGING.to_owned(),
            acme_contact: None,
            renewal: false,
        };
        // Both, because a wildcard covers one label and does not cover itself. Asking for only the
        // wildcard leaves the bare name unusable; asking for only the bare name leaves every name
        // under it unusable.
        assert_eq!(
            order.names(),
            vec!["*.a1b2c3d4.example.net", "a1b2c3d4.example.net"]
        );
    }

    #[test]
    fn a_self_signed_order_uses_one_exact_name_without_a_wildcard() {
        let order = CertificateOrder {
            certificate_id: "c1".to_owned(),
            label_id: "l1".to_owned(),
            domain_id: "internal.test".to_owned(),
            domain: "internal.test".to_owned(),
            label: "a1b2c3d4".to_owned(),
            certificate_name: Some("northstar-edge-0123abcd.test".to_owned()),
            acme_directory: SELF_SIGNED_DIRECTORY.to_owned(),
            acme_contact: None,
            renewal: false,
        };
        assert_eq!(order.names(), vec!["northstar-edge-0123abcd.test"]);
    }

    #[test]
    fn generated_self_signed_names_are_reserved_and_random() {
        let first = generate_reserved_certificate_name().unwrap();
        assert!(first.ends_with(".test"));
        assert_eq!(first.matches('.').count(), 1);
        assert_ne!(first, generate_reserved_certificate_name().unwrap());
    }

    #[test]
    fn a_label_is_lowercase_hex_and_not_the_same_twice() {
        let first = generate_label().unwrap();
        assert_eq!(first.len(), LABEL_BYTES * 2);
        assert!(first
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_ne!(first, generate_label().unwrap());
    }

    #[test]
    fn a_domain_is_normalized_before_it_is_stored() {
        // A trailing dot is a valid FQDN and a different string; upper case is a different string
        // again. Both would create a second row for the same domain.
        assert_eq!(normalize_domain("  Example.NET.  "), "example.net");
    }
}
