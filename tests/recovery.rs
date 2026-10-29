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
    expect_manual: bool,
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
    daily_resets: usize,
    card_budget: Option<KeyBudget>,
    lose_save_reply: bool,
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
        let link = &s.state.links[key];
        if s.expect_manual {
            assert_eq!(
                link.last_manual_probe.as_ref().unwrap().status,
                key_reset_follow::model::SendStatus::Unknown
            );
        } else {
            assert_eq!(
                link.deliveries.last().unwrap().probe,
                key_reset_follow::model::SendStatus::Unknown
            );
        }
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
        subject: &str,
        body: &str,
    ) -> Result<(), PluginFault> {
        if subject == "CPR Key 日限重置卡已使用" {
            let s = self.0.lock().unwrap();
            assert!(
                s.state
                    .links
                    .values()
                    .any(|l| l.card_notices.iter().any(|n| {
                        n.status == key_reset_follow::model::SendStatus::Unknown
                            && n.recipient == recipient
                    }))
            );
        } else if subject == "CPR SMTP测试" {
            assert!(body.contains("SMTP测试") && !body.contains("已重置"));
        } else {
            assert_eq!(subject, "CPR Key 日、周额度已重置");
            assert!(body.contains("日额度和周额度已同步重置"));
        }
        let mut s = self.0.lock().unwrap();
        if s.expect_manual {
            assert_eq!(subject, "CPR SMTP测试");
            assert!(s.state.links.values().any(|l| {
                l.last_manual_mail.as_ref().is_some_and(|m| {
                    m.status == key_reset_follow::model::SendStatus::Unknown
                        && m.recipient.as_deref() == Some(recipient)
                })
            }));
        }
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
        if s.lose_save_reply {
            s.lose_save_reply = false;
            return Err(fault("synthetic lost save reply"));
        }
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
        if let Some(budget) = &self.0.lock().unwrap().card_budget {
            return Ok(budget.clone());
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
    async fn reset_daily(&self, key: &str) -> Result<(), PluginFault> {
        let (hang, lost) = {
            let mut s = self.0.lock().unwrap();
            assert_eq!(
                s.state.links[key].card.as_ref().unwrap().status,
                key_reset_follow::model::CardStatus::Using
            );
            s.daily_resets += 1;
            (s.hang_reset, s.lost_response.as_deref() == Some(key))
        };
        if hang {
            std::future::pending::<()>().await;
        }
        if lost {
            Err(fault("synthetic daily response lost"))
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

async fn setup_card(h: &Fake) {
    setup(h).await;
    let mut e = Engine::load(h).await.unwrap();
    e.state.smtp.enabled = true;
    e.state.links.get_mut("key-a").unwrap().recipient = "member@example.test".into();
    e.card_toggle("key-a", true, 1_000_000).await.unwrap();
    h.0.lock().unwrap().card_budget = Some(KeyBudget {
        client_key_id: "key-a".into(),
        daily_limit_usd: "10".into(),
        daily_used_usd: "2".into(),
        weekly_limit_usd: "60".into(),
        weekly_used_usd: "12".into(),
        daily_resets_at_ms: None,
        weekly_resets_at_ms: None,
    });
}

#[tokio::test]
async fn card_rejects_each_budget_and_link_guard_without_reset() {
    for case in 0..6 {
        let h = Fake::new();
        setup_card(&h).await;
        {
            let mut s = h.0.lock().unwrap();
            match case {
                0 => s.card_budget.as_mut().unwrap().daily_limit_usd = "0".into(),
                1 => s.card_budget.as_mut().unwrap().daily_used_usd = "0".into(),
                2 => s.card_budget.as_mut().unwrap().weekly_used_usd = "60".into(),
                3 => s.state.links.get_mut("key-a").unwrap().cycle_seq += 1,
                4 => s.state.links.get_mut("key-a").unwrap().paused = true,
                _ => s.state.links.get_mut("key-a").unwrap().card_enabled = false,
            }
        }
        let mut e = Engine::load(&h).await.unwrap();
        assert!(
            matches!(
                e.use_card("key-a", 1_000_001).await,
                Err(key_reset_follow::card::UseError::Blocked(_))
            ),
            "case {case}"
        );
        let s = h.0.lock().unwrap();
        assert_eq!(s.daily_resets, 0, "case {case}");
        assert!(s.resets.is_empty());
    }
}

#[tokio::test]
async fn card_success_calls_daily_once_and_mail_once_after_unknown_intent() {
    let h = Fake::new();
    setup_card(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    assert_eq!(
        e.use_card("key-a", 1_000_001).await.unwrap(),
        key_reset_follow::card::UseResult::Used
    );
    assert!(e.use_card("key-a", 1_000_002).await.is_err());
    e.deliver_one().await.unwrap();
    Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
    let s = h.0.lock().unwrap();
    assert_eq!(s.daily_resets, 1);
    assert!(s.resets.is_empty());
    assert_eq!(s.mails, ["member@example.test"]);
    assert_eq!(
        s.state.links["key-a"].card_notices[0].status,
        key_reset_follow::model::SendStatus::Succeeded
    );
}

#[tokio::test]
async fn card_error_and_real_timeout_remain_unknown_without_retry() {
    for hang in [false, true] {
        let h = Fake::new();
        setup_card(&h).await;
        {
            let mut s = h.0.lock().unwrap();
            s.hang_reset = hang;
            s.lost_response = Some("key-a".into());
            s.fail_mail = true;
        }
        let mut e = Engine::load(&h).await.unwrap();
        assert_eq!(
            e.use_card("key-a", 1_000_001).await.unwrap(),
            key_reset_follow::card::UseResult::Unknown
        );
        drop(e);
        let mut e = Engine::load(&h).await.unwrap();
        assert!(e.use_card("key-a", 1_000_002).await.is_err());
        e.deliver_one().await.unwrap();
        Engine::load(&h).await.unwrap().deliver_one().await.unwrap();
        let s = h.0.lock().unwrap();
        assert_eq!(s.daily_resets, 1);
        assert_eq!(s.mails.len(), 1);
        assert_eq!(
            s.state.links["key-a"].card_notices[0].status,
            key_reset_follow::model::SendStatus::Unknown
        );
    }
}

#[tokio::test]
async fn card_interrupted_call_and_failed_completion_recover_unknown() {
    for interrupted in [false, true] {
        let h = Fake::new();
        setup_card(&h).await;
        {
            let mut s = h.0.lock().unwrap();
            s.hang_reset = interrupted;
            if !interrupted {
                s.fail_save_at = Some(s.version + 2);
            }
        }
        let mut e = Engine::load(&h).await.unwrap();
        if interrupted {
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(10),
                    e.use_card("key-a", 1_000_001)
                )
                .await
                .is_err()
            );
        } else {
            assert_eq!(
                e.use_card("key-a", 1_000_001).await.unwrap(),
                key_reset_follow::card::UseResult::Unknown
            );
        }
        drop(e);
        h.0.lock().unwrap().fail_save_at = None;
        let mut e = Engine::load(&h).await.unwrap();
        assert_eq!(
            e.state.links["key-a"].card.as_ref().unwrap().status,
            key_reset_follow::model::CardStatus::Unknown
        );
        assert!(e.use_card("key-a", 1_000_002).await.is_err());
        assert_eq!(h.0.lock().unwrap().daily_resets, 1);
    }
}

#[tokio::test]
async fn card_pre_call_failures_preserve_available_and_never_reset() {
    for case in 0..3 {
        let h = Fake::new();
        setup_card(&h).await;
        {
            let mut s = h.0.lock().unwrap();
            match case {
                0 => s.fail_budget = Some("key-a".into()),
                1 => s.fail_save_at = Some(s.version + 1),
                _ => s.lose_save_reply = true,
            }
        }
        assert!(
            Engine::load(&h)
                .await
                .unwrap()
                .use_card("key-a", 1_000_001)
                .await
                .is_err()
        );
        let e = Engine::load(&h).await.unwrap();
        assert_eq!(
            e.state.links["key-a"].card.as_ref().unwrap().status,
            key_reset_follow::model::CardStatus::Available,
            "case {case}"
        );
        assert!(e.state.links["key-a"].card_notices.is_empty());
        assert_eq!(h.0.lock().unwrap().daily_resets, 0);
    }
}

#[tokio::test]
async fn card_refund_requires_current_unknown_and_explicit_confirmation() {
    use key_reset_follow::model::CardStatus;
    for status in [CardStatus::Available, CardStatus::Used, CardStatus::Unknown] {
        for current in [false, true] {
            for acknowledged in [false, true] {
                let h = Fake::new();
                setup_card(&h).await;
                let mut e = Engine::load(&h).await.unwrap();
                let link = e.state.links.get_mut("key-a").unwrap();
                link.card.as_mut().unwrap().status = status;
                if !current {
                    link.cycle_seq += 1;
                }
                let result = e.card_refund("key-a", acknowledged, 1_000_001).await;
                assert_eq!(
                    result.is_ok(),
                    status == CardStatus::Unknown && current && acknowledged
                );
                assert_eq!(h.0.lock().unwrap().daily_resets, 0);
            }
        }
    }
}

#[tokio::test]
async fn card_cas_conflict_cannot_claim_or_overwrite_used_card() {
    let h = Fake::new();
    setup_card(&h).await;
    let mut stale = Engine::load(&h).await.unwrap();
    Engine::load(&h)
        .await
        .unwrap()
        .use_card("key-a", 1_000_001)
        .await
        .unwrap();
    assert!(stale.use_card("key-a", 1_000_002).await.is_err());
    let s = h.0.lock().unwrap();
    assert_eq!(s.daily_resets, 1);
    assert_eq!(
        s.state.links["key-a"].card.as_ref().unwrap().status,
        key_reset_follow::model::CardStatus::Used
    );
}

#[tokio::test]
async fn card_lost_mail_completion_is_not_replayed_after_refund_or_new_cycle() {
    let h = Fake::new();
    setup_card(&h).await;
    h.0.lock().unwrap().lost_response = Some("key-a".into());
    let mut e = Engine::load(&h).await.unwrap();
    e.use_card("key-a", 1_000_001).await.unwrap();
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 2);
    }
    assert!(e.deliver_one().await.is_err());
    h.0.lock().unwrap().fail_save_at = None;
    let mut e = Engine::load(&h).await.unwrap();
    e.card_refund("key-a", true, 1_000_002).await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1_000).await.unwrap();
    e.deliver_one().await.unwrap();
    let s = h.0.lock().unwrap();
    assert_eq!(s.mails.len(), 1);
    assert_eq!(
        s.state.links["key-a"].card_notices[0].status,
        key_reset_follow::model::SendStatus::Unknown
    );
    assert_eq!(s.state.links["key-a"].cycle_seq, 1);
}

