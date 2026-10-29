//! 日限卡的持久意图、预算检查与一次性调用
use crate::{
    engine::Engine,
    host::{Host, fault},
    model::{CardNotice, CardStatus, DailyCard, Link, SendStatus},
};
use gateway_plugin_sdk::{PluginFault, call::key_budgets::KeyBudget};
use std::{cmp::Ordering, time::Duration};

const MAX_LINK_NOTICES: usize = 8;
const MAX_TOTAL_NOTICES: usize = 64;

fn unfinished(status: SendStatus) -> bool {
    matches!(status, SendStatus::Queued | SendStatus::Unknown)
}

pub fn advance(link: &mut Link, now: i64) -> Result<(), PluginFault> {
    link.cycle_seq = link
        .cycle_seq
        .checked_add(1)
        .ok_or_else(|| fault("周期序号已达上限"))?;
    if link.card_enabled && !link.paused {
        issue(link, now);
    }
    Ok(())
}

fn issue(link: &mut Link, now: i64) {
    if link
        .card
        .as_ref()
        .is_none_or(|card| card.seq != link.cycle_seq)
    {
        link.card = Some(DailyCard {
            seq: link.cycle_seq,
            issued_at: now,
            status: CardStatus::Available,
            used_at: None,
            mail: None,
            recipient: None,
        });
    }
}

fn decimal_parts(value: &str) -> Option<(&str, &str)> {
    let value = value.strip_prefix('+').unwrap_or(value);
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    if (integer.is_empty() && fraction.is_empty())
        || !integer.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((
        integer.trim_start_matches('0'),
        fraction.trim_end_matches('0'),
    ))
}

fn decimal_cmp(a: &str, b: &str) -> Option<Ordering> {
    let (ai, af) = decimal_parts(a)?;
    let (bi, bf) = decimal_parts(b)?;
    let integer = ai.len().cmp(&bi.len()).then_with(|| ai.cmp(bi));
    if integer != Ordering::Equal {
        return Some(integer);
    }
    Some(
        af.bytes()
            .chain(std::iter::repeat(b'0'))
            .zip(bf.bytes().chain(std::iter::repeat(b'0')))
            .take(af.len().max(bf.len()))
            .map(|(x, y)| x.cmp(&y))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or(Ordering::Equal),
    )
}

pub fn block_reason(link: &Link, budget: &KeyBudget) -> Option<&'static str> {
    if link.paused {
        return Some("关联已暂停");
    }
    if !link.card_enabled {
        return Some("日限重置卡未启用");
    }
    let Some(card) = link.card.as_ref().filter(|card| card.seq == link.cycle_seq) else {
        return Some("当前周期没有可用的卡");
    };
    if card.status != CardStatus::Available {
        return Some("当前周期的卡已使用或结果未知");
    }
    let Some(daily_limit) = decimal_cmp(&budget.daily_limit_usd, "0") else {
        return Some("日限额数据无效");
    };
    let Some(daily_used) = decimal_cmp(&budget.daily_used_usd, "0") else {
        return Some("今日用量数据无效");
    };
    let Some(weekly_limit) = decimal_cmp(&budget.weekly_limit_usd, "0") else {
        return Some("周限额数据无效");
    };
    let Some(weekly_used) = decimal_cmp(&budget.weekly_used_usd, &budget.weekly_limit_usd) else {
        return Some("本周用量数据无效");
    };
    if daily_limit == Ordering::Equal {
        return Some("未设置日限额");
    }
    if daily_used == Ordering::Equal {
        return Some("今日用量为零");
    }
    if weekly_limit != Ordering::Equal && weekly_used != Ordering::Less {
        return Some("本周用量已达周限额");
    }
    None
}

