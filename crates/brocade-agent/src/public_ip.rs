use std::{
    net::IpAddr,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brocade_deployment::protocol::{E2eProbeTargetList, NodePublicIpObservation, PublicIpFamily};

use crate::{
    http::{AddressFamily, HttpClient},
    options::Options,
};

const MAX_TRACE_BODY_BYTES: usize = 64 * 1024;

pub(crate) fn observe_and_report(
    options: &Options,
    settings: &E2eProbeTargetList,
) -> Vec<(PublicIpFamily, Result<(), String>)> {
    let timeout = Duration::from_secs(settings.timeout_secs.clamp(1, 120));
    let observations = thread::scope(|scope| {
        let v4 = scope.spawn(|| {
            observe_family(
                &settings.endpoint_url,
                timeout,
                PublicIpFamily::V4,
                AddressFamily::V4,
            )
        });
        let v6 = scope.spawn(|| {
            observe_family(
                &settings.endpoint_url,
                timeout,
                PublicIpFamily::V6,
                AddressFamily::V6,
            )
        });
        [
            (PublicIpFamily::V4, v4.join()),
            (PublicIpFamily::V6, v6.join()),
        ]
    });

    observations
        .into_iter()
        .map(|(family, joined)| {
            let result = joined
                .map_err(|_| "公网 IP 探测线程异常退出".to_owned())
                .and_then(std::convert::identity)
                .and_then(|observation| send_observation(options, &observation));
            (family, result)
        })
        .collect()
}

fn observe_family(
    endpoint_url: &str,
    timeout: Duration,
    family: PublicIpFamily,
    address_family: AddressFamily,
) -> Result<NodePublicIpObservation, String> {
    let client = HttpClient::new(endpoint_url)?;
    let response = client.request_public_on_family(address_family, "GET", "", timeout)?;
    if !(200..400).contains(&response.status) {
        return Err(format!("CGI Trace 返回 HTTP {}", response.status));
    }
    if response.body.len() > MAX_TRACE_BODY_BYTES {
        return Err(format!("CGI Trace 响应超过 {MAX_TRACE_BODY_BYTES} 字节"));
    }
    parse_trace(&response.body, family)
}

fn parse_trace(body: &str, family: PublicIpFamily) -> Result<NodePublicIpObservation, String> {
    let ip = trace_field(body, "ip")
        .ok_or("CGI Trace 响应缺少 ip 字段")?
        .parse::<IpAddr>()
        .map_err(|error| format!("CGI Trace ip 字段无效：{error}"))?;
    let family_matches = matches!(
        (family, ip),
        (PublicIpFamily::V4, IpAddr::V4(_)) | (PublicIpFamily::V6, IpAddr::V6(_))
    );
    if !family_matches {
        return Err(format!(
            "强制探测 {}，但 CGI Trace 返回了 {ip}",
            family_label(family)
        ));
    }
    let country_code = trace_field(body, "loc").and_then(|value| {
        (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()))
            .then(|| value.to_ascii_uppercase())
    });
    let observed_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before unix epoch: {error}"))?
        .as_secs()
        .try_into()
        .map_err(|_| "unix timestamp does not fit i64".to_owned())?;

    Ok(NodePublicIpObservation {
        observed_at_unix_secs,
        family,
        ip: ip.to_string(),
        country_code,
    })
}

fn trace_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name == key && !value.is_empty()).then_some(value)
    })
}

fn send_observation(
    options: &Options,
    observation: &NodePublicIpObservation,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(observation).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/public-ip-observation",
        &options.token,
        Some(&body),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "公网 IP 观测上报失败：HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

pub(crate) fn family_index(family: PublicIpFamily) -> usize {
    match family {
        PublicIpFamily::V4 => 0,
        PublicIpFamily::V6 => 1,
    }
}

pub(crate) fn family_label(family: PublicIpFamily) -> &'static str {
    match family {
        PublicIpFamily::V4 => "IPv4",
        PublicIpFamily::V6 => "IPv6",
    }
}

#[cfg(test)]
mod tests {
    use brocade_deployment::protocol::PublicIpFamily;

    use super::parse_trace;

    #[test]
    fn parses_each_trace_family_and_normalizes_country() {
        let v4 = parse_trace("fl=x\nip=1.1.1.1\nloc=tw\n", PublicIpFamily::V4).unwrap();
        assert_eq!(v4.ip, "1.1.1.1");
        assert_eq!(v4.country_code.as_deref(), Some("TW"));

        let v6 = parse_trace("ip=2606:4700:4700::1111\nloc=US\n", PublicIpFamily::V6).unwrap();
        assert_eq!(v6.ip, "2606:4700:4700::1111");
    }

    #[test]
    fn rejects_a_trace_response_from_the_other_family() {
        let error = parse_trace("ip=1.1.1.1\nloc=US\n", PublicIpFamily::V6).unwrap_err();
        assert!(error.contains("IPv6"), "{error}");
    }

    #[test]
    fn ignores_invalid_location_without_losing_the_ip() {
        let observation = parse_trace("ip=1.1.1.1\nloc=USA\n", PublicIpFamily::V4).unwrap();
        assert_eq!(observation.country_code, None);
    }
}