#[tokio::test]
async fn card_lost_intent_reply_with_failed_rollback_stays_unknown() {
    let h = Fake::new();
    setup_card(&h).await;
    {
        let mut s = h.0.lock().unwrap();
        s.lose_save_reply = true;
        s.fail_save_at = Some(s.version + 2);
    }
    assert!(
        Engine::load(&h)
            .await
            .unwrap()
            .use_card("key-a", 1_000_001)
            .await
            .is_err()
    );
    h.0.lock().unwrap().fail_save_at = None;
    let mut e = Engine::load(&h).await.unwrap();
    assert_eq!(
        e.state.links["key-a"].card.as_ref().unwrap().status,
        key_reset_follow::model::CardStatus::Unknown
    );
    assert!(e.use_card("key-a", 1_000_002).await.is_err());
    assert_eq!(h.0.lock().unwrap().daily_resets, 0);
}

#[tokio::test]
async fn card_cycle_events_advance_once_and_invalidate_old_card() {
    for early in [false, true] {
        for unknown in [false, true] {
            let h = Fake::new();
            setup_card(&h).await;
            {
                let mut s = h.0.lock().unwrap();
                s.time = if early { WEEK / 2 } else { WEEK + 1_000 };
                s.reset_at = if early { WEEK + WEEK / 2 } else { WEEK * 2 };
                if unknown {
                    s.lost_response = Some("key-a".into());
                }
            }
            let now = h.0.lock().unwrap().time;
            let mut e = Engine::load(&h).await.unwrap();
            e.check("account-a", now).await.unwrap();
            assert_eq!(e.state.links["key-a"].cycle_seq, 1);
            assert_eq!(e.state.links["key-a"].card.as_ref().unwrap().seq, 1);
            assert!(e.state.links["key-b"].card.is_none());
            assert_eq!(e.state.links["key-b"].cycle_seq, 1);
            if unknown {
                e.action("key-a", "retry", true, now).await.unwrap();
                assert_eq!(e.state.links["key-a"].cycle_seq, 1);
                e.action("key-a", "skip", false, now).await.unwrap();
                assert_eq!(e.state.links["key-a"].cycle_seq, 1);
            }
        }
    }
}