#[derive(Debug, PartialEq)]
pub enum UseError {
    Blocked(&'static str),
    Unavailable,
}
#[derive(Debug, PartialEq)]
pub enum UseResult {
    Used,
    Unknown,
}

impl<H: Host> Engine<'_, H> {
    pub async fn recover_cards(&mut self, now: i64) -> Result<(), PluginFault> {
        let mut changed = false;
        for link in self.state.links.values_mut() {
            if let Some(card) = &mut link.card
                && card.status == CardStatus::Using
            {
                card.status = CardStatus::Unknown;
                link.event(now, "日限重置卡结果未知，已停止自动重试");
                changed = true;
            }
        }
        if changed {
            self.save().await?;
        }
        Ok(())
    }

    pub async fn card_toggle(
        &mut self,
        key: &str,
        enabled: bool,
        now: i64,
    ) -> Result<(), PluginFault> {
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or_else(|| fault("关联不存在"))?;
        let was_enabled = link.card_enabled;
        link.card_enabled = enabled;
        if enabled && !was_enabled && !link.paused {
            issue(link, now);
        }
        self.save().await
    }

    pub async fn card_refund(
        &mut self,
        key: &str,
        acknowledge: bool,
        now: i64,
    ) -> Result<(), PluginFault> {
        if !acknowledge {
            return Err(fault("请确认已核实未清零"));
        }
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or_else(|| fault("关联不存在"))?;
        let card = link
            .card
            .as_mut()
            .filter(|card| card.seq == link.cycle_seq && card.status == CardStatus::Unknown)
            .ok_or_else(|| fault("只有当前周期结果未知的卡可补回"))?;
        card.status = CardStatus::Available;
        card.used_at = None;
        card.mail = None;
        card.recipient = None;
        link.event(now, "管理员核实未清零，已补回日限重置卡");
        self.save().await
    }

    pub async fn card_notice_clear(
        &mut self,
        key: &str,
        expected_version: Option<u64>,
        acknowledge: bool,
        now: i64,
    ) -> Result<(), PluginFault> {
        if expected_version != self.version {
            return Err(fault("页面状态已变化，请刷新后重试"));
        }
        if !acknowledge {
            return Err(fault("请确认清理结果未知的卡通知记录"));
        }
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or_else(|| fault("关联不存在"))?;
        link.card_notices
            .retain(|notice| notice.status != SendStatus::Unknown);
        link.event(now, "管理员确认清理结果未知的卡通知记录");
        self.save().await
    }

