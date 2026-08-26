use std::collections::BTreeMap;

use url::Url;

/// One VLESS entry from a subscription, enough to assemble a temporary xray client config.
#[derive(Debug)]
pub(crate) struct VlessEntry {
    pub(crate) name: String,
    pub(crate) uuid: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) sni: String,
    pub(crate) fingerprint: String,
    pub(crate) public_key: String,
    pub(crate) short_id: String,
    pub(crate) flow: Option<String>,
}

impl VlessEntry {
    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let url = Url::parse(&line.replacen("vless://", "http://", 1))
            .map_err(|error| format!("invalid vless URI: {error}"))?;
        let uuid = url.username();
        if uuid.is_empty() {
            return Err("vless URI is missing uuid".to_owned());
        }
        let raw_host = url
            .host_str()
            .ok_or("vless URI is missing host")?
            .to_owned();
        let host = raw_host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(&raw_host)
            .to_owned();
        let port = url.port().ok_or("vless URI is missing port")?;
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        let security = query
            .get("security")
            .map(|value| value.as_ref())
            .unwrap_or("none");
        if security != "reality" {
            return Err(format!("unsupported subscription security {security}"));
        }
        let public_key = query
            .get("pbk")
            .filter(|value| !value.is_empty())
            .ok_or("REALITY subscription is missing pbk")?
            .to_string();
        Ok(Self {
            name: url.fragment().unwrap_or("").to_owned(),
            uuid: uuid.to_owned(),
            host,
            port,
            sni: query
                .get("sni")
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fingerprint: query
                .get("fp")
                .map(|value| value.to_string())
                .unwrap_or_else(|| "chrome".to_owned()),
            public_key,
            short_id: query
                .get("sid")
                .map(|value| value.to_string())
                .unwrap_or_default(),
            flow: query
                .get("flow")
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vless_entry_parse_requires_reality_subscription() {
        let entry = VlessEntry::parse(
            "vless://uuid-1@172.31.90.11:443?encryption=none&type=tcp&security=reality&sni=www.example.com&fp=chrome&pbk=pub&sid=abc#hk",
        )
        .unwrap();

        assert_eq!(entry.uuid, "uuid-1");
        assert_eq!(entry.host, "172.31.90.11");
        assert_eq!(entry.port, 443);
        assert_eq!(entry.sni, "www.example.com");
        assert_eq!(entry.public_key, "pub");
    }

    #[test]
    fn vless_entry_parse_accepts_bracketed_ipv6_host() {
        let entry = VlessEntry::parse(
            "vless://uuid-1@[fd42:31:90::11]:443?encryption=none&type=tcp&security=reality&sni=www.example.com&fp=chrome&pbk=pub&sid=abc#hk",
        )
        .unwrap();

        assert_eq!(entry.host, "fd42:31:90::11");
        assert_eq!(entry.port, 443);
    }

    #[test]
    fn vless_entry_parse_rejects_non_reality_security() {
        let error =
            VlessEntry::parse("vless://uuid-1@172.31.90.11:443?security=tls#hk").unwrap_err();

        assert!(error.contains("unsupported subscription security"));
    }

    #[test]
    fn vless_entry_parse_requires_a_reality_public_key() {
        let error =
            VlessEntry::parse("vless://uuid-1@172.31.90.11:443?security=reality#hk").unwrap_err();

        assert!(error.contains("missing pbk"));
    }
}
