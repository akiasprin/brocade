//! Existing operating systems adopted as instances. Their disks remain externally owned.
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionTarget {
    #[default]
    Cube,
    #[serde(rename = "baremetal")]
    BareMetal { identity: HostIdentity },
}

impl ExecutionTarget {
    pub fn is_cube(&self) -> bool {
        matches!(self, Self::Cube)
    }
    pub fn host_identity(&self) -> Option<&HostIdentity> {
        match self {
            Self::Cube => None,
            Self::BareMetal { identity } => Some(identity),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    /// Digests, not raw machine IDs or physical device paths.
    pub machine: String,
    pub root_filesystem: String,
}
impl HostIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_digest(&self.machine) || !valid_digest(&self.root_filesystem) {
            return Err("invalid bare-metal host identity".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInventory {
    pub identity: HostIdentity,
    pub os: String,
    pub terminal_user: String,
    pub primary_device: String,
}
impl HostInventory {
    pub fn validate(&self) -> Result<(), String> {
        self.identity.validate()?;
        if self.os.is_empty()
            || self.os.len() > 160
            || self.os.chars().any(char::is_control)
            || self.terminal_user.is_empty()
            || self.terminal_user.len() > 32
            || !self
                .terminal_user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || self.primary_device.is_empty()
            || self.primary_device.len() > 15
            || !self
                .primary_device
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
        {
            return Err("invalid bare-metal host inventory".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvidence {
    pub identity: HostIdentity,
    pub boot_id: String,
}
impl HostEvidence {
    pub fn validate(&self) -> Result<(), String> {
        self.identity.validate()?;
        if self.boot_id.len() != 36
            || self.boot_id.bytes().enumerate().any(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c != b'-'
                } else {
                    !c.is_ascii_hexdigit()
                }
            })
        {
            return Err("invalid bare-metal boot identity".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_target_is_explicit_and_native_identity_is_strict() {
        let raw = serde_json::json!({"execution":{"kind":"cube"},"login_revision":1,"routing":{"revision":0,"priorities":[],"routes":[],"dns":[]},"name":"current","image":{"id":"debian","name":"Debian","version":"13","architecture":"x86_64","sha256":"a".repeat(64),"boot_mode":"cube-oci"},"resources":{"vcpus":1,"memory_bytes":134217728,"root_disk_bytes":3221225472u64},"interfaces":[]});
        let spec: InstanceSpec = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(spec.execution, ExecutionTarget::Cube);
        assert_eq!(serde_json::to_value(spec).unwrap(), raw);
        for field in ["execution", "routing", "login_revision"] {
            let mut incomplete = raw.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<InstanceSpec>(incomplete).is_err());
        }
        let identity = HostIdentity {
            machine: "a".repeat(64),
            root_filesystem: "b".repeat(64),
        };
        assert!(HostEvidence {
            identity: identity.clone(),
            boot_id: "00000000-0000-0000-0000-000000000000".into()
        }
        .validate()
        .is_ok());
        assert!(HostEvidence {
            identity,
            boot_id: "------------------------------------".into()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn published_key_specs_preserve_their_bytes_and_new_credentials_are_explicit() {
        #[derive(Serialize)]
        struct PublishedSpec {
            #[serde(skip_serializing_if = "ExecutionTarget::is_cube")]
            execution: ExecutionTarget,
            routing: ContainerRoutes,
            name: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            image: Option<Image>,
            resources: Resources,
            ssh_public_key: String,
            interfaces: Vec<InterfaceSpec>,
        }
        for execution in [
            ExecutionTarget::Cube,
            ExecutionTarget::BareMetal {
                identity: HostIdentity {
                    machine: "a".repeat(64),
                    root_filesystem: "b".repeat(64),
                },
            },
        ] {
            let published = PublishedSpec {
                execution,
                routing: ContainerRoutes::default(),
                name: "published".into(),
                image: None,
                resources: Resources {
                    vcpus: 1,
                    memory_bytes: 134217728,
                    root_disk_bytes: 3221225472,
                },
                ssh_public_key: String::new(),
                interfaces: vec![],
            };
            let bytes = serde_json::to_vec(&published).unwrap();
            let mut decoded: InstanceSpec = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(decoded.login_revision, 0);
            assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
            decoded.login_revision = 1;
            let current = serde_json::to_value(&decoded).unwrap();
            assert_eq!(current["login_revision"], 1);
            assert!(current.get("ssh_public_key").is_none());
            assert!(current.get("execution").is_some());
            let mut invalid = current.clone();
            invalid.as_object_mut().unwrap().remove("login_revision");
            assert!(serde_json::from_value::<InstanceSpec>(invalid).is_err());
            let mut invalid = current;
            invalid["login_revision"] = serde_json::Value::Null;
            invalid["ssh_public_key"] = "".into();
            assert!(serde_json::from_value::<InstanceSpec>(invalid).is_err());
        }
    }
}
