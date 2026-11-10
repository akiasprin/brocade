use crate::{proto, Result};
use hyper_util::rt::TokioIo;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tonic::{
    transport::{Channel, Endpoint},
    Request,
};
use tower::service_fn;

use proto::containerd::services::tasks::v1 as tasks;
pub(crate) use proto::cubelet::services::cubebox::v1 as cb;

const MAX_MESSAGE: usize = 2 * 1024 * 1024;

pub(crate) fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(include_bytes!(concat!(env!("OUT_DIR"), "/cube.bin")).as_ref())
            .expect("build.rs compiles and validates the embedded Cube descriptor set")
    })
}

pub(crate) fn decode<M: Message + Default>(name: &str, value: Value) -> Result<M> {
    let descriptor = pool()
        .get_message_by_name(name)
        .ok_or("Cube message descriptor is missing")?;
    let message = DynamicMessage::deserialize(descriptor, value)
        .map_err(|_| format!("Cube {name} protobuf JSON is invalid"))?;
    message
        .transcode_to()
        .map_err(|_| "Cube protobuf conversion failed".into())
}

pub(crate) fn encode<M: Message>(name: &str, value: M) -> Result<Value> {
    let descriptor = pool()
        .get_message_by_name(name)
        .ok_or("Cube message descriptor is missing")?;
    let message = DynamicMessage::decode(descriptor, value.encode_to_vec().as_slice())
        .map_err(|_| "Cube protobuf response is invalid")?;
    let options = SerializeOptions::new()
        .use_proto_field_name(true)
        .use_enum_numbers(true)
        .stringify_64_bit_integers(false)
        .skip_default_fields(false);
    let mut bytes = Vec::new();
    message
        .serialize_with_options(&mut serde_json::Serializer::new(&mut bytes), &options)
        .map_err(|_| "Cube protobuf JSON serialization failed")?;
    serde_json::from_slice(&bytes).map_err(|_| "Cube protobuf response JSON is invalid".into())
}

#[derive(Clone)]
pub(crate) struct Rpc {
    channel: Channel,
}

