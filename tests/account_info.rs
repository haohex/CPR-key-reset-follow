//! 通过公开缓存接口验证按需读取、故障保留和删除后的隔离。
use gateway_plugin_sdk::{
    PluginFault,
    call::{data::QuotaFacts, key_budgets::KeyBudget},
};
use key_reset_follow::{
    account_info::AccountInfoCache,
    host::{Host, fault},
    model::{Credits, ResetCards, State},
};
use std::sync::{
    Mutex,
    atomic::{AtomicU8, Ordering},
};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

#[derive(Default)]
struct FakeHost {
    calls: Mutex<Vec<(&'static str, String)>>,
    cards_mode: AtomicU8,
    credits_mode: AtomicU8,
    started: Notify,
    release: Notify,
}
impl FakeHost {
    fn count(&self, kind: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == kind)
            .count()
    }
    async fn query(&self, kind: &'static str, account: &str, mode: u8) -> Result<(), PluginFault> {
        self.calls.lock().unwrap().push((kind, account.into()));
        match mode {
            1 => return Err(fault("synthetic query failure")),
            2 => std::future::pending::<()>().await,
            3 => {
                self.started.notify_one();
                self.release.notified().await;
            }
            _ => {}
        }
        Ok(())
    }
}
impl Host for FakeHost {
    async fn reset_cards(&self, account: &str) -> Result<ResetCards, PluginFault> {
        self.query("cards", account, self.cards_mode.load(Ordering::SeqCst))
            .await?;
        Ok(ResetCards {
            available_count: 2,
            cards: vec![],
        })
    }
    async fn quota_credits(&self, account: &str) -> Result<Option<Credits>, PluginFault> {
        self.query("credits", account, self.credits_mode.load(Ordering::SeqCst))
            .await?;
        Ok(Some(Credits {
            has_credits: true,
            unlimited: false,
            balance: Some("12.340000".into()),
        }))
    }
    async fn load(&self) -> Result<(State, Option<u64>), PluginFault> {
        panic!("cache must not load persistent state")
    }
    async fn save(&self, _: &State, _: Option<u64>) -> Result<u64, PluginFault> {
        panic!("cache must not persist")
    }
    async fn refresh(&self, _: &str) -> Result<QuotaFacts, PluginFault> {
        panic!("cache must not refresh quota")
    }
    async fn budget(&self, _: &str) -> Result<KeyBudget, PluginFault> {
        panic!("cache must not read key budget")
    }
    async fn reset(&self, _: &str) -> Result<(), PluginFault> {
        panic!("cache must not reset budget")
    }
}
fn linked(accounts: &[&str]) -> State {
    let mut state = State::default();
    for (i, account) in accounts.iter().enumerate() {
        let key = format!("synthetic-key-{i}");
        state.links.insert(
            key.clone(),
            serde_json::from_value(serde_json::json!({
                "key_id": key, "account_id": account,
                "baseline": {"observed": 1000, "reset": 604800000, "used": null},
                "paused": false, "pending": null, "events": []
            }))
            .unwrap(),
        );
    }
    state
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_millis(100)
}

#[tokio::test]
async fn viewing_only_marks_demand_and_maintenance_obeys_ten_and_thirty_minutes() {
    for management in [false, true] {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&linked(&["account-a"]));
        cache.refresh(&host, 1000, deadline()).await;
        assert_eq!(host.count("cards"), 0);
        if management {
            cache.mark_all_viewed(2000);
        } else {
            cache.view_member("account-a", 2000);
        }
        assert_eq!(host.count("cards"), 0);
        cache.refresh(&host, 2000, deadline()).await;
        cache.refresh(&host, 601999, deadline()).await;
        assert_eq!(host.count("cards"), 1);
        cache.refresh(&host, 602000, deadline()).await;
        assert_eq!(host.count("cards"), 2);
        cache.refresh(&host, 1802001, deadline()).await;
        assert_eq!(host.count("cards"), 2);
    }
}

