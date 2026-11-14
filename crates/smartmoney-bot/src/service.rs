//! Bot service: outgoing notifications, allowlisted commands and the
//! daily digest.

use crate::client::{TelegramClient, Update};
use crate::format::{
    alert_message, digest_message, fill_message, help_message, not_allowed_message, settle_message,
    DigestAccount,
};
use chrono::TimeZone;
use chrono_tz::Europe::Berlin;
use smartmoney_store::{ScoredTrade, Store};
use std::sync::Arc;
use tokio::sync::watch;

const STATE_OFFSET: &str = "bot.updates_offset";
const STATE_LAST_DIGEST: &str = "bot.last_digest";
const DIGEST_HOUR: u32 = 19;
const POLL_TIMEOUT_S: u32 = 25;

pub struct Bot {
    client: TelegramClient,
    store: Store,
    allowed: Vec<i64>,
}

impl Bot {
    pub fn new(client: TelegramClient, store: Store, allowed: Vec<i64>) -> Self {
        Self {
            client,
            store,
            allowed,
        }
    }

    /// Send to every allowed chat; failures are logged per chat.
    pub async fn notify(&self, html: &str) {
        for chat in &self.allowed {
            if let Err(e) = self.client.send_message(*chat, html).await {
                tracing::warn!(chat, error = %e, "telegram notify failed");
            }
        }
    }

