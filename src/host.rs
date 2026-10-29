//! 通过公开 SDK 与 SMTP 发送边界访问外部服务
use crate::model::{SmtpSettings, State};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{QuotaFacts, QuotaFactsQuery, VerifyClientRequest},
        host::{StateGetRequest, StateGetResult, StatePutRequest, StatePutResult},
        key_budgets::{BudgetPeriod, GetKeyBudgetRequest, KeyBudget, ResetKeyBudgetRequest},
    },
    client::{HostClient, HttpBody, HttpFrame, HttpRequest, SessionError},
};
use std::future::Future;

/// 日、周用量在宿主同一事务中清零，只调用一次，避免只清一项后结果未知
pub fn reset_request(key: &str) -> ResetKeyBudgetRequest {
    ResetKeyBudgetRequest {
        client_key_id: key.into(),
        period: BudgetPeriod::All,
    }
}

pub fn fault(message: impl Into<String>) -> PluginFault {
    PluginFault::new(ErrorCode::Rejected, message)
}
pub trait Host: Sync {
    fn verify_client(
        &self,
        _authorization: &str,
    ) -> impl Future<Output = Result<String, PluginFault>> + Send {
        async { Err(fault("此宿主未实现客户端认证")) }
    }
    fn reset_daily(&self, _key: &str) -> impl Future<Output = Result<(), PluginFault>> + Send {
        async { Err(fault("此宿主未实现日限清零")) }
    }
    fn statistics(
        &self,
        _dimension: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> impl Future<Output = Result<crate::stats::DiagnosticCosts, PluginFault>> + Send {
        async { Err(fault("此宿主未实现统计查询")) }
    }
    fn probe(
        &self,
        _key: &str,
        _account: &str,
    ) -> impl Future<Output = Result<(), PluginFault>> + Send {
        async { Err(fault("此宿主未实现模型测试")) }
    }
    fn email(
        &self,
        settings: &SmtpSettings,
        recipient: &str,
        subject: &str,
        body: &str,
    ) -> impl Future<Output = Result<(), PluginFault>> + Send {
        crate::delivery::send_email(settings, recipient, subject, body)
    }
    fn key_name(&self, key: &str) -> impl Future<Output = Result<String, PluginFault>> + Send {
        async move { Ok(key.to_owned()) }
    }
    fn load(&self) -> impl Future<Output = Result<(State, Option<u64>), PluginFault>> + Send;
    fn save(
        &self,
        state: &State,
        version: Option<u64>,
    ) -> impl Future<Output = Result<u64, PluginFault>> + Send;
    fn refresh(
        &self,
        account: &str,
    ) -> impl Future<Output = Result<QuotaFacts, PluginFault>> + Send;
    fn budget(&self, key: &str) -> impl Future<Output = Result<KeyBudget, PluginFault>> + Send;
    fn reset(&self, key: &str) -> impl Future<Output = Result<(), PluginFault>> + Send;
}
pub struct SdkHost(pub HostClient);
impl Host for SdkHost {
    async fn verify_client(&self, authorization: &str) -> Result<String, PluginFault> {
        self.0
            .verify_client(VerifyClientRequest {
                authorization: authorization.into(),
            })
            .await
            .map(|client| client.client_key_id)
    }
    async fn reset_daily(&self, key: &str) -> Result<(), PluginFault> {
        self.0
            .reset_key_budget(ResetKeyBudgetRequest {
                client_key_id: key.into(),
                period: BudgetPeriod::Daily,
            })
            .await
            .map(|_| ())
    }
    async fn statistics(
        &self,
        dimension: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<crate::stats::DiagnosticCosts, PluginFault> {
        let format_time = |value| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
                .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .ok_or_else(|| fault("统计时间无效"))
        };
        if !matches!(dimension, "account" | "apiKey") {
            return Err(fault("统计维度无效"));
        }
        let start = format_time(start_ms)?.replace(':', "%3A");
        let end = format_time(end_ms)?.replace(':', "%3A");
        let request = HttpRequest {
            settings: serde_json::Value::Null,
            method: "GET".into(),
            uri: format!(
                "/api/admin/usage/insights/diagnostics?dimension={dimension}&startTime={start}&endTime={end}"
            ),
            version: gateway_plugin_sdk::call::middleware::http::Version::Http11,
            headers: vec![],
            timeout_ms: Some(1_000),
            body: HttpBody::empty(),
        };
        let mut response = self.0.dispatch_http(request).await?;
        if response.status != 200 {
            return Err(fault("统计接口不可用"));
        }
        let mut body = Vec::new();
        while let Some(frame) = response.body.read().await? {
            if let HttpFrame::Data(bytes) = frame {
                if body.len().saturating_add(bytes.len()) > 1_048_576 {
                    return Err(fault("统计响应过大"));
                }
                body.extend(bytes);
            }
        }
        crate::stats::parse_diagnostics(&body)
    }
    async fn key_name(&self, key: &str) -> Result<String, PluginFault> {
        let mut cursor = None;
        loop {
            let page = self
                .0
                .list_keys(gateway_plugin_sdk::call::host::KeyListRequest { cursor, limit: 200 })
                .await?;
            if let Some(found) = page.keys.into_iter().find(|k| k.id == key) {
                return Ok(found.name);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Err(fault("Key 不存在"));
            }
        }
    }
    async fn probe(&self, key: &str, account: &str) -> Result<(), PluginFault> {
        let reply = self
            .0
            .call(
                "host.model.execute",
                serde_json::json!({
                    "client_key_id": key, "account_id": account, "model": "gpt-6.1-sol",
                    "protocol": "openai", "operation": "generate"
                }),
                br#"{"model":"gpt-6.1-sol","input":"hello"}"#.to_vec(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let batch = gateway_plugin_sdk::call::host::ModelEventBatch::decode(&reply.payload)
            .map_err(|_| fault("模型响应解析失败"))?;
        if !batch.events.iter().flat_map(|e| &e.facts).any(|e| {
            matches!(
                e,
                gateway_plugin_sdk::call::model::CanonicalEvent::Completed { .. }
            )
        }) {
            return Err(fault("未确认模型完成"));
        }
        Ok(())
    }

    async fn load(&self) -> Result<(State, Option<u64>), PluginFault> {
        let reply = self
            .0
            .call(
                "host.state.get",
                serde_json::to_value(StateGetRequest {
                    namespace: "follow".into(),
                    key: "state".into(),
                })
                .map_err(|_| fault("状态查询编码失败"))?,
                vec![],
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: StateGetResult =
            serde_json::from_value(reply.result).map_err(|_| fault("状态响应解析失败"))?;
        match result.record {
            Some(record) => Ok((
                serde_json::from_value(record.value)
                    .map_err(|_| fault("状态格式不兼容，已停止处理"))?,
                Some(record.version),
            )),
            None => Ok((State::default(), None)),
        }
    }
    async fn save(&self, state: &State, version: Option<u64>) -> Result<u64, PluginFault> {
        let request = StatePutRequest {
            namespace: "follow".into(),
            key: "state".into(),
            value: serde_json::to_value(state).map_err(|_| fault("状态编码失败"))?,
            expected_version: version,
        };
        let reply = self
            .0
            .call(
                "host.state.put",
                serde_json::to_value(request).map_err(|_| fault("状态编码失败"))?,
                vec![],
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: StatePutResult =
            serde_json::from_value(reply.result).map_err(|_| fault("状态写入结果未知"))?;
        Ok(result.version)
    }
    async fn refresh(&self, account: &str) -> Result<QuotaFacts, PluginFault> {
        self.0
            .refresh_account_quota(QuotaFactsQuery {
                account_id: account.into(),
            })
            .await
    }
    async fn budget(&self, key: &str) -> Result<KeyBudget, PluginFault> {
        self.0
            .get_key_budget(GetKeyBudgetRequest {
                client_key_id: key.into(),
            })
            .await
    }
    async fn reset(&self, key: &str) -> Result<(), PluginFault> {
        self.0
            .reset_key_budget(reset_request(key))
            .await
            .map(|_| ())
    }
}