    pub async fn use_card(&mut self, key: &str, now: i64) -> Result<UseResult, UseError> {
        let budget = self
            .host
            .budget(key)
            .await
            .map_err(|_| UseError::Unavailable)?;
        if budget.client_key_id != key {
            return Err(UseError::Unavailable);
        }
        let before_intent = self.state.clone();
        let before_version = self.version;
        let total_notices = self
            .state
            .links
            .values()
            .flat_map(|link| &link.card_notices)
            .filter(|notice| unfinished(notice.status))
            .take(MAX_TOTAL_NOTICES)
            .count();
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or(UseError::Blocked("关联不存在"))?;
        if let Some(reason) = block_reason(link, &budget) {
            return Err(UseError::Blocked(reason));
        }
        let wants_notice = self.state.smtp.enabled && !link.recipient.is_empty();
        let link_notices = link
            .card_notices
            .iter()
            .filter(|notice| unfinished(notice.status))
            .take(MAX_LINK_NOTICES)
            .count();
        let full = wants_notice
            && (link_notices >= MAX_LINK_NOTICES || total_notices >= MAX_TOTAL_NOTICES);
        let notice = wants_notice && !full;
        link.card_notices
            .retain(|n| n.status != SendStatus::Succeeded);
        let used_at = link
            .card_notices
            .iter()
            .map(|n| n.used_at)
            .max()
            .map_or(now, |last| now.max(last.saturating_add(1)));
        let id = if notice {
            Some(
                link.card_notices
                    .iter()
                    .map(|n| n.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(UseError::Unavailable)?,
            )
        } else {
            None
        };
        if full {
            link.event(now, "卡通知积压已满，本次未排队邮件");
        }
        let card = link.card.as_mut().ok_or(UseError::Unavailable)?;
        card.status = CardStatus::Using;
        card.used_at = Some(used_at);
        if let Some(id) = id {
            card.mail = Some(SendStatus::Queued);
            card.recipient = Some(link.recipient.clone());
            link.card_notices.push(CardNotice {
                id,
                seq: link.cycle_seq,
                used_at,
                result: CardStatus::Unknown,
                status: SendStatus::Queued,
                recipient: link.recipient.clone(),
            });
        } else {
            card.mail = full.then_some(SendStatus::Skipped);
            card.recipient = None;
        }
        // 意图与未知结果通知一起保存，进程退出也不会遗漏邮件或自动重试。
        if self.save().await.is_err() {
            // 写入回执丢失时，仅在确认仍是本次完整意图且版本未被其他操作改动后撤销。
            if let Ok((stored, version)) = self.host.load().await
                && matches!((before_version, version), (Some(before), Some(after)) if before.checked_add(1) == Some(after))
                && serde_json::to_value(&stored)
                    .ok()
                    .zip(serde_json::to_value(&self.state).ok())
                    .is_some_and(|(stored, intended)| stored == intended)
            {
                let _ = self.host.save(&before_intent, version).await;
            }
            return Err(UseError::Unavailable);
        }
        let result = tokio::time::timeout(Duration::from_secs(6), self.host.reset_daily(key)).await;
        let outcome = if matches!(result, Ok(Ok(()))) {
            CardStatus::Used
        } else {
            CardStatus::Unknown
        };
        let link = self.state.links.get_mut(key).ok_or(UseError::Unavailable)?;
        if let Some(card) = &mut link.card {
            card.status = outcome;
        }
        if let Some(id) = id
            && let Some(job) = link.card_notices.iter_mut().find(|job| job.id == id)
        {
            job.result = outcome;
        }
        link.event(
            now,
            if outcome == CardStatus::Used {
                "日限重置卡已使用"
            } else {
                "日限重置卡结果未知，已停止自动重试"
            },
        );
        if self.save().await.is_err() {
            return Ok(UseResult::Unknown);
        }
        Ok(if outcome == CardStatus::Used {
            UseResult::Used
        } else {
            UseResult::Unknown
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cycle::{Decision, Sample, WEEK},
        model::{Pending, Phase, State},
    };
    use gateway_plugin_sdk::call::data::{QuotaFacts, QuotaWindowFacts};
    use std::sync::Mutex;

    struct Fake(Mutex<FakeState>);
    struct FakeState {
        state: State,
        version: u64,
        resets: usize,
        all_resets: usize,
        lose_response: bool,
        hang: bool,
        fail_save_at: Option<u64>,
        lose_save_reply_once: bool,
        emails: Vec<(String, String)>,
    }
    fn budget(key: &str) -> KeyBudget {
        KeyBudget {
            client_key_id: key.into(),
            daily_limit_usd: "10".into(),
            daily_used_usd: "2".into(),
            weekly_limit_usd: "20".into(),
            weekly_used_usd: "4".into(),
            daily_resets_at_ms: None,
            weekly_resets_at_ms: None,
        }
    }
    fn sample() -> Sample {
        Sample {
            observed: 1_000,
            reset: WEEK,
            used: None,
        }
    }
    fn fake() -> Fake {
        let mut state = State::default();
        state.smtp.enabled = true;
        state.links.insert(
            "key".into(),
            Link {
                recipient: "member@example.test".into(),
                deliveries: vec![],
                last_manual_probe: None,
                last_manual_mail: None,
                key_id: "key".into(),
                account_id: "account".into(),
                baseline: sample(),
                paused: false,
                pending: None,
                events: Default::default(),
                retry_at: 0,
                phase: Phase::Tracking,
                card_enabled: true,
                cycle_seq: 0,
                card: Some(DailyCard {
                    seq: 0,
                    issued_at: 10,
                    status: CardStatus::Available,
                    used_at: None,
                    mail: None,
                    recipient: None,
                }),
                card_notices: vec![],
            },
        );
        Fake(Mutex::new(FakeState {
            state,
            version: 1,
            resets: 0,
            all_resets: 0,
            lose_response: false,
            hang: false,
            fail_save_at: None,
            lose_save_reply_once: false,
            emails: vec![],
        }))
    }
    impl Host for Fake {
        async fn load(&self) -> Result<(State, Option<u64>), PluginFault> {
            let state = self.0.lock().unwrap();
            Ok((state.state.clone(), Some(state.version)))
        }
        async fn save(&self, value: &State, version: Option<u64>) -> Result<u64, PluginFault> {
            let mut state = self.0.lock().unwrap();
            if version != Some(state.version) || state.fail_save_at == Some(state.version + 1) {
                return Err(fault("synthetic save failure"));
            }
            state.version += 1;
            state.state = value.clone();
            if state.lose_save_reply_once {
                state.lose_save_reply_once = false;
                return Err(fault("synthetic lost save response"));
            }
            Ok(state.version)
        }
        async fn refresh(&self, account: &str) -> Result<QuotaFacts, PluginFault> {
            Ok(QuotaFacts {
                schema_version: 1,
                account_id: account.into(),
                observed_at_ms: Some(2_000),
                windows: vec![QuotaWindowFacts {
                    key: "codex:604800s".into(),
                    window_seconds: Some(604800),
                    used_percent: None,
                    reset_at_ms: Some(WEEK + 1_000),
                }],
            })
        }
        async fn budget(&self, key: &str) -> Result<KeyBudget, PluginFault> {
            Ok(budget(key))
        }
        async fn reset(&self, _key: &str) -> Result<(), PluginFault> {
            self.0.lock().unwrap().all_resets += 1;
            Ok(())
        }
        async fn reset_daily(&self, key: &str) -> Result<(), PluginFault> {
            let (hang, lost) = {
                let mut state = self.0.lock().unwrap();
                let link = &state.state.links[key];
                let card = link.card.as_ref().unwrap();
                assert_eq!(card.status, CardStatus::Using);
                if card.mail == Some(SendStatus::Queued) {
                    assert_eq!(link.card_notices.last().unwrap().status, SendStatus::Queued);
                }
                state.resets += 1;
                (state.hang, state.lose_response)
            };
            if hang {
                std::future::pending::<()>().await;
            }
            if lost {
                Err(fault("synthetic lost response"))
            } else {
                Ok(())
            }
        }
        async fn email(
            &self,
            _settings: &crate::model::SmtpSettings,
            _recipient: &str,
            subject: &str,
            body: &str,
        ) -> Result<(), PluginFault> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(
                state.state.links["key"].card_notices.last().unwrap().status,
                SendStatus::Unknown
            );
            state.emails.push((subject.into(), body.into()));
            Ok(())
        }
    }

    #[test]
    fn decimal_comparison_does_not_round_budget_limits() {
        assert_eq!(
            decimal_cmp("1.00000000000000001", "1"),
            Some(Ordering::Greater)
        );
        assert_eq!(decimal_cmp("0.00", "000.0"), Some(Ordering::Equal));
        assert_eq!(decimal_cmp("-1", "0"), None);
    }
    #[test]
    fn conditions_reject_zero_usage_limits_and_mismatched_card() {
        let fake = fake();
        let state = fake.0.lock().unwrap();
        let link = &state.state.links["key"];
        assert_eq!(block_reason(link, &budget("key")), None);
        let mut b = budget("key");
        b.daily_limit_usd = "0".into();
        assert!(block_reason(link, &b).is_some());
        b = budget("key");
        b.daily_used_usd = "0.000".into();
        assert!(block_reason(link, &b).is_some());
        b = budget("key");
        b.weekly_used_usd = "20".into();
        assert!(block_reason(link, &b).is_some());
        let mut link = link.clone();
        link.cycle_seq = 1;
        assert!(block_reason(&link, &budget("key")).is_some());
    }
    #[tokio::test]
    async fn successful_use_persists_intent_and_sends_one_email() {
        let fake = fake();
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Used
        );
        engine.deliver_one().await.unwrap();
        engine.deliver_one().await.unwrap();
        let state = fake.0.lock().unwrap();
        assert_eq!(state.resets, 1);
        assert_eq!(state.emails.len(), 1);
        assert_eq!(state.emails[0].0, "CPR Key 日限重置卡已使用");
        assert!(state.emails[0].1.contains("UTC+8"));
        assert_eq!(
            state.state.links["key"].card.as_ref().unwrap().status,
            CardStatus::Used
        );
    }
    #[tokio::test]
    async fn unknown_result_never_retries_and_refund_requires_confirmation() {
        let fake = fake();
        fake.0.lock().unwrap().lose_response = true;
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Unknown
        );
        assert!(matches!(
            engine.use_card("key", 11_000).await,
            Err(UseError::Blocked(_))
        ));
        assert!(engine.card_refund("key", false, 12_000).await.is_err());
        engine.card_refund("key", true, 12_000).await.unwrap();
        assert_eq!(fake.0.lock().unwrap().resets, 1);
    }
    #[tokio::test]
    async fn lost_completion_write_recovers_unknown_with_queued_notice() {
        let fake = fake();
        fake.0.lock().unwrap().fail_save_at = Some(3);
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Unknown
        );
        drop(engine);
        fake.0.lock().unwrap().fail_save_at = None;
        let engine = Engine::load(&fake).await.unwrap();
        let link = &engine.state.links["key"];
        assert_eq!(link.card.as_ref().unwrap().status, CardStatus::Unknown);
        assert_eq!(link.card_notices[0].status, SendStatus::Queued);
        assert_eq!(fake.0.lock().unwrap().resets, 1);
    }
    #[tokio::test]
    async fn lost_intent_response_restores_available_without_calling_reset() {
        let fake = fake();
        fake.0.lock().unwrap().lose_save_reply_once = true;
        let mut engine = Engine::load(&fake).await.unwrap();
        assert!(matches!(
            engine.use_card("key", 10_000).await,
            Err(UseError::Unavailable)
        ));
        let state = fake.0.lock().unwrap();
        assert_eq!(state.resets, 0);
        assert_eq!(
            state.state.links["key"].card.as_ref().unwrap().status,
            CardStatus::Available
        );
        assert!(state.state.links["key"].card_notices.is_empty());
    }
    #[tokio::test]
    async fn interrupted_reset_recovers_unknown_without_repeating_call() {
        let fake = fake();
        fake.0.lock().unwrap().hang = true;
        let mut engine = Engine::load(&fake).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), engine.use_card("key", 10_000))
                .await
                .is_err()
        );
        drop(engine);
        let engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.state.links["key"].card.as_ref().unwrap().status,
            CardStatus::Unknown
        );
        assert_eq!(fake.0.lock().unwrap().resets, 1);
    }
    #[tokio::test]
    async fn only_confirmed_cycles_issue_cards_and_retry_does_not_advance() {
        let fake = fake();
        let mut engine = Engine::load(&fake).await.unwrap();
        engine.state.links.get_mut("key").unwrap().pending = Some(Pending::Ready {
            sample: sample(),
            reason: "正常换周".into(),
        });
        engine.execute("key", 2_000).await.unwrap();
        assert_eq!(engine.state.links["key"].cycle_seq, 1);
        assert_eq!(engine.state.links["key"].card.as_ref().unwrap().seq, 1);
        engine.state.links.get_mut("key").unwrap().pending = Some(Pending::Ready {
            sample: sample(),
            reason: "人工再次清零".into(),
        });
        engine.execute("key", 2_000).await.unwrap();
        engine.state.links.get_mut("key").unwrap().pending = Some(Pending::Ready {
            sample: sample(),
            reason: "手动确认窗口变化".into(),
        });
        engine.execute("key", 2_000).await.unwrap();
        assert_eq!(engine.state.links["key"].cycle_seq, 1);
        engine.state.links.get_mut("key").unwrap().pending = Some(Pending::Confirm {
            sample: sample(),
            reason: Decision::Early,
        });
        engine.action("key", "skip", false, 2_000).await.unwrap();
        assert_eq!(engine.state.links["key"].cycle_seq, 2);
    }
    #[tokio::test]
    async fn toggles_do_not_reissue_the_same_cycle_or_issue_while_paused() {
        let fake = fake();
        let mut engine = Engine::load(&fake).await.unwrap();
        engine.card_toggle("key", false, 100).await.unwrap();
        engine.card_toggle("key", true, 101).await.unwrap();
        assert_eq!(
            engine.state.links["key"].card.as_ref().unwrap().issued_at,
            10
        );
        let link = engine.state.links.get_mut("key").unwrap();
        link.paused = true;
        advance(link, 200).unwrap();
        assert_ne!(
            engine.state.links["key"].card.as_ref().unwrap().seq,
            engine.state.links["key"].cycle_seq
        );
    }

    fn notice(id: u64, status: SendStatus) -> CardNotice {
        CardNotice {
            id,
            seq: 0,
            used_at: id as i64,
            result: CardStatus::Unknown,
            status,
            recipient: "member@example.test".into(),
        }
    }

    #[tokio::test]
    async fn per_link_limit_keeps_unknown_and_allows_reset_when_full() {
        for existing in [7, 8, 9] {
            let fake = fake();
            {
                let mut state = fake.0.lock().unwrap();
                let link = state.state.links.get_mut("key").unwrap();
                link.card_notices = (1..=existing)
                    .map(|id| notice(id, SendStatus::Unknown))
                    .collect();
                link.card_notices.push(notice(100, SendStatus::Succeeded));
                link.card_notices.push(notice(101, SendStatus::Failed));
            }
            let mut engine = Engine::load(&fake).await.unwrap();
            assert_eq!(
                engine.use_card("key", 10_000).await.unwrap(),
                UseResult::Used
            );
            let link = &engine.state.links["key"];
            assert_eq!(
                link.card_notices
                    .iter()
                    .filter(|job| job.status == SendStatus::Unknown)
                    .count(),
                existing as usize
            );
            assert!(
                link.card_notices
                    .iter()
                    .any(|job| job.status == SendStatus::Failed)
            );
            assert_eq!(
                link.card_notices
                    .iter()
                    .filter(|job| job.status == SendStatus::Queued)
                    .count(),
                usize::from(existing == 7)
            );
            assert_eq!(
                link.card.as_ref().unwrap().mail,
                Some(if existing == 7 {
                    SendStatus::Queued
                } else {
                    SendStatus::Skipped
                })
            );
            assert_eq!(
                link.events
                    .iter()
                    .any(|event| event.reason == "卡通知积压已满，本次未排队邮件"),
                existing >= 8
            );
            assert_eq!(fake.0.lock().unwrap().resets, 1);
            if existing >= 8 {
                engine.deliver_one().await.unwrap();
                assert!(fake.0.lock().unwrap().emails.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn global_limit_counts_other_links_and_still_uses_card() {
        for existing in [63, 64] {
            let fake = fake();
            {
                let mut state = fake.0.lock().unwrap();
                let template = state.state.links["key"].clone();
                let mut left = existing;
                for index in 0..8 {
                    let mut link = template.clone();
                    let count = left.min(8);
                    let key = format!("other-{index}");
                    link.key_id = key.clone();
                    link.card_notices = (1..=count)
                        .map(|id| {
                            notice(
                                id,
                                if id % 2 == 0 {
                                    SendStatus::Queued
                                } else {
                                    SendStatus::Unknown
                                },
                            )
                        })
                        .collect();
                    state.state.links.insert(key, link);
                    left -= count;
                }
                assert_eq!(left, 0);
            }
            let mut engine = Engine::load(&fake).await.unwrap();
            assert_eq!(
                engine.use_card("key", 10_000).await.unwrap(),
                UseResult::Used
            );
            let target = &engine.state.links["key"];
            assert_eq!(target.card_notices.len(), usize::from(existing == 63));
            assert_eq!(
                target.card.as_ref().unwrap().mail,
                Some(if existing == 63 {
                    SendStatus::Queued
                } else {
                    SendStatus::Skipped
                })
            );
            assert_eq!(
                target
                    .events
                    .iter()
                    .any(|event| event.reason == "卡通知积压已满，本次未排队邮件"),
                existing == 64
            );
            assert_eq!(fake.0.lock().unwrap().resets, 1);
        }
    }

    #[tokio::test]
    async fn per_link_limit_counts_queued_and_unknown_together() {
        let fake = fake();
        {
            let mut state = fake.0.lock().unwrap();
            state.state.links.get_mut("key").unwrap().card_notices = (1..=8)
                .map(|id| {
                    notice(
                        id,
                        if id <= 4 {
                            SendStatus::Queued
                        } else {
                            SendStatus::Unknown
                        },
                    )
                })
                .collect();
        }
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Used
        );
        let link = &engine.state.links["key"];
        assert_eq!(
            link.card_notices
                .iter()
                .filter(|job| job.status == SendStatus::Queued)
                .count(),
            4
        );
        assert_eq!(
            link.card_notices
                .iter()
                .filter(|job| job.status == SendStatus::Unknown)
                .count(),
            4
        );
        assert_eq!(link.card.as_ref().unwrap().mail, Some(SendStatus::Skipped));
        assert_eq!(fake.0.lock().unwrap().resets, 1);
    }

    #[tokio::test]
    async fn clear_unknown_notices_requires_confirmation_and_exact_version() {
        let fake = fake();
        {
            let mut state = fake.0.lock().unwrap();
            let link = state.state.links.get_mut("key").unwrap();
            link.card_notices = vec![
                notice(1, SendStatus::Unknown),
                notice(2, SendStatus::Queued),
                notice(3, SendStatus::Unknown),
                notice(4, SendStatus::Succeeded),
            ];
            let card = link.card.as_mut().unwrap();
            card.status = CardStatus::Unknown;
            card.mail = Some(SendStatus::Unknown);
            let mut other = link.clone();
            other.key_id = "other".into();
            other.card_notices = vec![notice(1, SendStatus::Unknown)];
            state.state.links.insert("other".into(), other);
        }
        let mut engine = Engine::load(&fake).await.unwrap();
        let version = engine.version;
        assert!(
            engine
                .card_notice_clear("key", version, false, 20_000)
                .await
                .is_err()
        );
        assert!(
            engine
                .card_notice_clear("key", None, true, 20_000)
                .await
                .is_err()
        );
        assert_eq!(engine.state.links["key"].card_notices.len(), 4);
        engine
            .card_notice_clear("key", version, true, 20_000)
            .await
            .unwrap();
        let link = &engine.state.links["key"];
        assert_eq!(
            link.card_notices
                .iter()
                .map(|job| job.status)
                .collect::<Vec<_>>(),
            vec![SendStatus::Queued, SendStatus::Succeeded]
        );
        assert_eq!(link.card.as_ref().unwrap().status, CardStatus::Unknown);
        assert_eq!(link.card.as_ref().unwrap().mail, Some(SendStatus::Unknown));
        assert!(
            link.events
                .iter()
                .any(|event| event.reason == "管理员确认清理结果未知的卡通知记录")
        );
        assert_eq!(
            engine.state.links["other"].card_notices[0].status,
            SendStatus::Unknown
        );
        assert_eq!(
            fake.0.lock().unwrap().state.links["key"].card_notices.len(),
            2
        );
    }

    #[tokio::test]
    async fn stale_clear_cannot_discard_unknown_notices() {
        let fake = fake();
        fake.0
            .lock()
            .unwrap()
            .state
            .links
            .get_mut("key")
            .unwrap()
            .card_notices
            .push(notice(1, SendStatus::Unknown));
        let mut stale = Engine::load(&fake).await.unwrap();
        let mut current = Engine::load(&fake).await.unwrap();
        current.card_toggle("key", false, 20_000).await.unwrap();
        let version = stale.version;
        assert!(
            stale
                .card_notice_clear("key", version, true, 20_001)
                .await
                .is_err()
        );
        assert_eq!(
            fake.0.lock().unwrap().state.links["key"].card_notices[0].status,
            SendStatus::Unknown
        );
    }

    #[tokio::test]
    async fn cleared_capacity_can_queue_notice_in_a_new_cycle() {
        let fake = fake();
        fake.0
            .lock()
            .unwrap()
            .state
            .links
            .get_mut("key")
            .unwrap()
            .card_notices = (1..=8).map(|id| notice(id, SendStatus::Unknown)).collect();
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Used
        );
        advance(engine.state.links.get_mut("key").unwrap(), 11_000).unwrap();
        let version = engine.version;
        engine
            .card_notice_clear("key", version, true, 11_001)
            .await
            .unwrap();
        assert_eq!(
            engine.use_card("key", 12_000).await.unwrap(),
            UseResult::Used
        );
        assert_eq!(engine.state.links["key"].card_notices.len(), 1);
        assert_eq!(
            engine.state.links["key"].card_notices[0].status,
            SendStatus::Queued
        );
        assert_eq!(fake.0.lock().unwrap().resets, 2);
    }

    #[tokio::test]
    async fn full_backlog_and_lost_completion_remain_unknown_without_replay() {
        let fake = fake();
        {
            let mut state = fake.0.lock().unwrap();
            state.state.links.get_mut("key").unwrap().card_notices =
                (1..=8).map(|id| notice(id, SendStatus::Unknown)).collect();
            state.fail_save_at = Some(3);
        }
        let mut engine = Engine::load(&fake).await.unwrap();
        assert_eq!(
            engine.use_card("key", 10_000).await.unwrap(),
            UseResult::Unknown
        );
        drop(engine);
        fake.0.lock().unwrap().fail_save_at = None;
        let mut recovered = Engine::load(&fake).await.unwrap();
        assert_eq!(
            recovered.state.links["key"].card.as_ref().unwrap().status,
            CardStatus::Unknown
        );
        assert_eq!(recovered.state.links["key"].card_notices.len(), 8);
        assert!(recovered.use_card("key", 10_001).await.is_err());
        recovered.deliver_one().await.unwrap();
        let state = fake.0.lock().unwrap();
        assert_eq!(state.resets, 1);
        assert!(state.emails.is_empty());
    }

    #[tokio::test]
    async fn full_card_notice_backlog_does_not_block_cycle_reset() {
        let fake = fake();
        fake.0
            .lock()
            .unwrap()
            .state
            .links
            .get_mut("key")
            .unwrap()
            .card_notices = (1..=8).map(|id| notice(id, SendStatus::Unknown)).collect();
        let mut engine = Engine::load(&fake).await.unwrap();
        engine.state.links.get_mut("key").unwrap().pending = Some(Pending::Ready {
            sample: sample(),
            reason: "正常换周".into(),
        });
        engine.execute("key", 2_000).await.unwrap();
        engine.drain(2_000).await.unwrap();
        let state = fake.0.lock().unwrap();
        assert_eq!(state.all_resets, 1);
        assert_eq!(state.state.links["key"].card_notices.len(), 8);
        assert!(
            state.state.links["key"]
                .card_notices
                .iter()
                .all(|job| job.status == SendStatus::Unknown)
        );
    }
}
