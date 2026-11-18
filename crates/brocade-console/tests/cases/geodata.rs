use super::*;
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct List {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<Site>,
}
#[derive(Clone, PartialEq, Message)]
struct Site {
    #[prost(string, tag = "1")]
    code: String,
    #[prost(message, repeated, tag = "2")]
    domain: Vec<Domain>,
}
#[derive(Clone, PartialEq, Message)]
struct Domain {
    #[prost(int32, tag = "1")]
    kind: i32,
    #[prost(string, tag = "2")]
    value: String,
    #[prost(message, repeated, tag = "3")]
    attribute: Vec<Attr>,
}
#[derive(Clone, PartialEq, Message)]
struct Attr {
    #[prost(string, tag = "1")]
    key: String,
}
#[derive(Clone, PartialEq, Message)]
struct IpList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<IpSite>,
}
#[derive(Clone, PartialEq, Message)]
struct IpSite {
    #[prost(string, tag = "1")]
    code: String,
    #[prost(message, repeated, tag = "2")]
    cidr: Vec<Cidr>,
}
#[derive(Clone, PartialEq, Message)]
struct Cidr {
    #[prost(bytes = "vec", tag = "1")]
    ip: Vec<u8>,
    #[prost(uint32, tag = "2")]
    prefix: u32,
}
fn ip_dat() -> Vec<u8> {
    IpList {
        entry: vec![
            IpSite {
                code: "CN".into(),
                cidr: vec![
                    Cidr {
                        ip: vec![192, 0, 2, 0],
                        prefix: 24,
                    },
                    Cidr {
                        ip: "2001:db8::"
                            .parse::<std::net::Ipv6Addr>()
                            .unwrap()
                            .octets()
                            .to_vec(),
                        prefix: 32,
                    },
                ],
            },
            IpSite {
                code: "JP".into(),
                cidr: vec![Cidr {
                    ip: vec![198, 51, 100, 0],
                    prefix: 24,
                }],
            },
        ],
    }
    .encode_to_vec()
}

