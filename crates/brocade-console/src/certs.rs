//! The issuing worker: what runs on a timer and turns "this node needs a certificate" into one.
//!
//! # Why it is serial
//!
//! Orders are issued one at a time, deliberately. Certificate authorities rate-limit per account
//! and per domain, and the limits are per week — a fleet that fans out and hits one spends the
//! rest of the week unable to issue anything, including the renewal that was actually urgent.
//! The database lock makes that serialization fleet-wide, not merely per process.
//!
//! Issuance takes about half a minute, so serial costs a fleet of thirty machines fifteen minutes
//! once every sixty days. That is not a number worth trading a rate limit for.
//!
//! # Why failures are recorded rather than raised
//!
//! Nothing here has a caller waiting on it. A failure that only logs is a certificate that expires
//! while the console shows nothing wrong, so every outcome lands in `node_certificates` — status,
//! attempt count and the CA's own words — and the console reads it from there.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use brocade_store::{
    CertificateOrder, CertificateScanCounts, CertificateScanPhase, CertificateScanTrigger,
    CertificateSigningMethod, ClaimedCertificateScan, IssuedCertificate, PgStore,
    SELF_SIGNED_DIRECTORY,
};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::acme::{self, AccountKey};
use crate::dns::Cloudflare;

/// How often the fleet is scanned. Renewal normally happens 60 days before expiry, so the scan being
/// hourly rather than by the minute costs nothing; what it buys is that a control plane restarting
/// in a loop cannot become a source of ACME traffic.
const SCAN_INTERVAL: Duration = Duration::from_secs(3600);
const SCHEDULER_TICK: Duration = Duration::from_secs(1);
const LEASE_REFRESH: Duration = Duration::from_secs(5);

/// How long a failed node waits before being tried again.
///
/// A wrong credential fails in milliseconds. Without a floor, an hourly scan would still be
/// polite, but the manual "retry now" button and a restarting control plane would not — and the
/// thing being protected is a weekly quota.
const RETRY_AFTER_MINUTES: i32 = 30;
const SELF_SIGNED_DAYS: i64 = 36_525;

struct SelfSignedCertificate {
    cert_pem: String,
    key_pem: String,
    not_after: String,
    issuer: String,
    peer_sha256: String,
}

enum IssueOutcome {
    Stored(f64),
    Obsolete,
}

fn generate_self_signed(names: Vec<String>) -> Result<SelfSignedCertificate, String> {
    let now = OffsetDateTime::now_utc();
    let expires = now + TimeDuration::days(SELF_SIGNED_DAYS);
    let key = KeyPair::generate().map_err(|error| format!("生成自签证书密钥失败：{error}"))?;
    let mut params =
        CertificateParams::new(names).map_err(|error| format!("自签证书名称不合法：{error}"))?;
    params.not_before = now - TimeDuration::minutes(5);
    params.not_after = expires;
    params.distinguished_name = DistinguishedName::new();
    let mut identity = [0u8; 6];
    getrandom::fill(&mut identity).map_err(|error| format!("生成自签证书身份失败：{error}"))?;
    const BRANDS: &[&str] = &[
        "Arcadia",
        "Cedar",
        "Harbor",
        "Lumen",
        "Northstar",
        "Silverline",
        "Velora",
    ];
    const UNITS: &[&str] = &["Edge", "Network", "Relay", "Secure", "Systems"];
    let brand = BRANDS[usize::from(identity[0]) % BRANDS.len()];
    let unit = UNITS[usize::from(identity[1]) % UNITS.len()];
    let serial = identity[2..]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    params.distinguished_name.push(
        DnType::OrganizationName,
        format!("{brand} Network Services"),
    );
    params.distinguished_name.push(
        DnType::OrganizationalUnitName,
        "Private Certificate Authority",
    );
    params.distinguished_name.push(
        DnType::CommonName,
        format!("{brand} {unit} Root CA {serial}"),
    );
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params
        .self_signed(&key)
        .map_err(|error| format!("生成自签证书失败：{error}"))?;
    let cert_pem = certificate.pem();
    let (not_after, issuer) =
        acme::leaf_facts(&cert_pem).map_err(|error| format!("读不回自签证书：{error}"))?;
    Ok(SelfSignedCertificate {
        cert_pem,
        key_pem: key.serialize_pem(),
        not_after,
        issuer,
        peer_sha256: format!("{:x}", Sha256::digest(certificate.der())),
    })
}

