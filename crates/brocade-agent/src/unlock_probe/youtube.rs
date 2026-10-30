//! Independent YouTube webpage, Premium, response-region and video-CDN observations.
use super::{country, field, get, parse, region, result, Page};
use base64::{engine::general_purpose, Engine as _};
use brocade_deployment::unlock_probe::{
    UnlockCode, UnlockResult, UnlockService, UnlockStatus, YoutubeCdnResult, YoutubeDiagnostics,
    YoutubePageResult,
};
use std::time::Instant;

fn privacy_region<'a>(cookies: impl IntoIterator<Item = &'a str>) -> Option<String> {
    cookies
        .into_iter()
        .filter_map(|cookie| {
            let (name, value) = cookie.split(';').next()?.trim().split_once('=')?;
            if name != "VISITOR_PRIVACY_METADATA" || value.len() > 1024 {
                return None;
            }
            // Fresh YouTube responses encode their privacy-country context as protobuf field
            // 1, a two-byte string, in URL-safe base64. GL/client.gl can be US even for MO;
            // they describe the page's localization, not necessarily this request's country.
            // Only decode the observed explicit shape. A changed or absent shape stays unknown.
            let value = value.replace("%3D", "=").replace("%3d", "=");
            let bytes = general_purpose::URL_SAFE
                .decode(&value)
                .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(&value))
                .ok()?;
            if bytes.get(..2)? != [0x0a, 2] {
                return None;
            }
            std::str::from_utf8(bytes.get(2..4)?).ok().and_then(country)
        })
        .last()
}

fn premium_result(page: &Page) -> UnlockResult {
    let mut outcome = parse(
        UnlockService::YoutubePremium,
        page.response.status,
        &page.response.body,
        page.url.as_str(),
    );
    outcome.region = privacy_region(page.response.header_values("set-cookie")).or(outcome.region);
    outcome.apply_youtube_premium_region_policy();
    outcome
}

pub(super) fn topbar_region(body: &str) -> Option<String> {
    field(body, "desktopTopbarRenderer")
        .and_then(|renderer| renderer.get("countryCode")?.as_str().and_then(country))
}

fn page_result(page: Result<Page, UnlockCode>, homepage: bool) -> YoutubePageResult {
    let page = match page {
        Ok(page) => page,
        Err(code) => {
            return YoutubePageResult {
                status: if code.is_family_skip() {
                    UnlockStatus::Skipped
                } else {
                    UnlockStatus::Failed
                },
                code,
                http_status: None,
                page_region: None,
                topbar_region: None,
                privacy_region: None,
            }
        }
    };
    let (status, code) = if homepage {
        let body = page.response.body.to_ascii_lowercase();
        if body.contains("youtube is not available in your country") {
            (
                UnlockStatus::UnsupportedRegion,
                UnlockCode::RegionUnsupported,
            )
        } else if matches!(page.response.status, 403 | 429 | 451)
            || body.contains("<title>just a moment")
            || body.contains("window._cf_chl_opt")
            || body.contains("access denied")
            || body.contains("blocked by network security")
        {
            (UnlockStatus::Restricted, UnlockCode::AccessDenied)
        } else if (200..300).contains(&page.response.status)
            && page.response.body.contains("ytInitialData")
            && [
                "desktopTopbarRenderer",
                "richGridRenderer",
                "twoColumnBrowseResultsRenderer",
            ]
            .iter()
            .any(|marker| page.response.body.contains(marker))
        {
            (UnlockStatus::Available, UnlockCode::WebAvailable)
        } else {
            (UnlockStatus::Failed, UnlockCode::UnrecognizedResponse)
        }
    } else {
        let outcome = premium_result(&page);
        (outcome.status, outcome.code)
    };
    YoutubePageResult {
        status,
        code,
        http_status: Some(page.response.status),
        page_region: region(&page.response.body, &["GL"]),
        topbar_region: topbar_region(&page.response.body),
        privacy_region: privacy_region(page.response.header_values("set-cookie")),
    }
}

fn cdn_node(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (source, tail) = line.split_once("=>")?;
        let source = source.trim().rsplit(['>', ' ', '\t']).next()?;
        source.split('/').next()?.parse::<std::net::IpAddr>().ok()?;
        let tail = tail.trim_start();
        let node = tail.split_whitespace().next()?;
        if !(4..=96).contains(&node.len())
            || !node
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
            || !node.bytes().any(|byte| byte.is_ascii_alphabetic())
            || node
                .split(['.', '-', '_'])
                .any(|label| label.is_empty() || label.len() > 63)
        {
            return None;
        }
        // GGC also returns alphabetic node names such as nctsaa-ae. Those are normal
        // mappings, not a challenge page. Require the accompanying IP/prefix so a
        // changed response or a plain error word cannot masquerade as a mapping.
        if !node.bytes().any(|byte| byte.is_ascii_digit()) {
            if !node.contains('-') && !node.contains('.') && !node.contains('_') {
                return None;
            }
            let prefix = tail
                .strip_prefix(node)?
                .trim_start()
                .strip_prefix('(')?
                .split_once(')')?
                .0;
            let (ip, bits) = prefix
                .split_once('/')
                .map_or((prefix, None), |(ip, bits)| (ip, Some(bits)));
            let ip = ip.parse::<std::net::IpAddr>().ok()?;
            if bits.is_some_and(|bits| {
                bits.parse::<u8>()
                    .map_or(true, |bits| bits > if ip.is_ipv4() { 32 } else { 128 })
            }) {
                return None;
            }
        }
        Some(node.to_ascii_lowercase())
    })
}