impl Rpc {
    pub(crate) fn new(path: PathBuf) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err("Cubelet socket must be an absolute clean path".into());
        }
        let channel = Endpoint::from_static("http://[::1]:50051")
            .connect_timeout(Duration::from_secs(10))
            .connect_with_connector_lazy(service_fn(move |_| {
                let path = path.clone();
                async move {
                    let uid = trusted_socket(&path)?;
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    if stream.peer_cred()?.uid() != uid {
                        return Err(std::io::Error::other(
                            "Cubelet peer identity is not trusted",
                        ));
                    }
                    Ok(TokioIo::new(stream))
                }
            }));
        Ok(Self { channel })
    }

    pub(crate) async fn cube(&self, method: &str, value: Value) -> Result<Value> {
        let mut client = cb::cubebox_mgr_client::CubeboxMgrClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_MESSAGE)
            .max_encoding_message_size(MAX_MESSAGE);
        macro_rules! call {
            ($method:ident, $input:ident, $output:ident) => {{
                let input = decode::<cb::$input>(
                    concat!("cubelet.services.cubebox.v1.", stringify!($input)),
                    value,
                )?;
                let response = client
                    .$method(request(input, Duration::from_secs(240)))
                    .await
                    .map_err(|e| format!("Cubelet {} RPC failed: {:?}", method, e.code()))?
                    .into_inner();
                encode(
                    concat!("cubelet.services.cubebox.v1.", stringify!($output)),
                    response,
                )
            }};
        }
        match method {
            "List" => call!(list, ListCubeSandboxRequest, ListCubeSandboxResponse),
            "Update" => call!(update, UpdateCubeSandboxRequest, UpdateCubeSandboxResponse),
            "Create" => call!(create, RunCubeSandboxRequest, RunCubeSandboxResponse),
            "Destroy" => call!(
                destroy,
                DestroyCubeSandboxRequest,
                DestroyCubeSandboxResponse
            ),
            "InspectStorageVolumes" => call!(
                inspect_storage_volumes,
                InspectStorageVolumesRequest,
                InspectStorageVolumesResponse
            ),
            "ListSandboxSnapshots" => call!(
                list_sandbox_snapshots,
                ListSandboxSnapshotsRequest,
                ListSandboxSnapshotsResponse
            ),
            _ => Err("unsupported Cube RPC".into()),
        }
    }

    pub(crate) async fn task(&self, method: &str, value: Value) -> Result<Value> {
        let mut client = tasks::tasks_client::TasksClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_MESSAGE)
            .max_encoding_message_size(MAX_MESSAGE);
        macro_rules! call {
            ($method:ident, $input:ident, $output:literal) => {{
                let input = decode::<tasks::$input>(
                    concat!("containerd.services.tasks.v1.", stringify!($input)),
                    value,
                )?;
                let timeout = if method == "Wait" {
                    Duration::from_secs(7200)
                } else {
                    Duration::from_secs(20)
                };
                let response = client
                    .$method(request(input, timeout))
                    .await
                    .map_err(|e| format!("Cube Guest {} RPC failed: {:?}", method, e.code()))?
                    .into_inner();
                encode($output, response)
            }};
        }
        match method {
            "Exec" => call!(exec, ExecProcessRequest, "google.protobuf.Empty"),
            "Start" => call!(
                start,
                StartRequest,
                "containerd.services.tasks.v1.StartResponse"
            ),
            "Wait" => call!(
                wait,
                WaitRequest,
                "containerd.services.tasks.v1.WaitResponse"
            ),
            "Kill" => call!(kill, KillRequest, "google.protobuf.Empty"),
            "DeleteProcess" => call!(
                delete_process,
                DeleteProcessRequest,
                "containerd.services.tasks.v1.DeleteResponse"
            ),
            "ResizePty" => call!(resize_pty, ResizePtyRequest, "google.protobuf.Empty"),
            "CloseIO" => {
                let input = decode::<tasks::CloseIoRequest>(
                    "containerd.services.tasks.v1.CloseIORequest",
                    value,
                )?;
                client
                    .close_io(request(input, Duration::from_secs(5)))
                    .await
                    .map_err(|e| format!("Cube Guest CloseIO RPC failed: {:?}", e.code()))?;
                Ok(serde_json::json!({}))
            }
            _ => Err("unsupported Cube Guest RPC".into()),
        }
    }

    pub(crate) async fn exec(
        &self,
        runtime: &str,
        id: &str,
        input: &Path,
        output: &Path,
        process: Value,
    ) -> Result<()> {
        let spec =
            serde_json::to_vec(&process).map_err(|_| "invalid Guest process specification")?;
        let request_value = tasks::ExecProcessRequest {
            container_id: runtime.into(),
            exec_id: id.into(),
            terminal: true,
            stdin: input.to_str().ok_or("Guest FIFO path is not UTF-8")?.into(),
            stdout: output
                .to_str()
                .ok_or("Guest FIFO path is not UTF-8")?
                .into(),
            stderr: String::new(),
            spec: Some(prost_types::Any {
                type_url: "types.containerd.io/opencontainers/runtime-spec/1/Process".into(),
                value: spec,
            }),
        };
        tasks::tasks_client::TasksClient::new(self.channel.clone())
            .max_encoding_message_size(MAX_MESSAGE)
            .exec(request(request_value, Duration::from_secs(10)))
            .await
            .map_err(|e| format!("Cube Guest exec setup failed: {:?}", e.code()))?;
        Ok(())
    }
}

fn request<T>(value: T, timeout: Duration) -> Request<T> {
    let mut request = Request::new(value);
    request.metadata_mut().insert(
        "containerd-namespace",
        "default"
            .parse()
            .expect("static namespace is valid metadata"),
    );
    request.set_timeout(timeout);
    request
}

pub(crate) fn trusted_socket(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions or pointer arguments.
    let uid = unsafe { libc::geteuid() };
    // Cubelet/containerd intentionally creates 0660 sockets. The containing
    // directory prevents replacement; SO_PEERCRED authenticates the live server.
    let parent = std::fs::metadata(
        path.parent()
            .ok_or_else(|| std::io::Error::other("Cubelet socket directory missing"))?,
    )?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid
        || metadata.mode() & 0o002 != 0
        || !parent.is_dir()
        || ![0, uid].contains(&parent.uid())
        || parent.mode() & 0o022 != 0
    {
        return Err(std::io::Error::other(
            "Cubelet socket ownership or mode is unsafe",
        ));
    }
    Ok(uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn cubelet_group_socket_requires_an_owned_nonwritable_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("cubelet.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(trusted_socket(&path).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(trusted_socket(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(trusted_socket(&path).is_err());
    }
}
