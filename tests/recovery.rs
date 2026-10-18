//! 周期清零及后续发送在故障、并发和重启下的恢复测试
use gateway_plugin_sdk::{
    PluginFault,
    call::{
        data::{QuotaFacts, QuotaWindowFacts},
        key_budgets::KeyBudget,
    },
};
use key_reset_follow::{
    cycle::WEEK,
    engine::Engine,
    host::{Host, fault},
    model::{Pending, State},
};
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Default)]
struct Storage {
    state: State,
    version: u64,
    resets: Vec<String>,
    probes: Vec<String>,
    mails: Vec<String>,
    fail_probe: bool,
    fail_mail: bool,
    refreshes: usize,
    time: i64,
    reset_at: i64,
    fail_save_at: Option<u64>,
    lost_response: Option<String>,
    hang_reset: bool,
    missing_window: bool,
    changed_window: bool,
    fail_budget: Option<String>,
    weekly_used: BTreeMap<String, String>,
    used_percent: f64,
}
struct Fake(Mutex<Storage>);
impl Fake {
    fn new() -> Self {
        Self(Mutex::new(Storage {
            time: 1000000,
            reset_at: WEEK,
            used_percent: 10.0,
            ..Default::default()
        }))
    }
}
impl Host for Fake {
    async fn probe(&self, key: &str, _account: &str) -> Result<(), PluginFault> {
        let mut s = self.0.lock().unwrap();
        assert_eq!(
            s.state.links[key].deliveries.last().unwrap().probe,
            key_reset_follow::model::SendStatus::Unknown
        );
        s.probes.push(key.into());
        if s.fail_probe {
            Err(fault("response lost"))
        } else {
            Ok(())
        }
    }
    async fn email(
        &self,
        _settings: &key_reset_follow::model::SmtpSettings,
        recipient: &str,
        body: &str,
    ) -> Result<(), PluginFault> {
        assert!(body.contains("周额度已重置"));
        let mut s = self.0.lock().unwrap();
        s.mails.push(recipient.into());
        if s.fail_mail {
            Err(fault("mail response lost"))
        } else {
            Ok(())
        }
    }

