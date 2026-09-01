//! Console-facing name and mark.
//!
//! Branding is deliberately outside the model settings path. It changes no generated artifact,
//! so saving it must neither stamp a revision nor ask the fleet to publish. The public read is
//! also intentional: the login page and the read-only public shell need the same identity before
//! an operator session exists.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

pub const DEFAULT_SITE_NAME: &str = "Brocade";
const MAX_SITE_NAME_CHARS: usize = 64;
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_ICON_DATA_URL_LEN: usize = 350_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandingSettings {
    pub site_name: String,
    /// A self-contained PNG, JPEG or WebP image. `None` selects the built-in woven mark.
    pub icon_data_url: Option<String>,
}

impl Default for BrandingSettings {
    fn default() -> Self {
        Self {
            site_name: DEFAULT_SITE_NAME.to_owned(),
            icon_data_url: None,
        }
    }
}

pub async fn load_branding(pool: &PgPool) -> Result<BrandingSettings> {
    let row =
        sqlx::query("SELECT site_name, site_icon_data_url FROM control_state WHERE id = TRUE")
            .fetch_one(pool)
            .await?;
    Ok(BrandingSettings {
        site_name: row.try_get("site_name")?,
        icon_data_url: row.try_get("site_icon_data_url")?,
    })
}

pub async fn update_branding(
    pool: &PgPool,
    actor: &AdminContext,
    settings: BrandingSettings,
) -> Result<BrandingSettings> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update console branding".to_owned(),
        ));
    }
    let settings = normalize(settings);
    validate(&settings)?;
    sqlx::query(
        "UPDATE control_state
            SET site_name = $1, site_icon_data_url = $2
          WHERE id = TRUE",
    )
    .bind(&settings.site_name)
    .bind(&settings.icon_data_url)
    .execute(pool)
    .await?;
    Ok(settings)
}

fn normalize(settings: BrandingSettings) -> BrandingSettings {
    let site_name = settings.site_name.trim();
    BrandingSettings {
        site_name: if site_name.is_empty() {
            DEFAULT_SITE_NAME.to_owned()
        } else {
            site_name.to_owned()
        },
        icon_data_url: settings
            .icon_data_url
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
    }
}

fn validate(settings: &BrandingSettings) -> Result<()> {
    let count = settings.site_name.chars().count();
    if count == 0 || count > MAX_SITE_NAME_CHARS {
        return Err(StoreError::InvalidData(format!(
            "站点名称必须为 1–{MAX_SITE_NAME_CHARS} 个字符"
        )));
    }
    if settings.site_name.chars().any(char::is_control) {
        return Err(StoreError::InvalidData(
            "站点名称不能包含控制字符".to_owned(),
        ));
    }
    let Some(url) = &settings.icon_data_url else {
        return Ok(());
    };
    if url.len() > MAX_ICON_DATA_URL_LEN {
        return Err(StoreError::InvalidData(
            "站点图标不能超过 256 KiB".to_owned(),
        ));
    }
    let (mime, encoded) = url
        .strip_prefix("data:")
        .and_then(|value| value.split_once(";base64,"))
        .ok_or_else(|| StoreError::InvalidData("站点图标必须是 base64 图片".to_owned()))?;
    if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return Err(StoreError::InvalidData(
            "站点图标只支持 PNG、JPEG 或 WebP".to_owned(),
        ));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| StoreError::InvalidData("站点图标的 base64 内容无效".to_owned()))?;
    if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
        return Err(StoreError::InvalidData(
            "站点图标不能超过 256 KiB".to_owned(),
        ));
    }
    let signature_matches = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !signature_matches {
        return Err(StoreError::InvalidData(
            "站点图标内容与声明的图片格式不符".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_name_and_icon_restore_the_defaults() {
        assert_eq!(
            normalize(BrandingSettings {
                site_name: "   ".to_owned(),
                icon_data_url: Some(String::new()),
            }),
            BrandingSettings::default()
        );
    }

    #[test]
    fn supported_image_signature_is_required() {
        let valid = BrandingSettings {
            site_name: "织网".to_owned(),
            icon_data_url: Some("data:image/png;base64,iVBORw0KGgo=".to_owned()),
        };
        assert!(validate(&valid).is_ok());

        let disguised = BrandingSettings {
            icon_data_url: Some("data:image/png;base64,aGVsbG8=".to_owned()),
            ..valid
        };
        assert!(validate(&disguised).is_err());
    }

    #[test]
    fn svg_and_remote_urls_are_not_accepted() {
        for icon in [
            "https://example.com/icon.png",
            "data:image/svg+xml;base64,PHN2Zy8+",
        ] {
            let settings = BrandingSettings {
                site_name: DEFAULT_SITE_NAME.to_owned(),
                icon_data_url: Some(icon.to_owned()),
            };
            assert!(validate(&settings).is_err());
        }
    }
}
