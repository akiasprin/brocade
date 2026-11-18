//! Shadow comparison and the fixed native binding boundary.
use crate::{backend::Inner, records::Record, Result};
use brocade_cvm_compile::{bind, canonical, compile, digest, verify, LocalBindings};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};

impl Inner {
    pub(crate) fn artifact_receipt(
        &self,
        desired: &InstanceDesired,
    ) -> Result<Option<CvmArtifactReceipt>> {
        // Cleanup must remain possible after a template/compiler upgrade. It uses
        // durable local ownership, never untrusted creation products.
        if desired.power != PowerState::Running {
            return Ok(None);
        }
        let Some(bundle) = &desired.artifacts else {
            if desired.artifact_mode == CvmArtifactMode::Enforce {
                return Err("enforced Cube artifacts missing".into());
            }
            return Ok(None);
        };
        let verified = verify(bundle, desired).is_ok();
        let image = desired.spec.image.as_ref().ok_or("Cube image missing")?;
        let local_facts = self.cfg.configured_capabilities();
        let manifest = if desired.artifact_mode == CvmArtifactMode::Enforce {
            Some(&bundle.input.image_manifest)
        } else {
            self.cfg
                .manifests
                .get(&image.id)
                .filter(|manifest| manifest.sha256 == image.sha256)
        };
        let compiled = match manifest {
            Some(local) => {
                let mut facts = bundle.input.capabilities.clone();
                facts.cube_storage_backend = local_facts.cube_storage_backend;
                facts.stable_storage = local_facts.stable_storage;
                if desired.artifact_mode == CvmArtifactMode::Shadow {
                    facts.resource_limits = local_facts.resource_limits;
                    facts.images = local_facts.images;
                    if !facts.images.contains(image) {
                        facts.images.clear();
                    }
                }
                compile(&CvmArtifactInput {
                    instance_id: desired.id.clone(),
                    spec: desired.spec.clone(),
                    image_manifest: local.clone(),
                    capabilities: facts,
                })
            }
            None => Err("local verified Cube manifest missing".into()),
        };
        let kinds = [
            CvmArtifactKind::Sandbox,
            CvmArtifactKind::Egress,
            CvmArtifactKind::Attachments,
            CvmArtifactKind::GuestInit,
        ];
        let mismatches = kinds
            .into_iter()
            .filter(|kind| {
                !verified
                    || compiled.as_ref().is_err()
                    || compiled.as_ref().ok().and_then(|b| b.file(*kind).ok())
                        != bundle.file(*kind).ok()
            })
            .collect::<Vec<_>>();
        let files = kinds
            .into_iter()
            .filter_map(|kind| bundle.file(kind).ok())
            .map(|f| CvmArtifactApplied {
                kind: f.kind,
                compiled_sha256: f.sha256.clone(),
                effective_sha256: None,
                bindings: json!({}),
            })
            .collect();
        Ok(Some(CvmArtifactReceipt {
            compiler: bundle.compiler.clone(),
            input_sha256: bundle.input_sha256.clone(),
            mode: desired.artifact_mode,
            mismatches,
            files,
        }))
    }
    pub(crate) fn artifact_value(
        &self,
        desired: &InstanceDesired,
        kind: CvmArtifactKind,
        local: &LocalBindings,
    ) -> Result<Option<Value>> {
        if desired.power != PowerState::Running || desired.artifact_mode != CvmArtifactMode::Enforce
        {
            return Ok(None);
        }
        let bundle = desired
            .artifacts
            .as_ref()
            .ok_or("enforced Cube artifacts missing")?;
        bind(bundle, kind, desired, local).map(Some)
    }
    /// Acknowledged execution only. Historical sandbox identity survives hot route changes.
    pub(crate) fn complete_artifact_receipt(
        &self,
        record: &Record,
        desired: &InstanceDesired,
        result: &mut InstanceReport,
        mut receipt: Option<CvmArtifactReceipt>,
    ) -> Result<()> {
        if let Some(receipt) = &mut receipt {
            if result.status == ApplyStatus::Applied && result.power == Some(PowerState::Running) {
                for file in &mut receipt.files {
                    if receipt.mismatches.contains(&file.kind) {
                        continue;
                    }
                    if file.kind == CvmArtifactKind::Sandbox {
                        if record.sandbox_sha256 == file.compiled_sha256
                            && !record.sandbox_effective_sha256.is_empty()
                        {
                            file.effective_sha256 = Some(record.sandbox_effective_sha256.clone());
                            file.bindings = record.sandbox_bindings.clone();
                        }
                    } else if let Some(bundle) = &desired.artifacts {
                        let bindings = LocalBindings {
                            runtime_id: Some(record.runtime.clone()),
                            ..Default::default()
                        };
                        let value = bind(bundle, file.kind, desired, &bindings)?;
                        file.effective_sha256 = Some(digest(&value)?);
                        if file.kind == CvmArtifactKind::Attachments {
                            file.bindings = json!({"runtime_id":record.runtime,"generation":desired.generation,"routing_revision":desired.spec.routing.revision,"detach":desired.detached_interfaces});
                        }
                    }
                }
            }
        }
        result.artifacts = receipt;
        Ok(())
    }
}

pub(crate) fn record_sandbox(
    record: &mut Record,
    desired: &InstanceDesired,
    request: &Value,
) -> Result<()> {
    record.sandbox_sha256.clear();
    record.sandbox_effective_sha256.clear();
    record.sandbox_bindings = json!({});
    if let Some(bundle) = &desired.artifacts {
        if desired.artifact_mode == CvmArtifactMode::Enforce {
            record.sandbox_sha256 = bundle.file(CvmArtifactKind::Sandbox)?.sha256.clone();
            record.sandbox_effective_sha256 =
                brocade_core::hash::sha256_hex(canonical(request)?.as_bytes());
            record.sandbox_bindings = json!({"expected_root":request["annotations"]["brocade.cvm.expected_root"],"shim_version":request["annotations"]["cube.master.components.cube-shim.version"],"spec_sha256":desired.spec_sha256,"egress_sha256":bundle.file(CvmArtifactKind::Egress)?.sha256});
        }
    }
    Ok(())
}