async fn issue_self_signed(
    store: &PgStore,
    order: &CertificateOrder,
    claim: &ClaimedCertificateScan,
    counts: CertificateScanCounts,
    subject: &str,
) -> Result<IssueOutcome, String> {
    let started = std::time::Instant::now();
    let certificate = generate_self_signed(order.names())?;
    report_progress(
        store,
        claim,
        CertificateScanPhase::Storing,
        counts,
        Some(&order.certificate_id),
        Some(subject),
    )
    .await;
    let stored = store
        .record_certificate(IssuedCertificate {
            certificate_id: &order.certificate_id,
            acme_directory: &order.acme_directory,
            cert_pem: &certificate.cert_pem,
            key_pem: &certificate.key_pem,
            not_after: &certificate.not_after,
            issuer: &certificate.issuer,
            peer_sha256: &certificate.peer_sha256,
        })
        .await
        .map_err(|error| format!("签发成功但存不下来：{error}"))?;
    if stored {
        Ok(IssueOutcome::Stored(started.elapsed().as_secs_f64()))
    } else {
        Ok(IssueOutcome::Obsolete)
    }
}

#[derive(Debug)]
struct IssuanceResult {
    issued: usize,
    failed: usize,
}

async fn report_progress(
    store: &PgStore,
    claim: &ClaimedCertificateScan,
    phase: CertificateScanPhase,
    counts: CertificateScanCounts,
    certificate_id: Option<&str>,
    subject: Option<&str>,
) {
    match store
        .update_certificate_scan_progress(claim, phase, counts, certificate_id, subject)
        .await
    {
        Ok(true) => {}
        Ok(false) => eprintln!("证书任务 {}：进度写入被 fencing 拒绝", claim.run.id),
        Err(error) => eprintln!("证书任务 {}：进度写不下来：{error}", claim.run.id),
    }
}

/// Runs one durable pass over everything due. The retry floor applies to automatic and manual
/// runs alike; saving corrected settings explicitly clears unfinished rows' backoff.
async fn process_pending(
    store: &PgStore,
    claim: &ClaimedCertificateScan,
) -> Result<IssuanceResult, String> {
    // Domains are loaded before the due list so cleanup also runs when nothing needs issuance.
    // A missing public-CA credential remains in the due list once: `issue` records an actionable
    // failure on its certificate row and the retry floor prevents a tight loop.
    let domains = store
        .cert_domains()
        .await
        .map_err(|error| error.to_string())?;
    // Before the "nothing to issue" exit below, deliberately. Challenge records can survive a
    // process interruption, and older consoles also kept unnecessary A records for certificate
    // SNI names. A full scan removes only records carrying Brocade's exact ownership comments.
    cleanup_dns(store, &domains).await;

    let due = store
        .certificates_due(RETRY_AFTER_MINUTES)
        .await
        .map_err(|error| error.to_string())?;
    let total = due.len();
    let mut issued = 0;
    let mut failed = 0;
    let mut processed = 0;
    report_progress(
        store,
        claim,
        CertificateScanPhase::Preparing,
        CertificateScanCounts {
            total,
            processed,
            issued,
            failed,
        },
        None,
        None,
    )
    .await;
    for order in due {
        // Named by the group, not by a machine: one order covers every machine drawing from it,
        // and saying "hk-01 已签发" when five machines share the certificate would be wrong four
        // times over.
        let what = format!("{}.{}", order.label, order.domain);
        let cert_id = order.certificate_id.clone();
        let counts = CertificateScanCounts {
            total,
            processed,
            issued,
            failed,
        };
        report_progress(
            store,
            claim,
            CertificateScanPhase::Preparing,
            counts,
            Some(&cert_id),
            Some(&what),
        )
        .await;
        // Stamped before the CA is asked anything, so that a process which dies partway through
        // one — or is restarted during one — still looks like it tried. Written afterwards only,
        // the backoff never applied to the case that needs it most.
        if let Err(error) = store.record_certificate_attempt(&cert_id).await {
            eprintln!("证书：{what} 记不下这次尝试，跳过这一轮：{error}");
            failed += 1;
            processed += 1;
            report_progress(
                store,
                claim,
                CertificateScanPhase::Preparing,
                CertificateScanCounts {
                    total,
                    processed,
                    issued,
                    failed,
                },
                None,
                None,
            )
            .await;
            continue;
        }
        match issue(store, &order, claim, counts, &what).await {
            Ok(IssueOutcome::Stored(seconds)) => {
                issued += 1;
                eprintln!("证书：{what} 已签发，用时 {seconds:.0}s");
            }
            Ok(IssueOutcome::Obsolete) => {
                failed += 1;
                eprintln!("证书：{what} 签发期间设置已变化，丢弃结果并按新设置重新排队");
            }
            Err(error) => {
                failed += 1;
                // Recorded before logging: the log line is for whoever is watching now, the row is
                // for whoever looks later, and the second one is the one that matters.
                if let Err(write) = store.record_certificate_failure(&cert_id, &error).await {
                    eprintln!("证书：{what} 失败，而且记不下来：{write}");
                }
                eprintln!("证书：{what} 签发失败：{error}");
            }
        }
        processed += 1;
        report_progress(
            store,
            claim,
            CertificateScanPhase::Preparing,
            CertificateScanCounts {
                total,
                processed,
                issued,
                failed,
            },
            None,
            None,
        )
        .await;
    }
    Ok(IssuanceResult { issued, failed })
}

