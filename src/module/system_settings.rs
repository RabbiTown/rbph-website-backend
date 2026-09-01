use std::time::Duration;

use deadpool_redis::redis::{self, AsyncCommands};
use futures_util::StreamExt;

use crate::{AppState, db};

const SETTINGS_CHANNEL: &str = "rbph:system-settings:updated";

async fn apply_latest_from_db(app: &AppState) {
    let Ok(settings) = db::system_settings::get(&app.db).await else {
        log::error!("failed to reconcile system settings");
        return;
    };

    let mut guard = app.system_settings.write().await;
    if guard.updated_at >= settings.updated_at {
        return;
    }

    *guard = settings.clone();
    drop(guard);

    app.sync_hub
        .enforce_connection_limit(settings.max_websocket_connections as usize)
        .await;
}

pub async fn publish_updated(app: &AppState) {
    let mut conn = match app.kv.get().await {
        Ok(conn) => conn,
        Err(error) => {
            log::error!("failed to get redis connection for system settings notification: {error}");
            return;
        }
    };

    let payload =
        crate::serde_helpers::format_offset_datetime(&app.system_settings.read().await.updated_at);
    let result: redis::RedisResult<()> = conn.publish(SETTINGS_CHANNEL, payload).await;
    if let Err(error) = result {
        log::error!("failed to publish system settings notification: {error}");
    }
}

pub async fn run_reconciler(app: AppState) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        apply_latest_from_db(&app).await;
    }
}

pub async fn run_subscriber(app: AppState) {
    loop {
        let client = match redis::Client::open(app.settings.app.kv_addr.as_str()) {
            Ok(client) => client,
            Err(error) => {
                log::error!(
                    "failed to create redis client for system settings subscriber: {error}"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut pubsub = match client.get_async_pubsub().await {
            Ok(pubsub) => pubsub,
            Err(error) => {
                log::error!("failed to connect system settings subscriber: {error}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        if let Err(error) = pubsub.subscribe(SETTINGS_CHANNEL).await {
            log::error!("failed to subscribe system settings updates: {error}");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let mut stream = pubsub.on_message();
        while let Some(message) = stream.next().await {
            if let Err(error) = message.get_payload::<String>() {
                log::error!("invalid system settings notification payload: {error}");
                continue;
            }
            apply_latest_from_db(&app).await;
        }

        log::error!("system settings subscriber disconnected");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
