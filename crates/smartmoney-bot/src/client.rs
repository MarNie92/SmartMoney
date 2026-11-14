//! Minimal Telegram Bot API client. The token never appears in logs or
//! error messages.

use serde::Deserialize;
use smartmoney_core::Secret;

pub const TELEGRAM_BASE: &str = "https://api.telegram.org";

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub chat: Chat,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone)]
pub struct TelegramClient {
    http: reqwest::Client,
    token: Secret,
    base: String,
}

impl TelegramClient {
    pub fn new(token: Secret) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(40))
                .build()?,
            token,
            base: TELEGRAM_BASE.to_owned(),
        })
    }

    /// Override the API base URL (tests).
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.to_owned();
        self
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token.expose())
    }

    /// Send an HTML-formatted message. Errors carry no URL or token.
    pub async fn send_message(&self, chat_id: i64, html: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .post(self.url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("telegram send failed: {}", e.without_url()))?;
        let status = response.status();
        let body: Envelope<serde_json::Value> = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("telegram response: {}", e.without_url()))?;
        if !body.ok {
            anyhow::bail!(
                "telegram sendMessage {status}: {}",
                body.description.unwrap_or_default()
            );
        }
        Ok(())
    }

    /// Long-poll for updates after `offset`.
    pub async fn get_updates(&self, offset: i64, timeout_s: u32) -> anyhow::Result<Vec<Update>> {
        let response = self
            .http
            .get(self.url("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", timeout_s.to_string()),
                ("allowed_updates", "[\"message\"]".to_owned()),
            ])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("telegram poll failed: {}", e.without_url()))?;
        let body: Envelope<Vec<Update>> = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("telegram response: {}", e.without_url()))?;
        if !body.ok {
            anyhow::bail!(
                "telegram getUpdates: {}",
                body.description.unwrap_or_default()
            );
        }
        Ok(body.result.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> TelegramClient {
        TelegramClient::new(Secret::new("TOKEN123"))
            .unwrap()
            .with_base(&server.uri())
    }

    #[tokio::test]
    async fn sends_messages_and_hides_token_in_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN123/sendMessage"))
            .and(body_partial_json(serde_json::json!({
                "chat_id": 42, "parse_mode": "HTML"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "result": {}
            })))
            .mount(&server)
            .await;
        client(&server).send_message(42, "<b>hi</b>").await.unwrap();

        // API-level error: message passes the description through only.
        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "ok": false, "description": "Forbidden: bot was blocked"
            })))
            .mount(&server2)
            .await;
        let err = client(&server2).send_message(42, "x").await.unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("Forbidden"), "{text}");
        assert!(!text.contains("TOKEN123"), "token must never leak: {text}");
    }

    #[tokio::test]
    async fn polls_updates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/botTOKEN123/getUpdates"))
            .and(query_param("offset", "7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": [
                    {"update_id": 7, "message": {"chat": {"id": 99}, "text": "/status"}},
                    {"update_id": 8, "message": null}
                ]
            })))
            .mount(&server)
            .await;
        let updates = client(&server).get_updates(7, 0).await.unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].message.as_ref().unwrap().chat.id, 99);
        assert_eq!(
            updates[0].message.as_ref().unwrap().text.as_deref(),
            Some("/status")
        );
        assert!(updates[1].message.is_none());
    }
}