#[tokio::test]
async fn card_confirm_skip_advances_but_changed_skip_does_not() {
    let h = Fake::new();
    setup_card(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    for reason in [
        key_reset_follow::cycle::Decision::Early,
        key_reset_follow::cycle::Decision::Unexplained,
    ] {
        let sample = e.state.links["key-a"].baseline.clone();
        let before = e.state.links["key-a"].cycle_seq;
        e.state.links.get_mut("key-a").unwrap().pending = Some(Pending::Confirm { sample, reason });
        e.action("key-a", "skip", false, 1_000_001).await.unwrap();
        assert_eq!(e.state.links["key-a"].cycle_seq, before + 1);
    }
    h.0.lock().unwrap().changed_window = true;
    h.0.lock().unwrap().time = 1_000_002;
    e.check("account-a", 1_000_002).await.unwrap();
    assert!(matches!(
        e.state.links["key-a"].pending,
        Some(Pending::Changed { .. })
    ));
    h.0.lock().unwrap().changed_window = false;
    e.action("key-a", "skip", false, 1_000_003).await.unwrap();
    assert_eq!(e.state.links["key-a"].cycle_seq, 2);
    assert!(h.0.lock().unwrap().resets.is_empty());
}

#[tokio::test]
async fn card_toggle_pause_resume_and_settling_never_duplicate_issue() {
    let h = Fake::new();
    setup_card(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    let issued = e.state.links["key-a"].card.as_ref().unwrap().issued_at;
    e.card_toggle("key-a", false, 1_000_001).await.unwrap();
    e.card_toggle("key-a", true, 1_000_002).await.unwrap();
    assert_eq!(
        e.state.links["key-a"].card.as_ref().unwrap().issued_at,
        issued
    );
    e.action("key-b", "pause", false, 1_000_003).await.unwrap();
    e.card_toggle("key-b", true, 1_000_004).await.unwrap();
    next(&h);
    e.check("account-a", WEEK + 1_000).await.unwrap();
    e.action("key-b", "resume", false, WEEK + 1_001)
        .await
        .unwrap();
    assert_eq!(e.state.links["key-b"].cycle_seq, 0);
    assert!(e.state.links["key-b"].card.is_none());
    for now in [WEEK + 400_000, WEEK + 800_000] {
        {
            let mut s = h.0.lock().unwrap();
            s.time = now;
            s.reset_at = now + WEEK;
        }
        e.check("account-a", now).await.unwrap();
    }
    assert_eq!(e.state.links["key-a"].cycle_seq, 1);
    assert_eq!(h.0.lock().unwrap().resets.len(), 1);
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

async fn manual_setup(h: &Fake) {
    setup(h).await;
    let mut e = Engine::load(h).await.unwrap();
    e.state.smtp.server = "smtp.example.com".into();
    e.state.smtp.port = 587;
    e.state.smtp.from = "sender@example.com".into();
    e.state.links.get_mut("key-a").unwrap().recipient = "saved@example.com".into();
    e.state.links.get_mut("key-a").unwrap().paused = true;
    e.state
        .links
        .get_mut("key-a")
        .unwrap()
        .deliveries
        .push(key_reset_follow::model::Delivery {
            id: 99,
            reset_at: 123,
            recipient: "original@example.com".into(),
            probe: key_reset_follow::model::SendStatus::Queued,
            mail: key_reset_follow::model::SendStatus::Queued,
        });
    e.save().await.unwrap();
    h.0.lock().unwrap().expect_manual = true;
}
#[tokio::test]
async fn manual_tests_are_immediate_independent_and_do_not_touch_reset_state() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    manual_setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    let baseline = serde_json::to_value(&e.state.links["key-a"]).unwrap();
    let refreshes = h.0.lock().unwrap().refreshes;
    let version = e.version;
    e.manual_test("key-a", true, true, version, 2000)
        .await
        .unwrap();
    let version = e.version;
    e.manual_test("key-a", false, true, version, 3000)
        .await
        .unwrap();
    let link = &e.state.links["key-a"];
    assert_eq!(
        link.last_manual_probe.as_ref().unwrap().status,
        SendStatus::Succeeded
    );
    assert_eq!(link.last_manual_probe.as_ref().unwrap().at, 2000);
    assert_eq!(
        link.last_manual_mail.as_ref().unwrap().status,
        SendStatus::Succeeded
    );
    assert_eq!(
        link.last_manual_mail.as_ref().unwrap().recipient.as_deref(),
        Some("saved@example.com")
    );
    let mut after = serde_json::to_value(link).unwrap();
    after["last_manual_probe"] = serde_json::Value::Null;
    after["last_manual_mail"] = serde_json::Value::Null;
    assert_eq!(after, baseline);
    let s = h.0.lock().unwrap();
    assert_eq!(s.probes, ["key-a"]);
    assert_eq!(s.mails, ["saved@example.com"]);
    assert_eq!(s.refreshes, refreshes);
    assert!(s.resets.is_empty());
    assert!(!s.state.probe_enabled && !s.state.smtp.enabled);
}
#[tokio::test]
async fn manual_tests_reject_missing_confirmation_invalid_config_and_stale_version() {
    let h = Fake::new();
    manual_setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    let version = e.version;
    assert!(
        e.manual_test("key-a", true, false, version, 2000)
            .await
            .is_err()
    );
    assert!(
        e.manual_test("key-a", true, true, None, 2000)
            .await
            .is_err()
    );
    e.state.smtp.port = 0;
    assert!(
        e.manual_test("key-a", false, true, version, 2000)
            .await
            .is_err()
    );
    e.state.smtp.port = 587;
    e.state.links.get_mut("key-a").unwrap().recipient.clear();
    assert!(
        e.manual_test("key-a", false, true, version, 2000)
            .await
            .is_err()
    );
    let s = h.0.lock().unwrap();
    assert!(s.probes.is_empty() && s.mails.is_empty());
}
#[tokio::test]
async fn manual_errors_and_lost_completion_never_replay_on_maintenance() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    manual_setup(&h).await;
    h.0.lock().unwrap().fail_probe = true;
    let mut e = Engine::load(&h).await.unwrap();
    let version = e.version;
    e.manual_test("key-a", true, true, version, 2000)
        .await
        .unwrap();
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 2);
    }
    let version = e.version;
    assert!(
        e.manual_test("key-a", false, true, version, 3000)
            .await
            .is_err()
    );
    h.0.lock().unwrap().fail_save_at = None;
    for _ in 0..3 {
        let mut e = Engine::load(&h).await.unwrap();
        e.drain(4000).await.unwrap();
        e.deliver_one().await.unwrap();
    }
    let mut e = Engine::load(&h).await.unwrap();
    assert_eq!(
        e.state.links["key-a"]
            .last_manual_probe
            .as_ref()
            .unwrap()
            .status,
        SendStatus::Unknown
    );
    assert_eq!(
        e.state.links["key-a"]
            .last_manual_mail
            .as_ref()
            .unwrap()
            .status,
        SendStatus::Unknown
    );
    h.0.lock().unwrap().fail_probe = false;
    let version = e.version;
    e.manual_test("key-a", true, true, version, 5000)
        .await
        .unwrap();
    let s = h.0.lock().unwrap();
    assert_eq!(s.probes.len(), 2);
    assert_eq!(s.mails.len(), 1);
    assert!(s.resets.is_empty());
    assert_eq!(
        s.state.links["key-a"].last_manual_mail.as_ref().unwrap().at,
        3000
    );
}
#[tokio::test]
async fn manual_intent_save_failure_does_not_send_and_legacy_link_is_readable() {
    let h = Fake::new();
    manual_setup(&h).await;
    let mut e = Engine::load(&h).await.unwrap();
    let mut old = serde_json::to_value(&e.state).unwrap();
    for link in old["links"].as_object_mut().unwrap().values_mut() {
        link.as_object_mut().unwrap().remove("last_manual_probe");
        link.as_object_mut().unwrap().remove("last_manual_mail");
    }
    let old: State = serde_json::from_value(old).unwrap();
    assert!(old.links["key-a"].last_manual_probe.is_none());
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 1);
    }
    let version = e.version;
    assert!(
        e.manual_test("key-a", true, true, version, 2000)
            .await
            .is_err()
    );
    assert!(h.0.lock().unwrap().probes.is_empty());
}