/// Removes DNS state owned by Brocade but no longer needed. ACME TXT records are temporary proof;
/// certificate names are carried as SNI while clients dial the configured IP directly, so the A
/// records maintained by older consoles have no consumer either.
async fn cleanup_dns(store: &PgStore, domains: &[brocade_store::CertDomain]) {
    for domain in domains.iter().filter(|domain| {
        domain.signing_method == CertificateSigningMethod::PublicCa && domain.has_credential
    }) {
        let credential = match store.cert_domain_secrets(&domain.id).await {
            Ok((Some(credential), _, _)) => credential,
            Ok(_) => continue,
            Err(error) => {
                eprintln!("证书：读不出 {} 的凭据：{error}", domain.domain);
                continue;
            }
        };
        let dns = match Cloudflare::connect(&credential, &domain.domain).await {
            Ok(dns) => dns,
            Err(error) => {
                eprintln!("证书：{} 的 DNS 连不上：{error}", domain.domain);
                continue;
            }
        };
        match dns.cleanup_managed_records().await {
            Ok(cleanup) => {
                if cleanup.legacy_address_records > 0 || cleanup.challenge_records > 0 {
                    eprintln!(
                        "证书：{} 清掉长期 A 记录 {} 条、遗留挑战 TXT {} 条",
                        domain.domain, cleanup.legacy_address_records, cleanup.challenge_records
                    );
                }
                for failure in cleanup.failures {
                    eprintln!("证书：DNS 遗留记录清理失败：{failure}");
                }
            }
            Err(error) => eprintln!("证书：{} 的 DNS 遗留记录查不了：{error}", domain.domain),
        }
    }
}

