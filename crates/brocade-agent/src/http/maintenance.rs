//! A private asynchronous HTTP client with connection-pinned, public-only DNS results.
use super::{resolver, tls_config, AddressFamily};
use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) async fn maintenance_client(
    url: &reqwest::Url,
    family: AddressFamily,
    source: Option<IpAddr>,
    cookies: Arc<reqwest_cookie_store::CookieStoreMutex>,
    timeout: Duration,
    _compressed: bool,
) -> Result<reqwest::Client, String> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid_target".into());
    }
    let host = url
        .host_str()
        .ok_or("invalid_target")?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url.port_or_known_default().ok_or("invalid_target")?;
    let mut addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        let resolver = resolver::public();
        let host = host.clone();
        tokio::task::spawn_blocking(move || {
            resolver.resolve_blocking(&host, port, Instant::now() + timeout)
        })
        .await
        .map_err(|_| "dns_worker")?
        .map_err(|_| "dns")?
    };
    // resolve_blocking returns both families. Only addresses we can connect to belong
    // in the pinned set; an ordinary dual-stack answer is not an unsafe target.
    addresses.retain(|address| family.accepts(address));
    if addresses.is_empty()
        || addresses.iter().any(|address| {
            brocade_core::forwarding::target_ip_is_blocked_by_policy(address.ip(), false)
        })
    {
        return Err("non_public_target".into());
    }
    let mut tls = (*tls_config()?).clone();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    tls.resumption = rustls::client::Resumption::disabled();
    let mut builder = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .no_proxy()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .cookie_provider(cookies)
        .no_gzip()
        .pool_max_idle_per_host(0)
        .connect_timeout(timeout)
        .timeout(timeout)
        .resolve_to_addrs(&host, &addresses);
    builder = builder.local_address(source.unwrap_or(match family {
        AddressFamily::V4 => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        AddressFamily::V6 => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    }));
    builder.build().map_err(|_| "client_configuration".into())
}
