//! 決済サービス（payment）への接続。proxy だけが持つ。
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct ChargeRequest<'a> {
    idempotency_key: &'a str,
    amount: u64,
    currency: &'a str,
    card_token: &'a str,
}

#[derive(Deserialize)]
struct FoundCharge {
    id: String,
    status: String,
    reason: Option<String>,
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

/// 請求できなかった。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentError {
    /// 届かない、時間切れ、混んでいる。同じ冪等キーでやり直してよい。
    Unavailable(String),
    /// 同じ冪等キーで、前と違う中身の請求をした（カードを替えた、など）。
    Conflict,
    /// 依頼の形が違う（proxy の誤り）。
    Invalid(String),
}

#[derive(Clone)]
pub struct PaymentClient {
    base: String,
    http: reqwest::Client,
}

impl PaymentClient {
    /// timeout は 1 回の請求を待つ上限。
    pub fn new(base: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1).min(timeout))
            .timeout(timeout)
            .build()
            .expect("http client");
        Self { base: base.into().trim_end_matches('/').to_owned(), http }
    }

    /// 請求する。同じ冪等キーには、決済サービスが同じ結果を返す（やり直しても二重に請求しない）。
    pub async fn charge(
        &self,
        key: &str,
        amount: u64,
        currency: &str,
        card_token: &str,
    ) -> Result<Charge, PaymentError> {
        let body = ChargeRequest { idempotency_key: key, amount, currency, card_token };
        let r = self
            .http
            .post(format!("{}/charges", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| PaymentError::Unavailable(e.to_string()))?;
        let status = r.status();
        let b: ChargeResponse =
            r.json().await.map_err(|e| PaymentError::Unavailable(format!("payment answered {status}: {e}")))?;
        match (status.as_u16(), b.status.as_deref(), b.id) {
            (201, Some("succeeded"), Some(id)) => Ok(Charge::Succeeded { id }),
            (402, _, _) => Ok(Charge::Declined { reason: b.reason.unwrap_or_default() }),
            (409, _, _) => Err(PaymentError::Conflict),
            (400, _, _) => Err(PaymentError::Invalid(b.error.unwrap_or_default())),
            _ => Err(PaymentError::Unavailable(format!("payment answered {status}: {}", b.error.unwrap_or_default()))),
        }
    }
}

impl PaymentClient {
    /// 冪等キーで決済を探す。その冪等キーで請求が届いていなければ None。
    pub async fn find(&self, key: &str) -> Result<Option<Charge>, PaymentError> {
        let r = self
            .http
            .get(format!("{}/charges", self.base))
            .query(&[("idempotency_key", key)])
            .send()
            .await
            .map_err(|e| PaymentError::Unavailable(e.to_string()))?;
        match r.status().as_u16() {
            404 => Ok(None),
            200 => {
                let c: FoundCharge = r.json().await.map_err(|e| PaymentError::Unavailable(e.to_string()))?;
                Ok(Some(if c.status == "succeeded" {
                    Charge::Succeeded { id: c.id }
                } else {
                    Charge::Declined { reason: c.reason.unwrap_or_default() }
                }))
            }
            s => Err(PaymentError::Unavailable(format!("payment answered {s}"))),
        }
    }
}

/// 拒否の理由のうち、利用者に見せてよいもの。知らない理由は "declined" にまとめる。
pub fn public_reason(reason: &str) -> &'static str {
    match reason {
        "card declined" => "card declined",
        "over the card limit" => "over the card limit",
        "unknown card" => "unknown card",
        _ => "declined",
    }
}