fn cdn_airport(node: &str) -> (Option<String>, Option<String>) {
    let airport = node.split(['.', '-', '_']).find_map(|part| {
        let prefix = part.get(..3)?;
        (prefix.bytes().all(|byte| byte.is_ascii_alphabetic())
            && part.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .then(|| prefix.to_ascii_uppercase())
    });
    let country = airport
        .as_deref()
        .and_then(|airport| match airport {
            "HKG" => Some("HK"),
            "MFM" => Some("MO"),
            "TPE" | "TSA" | "KHH" => Some("TW"),
            "NRT" | "HND" | "KIX" | "NGO" | "FUK" => Some("JP"),
            "SIN" => Some("SG"),
            "KUL" | "JHB" | "PEN" => Some("MY"),
            "ICN" | "GMP" => Some("KR"),
            "BKK" => Some("TH"),
            "SGN" | "HAN" => Some("VN"),
            "MNL" => Some("PH"),
            "LAX" | "SJC" | "SFO" | "SEA" | "DFW" | "ORD" | "IAD" | "ATL" | "JFK" | "EWR"
            | "MIA" => Some("US"),
            "YYZ" | "YVR" => Some("CA"),
            "LHR" => Some("GB"),
            "AMS" => Some("NL"),
            "FRA" | "MUC" => Some("DE"),
            "CDG" => Some("FR"),
            "SYD" | "MEL" => Some("AU"),
            _ => None,
        })
        .map(str::to_owned);
    (airport, country)
}

fn cdn_result(page: Result<Page, UnlockCode>, elapsed_ms: u32) -> YoutubeCdnResult {
    let mut outcome = YoutubeCdnResult {
        http_status: None,
        node: None,
        airport: None,
        country: None,
        error: None,
        elapsed_ms,
    };
    let page = match page {
        Ok(page) => page,
        Err(code) => {
            outcome.error = Some(code);
            return outcome;
        }
    };
    outcome.http_status = Some(page.response.status);
    if matches!(page.response.status, 403 | 429 | 451) {
        outcome.error = Some(UnlockCode::AccessDenied);
    } else if (200..300).contains(&page.response.status) {
        outcome.node = cdn_node(&page.response.body);
        if let Some(node) = &outcome.node {
            (outcome.airport, outcome.country) = cdn_airport(node);
        } else {
            outcome.error = Some(UnlockCode::UnrecognizedResponse);
        }
    } else {
        outcome.error = Some(UnlockCode::UnrecognizedResponse);
    }
    outcome
}

pub(super) fn detect(deadline: Instant, proxy: Option<&super::ProbeContext>) -> UnlockResult {
    detect_with(|url| {
        use brocade_deployment::unlock_probe::UnlockPart;
        let part = if url.contains("report_mapping") {
            UnlockPart::Cdn
        } else if url.contains("/premium") {
            UnlockPart::Premium
        } else {
            UnlockPart::Webpage
        };
        let scoped = proxy.map(|context| super::ProbeContext {
            part,
            ..context.clone()
        });
        get(url, deadline, scoped.as_ref())
    })
}

fn detect_with(mut request: impl FnMut(&str) -> Result<Page, UnlockCode>) -> UnlockResult {
    // Independent facts, one path and one service deadline. A failed homepage, Premium
    // response or CDN mapping never substitutes its state or country for another item.
    let webpage = page_result(request("https://www.youtube.com/"), true);
    let premium = page_result(request("https://www.youtube.com/premium?hl=en"), false);
    let started = Instant::now();
    let mapping = request("https://redirector.googlevideo.com/report_mapping");
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
    let cdn = cdn_result(mapping, elapsed_ms);
    let mut outcome = result(
        UnlockService::YoutubePremium,
        premium.status,
        premium.code,
        premium.region().map(str::to_owned),
    );
    outcome.youtube = Some(YoutubeDiagnostics {
        webpage,
        premium,
        cdn,
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(body: &str, privacy: Option<&str>) -> Page {
        let cookie = privacy
            .map(|country| {
                let bytes = [b"\x0a\x02".as_slice(), country.as_bytes()].concat();
                format!(
                    "Set-Cookie: VISITOR_PRIVACY_METADATA={}; Path=/\r\n",
                    general_purpose::URL_SAFE.encode(bytes)
                )
            })
            .unwrap_or_default();
        let wire = format!(
            "HTTP/1.1 200 OK\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        Page {
            response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
            url: reqwest::Url::parse("https://www.youtube.com/").unwrap(),
        }
    }

    #[test]
    fn split_results_preserve_page_configuration_privacy_country_and_cdn_independently() {
        let result = detect_with(|url| {
            Ok(if url.contains("report_mapping") {
                page("203.0.113.1 => hkg12s15 (2001:db8::/32)", None)
            } else if url.contains("/premium") {
                page(
                    r#"{"GL":"US","browseId":"SPunlimited"} Premium is not available in your country"#,
                    Some("MO"),
                )
            } else {
                page(
                    r#"var ytInitialData = {"GL":"US","desktopTopbarRenderer":{}};"#,
                    Some("MO"),
                )
            })
        });
        assert!(result.valid());
        assert_eq!(result.status, UnlockStatus::UnsupportedRegion);
        assert_eq!(result.region.as_deref(), Some("MO"));
        let detail = result.youtube.unwrap();
        assert_eq!(detail.webpage.status, UnlockStatus::Available);
        assert_eq!(detail.webpage.page_region.as_deref(), Some("US"));
        assert_eq!(detail.webpage.privacy_region.as_deref(), Some("MO"));
        assert_eq!(detail.premium.status, UnlockStatus::UnsupportedRegion);
        assert_eq!(detail.cdn.node.as_deref(), Some("hkg12s15"));
        assert_eq!(detail.cdn.airport.as_deref(), Some("HKG"));
        assert_eq!(detail.cdn.country.as_deref(), Some("HK"));
        let serialized = serde_json::to_string(&detail).unwrap();
        for private in ["203.0.113.1", "2001:db8", "VISITOR_PRIVACY_METADATA"] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn premium_country_overrides_generic_success_markers_without_changing_other_parts() {
        for (privacy, expected) in [
            (Some("CN"), UnlockStatus::UnsupportedRegion),
            (Some("MO"), UnlockStatus::UnsupportedRegion),
            (Some("HK"), UnlockStatus::Available),
            (None, UnlockStatus::Available),
        ] {
            let mut requests = 0;
            let result = detect_with(|url| {
                requests += 1;
                Ok(if url.contains("report_mapping") {
                    page("203.0.113.1 => hkg12s15 (2001:db8::/32)", None)
                } else {
                    page(
                        r#"ytInitialData={"GL":"US","desktopTopbarRenderer":{"countryCode":"HK"},"browseId":"SPunlimited"}; ad-free"#,
                        privacy,
                    )
                })
            });
            assert_eq!(requests, 3);
            assert!(result.valid());
            assert_eq!(result.status, expected);
            assert_eq!(result.region.as_deref(), privacy.or(Some("HK")));
            let detail = result.youtube.unwrap();
            assert_eq!(detail.premium.status, expected);
            assert_eq!(detail.webpage.status, UnlockStatus::Available);
            assert_eq!(detail.cdn.node.as_deref(), Some("hkg12s15"));
        }
        for region in ["CN", "MO"] {
            let body = format!(
                r#"ytInitialData={{"GL":"US","desktopTopbarRenderer":{{"countryCode":"{region}"}},"browseId":"SPunlimited"}};"#
            );
            let premium = premium_result(&page(&body, None));
            assert_eq!(premium.status, UnlockStatus::UnsupportedRegion);
            assert_eq!(premium.code, UnlockCode::RegionUnsupported);
        }
    }

    #[test]
    fn a_failed_subprobe_does_not_hide_other_youtube_results() {
        let mut requested = Vec::new();
        let result = detect_with(|url| {
            requested.push(url.to_owned());
            if url.contains("/premium") {
                Err(UnlockCode::NetworkError)
            } else if url.contains("report_mapping") {
                Err(UnlockCode::Timeout)
            } else {
                Ok(page(
                    r#"ytInitialData={"desktopTopbarRenderer":{"countryCode":"HK"}}; Premium is not available in your country"#,
                    None,
                ))
            }
        });
        assert_eq!(requested.len(), 3);
        assert!(result.valid());
        assert_eq!(result.status, UnlockStatus::Failed);
        let detail = result.youtube.unwrap();
        assert_eq!(detail.webpage.status, UnlockStatus::Available);
        assert_eq!(detail.webpage.region(), Some("HK"));
        assert_eq!(detail.premium.code, UnlockCode::NetworkError);
        assert_eq!(detail.cdn.error, Some(UnlockCode::Timeout));
    }

    #[test]
    fn cdn_mapping_is_sanitized_and_unknown_locations_are_not_guessed() {
        for body in [
            "wrong => hkg12s15",
            "203.0.113.1 => <script>alert(1)</script>",
            "203.0.113.1 => 192.0.2.1",
            "203.0.113.1 => https://secret.test/key",
            "changed response",
            "203.0.113.1 => unrecognized",
            "203.0.113.1 => not-found",
            "203.0.113.1 => nctsaa-ae (not-an-address)",
            "203.0.113.1 => nctsaa-ae (192.0.2.0/129)",
        ] {
            assert!(cdn_node(body).is_none());
        }
        assert_eq!(
            cdn_node("<pre>203.0.113.1 => router.hkg12s15 (2001:db8::/32)</pre>").as_deref(),
            Some("router.hkg12s15")
        );
        assert_eq!(
            cdn_airport("router.hkg12s15"),
            (Some("HKG".into()), Some("HK".into()))
        );
        assert_eq!(
            cdn_airport("isp-mfm01"),
            (Some("MFM".into()), Some("MO".into()))
        );
        assert_eq!(cdn_airport("zzz12s15"), (Some("ZZZ".into()), None));
        assert_eq!(cdn_airport("unrecognized"), (None, None));
    }
    #[test]
    fn alphabetic_ggc_nodes_are_valid_mappings_without_guessing_an_airport() {
        for (mapping, expected) in [
            (
                "203.0.113.1 => nctsaa-ae (192.0.2.0/25)\n\nDebug Info:\nprivate-data",
                "nctsaa-ae",
            ),
            ("2001:db8::1 => lctsaa-af (2001:db8::/64)", "lctsaa-af"),
        ] {
            assert_eq!(cdn_node(mapping).as_deref(), Some(expected));
            let outcome = cdn_result(Ok(page(mapping, None)), 42);
            assert!(outcome.valid());
            assert_eq!(outcome.error, None);
            assert_eq!(outcome.node.as_deref(), Some(expected));
            assert_eq!(outcome.airport, None);
            assert_eq!(outcome.country, None);
            let encoded = serde_json::to_string(&outcome).unwrap();
            assert!(!encoded.contains("private-data"));
            assert!(!encoded.contains("203.0.113"));
            assert!(!encoded.contains("2001:db8"));
        }
    }
    #[test]
    fn youtube_privacy_country_overrides_us_page_localization() {
        let body = r#"{"GL":"US","INNERTUBE_CONTEXT":{"client":{"gl":"US"}},"browseId":"SPunlimited"} Premium is not available in your country"#;
        let encoded = general_purpose::URL_SAFE.encode(b"\x0a\x02MO\x12\x00");
        let wire = format!("HTTP/1.1 200 OK\r\nSet-Cookie: VISITOR_PRIVACY_METADATA={}; Path=/; Secure\r\nSet-Cookie: unrelated=mock\r\nContent-Length: {}\r\n\r\n{body}", encoded.replace('=', "%3D"), body.len());
        let page = Page {
            response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
            url: reqwest::Url::parse("https://www.youtube.com/premium?hl=en").unwrap(),
        };
        let outcome = premium_result(&page);
        assert_eq!(outcome.region.as_deref(), Some("MO"));
        assert_eq!(outcome.status, UnlockStatus::UnsupportedRegion);
        assert!(outcome.valid());
        assert!(parse(UnlockService::YoutubePremium, 200, body, "")
            .region
            .is_none());
        assert!(parse(
            UnlockService::YoutubePremium,
            200,
            r#"{"GL":"US","countryCode":"US"}"#,
            ""
        )
        .region
        .is_none());
    }

    #[test]
    fn youtube_privacy_country_is_strict_and_never_reads_unrelated_cookies() {
        let encoded = general_purpose::URL_SAFE_NO_PAD.encode(b"\x0a\x02HK");
        let cookie = format!("VISITOR_PRIVACY_METADATA={encoded}; Secure");
        assert_eq!(privacy_region([cookie.as_str()]).as_deref(), Some("HK"));
        for bytes in [
            b"\x12\x02MO".as_slice(),
            b"\x0a\x03MOO",
            b"\x0a\x02M",
            b"\x0a\x0212",
        ] {
            let encoded = general_purpose::URL_SAFE.encode(bytes);
            assert!(
                privacy_region([format!("VISITOR_PRIVACY_METADATA={encoded}").as_str()]).is_none()
            );
        }
        for cookie in [
            "unrelated=CgJNTw==".to_owned(),
            "VISITOR_PRIVACY_METADATA=not_base64%".to_owned(),
            format!("VISITOR_PRIVACY_METADATA={}", "A".repeat(1025)),
        ] {
            assert!(privacy_region([cookie.as_str()]).is_none());
        }
    }
}
