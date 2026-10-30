//! Lightweight anonymous service checks; no browser, account login, or media playback.
use super::{
    country, field, get, html_attribute, request_error_code, result, safe_url, HttpClient,
    HttpResponse, ProbeContext,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use brocade_deployment::unlock_probe::{
    UnlockCode as C, UnlockResult, UnlockService as S, UnlockStatus as U,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

const SPOTIFY_URL: &str = "https://open.spotify.com/";
const HBO_MAX_URL: &str = "https://www.hbomax.com/";
const DAZN_URL: &str = "https://startup.core.indazn.com/misl/v5/Startup";
const BBC_URL: &str = "https://open.live.bbc.co.uk/mediaselector/6/select/version/2.0/mediaset/pc/vpid/bbc_one_london/format/json/jsfunc/JS_callbacks0";
const CRUNCHYROLL_URL: &str = "https://www.crunchyroll.com/auth/v1/token";

fn post(
    url: &str,
    body: &str,
    content_type: &str,
    headers: &[(&str, String)],
    deadline: Instant,
    proxy: Option<&ProbeContext>,
) -> Result<HttpResponse, C> {
    let deadline = super::request_deadline(deadline, proxy);
    let url = reqwest::Url::parse(url).map_err(|_| C::UnrecognizedResponse)?;
    if !safe_url(&url) {
        return Err(C::UnrecognizedResponse);
    }
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Err(C::Timeout);
    }
    if let Some(context) = proxy {
        context.track_request();
    }
    HttpClient::new(&url.origin().ascii_serialization())
        .map_err(|_| C::NetworkError)?
        .with_diagnostic_proxy(proxy.map(|context| &context.client))
        .request_public_diagnostic(
            "POST",
            url.path(),
            Some(body),
            content_type,
            headers,
            budget,
        )
        .map_err(|error| request_error_code(&error, proxy))
}

pub(super) fn detect(
    service: S,
    deadline: Instant,
    proxy: Option<&ProbeContext>,
) -> Option<Result<UnlockResult, C>> {
    Some(match service {
        S::Spotify => spotify(deadline, proxy),
        S::Dazn => post(DAZN_URL, r#"{"Version":"2","LandingPageKey":"generic","Languages":"en","Platform":"web","Manufacturer":"","PromoCode":"","PlatformAttributes":{}}"#, "application/json", &[("Origin", "https://www.dazn.com".into())], deadline, proxy)
            .map(|response| evaluate(service, &response, DAZN_URL)),
        // cr_web is the public anonymous client identifier in Crunchyroll's web config.
        // Only country and token presence are inspected; the short-lived token is discarded.
        S::Crunchyroll => post(CRUNCHYROLL_URL, "grant_type=client_id&scope=offline_access", "application/x-www-form-urlencoded", &[("Authorization", format!("Basic {}", STANDARD.encode("cr_web:")))], deadline, proxy)
            .map(|response| evaluate(service, &response, CRUNCHYROLL_URL)),
        S::HboMax | S::BbcIplayer | S::ParamountPlus => {
            let url = match service { S::HboMax => HBO_MAX_URL, S::BbcIplayer => BBC_URL, _ => "https://www.paramountplus.com/" };
            get(url, deadline, proxy).map(|page| evaluate(service, &page.response, page.url.as_str()))
        }
        _ => return None,
    })
}

fn spotify(deadline: Instant, proxy: Option<&ProbeContext>) -> Result<UnlockResult, C> {
    let page = get(SPOTIFY_URL, deadline, proxy)?;
    Ok(evaluate(S::Spotify, &page.response, page.url.as_str()))
}

fn spotify_market(body: &str) -> Option<String> {
    body.split("</script>").find_map(|block| {
        let (_, tail) = block.rsplit_once("<script")?;
        let (attributes, encoded) = tail.split_once('>')?;
        if html_attribute(attributes, "id") != Some("appServerConfig") || encoded.len() > 65_536 {
            return None;
        }
        let decoded = STANDARD.decode(encoded.trim()).ok()?;
        let value: Value = serde_json::from_slice(&decoded).ok()?;
        if value.get("isAnonymous")?.as_bool() != Some(true)
            || !matches!(
                value.get("appName")?.as_str()?,
                "web_player" | "web_player_prototype"
            )
        {
            return None;
        }
        let market = value.get("market")?.as_str()?;
        (market.len() == 2 && !matches!(market.to_ascii_uppercase().as_str(), "XX" | "ZZ"))
            .then(|| country(market))
            .flatten()
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HboGeoConfiguration {
    brand: HboGeoBrand,
    page_locales: HboGeoLocales,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HboGeoBrand {
    default_lang: BTreeMap<String, String>,
    region: BTreeMap<String, Vec<String>>,
    territory: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HboGeoLocales {
    available_locales: Vec<String>,
    can_be_out_of_region: bool,
    pathname: String,
    #[serde(default)]
    windows: BTreeMap<String, Option<[Option<String>; 2]>>,
}

fn hbo_max_geo(response: &HttpResponse) -> (Option<String>, Option<bool>) {
    let script = response.body.split("</script>").find_map(|block| {
        let (_, tail) = block.rsplit_once("<script")?;
        let (attributes, json) = tail.split_once('>')?;
        (html_attribute(attributes, "id") == Some("__DOTCOM_GEO__")).then_some(json)
    });
    let Some(script) = script else {
        // Retain the older server-rendered decision during the site's transition.
        let region =
            field(&response.body, "userCountry").and_then(|value| value.as_str().and_then(country));
        let allowed =
            match field(&response.body, "isUserOutOfRegion").and_then(|value| value.as_bool()) {
                Some(true) => Some(false),
                Some(false)
                    if region.is_some()
                        && field(&response.body, "countryLangUris")
                            .is_some_and(|value| value.is_object()) =>
                {
                    Some(true)
                }
                _ => None,
            };
        return (region, allowed);
    };
    // The current client reads the fresh edge cookie, not analytics userCountry or
    // the page's default US locale. The last cookie of this name wins in a browser.
    let region = response
        .header_values("set-cookie")
        .filter_map(|cookie| {
            let (name, value) = cookie.split(';').next()?.trim().split_once('=')?;
            (name == "countryCode").then_some(value)
        })
        .last()
        .filter(|value| {
            value.len() == 2 && !matches!(value.to_ascii_uppercase().as_str(), "XX" | "ZZ")
        })
        .and_then(country);
    let allowed = (|| {
        let region = region.as_ref()?.to_ascii_lowercase();
        if script.len() > 65_536 {
            return None;
        }
        let geo: HboGeoConfiguration = serde_json::from_str(script).ok()?;
        let locales = geo.page_locales;
        // Only the global root landing configuration describes service-wide support.
        if locales.pathname != "/"
            || locales.can_be_out_of_region
            || locales.available_locales.is_empty()
            || geo.brand.default_lang.is_empty()
            || !geo
                .brand
                .region
                .values()
                .any(|countries| !countries.is_empty())
        {
            return None;
        }
        let market = geo
            .brand
            .territory
            .iter()
            .find_map(|(market, territories)| territories.contains(&region).then_some(market))
            .unwrap_or(&region);
        if !geo
            .brand
            .region
            .values()
            .any(|countries| countries.contains(market))
        {
            return Some(false);
        }
        let language = geo
            .brand
            .default_lang
            .get(market)
            .map_or("en", String::as_str);
        let locale = format!("{market}/{language}");
        if locales.windows.get(&locale).is_some_and(|window| {
            window
                .as_ref()
                .is_some_and(|bounds| bounds.iter().any(Option::is_some))
        }) {
            // A scheduled locale is not evidence of current availability.
            return None;
        }
        // The marketing region map also includes partner-only markets such as JP;
        // require the actual landing locale before reporting direct web availability.
        Some(locales.available_locales.contains(&locale))
    })();
    (region, allowed)
}

fn jsonp(body: &str) -> Option<Value> {
    let body = body
        .trim()
        .strip_prefix("/**/")
        .unwrap_or(body.trim())
        .trim();
    let payload = body.strip_prefix("JS_callbacks0(")?.strip_suffix(");")?;
    serde_json::from_str(payload).ok()
}

fn evaluate(service: S, response: &HttpResponse, url: &str) -> UnlockResult {
    let body = &response.body;
    let hbo_geo = (service == S::HboMax).then(|| hbo_max_geo(response));
    let value: Option<Value> = if service == S::BbcIplayer {
        jsonp(body)
    } else {
        serde_json::from_str(body).ok()
    };
    let region = match service {
        S::HboMax => hbo_geo.as_ref().and_then(|(region, _)| region.clone()),
        S::Dazn => field(body, "GeolocatedCountry").and_then(|v| v.as_str().and_then(country)),
        S::Spotify => spotify_market(body),
        S::Crunchyroll => value
            .as_ref()
            .and_then(|v| v.get("country")?.as_str().and_then(country)),
        S::ParamountPlus => field(body, "userCountry")
            .and_then(|v| v.as_str().and_then(country))
            .or_else(|| {
                let url = reqwest::Url::parse(url).ok()?;
                let part = url.path().trim_matches('/').split('/').next()?;
                (part.len() == 2).then(|| country(part)).flatten()
            }),
        _ => None,
    };
    // Full application pages contain unused translations for errors. Decisions below
    // use the service's explicit response, rather than searching every script for an
    // unavailable-country/access-denied sentence. Actual interstitials still win.
    let lower = body.to_ascii_lowercase();
    if matches!(response.status, 403 | 406 | 429 | 451)
        || lower.contains("<title>just a moment")
        || lower.contains("window._cf_chl_opt")
        || lower.contains("blocked by network security")
        || lower.contains("security policy has been breached")
    {
        return result(service, U::Restricted, C::AccessDenied, region);
    }
    if !(200..300).contains(&response.status) {
        return result(service, U::Failed, C::UnrecognizedResponse, region);
    }
    let (status, code) = match service {
        S::Spotify => {
            if region.is_some() {
                (U::Available, C::WebAvailable)
            } else {
                (U::Failed, C::UnrecognizedResponse)
            }
        }
        S::HboMax => match hbo_geo.and_then(|(_, allowed)| allowed) {
            Some(true) => (U::Available, C::WebAvailable),
            Some(false) => (U::UnsupportedRegion, C::RegionUnsupported),
            _ => (U::Failed, C::UnrecognizedResponse),
        },
        S::Dazn => match field(body, "isAllowed").and_then(|v| v.as_bool()) {
            Some(true) if region.is_some() => (U::Available, C::WebAvailable),
            Some(false) => (U::UnsupportedRegion, C::RegionUnsupported),
            _ => (U::Failed, C::UnrecognizedResponse),
        },
        S::BbcIplayer => {
            if value.as_ref().and_then(|v| v.get("result")?.as_str()) == Some("geolocation") {
                (U::UnsupportedRegion, C::RegionUnsupported)
            } else if value
                .as_ref()
                .and_then(|v| v.get("media")?.as_array())
                .is_some_and(|media| {
                    media.iter().any(|item| {
                        item.get("connection")
                            .and_then(Value::as_array)
                            .is_some_and(|connections| {
                                connections.iter().any(|connection| {
                                    connection
                                        .get("href")
                                        .and_then(Value::as_str)
                                        .and_then(|href| reqwest::Url::parse(href).ok())
                                        .is_some_and(|url| {
                                            url.scheme() == "https"
                                                && url.host_str().is_some_and(|host| {
                                                    [
                                                        "bbc.co.uk",
                                                        "bbc.net.uk",
                                                        "akamaized.net",
                                                        "akamaihd.net",
                                                    ]
                                                    .iter()
                                                    .any(|suffix| {
                                                        host == *suffix
                                                            || host.ends_with(&format!(".{suffix}"))
                                                    })
                                                })
                                        })
                                })
                            })
                    })
                })
            {
                (U::Available, C::WebAvailable)
            } else {
                (U::Failed, C::UnrecognizedResponse)
            }
        }
        S::Crunchyroll => {
            if region.is_some()
                && value.as_ref().is_some_and(|v| {
                    v.get("access_token")
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.is_empty() && token.len() <= 16_384)
                        && v.get("token_type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
                })
            {
                (U::Available, C::WebAvailable)
            } else {
                (U::Failed, C::UnrecognizedResponse)
            }
        }
        S::ParamountPlus => {
            if reqwest::Url::parse(url)
                .is_ok_and(|url| url.path().trim_matches('/').split('/').next() == Some("intl"))
            {
                (U::UnsupportedRegion, C::RegionUnsupported)
            } else if body.to_ascii_lowercase().contains("paramount")
                && (body.contains("window.CBS") || body.contains("\"siteType\":\"paramountplus\""))
            {
                (U::Available, C::WebAvailable)
            } else {
                (U::Failed, C::UnrecognizedResponse)
            }
        }
        _ => (U::Failed, C::UnrecognizedResponse),
    };
    // A successful BBC media-selector response is specifically the UK-only live service.
    let region = if service == S::BbcIplayer && status == U::Available {
        Some("GB".into())
    } else {
        region
    };
    result(service, status, code, region)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "contacts only Spotify Web Player on the development machine"]
    fn live_spotify_web_player() {
        let context = ProbeContext {
            client: crate::http::diagnostic_family_client(crate::http::AddressFamily::V4).unwrap(),
            request_timeout: super::super::REQUEST_TIMEOUT,
            ipv6: false,
            trace: None,
            trace_expected: false,
            service: None,
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        };
        let outcome = spotify(Instant::now() + Duration::from_secs(20), Some(&context)).unwrap();
        assert!(outcome.valid());
        println!("{}", serde_json::to_string(&outcome).unwrap());
        assert_eq!(outcome.status, U::Available);
        assert!(outcome.region.is_some());
    }
    #[test]
    #[ignore = "contacts public services on the development machine; sanitized results only"]
    fn live_extended_service_results() {
        for service in [
            S::Spotify,
            S::HboMax,
            S::Dazn,
            S::BbcIplayer,
            S::Crunchyroll,
            S::ParamountPlus,
        ] {
            let outcome = detect(service, Instant::now() + Duration::from_secs(20), None)
                .unwrap()
                .unwrap_or_else(|code| result(service, U::Failed, code, None));
            assert!(outcome.valid());
            println!("{}", serde_json::to_string(&outcome).unwrap());
        }
    }
    #[test]
    #[ignore = "contacts HBO Max through an explicitly selected diagnostic SOCKS proxy"]
    fn live_hbo_max_transport() {
        let port: u16 = std::env::var("BROCADE_DIAGNOSTIC_SOCKS_PORT")
            .expect("select an explicit diagnostic SOCKS proxy")
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        let context = ProbeContext {
            client: crate::http::diagnostic_socks_client(port).unwrap(),
            request_timeout: super::super::CHAIN_REQUEST_TIMEOUT,
            ipv6: false,
            trace: None,
            trace_expected: false,
            service: Some(S::HboMax),
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        };
        let outcome = detect(
            S::HboMax,
            Instant::now() + Duration::from_secs(20),
            Some(&context),
        )
        .unwrap()
        .unwrap();
        assert!(outcome.valid());
        println!("{}", serde_json::to_string(&outcome).unwrap());
        assert_ne!(outcome.status, U::Failed);
        assert!(outcome.region.is_some());
    }
    fn check(service: S, status: u16, body: &str, path: &str) -> UnlockResult {
        let wire = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = crate::http::tests::response_from_wire(wire.as_bytes()).unwrap();
        let result = evaluate(service, &response, path);
        assert!(result.valid());
        result
    }
    #[test]
    fn spotify_uses_the_web_player_configuration_and_market() {
        for name in ["web_player", "web_player_prototype"] {
            let data = STANDARD.encode(format!(
                r#"{{"appName":"{name}","market":"HK","isAnonymous":true}}"#
            ));
            let page = format!("<title>Spotify</title><script id='appServerConfig' type='text/plain'>{data}</script>");
            let outcome = check(S::Spotify, 200, &page, SPOTIFY_URL);
            assert_eq!(outcome.status, U::Available);
            assert_eq!(outcome.region.as_deref(), Some("HK"));
            assert!(spotify_market(&format!("<script id='other'>{data}</script>")).is_none());
            for status in [403, 406, 429, 451] {
                assert_eq!(
                    check(S::Spotify, status, &page, SPOTIFY_URL).status,
                    U::Restricted
                );
            }
            assert_eq!(
                check(
                    S::Spotify,
                    200,
                    &format!("<script>window._cf_chl_opt={{}};</script>{page}"),
                    SPOTIFY_URL
                )
                .status,
                U::Restricted
            );
        }
    }
    #[test]
    fn spotify_does_not_accept_plain_success_bad_configurations_or_old_responses() {
        for config in [
            r#"{"market":"HK","isAnonymous":true}"#,
            r#"{"appName":"web_player","market":"HK","isAnonymous":false}"#,
            r#"{"appName":"other","market":"HK","isAnonymous":true}"#,
            r#"{"appName":"web_player","isAnonymous":true}"#,
            r#"{"appName":"web_player","market":"ZZ","isAnonymous":true}"#,
            r#"{"appName":"web_player","market":"not-a-country","isAnonymous":true}"#,
            "not-json",
        ] {
            let data = STANDARD.encode(config);
            let page = format!("<script id='appServerConfig'>{data}</script>");
            let outcome = check(S::Spotify, 200, &page, SPOTIFY_URL);
            assert_eq!(outcome.status, U::Failed);
            assert!(outcome.region.is_none());
        }
        for page in [
            "<title>Spotify</title>",
            "<script id='appServerConfig'>bad-base64!</script>",
            r#"{"status":320,"country":"HK"}"#,
        ] {
            assert_eq!(check(S::Spotify, 200, page, SPOTIFY_URL).status, U::Failed);
        }
    }
    #[test]
    fn hbo_max_uses_the_server_geo_decision_and_never_a_country_menu() {
        let ok = check(
            S::HboMax,
            200,
            r#"{"userCountry":"HK","isUserOutOfRegion":false,"countryLangUris":{"us/en":{"url":"/us/en"}}}"#,
            "https://www.max.com/",
        );
        assert_eq!(ok.status, U::Available);
        assert_eq!(ok.region.as_deref(), Some("HK"));
        assert_eq!(
            check(
                S::HboMax,
                200,
                r#"{"userCountry":"CN","isUserOutOfRegion":true}"#,
                "https://www.max.com/"
            )
            .status,
            U::UnsupportedRegion
        );
        assert_eq!(
            check(
                S::HboMax,
                200,
                r#"{"countryCode":"US","countryLangUris":{"us/en":{}}}"#,
                "https://www.max.com/"
            )
            .status,
            U::Failed
        );
        let translated_errors = r#"{"userCountry":"HK","isUserOutOfRegion":false,"countryLangUris":{},"translations":["not available in your country","access denied"]}"#;
        assert_eq!(
            check(S::HboMax, 200, translated_errors, "https://www.max.com/").status,
            U::Available
        );
        assert_eq!(
            check(S::HboMax, 403, translated_errors, "https://www.max.com/").status,
            U::Restricted
        );
        assert_eq!(check(S::HboMax, 200, &format!("<title>Just a moment</title><script>window._cf_chl_opt={{}};</script>{translated_errors}"), "https://www.max.com/").status, U::Restricted);
    }
    const HBO_GEO_PAGE: &str = include_str!("../../tests/fixtures/hbo-max-geo.html");

    fn hbo_page(status: u16, body: &str, cookies: &[&str]) -> UnlockResult {
        let response = HttpResponse {
            status,
            body: body.into(),
            headers: std::collections::BTreeMap::from([(
                "set-cookie".into(),
                cookies.iter().map(|cookie| (*cookie).into()).collect(),
            )]),
        };
        let outcome = evaluate(S::HboMax, &response, "https://www.hbomax.com/");
        assert!(outcome.valid());
        outcome
    }

    #[test]
    fn hbo_max_uses_the_current_edge_country_and_landing_page_geo_configuration() {
        let cookies = [
            "countryCode=US; Path=/",
            "countryCode=HK; Expires=Thu, 29 Oct 2026 05:00:00 GMT; Path=/; Domain=.hbomax.com",
            "unrelated=mock; Path=/",
        ];
        let outcome = hbo_page(200, HBO_GEO_PAGE, &cookies);
        assert_eq!(outcome.status, U::Available);
        assert_eq!(outcome.region.as_deref(), Some("HK"));
        let territory = hbo_page(200, HBO_GEO_PAGE, &["countryCode=GU; Path=/"]);
        assert_eq!(territory.status, U::Available);
        assert_eq!(territory.region.as_deref(), Some("GU"));
        for status in [403, 406, 429, 451] {
            assert_eq!(
                hbo_page(status, HBO_GEO_PAGE, &cookies).status,
                U::Restricted
            );
        }
        assert_eq!(hbo_page(500, HBO_GEO_PAGE, &cookies).status, U::Failed);
        assert_eq!(
            hbo_page(
                200,
                &format!("<script>window._cf_chl_opt={{}};</script>{HBO_GEO_PAGE}"),
                &cookies
            )
            .status,
            U::Restricted
        );
    }

    #[test]
    fn hbo_max_does_not_accept_an_unsupported_country_or_a_partner_marketing_region() {
        // JP is present in brand.region, but has no direct-service landing locale.
        for region in ["KR", "CN", "JP"] {
            let outcome = hbo_page(
                200,
                HBO_GEO_PAGE,
                &[&format!("countryCode={region}; Path=/")],
            );
            assert_eq!(outcome.status, U::UnsupportedRegion, "{region}");
            assert_eq!(outcome.region.as_deref(), Some(region));
        }
    }

    #[test]
    fn hbo_max_requires_both_current_country_evidence_and_a_recognized_geo_configuration() {
        for cookies in [
            vec![],
            vec!["other=countryCode=HK"],
            vec!["countryCode=XX"],
            vec!["countryCode=ZZ"],
            vec!["countryCode=HKG"],
            vec!["countryCode=HK", "countryCode=invalid"],
        ] {
            let outcome = hbo_page(200, HBO_GEO_PAGE, &cookies);
            assert_eq!(outcome.status, U::Failed, "{cookies:?}");
            assert!(outcome.region.is_none());
        }
        for page in [
            "<title>HBO Max</title>".to_owned(),
            HBO_GEO_PAGE.replace("__DOTCOM_GEO__", "country-menu"),
            HBO_GEO_PAGE.replace("\"region\":{\"apac\":[\"hk\",\"jp\"],\"amer\":[\"us\",\"gu\"]}", "\"region\":{}"),
            HBO_GEO_PAGE.replace("\"pathname\":\"/\"", "\"pathname\":\"/privacy\""),
            HBO_GEO_PAGE.replace("\"canBeOutOfRegion\":false", "\"canBeOutOfRegion\":true"),
            HBO_GEO_PAGE.replace("\"availableLocales\":[\"us/en\",\"hk/en\",\"hk/zh\"]", "\"availableLocales\":[]"),
            HBO_GEO_PAGE.replace("\"urlCountry\":\"us\"", "\"windows\":{\"hk/zh\":[\"2099-01-01T00:00:00Z\",null]}"),
            format!("<script id='__DOTCOM_GEO__'>{}</script>", " ".repeat(65_537)),
            "<script id='__DOTCOM_GEO__'>{invalid}</script>{\"userCountry\":\"HK\",\"isUserOutOfRegion\":false,\"countryLangUris\":{}}".to_owned(),
        ] {
            assert_eq!(hbo_page(200, &page, &["countryCode=HK"]).status, U::Failed);
        }
    }
    #[test]
    fn dazn_uses_explicit_startup_eligibility() {
        let ok = check(
            S::Dazn,
            200,
            r#"{"GeolocatedCountry":"JP","isAllowed":true}"#,
            DAZN_URL,
        );
        assert_eq!(ok.status, U::Available);
        assert_eq!(ok.region.as_deref(), Some("JP"));
        assert_eq!(
            check(
                S::Dazn,
                200,
                r#"{"GeolocatedCountry":"CN","isAllowed":false}"#,
                DAZN_URL
            )
            .status,
            U::UnsupportedRegion
        );
        assert_eq!(
            check(S::Dazn, 403, "Security policy has been breached", DAZN_URL).status,
            U::Restricted
        );
    }
    #[test]
    fn bbc_checks_its_own_media_selector_and_does_not_guess_a_blocked_country() {
        let blocked = check(
            S::BbcIplayer,
            200,
            r#"/**/ JS_callbacks0({"result":"geolocation"});"#,
            BBC_URL,
        );
        assert_eq!(blocked.status, U::UnsupportedRegion);
        assert!(blocked.region.is_none());
        let ok = check(
            S::BbcIplayer,
            200,
            r#"JS_callbacks0({"media":[{"connection":[{"href":"https://as-hls-uk-live.akamaized.net/playlist.m3u8"}]}]});"#,
            BBC_URL,
        );
        assert_eq!(ok.status, U::Available);
        assert_eq!(ok.region.as_deref(), Some("GB"));
        assert_eq!(
            check(S::BbcIplayer, 200, "geolocation help text", BBC_URL).status,
            U::Failed
        );
    }
    #[test]
    fn crunchyroll_uses_its_anonymous_token_country_without_a_third_party_geo_test() {
        let ok = check(
            S::Crunchyroll,
            200,
            r#"{"country":"HK","token_type":"bearer","access_token":"test-only"}"#,
            CRUNCHYROLL_URL,
        );
        assert_eq!(ok.status, U::Available);
        assert_eq!(ok.region.as_deref(), Some("HK"));
        assert_eq!(
            check(S::Crunchyroll, 200, r#"{"country":"US"}"#, CRUNCHYROLL_URL).status,
            U::Failed
        );
        assert_eq!(
            check(S::Crunchyroll, 403, "Access denied", CRUNCHYROLL_URL).status,
            U::Restricted
        );
    }
    #[test]
    fn paramount_requires_an_application_page_and_keeps_intl_unavailable() {
        let ok = check(
            S::ParamountPlus,
            200,
            "<title>Paramount+</title><script>window.CBS={};</script>",
            "https://www.paramountplus.com/gb/",
        );
        assert_eq!(ok.status, U::Available);
        assert_eq!(ok.region.as_deref(), Some("GB"));
        let blocked = check(
            S::ParamountPlus,
            200,
            "Paramount+",
            "https://www.paramountplus.com/intl/",
        );
        assert_eq!(blocked.status, U::UnsupportedRegion);
        assert!(blocked.region.is_none());
        assert_eq!(
            check(
                S::ParamountPlus,
                200,
                "<title>Paramount+</title>",
                "https://www.paramountplus.com/"
            )
            .status,
            U::Failed
        );
        assert_eq!(
            check(
                S::ParamountPlus,
                406,
                "Not Acceptable",
                "https://www.paramountplus.com/"
            )
            .status,
            U::Restricted
        );
    }
}