#[tokio::test]
async fn manual_mail_error_and_probe_completion_loss_remain_independent_unknown_results() {
    use key_reset_follow::model::SendStatus;
    let h = Fake::new();
    manual_setup(&h).await;
    h.0.lock().unwrap().fail_mail = true;
    let mut e = Engine::load(&h).await.unwrap();
    let version = e.version;
    e.manual_test("key-a", false, true, version, 2000)
        .await
        .unwrap();
    {
        let mut s = h.0.lock().unwrap();
        s.fail_save_at = Some(s.version + 2);
    }
    let version = e.version;
    assert!(
        e.manual_test("key-a", true, true, version, 3000)
            .await
            .is_err()
    );
    h.0.lock().unwrap().fail_save_at = None;
    let mut e = Engine::load(&h).await.unwrap();
    e.deliver_one().await.unwrap();
    assert_eq!(
        e.state.links["key-a"]
            .last_manual_probe
            .as_ref()
            .unwrap()
            .status,
        SendStatus::Unknown
    );
    assert_eq!(
        e.state.links["key-a"]
            .last_manual_mail
            .as_ref()
            .unwrap()
            .status,
        SendStatus::Unknown
    );
    let s = h.0.lock().unwrap();
    assert_eq!(s.mails.len(), 1);
    assert_eq!(s.probes.len(), 1);
}
