use brocade_deployment::product::{ProductFlavor, ProductManifest};
use std::path::Path;

pub const FLAVOR: ProductFlavor = ProductFlavor::from_cvm(cfg!(feature = "cvm"));

pub fn validate_assets(directory: &Path) -> Result<(), String> {
    let path = directory.join("brocade-product.json");
    let metadata = std::fs::metadata(&path).map_err(|_| "前端缺少产品变体清单")?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err("前端产品变体清单无效".into());
    }
    let manifest: ProductManifest =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "无法读取前端产品变体清单")?)
            .map_err(|_| "前端产品变体清单无效")?;
    if manifest.flavor != FLAVOR || manifest.features.cvm != cfg!(feature = "cvm") {
        return Err("前端与 Console 产品变体不匹配".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_assets_require_matching_product_and_capabilities() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_assets(directory.path()).is_err());
        let path = directory.path().join("brocade-product.json");
        for (flavor, features, accepted) in [
            (FLAVOR, cfg!(feature = "cvm"), true),
            (
                ProductFlavor::from_cvm(!cfg!(feature = "cvm")),
                !cfg!(feature = "cvm"),
                false,
            ),
            (FLAVOR, !cfg!(feature = "cvm"), false),
        ] {
            std::fs::write(
                &path,
                serde_json::to_vec(&ProductManifest {
                    flavor,
                    features: brocade_deployment::product::ProductFeatures { cvm: features },
                })
                .unwrap(),
            )
            .unwrap();
            assert_eq!(validate_assets(directory.path()).is_ok(), accepted);
        }
        std::fs::write(&path, b"{}").unwrap();
        assert!(validate_assets(directory.path()).is_err());
    }
}
