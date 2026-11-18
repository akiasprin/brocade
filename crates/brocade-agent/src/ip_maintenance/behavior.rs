//! Maintenance action generation; no networking, persistence or control-plane access.
use brocade_deployment::ip_maintenance::{
    data::MaintenanceRegion, MaintenanceBehavior, MaintenanceModule, NumberRange,
};
use reqwest::Url;

pub(super) struct Random;
impl Random {
    pub fn draw(&mut self, upper: u32) -> Result<u32, String> {
        if upper == 0 {
            return Err("invalid_random_range".into());
        }
        let mut bytes = [0; 2];
        getrandom::fill(&mut bytes).map_err(|_| "random_source")?;
        // The reference uses Bash RANDOM (15 bits) followed by modulo.
        Ok(u32::from(u16::from_le_bytes(bytes) & 32767) % upper)
    }
    pub fn between(&mut self, range: &NumberRange) -> Result<u32, String> {
        Ok(range.min + self.draw(range.max - range.min + 1)?)
    }
}
pub(super) fn cksum(input: &[u8]) -> u32 {
    fn byte(mut crc: u32, value: u8) -> u32 {
        crc ^= u32::from(value) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
        crc
    }
    let mut crc = input.iter().fold(0, |crc, b| byte(crc, *b));
    let mut length = input.len();
    while length > 0 {
        crc = byte(crc, length as u8);
        length >>= 8;
    }
    !crc
}
pub(super) fn persona_indexes(ip: &str, length: usize) -> [usize; 3] {
    let seed = u64::from(cksum(ip.as_bytes()));
    [1, 17, 31].map(|factor| (seed * factor % length as u64) as usize)
}
pub(super) fn platform(ua: &str) -> &'static str {
    if ua.contains("Android") {
        "android"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        "ios"
    } else if ua.contains("Macintosh") {
        "macos"
    } else if ua.contains("Linux") {
        "linux"
    } else {
        "windows"
    }
}
pub(super) fn weighted(weights: &[u8], roll: u32) -> usize {
    let mut end = 0;
    for (index, weight) in weights.iter().enumerate() {
        end += u32::from(*weight);
        if roll < end {
            return index;
        }
    }
    weights.len() - 1
}
#[derive(Clone, Copy)]
pub(super) enum ActionRequest {
    Get,
    GeminiLocation { latitude: f64, longitude: f64 },
}
pub(super) struct Action {
    pub kind: &'static str,
    pub url: Url,
    pub dwell_secs: u32,
    pub request: ActionRequest,
}
pub(super) struct DeviceSession<'a> {
    pub region: &'a MaintenanceRegion,
    pub behavior: &'a MaintenanceBehavior,
    pub device: &'a str,
    pub latitude: f64,
    pub longitude: f64,
}
pub(super) fn scheduled_action(
    module: MaintenanceModule,
    session: &DeviceSession<'_>,
    index: u32,
    total: u32,
    random: &mut Random,
) -> Result<Action, String> {
    if index >= total {
        return Err("invalid_action_index".into());
    }
    if module == MaintenanceModule::Google && index == 0 {
        return Ok(Action {
            kind: "gemini_location",
            url: Url::parse("https://gemini.google.com/_/BardChatUi/data/batchexecute?rpcids=K4WWud&source-path=%2Fapp&hl=en&rt=c")
                .map_err(|_| "static_url")?,
            dwell_secs: 0,
            request: ActionRequest::GeminiLocation {
                latitude: session.latitude,
                longitude: session.longitude,
            },
        });
    }
    action(module, session, index + 1 == total, random)
}
pub(super) fn action(
    module: MaintenanceModule,
    session: &DeviceSession<'_>,
    last: bool,
    random: &mut Random,
) -> Result<Action, String> {
    let region = session.region;
    let behavior = session.behavior;
    let device = session.device;
    let latitude = session.latitude;
    let longitude = session.longitude;
    let mut url;
    let kind;
    let dwell_secs;
    if module == MaintenanceModule::Trust {
        let text = region
            .sites
            .get(random.draw(region.sites.len() as u32)? as usize)
            .ok_or("empty_site_catalog")?;
        url = Url::parse(text).map_err(|_| "invalid_catalog_url")?;
        // RSS article links now need a JavaScript decoder before reaching the
        // publisher. Prefer the same catalog's direct regional sites for HTTP visits.
        if is_news_redirect(&url) && !region.static_sites.is_empty() {
            let target =
                &region.static_sites[random.draw(region.static_sites.len() as u32)? as usize];
            url = Url::parse(target).map_err(|_| "invalid_catalog_url")?;
        }
        kind = "local_site";
        let index = weighted(
            &behavior
                .trust_dwell
                .iter()
                .map(|b| b.weight)
                .collect::<Vec<_>>(),
            random.draw(100)?,
        );
        dwell_secs = if last {
            0
        } else {
            random.between(&behavior.trust_dwell[index].seconds)?
        };
    } else {
        let desktop = [
            behavior.desktop_actions[0],
            behavior.desktop_actions[1],
            behavior.desktop_actions[3],
            behavior.desktop_actions[2],
        ];
        let weights = match device {
            "android" => &behavior.android_actions,
            "ios" | "macos" => &behavior.apple_actions,
            _ => &desktop,
        };
        let selected = weighted(weights, random.draw(100)?);
        let category = if !matches!(device, "android" | "ios" | "macos") && selected >= 2 {
            5 - selected
        } else {
            selected
        };
        let keyword = region
            .keywords
            .get(random.draw(region.keywords.len() as u32)? as usize)
            .ok_or("empty_keywords")?;
        match category {
            0 => {
                kind = "google_search";
                url = Url::parse("https://www.google.com/search").map_err(|_| "static_url")?;
                url.query_pairs_mut().append_pair("q", keyword);
            }
            1 => {
                kind = "google_news";
                url = Url::parse("https://news.google.com/home").map_err(|_| "static_url")?;
            }
            2 => {
                kind = "google_maps";
                url =
                    Url::parse("https://www.google.com/maps/search/").map_err(|_| "static_url")?;
                let lat = latitude + (f64::from(random.draw(2)?) - 1.0) / 10000.0;
                let lon = longitude + (f64::from(random.draw(2)?) - 1.0) / 10000.0;
                url.path_segments_mut()
                    .map_err(|_| "static_url")?
                    .pop_if_empty()
                    .push(keyword)
                    .push(&format!("@{lat},{lon},17z"));
            }
            _ => {
                kind = "platform_page";
                let target = match device {
                    "android" => "https://connectivitycheck.gstatic.com/generate_204",
                    "ios" | "macos" => "https://captive.apple.com/hotspot-detect.html",
                    _ => [
                        "https://about.google/",
                        "https://safety.google/",
                        "https://support.google.com/?hl=",
                    ][random.draw(3)? as usize],
                };
                url = Url::parse(target).map_err(|_| "static_url")?;
            }
        }
        if category < 3 {
            for parameter in region.language_params.split('&') {
                if let Some((key, value)) = parameter.split_once('=') {
                    url.query_pairs_mut().append_pair(key, value);
                }
            }
        }
        dwell_secs = if last {
            0
        } else {
            random.between(&behavior.google_dwell_secs)?
        };
    }
    Ok(Action {
        kind,
        url,
        dwell_secs,
        request: ActionRequest::Get,
    })
}