    async fn load(&self) -> Result<(State, Option<u64>), PluginFault> {
        let s = self.0.lock().unwrap();
        Ok((s.state.clone(), (s.version > 0).then_some(s.version)))
    }
    async fn save(&self, state: &State, version: Option<u64>) -> Result<u64, PluginFault> {
        let mut s = self.0.lock().unwrap();
        if version.unwrap_or(0) != s.version || s.fail_save_at == Some(s.version + 1) {
            return Err(fault("injected state failure"));
        }
        s.version += 1;
        s.state = state.clone();
        Ok(s.version)
    }
    async fn refresh(&self, account: &str) -> Result<QuotaFacts, PluginFault> {
        let mut s = self.0.lock().unwrap();
        s.refreshes += 1;
        Ok(QuotaFacts {
            schema_version: 1,
            account_id: account.into(),
            observed_at_ms: Some(s.time),
            windows: if s.missing_window {
                vec![]
            } else {
                vec![QuotaWindowFacts {
                    key: if s.changed_window {
                        "codex:604800s:secondary_window"
                    } else {
                        "codex:604800s"
                    }
                    .into(),
                    window_seconds: Some(604800),
                    used_percent: Some(s.used_percent),
                    reset_at_ms: Some(s.reset_at),
                }]
            },
        })
    }
    async fn budget(&self, key: &str) -> Result<KeyBudget, PluginFault> {
        if self.0.lock().unwrap().fail_budget.as_deref() == Some(key) {
            return Err(fault("budget unavailable"));
        }
        Ok(KeyBudget {
            client_key_id: key.into(),
            daily_limit_usd: "0".into(),
            weekly_limit_usd: "60".into(),
            daily_used_usd: "0".into(),
            weekly_used_usd: self
                .0
                .lock()
                .unwrap()
                .weekly_used
                .get(key)
                .cloned()
                .unwrap_or_else(|| "12".into()),
            daily_resets_at_ms: None,
            weekly_resets_at_ms: Some(WEEK),
        })
    }
    async fn reset(&self, key: &str) -> Result<(), PluginFault> {
        let hang = self.0.lock().unwrap().hang_reset;
        if hang {
            std::future::pending::<()>().await;
        }
        let mut s = self.0.lock().unwrap();
        s.resets.push(key.into());
        s.weekly_used.insert(key.into(), "0".into());
        if s.lost_response.as_deref() == Some(key) {
            Err(fault("response lost after commit"))
        } else {
            Ok(())
        }
    }
}
async fn setup(h: &Fake) {
    let mut e = Engine::load(h).await.unwrap();
    e.add("key-a", "account-a", 1000000).await.unwrap();
    e.add("key-b", "account-a", 1000000).await.unwrap();
}
fn next(h: &Fake) {
    let mut s = h.0.lock().unwrap();
    s.time = WEEK + 1000;
    s.reset_at = WEEK * 2;
}
#[tokio::test]
async fn shared_refresh_and_partial_unknown_do_not_repeat_success() {
    let h = Fake::new();
    setup(&h).await;
    assert!(h.0.lock().unwrap().resets.is_empty());
    next(&h);
    h.0.lock().unwrap().lost_response = Some("key-b".into());
    let before = h.0.lock().unwrap().refreshes;
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().refreshes, before + 1);
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-a", "key-b"]);
    Engine::load(&h)
        .await
        .unwrap()
        .drain(WEEK + 2000)
        .await
        .unwrap();
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 2000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets.len(), 2);
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-b"].pending,
        Some(Pending::Unknown { .. })
    ));
}
#[tokio::test]
async fn completion_write_failure_cannot_repeat_reset() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    // check 保存待处理、保存意图后，故障发生于完成落盘。
    h.0.lock().unwrap().fail_save_at = Some(5);
    assert!(
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", WEEK + 1000)
            .await
            .is_err()
    );
    h.0.lock().unwrap().fail_save_at = None;
    Engine::load(&h)
        .await
        .unwrap()
        .drain(WEEK + 2000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-a", "key-b"]);
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-a"].pending,
        Some(Pending::Unknown { .. })
    ));
}
#[tokio::test]
async fn failed_intent_write_does_not_call_reset() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().fail_save_at = Some(4);
    assert!(
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", WEEK + 1000)
            .await
            .is_err()
    );
    assert!(h.0.lock().unwrap().resets.is_empty());
}
#[tokio::test]
async fn early_switch_and_manual_skip_preserve_usage() {
    let h = Fake::new();
    setup(&h).await;
    {
        let mut s = h.0.lock().unwrap();
        s.state.early_auto = false;
        s.time = 2000000;
        s.reset_at = WEEK + 1500000;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", 2000000)
        .await
        .unwrap();
    assert!(h.0.lock().unwrap().resets.is_empty());
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-a"].pending,
        Some(Pending::Confirm { .. })
    ));
    Engine::load(&h)
        .await
        .unwrap()
        .action("key-a", "skip", false, 2000000)
        .await
        .unwrap();
    assert!(h.0.lock().unwrap().resets.is_empty());
    assert_eq!(
        h.0.lock().unwrap().state.links["key-a"].baseline.reset,
        WEEK + 1500000
    );
}
#[tokio::test]
async fn concurrent_stale_state_cannot_claim_reset() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    let mut old = Engine::load(&h).await.unwrap();
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    assert!(old.check("account-a", WEEK + 1000).await.is_err());
    assert_eq!(h.0.lock().unwrap().resets.len(), 2);
}

