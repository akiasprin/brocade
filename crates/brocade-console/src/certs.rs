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

use std::sync::Arc;
use std::time::Duration;

use brocade_store::{CertificateOrder, PgStore};

use crate::acme::{self, AccountKey};
use crate::dns::Cloudflare;

/// How often the fleet is scanned. Renewal happens 30 days before expiry, so the scan being
/// hourly rather than by the minute costs nothing; what it buys is that a control plane restarting
/// in a loop cannot become a source of ACME traffic.
const SCAN_INTERVAL: Duration = Duration::from_secs(3600);

/// How long a failed node waits before being tried again.
///
/// A wrong credential fails in milliseconds. Without a floor, an hourly scan would still be
/// polite, but the manual "retry now" button and a restarting control plane would not — and the
/// thing being protected is a weekly quota.
const RETRY_AFTER_MINUTES: i32 = 30;

/// Runs one pass over everything that is due. Returns how many succeeded and how many failed.
pub async fn scan_once(store: &PgStore) -> (usize, usize) {
    // Serializing inside one process is insufficient when two console replicas share the same
    // account and DNS zone. Hold a PostgreSQL advisory transaction lock for the complete pass;
    // another replica skips this round and the next scheduled pass will pick up anything due.
    let _scan_lock = match store.try_certificate_scan_lock().await {
        Ok(Some(lock)) => lock,
        Ok(None) => return (0, 0),
        Err(error) => {
            eprintln!("证书：拿不到全局扫描锁：{error}");
            return (0, 0);
        }
    };

    // A domain with no credential cannot issue, and rows created against it would show up in the
    // console as pending forever with no explanation. Skipped entirely instead — unless it signs
    // its own, which needs no credential and would otherwise be the one setting that silently
    // never produces a certificate.
    let domains = match store.cert_domains().await {
        Ok(domains) => domains,
        Err(error) => {
            eprintln!("证书：读不出域名配置：{error}");
            return (0, 0);
        }
    };
    // Before the "nothing to issue" exit below, deliberately. A fleet whose certificates are all
    // current is exactly when nothing is due — and also exactly when a machine may have changed
    // address with nobody watching. Reconciling after that early return would mean DNS is only
    // ever fixed in the same pass as an issuance, which is once every sixty days.
    reconcile_dns(store, &domains).await;

    let due = match store.certificates_due(RETRY_AFTER_MINUTES).await {
        Ok(due) => due,
        Err(error) => {
            eprintln!("证书：读不出待签清单：{error}");
            return (0, 0);
        }
    };
    if due.is_empty() {
        return (0, 0);
    }

    let mut issued = 0;
    let mut failed = 0;
    for order in due {
        // Named by the group, not by a machine: one order covers every machine drawing from it,
        // and saying "hk-01 已签发" when five machines share the certificate would be wrong four
        // times over.
        let what = format!("{}.{}", order.label, order.domain);
        let cert_id = order.certificate_id.clone();
        // Stamped before the CA is asked anything, so that a process which dies partway through
        // one — or is restarted during one — still looks like it tried. Written afterwards only,
        // the backoff never applied to the case that needs it most.
        if let Err(error) = store.record_certificate_attempt(&cert_id).await {
            eprintln!("证书：{what} 记不下这次尝试，跳过这一轮：{error}");
            failed += 1;
            continue;
        }
        match issue(store, &order).await {
            Ok(seconds) => {
                issued += 1;
                eprintln!("证书：{what} 已签发，用时 {seconds:.0}s");
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
    }
    (issued, failed)
}

/// Points every issued name at the machine it belongs to.
///
/// Separate from issuance and run every scan, because the two go out of date for different
/// reasons: a certificate expires on a schedule, while a name stops resolving the moment a machine
/// changes address — and nothing announces that. Reconciling here means a rebuilt node fixes its
/// own DNS on the next pass, which is the same bargain the rest of the system makes.
///
/// Without the record the certificate is valid and unreachable: a client dialing a TLS ingress
/// resolves this name, and there is nothing else to resolve it to.
async fn reconcile_dns(store: &PgStore, domains: &[brocade_store::CertDomain]) {
    let targets = match store.certificate_dns_targets().await {
        Ok(targets) if !targets.is_empty() => targets,
        Ok(_) => return,
        Err(error) => {
            eprintln!("证书：读不出要对账的 DNS 记录：{error}");
            return;
        }
    };

    for domain in domains.iter().filter(|domain| domain.has_credential) {
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
        for target in targets
            .iter()
            .filter(|target| target.name.ends_with(&format!(".{}", domain.domain)))
        {
            match dns.ensure_a(&target.name, &target.ipv4).await {
                // Silence is the point: this runs hourly and says something only when it acted.
                Ok(None) => {}
                Ok(Some(what)) => eprintln!("证书：DNS {what}"),
                Err(error) => eprintln!("证书：{} 的 A 记录写不了：{error}", target.name),
            }
        }
    }
}

/// One order, start to finish. The error is a sentence meant for the console, not a type — every
/// caller does the same thing with it, which is show it to a person.
async fn issue(store: &PgStore, order: &CertificateOrder) -> Result<f64, String> {
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

        for challenge in &challenges {
            if let Err(error) = session.validate(challenge).await {
                retract(&dns, &published).await;
                return Err(error.to_string());
            }
        }
    }

    let result = session.finalize(&acme_order, &names).await;
    // Before the result is inspected: the records have done their job either way, and leaving them
    // behind on the failure path is exactly how the leftovers this code sweeps came to exist.
    retract(&dns, &published).await;
    let issued = result.map_err(|error| error.to_string())?;

    store
        .record_certificate(
            &order.certificate_id,
            &issued.chain_pem,
            &issued.key_pem,
            &issued.not_after,
            &issued.issuer,
        )
        .await
        .map_err(|error| format!("签发成功但存不下来：{error}"))?;

    Ok(started.elapsed().as_secs_f64())
}

/// Best-effort cleanup. A failure here is logged and not propagated: the order's own outcome is
/// what the caller is reporting, and "the certificate was issued but a TXT record could not be
/// deleted" must not read as a failed issuance.
async fn retract(dns: &Cloudflare, records: &[crate::dns::PublishedRecord]) {
    for record in records {
        if let Err(error) = dns.delete(&record.id).await {
            eprintln!(
                "证书：{} 的挑战记录没删掉（{error}）——下次签发前会被扫掉",
                record.name
            );
        }
    }
}

/// Starts the scan loop. Returns a handle the HTTP layer can use to ask for a scan now, which is
/// what the console's "retry" button is: the same pass, not a second code path.
pub fn spawn(store: PgStore) -> Arc<tokio::sync::Notify> {
    let wake = Arc::new(tokio::sync::Notify::new());
    let signal = wake.clone();
    tokio::spawn(async move {
        loop {
            // Scanning first and sleeping after, rather than the other way round. A restart is
            // not a quiet moment for this: it is what follows enrolling a machine, changing the
            // CA, or fixing a credential — exactly the states with work waiting. Sleeping first
            // made all of those read as "I changed the setting and nothing happened" for an hour,
            // and the only thing that ever broke the silence was somebody pressing retry.
            //
            // Safe to run on every start because being due is a property of the row: a
            // certificate that was just issued is not due again, and one that just failed is held
            // off by `RETRY_AFTER_MINUTES`. A restart loop therefore cannot spend the CA's rate
            // limit — provided the attempt is stamped before the CA is asked, which is why
            // `scan_once` does that first.
            let (issued, failed) = scan_once(&store).await;
            if issued > 0 || failed > 0 {
                eprintln!("证书：这一轮签发 {issued} 张，失败 {failed} 张");
            }
            // Both triggers, and the timer is the one that matters: the button only ever makes
            // something happen sooner.
            tokio::select! {
                _ = tokio::time::sleep(SCAN_INTERVAL) => {}
                _ = signal.notified() => {}
            }
        }
    });
    wake
}
