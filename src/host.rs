//! 通过公开 SDK 与 SMTP 发送边界访问外部服务
use crate::model::{SmtpSettings, State};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        data::{QuotaFacts, QuotaFactsQuery},
        host::{StateGetRequest, StateGetResult, StatePutRequest, StatePutResult},
        key_budgets::{BudgetPeriod, GetKeyBudgetRequest, KeyBudget, ResetKeyBudgetRequest},
    },
    client::{HostClient, SessionError},
};
use std::future::Future;

pub fn fault(message: impl Into<String>) -> PluginFault {
    PluginFault::new(ErrorCode::Rejected, message)
}
pub trait Host: Sync {
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
            .reset_key_budget(ResetKeyBudgetRequest {
                client_key_id: key.into(),
                period: BudgetPeriod::Weekly,
            })
            .await
            .map(|_| ())
    }
}