#[tokio::test]
async fn termination_after_durable_intent_never_replays() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().hang_reset = true;
    let mut e = Engine::load(&h).await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            e.check("account-a", WEEK + 1000)
        )
        .await
        .is_err()
    );
    drop(e);
    h.0.lock().unwrap().hang_reset = false;
    Engine::load(&h)
        .await
        .unwrap()
        .drain(WEEK + 2000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b"]);
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-a"].pending,
        Some(Pending::Unknown { .. })
    ));
}
#[tokio::test]
async fn pause_resume_and_accounts_are_independent() {
    let h = Fake::new();
    setup(&h).await;
    Engine::load(&h)
        .await
        .unwrap()
        .add("key-c", "account-b", 1000000)
        .await
        .unwrap();
    Engine::load(&h)
        .await
        .unwrap()
        .action("key-a", "pause", false, 1000000)
        .await
        .unwrap();
    next(&h);
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b"]);
    Engine::load(&h)
        .await
        .unwrap()
        .action("key-a", "resume", false, WEEK + 2000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b"]);
    assert_eq!(
        h.0.lock().unwrap().state.links["key-c"].baseline.reset,
        WEEK
    );
}
#[tokio::test]
async fn missing_samples_keep_baseline_and_changed_identity_requires_confirmation() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().missing_window = true;
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    assert_eq!(
        h.0.lock().unwrap().state.links["key-a"].baseline.reset,
        WEEK
    );
    assert!(h.0.lock().unwrap().resets.is_empty());
    {
        let mut s = h.0.lock().unwrap();
        s.missing_window = false;
        s.changed_window = true;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 2000)
        .await
        .unwrap();
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-a"].pending,
        Some(Pending::Changed { .. })
    ));
    assert!(h.0.lock().unwrap().resets.is_empty());
}
#[tokio::test]
async fn budget_failure_leaves_other_keys_progressing_and_retries_only_unclaimed_key() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().fail_budget = Some("key-a".into());
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b"]);
    h.0.lock().unwrap().fail_budget = None;
    Engine::load(&h)
        .await
        .unwrap()
        .drain(WEEK + 301000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b", "key-a"]);
}
#[tokio::test]
async fn early_auto_is_enabled_by_default_and_unknown_retry_requires_acknowledgement() {
    let h = Fake::new();
    setup(&h).await;
    {
        let mut s = h.0.lock().unwrap();
        s.time = 2000000;
        s.reset_at = WEEK + 1500000;
        s.lost_response = Some("key-a".into());
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", 2000000)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets.len(), 2);
    assert!(
        Engine::load(&h)
            .await
            .unwrap()
            .action("key-a", "retry", false, 2000000)
            .await
            .is_err()
    );
    assert_eq!(h.0.lock().unwrap().resets.len(), 2);
}

#[tokio::test]
async fn prolonged_outage_discards_queued_cycle_before_any_reset() {
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().fail_budget = Some("key-a".into());
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    h.0.lock().unwrap().fail_budget = None;
    Engine::load(&h)
        .await
        .unwrap()
        .drain(WEEK * 3)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b"]);
    assert!(h.0.lock().unwrap().state.links["key-a"].pending.is_none());
    {
        let mut s = h.0.lock().unwrap();
        s.time = WEEK * 3;
        s.reset_at = WEEK * 4;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK * 3)
        .await
        .unwrap();
    assert_eq!(h.0.lock().unwrap().resets, vec!["key-b", "key-a", "key-b"]);
    assert_eq!(
        h.0.lock().unwrap().state.links["key-a"].baseline.reset,
        WEEK * 4
    );
}

#[tokio::test]
async fn beta_state_and_pending_reset_survive_display_settings() {
    use key_reset_follow::model::DisplayTimezone;
    let h = Fake::new();
    setup(&h).await;
    next(&h);
    h.0.lock().unwrap().lost_response = Some("key-a".into());
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    let mut old = serde_json::to_value(&h.0.lock().unwrap().state).unwrap();
    old.as_object_mut().unwrap().remove("display_timezone");
    let recovered: State = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(recovered.display_timezone, DisplayTimezone::Browser);
    h.0.lock().unwrap().state = recovered;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.display_timezone = DisplayTimezone::Shanghai;
    e.state.early_auto = false;
    e.save().await.unwrap();
    let after = serde_json::to_value(&h.0.lock().unwrap().state).unwrap();
    assert_eq!(after["links"], old["links"]);
    assert_eq!(after["accounts"], old["accounts"]);
    assert_eq!(h.0.lock().unwrap().resets.len(), 2);
    assert!(serde_json::from_value::<DisplayTimezone>(serde_json::json!("unsupported")).is_err());
}

#[tokio::test]
async fn rolling_unused_window_resets_once_and_records_one_event_per_key() {
    let h = Fake::new();
    setup(&h).await;
    let first = 2_000_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = first;
        s.reset_at = first + WEEK;
        s.used_percent = 0.0;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", first)
        .await
        .unwrap();
    {
        let mut s = h.0.lock().unwrap();
        assert_eq!(s.resets, ["key-a", "key-b"]);
        s.weekly_used.insert("key-a".into(), "7".into());
    }
    let first_event = h.0.lock().unwrap().state.links["key-a"]
        .events
        .back()
        .unwrap()
        .at;
    for i in 1..=12 {
        let t = first + i * 300_000;
        {
            let mut s = h.0.lock().unwrap();
            s.time = t;
            s.reset_at = t + WEEK;
        }
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", t)
            .await
            .unwrap();
        let s = h.0.lock().unwrap();
        assert_eq!(s.resets, ["key-a", "key-b"], "poll {i}");
        assert_eq!(s.weekly_used["key-a"], "7");
        for key in ["key-a", "key-b"] {
            let events = &s.state.links[key].events;
            assert_eq!(events.iter().filter(|e| e.reason == "提前重置").count(), 1);
            assert_eq!(events.back().unwrap().at, first_event);
            assert!(matches!(
                s.state.links[key].phase,
                key_reset_follow::model::Phase::Settling { .. }
            ));
        }
    }
    let fixed_reset = h.0.lock().unwrap().reset_at;
    for i in 13..=15 {
        let t = first + i * 300_000;
        h.0.lock().unwrap().time = t;
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", t)
            .await
            .unwrap();
    }
    {
        let s = h.0.lock().unwrap();
        assert_eq!(s.resets.len(), 2);
        assert_eq!(s.state.links["key-a"].baseline.reset, fixed_reset);
        assert!(matches!(
            s.state.links["key-a"].phase,
            key_reset_follow::model::Phase::Tracking
        ));
    }
    let t = first + 20 * 300_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = t;
        s.reset_at = fixed_reset + 1_500_000;
        s.used_percent = 2.0;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", t)
        .await
        .unwrap();
    let s = h.0.lock().unwrap();
    assert_eq!(s.resets.len(), 4);
    assert_eq!(
        s.state.links["key-a"]
            .events
            .iter()
            .filter(|e| e.reason == "提前重置")
            .count(),
        2
    );
}

#[tokio::test]
async fn legacy_link_without_phase_waits_for_anchor_without_replaying_reset() {
    let h = Fake::new();
    setup(&h).await;
    let first = 2_000_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = first;
        s.reset_at = first + WEEK;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", first)
        .await
        .unwrap();
    let mut old = serde_json::to_value(&h.0.lock().unwrap().state).unwrap();
    for link in old["links"].as_object_mut().unwrap().values_mut() {
        link.as_object_mut().unwrap().remove("phase");
    }
    h.0.lock().unwrap().state = serde_json::from_value(old).unwrap();
    for i in 1..=3 {
        let t = first + i * 300_000;
        {
            let mut s = h.0.lock().unwrap();
            s.time = t;
            s.reset_at = t + WEEK;
        }
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", t)
            .await
            .unwrap();
    }
    let s = h.0.lock().unwrap();
    assert_eq!(s.resets, ["key-a", "key-b"]);
    assert_eq!(
        s.state.links["key-a"]
            .events
            .iter()
            .filter(|e| e.reason == "提前重置")
            .count(),
        1
    );
}

#[tokio::test]
async fn small_forward_shifts_do_not_accumulate_into_a_second_reset() {
    let h = Fake::new();
    setup(&h).await;
    let first = 2_000_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = first;
        s.reset_at = first + WEEK;
        s.used_percent = 0.0;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", first)
        .await
        .unwrap();
    for i in 1..=20 {
        let t = first + i * 300_000;
        {
            let mut s = h.0.lock().unwrap();
            s.time = t;
            s.reset_at = first + WEEK + i * 60_000;
        }
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", t)
            .await
            .unwrap();
        let s = h.0.lock().unwrap();
        assert_eq!(s.resets.len(), 2, "poll {i}");
        assert!(matches!(
            s.state.links["key-a"].phase,
            key_reset_follow::model::Phase::Settling { .. }
        ));
        assert!(s.state.links["key-a"].pending.is_none());
    }
}

#[tokio::test]
async fn new_and_resumed_links_wait_for_a_floating_window_to_settle() {
    let h = Fake::new();
    let first = 2_000_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = first;
        s.reset_at = first + WEEK;
        s.used_percent = 0.0;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .add("key-a", "account-a", first)
        .await
        .unwrap();
    assert!(matches!(
        h.0.lock().unwrap().state.links["key-a"].phase,
        key_reset_follow::model::Phase::Settling { .. }
    ));
    Engine::load(&h)
        .await
        .unwrap()
        .action("key-a", "pause", false, first)
        .await
        .unwrap();
    let resumed = first + 300_000;
    {
        let mut s = h.0.lock().unwrap();
        s.time = resumed;
        s.reset_at = resumed + WEEK;
    }
    Engine::load(&h)
        .await
        .unwrap()
        .action("key-a", "resume", false, resumed)
        .await
        .unwrap();
    for i in 2..=5 {
        let t = first + i * 300_000;
        {
            let mut s = h.0.lock().unwrap();
            s.time = t;
            s.reset_at = t + WEEK;
        }
        Engine::load(&h)
            .await
            .unwrap()
            .check("account-a", t)
            .await
            .unwrap();
    }
    let s = h.0.lock().unwrap();
    assert!(s.resets.is_empty());
    assert!(s.state.links["key-a"].pending.is_none());
}

#[tokio::test]
async fn post_reset_delivery_is_persisted_and_unknown_probe_does_not_repeat() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    setup(&h).await;
    {
        let mut e = Engine::load(&h).await.unwrap();
        e.state.probe_enabled = true;
        e.state.smtp.enabled = true;
        for link in e.state.links.values_mut() {
            link.recipient = "synthetic@example.com".into();
        }
        e.save().await.unwrap();
        e.deliver_one().await.unwrap();
    }
    assert!(h.0.lock().unwrap().probes.is_empty());
    next(&h);
    h.0.lock().unwrap().fail_probe = true;
    Engine::load(&h)
        .await
        .unwrap()
        .check("account-a", WEEK + 1000)
        .await
        .unwrap();
    for _ in 0..6 {
        Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
    }
    let s = h.0.lock().unwrap();
    assert_eq!(s.probes.len(), 2);
    assert_eq!(s.mails.len(), 2);
    assert_eq!(s.resets.len(), 2);
    assert_eq!(
        s.state.links["key-a"].deliveries[0].probe,
        SendStatus::Unknown
    );
    assert_eq!(
        s.state.links["key-a"].deliveries[0].mail,
        SendStatus::Succeeded
    );
}
#[tokio::test]
async fn paused_and_disabled_deliveries_wait_and_retry_requires_acknowledgement() {
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.probe_enabled = true;
    e.save().await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1000).await.unwrap();
    e.state.probe_enabled = false;
    e.save().await.unwrap();
    e.deliver_one().await.unwrap();
    assert!(h.0.lock().unwrap().probes.is_empty());
    e.state.probe_enabled = true;
    for l in e.state.links.values_mut() {
        l.paused = true;
    }
    e.save().await.unwrap();
    e.deliver_one().await.unwrap();
    assert!(h.0.lock().unwrap().probes.is_empty());
    assert!(e.retry_delivery("key-a", 1, true, false).await.is_err());
}
#[test]
fn legacy_settings_default_to_no_sending() {
    let state: State =
        serde_json::from_value(serde_json::json!({"early_auto":true,"links":{},"accounts":{}}))
            .unwrap();
    assert!(!state.probe_enabled && !state.smtp.enabled);
}

