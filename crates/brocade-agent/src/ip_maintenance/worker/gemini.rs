//! Anonymous location submission; never evidence of the exit IP's country.
use super::{
    fetch_request, now, Context, FetchOptions, IpMaintenancePolicy, MaintenanceModule, Page,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Url;
use reqwest_cookie_store::CookieStoreMutex;
use serde_json::{json, Value};
use std::sync::Arc;

const RPC_ID: &str = "K4WWud";
const INVALID_RESPONSE: &str = "gemini_location_response_invalid";
const UNCONFIRMED: &str = "gemini_location_unconfirmed";

fn location_cookie(
    latitude: f64,
    longitude: f64,
    timestamp: i64,
    secure: bool,
) -> Result<String, String> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || timestamp < 0
    {
        return Err("gemini_location_invalid_coordinates".into());
    }
    let timestamp = timestamp.checked_mul(1_000_000).ok_or("clock_range")?;
    // Gemini's current frontend writes a base64 JSON-proto UULE. E7 coordinates,
    // provenance 6 and the accuracy conversion follow that writer. The supplied
    // city coordinates are synthetic maintenance input, not device observations.
    let payload = json!([
        1,
        12,
        timestamp.to_string(),
        null,
        [
            (latitude * 1e7).round() as i32,
            (longitude * 1e7).round() as i32
        ],
        null,
        620_000,
        null,
        6
    ]);
    let encoded =
        STANDARD.encode(serde_json::to_vec(&payload).map_err(|_| "gemini_location_encode")?);
    Ok(format!(
        "UULE=j+{encoded}; Path=/; Max-Age=21600{}",
        if secure { "; Secure" } else { "" }
    ))
}

fn request_body() -> String {
    json!([[[RPC_ID, "[null,[\"en\"]]", null, "generic"]]]).to_string()
}

fn validate_response(body: &str) -> Result<(), String> {
    if body.len() > 64 * 1024 {
        return Err(INVALID_RESPONSE.into());
    }
    let mut result = None;
    // batchexecute uses an XSSI prefix, decimal chunk lengths and JSON frame lines.
    // Only the matching RPC payload is authoritative; never search arbitrary text.
    for line in body
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('['))
    {
        let frames: Value = serde_json::from_str(line).map_err(|_| INVALID_RESPONSE)?;
        for frame in frames.as_array().ok_or(INVALID_RESPONSE)? {
            if frame.get(0).and_then(Value::as_str) != Some("wrb.fr")
                || frame.get(1).and_then(Value::as_str) != Some(RPC_ID)
            {
                continue;
            }
            if result.is_some() {
                return Err(INVALID_RESPONSE.into());
            }
            let payload: Value = serde_json::from_str(
                frame
                    .get(2)
                    .and_then(Value::as_str)
                    .ok_or(INVALID_RESPONSE)?,
            )
            .map_err(|_| INVALID_RESPONSE)?;
            let location = payload
                .get(0)
                .and_then(Value::as_array)
                .ok_or(INVALID_RESPONSE)?;
            let name = location
                .first()
                .and_then(Value::as_str)
                .ok_or(INVALID_RESPONSE)?;
            if name.trim().is_empty() || name.len() > 512 {
                return Err(INVALID_RESPONSE.into());
            }
            result = Some(
                location.get(1).and_then(Value::as_str)
                    == Some("SWML_DESCRIPTION_FROM_YOUR_DEVICE")
                    && location.get(2).and_then(Value::as_bool) == Some(true),
            );
        }
    }
    match result {
        Some(true) => Ok(()),
        Some(false) => Err(UNCONFIRMED.into()),
        None => Err(INVALID_RESPONSE.into()),
    }
}