#[tokio::test]
async fn each_maintenance_queries_one_card_account_and_throttles_each_credit_account() {
    let host = FakeHost::default();
    let cache = AccountInfoCache::default();
    cache.sync_accounts(&linked(&["a", "a", "b", "c"]));
    cache.mark_all_viewed(1000);
    for round in 0..3 {
        cache.refresh(&host, 1000 + round, deadline()).await;
        assert_eq!(host.count("cards"), (round + 1) as usize);
        assert_eq!(host.count("credits"), 3);
    }
    cache.refresh(&host, 600999, deadline()).await;
    assert_eq!(host.count("credits"), 3);
    cache.refresh(&host, 602000, deadline()).await;
    assert_eq!(host.count("credits"), 6);
    for id in ["a", "b", "c"] {
        assert_eq!(
            host.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(kind, account)| *kind == "credits" && account == id)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn errors_and_timeouts_preserve_cards_and_success_clears_failure_time() {
    for failure in [1, 2] {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&linked(&["a"]));
        cache.mark_all_viewed(1000);
        cache.refresh(&host, 1000, deadline()).await;
        let old = cache.snapshot()["a"].clone();
        host.cards_mode.store(failure, Ordering::SeqCst);
        cache.refresh(&host, 602000, deadline()).await;
        let failed = cache.snapshot()["a"].clone();
        assert_eq!(failed.reset_cards, old.reset_cards);
        assert_eq!(failed.reset_cards_at, old.reset_cards_at);
        assert!(failed.reset_cards_failed_at.is_some_and(|at| at >= 602000));
        cache.refresh(&host, 602001, deadline()).await;
        assert_eq!(host.count("cards"), 2);
        host.cards_mode.store(0, Ordering::SeqCst);
        cache.refresh(&host, 1204000, deadline()).await;
        assert_eq!(cache.snapshot()["a"].reset_cards_failed_at, None);
        assert!(cache.snapshot()["a"].reset_cards_at.unwrap() > old.reset_cards_at.unwrap());
    }
}

#[tokio::test]
async fn last_link_deletion_clears_cache_and_view_history_but_shared_link_preserves_it() {
    let host = FakeHost::default();
    let cache = AccountInfoCache::default();
    let mut state = linked(&["a", "a"]);
    cache.sync_accounts(&state);
    cache.mark_all_viewed(1000);
    cache.refresh(&host, 1000, deadline()).await;
    state.links.remove("synthetic-key-0");
    cache.sync_accounts(&state);
    assert!(cache.snapshot().contains_key("a"));
    state.links.clear();
    cache.sync_accounts(&state);
    assert!(cache.snapshot().is_empty());
    assert!(cache.view_member("a", 2000).is_none());
    cache.sync_accounts(&linked(&["a"]));
    cache.refresh(&host, 602000, deadline()).await;
    assert_eq!(host.count("cards"), 1);
    assert!(cache.snapshot()["a"].reset_cards.is_none());
}

#[tokio::test]
async fn deleted_generation_rejects_inflight_cards_and_credits_even_after_readdition() {
    for cards in [false, true] {
        let host = FakeHost::default();
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&linked(&["a"]));
        if cards {
            cache.mark_all_viewed(1000);
            host.cards_mode.store(3, Ordering::SeqCst);
        } else {
            host.credits_mode.store(3, Ordering::SeqCst);
        }
        let refresh = cache.refresh(&host, 1000, Instant::now() + Duration::from_secs(1));
        let deletion = async {
            host.started.notified().await;
            cache.sync_accounts(&State::default());
            cache.sync_accounts(&linked(&["a"]));
            // 新代次的点数查询也必须失败，才能观察旧结果是否错误回填。
            host.credits_mode.store(1, Ordering::SeqCst);
            host.release.notify_one();
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(refresh, deletion);
        })
        .await
        .unwrap();
        assert!(
            cache
                .snapshot()
                .get("a")
                .is_none_or(|info| info.credits.is_none() && info.reset_cards.is_none())
        );
        host.cards_mode.store(0, Ordering::SeqCst);
        cache.refresh(&host, 602000, deadline()).await;
        assert_eq!(host.count("cards"), usize::from(cards));
    }
}

#[tokio::test]
async fn deadline_bounds_never_returning_cards_and_credits_and_expired_deadline_does_nothing() {
    for cards in [false, true] {
        let host = FakeHost::default();
        host.cards_mode.store(2, Ordering::SeqCst);
        host.credits_mode.store(2, Ordering::SeqCst);
        let cache = AccountInfoCache::default();
        cache.sync_accounts(&linked(&["a", "b"]));
        if cards {
            cache.mark_all_viewed(1000);
        }
        cache.refresh(&host, 1000, Instant::now()).await;
        assert!(host.calls.lock().unwrap().is_empty());
        // 留出调度误差；外层 watchdog 防止 deadline 回归让整个测试挂死。
        let start = Instant::now();
        tokio::time::timeout(
            Duration::from_millis(250),
            cache.refresh(&host, 1000, start + Duration::from_millis(30)),
        )
        .await
        .unwrap();
        assert!(start.elapsed() < Duration::from_millis(200));
        assert_eq!(host.count("cards"), usize::from(cards));
        assert_eq!(host.count("credits"), usize::from(!cards));
        if cards {
            assert!(cache.snapshot()["a"].reset_cards_failed_at.is_some());
        }
    }
}
