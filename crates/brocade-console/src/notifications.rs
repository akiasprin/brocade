use std::{env, sync::Arc, time::Duration};

use brocade_store::{ClaimedNotificationDelivery, PgStore};
use reqwest::{redirect::Policy, Client, Url};
use serde::Serialize;

const PRESENCE_TICK: Duration = Duration::from_secs(15);
const DELIVERY_IDLE_TICK: Duration = Duration::from_secs(2);
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize)]
struct WebhookEnvelope<'a> {
    schema_version: u8,
    event: &'a brocade_store::MachineEventView,
}

pub async fn spawn(store: PgStore) {
    let webhook = webhook_from_env();
    let channel_ready = match store
        .configure_webhook_notification_channel(webhook.is_some())
        .await
    {
        Ok(0) => true,
        Ok(suppressed) => {
            eprintln!("notifications: Webhook 未配置，已抑制 {suppressed} 条历史待投递事件");
            true
        }
        Err(error) => {
            eprintln!("notifications: 更新 Webhook 通道状态失败：{error}");
            false
        }
    };

    let presence_store = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRESENCE_TICK);
        loop {
            tick.tick().await;
            match presence_store.reconcile_node_presence().await {
                Ok(0) => {}
                Ok(count) => eprintln!("notifications: {count} 台机器转为离线"),
                Err(error) => eprintln!("notifications: 检查机器在线状态失败：{error}"),
            }
        }
    });

    let Some(webhook) = webhook else {
        return;
    };
    if !channel_ready {
        return;
    }
    let client = match Client::builder()
        .timeout(WEBHOOK_TIMEOUT)
        // A redirect to a different host would copy the event payload to an origin the operator
        // did not configure. Webhook endpoints should answer directly.
        .redirect(Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("notifications: Webhook HTTP 客户端初始化失败：{error}");
            return;
        }
    };
    let owner: Arc<str> = format!("console-{}", std::process::id()).into();
    tokio::spawn(async move {
        loop {
            match store.claim_notification_delivery(&owner).await {
                Ok(Some(delivery)) => {
                    deliver(&store, &client, &webhook, &owner, delivery).await;
                }
                Ok(None) => tokio::time::sleep(DELIVERY_IDLE_TICK).await,
                Err(error) => {
                    eprintln!("notifications: 领取 Webhook 投递失败：{error}");
                    tokio::time::sleep(DELIVERY_IDLE_TICK).await;
                }
            }
        }
    });
}

fn webhook_from_env() -> Option<Url> {
    let raw = env::var("BROCADE_NOTIFICATION_WEBHOOK_URL").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Some(url),
        Ok(_) => {
            eprintln!("notifications: 忽略 BROCADE_NOTIFICATION_WEBHOOK_URL，只支持 http/https");
            None
        }
        Err(error) => {
            // Never print the configured value: webhook URLs commonly carry a secret path/query.
            eprintln!("notifications: BROCADE_NOTIFICATION_WEBHOOK_URL 无效：{error}");
            None
        }
    }
}

async fn deliver(
    store: &PgStore,
    client: &Client,
    webhook: &Url,
    owner: &str,
    delivery: ClaimedNotificationDelivery,
) {
    let outcome = client
        .post(webhook.clone())
        .json(&WebhookEnvelope {
            // v2 adds structured sustained-metric incident fields and event kinds. A strict v1
            // consumer must not mistake a cpu_steal transition for a presence transition.
            schema_version: 2,
            event: &delivery.event,
        })
        .send()
        .await;
    match outcome {
        Ok(response) if response.status().is_success() => {
            if let Err(error) = store
                .complete_notification_delivery(delivery.delivery_id, owner, delivery.attempt)
                .await
            {
                eprintln!("notifications: 提交 Webhook 成功状态失败：{error}");
            }
        }
        Ok(response) => {
            let detail = format!("webhook returned HTTP {}", response.status().as_u16());
            fail(store, owner, &delivery, &detail).await;
        }
        Err(error) => {
            // reqwest's Display includes the request URL. Keep secrets out of both logs and the
            // outbox row; the category is sufficient for retry diagnostics.
            let detail = if error.is_timeout() {
                "webhook request timed out"
            } else if error.is_connect() {
                "webhook connection failed"
            } else if error.is_request() {
                "webhook request failed"
            } else {
                "webhook transport failed"
            };
            fail(store, owner, &delivery, detail).await;
        }
    }
}

async fn fail(store: &PgStore, owner: &str, delivery: &ClaimedNotificationDelivery, detail: &str) {
    match store
        .fail_notification_delivery(delivery.delivery_id, owner, delivery.attempt, detail)
        .await
    {
        Ok(true) => eprintln!(
            "notifications: Webhook 投递失败，第 {} 次尝试稍后重试（{detail}）",
            delivery.attempt
        ),
        Ok(false) => {}
        Err(error) => eprintln!("notifications: 保存 Webhook 重试状态失败：{error}"),
    }
}
