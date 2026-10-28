//! 插件管理路由、会话与串行维护入口
use gateway_plugin_sdk::{
    PluginFault,
    call::{
        host::{AuthListRequest, AuthListResult, KeyListRequest},
        management::*,
    },
    client::{Empty, PluginBuilder, PluginSession, SessionConfig, TypedCall, TypedReply, methods},
};
use key_reset_follow::{
    engine::Engine,
    host::{Host, SdkHost, fault},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    action: String,
    key_id: Option<String>,
    account_id: Option<String>,
    version: Option<u64>,
    enabled: Option<bool>,
    display_timezone: Option<key_reset_follow::model::DisplayTimezone>,
    probe_enabled: Option<bool>,
    smtp: Option<key_reset_follow::model::SmtpSettings>,
    #[serde(default)]
    clear_smtp_password: bool,
    recipient: Option<String>,
    delivery_id: Option<u64>,
    #[serde(default)]
    acknowledge: bool,
}
fn update_smtp(
    state: &mut key_reset_follow::model::State,
    mut smtp: key_reset_follow::model::SmtpSettings,
    clear: bool,
) -> Result<(), PluginFault> {
    if clear {
        smtp.password.clear();
    } else if smtp.password.is_empty() {
        smtp.password.clone_from(&state.smtp.password);
    }
    key_reset_follow::delivery::validate(&smtp)?;
    state.smtp = smtp;
    Ok(())
}
fn public_state(state: &key_reset_follow::model::State) -> key_reset_follow::model::State {
    let mut result = state.clone();
    result.smtp.password.clear();
    result
}
fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: vec![
            ManagementRoute {
                method: "GET".into(),
                path: "state".into(),
                request_content_types: vec![],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "action".into(),
                request_content_types: vec!["application/json".into()],
                response_content_types: vec!["application/json".into()],
            },
        ],
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "follow".into(),
            title: "Key 周额度跟随账号重置".into(),
            description: Some("确认账号新周期后同步清零关联 Key 日、周用量".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: vec![],
    }
}
async fn handle(call: TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    let host = SdkHost(call.host);
    let mut engine = Engine::load(&host).await?;
    if call.request.method == "POST" {
        let cmd: Command =
            serde_json::from_slice(&call.payload).map_err(|_| fault("请求格式不正确"))?;
        if cmd.action != "preview" && cmd.version != engine.version {
            return Err(fault("页面状态已变化，请刷新后重试"));
        }
        let key = cmd.key_id.as_deref().unwrap_or("");
        match cmd.action.as_str() {
            "preview" => {
                return Ok(
                    json!({"sample": engine.fresh(cmd.account_id.as_deref().ok_or_else(|| fault("请选择账号"))?).await?}),
                );
            }
            "add" => {
                engine
                    .add(
                        key,
                        cmd.account_id
                            .as_deref()
                            .ok_or_else(|| fault("请选择账号"))?,
                        now(),
                    )
                    .await?
            }
            "settings" => {
                engine.state.early_auto = cmd.enabled.ok_or_else(|| fault("缺少开关值"))?;
                if let Some(timezone) = cmd.display_timezone {
                    engine.state.display_timezone = timezone;
                }
                if let Some(enabled) = cmd.probe_enabled {
                    engine.state.probe_enabled = enabled;
                }
                if let Some(smtp) = cmd.smtp {
                    update_smtp(&mut engine.state, smtp, cmd.clear_smtp_password)?;
                }
                engine.save().await?;
            }
            "recipient" => {
                let recipient = cmd.recipient.unwrap_or_default().trim().to_owned();
                key_reset_follow::delivery::validate_recipient(&recipient)?;
                engine
                    .state
                    .links
                    .get_mut(key)
                    .ok_or_else(|| fault("关联不存在"))?
                    .recipient = recipient;
                engine.save().await?;
            }
            "manual_probe" | "manual_mail" => {
                engine
                    .manual_test(
                        key,
                        cmd.action == "manual_probe",
                        cmd.acknowledge,
                        cmd.version,
                        now(),
                    )
                    .await?;
            }
            "retry_probe" | "retry_mail" => {
                engine
                    .retry_delivery(
                        key,
                        cmd.delivery_id.ok_or_else(|| fault("缺少发送记录"))?,
                        cmd.action == "retry_probe",
                        cmd.acknowledge,
                    )
                    .await?;
            }
            "check" => {
                let account = engine
                    .state
                    .links
                    .get(key)
                    .ok_or_else(|| fault("关联不存在"))?
                    .account_id
                    .clone();
                engine.check(&account, now()).await?;
            }
            action => engine.action(key, action, cmd.acknowledge, now()).await?,
        }
        return Ok(json!({"ok": true}));
    }
    let mut keys = vec![];
    let mut cursor = None;
    loop {
        let page = host
            .0
            .list_keys(KeyListRequest { cursor, limit: 200 })
            .await?;
        keys.extend(page.keys);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let mut accounts = vec![];
    let mut cursor = None;
    loop {
        let reply = host
            .0
            .call(
                "host.auth.list",
                json!({}),
                serde_json::to_vec(&AuthListRequest {
                    provider_id: None,
                    cursor,
                    limit: 200,
                })
                .map_err(|_| fault("账号查询编码失败"))?,
            )
            .await
            .map_err(gateway_plugin_sdk::client::SessionError::into_plugin_fault)?;
        let page: AuthListResult =
            serde_json::from_slice(&reply.payload).map_err(|_| fault("账号目录解析失败"))?;
        accounts.extend(page.accounts.into_iter().map(|a| {
            json!({
                "account_id": a.account_id, "name": a.name, "email": a.email,
                "provider_id": a.provider_id, "authentication_kind": a.authentication_kind,
            })
        }));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let mut budgets = serde_json::Map::new();
    for key in &keys {
        if let Ok(budget) = host.budget(&key.id).await {
            budgets.insert(key.id.clone(), json!(budget));
        }
    }
    // 密码仅写入私有状态，管理响应不回显
    let smtp_password_configured = !engine.state.smtp.password.is_empty();
    let state = public_state(&engine.state);
    Ok(
        json!({"version": engine.version, "state": state, "smtp_password_configured": smtp_password_configured, "keys": keys, "accounts": accounts, "budgets": budgets}),
    )
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lock = Arc::new(Mutex::new(()));
    let management_lock = lock.clone();
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .management(registration(), move |call| {
            let lock = management_lock.clone();
            async move {
                let _guard = lock.lock().await;
                let (status, body) = match handle(call).await {
                    Ok(v) => (200, v),
                    Err(e) => (400, json!({"error": e.message})),
                };
                Ok(TypedReply::new(ManagementResponse {
                    status,
                    content_type: "application/json".into(),
                    headers: vec![],
                })
                .with_payload(serde_json::to_vec(&body).unwrap()))
            }
        })?
        .on(methods::RECONCILE, move |call| {
            let lock = lock.clone();
            async move {
                let _guard = lock.lock().await;
                tokio::time::timeout(std::time::Duration::from_secs(25), async {
                    let host = SdkHost(call.host);
                    let mut engine = Engine::load(&host).await?;
                    engine.deliver_one().await?;
                    engine.drain(now()).await?;
                    let mut due: Vec<_> = engine
                        .state
                        .accounts
                        .iter()
                        .filter(|(id, a)| {
                            a.next_check <= now()
                                && engine
                                    .state
                                    .links
                                    .values()
                                    .any(|l| &l.account_id == *id && !l.paused)
                        })
                        .map(|(id, a)| (a.next_check, id.clone()))
                        .collect();
                    due.sort();
                    for (_, id) in due.into_iter().take(2) {
                        engine.check(&id, now()).await?;
                    }
                    Ok(TypedReply::new(Empty {}))
                })
                .await
                .map_err(|_| fault("维护达到时间上限，未完成发送保持未知结果"))?
            }
        })?
        .build()?;
    PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?
    .run(plugin)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use key_reset_follow::model::State;

    #[test]
    fn smtp_password_is_preserved_replaced_and_explicitly_cleared() {
        let mut state = State::default();
        state.smtp.password = "synthetic-secret".into();
        update_smtp(&mut state, Default::default(), false).unwrap();
        assert_eq!(state.smtp.password, "synthetic-secret");
        let mut replacement = state.smtp.clone();
        replacement.password = "synthetic-replacement".into();
        update_smtp(&mut state, replacement, false).unwrap();
        assert_eq!(state.smtp.password, "synthetic-replacement");
        update_smtp(&mut state, Default::default(), true).unwrap();
        assert!(state.smtp.password.is_empty());
    }
    #[test]
    fn public_state_never_exposes_saved_password() {
        let mut state = State::default();
        state.smtp.password = "synthetic-secret".into();
        let response = serde_json::to_string(&public_state(&state)).unwrap();
        assert!(!response.contains("synthetic-secret"));
        assert_eq!(state.smtp.password, "synthetic-secret");
    }
}
