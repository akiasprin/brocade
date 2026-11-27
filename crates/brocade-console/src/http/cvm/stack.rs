//! Private discovery and authenticated delivery for Cube stack maintenance.
use super::*;
use brocade_deployment::cvm::stack::*;
use brocade_store::cvm::stack::VerifiedStackPair;
use std::path::{Path as FsPath, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncReadExt;

const MAX_CATALOG_ENTRIES: usize = 256;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const FILE_TIMEOUT: Duration = Duration::from_secs(5);
const BUNDLE_READ_TIMEOUT: Duration = Duration::from_secs(30);

struct Catalog {
    directory: PathBuf,
    public_key: [u8; 32],
}

#[derive(Serialize)]
struct PairView {
    id: String,
    contract: String,
    source_sha256: Option<String>,
    source_cubelet_sha256: Option<String>,
    source_shim_sha256: Option<String>,
    target_sha256: String,
    storage_backend: StackStorage,
}

impl Catalog {
    fn from_env() -> ApiResult<Option<Self>> {
        let dir = std::env::var_os("BROCADE_CVM_STACK_CATALOG_DIR");
        let key = std::env::var_os("BROCADE_CVM_STACK_PUBLIC_KEY");
        match (dir, key) {
            (None, None) => Ok(None),
            (Some(dir), Some(key)) if !dir.is_empty() => {
                let public_key = key
                    .to_str()
                    .and_then(stack_hex::<32>)
                    .ok_or_else(catalog_unavailable)?;
                let directory = PathBuf::from(dir);
                if !directory.is_absolute() {
                    return Err(catalog_unavailable());
                }
                Ok(Some(Self {
                    directory,
                    public_key,
                }))
            }
            _ => Err(catalog_unavailable()),
        }
    }

    async fn pairs(&self) -> ApiResult<Vec<PairView>> {
        tokio::time::timeout(FILE_TIMEOUT, self.read_pairs())
            .await
            .map_err(|_| catalog_unavailable())?
    }

    async fn read_pairs(&self) -> ApiResult<Vec<PairView>> {
        let mut directory = tokio::fs::read_dir(&self.directory)
            .await
            .map_err(|_| catalog_unavailable())?;
        let mut entries = 0;
        let mut pairs = Vec::new();
        while let Some(entry) = directory
            .next_entry()
            .await
            .map_err(|_| catalog_unavailable())?
        {
            entries += 1;
            if entries > MAX_CATALOG_ENTRIES {
                return Err(catalog_unavailable());
            }
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            if let Ok(verified) = self.read_pair(id).await {
                let pair = verified.pair();
                pairs.push(PairView {
                    id: id.into(),
                    contract: pair.contract.clone(),
                    source_sha256: pair.source_sha256.clone(),
                    source_cubelet_sha256: pair.source_cubelet_sha256.clone(),
                    source_shim_sha256: pair.source_shim_sha256.clone(),
                    target_sha256: pair.target_sha256.clone(),
                    storage_backend: pair.storage_backend,
                });
            }
        }
        pairs.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(pairs)
    }

    async fn read_pair(&self, id: &str) -> ApiResult<VerifiedStackPair> {
        if stack_hex::<32>(id).is_none() {
            return Err(unavailable_item());
        }
        let path = self.directory.join(format!("{id}.json"));
        let (file, size) = open_regular(&path).await?;
        if !(1..=MAX_MANIFEST_BYTES).contains(&size) {
            return Err(unavailable_item());
        }
        let mut contents = Vec::with_capacity(size as usize);
        file.take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut contents)
            .await
            .map_err(|_| unavailable_item())?;
        if contents.len() as u64 != size {
            return Err(unavailable_item());
        }
        let artifact: SignedStackPair =
            serde_json::from_slice(&contents).map_err(|_| unavailable_item())?;
        let verified = VerifiedStackPair::verify(artifact, &self.public_key)
            .map_err(|_| unavailable_item())?;
        let pair = verified.pair();
        if verified.id() != id {
            return Err(unavailable_item());
        }
        self.bundle(&pair.target_sha256, pair.target_bytes).await?;
        Ok(verified)
    }

    async fn bundle(&self, sha256: &str, bytes: u64) -> ApiResult<File> {
        if stack_hex::<32>(sha256).is_none() || !(1..=MAX_STACK_BUNDLE_BYTES).contains(&bytes) {
            return Err(unavailable_item());
        }
        let (file, size) = open_regular(&self.directory.join(format!("{sha256}.tar.gz"))).await?;
        if size != bytes {
            return Err(unavailable_item());
        }
        Ok(file)
    }
}

