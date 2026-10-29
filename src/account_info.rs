//! 点数与上游重置卡的只读内存缓存
use crate::{
    host::{Host, fault},
    model::{AccountInfo, Credits, ResetCard, ResetCards, State},
};
use gateway_plugin_sdk::PluginFault;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

const INTERVAL: i64 = 600_000;
const VIEW_WINDOW: i64 = 1_800_000;

#[derive(Default)]
pub struct AccountInfoCache {
    inner: Mutex<BTreeMap<String, TrackedAccount>>,
}

#[derive(Default)]
struct TrackedAccount {
    generation: Arc<()>,
    info: Option<AccountInfo>,
    viewed_at: Option<i64>,
    credits_attempted_at: Option<i64>,
    cards_attempted_at: Option<i64>,
}

struct Query {
    account: String,
    generation: Arc<()>,
}

#[derive(Serialize)]
pub struct MemberAccount {
    credits: Option<Credits>,
    credits_at: Option<i64>,
    reset_cards: Option<ResetCards>,
    reset_cards_at: Option<i64>,
    reset_cards_failed_at: Option<i64>,
}

impl From<&AccountInfo> for MemberAccount {
    fn from(info: &AccountInfo) -> Self {
        Self {
            credits: info.credits.clone(),
            credits_at: info.credits_at,
            reset_cards: info.reset_cards.clone(),
            reset_cards_at: info.reset_cards_at,
            reset_cards_failed_at: info.reset_cards_failed_at,
        }
    }
}

fn due(last: Option<i64>, now: i64) -> bool {
    last.is_none_or(|last| now.saturating_sub(last) >= INTERVAL)
}

fn current_time(base: i64, started: Instant) -> i64 {
    base.saturating_add(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX))
}

impl AccountInfoCache {
    // 调用方须持有关联串行锁，避免旧状态快照把已删除账号重新加入。
    pub fn sync_accounts(&self, state: &State) {
        let accounts: BTreeSet<&str> = state
            .links
            .values()
            .map(|link| link.account_id.as_str())
            .collect();
        let mut tracked = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        tracked.retain(|account, _| accounts.contains(account.as_str()));
        for account in accounts {
            tracked.entry(account.to_owned()).or_default();
        }
    }

    pub fn mark_all_viewed(&self, now: i64) {
        for account in self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values_mut()
        {
            account.viewed_at = Some(account.viewed_at.map_or(now, |old| old.max(now)));
        }
    }

    pub fn view_member(&self, account: &str, now: i64) -> Option<MemberAccount> {
        let mut tracked = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let entry = tracked.get_mut(account)?;
        entry.viewed_at = Some(entry.viewed_at.map_or(now, |old| old.max(now)));
        entry.info.as_ref().map(MemberAccount::from)
    }