fn dat() -> Vec<u8> {
    let domain = |value: &str, attr: &str| Domain {
        kind: 2,
        value: value.into(),
        attribute: if attr.is_empty() {
            vec![]
        } else {
            vec![Attr { key: attr.into() }]
        },
    };
    List {
        entry: vec![
            Site {
                code: "CATEGORY-AI-!CN".into(),
                domain: vec![domain("openai.com", ""), domain("huggingface.co", "")],
            },
            Site {
                code: "OPENAI".into(),
                domain: vec![domain("openai.com", "")],
            },
            Site {
                code: "HUGGINGFACE".into(),
                domain: vec![domain("huggingface.co", "")],
            },
            Site {
                code: "GOOGLE".into(),
                domain: vec![domain("google.com", "cn"), domain("googleapis.com", "ads")],
            },
            Site {
                code: "NETFLIX".into(),
                domain: vec![domain("netflix.com", "")],
            },
        ],
    }
    .encode_to_vec()
}
fn rule(m: Value) -> Value {
    json!({"m":m,"a":{"t":"egress","send_through":null}})
}
fn op(rules: Vec<Value>) -> Value {
    json!({"op":"put_step","app_id":"app-a1b2","chain_id":"chn-b2c3-d4e5","node_id":"n1","step":{"rules":rules}})
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn geodata_validation_rejects_p0_writes_atomically_and_preserves_unknown_old_values() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = Router::new()
        .route("/geosite.dat", axum::routing::get(|| async { dat() }))
        .route("/geoip.dat", axum::routing::get(|| async { ip_dat() }));
    let worker = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
    let source = format!("http://{address}/geosite.dat");
    sqlx::query("UPDATE control_state SET geodata_geosite_url=$1")
        .bind(&source)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE control_state SET geodata_geoip_url=$1")
        .bind(format!("http://{address}/geoip.dat"))
        .execute(db.pool())
        .await
        .unwrap();
    refresh_current_model_snapshot(&db).await;
    let (app, token) = admin_app(&db).await;
    let baseline: Value = sqlx::query_scalar("SELECT rules FROM steps WHERE node_id='n1'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT current_revision FROM control_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let cases = [
        json!({"t":"geosite","v":["does-not-exist"]}),
        json!({"t":"geosite","v":["google@jp"]}),
        json!({"t":"geoip","v":["cn@ipv5"]}),
        json!({"t":"geoip","v":["cn@ipv4@ipv6"]}),
        json!({"t":"geoip","v":["does-not-exist"]}),
        json!({"t":"domain_keyword","v":["ext:/private/file:cn"]}),
        json!({"t":"ip_cidr","v":["192.0.2.0/33"]}),
        json!({"t":"ip_cidr","v":["ext:/private/file:cn"]}),
        json!({"t":"port","v":["70000"]}),
        json!({"t":"port","v":["8000-7000"]}),
        json!({"t":"domain_regex","v":"(?=openai)"}),
    ];
    for selector in cases {
        let (status,body)=post_json(&app,&token,"/model/apply",json!({"ops":[{"op":"upsert_app","app":{"id":"app-a1b2","label":"must roll back"}},op(vec![rule(selector.clone())])]})).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{selector}: {body}"
        );
        assert!(
            body["issues"].as_array().is_some_and(|v| !v.is_empty()),
            "{body}"
        );
        assert_eq!(body["issues"][0]["location"], "app-a1b2/chn-b2c3-d4e5/n1");
        let stored: Value = sqlx::query_scalar("SELECT rules FROM steps WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(stored, baseline);
        let label: String = sqlx::query_scalar("SELECT label FROM apps WHERE id='app-a1b2'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(label, "Main App");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        revision
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM revisions")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        count
    );
    for rules in [
        vec![rule(json!({"t":"geosite","v":["google","google"]}))],
        vec![
            rule(json!({"t":"geosite","v":["category-ai-!cn"]})),
            rule(json!({"t":"geosite","v":["openai"]})),
        ],
        vec![rule(
            json!({"t":"geosite","v":["openai","category-ai-!cn"]}),
        )],
        vec![
            rule(json!({"t":"domain_suffix","v":["openai.com"]})),
            rule(json!({"t":"domain_suffix","v":["api.openai.com"]})),
        ],
    ] {
        let (status, body) =
            post_json(&app, &token, "/model/apply", json!({"ops":[op(rules)]})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let valid = vec![
        rule(json!({"t":"geosite","v":["huggingface"]})),
        rule(json!({"t":"geosite","v":["category-ai-!cn"]})),
        rule(json!({"t":"any"})),
    ];
    let (status, body) = post_json(
        &app,
        &token,
        "/model/apply",
        json!({"ops":[op(valid.clone())]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, catalog) = get_json(&app, &token, "/routing/geodata/catalog?kind=geosite").await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["entries"].as_array().unwrap().len(), 5);
    let (status, page) = get_json(
        &app,
        &token,
        "/routing/geodata/geosite/google?attr=cn&limit=1&target=www.google.com",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["hit"], true);
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    let (status, ip_catalog) = get_json(&app, &token, "/routing/geodata/catalog?kind=geoip").await;
    assert_eq!(status, StatusCode::OK, "{ip_catalog}");
    assert_eq!(ip_catalog["entries"][0]["attrs"], json!(["ipv4", "ipv6"]));
    for (code, target, hit) in [
        ("cn", "192.0.2.10", true),
        ("!cn", "192.0.2.10", false),
        ("!cn", "198.51.100.1", true),
    ] {
        let (status, page) = get_json(
            &app,
            &token,
            &format!("/routing/geodata/geoip/{code}?attr=ipv4&limit=1&target={target}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["hit"], hit);
    }
    let mut ip_rules = valid.clone();
    ip_rules.insert(0, rule(json!({"t":"geoip","v":["!cn@ipv4"]})));
    let (status, body) = post_json(
        &app,
        &token,
        "/model/validate",
        json!({"ops":[op(ip_rules)]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Source replacement validates references, including untouched tables.
    let mut settings = db.store.settings().await.unwrap();
    settings.geodata.geosite_url = format!("http://{address}/missing");
    let (status, body) = post_json(
        &app,
        &token,
        "/model/apply",
        json!({"ops":[{"op":"update_settings","settings":settings}]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let mut settings = db.store.settings().await.unwrap();
    settings.geodata.cron = "CRON_TZ=Unknown/Nowhere @daily".into();
    let (status, body) = post_json(
        &app,
        &token,
        "/model/apply",
        json!({"ops":[{"op":"update_settings","settings":settings}]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["issues"][0]["field"], "geodata.cron");
    // A temporarily unavailable, unchanged source permits only values already in this table.
    sqlx::query("UPDATE control_state SET geodata_geosite_url='http://127.0.0.1:1/unavailable'")
        .execute(db.pool())
        .await
        .unwrap();
    let (status, body) = post_json(
        &app,
        &token,
        "/model/apply",
        json!({"ops":[op(valid.clone())]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut changed = valid;
    changed.insert(0, rule(json!({"t":"geosite","v":["netflix"]})));
    let (status, body) =
        post_json(&app, &token, "/model/apply", json!({"ops":[op(changed)]})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    worker.abort();
    let _ = worker.await;
}