#[tokio::test]
async fn disabling_probe_does_not_block_independent_email() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.probe_enabled = true;
    e.state.smtp.enabled = true;
    e.state.links.get_mut("key-a").unwrap().recipient = "synthetic@example.com".into();
    e.save().await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1000).await.unwrap();
    e.state.probe_enabled = false;
    e.save().await.unwrap();
    e.deliver_one().await.unwrap();
    let s = h.0.lock().unwrap();
    assert!(s.probes.is_empty());
    assert_eq!(s.mails, ["synthetic@example.com"]);
    assert_eq!(
        s.state.links["key-a"].deliveries[0].probe,
        SendStatus::Skipped
    );
}

#[tokio::test]
async fn lost_delivery_completion_write_does_not_replay_model_call() {
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.probe_enabled = true;
    e.save().await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1000).await.unwrap();
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 2);
    }
    assert!(e.deliver_one().await.is_err());
    h.0.lock().unwrap().fail_save_at = None;
    for _ in 0..3 {
        Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
    }
    assert_eq!(h.0.lock().unwrap().probes, ["key-a", "key-b"]);
}

#[tokio::test]
async fn unknown_reset_never_creates_delivery() {
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.probe_enabled = true;
    e.save().await.unwrap();
    next(&h);
    h.0.lock().unwrap().lost_response = Some("key-b".into());
    e.check("account-a", WEEK + 1000).await.unwrap();
    assert!(e.state.links["key-b"].deliveries.is_empty());
    assert_eq!(e.state.links["key-a"].deliveries.len(), 1);
}