    pub fn snapshot(&self) -> BTreeMap<String, AccountInfo> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|(id, tracked)| {
                tracked.info.as_ref().map(|info| (id.clone(), info.clone()))
            })
            .collect()
    }

    fn reserve_cards(&self, now: i64) -> Option<Query> {
        let mut tracked = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let id = tracked
            .iter()
            .filter(|(_, entry)| {
                entry
                    .viewed_at
                    .is_some_and(|viewed| now.saturating_sub(viewed).max(0) <= VIEW_WINDOW)
                    && due(entry.cards_attempted_at, now)
            })
            .min_by_key(|(id, entry)| (entry.cards_attempted_at.unwrap_or(i64::MIN), *id))
            .map(|(id, _)| id.clone())?;
        let entry = tracked.get_mut(&id)?;
        entry.cards_attempted_at = Some(now);
        Some(Query {
            account: id,
            generation: entry.generation.clone(),
        })
    }

    fn reserve_credits(&self, now: i64) -> Option<Query> {
        let mut tracked = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let id = tracked
            .iter()
            .filter(|(_, entry)| due(entry.credits_attempted_at, now))
            .min_by_key(|(id, entry)| (entry.credits_attempted_at.unwrap_or(i64::MIN), *id))
            .map(|(id, _)| id.clone())?;
        let entry = tracked.get_mut(&id)?;
        entry.credits_attempted_at = Some(now);
        Some(Query {
            account: id,
            generation: entry.generation.clone(),
        })
    }

    fn update(&self, query: &Query, update: impl FnOnce(&mut AccountInfo)) {
        let mut tracked = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(entry) = tracked
            .get_mut(&query.account)
            .filter(|entry| Arc::ptr_eq(&entry.generation, &query.generation))
        else {
            return;
        };
        let info = entry.info.get_or_insert_with(|| AccountInfo {
            account_id: query.account.clone(),
            ..Default::default()
        });
        update(info);
    }

    pub async fn refresh<H: Host>(&self, host: &H, now: i64, deadline: Instant) {
        let started = Instant::now();
        if started >= deadline {
            return;
        }
        if let Some(query) = self.reserve_cards(now) {
            let limit = deadline.min(Instant::now() + Duration::from_secs(5));
            let result = tokio::time::timeout_at(limit, host.reset_cards(&query.account)).await;
            let at = current_time(now, started);
            self.update(&query, |info| match result {
                Ok(Ok(cards)) => {
                    info.reset_cards = Some(cards);
                    info.reset_cards_at = Some(at);
                    info.reset_cards_failed_at = None;
                }
                _ => info.reset_cards_failed_at = Some(at),
            });
        }
        while Instant::now() < deadline {
            let at = current_time(now, started);
            let Some(query) = self.reserve_credits(at) else {
                break;
            };
            let limit = deadline.min(Instant::now() + Duration::from_secs(1));
            if let Ok(Ok(credits)) =
                tokio::time::timeout_at(limit, host.quota_credits(&query.account)).await
            {
                let at = current_time(now, started);
                self.update(&query, |info| {
                    info.credits = credits;
                    info.credits_at = Some(at);
                });
            }
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    code: u32,
    data: ResetCardData,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCardData {
    available_count: u64,
    credits: Vec<ResetCardWire>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCardWire {
    status: Option<String>,
    title: Option<String>,
    expires_at: Option<String>,
    reset_type: Option<String>,
}

pub fn parse_reset_cards(body: &[u8]) -> Result<ResetCards, PluginFault> {
    let response: Envelope =
        serde_json::from_slice(body).map_err(|_| fault("重置卡响应格式错误"))?;
    if response.code != 200 {
        return Err(fault("重置卡查询失败"));
    }
    Ok(ResetCards {
        available_count: response.data.available_count,
        cards: response
            .data
            .credits
            .into_iter()
            .map(|card| ResetCard {
                title: card.title,
                status: card.status,
                reset_type: card.reset_type,
                expires_at: card
                    .expires_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|time| time.timestamp_millis()),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_plugin_sdk::call::{data::QuotaFacts, key_budgets::KeyBudget};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct FakeHost {
        calls: Mutex<Vec<(String, String)>>,
        fail_cards: AtomicBool,
        fail_credits: AtomicBool,
        missing_credits: AtomicBool,
        hang_cards: AtomicBool,
        block_cards: AtomicBool,
        card_started: Notify,
        card_release: Notify,
    }
    impl Host for FakeHost {
        async fn quota_credits(&self, account: &str) -> Result<Option<Credits>, PluginFault> {
            self.calls
                .lock()
                .unwrap()
                .push(("credits".into(), account.into()));
            if self.fail_credits.load(Ordering::SeqCst) {
                return Err(fault("synthetic error"));
            }
            if self.missing_credits.load(Ordering::SeqCst) {
                return Ok(None);
            }
            Ok(Some(Credits {
                has_credits: true,
                unlimited: false,
                balance: Some("1.230000".into()),
            }))
        }
        async fn reset_cards(&self, account: &str) -> Result<ResetCards, PluginFault> {
            self.calls
                .lock()
                .unwrap()
                .push(("cards".into(), account.into()));
            if self.block_cards.load(Ordering::SeqCst) {
                self.card_started.notify_one();
                self.card_release.notified().await;
            }
            if self.hang_cards.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.fail_cards.load(Ordering::SeqCst) {
                return Err(fault("synthetic error"));
            }
            Ok(ResetCards {
                available_count: 1,
                cards: vec![ResetCard {
                    title: Some("合成卡".into()),
                    status: Some("available".into()),
                    reset_type: Some("weekly".into()),
                    expires_at: Some(123),
                }],
            })
        }
        async fn load(&self) -> Result<(State, Option<u64>), PluginFault> {
            Err(fault("unused"))
        }
        async fn save(&self, _state: &State, _version: Option<u64>) -> Result<u64, PluginFault> {
            Err(fault("unused"))
        }
        async fn refresh(&self, _account: &str) -> Result<QuotaFacts, PluginFault> {
            Err(fault("unused"))
        }
        async fn budget(&self, _key: &str) -> Result<KeyBudget, PluginFault> {
            Err(fault("unused"))
        }
        async fn reset(&self, _key: &str) -> Result<(), PluginFault> {
            Err(fault("unused"))
        }
    }
    fn state(accounts: &[&str]) -> State {
        let mut state = State::default();
        for (index, account) in accounts.iter().enumerate() {
            let key = format!("synthetic-key-{index}");
            let link = serde_json::from_value(serde_json::json!({
                "key_id":key, "account_id":account,
                "baseline":{"observed":1000,"reset":604_800_000,"used":null},
                "paused":false,"pending":null,"events":[]
            }))
            .unwrap();
            state.links.insert(key, link);
        }
        state
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(1)
    }
    fn calls(host: &FakeHost, kind: &str) -> Vec<String> {
        host.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(call, _)| call == kind)
            .map(|(_, id)| id.clone())
            .collect()
    }

    #[test]
    fn reset_card_parser_discards_ids_and_interprets_rfc3339_offsets() {
        let cards = parse_reset_cards(br#"{"code":200,"message":"OK","data":{"availableCount":1,"credits":[{"id":"synthetic-secret-id","title":"synthetic card","status":"available","resetType":"weekly","expiresAt":"2026-10-29T12:34:56+08:00","expiresAtDisplay":"private display"}]}}"#).unwrap();
        assert_eq!(cards.available_count, 1);
        assert_eq!(cards.cards[0].expires_at, Some(1_793_248_496_000));
        let public = serde_json::to_string(&cards).unwrap();
        assert!(!public.contains("synthetic-secret-id"));
        assert!(!public.contains("private display"));
        assert!(!public.contains("expiresAtDisplay"));
    }

    #[test]
    fn reset_card_parser_handles_missing_optional_fields_and_bad_time() {
        let cards = parse_reset_cards(br#"{"code":200,"data":{"availableCount":0,"credits":[{}, {"expiresAt":"not-a-time"}]}}"#).unwrap();
        assert_eq!(
            cards.cards[0],
            ResetCard {
                title: None,
                status: None,
                reset_type: None,
                expires_at: None
            }
        );
        assert_eq!(cards.cards[1].expires_at, None);
        assert!(parse_reset_cards(br#"{"code":200,"data":{"credits":[]}}"#).is_err());
        assert!(parse_reset_cards(br#"{"code":200,"data":{"availableCount":0}}"#).is_err());
        assert!(
            parse_reset_cards(br#"{"code":403,"data":{"availableCount":0,"credits":[]}}"#).is_err()
        );
    }

    #[tokio::test]
    async fn cards_require_recent_view_and_ten_minute_throttle() {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&state(&["a", "b"]));
        cache.refresh(&host, 1_000, deadline()).await;
        assert!(calls(&host, "cards").is_empty());
        assert_eq!(calls(&host, "credits"), vec!["a", "b"]);
        cache.view_member("a", 2_000);
        cache.refresh(&host, 2_000, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a"]);
        let old = cache.snapshot()["a"].clone();
        cache.refresh(&host, 601_999, deadline()).await;
        assert_eq!(calls(&host, "cards").len(), 1);
        host.fail_cards.store(true, Ordering::SeqCst);
        cache.refresh(&host, 602_000, deadline()).await;
        let failed = &cache.snapshot()["a"];
        assert_eq!(failed.reset_cards, old.reset_cards);
        assert_eq!(failed.reset_cards_at, old.reset_cards_at);
        assert!(failed.reset_cards_failed_at.is_some());
        cache.refresh(&host, 602_001, deadline()).await;
        assert_eq!(calls(&host, "cards").len(), 2);
        host.fail_cards.store(false, Ordering::SeqCst);
        cache.refresh(&host, 1_202_000, deadline()).await;
        assert_eq!(calls(&host, "cards").len(), 3);
        assert_eq!(cache.snapshot()["a"].reset_cards_failed_at, None);
    }

    #[tokio::test]
    async fn card_queries_are_limited_to_one_account_per_maintenance() {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&state(&["b", "a", "c"]));
        cache.mark_all_viewed(1000);
        cache.refresh(&host, 1000, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a"]);
        cache.refresh(&host, 1001, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a", "b"]);
        cache.refresh(&host, 1002, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a", "b", "c"]);
        assert_eq!(calls(&host, "credits"), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn view_window_expires_after_thirty_minutes() {
        for (age, expected) in [(VIEW_WINDOW, 1), (VIEW_WINDOW + 1, 0)] {
            let host = FakeHost::default();
            let cache = AccountInfoCache::default();
            cache.sync_accounts(&state(&["a"]));
            cache.view_member("a", 0);
            cache.refresh(&host, age, deadline()).await;
            assert_eq!(calls(&host, "cards").len(), expected);
        }
    }

    #[tokio::test]
    async fn credits_failure_preserves_last_success_and_null_is_a_successful_read() {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&state(&["a"]));
        cache.refresh(&host, 1000, deadline()).await;
        let initial = cache.snapshot()["a"].clone();
        assert_eq!(
            initial.credits.as_ref().unwrap().balance.as_deref(),
            Some("1.230000")
        );
        host.fail_credits.store(true, Ordering::SeqCst);
        cache.refresh(&host, 601_000, deadline()).await;
        assert_eq!(cache.snapshot()["a"].credits, initial.credits);
        assert_eq!(cache.snapshot()["a"].credits_at, initial.credits_at);
        host.fail_credits.store(false, Ordering::SeqCst);
        host.missing_credits.store(true, Ordering::SeqCst);
        cache.refresh(&host, 1_201_000, deadline()).await;
        assert_eq!(cache.snapshot()["a"].credits, None);
        assert!(
            cache.snapshot()["a"]
                .credits_at
                .is_some_and(|at| at >= 1_201_000)
        );
    }

    #[tokio::test]
    async fn deleting_last_link_removes_data_and_view_history() {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        let mut linked = state(&["a", "a"]);
        cache.sync_accounts(&linked);
        cache.view_member("a", 1_000);
        cache.refresh(&host, 1_000, deadline()).await;
        linked.links.remove("synthetic-key-0");
        cache.sync_accounts(&linked);
        assert!(cache.snapshot().contains_key("a"));
        linked.links.clear();
        cache.sync_accounts(&linked);
        assert!(cache.snapshot().is_empty());
        cache.sync_accounts(&state(&["a"]));
        cache.refresh(&host, 601_001, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a"]);
    }

    #[tokio::test]
    async fn deleted_and_readded_account_rejects_an_old_inflight_result() {
        let host = Arc::new(FakeHost::default());
        host.block_cards.store(true, Ordering::SeqCst);
        let cache = Arc::new(AccountInfoCache::default());
        cache.sync_accounts(&state(&["a"]));
        cache.view_member("a", 1_000);
        let running = {
            let host = host.clone();
            let cache = cache.clone();
            tokio::spawn(async move {
                cache.refresh(&*host, 1_000, deadline()).await;
            })
        };
        tokio::time::timeout(Duration::from_secs(1), host.card_started.notified())
            .await
            .unwrap();
        cache.sync_accounts(&state(&[]));
        cache.sync_accounts(&state(&["a"]));
        host.card_release.notify_one();
        running.await.unwrap();
        assert!(
            cache
                .snapshot()
                .get("a")
                .is_none_or(|info| info.reset_cards.is_none())
        );
        cache.refresh(&*host, 601_001, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a"]);
    }

    #[tokio::test]
    async fn deadline_stops_hanging_query_and_preserves_failure_time() {
        let host = FakeHost::default();
        host.hang_cards.store(true, Ordering::SeqCst);
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&state(&["a"]));
        cache.view_member("a", 1000);
        let start = Instant::now();
        cache
            .refresh(&host, 1000, start + Duration::from_millis(20))
            .await;
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(cache.snapshot()["a"].reset_cards_failed_at.is_some());
        assert!(calls(&host, "credits").is_empty());
        cache.refresh(&host, 1_100, deadline()).await;
        assert_eq!(calls(&host, "cards"), vec!["a"]);
    }

    #[tokio::test]
    async fn member_projection_never_exposes_account_or_card_identifiers() {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&state(&["synthetic-account-id"]));
        assert!(cache.view_member("synthetic-account-id", 1000).is_none());
        cache.refresh(&host, 1000, deadline()).await;
        let member = serde_json::to_value(cache.view_member("synthetic-account-id", 1001)).unwrap();
        let serialized = member.to_string();
        assert!(!serialized.contains("account_id"));
        assert!(!serialized.contains("synthetic-account-id"));
        assert!(!serialized.contains("\"id\""));
        assert!(member["credits"].is_object());
        assert!(member["reset_cards"].is_object());
    }
}
