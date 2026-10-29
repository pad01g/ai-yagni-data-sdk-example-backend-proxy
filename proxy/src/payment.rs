//! 決済サービス（payment）への接続。proxy だけが持つ。
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct ChargeRequest<'a> {
    idempotency_key: &'a str,
    amount: u64,
    currency: &'a str,
    card_token: &'a str,
}

#[derive(Deserialize)]
struct ChargeResponse {
    id: Option<String>,
    status: Option<String>,
    reason: Option<String>,
    error: Option<String>,
}

/// 請求の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Charge {
    Succeeded { id: String },
    Declined { reason: String },
}

#[derive(Clone)]
pub struct PaymentClient {
    base: String,
    http: reqwest::Client,
}

impl PaymentClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_owned(), http: reqwest::Client::new() }
    }

    /// 請求する。同じ冪等キーには、決済サービスが同じ結果を返す（注文をやり直しても二重に請求しない）。
    pub async fn charge(&self, key: &str, amount: u64, currency: &str, card_token: &str) -> Result<Charge, String> {
        let body = ChargeRequest { idempotency_key: key, amount, currency, card_token };
        let r = self
            .http
            .post(format!("{}/charges", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("payment unreachable: {e}"))?;
        let status = r.status();
        let b: ChargeResponse = r.json().await.map_err(|e| format!("payment answered {status}: {e}"))?;
        match (status.as_u16(), b.status.as_deref(), b.id) {
            (201, Some("succeeded"), Some(id)) => Ok(Charge::Succeeded { id }),
            (402, _, _) => Ok(Charge::Declined { reason: b.reason.unwrap_or_else(|| "declined".to_owned()) }),
            _ => Err(format!("payment answered {status}: {}", b.error.unwrap_or_default())),
        }
    }
}