#[tokio::test]
async fn unknown_mail_is_not_repeated_and_explicit_retry_only_resends_selected_mail() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.smtp.enabled = true;
    e.state.links.get_mut("key-a").unwrap().recipient = "a@example.com".into();
    e.state.links.get_mut("key-b").unwrap().recipient = "b@example.com".into();
    e.save().await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1000).await.unwrap();
    h.0.lock().unwrap().fail_mail = true;
    for _ in 0..4 {
        Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
    }
    assert_eq!(h.0.lock().unwrap().mails.len(), 2);
    h.0.lock().unwrap().fail_mail = false;
    let mut e = Engine::load(&h).await.unwrap();
    assert!(e.retry_delivery("key-a", 1, false, false).await.is_err());
    e.retry_delivery("key-a", 1, false, true).await.unwrap();
    e.deliver_one().await.unwrap();
    let s = h.0.lock().unwrap();
    assert_eq!(s.mails, ["a@example.com", "b@example.com", "a@example.com"]);
    assert!(s.probes.is_empty());
    assert_eq!(s.resets.len(), 2);
    assert_eq!(
        s.state.links["key-a"].deliveries[0].mail,
        SendStatus::Succeeded
    );
    assert_eq!(
        s.state.links["key-b"].deliveries[0].mail,
        SendStatus::Unknown
    );
}

#[tokio::test]
async fn lost_mail_completion_write_does_not_replay_smtp_send() {
    let h = Fake::new();
    setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    e.state.smtp.enabled = true;
    e.state.links.get_mut("key-a").unwrap().recipient = "a@example.com".into();
    e.save().await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1000).await.unwrap();
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 2);
    }
    assert!(e.deliver_one().await.is_err());
    h.0.lock().unwrap().fail_save_at = None;
    for _ in 0..3 {
        Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
    }
    assert_eq!(h.0.lock().unwrap().mails, ["a@example.com"]);
}
