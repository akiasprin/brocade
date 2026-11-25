use super::*;
use brocade_store::cvm::{
    CvmMutationReview, CvmNetworkEditRequest, CvmNetworkEntryAddition, CvmNetworkEntryRelease,
    CvmNetworkMount, CvmNetworkRoutesWrite,
};

const SHARED: &str = "198.51.100.20";
const WHOLE: &str = "198.51.100.30";

async fn entry_setup(db: &TestPg) {
    setup(db).await;
    let mut caps = catalogue();
    caps.primary_ip_delivery = true;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    for node in ["n1", "n2"] {
        db.store
            .cvm_record_nic_capabilities(
                node,
                NicNodeCapabilities {
                    entry_protocol_rules: true,
                    ssh_ports: Vec::new(),
                    contract: NIC_CONTRACT_VERSION,
                    transports: vec![TransportKind::Wireguard, TransportKind::Xray],
                    consume: node == "n1",
                    provide: true,
                    max_bindings: 64,
                    addresses: vec![SHARED.parse().unwrap()],
                    host_ports: vec![NicHostPort {
                        protocol: L4Protocol::Tcp,
                        port: 30080,
                    }],
                    xray_certificate_pem: Some("fixture-public-certificate".into()),
                    error: None,
                    entries: true,
                    route_service: false,
                    entry_host_l3: false,
                    entry_xray: false,
                    entry_phantun: false,
                },
            )
            .await
            .unwrap();
    }
}
fn entry_spec() -> EntryPublicationSpec {
    EntryPublicationSpec {
        name: "HK entries".into(),
        provider_node: "n2".into(),
        addresses: vec![
            EntryAddressOffer {
                reserved_ports: Default::default(),
                address: SHARED.parse().unwrap(),
                public_address: None,
                kind: EntryAddressKind::Shared {
                    port_start: 30000,
                    port_end: 30999,
                },
            },
            EntryAddressOffer {
                reserved_ports: Default::default(),
                address: WHOLE.parse().unwrap(),
                public_address: Some("203.0.113.30".parse().unwrap()),
                kind: EntryAddressKind::Dedicated,
            },
        ],
        transports: vec![TransportOffer {
            listen_side: TransportListenSide::Auto,
            kind: TransportKind::Wireguard,
            endpoint: SHARED.parse().unwrap(),
            port_start: 40000,
            port_end: 40010,
            mtu: 1420,
        }],
        grants: vec![NetworkOwner {
            tenant_id: "platform.acme".into(),
            user_id: "alice".into(),
        }],
        wireguard_transport: EntryWireguardTransport::Udp,
        max_entries: 8,
    }
}
async fn publish_entries(db: &TestPg) -> EntryPublication {
    ({
        supersede_cvm_fixture(db).await;
        &db.store
    })
    .write_cvm_entry_publication(
        &system_admin(),
        None,
        EntryPublicationWrite {
            request_id: "entry-publication-request-0001".into(),
            expected_revision: 0,
            spec: entry_spec(),
        },
    )
    .await
    .unwrap()
}
fn tcp(listen_port: u16, target_port: u16) -> EntryMode {
    EntryMode::L4Range {
        protocol: EntryProtocol::Tcp,
        listen_start: listen_port,
        listen_end: listen_port,
        target_start: target_port,
        target_end: target_port,
    }
}
fn entry_request(key: &str, instance: &CvmInstance, address: &str, mode: EntryMode) -> EntryCreate {
    EntryCreate {
        request_id: key.into(),
        instance_id: instance.desired.id.clone(),
        publication_id: String::new(),
        address: address.parse().unwrap(),
        mode,
    }
}
async fn add(
    db: &TestPg,
    p: &EntryPublication,
    mut request: EntryCreate,
) -> brocade_store::Result<CvmEntryView> {
    request.publication_id = p.id.clone();
    ({
        supersede_cvm_fixture(db).await;
        &db.store
    })
    .create_cvm_entry(&system_admin(), request)
    .await
}
/// Reports full convergence of a node's current entry desired state.
async fn converge(db: &TestPg, node: &str) {
    let desired = ({
        publish_cvm_intent(db).await;
        &db.store
    })
    .cvm_entry_desired(node)
    .await
    .unwrap();
    let configured = |id: &String| EntryItemReport {
        id: id.clone(),
        configured: true,
        error: None,
    };
    assert!(db
        .store
        .cvm_record_entry_report(
            node,
            EntryNodeReport {
                diagnostics: None,
                generation: desired.generation,
                rules: desired.rules.iter().map(|r| configured(&r.id)).collect(),
                deliveries: desired
                    .deliveries
                    .iter()
                    .map(|d| configured(&d.id))
                    .collect(),
                tunnels: desired
                    .tunnels
                    .iter()
                    .map(|t| EntryTunnelReport {
                        id: t.id.clone(),
                        connected: true,
                        error: None,
                    })
                    .collect(),
                error: None,
            },
        )
        .await
        .unwrap());
}
async fn claims(db: &TestPg, entry: &str) -> Vec<(String, String, bool)> {
    sqlx::query_as(
        "SELECT kind,value,released FROM cvm_nic_resources WHERE entry_id=$1 ORDER BY kind,value",
    )
    .bind(entry)
    .fetch_all(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entries_share_addresses_by_protocol_and_hold_claims_until_the_entry_machine_confirms()
{
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let p = publish_entries(&db).await;
    let web = create(&db, "entry-container-web-0001").await;
    let api = create(&db, "entry-container-api-0001").await;

    let ssh = add(
        &db,
        &p,
        entry_request("entry-create-ssh-0001", &web, SHARED, tcp(30022, 22)),
    )
    .await
    .unwrap();
    assert!(ssh.remote && ssh.state == CvmEntryState::Pending);
    let replay = add(
        &db,
        &p,
        entry_request("entry-create-ssh-0001", &web, SHARED, tcp(30022, 22)),
    )
    .await
    .unwrap();
    assert_eq!(replay.id, ssh.id);
    // Same protocol and port conflicts across containers; the other protocol is free.
    assert!(matches!(
        add(
            &db,
            &p,
            entry_request("entry-create-dup-0001", &api, SHARED, tcp(30022, 2222))
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    let dns = add(
        &db,
        &p,
        entry_request(
            "entry-create-dns-0001",
            &api,
            SHARED,
            EntryMode::L4Range {
                protocol: EntryProtocol::Udp,
                listen_start: 30022,
                listen_end: 30022,
                target_start: 53,
                target_end: 53,
            },
        ),
    )
    .await
    .unwrap();
    for (key, mode) in [
        (
            "entry-create-both-0001",
            EntryMode::L4Range {
                protocol: EntryProtocol::TcpUdp,
                listen_start: 30022,
                listen_end: 30022,
                target_start: 22,
                target_end: 22,
            },
        ),
        // A port the entry machine's own service listens on.
        ("entry-create-host-0001", tcp(30080, 80)),
    ] {
        assert!(matches!(
            add(&db, &p, entry_request(key, &api, SHARED, mode)).await,
            Err(StoreError::Conflict(_))
        ));
    }
    // Shared addresses take ports inside the published range; whole IPs only L3.
    for (key, address, mode) in [
        ("entry-create-range-0001", SHARED, tcp(22, 22)),
        ("entry-create-l3sh-0001", SHARED, EntryMode::L3),
        ("entry-create-l4wh-0001", WHOLE, tcp(30443, 443)),
    ] {
        assert!(matches!(
            add(&db, &p, entry_request(key, &api, address, mode)).await,
            Err(StoreError::InvalidData(_))
        ));
    }
    let whole = add(
        &db,
        &p,
        entry_request("entry-create-whole-0001", &api, WHOLE, EntryMode::L3),
    )
    .await
    .unwrap();
    assert_eq!(whole.visible_address.to_string(), "203.0.113.30");
    assert!(matches!(
        add(
            &db,
            &p,
            entry_request("entry-create-whole-0002", &web, WHOLE, EntryMode::L3)
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        claims(&db, &ssh.id).await,
        vec![("tcp".into(), format!("{SHARED}:30022"), false)]
    );
    // Entry ports block the machine's managed listeners like any NIC claim.
    let request: brocade_store::UpdateNodeRequest =
        serde_json::from_value(json!({"wg_listen_port":30022})).unwrap();
    assert!(matches!(
        db.store.update_node(&system_admin(), "n2", request).await,
        Err(StoreError::Conflict(_))
    ));

    // The entry machine maps to host-side primary addresses through one shared tunnel.
    let entry_side = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap();
    assert_eq!(entry_side.rules.len(), 3);
    assert_eq!(entry_side.tunnels.len(), 1);
    let tunnel = &entry_side.tunnels[0];
    assert_eq!(tunnel.side, EntryTunnelSide::Entry);
    assert!(tunnel.listens() && !tunnel.credential.is_empty());
    assert!(entry_side
        .rules
        .iter()
        .all(|r| r.tunnel.as_deref() == Some(tunnel.id.as_str())
            && r.target == Some("192.0.2.10".parse().unwrap())));
    let host_side = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n1")
    .await
    .unwrap();
    assert_eq!(host_side.deliveries.len(), 3);
    assert_eq!(host_side.tunnels[0].side, EntryTunnelSide::Target);
    assert_eq!(host_side.tunnels[0].slot, tunnel.slot);
    assert_ne!(host_side.tunnels[0].credential, tunnel.credential);
    let view = serde_json::to_string(
        &db.store
            .cvm_entry_board(&actor_user("alice"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!view.contains(&tunnel.credential) && !view.contains("sealed"));
    converge(&db, "n2").await;
    converge(&db, "n1").await;
    let board = db
        .store
        .cvm_entry_board(&actor_user("alice"))
        .await
        .unwrap();
    assert!(board
        .entries
        .iter()
        .all(|e| e.state == CvmEntryState::Active));
    let shared = &board.publications[0].usage[0];
    assert!(shared.ports.iter().any(|u| u.protocol == L4Protocol::Tcp
        && u.port_start == 30022
        && u.entry_id.as_deref() == Some(ssh.id.as_str())));
    assert!(shared
        .ports
        .iter()
        .any(|u| u.protocol == L4Protocol::Tcp && u.port_start == 30080 && u.entry_id.is_none()));
    // Another user sees neither the entries nor an address that is not granted.
    let other = db.store.cvm_entry_board(&actor_user("bob")).await.unwrap();
    assert!(other.entries.is_empty() && other.publications.is_empty());

    // Re-pointing keeps the claim and changes only the delivery target.
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .repoint_cvm_entry(
            &actor_user("bob"),
            &ssh.id,
            EntryRepoint {
                request_id: "entry-repoint-bob-0001".into(),
                expected_revision: ssh.revision,
                instance_id: api.desired.id.clone(),
            },
        )
        .await,
        Err(StoreError::Forbidden(_))
    ));
    let moved = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .repoint_cvm_entry(
        &system_admin(),
        &ssh.id,
        EntryRepoint {
            request_id: "entry-repoint-ssh-0001".into(),
            expected_revision: ssh.revision,
            instance_id: api.desired.id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(moved.instance_id, api.desired.id);
    assert_eq!(moved.revision, ssh.revision + 1);
    assert_eq!(claims(&db, &ssh.id).await.len(), 1);

    // A released entry keeps its port until the entry machine reports without it.
    let released = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .release_cvm_entry(
        &system_admin(),
        &dns.id,
        EntryRelease {
            request_id: "entry-release-dns-0001".into(),
            expected_revision: dns.revision,
        },
    )
    .await
    .unwrap();
    assert_eq!(released.state, CvmEntryState::Releasing);
    let again = EntryMode::L4Range {
        protocol: EntryProtocol::Udp,
        listen_start: 30022,
        listen_end: 30022,
        target_start: 53,
        target_end: 53,
    };
    assert!(matches!(
        add(
            &db,
            &p,
            entry_request("entry-create-dns-0002", &web, SHARED, again.clone())
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    converge(&db, "n2").await;
    assert!(claims(&db, &dns.id).await.iter().all(|c| c.2));
    add(
        &db,
        &p,
        entry_request("entry-create-dns-0003", &web, SHARED, again),
    )
    .await
    .unwrap();

    // Deleting a container releases its entries in the same transaction.
    let delete = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .command_cvm(
        &system_admin(),
        &api.desired.id,
        CvmCommand {
            request_id: "entry-delete-api-0001".into(),
            expected_generation: api.desired.generation,
            change: CvmChange::Delete { delete_data: true },
        },
    )
    .await;
    assert!(delete.is_ok(), "{delete:?}");
    let left: Vec<(String, bool)> =
        sqlx::query_as("SELECT id,released FROM cvm_entries WHERE instance_id=$1 ORDER BY id")
            .bind(&api.desired.id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(!left.is_empty() && left.iter().all(|(_, released)| *released));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_tunnels_follow_their_last_entry_and_wait_for_both_machines() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let p = publish_entries(&db).await;
    let web = create(&db, "tunnel-container-web-0001").await;
    let first = add(
        &db,
        &p,
        entry_request("tunnel-entry-a-0001", &web, SHARED, tcp(30001, 80)),
    )
    .await
    .unwrap();
    let second = add(
        &db,
        &p,
        entry_request("tunnel-entry-b-0001", &web, SHARED, tcp(30002, 80)),
    )
    .await
    .unwrap();
    let tunnels: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_entry_tunnels")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        tunnels, 1,
        "entries between one machine pair share one tunnel"
    );
    publish_cvm_intent(&db).await;
    // Simulate the prior persisted encoding without touching immutable revision history.
    // Both the provider and the compute host must be able to read it before withdrawal.
    let frozen_before: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT revision_id,node_id,payload FROM cvm_revision_nodes ORDER BY revision_id,node_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    for node in ["n1", "n2"] {
        let (revision, payload): (i64, String) =
            sqlx::query_as("SELECT revision_id,payload FROM cvm_serving_nodes WHERE node_id=$1")
                .bind(node)
                .fetch_one(db.pool())
                .await
                .unwrap();
        let context = format!("cvm-revision:{node}:{revision}");
        let json = brocade_store::secrets::open(&context, &payload).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in ["rules", "deliveries"] {
            for item in saved["entry"][field].as_array_mut().unwrap() {
                if item["host_local"] == false {
                    item.as_object_mut().unwrap().remove("host_local");
                }
            }
        }
        for tunnel in saved["entry"]["tunnels"].as_array_mut().unwrap() {
            if tunnel["wireguard_transport"] == "udp" {
                tunnel
                    .as_object_mut()
                    .unwrap()
                    .remove("wireguard_transport");
            }
        }
        let legacy = brocade_store::secrets::seal(&context, &saved.to_string()).unwrap();
        sqlx::query("UPDATE cvm_serving_nodes SET payload=$2 WHERE node_id=$1")
            .bind(node)
            .bind(&legacy)
            .execute(db.pool())
            .await
            .unwrap();
        let desired = db.store.cvm_entry_desired(node).await.unwrap();
        assert!(!desired.tunnels.is_empty());
        let preserved: String =
            sqlx::query_scalar("SELECT payload FROM cvm_serving_nodes WHERE node_id=$1")
                .bind(node)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            preserved, legacy,
            "reading must not rewrite the saved product"
        );
    }
    let frozen_after: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT revision_id,node_id,payload FROM cvm_revision_nodes ORDER BY revision_id,node_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen_before, frozen_after);
    for entry in [&first, &second] {
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .release_cvm_entry(
            &system_admin(),
            &entry.id,
            EntryRelease {
                request_id: format!("tunnel-release-{}", &entry.id[6..22]),
                expected_revision: entry.revision,
            },
        )
        .await
        .unwrap();
    }
    let (withdrawn, cleaned): (bool, bool) =
        sqlx::query_as("SELECT withdrawn,cleaned FROM cvm_entry_tunnels")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(withdrawn && !cleaned);
    assert!(({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap()
    .tunnels
    .is_empty());
    converge(&db, "n2").await;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cvm_nic_resources WHERE tunnel_id IS NOT NULL AND NOT released",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        held, 2,
        "port and slot stay held until the host also confirms"
    );
    converge(&db, "n1").await;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cvm_nic_resources WHERE tunnel_id IS NOT NULL AND NOT released",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let states:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('node',n.node_id,'desired',n.desired_generation,'applied',n.applied_generation,'serving',s.entry_generation,'tunnels',s.tunnel_ids) FROM cvm_entry_nodes n LEFT JOIN cvm_serving_nodes s ON s.node_id=n.node_id ORDER BY n.node_id").fetch_all(db.pool()).await.unwrap();
    assert_eq!(held, 0, "cleanup receipts: {states:?}");
    // A failed report does not prove cleanup and keeps the applied generation.
    let before: i64 =
        sqlx::query_scalar("SELECT applied_generation FROM cvm_entry_nodes WHERE node_id='n2'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let third = add(
        &db,
        &p,
        entry_request("tunnel-entry-c-0001", &web, SHARED, tcp(30003, 80)),
    )
    .await
    .unwrap();
    let desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap();
    db.store
        .cvm_record_entry_report(
            "n2",
            EntryNodeReport {
                generation: desired.generation,
                error: Some("nft failed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT applied_generation FROM cvm_entry_nodes WHERE node_id='n2'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    let view = db
        .store
        .cvm_entry_board(&actor_user("alice"))
        .await
        .unwrap();
    assert!(view.entries.iter().any(|e| e.id == third.id
        && e.state == CvmEntryState::Failed
        && e.error.as_deref() == Some("nft failed")));
    assert!(db
        .store
        .cvm_record_entry_report(
            "n2",
            EntryNodeReport {
                generation: desired.generation + 1,
                ..Default::default()
            },
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_publications_gate_host_ips_and_protect_live_entries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let write = |key: &str, revision: u64, spec: EntryPublicationSpec| EntryPublicationWrite {
        request_id: key.into(),
        expected_revision: revision,
        spec,
    };
    let mut remote = entry_spec();
    remote.addresses[0].address = "198.51.100.99".parse().unwrap();
    let mut own = entry_spec();
    own.addresses[1].address = "198.51.100.20".parse().unwrap();
    own.addresses.remove(0);
    let mut backbone = own.clone();
    backbone.addresses[0].address = "10.66.0.2".parse().unwrap();
    for (key, spec, needs_upgrade) in [
        ("entry-pub-remote-0001", remote, false),
        ("entry-pub-own-0001", own, true),
        ("entry-pub-backbone-0001", backbone, false),
    ] {
        let result = ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .write_cvm_entry_publication(&system_admin(), None, write(key, 0, spec))
        .await;
        assert!(matches!(
            (result, needs_upgrade),
            (Err(StoreError::InvalidData(_)), false) | (Err(StoreError::Unsupported(_)), true)
        ));
    }
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .write_cvm_entry_publication(
            &actor_user("alice"),
            None,
            write("entry-pub-user-0001", 0, entry_spec())
        )
        .await,
        Err(StoreError::Forbidden(_))
    ));
    let p = publish_entries(&db).await;
    let mut twice = entry_spec();
    twice.name = "Second".into();
    twice.addresses.truncate(1);
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .write_cvm_entry_publication(
            &system_admin(),
            None,
            write("entry-pub-twice-0001", 0, twice)
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    let web = create(&db, "publication-container-0001").await;
    add(
        &db,
        &p,
        entry_request("publication-entry-0001", &web, SHARED, tcp(30500, 80)),
    )
    .await
    .unwrap();
    let mut narrowed = entry_spec();
    narrowed.addresses[0].kind = EntryAddressKind::Shared {
        port_start: 30000,
        port_end: 30100,
    };
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .write_cvm_entry_publication(
            &system_admin(),
            Some(&p.id),
            write("entry-pub-narrow-0001", p.revision, narrowed)
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .delete_cvm_entry_publication(
            &system_admin(),
            &p.id,
            PublicationAdmission {
                request_id: "entry-pub-delete-0001".into(),
                expected_revision: p.revision,
                admitting: false,
            }
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    // Revoking the grant works offline and stops forwarding without dropping the claim.
    sqlx::query("UPDATE cvm_nic_nodes SET observed_at=now()-interval '1 hour' WHERE node_id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    let mut revoked = entry_spec();
    revoked.grants.clear();
    let p = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .write_cvm_entry_publication(
        &system_admin(),
        Some(&p.id),
        write("entry-pub-revoke-0001", p.revision, revoked),
    )
    .await
    .unwrap();
    assert_eq!(p.revision, 2);
    let rules = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap()
    .rules;
    assert!(rules.len() == 1 && rules[0].target.is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_whole_ip_entry_becomes_the_egress_source_on_the_same_machine() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let network = publish(&db).await;
    let entries = publish_entries(&db).await;
    let web = create(&db, "symmetric-container-0001").await;
    let web = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &web.desired.id,
        mount_request(&web, &network, "symmetric-mount-0001"),
    )
    .await
    .unwrap();
    let source = |db: &TestPg| {
        let pool = db.pool().clone();
        async move {
            let raw: serde_json::Value =
                sqlx::query_scalar("SELECT target FROM cvm_nic_bindings WHERE NOT cleaned")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            serde_json::from_value::<NicBinding>(raw).unwrap()
        }
    };
    let mounted = source(&db).await;
    assert_eq!(mounted.entry_source_ipv4, None);
    let whole = add(
        &db,
        &entries,
        entry_request("symmetric-entry-0001", &web, WHOLE, EntryMode::L3),
    )
    .await
    .unwrap();
    let binding = source(&db).await;
    assert_eq!(binding.entry_source_ipv4, Some(WHOLE.parse().unwrap()));
    assert_eq!(binding.generation, mounted.generation + 1);
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .release_cvm_entry(
        &system_admin(),
        &whole.id,
        EntryRelease {
            request_id: "symmetric-release-0001".into(),
            expected_revision: whole.revision,
        },
    )
    .await
    .unwrap();
    assert_eq!(source(&db).await.entry_source_ipv4, None);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_host_ip_l3_is_gated_exclusive_and_coexists_with_shared_egress() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let network = publish(&db).await;
    let web = create_native(&db, "host-l3-native-0001").await;
    let mut native = super::super::baremetal_tests::native_catalogue();
    native.managed_networks = true;
    native.configure_network = true;
    native.hot_attach = true;
    native.hot_detach = true;
    native.primary_ip_delivery = true;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: native,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let web = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &web.desired.id,
        mount_request(&web, &network, "host-l3-mount-0001"),
    )
    .await
    .unwrap();
    let mut spec = entry_spec();
    spec.addresses = vec![EntryAddressOffer {
        reserved_ports: Default::default(),
        address: SHARED.parse().unwrap(),
        public_address: None,
        kind: EntryAddressKind::Dedicated,
    }];
    let write = EntryPublicationWrite {
        request_id: "host-l3-publication-0001".into(),
        expected_revision: 0,
        spec: spec.clone(),
    };
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .write_cvm_entry_publication(&system_admin(), None, write.clone())
        .await,
        Err(StoreError::Unsupported(_))
    ));
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT capabilities FROM cvm_nic_nodes WHERE node_id='n2'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let mut caps: NicNodeCapabilities = serde_json::from_value(raw).unwrap();
    caps.entry_host_l3 = true;
    db.store
        .cvm_record_nic_capabilities("n2", caps.clone())
        .await
        .unwrap();
    let publication = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .write_cvm_entry_publication(&system_admin(), None, write)
    .await
    .unwrap();
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request("host-l3-target-old-agent", &web, SHARED, EntryMode::L3)
        )
        .await,
        Err(StoreError::Unsupported(_))
    ));
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT capabilities FROM cvm_nic_nodes WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let mut target_caps: NicNodeCapabilities = serde_json::from_value(raw).unwrap();
    target_caps.entry_host_l3 = true;
    db.store
        .cvm_record_nic_capabilities("n1", target_caps)
        .await
        .unwrap();
    let entry = add(
        &db,
        &publication,
        entry_request("host-l3-create-0001", &web, SHARED, EntryMode::L3),
    )
    .await
    .unwrap();
    let desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap();
    assert_eq!(desired.rules.len(), 1);
    assert_eq!(desired.rules[0].mode, EntryMode::L3);
    assert_eq!(desired.tunnels.len(), 1);
    assert_eq!(
        desired.tunnels[0].offer.endpoint,
        SHARED.parse::<std::net::IpAddr>().unwrap()
    );
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request("host-l3-create-duplicate", &web, SHARED, EntryMode::L3)
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    // Shared NAT only uses outbound replies, so it remains publishable after L3 ingress.
    let mut second = publication_spec();
    second.name = "second egress".into();
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .write_cvm_nic_publication(
        &system_admin(),
        None,
        PublicationWrite {
            request_id: "host-l3-shared-after-0001".into(),
            expected_revision: 0,
            spec: second,
        },
    )
    .await
    .unwrap();
    caps.entry_host_l3 = false;
    db.store
        .cvm_record_nic_capabilities("n2", caps.clone())
        .await
        .unwrap();
    assert!(matches!(
        ({
            publish_cvm_intent(&db).await;
            &db.store
        })
        .cvm_entry_desired("n2")
        .await,
        Err(StoreError::Unsupported(_))
    ));
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request("host-l3-old-agent-0001", &web, SHARED, EntryMode::L3)
        )
        .await,
        Err(StoreError::Unsupported(_))
    ));
    caps.entry_host_l3 = true;
    db.store
        .cvm_record_nic_capabilities("n2", caps)
        .await
        .unwrap();
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .release_cvm_entry(
        &system_admin(),
        &entry.id,
        EntryRelease {
            request_id: "host-l3-release-0001".into(),
            expected_revision: entry.revision,
        },
    )
    .await
    .unwrap();
    assert!(claims(&db, &entry.id).await.iter().all(|c| !c.2));
    converge(&db, "n2").await;
    converge(&db, "n1").await;
    assert!(claims(&db, &entry.id).await.iter().all(|c| c.2));
    add(
        &db,
        &publication,
        entry_request("host-l3-reuse-0001", &web, SHARED, EntryMode::L3),
    )
    .await
    .unwrap();
}

/// Optionally exports real Store-generated private assignments for the isolated KVM runner.
/// No credentials are printed; exports require an explicit existing private directory.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_xray_and_phantun_assignments_and_capability_gates() {
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::Path,
    };
    let output = std::env::var_os("BROCADE_ENTRY_ASSIGNMENTS").map(std::path::PathBuf::from);
    if let Some(output) = &output {
        assert!(output.is_absolute() && output.is_dir());
        assert_eq!(
            std::fs::metadata(output).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    for (name, kind, encapsulation) in [
        (
            "udp",
            TransportKind::Wireguard,
            EntryWireguardTransport::Udp,
        ),
        ("xray", TransportKind::Xray, EntryWireguardTransport::Udp),
        (
            "phantun",
            TransportKind::Wireguard,
            EntryWireguardTransport::Phantun,
        ),
    ] {
        for listener in [TransportListenSide::Provider, TransportListenSide::Consumer] {
            for ipv6 in [false, true] {
                let Some(db) = TestPg::start_if_enabled().await else {
                    return;
                };
                entry_setup(&db).await;
                let key = format!(
                    "{name}-{}-{}",
                    if listener == TransportListenSide::Provider {
                        "provider"
                    } else {
                        "consumer"
                    },
                    if ipv6 { "v6" } else { "v4" }
                );
                let endpoint = std::env::var(if ipv6 {
                    "BROCADE_ENTRY_ENDPOINT_V6"
                } else {
                    "BROCADE_ENTRY_ENDPOINT_V4"
                })
                .unwrap_or_else(|_| {
                    if ipv6 {
                        "2001:db8::20".into()
                    } else {
                        "198.51.100.30".into()
                    }
                });
                let target = std::env::var("BROCADE_ENTRY_TARGET_V4")
                    .unwrap_or_else(|_| "192.0.2.10".into());
                let target6 = std::env::var("BROCADE_ENTRY_TARGET_V6")
                    .unwrap_or_else(|_| "2001:db8::10".into());
                sqlx::query("UPDATE nodes SET public_ipv4=$2,public_ipv6=$3 WHERE id=$1")
                    .bind("n1")
                    .bind(&target)
                    .bind(&target6)
                    .execute(db.pool())
                    .await
                    .unwrap();
                for node in ["n1", "n2"] {
                    let raw: serde_json::Value = sqlx::query_scalar(
                        "SELECT capabilities FROM cvm_nic_nodes WHERE node_id=$1",
                    )
                    .bind(node)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
                    let mut caps: NicNodeCapabilities = serde_json::from_value(raw).unwrap();
                    caps.entry_host_l3 = true;
                    caps.entry_xray = true;
                    caps.entry_phantun = true;
                    if !ipv6 && node == "n2" {
                        caps.addresses.push(endpoint.parse().unwrap());
                    }
                    if let Ok(file) =
                        std::env::var(format!("BROCADE_ENTRY_CERTIFICATE_{}", node.to_uppercase()))
                    {
                        caps.xray_certificate_pem = Some(std::fs::read_to_string(file).unwrap());
                    }
                    db.store
                        .cvm_record_nic_capabilities(node, caps)
                        .await
                        .unwrap();
                }
                let instance = create_native(&db, &format!("entry-native-{key}")).await;
                let mut runtime = super::super::baremetal_tests::native_catalogue();
                runtime.primary_ip_delivery = true;
                runtime.managed_networks = true;
                runtime.configure_network = true;
                runtime.hot_attach = true;
                runtime.hot_detach = true;
                db.store
                    .cvm_record_inventory(
                        "n1",
                        InventoryReport {
                            capabilities: runtime,
                            terminal_sessions_used: None,
                            error: None,
                        },
                    )
                    .await
                    .unwrap();
                let mut report = guest_evidence(&instance);
                report.access_address = Some(target.clone());
                ({
                    publish_cvm_intent(&db).await;
                    &db.store
                })
                .cvm_record_report("n1", report)
                .await
                .unwrap();
                let mut spec = entry_spec();
                spec.name = key.clone();
                spec.transports[0].kind = kind;
                spec.transports[0].listen_side = listener;
                spec.transports[0].endpoint = endpoint.parse().unwrap();
                spec.transports[0].mtu = 1380;
                spec.wireguard_transport = encapsulation;
                if !ipv6 {
                    spec.addresses[1].address = endpoint.parse().unwrap();
                    spec.addresses[1].public_address = None;
                }
                let public = spec.addresses[1].address.to_string();
                let publication = ({
                    supersede_cvm_fixture(&db).await;
                    &db.store
                })
                .write_cvm_entry_publication(
                    &system_admin(),
                    None,
                    EntryPublicationWrite {
                        request_id: format!("entry-publish-{key}"),
                        expected_revision: 0,
                        spec: spec.clone(),
                    },
                )
                .await
                .unwrap();
                let whole = add(
                    &db,
                    &publication,
                    entry_request(
                        &format!("entry-whole-{key}"),
                        &instance,
                        &public,
                        EntryMode::L3,
                    ),
                )
                .await
                .unwrap();
                for (suffix, protocol, listen, target_port) in [
                    ("tcp", EntryProtocol::Tcp, 30022, 22222),
                    ("udp", EntryProtocol::Udp, 30023, 22223),
                ] {
                    add(
                        &db,
                        &publication,
                        entry_request(
                            &format!("entry-{suffix}-{key}"),
                            &instance,
                            SHARED,
                            EntryMode::L4Range {
                                protocol,
                                listen_start: listen,
                                listen_end: listen,
                                target_start: target_port,
                                target_end: target_port,
                            },
                        ),
                    )
                    .await
                    .unwrap();
                }
                let provider = ({
                    publish_cvm_intent(&db).await;
                    &db.store
                })
                .cvm_entry_desired("n2")
                .await
                .unwrap();
                let consumer = ({
                    publish_cvm_intent(&db).await;
                    &db.store
                })
                .cvm_entry_desired("n1")
                .await
                .unwrap();
                assert_eq!(provider.tunnels.len(), 1);
                assert_eq!(consumer.tunnels.len(), 1);
                assert_eq!(provider.tunnels[0].offer.kind, kind);
                assert_eq!(provider.tunnels[0].wireguard_transport, encapsulation);
                assert_eq!(
                    provider.tunnels[0].credential == consumer.tunnels[0].credential,
                    kind == TransportKind::Xray
                );
                assert!(provider
                    .rules
                    .iter()
                    .all(|r| r.target == Some(target.parse().unwrap())));
                assert!(consumer.deliveries.iter().all(|d| d.host_local));
                let protocol = if name == "udp" { "udp" } else { "tcp" };
                let claimed:i64=sqlx::query_scalar("SELECT count(*) FROM cvm_nic_resources WHERE tunnel_id=$1 AND kind=$2 AND NOT released").bind(&provider.tunnels[0].id).bind(protocol).fetch_one(db.pool()).await.unwrap();
                assert_eq!(claimed, 1);
                let mut changed = spec.clone();
                changed.transports[0].port_start += 1;
                assert!(matches!(
                    ({
                        supersede_cvm_fixture(&db).await;
                        &db.store
                    })
                    .write_cvm_entry_publication(
                        &system_admin(),
                        Some(&publication.id),
                        EntryPublicationWrite {
                            request_id: format!("entry-change-live-{key}"),
                            expected_revision: publication.revision,
                            spec: changed
                        }
                    )
                    .await,
                    Err(StoreError::Conflict(_))
                ));
                if name != "udp" {
                    let raw: serde_json::Value = sqlx::query_scalar(
                        "SELECT capabilities FROM cvm_nic_nodes WHERE node_id='n1'",
                    )
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
                    let caps: NicNodeCapabilities = serde_json::from_value(raw).unwrap();
                    let mut old = caps.clone();
                    old.entry_xray = false;
                    old.entry_phantun = false;
                    db.store
                        .cvm_record_nic_capabilities("n1", old)
                        .await
                        .unwrap();
                    assert!(matches!(
                        ({
                            publish_cvm_intent(&db).await;
                            &db.store
                        })
                        .cvm_entry_desired("n1")
                        .await,
                        Err(StoreError::Unsupported(_))
                    ));
                    assert!(matches!(
                        add(
                            &db,
                            &publication,
                            entry_request(
                                &format!("entry-old-target-{key}"),
                                &instance,
                                SHARED,
                                tcp(30024, 22222)
                            )
                        )
                        .await,
                        Err(StoreError::Unsupported(_))
                    ));
                    db.store
                        .cvm_record_nic_capabilities("n1", caps)
                        .await
                        .unwrap();
                }
                let waiting = EntryNodeReport {
                    generation: provider.generation,
                    rules: provider
                        .rules
                        .iter()
                        .map(|r| EntryItemReport {
                            id: r.id.clone(),
                            configured: true,
                            error: None,
                        })
                        .collect(),
                    tunnels: vec![EntryTunnelReport {
                        id: provider.tunnels[0].id.clone(),
                        connected: false,
                        error: None,
                    }],
                    ..Default::default()
                };
                db.store
                    .cvm_record_entry_report("n2", waiting)
                    .await
                    .unwrap();
                let failed = EntryNodeReport {
                    generation: consumer.generation,
                    deliveries: consumer
                        .deliveries
                        .iter()
                        .map(|r| EntryItemReport {
                            id: r.id.clone(),
                            configured: false,
                            error: Some("入口通道尚未就绪".into()),
                        })
                        .collect(),
                    tunnels: vec![EntryTunnelReport {
                        id: consumer.tunnels[0].id.clone(),
                        connected: false,
                        error: Some("fixture transport startup failure".into()),
                    }],
                    ..Default::default()
                };
                db.store
                    .cvm_record_entry_report("n1", failed)
                    .await
                    .unwrap();
                let failed = db
                    .store
                    .cvm_entry_board(&actor_user("alice"))
                    .await
                    .unwrap()
                    .entries
                    .into_iter()
                    .find(|e| e.id == whole.id)
                    .unwrap();
                assert_eq!(failed.state, CvmEntryState::Failed);
                assert_eq!(
                    failed.error.as_deref(),
                    Some("fixture transport startup failure")
                );
                converge(&db, "n1").await;
                converge(&db, "n2").await;
                assert_eq!(
                    db.store
                        .cvm_entry_board(&actor_user("alice"))
                        .await
                        .unwrap()
                        .entries
                        .iter()
                        .find(|e| e.id == whole.id)
                        .unwrap()
                        .state,
                    CvmEntryState::Active
                );
                if let Some(output) = &output {
                    let value = json!({"provider":provider,"consumer":consumer,"public":public,"shared":SHARED,"name":key});
                    let path = output.join(format!("{key}.json"));
                    assert!(Path::new(&path).parent() == Some(output.as_path()));
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)
                        .unwrap();
                    file.write_all(&serde_json::to_vec(&value).unwrap())
                        .unwrap();
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_network_logs_are_admin_only_and_visible_without_a_successful_apply() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let p = publish_entries(&db).await;
    let web = create(&db, "network-logs-instance-0001").await;
    let entry = add(
        &db,
        &p,
        entry_request("network-logs-entry-0001", &web, SHARED, tcp(30022, 22)),
    )
    .await
    .unwrap();
    let desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_entry_desired("n2")
    .await
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let diagnostics = NetworkDiagnostics {
        collected_at: now,
        events: vec![NetworkLogEvent {
            at: now,
            resource_id: Some(desired.tunnels[0].id.clone()),
            source: NetworkLogSource::Phantun,
            level: NetworkLogLevel::Error,
            message: "Phantun firewall configuration failed".into(),
        }],
        ..Default::default()
    };
    db.store
        .cvm_record_entry_report(
            "n2",
            EntryNodeReport {
                generation: desired.generation,
                error: Some("nft failed".into()),
                diagnostics: Some(diagnostics.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let logs = db
        .store
        .cvm_network_logs(&system_admin(), &["n2".into(), "n1".into()])
        .await
        .unwrap();
    let node = logs.nodes.iter().find(|n| n.node_id == "n2").unwrap();
    assert!(node.fresh);
    assert_eq!(
        node.diagnostics.as_ref().unwrap().events[0].message,
        "Phantun firewall configuration failed"
    );
    assert_eq!(
        node.domains
            .iter()
            .find(|d| d.domain == "entry")
            .unwrap()
            .error
            .as_deref(),
        Some("nft failed")
    );
    let json = serde_json::to_string(&logs).unwrap();
    assert!(!json.contains(&desired.tunnels[0].credential));
    assert!(!json.contains(&desired.tunnels[0].peer_credential));
    assert!(matches!(
        db.store
            .cvm_network_logs(&actor_user("alice"), &["n2".into()])
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(db
        .store
        .cvm_network_logs(&system_admin(), &[])
        .await
        .is_err());
    assert!(db
        .store
        .cvm_network_logs(&system_admin(), &["absent-node".into()])
        .await
        .is_err());
    let view = db
        .store
        .cvm_entry_board(&actor_user("alice"))
        .await
        .unwrap();
    let current = view.entries.iter().find(|e| e.id == entry.id).unwrap();
    assert_eq!(current.state, CvmEntryState::Failed);
    assert!(!current.can_read_logs);
    // NIC-only histories use the same JSON observation field and do not need an entry receipt.
    let nic_desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n1")
    .await
    .unwrap();
    db.store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                generation: nic_desired.generation,
                bindings: vec![],
                diagnostics: Some(diagnostics),
            },
        )
        .await
        .unwrap();
    let logs = db
        .store
        .cvm_network_logs(&system_admin(), &["n1".into()])
        .await
        .unwrap();
    assert!(logs.nodes[0].diagnostics.is_some());
    let mut invalid = NetworkDiagnostics::default();
    invalid.events.push(NetworkLogEvent {
        at: now,
        resource_id: None,
        source: NetworkLogSource::Agent,
        level: NetworkLogLevel::Error,
        message: "x".repeat(1025),
    });
    assert!(db
        .store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                generation: nic_desired.generation,
                bindings: vec![],
                diagnostics: Some(invalid)
            }
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_mixed_entry_release_requires_both_peers_and_retains_unapproved_cleanup_claims() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let instance = create(&db, "release-entry-instance-01").await;
    let p = publish_entries(&db).await;
    let entry = add(
        &db,
        &p,
        entry_request(
            "release-entry-create-01",
            &instance,
            SHARED,
            tcp(30100, 8080),
        ),
    )
    .await
    .unwrap();
    assert!(db
        .store
        .cvm_entry_desired("n2")
        .await
        .unwrap()
        .rules
        .is_empty());
    assert!(db
        .store
        .cvm_entry_desired("n1")
        .await
        .unwrap()
        .deliveries
        .is_empty());
    let r: i64 = sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let plan = db
        .store
        .plan_deployment(&system_admin(), r as u64)
        .await
        .unwrap();
    assert_eq!(
        plan.targets
            .iter()
            .find(|t| t.node_id == "n1")
            .unwrap()
            .wave,
        plan.targets
            .iter()
            .find(|t| t.node_id == "n2")
            .unwrap()
            .wave
    );
    publish_cvm_intent(&db).await;
    assert_eq!(
        db.store.cvm_entry_desired("n2").await.unwrap().rules.len(),
        1
    );
    converge(&db, "n1").await;
    let pending:String=sqlx::query_scalar("SELECT status FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(r).fetch_one(db.pool()).await.unwrap();
    assert_ne!(pending, "succeeded");
    converge(&db, "n2").await;
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    let done:String=sqlx::query_scalar("SELECT status FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(r).fetch_one(db.pool()).await.unwrap();
    assert_eq!(done, "succeeded");
    // Withdraw intent keeps the previously published rule and address claims until approval.
    db.store
        .release_cvm_entry(
            &system_admin(),
            &entry.id,
            EntryRelease {
                request_id: "release-entry-withdraw-01".into(),
                expected_revision: entry.revision,
            },
        )
        .await
        .unwrap();
    let published = db.store.cvm_entry_desired("n2").await.unwrap();
    assert!(published.rules.iter().any(|r| r.id == entry.id));
    assert!(claims(&db, &entry.id)
        .await
        .iter()
        .any(|(_, _, released)| !*released));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_unpublished_entry_creation_releases_ports_without_guest_mutation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let item = create(&db, "undo-entry-create-instance-01").await;
    let p = publish_entries(&db).await;
    let entry = add(
        &db,
        &p,
        entry_request(
            "undo-entry-create-request-01",
            &item,
            SHARED,
            tcp(30081, 22),
        ),
    )
    .await
    .unwrap();
    let pending = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(pending.operation_kind.as_deref(), Some("entries"));
    assert!(pending.revision_id > item.revision_id);
    let restored = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-entry-create-restore-01".into(),
                expected_generation: pending.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: pending.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(restored.desired.power, PowerState::Running);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_nic_resources WHERE entry_id=$1 AND NOT released"
        )
        .bind(&entry.id)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
    let replacement = add(
        &db,
        &p,
        entry_request(
            "undo-entry-reallocate-01",
            &restored,
            SHARED,
            tcp(30081, 22),
        ),
    )
    .await
    .unwrap();
    assert_ne!(replacement.id, entry.id);
    let next = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(next.operation_kind.as_deref(), Some("entries"));
    db.store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-entry-newer-restore-01".into(),
                expected_generation: next.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: next.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_unpublished_entry_removal_restores_existing_tunnel_and_port() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let item = create(&db, "undo-entry-remove-instance-01").await;
    let p = publish_entries(&db).await;
    let entry = add(
        &db,
        &p,
        entry_request("undo-entry-remove-create-01", &item, SHARED, tcp(30081, 22)),
    )
    .await
    .unwrap();
    converge(&db, "n1").await;
    converge(&db, "n2").await;
    supersede_cvm_fixture(&db).await;
    let entry = db
        .store
        .cvm_instance_entries(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == entry.id)
        .unwrap();
    db.store
        .release_cvm_entry(
            &system_admin(),
            &entry.id,
            EntryRelease {
                request_id: "undo-entry-remove-request-01".into(),
                expected_revision: entry.revision,
            },
        )
        .await
        .unwrap();
    let pending = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(pending.operation_kind.as_deref(), Some("entries"));
    db.store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-entry-remove-restore-01".into(),
                expected_generation: pending.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: pending.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT released FROM cvm_entries WHERE id=$1")
            .bind(&entry.id)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_entry_tunnels WHERE withdrawn AND NOT cleaned"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
    assert!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_nic_resources WHERE entry_id=$1 AND NOT released"
        )
        .bind(&entry.id)
        .fetch_one(db.pool())
        .await
        .unwrap()
            > 0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_pending_creation_with_entries_is_atomic_and_revision_fenced() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let p = publish_entries(&db).await;
    let item = db
        .store
        .create_cvm(
            &system_admin(),
            create_request("undo-all-instance-create-01"),
        )
        .await
        .unwrap();
    let entry = add(
        &db,
        &p,
        entry_request("undo-all-entry-create-01", &item, SHARED, tcp(30081, 22)),
    )
    .await
    .unwrap();
    let pending = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(pending.desired.generation, item.desired.generation);
    let stale = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-all-stale-request-01".into(),
                expected_generation: pending.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: item.revision_id.unwrap(),
                },
            },
        )
        .await;
    assert!(matches!(stale, Err(StoreError::Conflict(_))));
    let restored = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-all-restore-request-01".into(),
                expected_generation: pending.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: pending.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    assert!(!restored.reserved);
    assert!(db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_nic_resources WHERE entry_id=$1 AND NOT released"
        )
        .bind(&entry.id)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_ranges_reserve_every_port_atomically_and_keep_claims_until_cleanup() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let publication = publish_entries(&db).await;
    let instance = create(&db, "entry-range-container-0001").await;
    let mode = EntryMode::L4Range {
        protocol: EntryProtocol::TcpUdp,
        listen_start: 30040,
        listen_end: 30043,
        target_start: 40000,
        target_end: 40003,
    };
    let request = entry_request("entry-range-create-0001", &instance, SHARED, mode);
    let entry = add(&db, &publication, request.clone()).await.unwrap();
    assert_eq!(add(&db, &publication, request).await.unwrap().id, entry.id);
    assert_eq!(claims(&db, &entry.id).await.len(), 8);
    // A collision in the middle rolls back all of the other candidate ports and the entry row.
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_entries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request(
                "entry-range-overlap-0001",
                &instance,
                SHARED,
                tcp(30042, 22)
            )
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request(
                "entry-range-host-port-01",
                &instance,
                SHARED,
                EntryMode::L4Range {
                    protocol: EntryProtocol::Tcp,
                    listen_start: 30079,
                    listen_end: 30082,
                    target_start: 1000,
                    target_end: 1003
                }
            )
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cvm_entries")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    let board = db
        .store
        .cvm_entry_board(&actor_user("alice"))
        .await
        .unwrap();
    let held: Vec<_> = board
        .publications
        .iter()
        .flat_map(|p| &p.usage)
        .flat_map(|u| &u.ports)
        .filter(|p| p.entry_id.as_deref() == Some(&entry.id))
        .collect();
    assert_eq!(held.len(), 2);
    assert!(held
        .iter()
        .all(|p| p.port_start == 30040 && p.port_end == 30043));
    let targets = db.store.cvm_observation_targets("n1").await.unwrap();
    let probes = &targets
        .iter()
        .find(|t| t.id == instance.desired.id)
        .unwrap()
        .services;
    assert!(probes.iter().any(|p| p.port == 40000));
    assert!(probes.iter().any(|p| p.port == 40003));
    supersede_cvm_fixture(&db).await;
    db.store
        .release_cvm_entry(
            &system_admin(),
            &entry.id,
            EntryRelease {
                request_id: "entry-range-release-0001".into(),
                expected_revision: entry.revision,
            },
        )
        .await
        .unwrap();
    assert!(claims(&db, &entry.id).await.iter().all(|p| !p.2));
    assert!(add(
        &db,
        &publication,
        entry_request(
            "entry-range-before-clean-01",
            &instance,
            SHARED,
            tcp(30041, 22)
        )
    )
    .await
    .is_err());
    converge(&db, "n2").await;
    assert!(claims(&db, &entry.id).await.iter().all(|p| p.2));
    add(
        &db,
        &publication,
        entry_request(
            "entry-range-after-clean-01",
            &instance,
            SHARED,
            tcp(30041, 22),
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_range_second_protocol_and_concurrent_overlap_roll_back_whole_allocations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let publication = publish_entries(&db).await;
    let instance = create(&db, "entry-range-concurrent-01").await;
    for node in ["n1", "n2"] {
        sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) VALUES($1,$2) ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=excluded.agent_protocol_version")
            .bind(node).bind(i32::try_from(brocade_deployment::protocol::AGENT_PROTOCOL_VERSION).unwrap()).execute(db.pool()).await.unwrap();
    }
    add(
        &db,
        &publication,
        entry_request(
            "entry-range-existing-udp",
            &instance,
            SHARED,
            EntryMode::L4Range {
                protocol: EntryProtocol::Udp,
                listen_start: 30052,
                listen_end: 30052,
                target_start: 53,
                target_end: 53,
            },
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        add(
            &db,
            &publication,
            entry_request(
                "entry-range-both-protocols",
                &instance,
                SHARED,
                EntryMode::L4Range {
                    protocol: EntryProtocol::TcpUdp,
                    listen_start: 30050,
                    listen_end: 30053,
                    target_start: 40000,
                    target_end: 40003
                }
            )
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    let tcp_claims:i64=sqlx::query_scalar("SELECT count(*) FROM cvm_nic_resources WHERE scope='port:n2' AND kind='tcp' AND NOT released AND value LIKE '198.51.100.20:3005%'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(tcp_claims, 0);
    supersede_cvm_fixture(&db).await;
    let request = |id: &str, start, end| {
        let mut r = entry_request(
            id,
            &instance,
            SHARED,
            EntryMode::L4Range {
                protocol: EntryProtocol::Tcp,
                listen_start: start,
                listen_end: end,
                target_start: 40000,
                target_end: 40003,
            },
        );
        r.publication_id = publication.id.clone();
        r
    };
    let actor = system_admin();
    let first_request = request("entry-range-concurrent-a", 30060, 30063);
    let second_request = request("entry-range-concurrent-b", 30062, 30065);
    let (first, second) = tokio::join!(
        db.store.create_cvm_entry(&actor, first_request),
        db.store.create_cvm_entry(&actor, second_request)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let entry = first.or(second).unwrap();
    assert_eq!(claims(&db, &entry.id).await.len(), 4);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_network_edit_previews_fences_and_undoes_one_combined_unpublished_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let network = publish(&db).await;
    let entries = publish_entries(&db).await;
    let mut caps = catalogue();
    caps.primary_ip_delivery = true;
    caps.hot_attach = false;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let actor = system_admin();
    let instance = db
        .store
        .create_cvm(&actor, create_request("atomic-network-create-01"))
        .await
        .unwrap();
    assert_eq!(instance.status, ApplyStatus::Pending);
    let id = &instance.desired.id;
    let draft = "draft-test-mount";
    let request = CvmNetworkEditRequest {
        request_id: "atomic-network-combined-01".into(),
        expected_generation: instance.desired.generation,
        mounts: vec![CvmNetworkMount {
            draft_id: draft.into(),
            publication_id: network.id,
            transport: TransportKind::Wireguard,
            use_dns_suggestions: false,
        }],
        detach_interface_ids: vec![],
        entry_additions: vec![CvmNetworkEntryAddition {
            publication_id: entries.id,
            address: SHARED.parse().unwrap(),
            mode: tcp(30051, 8080),
        }],
        entry_releases: vec![],
        routes: Some(CvmNetworkRoutesWrite {
            expected_revision: instance.desired.spec.routing.revision,
            routes: ContainerRoutes {
                revision: 0,
                priorities: vec![InterfacePriority {
                    target: RouteTarget::interface(draft),
                    ipv4: Some(10),
                    ipv6: None,
                }],
                routes: vec![ContainerRoute {
                    destination: "192.0.2.0/24".parse().unwrap(),
                    target: RouteTarget::interface(draft),
                    service: None,
                }],
                dns: vec![],
            },
        }),
    };
    let before = network_edit_counts(&db, id).await;
    let preview = db
        .store
        .edit_cvm_network(
            &actor,
            id,
            request.clone(),
            CvmMutationReview {
                preview: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(network_edit_counts(&db, id).await, before);
    assert!(matches!(
        db.store
            .edit_cvm_network(
                &actor,
                id,
                request.clone(),
                CvmMutationReview {
                    expected_input_sha256: Some("0".repeat(64)),
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(network_edit_counts(&db, id).await, before);

    let saved = db
        .store
        .edit_cvm_network(
            &actor,
            id,
            request.clone(),
            CvmMutationReview {
                expected_input_sha256: Some(preview.input_sha256),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.mounted_interfaces, preview.mounted_interfaces);
    assert_eq!(
        saved.instance.operation_kind.as_deref(),
        Some("network_edit")
    );
    assert_eq!(
        saved.instance.desired.generation,
        instance.desired.generation + 1
    );
    let planned = &saved.instance.desired.spec.routing;
    assert_eq!(planned.revision, instance.desired.spec.routing.revision + 1);
    assert_eq!(
        planned.routes[0].target.interface_id(),
        Some(saved.mounted_interfaces[draft].as_str())
    );
    assert_eq!(planned.priorities[0].target, planned.routes[0].target);
    assert_eq!(network_edit_counts(&db, id).await, (before.0 + 1, 1, 1));
    let revision = saved.instance.revision_id.unwrap();
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT node_id FROM cvm_revision_nodes WHERE revision_id=$1 ORDER BY node_id",
    )
    .bind(revision as i64)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(nodes, ["n1", "n2"]);
    let review =
        serde_json::to_value(db.store.cvm_pending_review(&actor, id).await.unwrap()).unwrap();
    assert_eq!(review["revision"], revision);
    assert_eq!(review["fields"], serde_json::json!(["create"]));
    assert_eq!(review["entries_added"], 1);
    assert_eq!(review["nics_added"], 1);
    assert!(review["author"]
        .as_str()
        .is_some_and(|author| !author.is_empty()));
    let reader = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(matches!(
        db.store.cvm_pending_review(&reader, id).await,
        Err(StoreError::Forbidden(_))
    ));

    let replayed = db
        .store
        .edit_cvm_network(&actor, id, request, CvmMutationReview::default())
        .await
        .unwrap();
    assert_eq!(replayed.input_sha256, saved.input_sha256);
    assert_eq!(network_edit_counts(&db, id).await, (before.0 + 1, 1, 1));
    let undone = db
        .store
        .command_cvm(
            &actor,
            id,
            CvmCommand {
                request_id: "atomic-network-undo-0001".into(),
                expected_generation: saved.instance.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: revision,
                },
            },
        )
        .await
        .unwrap();
    assert!(!undone.reserved);
    assert_eq!(network_edit_counts(&db, id).await, (before.0 + 2, 0, 0));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_network_edit_entry_only_replays_and_fences_releases_by_container() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let publication = publish_entries(&db).await;
    let other = create(&db, "entry-network-edit-other").await;
    let instance = create(&db, "entry-network-edit-owner").await;
    let actor = system_admin();
    let id = &instance.desired.id;
    let mut request = CvmNetworkEditRequest {
        request_id: "atomic-network-entry-0001".into(),
        expected_generation: instance.desired.generation,
        mounts: vec![],
        detach_interface_ids: vec![],
        entry_additions: vec![CvmNetworkEntryAddition {
            publication_id: publication.id,
            address: SHARED.parse().unwrap(),
            mode: tcp(30052, 8080),
        }],
        entry_releases: vec![],
        routes: None,
    };
    let before = network_edit_counts(&db, id).await;
    let preview = db
        .store
        .edit_cvm_network(
            &actor,
            id,
            request.clone(),
            CvmMutationReview {
                preview: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(network_edit_counts(&db, id).await, before);
    let saved = db
        .store
        .edit_cvm_network(
            &actor,
            id,
            request.clone(),
            CvmMutationReview {
                expected_input_sha256: Some(preview.input_sha256),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        saved.instance.desired.generation,
        instance.desired.generation
    );
    assert_eq!(saved.instance.operation_kind.as_deref(), Some("entries"));
    assert!(saved.instance.revision_id > instance.revision_id);
    assert_eq!(network_edit_counts(&db, id).await, (before.0 + 1, 0, 1));
    let replayed = db
        .store
        .edit_cvm_network(&actor, id, request.clone(), CvmMutationReview::default())
        .await
        .unwrap();
    assert_eq!(replayed.instance.revision_id, saved.instance.revision_id);
    request.entry_additions[0].mode = tcp(30053, 8080);
    assert!(matches!(
        db.store
            .edit_cvm_network(&actor, id, request.clone(), CvmMutationReview::default())
            .await,
        Err(StoreError::Conflict(_))
    ));

    let entry = db
        .store
        .cvm_instance_entries(&actor, id)
        .await
        .unwrap()
        .remove(0);
    request.request_id = "atomic-network-entry-0002".into();
    request.entry_additions.clear();
    request.entry_releases = vec![CvmNetworkEntryRelease {
        id: entry.id.clone(),
        expected_revision: entry.revision,
    }];
    let bad = db
        .store
        .edit_cvm_network(
            &actor,
            &other.desired.id,
            request.clone(),
            CvmMutationReview::default(),
        )
        .await;
    assert!(matches!(bad, Err(StoreError::NotFound(_))));
    assert_eq!(network_edit_counts(&db, id).await, (before.0 + 1, 0, 1));
    db.store
        .edit_cvm_network(&actor, id, request, CvmMutationReview::default())
        .await
        .unwrap();
    let still_held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cvm_nic_resources WHERE entry_id=$1 AND NOT released",
    )
    .bind(entry.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(still_held, 1);
    let released: bool =
        sqlx::query_scalar("SELECT released FROM cvm_entries WHERE instance_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(released);
}

async fn network_edit_counts(db: &TestPg, instance: &str) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM revisions),(SELECT count(*) FROM cvm_nic_allocations WHERE instance_id=$1 AND NOT released),(SELECT count(*) FROM cvm_entries WHERE instance_id=$1 AND NOT cleaned)")
        .bind(instance).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_protocol_policy_freezes_reservations_and_preserves_whole_address_claims() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let mut spec = entry_spec();
    spec.addresses[1].reserved_ports.tcp = vec![EntryPortRange { start: 22, end: 22 }];
    supersede_cvm_fixture(&db).await;
    let p = db
        .store
        .write_cvm_entry_publication(
            &system_admin(),
            None,
            EntryPublicationWrite {
                request_id: "protocol-publication-0001".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let instance = create(&db, "protocol-container-0001").await;
    let mut traffic = EntryTraffic::all();
    traffic.tcp = EntryTransportSelection::Forward {
        ports: EntryPortSet::All,
        mappings: vec![EntryPortMapping {
            listen: EntryPortRange {
                start: 30022,
                end: 30022,
            },
            target: EntryPortRange { start: 22, end: 22 },
        }],
    };
    let entry = add(
        &db,
        &p,
        entry_request(
            "protocol-entry-0001",
            &instance,
            WHOLE,
            EntryMode::Protocols { traffic },
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        claims(&db, &entry.id).await.len(),
        1,
        "an exclusive address is never expanded into per-port leases"
    );
    converge(&db, "n1").await;
    converge(&db, "n2").await;
    let old = db.store.cvm_entry_desired("n2").await.unwrap();
    assert_eq!(
        old.rules[0].reserved_ports.tcp,
        vec![EntryPortRange { start: 22, end: 22 }]
    );
    let mut spec = p.spec.clone();
    spec.addresses[1].reserved_ports.tcp.push(EntryPortRange {
        start: 8080,
        end: 8090,
    });
    supersede_cvm_fixture(&db).await;
    db.store
        .write_cvm_entry_publication(
            &system_admin(),
            Some(&p.id),
            EntryPublicationWrite {
                request_id: "protocol-publication-0002".into(),
                expected_revision: p.revision,
                spec,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_entry_desired("n2").await.unwrap(),
        old,
        "saving cannot mutate a frozen serving policy"
    );
    publish_cvm_intent(&db).await;
    let next = db.store.cvm_entry_desired("n2").await.unwrap();
    assert_eq!(next.rules[0].reserved_ports.tcp.len(), 2);
    let delivery = db.store.cvm_entry_desired("n1").await.unwrap();
    assert_eq!(
        delivery.deliveries[0].reserved_ports,
        next.rules[0].reserved_ports
    );
    assert_eq!(claims(&db, &entry.id).await.len(), 1);
    let replacement = create(&db, "protocol-repoint-target-01").await;
    sqlx::query("UPDATE cvm_nic_nodes SET capabilities=capabilities-'entry_protocol_rules' WHERE node_id='n1'").execute(db.pool()).await.unwrap();
    supersede_cvm_fixture(&db).await;
    assert!(matches!(
        db.store
            .repoint_cvm_entry(
                &system_admin(),
                &entry.id,
                EntryRepoint {
                    request_id: "protocol-repoint-old-agent-01".into(),
                    expected_revision: entry.revision,
                    instance_id: replacement.desired.id
                }
            )
            .await,
        Err(StoreError::Unsupported(_))
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_entry_protocol_policy_shared_reservations_and_capability_gate() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let mut spec = entry_spec();
    spec.addresses[0].reserved_ports.tcp = vec![EntryPortRange {
        start: 30050,
        end: 30051,
    }];
    spec.addresses[0].reserved_ports.tcp.push(EntryPortRange {
        start: 30060,
        end: 30060,
    });
    supersede_cvm_fixture(&db).await;
    let p = db
        .store
        .write_cvm_entry_publication(
            &system_admin(),
            None,
            EntryPublicationWrite {
                request_id: "shared-protocol-publication-01".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let instance = create(&db, "shared-protocol-container-01").await;
    let traffic = EntryTraffic {
        tcp: EntryTransportSelection::Forward {
            ports: EntryPortSet::Ranges {
                ranges: vec![EntryPortRange {
                    start: 30048,
                    end: 30053,
                }],
            },
            mappings: vec![],
        },
        udp: EntryTransportSelection::Disabled,
        icmp: false,
        other_ip: false,
    };
    let request = entry_request(
        "shared-protocol-entry-01",
        &instance,
        SHARED,
        EntryMode::Protocols { traffic },
    );
    sqlx::query("UPDATE cvm_nic_nodes SET capabilities=capabilities-'entry_protocol_rules' WHERE node_id='n2'").execute(db.pool()).await.unwrap();
    assert!(matches!(
        add(&db, &p, request.clone()).await,
        Err(StoreError::Unsupported(_))
    ));
    sqlx::query("UPDATE cvm_nic_nodes SET capabilities=jsonb_set(capabilities,'{entry_protocol_rules}','true') WHERE node_id='n2'").execute(db.pool()).await.unwrap();
    let entry = add(&db, &p, request).await.unwrap();
    let held = claims(&db, &entry.id).await;
    assert_eq!(held.len(), 4);
    assert!(held
        .iter()
        .all(|(_, value, _)| !value.ends_with(":30050") && !value.ends_with(":30051")));
    converge(&db, "n1").await;
    converge(&db, "n2").await;
    let mut revised = p.spec.clone();
    revised.addresses[0]
        .reserved_ports
        .tcp
        .retain(|r| r.start != 30060);
    supersede_cvm_fixture(&db).await;
    let revised = db
        .store
        .write_cvm_entry_publication(
            &system_admin(),
            Some(&p.id),
            EntryPublicationWrite {
                request_id: "shared-reservation-remove-01".into(),
                expected_revision: p.revision,
                spec: revised,
            },
        )
        .await
        .unwrap();
    let candidate = entry_request(
        "shared-reuse-reservation-01",
        &instance,
        SHARED,
        tcp(30060, 22),
    );
    assert!(matches!(
        add(&db, &revised, candidate.clone()).await,
        Err(StoreError::Conflict(_))
    ));
    publish_cvm_intent(&db).await;
    // A failed receipt must not drop the previous reservation protection.
    let desired = db.store.cvm_entry_desired("n2").await.unwrap();
    db.store
        .cvm_record_entry_report(
            "n2",
            EntryNodeReport {
                generation: desired.generation,
                rules: vec![],
                deliveries: vec![],
                tunnels: vec![],
                diagnostics: None,
                error: Some("fixture apply failure".into()),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        add(&db, &revised, candidate.clone()).await,
        Err(StoreError::Conflict(_))
    ));
    converge(&db, "n2").await;
    add(&db, &revised, candidate).await.unwrap();
    let mut spec = revised.spec.clone();
    spec.addresses[0].reserved_ports.tcp.push(EntryPortRange {
        start: 30052,
        end: 30052,
    });
    supersede_cvm_fixture(&db).await;
    assert!(matches!(
        db.store
            .write_cvm_entry_publication(
                &system_admin(),
                Some(&p.id),
                EntryPublicationWrite {
                    request_id: "shared-protocol-publication-02".into(),
                    expected_revision: revised.revision,
                    spec
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_cvm_entry_repair_keeps_both_peers_in_the_same_executable_wave() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    entry_setup(&db).await;
    let instance = create(&db, "isolated-entry-instance-01").await;
    let publication = publish_entries(&db).await;
    db.store
        .isolate_node(
            &system_admin(),
            "n1",
            IsolateNodeRequest {
                acknowledge_uncertain: true,
            },
        )
        .await
        .unwrap();
    let entry = add(
        &db,
        &publication,
        entry_request(
            "isolated-entry-create-0001",
            &instance,
            SHARED,
            tcp(30110, 8080),
        ),
    )
    .await
    .unwrap();
    let revision: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let order = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision as u64, "isolated-entry-order-0001"),
        )
        .await
        .unwrap();
    let a = order
        .plan
        .targets
        .iter()
        .find(|t| t.node_id == "n1")
        .unwrap();
    let b = order
        .plan
        .targets
        .iter()
        .find(|t| t.node_id == "n2")
        .unwrap();
    assert_eq!(
        a.status,
        brocade_deployment::plan::PlannedTargetStatus::Pending
    );
    assert_eq!(a.status, b.status);
    assert_eq!(a.wave, b.wave);
    for node in ["n1", "n2"] {
        let claim = db
            .store
            .claim_desired_for_node(node)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.deployment_id, order.deployment_id);
        db.store
            .report_target_result(applied_report(&claim))
            .await
            .unwrap();
    }
    converge(&db, "n1").await;
    assert_ne!(
        target_status(db.pool(), order.deployment_id, "n2").await,
        "succeeded",
        "one peer receipt cannot confirm the pair"
    );
    converge(&db, "n2").await;
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    assert_eq!(
        target_status(db.pool(), order.deployment_id, "n1").await,
        "succeeded"
    );
    assert_eq!(
        target_status(db.pool(), order.deployment_id, "n2").await,
        "succeeded"
    );
    assert!(db
        .store
        .cvm_entry_desired("n2")
        .await
        .unwrap()
        .rules
        .iter()
        .any(|r| r.id == entry.id));
    let isolated: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM node_operational_isolations WHERE node_id='n1')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(isolated);
    let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM node_convergence_obligations WHERE node_id='n1' AND status IN ('pending','dispatched','converging','failed-recovered','failed-dirty')").fetch_one(db.pool()).await.unwrap();
    assert_eq!(debt, 0);
}