/// One order, start to finish. The error is a sentence meant for the console, not a type — every
/// caller does the same thing with it, which is show it to a person.
async fn issue(
    store: &PgStore,
    order: &CertificateOrder,
    claim: &ClaimedCertificateScan,
    counts: CertificateScanCounts,
    subject: &str,
) -> Result<IssueOutcome, String> {
    if order.acme_directory == SELF_SIGNED_DIRECTORY {
        return issue_self_signed(store, order, claim, counts, subject).await;
    }
    let started = std::time::Instant::now();

    let (credential, account_key, account_url) = store
        .cert_domain_secrets(&order.domain_id)
        .await
        .map_err(|error| format!("读不出这个域的凭据：{error}"))?;
    let credential = credential.ok_or_else(|| {
        "这个域还没有配 DNS 凭据，签不了——先在设置里填上 Cloudflare 的 API token".to_owned()
    })?;

    let dns = Cloudflare::connect(&credential, &order.domain)
        .await
        .map_err(|error| error.to_string())?;

    // Reuse the account where there is one. A new account per order is its own rate limit, and it
    // also scatters the fleet's issuance history across accounts nobody can look up afterwards.
    let key = match &account_key {
        Some(stored) => AccountKey::from_pkcs8_b64(stored).map_err(|error| error.to_string())?,
        None => AccountKey::generate().map_err(|error| error.to_string())?,
    };
    let stored_key = key.as_stored().to_owned();
    let mut session = acme::Session::start(&order.acme_directory, key)
        .await
        .map_err(|error| error.to_string())?;

    match (&account_key, &account_url) {
        (Some(_), Some(url)) => session.adopt_account(url),
        _ => {
            let url = session
                .register(order.acme_contact.as_deref())
                .await
                .map_err(|error| error.to_string())?;
            store
                .save_acme_account(&order.domain_id, &stored_key, &url)
                .await
                .map_err(|error| format!("账号注册了但存不下来：{error}"))?;
        }
    }

    let names = order.names();
    let (acme_order, challenges) = session
        .begin_order(&names)
        .await
        .map_err(|error| error.to_string())?;

    // Nothing to prove — the CA still had valid authorizations for these names. Straight to
    // finalize; publishing records nobody will look at can only fail.
    let mut published = Vec::new();
    if !challenges.is_empty() {
        report_progress(
            store,
            claim,
            CertificateScanPhase::Dns,
            counts,
            Some(&order.certificate_id),
            Some(subject),
        )
        .await;
        let name = challenges[0].name.clone();
        let mut values = Vec::new();
        for challenge in &challenges {
            match dns.publish(&challenge.name, &challenge.value).await {
                Ok(record) => {
                    published.push(record);
                    values.push(challenge.value.clone());
                }
                Err(error) => {
                    retract(&dns, &published).await;
                    return Err(format!("写挑战记录失败：{error}"));
                }
            }
        }

        // Not a gate. Confirmation is worth waiting for — asking the CA to look too early spends
        // one of the challenge's attempts — but the resolver being behind says nothing about what
        // the CA can see, so an unconfirmed wait goes ahead anyway and lets the CA answer.
        match dns.await_propagation(&name, &values).await {
            Some(waited) => eprintln!("证书：{name} 已生效，等了 {:.0}s", waited.as_secs_f64()),
            None => eprintln!(
                "证书：{name} 在解析器上还没看到，仍然继续——CA 查的是权威服务器，不是这一层"
            ),
        }

        report_progress(
            store,
            claim,
            CertificateScanPhase::Validating,
            counts,
            Some(&order.certificate_id),
            Some(subject),
        )
        .await;
        for challenge in &challenges {
            if let Err(error) = session.validate(challenge).await {
                retract(&dns, &published).await;
                return Err(error.to_string());
            }
        }
    }

    report_progress(
        store,
        claim,
        CertificateScanPhase::Finalizing,
        counts,
        Some(&order.certificate_id),
        Some(subject),
    )
    .await;
    let result = session.finalize(&acme_order, &names).await;
    // Before the result is inspected: the records have done their job either way, and leaving them
    // behind on the failure path is exactly how the leftovers this code sweeps came to exist.
    retract(&dns, &published).await;
    let issued = result.map_err(|error| error.to_string())?;

    report_progress(
        store,
        claim,
        CertificateScanPhase::Storing,
        counts,
        Some(&order.certificate_id),
        Some(subject),
    )
    .await;
    let stored = store
        .record_certificate(IssuedCertificate {
            certificate_id: &order.certificate_id,
            acme_directory: &order.acme_directory,
            cert_pem: &issued.chain_pem,
            key_pem: &issued.key_pem,
            not_after: &issued.not_after,
            issuer: &issued.issuer,
            peer_sha256: &issued.peer_sha256,
        })
        .await
        .map_err(|error| format!("签发成功但存不下来：{error}"))?;

    if stored {
        Ok(IssueOutcome::Stored(started.elapsed().as_secs_f64()))
    } else {
        Ok(IssueOutcome::Obsolete)
    }
}

/// Best-effort cleanup. A failure here is logged and not propagated: the order's own outcome is
/// what the caller is reporting, and "the certificate was issued but a TXT record could not be
/// deleted" must not read as a failed issuance.
async fn retract(dns: &Cloudflare, records: &[crate::dns::PublishedRecord]) {
    let mut deleted = HashSet::new();
    for record in records
        .iter()
        .filter(|record| record.delete_after_use && deleted.insert(record.id.as_str()))
    {
        if let Err(error) = dns.delete(&record.id).await {
            eprintln!(
                "证书：{} 的挑战记录没删掉（{error}）——后台扫描会按 Brocade 标记重试清理",
                record.name
            );
        }
    }
}