pub(super) async fn update(
    context: &Context,
    policy: &IpMaintenancePolicy,
    ua: &str,
    jar: Arc<CookieStoreMutex>,
    target: &Url,
    latitude: f64,
    longitude: f64,
) -> Result<Page, String> {
    let cookie = location_cookie(latitude, longitude, now()?, target.scheme() == "https")?;
    // No Domain attribute: this must not become a google.com-wide location cookie.
    jar.lock()
        .map_err(|_| "cookie_lock")?
        .parse(&cookie, target)
        .map_err(|_| "gemini_location_encode")?;
    let body = request_body();
    let page = fetch_request(
        context,
        policy,
        target.clone(),
        FetchOptions {
            ua,
            jar,
            referer: Some("https://gemini.google.com/app?hl=en"),
            module: MaintenanceModule::Google,
            timeout_secs: policy.behavior.request_timeout_secs,
            youtube_probe: false,
        },
        Some(&body),
    )
    .await?;
    if (200..300).contains(&page.status) {
        validate_response(&page.body)?;
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::cookie::CookieStore as _;

    #[tokio::test]
    #[ignore = "contacts Gemini with synthetic public-city coordinates; requires BROCADE_GEMINI_LOCATION_LIVE_FAMILY=v4 or v6"]
    async fn live_anonymous_location_update() {
        let family = match std::env::var("BROCADE_GEMINI_LOCATION_LIVE_FAMILY").as_deref() {
            Ok("v4") => brocade_deployment::ip_maintenance::MaintenanceFamily::V4,
            Ok("v6") => brocade_deployment::ip_maintenance::MaintenanceFamily::V6,
            _ => panic!("select v4 or v6 explicitly for this external test"),
        };
        let fixture = crate::ip_maintenance::tests::Fixture::new();
        let policy = IpMaintenancePolicy {
            family,
            ..Default::default()
        };
        let region = brocade_deployment::ip_maintenance::data::region("JP/Default/Tokyo").unwrap();
        let device = crate::ip_maintenance::behavior::DeviceSession {
            region,
            behavior: &policy.behavior,
            device: "linux",
            latitude: 35.681236,
            longitude: 139.767125,
        };
        let action = crate::ip_maintenance::behavior::scheduled_action(
            MaintenanceModule::Google,
            &device,
            0,
            1,
            &mut crate::ip_maintenance::behavior::Random,
        )
        .unwrap();
        let page = update(
            &fixture.0,
            &policy,
            super::super::google::USER_AGENT,
            Arc::new(CookieStoreMutex::default()),
            &action.url,
            device.latitude,
            device.longitude,
        )
        .await
        .unwrap();
        assert!((200..300).contains(&page.status));
        assert_eq!(fixture.0.locked().unwrap().daily_requests, 1);
        println!(
            "gemini-location-live-confirmed family={family:?} status={} device_location=true",
            page.status
        );
    }

    fn response(name: &str, source: &str, device: bool) -> String {
        format!(
            ")]}}'\n\n500\n{}\n",
            json!([
                [
                    "wrb.fr",
                    RPC_ID,
                    json!([[name, source, device]]).to_string(),
                    null,
                    null,
                    null,
                    "generic"
                ],
                ["di", 27]
            ])
        )
    }

    #[test]
    fn coordinates_are_signed_e7_with_a_bounded_host_only_cookie() {
        let target = Url::parse("https://gemini.google.com/").unwrap();
        let jar = CookieStoreMutex::default();
        let cookie = location_cookie(40.758, -73.9855, 1_794_995_065, true).unwrap();
        assert!(cookie.contains("Max-Age=21600; Secure"));
        assert!(!cookie.contains("Domain="));
        jar.lock().unwrap().parse(&cookie, &target).unwrap();
        let header = jar.cookies(&target).unwrap();
        let encoded = header.to_str().unwrap().strip_prefix("UULE=j+").unwrap();
        let data: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(data[4], json!([407580000, -739855000]));
        assert_eq!(data[2], "1794995065000000");
        assert_eq!(data[8], 6);
        for url in [
            "https://www.google.com/",
            "https://www.youtube.com/",
            "https://gemini.google.com.example.org/",
            "http://gemini.google.com/",
        ] {
            assert!(jar.cookies(&Url::parse(url).unwrap()).is_none());
        }
        assert!(location_cookie(0.0, 0.0, 1, true).is_ok());
        for (latitude, longitude) in [
            (91.0, 0.0),
            (0.0, -181.0),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
        ] {
            assert_eq!(
                location_cookie(latitude, longitude, 1, true).unwrap_err(),
                "gemini_location_invalid_coordinates"
            );
        }
    }

    #[test]
    fn only_the_matching_server_device_location_payload_is_successful() {
        let valid = response("Tokyo, 千代田区", "SWML_DESCRIPTION_FROM_YOUR_DEVICE", true);
        validate_response(&valid).unwrap();
        for invalid in [
            valid.replace(RPC_ID, "other-rpc"),
            format!("{valid}{valid}"),
            "<html>Tokyo SWML_DESCRIPTION_FROM_YOUR_DEVICE true</html>".into(),
            "[[\"wrb.fr\",\"K4WWud\",null]]".into(),
            "[invalid JSON]".into(),
            " ".repeat(64 * 1024 + 1),
            response("", "SWML_DESCRIPTION_FROM_YOUR_DEVICE", true),
        ] {
            assert_eq!(validate_response(&invalid).unwrap_err(), INVALID_RESPONSE);
        }
        for (source, device) in [
            ("SWML_DESCRIPTION_FROM_YOUR_INTERNET_ADDRESS", false),
            ("SWML_DESCRIPTION_FROM_YOUR_DEVICE", false),
            ("unknown", true),
        ] {
            assert_eq!(
                validate_response(&response("Tokyo", source, device)).unwrap_err(),
                UNCONFIRMED
            );
        }
        let request: Value = serde_json::from_str(&request_body()).unwrap();
        assert_eq!(request[0][0][0], RPC_ID);
        assert_eq!(
            serde_json::from_str::<Value>(request[0][0][1].as_str().unwrap()).unwrap(),
            json!([null, ["en"]])
        );
    }
}
