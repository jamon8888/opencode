use std::sync::Arc;
use dashmap::DashMap;
use anyhow::Result;

use crate::cap::{UsageCap, BillingError};

/// Legacy aggregate — local stub until this file is replaced in Task 5.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageRecord {
    pub api_key_id:     String,
    pub month:          String,
    pub tokens_in:      u64,
    pub tokens_out:     u64,
    pub requests_count: u32,
}

pub struct BillingClient {
    pub http:     reqwest::Client,
    pub ch_url:   String,
    pub caps:     Arc<DashMap<String, UsageCap>>,
    pub monthly:  Arc<DashMap<String, UsageRecord>>,  // api_key_id → current month usage
}

impl BillingClient {
    pub fn new(clickhouse_url: String) -> Self {
        Self {
            http:    reqwest::Client::new(),
            ch_url:  clickhouse_url,
            caps:    Arc::new(DashMap::new()),
            monthly: Arc::new(DashMap::new()),
        }
    }

    pub fn set_cap(&self, api_key_id: &str, cap: UsageCap) {
        self.caps.insert(api_key_id.to_string(), cap);
    }

    pub fn check_cap(&self, api_key_id: &str, estimated_tokens: u64) -> Result<(), BillingError> {
        let Some(cap) = self.caps.get(api_key_id) else { return Ok(()); };
        let month = current_month();
        if let Some(usage) = self.monthly.get(&format!("{api_key_id}:{month}")) {
            if usage.tokens_in + usage.tokens_out + estimated_tokens > cap.monthly_tokens {
                return Err(BillingError::CapExceeded(
                    format!("monthly token cap {} exceeded", cap.monthly_tokens)
                ));
            }
            if usage.requests_count >= cap.monthly_requests {
                return Err(BillingError::CapExceeded(
                    format!("monthly request cap {} exceeded", cap.monthly_requests)
                ));
            }
        }
        Ok(())
    }

    pub async fn record_usage(
        &self,
        api_key_id: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) {
        let month = current_month();
        let key = format!("{api_key_id}:{month}");
        self.monthly
            .entry(key)
            .and_modify(|u| {
                u.tokens_in  += tokens_in;
                u.tokens_out += tokens_out;
                u.requests_count += 1;
            })
            .or_insert(UsageRecord {
                api_key_id: api_key_id.to_string(),
                month: month.clone(),
                tokens_in,
                tokens_out,
                requests_count: 1,
            });

        // Fire-and-forget: write to ClickHouse gdpr_billing
        let url  = self.ch_url.clone();
        let http = self.http.clone();
        let row = serde_json::json!({
            "api_key_id":      api_key_id,
            "month":           month,
            "tokens_in":       tokens_in,
            "tokens_out":      tokens_out,
            "documents_count": 0u32,
            "requests_count":  1u32,
        });
        tokio::spawn(async move {
            let body = serde_json::to_string(&row).unwrap_or_default();
            let _ = http
                .post(format!("{url}/"))
                .query(&[("query", "INSERT INTO gdpr_billing FORMAT JSONEachRow")])
                .header("Content-Type", "application/x-ndjson")
                .body(body)
                .send().await;
        });
    }

    pub async fn monthly_usage(&self, api_key_id: &str, month: &str) -> Result<UsageRecord> {
        let key = format!("{api_key_id}:{month}");
        if let Some(u) = self.monthly.get(&key) {
            return Ok(u.clone());
        }
        // Query ClickHouse
        let query = format!(
            "SELECT sum(tokens_in), sum(tokens_out), sum(requests_count) FROM gdpr_billing WHERE api_key_id='{}' AND month='{}'",
            api_key_id.replace('\'', ""), month.replace('\'', "")
        );
        let resp = self.http
            .get(format!("{}/", self.ch_url))
            .query(&[("query", &query)])
            .send().await?;
        // Parse TSV response
        let text = resp.text().await?;
        let parts: Vec<&str> = text.trim().split('\t').collect();
        let tokens_in      = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let tokens_out     = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let requests_count = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok(UsageRecord {
            api_key_id: api_key_id.to_string(),
            month: month.to_string(),
            tokens_in,
            tokens_out,
            requests_count,
        })
    }
}

fn current_month() -> String {
    chrono::Utc::now().format("%Y-%m").to_string()
}