async fn open_regular(path: &FsPath) -> ApiResult<(File, u64)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(not(unix))]
    if tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| unavailable_item())?
        .file_type()
        .is_symlink()
    {
        return Err(unavailable_item());
    }
    let file = options.open(path).await.map_err(|_| unavailable_item())?;
    let metadata = file.metadata().await.map_err(|_| unavailable_item())?;
    if !metadata.is_file() {
        return Err(unavailable_item());
    }
    Ok((file, metadata.len()))
}

fn catalog_unavailable() -> ApiError {
    ApiError::Unavailable("Cube stack artifact catalog is unavailable")
}

fn unavailable_item() -> ApiError {
    StoreError::NotFound("Cube stack artifact".into()).into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::http) struct Create {
    node_id: String,
    pair_id: String,
}

pub(in crate::http) async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let pairs = match Catalog::from_env()? {
        Some(catalog) => catalog.pairs().await?,
        None => Vec::new(),
    };
    let (hosts, releases) = state.store.cvm_stack_overview(&actor).await?;
    Ok(private(Json(
        json!({ "pairs": pairs, "hosts": hosts, "releases": releases }),
    )))
}

pub(in crate::http) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Create>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let catalog = Catalog::from_env()?.ok_or_else(catalog_unavailable)?;
    let verified = tokio::time::timeout(FILE_TIMEOUT, catalog.read_pair(&request.pair_id))
        .await
        .map_err(|_| catalog_unavailable())??;
    let release = state
        .store
        .create_cvm_stack_release(&actor, &request.node_id, verified)
        .await?;
    Ok(private(Json(release)))
}

pub(in crate::http) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(state.store.cvm_stack_history(&actor).await?)))
}

pub(in crate::http) async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state.store.cancel_cvm_stack_release(&actor, id).await?,
    )))
}

pub(in crate::http) async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<StackPoll>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        state
            .store
            .poll_cvm_stack_release(&node.node_id, request)
            .await?,
    )))
}

pub(in crate::http) async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<StackClaim>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        state
            .store
            .claim_cvm_stack_release(&node.node_id, id, request)
            .await?,
    )))
}