pub(super) fn is_news_redirect(url: &Url) -> bool {
    url.host_str() == Some("news.google.com")
        && (url.path().starts_with("/rss/articles/") || url.path().starts_with("/articles/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_round_reserves_one_existing_slot_for_gemini_and_trust_never_submits_location() {
        let region = brocade_deployment::ip_maintenance::data::region("JP/Default/Tokyo").unwrap();
        let behavior = MaintenanceBehavior::default();
        let session = DeviceSession {
            region,
            behavior: &behavior,
            device: "linux",
            latitude: 35.681236,
            longitude: 139.767125,
        };
        for total in [1, 5, 20] {
            let planned = (0..total)
                .map(|index| {
                    scheduled_action(
                        MaintenanceModule::Google,
                        &session,
                        index,
                        total,
                        &mut Random,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(planned.len(), total as usize);
            assert_eq!(planned[0].kind, "gemini_location");
            assert_eq!(planned[0].url.host_str(), Some("gemini.google.com"));
            assert_eq!(planned[0].dwell_secs, 0);
            assert!(
                matches!(planned[0].request, ActionRequest::GeminiLocation { latitude, longitude } if latitude == session.latitude && longitude == session.longitude)
            );
            assert!(planned[1..]
                .iter()
                .all(|a| matches!(a.request, ActionRequest::Get)));
            assert_eq!(planned.last().unwrap().dwell_secs, 0);
        }
        assert!(matches!(
            scheduled_action(MaintenanceModule::Trust, &session, 0, 1, &mut Random)
                .unwrap()
                .request,
            ActionRequest::Get
        ));
        assert!(scheduled_action(MaintenanceModule::Google, &session, 1, 1, &mut Random).is_err());
    }

    #[test]
    fn regional_http_visits_use_direct_catalog_sites_instead_of_javascript_rss_hops() {
        let mut region = brocade_deployment::ip_maintenance::data::region("DE/Default/Frankfurt")
            .unwrap()
            .clone();
        region.sites = vec!["https://news.google.com/rss/articles/fixture".into()];
        region.static_sites = vec!["https://www.bundesregierung.de/".into()];
        let behavior = MaintenanceBehavior::default();
        let session = DeviceSession {
            region: &region,
            behavior: &behavior,
            device: "windows",
            latitude: region.latitude,
            longitude: region.longitude,
        };
        let target = action(MaintenanceModule::Trust, &session, true, &mut Random).unwrap();
        assert_eq!(target.url.as_str(), "https://www.bundesregierung.de/");
        assert_eq!(target.kind, "local_site");
        assert_eq!(target.dwell_secs, 0);
    }
    #[test]
    fn probabilities_have_exact_platform_boundaries() {
        let b = MaintenanceBehavior::default();
        assert_eq!(weighted(&b.android_actions, 24), 0);
        assert_eq!(weighted(&b.android_actions, 25), 1);
        assert_eq!(weighted(&b.android_actions, 54), 1);
        assert_eq!(weighted(&b.android_actions, 55), 2);
        assert_eq!(weighted(&b.desktop_actions, 79), 2);
        assert_eq!(weighted(&b.desktop_actions, 80), 3);
    }
    #[test]
    fn posix_checksum_uses_length_and_three_stable_devices() {
        assert_eq!(cksum(b""), 4_294_967_295);
        // POSIX cksum reference vectors, including an IPv6 address.
        assert_eq!(cksum(b"192.0.2.25"), 880_073_966);
        assert_eq!(persona_indexes("192.0.2.25", 4000), [1966, 1422, 946]);
        assert_eq!(persona_indexes("2001:db8::25", 4000), [2543, 3231, 2833]);
        assert_ne!(cksum(b"192.0.2.25"), cksum(b"192.0.2.25\n"));
    }
    #[test]
    fn every_platform_produces_the_expected_targets_and_never_dwells_after_the_last_action() {
        let mut region = brocade_deployment::ip_maintenance::data::region("JP/Default/Tokyo")
            .unwrap()
            .clone();
        region.keywords = vec!["東京 coffee & maps".into()];
        region.sites = vec!["https://www.japan.go.jp/".into()];
        for device in ["android", "ios", "macos", "windows", "linux"] {
            for category in 0..4 {
                let mut weights = [0; 4];
                weights[category] = 100;
                let behavior = MaintenanceBehavior {
                    android_actions: weights,
                    apple_actions: weights,
                    desktop_actions: weights,
                    google_dwell_secs: NumberRange { min: 47, max: 47 },
                    ..Default::default()
                };
                let session = DeviceSession {
                    region: &region,
                    behavior: &behavior,
                    device,
                    latitude: region.latitude,
                    longitude: region.longitude,
                };
                let page = action(MaintenanceModule::Google, &session, false, &mut Random).unwrap();
                assert_eq!(page.dwell_secs, 47);
                let query = page
                    .url
                    .query_pairs()
                    .collect::<std::collections::BTreeMap<_, _>>();
                if category < 3 {
                    assert_eq!(query.get("hl").map(|s| s.as_ref()), Some("ja"));
                    assert_eq!(query.get("gl").map(|s| s.as_ref()), Some("JP"));
                }
                match category {
                    0 => {
                        assert_eq!(page.url.path(), "/search");
                        assert_eq!(
                            query.get("q").map(|s| s.as_ref()),
                            Some("東京 coffee & maps")
                        );
                    }
                    1 => assert_eq!(page.url.host_str(), Some("news.google.com")),
                    2 => assert!(page.url.path().starts_with("/maps/search/")),
                    _ => match device {
                        "android" => {
                            assert_eq!(page.url.host_str(), Some("connectivitycheck.gstatic.com"))
                        }
                        "ios" | "macos" => {
                            assert_eq!(page.url.host_str(), Some("captive.apple.com"))
                        }
                        _ => assert!(["about.google", "safety.google", "support.google.com"]
                            .contains(&page.url.host_str().unwrap())),
                    },
                }
                assert_eq!(
                    action(MaintenanceModule::Google, &session, true, &mut Random)
                        .unwrap()
                        .dwell_secs,
                    0
                );
                let trust = action(MaintenanceModule::Trust, &session, true, &mut Random).unwrap();
                assert_eq!(trust.url.as_str(), "https://www.japan.go.jp/");
                assert_eq!(trust.dwell_secs, 0);
            }
        }
    }
}