    pub async fn notify_alert(&self, s: &ScoredTrade) {
        self.notify(&alert_message(s)).await;
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn notify_fill(
        &self,
        strategy: &str,
        label: &str,
        shares: f64,
        avg_price: f64,
        cost_usd: f64,
        fee_usd: f64,
    ) {
        self.notify(&fill_message(
            strategy, label, shares, avg_price, cost_usd, fee_usd,
        ))
        .await;
    }

    pub async fn notify_settle(
        &self,
        strategy: &str,
        title: &str,
        shares: f64,
        payout_usd: f64,
        realized_pnl: f64,
    ) {
        self.notify(&settle_message(
            strategy,
            title,
            shares,
            payout_usd,
            realized_pnl,
        ))
        .await;
    }

    /// Start of the current day in Berlin, as a unix timestamp.
    fn berlin_midnight(now: i64) -> i64 {
        let local = Berlin.timestamp_opt(now, 0).unwrap().date_naive();
        Berlin
            .from_local_datetime(&local.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.timestamp())
            .unwrap_or(now)
    }

    async fn status_message(&self, now: i64) -> anyhow::Result<String> {
        let accounts = self.store.sim_accounts().await?;
        let open = self.store.open_sim_positions().await?;
        let settled = self
            .store
            .sim_settled_since(Self::berlin_midnight(now))
            .await?;
        let mut rows = Vec::new();
        for a in accounts {
            let equity = self
                .store
                .sim_equity(a.id, 1)
                .await?
                .last()
                .map(|(_, e)| *e);
            let today: Vec<_> = settled.iter().filter(|p| p.account_id == a.id).collect();
            rows.push(DigestAccount {
                strategy: a.strategy,
                cash: a.cash,
                equity,
                open: open.iter().filter(|p| p.account_id == a.id).count(),
                settled_today: today.len(),
                realized_today: today.iter().filter_map(|p| p.realized_pnl).sum(),
            });
        }
        let date = Berlin
            .timestamp_opt(now, 0)
            .unwrap()
            .format("%d.%m.")
            .to_string();
        Ok(digest_message(&date, &rows))
    }

    async fn handle_update(&self, update: &Update) {
        let Some(message) = &update.message else {
            return;
        };
        let chat = message.chat.id;
        let text = message.text.as_deref().unwrap_or_default().trim();
        if !self.allowed.contains(&chat) {
            tracing::info!(chat, "message from unknown chat");
            let _ = self
                .client
                .send_message(chat, &not_allowed_message(chat))
                .await;
            return;
        }
        let reply = if text.starts_with("/status") {
            match self.status_message(chrono::Utc::now().timestamp()).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "status failed");
                    "Status derzeit nicht verfügbar.".to_owned()
                }
            }
        } else {
            help_message(chat)
        };
        if let Err(e) = self.client.send_message(chat, &reply).await {
            tracing::warn!(chat, error = %e, "telegram reply failed");
        }
    }

    /// Send the daily digest once per Berlin day after DIGEST_HOUR.
    async fn maybe_digest(&self, now: i64) -> anyhow::Result<()> {
        if self.allowed.is_empty() {
            return Ok(());
        }
        let local = Berlin.timestamp_opt(now, 0).unwrap();
        if local.format("%H").to_string().parse::<u32>().unwrap_or(0) < DIGEST_HOUR {
            return Ok(());
        }
        let today = local.format("%Y-%m-%d").to_string();
        if self.store.get_state(STATE_LAST_DIGEST).await?.as_deref() == Some(today.as_str()) {
            return Ok(());
        }
        let message = self.status_message(now).await?;
        self.notify(&message).await;
        self.store.set_state(STATE_LAST_DIGEST, &today, now).await?;
        Ok(())
    }

    /// Long-poll loop for commands plus the digest timer.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut offset: i64 = match self.store.get_state(STATE_OFFSET).await {
            Ok(v) => v.and_then(|s| s.parse().ok()).unwrap_or(0),
            Err(_) => 0,
        };
        loop {
            if let Err(e) = self.maybe_digest(chrono::Utc::now().timestamp()).await {
                tracing::error!(error = %e, "digest failed");
            }
            tokio::select! {
                updates = self.client.get_updates(offset, POLL_TIMEOUT_S) => {
                    match updates {
                        Ok(list) => {
                            for u in &list {
                                offset = offset.max(u.update_id + 1);
                                self.handle_update(u).await;
                            }
                            if !list.is_empty() {
                                let now = chrono::Utc::now().timestamp();
                                let _ = self
                                    .store
                                    .set_state(STATE_OFFSET, &offset.to_string(), now)
                                    .await;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "telegram poll failed");
                            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartmoney_core::Secret;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn bot(server: &MockServer, allowed: Vec<i64>) -> Bot {
        let store = Store::open_in_memory().await.unwrap();
        let client = TelegramClient::new(Secret::new("T"))
            .unwrap()
            .with_base(&server.uri());
        Bot::new(client, store, allowed)
    }

    fn update(chat: i64, text: &str) -> Update {
        serde_json::from_value(serde_json::json!({
            "update_id": 1,
            "message": {"chat": {"id": chat}, "text": text}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn unknown_chat_gets_its_id_allowed_chat_gets_status() {
        let server = MockServer::start().await;
        let sent = Mock::given(method("POST"))
            .and(path("/botT/sendMessage"))
            .and(body_partial_json(serde_json::json!({"chat_id": 7})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "result": {}
            })))
            .expect(1)
            .named("reply to unknown chat");
        server.register(sent).await;
        let b = bot(&server, vec![42]).await;
        b.handle_update(&update(7, "/status")).await;
        server.verify().await;

        // Allowed chat asking /status gets the digest text.
        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botT/sendMessage"))
            .and(body_partial_json(serde_json::json!({"chat_id": 42})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "result": {}
            })))
            .expect(1)
            .mount(&server2)
            .await;
        let b2 = bot(&server2, vec![42]).await;
        b2.handle_update(&update(42, "/status")).await;
    }

    #[tokio::test]
    async fn digest_sends_once_per_day_after_hour() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botT/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "result": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let b = bot(&server, vec![42]).await;
        // 20:00 Berlin on 2026-11-14 = 19:00 UTC.
        let evening = chrono::DateTime::parse_from_rfc3339("2026-11-14T19:00:00Z")
            .unwrap()
            .timestamp();
        let morning = chrono::DateTime::parse_from_rfc3339("2026-11-14T08:00:00Z")
            .unwrap()
            .timestamp();
        b.maybe_digest(morning).await.unwrap();
        b.maybe_digest(evening).await.unwrap();
        b.maybe_digest(evening + 60).await.unwrap();
    }
}