pub(in crate::http) async fn bundle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sha256): Path<String>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    let bytes = state
        .store
        .cvm_stack_bundle_bytes(&node.node_id, &sha256)
        .await?;
    let catalog = Catalog::from_env()?.ok_or_else(catalog_unavailable)?;
    let file = tokio::time::timeout(FILE_TIMEOUT, catalog.bundle(&sha256, bytes))
        .await
        .map_err(|_| catalog_unavailable())??;
    let stream = futures_util::stream::try_unfold((file, bytes), |(mut file, left)| async move {
        if left == 0 {
            return Ok(None);
        }
        let mut chunk = vec![0; left.min(64 * 1024) as usize];
        tokio::time::timeout(BUNDLE_READ_TIMEOUT, file.read_exact(&mut chunk)).await??;
        Ok::<_, std::io::Error>(Some((chunk, (file, left - (64 * 1024).min(left)))))
    });
    Ok(private((
        [
            (header::CONTENT_TYPE, "application/gzip".to_owned()),
            (header::CONTENT_LENGTH, bytes.to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use brocade_core::hash::{hex_lower, sha256_hex};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[tokio::test]
    async fn catalog_lists_only_current_verified_named_pairs_with_exact_bundle_size() {
        let dir = tempfile::tempdir().unwrap();
        let signer = Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
        let catalog = Catalog {
            directory: dir.path().into(),
            public_key: signer.public_key().as_ref().try_into().unwrap(),
        };
        let pair = StackPair {
            contract: "brocade-cvm-stack-cold-pair-v1".into(),
            architecture: "x86_64".into(),
            storage_backend: StackStorage::Xfs,
            source_sha256: Some("a".repeat(64)),
            target_sha256: sha256_hex(b"fake archive"),
            target_bytes: 12,
            source_cubelet_sha256: Some("c".repeat(64)),
            source_shim_sha256: Some("d".repeat(64)),
            target_cubelet_sha256: "e".repeat(64),
            target_shim_sha256: "f".repeat(64),
        };
        let sign = |payload: String| {
            let signature = hex_lower(signer.sign(payload.as_bytes()).as_ref());
            let id = sha256_hex(payload.as_bytes());
            (
                id,
                serde_json::to_vec(&SignedStackPair { payload, signature }).unwrap(),
            )
        };
        let (id, signed) = sign(serde_json::to_string(&pair).unwrap());
        let archive = dir.path().join(format!("{}.tar.gz", pair.target_sha256));
        std::fs::write(&archive, b"fake archive").unwrap();
        std::fs::write(dir.path().join(format!("{id}.json")), &signed).unwrap();
        std::fs::write(dir.path().join(format!("{}.json", "b".repeat(64))), &signed).unwrap();
        let fresh = StackPair {
            contract: "brocade-cvm-stack-fresh-install-v1".into(),
            source_sha256: None,
            source_cubelet_sha256: None,
            source_shim_sha256: None,
            ..pair.clone()
        };
        let (fresh_id, fresh_signed) = sign(serde_json::to_string(&fresh).unwrap());
        std::fs::write(dir.path().join(format!("{fresh_id}.json")), fresh_signed).unwrap();
        let mut old = serde_json::to_value(&pair).unwrap();
        old["expires_at"] = json!(u64::MAX);
        let (legacy_id, legacy) = sign(old.to_string());
        std::fs::write(dir.path().join(format!("{legacy_id}.json")), legacy).unwrap();
        assert!(catalog.read_pair(&legacy_id).await.is_err());
        let pairs = catalog
            .pairs()
            .await
            .unwrap_or_else(|_| panic!("catalog should list the valid pair"));
        let mut expected = [&id, &fresh_id];
        expected.sort();
        assert_eq!(
            pairs.iter().map(|pair| &pair.id).collect::<Vec<_>>(),
            expected
        );
        let install = pairs.iter().find(|pair| pair.id == fresh_id).unwrap();
        assert!(install.source_sha256.is_none());
        assert_eq!(install.contract, fresh.contract);
        assert!(catalog.read_pair("../outside").await.is_err());
        let wrong_key = Catalog {
            directory: dir.path().into(),
            public_key: [4; 32],
        };
        assert!(wrong_key.read_pair(&id).await.is_err());
        std::fs::write(&archive, b"too short").unwrap();
        assert!(catalog.read_pair(&id).await.is_err());
        #[cfg(unix)]
        {
            std::fs::remove_file(&archive).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), b"fake archive").unwrap();
            std::os::unix::fs::symlink(outside.path(), &archive).unwrap();
            assert!(catalog.read_pair(&id).await.is_err());
        }
    }

    #[test]
    fn stack_creation_does_not_accept_a_client_deadline() {
        assert!(
            serde_json::from_value::<Create>(json!({ "node_id": "node", "pair_id": "pair" }))
                .is_ok()
        );
        for field in ["expires_at", "start_deadline_at"] {
            let mut old = json!({ "node_id": "node", "pair_id": "pair" });
            old[field] = json!(60);
            assert!(serde_json::from_value::<Create>(old).is_err());
        }
    }

    #[test]
    fn stack_workflow_is_not_a_public_or_self_route() {
        for (method, path) in [
            (Method::GET, "/cvm/stack-releases"),
            (Method::POST, "/cvm/stack-releases"),
            (Method::GET, "/cvm/stack-releases/history"),
            (Method::POST, "/cvm/stack-releases/1/cancel"),
        ] {
            assert!(!user_may(&method, path));
            assert!(!skips_admin_auth(&method, path));
        }
    }
}
