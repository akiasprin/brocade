use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Libc {
    Gnu,
    Musl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Platform {
    pub(crate) arch: &'static str,
    pub(crate) libc: Libc,
}

impl Platform {
    pub(crate) fn current() -> Result<Self> {
        if std::env::consts::OS != "linux" {
            bail!(
                "brocade up 当前只支持 Linux，检测到 {}",
                std::env::consts::OS
            );
        }
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            other => bail!("brocade up 当前只支持 x86_64 和 aarch64，检测到 {other}"),
        };
        let libc = if cfg!(target_env = "musl") {
            Libc::Musl
        } else if cfg!(target_env = "gnu") {
            Libc::Gnu
        } else {
            bail!("当前 launcher 不是 GNU 或 musl Linux 构建")
        };
        Ok(Self { arch, libc })
    }

    pub(crate) fn postgres_artifact(self) -> &'static str {
        match (self.arch, self.libc) {
            ("x86_64", Libc::Gnu) => "embedded-postgres-binaries-linux-amd64",
            ("x86_64", Libc::Musl) => "embedded-postgres-binaries-linux-amd64-alpine",
            ("aarch64", Libc::Gnu) => "embedded-postgres-binaries-linux-arm64v8",
            ("aarch64", Libc::Musl) => "embedded-postgres-binaries-linux-arm64v8-alpine",
            _ => unreachable!("Platform::current validates the supported matrix"),
        }
    }

    pub(crate) fn cloudflared_asset(self) -> &'static str {
        match self.arch {
            "x86_64" => "cloudflared-linux-amd64",
            "aarch64" => "cloudflared-linux-arm64",
            _ => unreachable!("Platform::current validates the supported matrix"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_matrix_maps_to_upstream_asset_names() {
        for (arch, libc, postgres, cloudflared) in [
            (
                "x86_64",
                Libc::Gnu,
                "embedded-postgres-binaries-linux-amd64",
                "cloudflared-linux-amd64",
            ),
            (
                "x86_64",
                Libc::Musl,
                "embedded-postgres-binaries-linux-amd64-alpine",
                "cloudflared-linux-amd64",
            ),
            (
                "aarch64",
                Libc::Gnu,
                "embedded-postgres-binaries-linux-arm64v8",
                "cloudflared-linux-arm64",
            ),
            (
                "aarch64",
                Libc::Musl,
                "embedded-postgres-binaries-linux-arm64v8-alpine",
                "cloudflared-linux-arm64",
            ),
        ] {
            let platform = Platform { arch, libc };
            assert_eq!(platform.postgres_artifact(), postgres);
            assert_eq!(platform.cloudflared_asset(), cloudflared);
        }
    }
}
