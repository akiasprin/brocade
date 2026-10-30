use std::{error::Error, io, time::Instant};

/// Log categories and OS codes, never Error::Display/source text: those can contain a full URL,
/// a third-party API key, an invalid bearer header or server-controlled response material.
pub(super) fn category(error: &(dyn Error + 'static)) -> String {
    let mut category = "transport";
    let mut errno = None;
    let mut source = Some(error);
    for _ in 0..16 {
        let Some(error) = source else {
            break;
        };
        if let Some(error) = error.downcast_ref::<reqwest::Error>() {
            category = if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connect"
            } else if error.is_builder() {
                "builder"
            } else if error.is_body() {
                "body"
            } else if error.is_decode() {
                "decode"
            } else {
                "request"
            };
        }
        if let Some(error) = error.downcast_ref::<io::Error>() {
            errno = error.raw_os_error().or(errno);
            if error
                .get_ref()
                .is_some_and(|cause| cause.is::<super::resolver::NoIpv6Addresses>())
            {
                category = "dns_no_ipv6";
            } else if error.kind() == io::ErrorKind::TimedOut {
                category = "timeout";
            } else if error.kind() == io::ErrorKind::WouldBlock {
                category = "capacity";
            }
        }
        source = error.source();
    }
    match errno {
        Some(errno) => format!("{category} os_errno={errno}"),
        None => category.to_owned(),
    }
}
pub(super) struct Progress {
    started: Instant,
    pub(super) status: Option<u16>,
    pub(super) content_length: Option<u64>,
    pub(super) received_bytes: usize,
}
impl Progress {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            status: None,
            content_length: None,
            received_bytes: 0,
        }
    }
    pub(super) fn error(&self, phase: &str, error: &(dyn Error + 'static)) -> String {
        self.message(phase, &category(error))
    }
    pub(super) fn message(&self, phase: &str, kind: &str) -> String {
        format!("HTTP phase={phase} kind={kind} status={} received_bytes={} content_length={} elapsed_ms={}",
            self.status.map_or_else(|| "unknown".to_owned(), |status| status.to_string()),
            self.received_bytes,
            self.content_length.map_or_else(|| "unknown".to_owned(), |length| length.to_string()),
            self.started.elapsed().as_millis())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_error_text_cannot_expose_credentials() {
        let error = io::Error::other("https://secret.example/?key=private Bearer sensitive-token");
        let text = Progress::new(Instant::now()).error("body", &error);
        for secret in ["secret.example", "private", "sensitive-token"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("phase=body"));
        assert!(text.contains("received_bytes=0"));
        assert!(category(&io::Error::from_raw_os_error(libc::EMFILE))
            .contains(&format!("os_errno={}", libc::EMFILE)));
    }

    #[test]
    fn confirmed_ipv6_address_absence_has_a_distinct_safe_category() {
        let absent = io::Error::new(
            io::ErrorKind::NotFound,
            super::super::resolver::NoIpv6Addresses,
        );
        assert_eq!(category(&absent), "dns_no_ipv6");
        assert!(Progress::new(Instant::now())
            .error("send", &absent)
            .contains("kind=dns_no_ipv6 "));
        for error in [
            io::Error::new(io::ErrorKind::NotFound, "ordinary lookup failure"),
            io::Error::new(io::ErrorKind::WouldBlock, "DNS capacity exhausted"),
            io::Error::new(io::ErrorKind::TimedOut, "DNS lookup did not complete"),
        ] {
            assert_ne!(category(&error), "dns_no_ipv6");
        }
    }

    #[test]
    fn ipv6_address_absence_survives_the_reqwest_error_chain() {
        struct NoIpv6;
        impl reqwest::dns::Resolve for NoIpv6 {
            fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                Box::pin(async {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        super::super::resolver::NoIpv6Addresses,
                    )
                    .into())
                })
            }
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(NoIpv6))
            .build()
            .unwrap();
        let error = client
            .get("http://no-ipv6.example.invalid/")
            .send()
            .unwrap_err();
        assert_eq!(category(&error), "dns_no_ipv6");
        assert!(Progress::new(Instant::now())
            .error("send", &error)
            .contains("kind=dns_no_ipv6 "));
    }
}
