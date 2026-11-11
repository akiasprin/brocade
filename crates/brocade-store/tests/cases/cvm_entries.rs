use super::*;

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
                    contract: NIC_PROTOCOL_VERSION,
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
                address: SHARED.parse().unwrap(),
                public_address: None,
                kind: EntryAddressKind::Shared {
                    port_start: 30000,
                    port_end: 30999,
                },
            },
            EntryAddressOffer {
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
        max_entries: 8,
    }
}
async fn publish_entries(db: &TestPg) -> EntryPublication {
    db.store
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
    EntryMode::L4 {
        protocol: EntryProtocol::Tcp,
        listen_port,
        target_port,
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
    db.store
        .create_cvm_entry(&actor_user("alice"), request)
        .await
}
/// Reports full convergence of a node's current entry desired state.
async fn converge(db: &TestPg, node: &str) {
    let desired = db.store.cvm_entry_desired(node).await.unwrap();
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
            EntryMode::L4 {
                protocol: EntryProtocol::Udp,
                listen_port: 30022,
                target_port: 53,
            },
        ),
    )
    .await
    .unwrap();
    for (key, mode) in [
        (
            "entry-create-both-0001",
            EntryMode::L4 {
                protocol: EntryProtocol::TcpUdp,
                listen_port: 30022,
                target_port: 22,
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
    let entry_side = db.store.cvm_entry_desired("n2").await.unwrap();
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
    let host_side = db.store.cvm_entry_desired("n1").await.unwrap();
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
        db.store
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
    let moved = db
        .store
        .repoint_cvm_entry(
            &actor_user("alice"),
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
    let released = db
        .store
        .release_cvm_entry(
            &actor_user("alice"),
            &dns.id,
            EntryRelease {
                request_id: "entry-release-dns-0001".into(),
                expected_revision: dns.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(released.state, CvmEntryState::Releasing);
    let again = EntryMode::L4 {
        protocol: EntryProtocol::Udp,
        listen_port: 30022,
        target_port: 53,
    };
    assert!(matches!(
        add(
            &db,
            &p,
            entry_request("entry-create-dns-0002", &web, SHARED, again)
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
    let delete = db
        .store
        .command_cvm(
            &actor_user("alice"),
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
    for entry in [&first, &second] {
        db.store
            .release_cvm_entry(
                &actor_user("alice"),
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
    assert!(db
        .store
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
    assert_eq!(held, 0);
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
    let desired = db.store.cvm_entry_desired("n2").await.unwrap();
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
    assert!(view
        .entries
        .iter()
        .any(|e| e.id == third.id && e.state == CvmEntryState::Pending));
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
async fn cvm_entry_publications_protect_machine_addresses_and_live_entries() {
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
    for (key, spec) in [
        ("entry-pub-remote-0001", remote),
        ("entry-pub-own-0001", own),
    ] {
        assert!(matches!(
            db.store
                .write_cvm_entry_publication(&system_admin(), None, write(key, 0, spec))
                .await,
            Err(StoreError::InvalidData(_))
        ));
    }
    assert!(matches!(
        db.store
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
        db.store
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
        db.store
            .write_cvm_entry_publication(
                &system_admin(),
                Some(&p.id),
                write("entry-pub-narrow-0001", p.revision, narrowed)
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        db.store
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
    let p = db
        .store
        .write_cvm_entry_publication(
            &system_admin(),
            Some(&p.id),
            write("entry-pub-revoke-0001", p.revision, revoked),
        )
        .await
        .unwrap();
    assert_eq!(p.revision, 2);
    let rules = db.store.cvm_entry_desired("n2").await.unwrap().rules;
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
    let web = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
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
    db.store
        .release_cvm_entry(
            &actor_user("alice"),
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