async fn execute_claim(store: &PgStore, claim: &ClaimedCertificateScan) {
    let work = process_pending(store, claim);
    tokio::pin!(work);
    let mut refresh =
        tokio::time::interval_at(tokio::time::Instant::now() + LEASE_REFRESH, LEASE_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            result = &mut work => break result,
            _ = refresh.tick() => {
                match store.renew_certificate_scan_lease(claim).await {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!("证书任务 {}：lease 已被其他执行器接管，停止本轮", claim.run.id);
                        return;
                    }
                    Err(error) => {
                        // Continuing after an uncertain heartbeat can overlap the executor that
                        // reclaims this row. Dropping the issuing future is the safe side; the row
                        // remains recoverable after its existing lease expires.
                        eprintln!("证书任务 {}：lease 续租失败，停止并等待恢复：{error}", claim.run.id);
                        return;
                    }
                }
            }
        }
    };

    let error = outcome.as_ref().err().map(String::as_str);
    match store.complete_certificate_scan(claim, error).await {
        Ok(true) => {}
        Ok(false) => eprintln!("证书任务 {}：完成写入被 fencing 拒绝", claim.run.id),
        Err(write) => eprintln!("证书任务 {}：完成状态写不下来：{write}", claim.run.id),
    }
    match outcome {
        Ok(result) if result.issued > 0 || result.failed > 0 => eprintln!(
            "证书：任务 {} 完成，签发 {} 张，失败 {} 张",
            claim.run.id, result.issued, result.failed
        ),
        Ok(_) => {}
        Err(error) => eprintln!("证书：任务 {} 失败：{error}", claim.run.id),
    }
}

async fn run_next(store: &PgStore, owner: &str) {
    // Keep the existing transaction-scoped fleet lock around the whole external workflow. The
    // durable row makes work recoverable and visible; this lock still excludes mutations that
    // require a stable certificate set while the worker is talking to the CA.
    let lock = match store.try_certificate_scan_lock().await {
        Ok(Some(lock)) => lock,
        Ok(None) => return,
        Err(error) => {
            eprintln!("证书任务：全局锁取不到：{error}");
            return;
        }
    };
    let claim = match store.claim_certificate_scan(owner).await {
        Ok(Some(claim)) => claim,
        Ok(None) => return,
        Err(error) => {
            eprintln!("证书任务：领取失败：{error}");
            return;
        }
    };
    execute_claim(store, &claim).await;
    drop(lock);
}

/// Starts the durable scan scheduler. HTTP handlers only enqueue and wake it; no request owns an
/// ACME operation, so closing a tab or a proxy timeout cannot cancel issuance.
pub fn spawn(store: PgStore) -> Arc<tokio::sync::Notify> {
    let wake = Arc::new(tokio::sync::Notify::new());
    let signal = wake.clone();
    tokio::spawn(async move {
        if let Err(error) = store
            .enqueue_certificate_scan(CertificateScanTrigger::Startup)
            .await
        {
            eprintln!("证书任务：启动扫描入队失败：{error}");
        }
        let owner = format!("console-{}", std::process::id());
        let mut poll = tokio::time::interval(SCHEDULER_TICK);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut hourly =
            tokio::time::interval_at(tokio::time::Instant::now() + SCAN_INTERVAL, SCAN_INTERVAL);
        hourly.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prune_ticks = 0_u32;
        loop {
            tokio::select! {
                _ = poll.tick() => {}
                _ = signal.notified() => {}
                _ = hourly.tick() => {
                    if let Err(error) = store
                        .enqueue_certificate_scan(CertificateScanTrigger::Scheduled)
                        .await
                    {
                        eprintln!("证书任务：定时扫描入队失败：{error}");
                    }
                    prune_ticks = prune_ticks.wrapping_add(1);
                    if prune_ticks.is_multiple_of(24) {
                        if let Err(error) = store.prune_certificate_scans().await {
                            eprintln!("证书任务：历史清理失败：{error}");
                        }
                    }
                }
            }
            run_next(&store, &owner).await;
        }
    });
    wake
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directly_self_signed_certificate_carries_an_arbitrary_private_sni_and_pin() {
        let certificate =
            generate_self_signed(vec!["northstar-edge-0123abcd.com".to_owned()]).unwrap();
        assert_eq!(certificate.cert_pem.matches("BEGIN CERTIFICATE").count(), 1);
        assert!(KeyPair::from_pem(&certificate.key_pem).is_ok());
        let (_, issuer) = crate::acme::leaf_facts(&certificate.cert_pem).unwrap();
        assert!(issuer.contains("Root CA"));
        let expiry_year: i32 = certificate.not_after[..4].parse().unwrap();
        assert!(expiry_year >= OffsetDateTime::now_utc().year() + 99);
        assert_eq!(
            crate::acme::leaf_sha256(&certificate.cert_pem).unwrap(),
            certificate.peer_sha256
        );
        assert_eq!(certificate.peer_sha256.len(), 64);
    }
}
