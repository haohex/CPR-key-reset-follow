//! 统计结果只驻留内存，成员视图不泄露其他成员金额或标识
use crate::{
    cycle::WEEK,
    host::{Host, fault},
    model::{ShareStats, State},
};
use gateway_plugin_sdk::PluginFault;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};

const INTERVAL: i64 = 600_000;

#[derive(Default)]
pub struct ShareCache {
    inner: Mutex<CacheInner>,
}
#[derive(Default)]
struct CacheInner {
    shares: BTreeMap<String, ShareStats>,
    attempted: BTreeMap<String, i64>,
}
impl ShareCache {
    pub fn snapshot(&self) -> BTreeMap<String, ShareStats> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shares
            .clone()
    }

    pub async fn refresh<H: Host>(
        &self,
        host: &H,
        state: &State,
        now: i64,
        deadline: tokio::time::Instant,
    ) {
        let mut accounts = BTreeMap::new();
        for link in state.links.values() {
            let sample = state
                .accounts
                .get(&link.account_id)
                .and_then(|a| a.sample.as_ref())
                .unwrap_or(&link.baseline);
            accounts
                .entry(link.account_id.clone())
                .or_insert(sample.reset);
        }
        {
            let mut cache = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            cache.shares.retain(|id, _| accounts.contains_key(id));
            cache.attempted.retain(|id, _| accounts.contains_key(id));
        }
        let mut due: Vec<_> = accounts
            .into_iter()
            .filter_map(|(id, end)| {
                let cache = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let last = cache.attempted.get(&id).copied().unwrap_or(i64::MIN);
                (now.saturating_sub(last) >= INTERVAL).then_some((last, id, end))
            })
            .collect();
        due.sort();
        for (_, account, window_end) in due {
            if deadline <= tokio::time::Instant::now() {
                break;
            }
            let Some(start) = window_end.checked_sub(WEEK) else {
                continue;
            };
            if start >= now {
                continue;
            }
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .attempted
                .insert(account.clone(), now);
            let query = async {
                let accounts = tokio::time::timeout(
                    Duration::from_secs(1),
                    host.statistics("account", start, now),
                )
                .await
                .map_err(|_| fault("统计超时"))??;
                let keys = tokio::time::timeout(
                    Duration::from_secs(1),
                    host.statistics("apiKey", start, now),
                )
                .await
                .map_err(|_| fault("统计超时"))??;
                Ok::<_, PluginFault>((accounts, keys))
            };
            if let Ok(Ok((accounts, keys))) = tokio::time::timeout_at(deadline, query).await {
                let Ok(account_cost) = accounts.cost(&account) else {
                    continue;
                };
                let key_costs: Result<BTreeMap<_, _>, PluginFault> = state
                    .links
                    .values()
                    .filter(|l| l.account_id == account)
                    .map(|l| Ok((l.key_id.clone(), keys.cost(&l.key_id)?)))
                    .collect();
                let Ok(keys) = key_costs else {
                    continue;
                };
                let share = ShareStats {
                    account_id: account.clone(),
                    computed_at: now,
                    start_ms: start,
                    end_ms: window_end,
                    account_cost,
                    keys,
                };
                self.inner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shares
                    .insert(account, share);
            }
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    code: u32,
    data: Diagnostics,
}
#[derive(Deserialize)]
struct Diagnostics {
    items: Vec<DiagnosticItem>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticItem {
    key: String,
    estimated_cost: Option<String>,
}

pub struct DiagnosticCosts {
    costs: BTreeMap<String, Option<String>>,
    potentially_truncated: bool,
}
impl DiagnosticCosts {
    fn cost(&self, id: &str) -> Result<String, PluginFault> {
        match self.costs.get(id) {
            Some(Some(cost)) => Ok(cost.clone()),
            Some(None) => Err(fault("统计费用不可用")),
            None if self.potentially_truncated => Err(fault("统计结果可能已截断")),
            None => Ok("0".into()),
        }
    }
}
pub fn parse_diagnostics(body: &[u8]) -> Result<DiagnosticCosts, PluginFault> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| fault("统计响应格式错误"))?;
    if envelope.code != 200 {
        return Err(fault("统计结果不可用"));
    }
    let potentially_truncated = envelope.data.items.len() >= 100;
    let mut result = BTreeMap::new();
    for item in envelope.data.items {
        if item
            .estimated_cost
            .as_deref()
            .is_some_and(|cost| !valid_cost(cost))
            || result.insert(item.key, item.estimated_cost).is_some()
        {
            return Err(fault("统计费用无效"));
        }
    }
    Ok(DiagnosticCosts {
        costs: result,
        potentially_truncated,
    })
}

fn valid_cost(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|v| v.is_finite() && v >= 0.0)
}

pub fn mask_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    match chars.as_slice() {
        [] => "*".into(),
        [_] => "*".into(),
        [first, _] => format!("{first}*"),
        [first, middle @ .., last] => format!("{first}{}{last}", "*".repeat(middle.len())),
    }
}

#[derive(Serialize)]
pub struct ShareItem {
    pub label: String,
    pub me: bool,
    pub percent: f64,
}
#[derive(Serialize)]
pub struct MemberShares {
    pub computed_at: i64,
    pub items: Vec<ShareItem>,
    pub other_percent: f64,
}

pub fn member_shares(
    stats: &ShareStats,
    names: &BTreeMap<String, String>,
    own_key: &str,
) -> MemberShares {
    let total = stats.account_cost.parse::<f64>().unwrap_or(0.0);
    let mut allocated = 0.0;
    let mut items: Vec<_> = stats
        .keys
        .iter()
        .map(|(id, cost)| {
            let amount = cost.parse::<f64>().unwrap_or(0.0);
            allocated += amount;
            let me = id == own_key;
            let name = names.get(id).map(String::as_str).unwrap_or("成员");
            ShareItem {
                label: if me { name.to_owned() } else { mask_name(name) },
                me,
                percent: if total > 0.0 {
                    amount / total * 100.0
                } else {
                    0.0
                },
            }
        })
        .collect();
    items.sort_by(|a, b| b.percent.total_cmp(&a.percent));
    MemberShares {
        computed_at: stats.computed_at,
        items,
        other_percent: if total > 0.0 {
            (total - allocated).max(0.0) / total * 100.0
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_plugin_sdk::call::{data::QuotaFacts, key_budgets::KeyBudget};
    struct StatsHost {
        calls: Mutex<Vec<(String, i64, i64)>>,
        fail: Mutex<bool>,
    }
    impl Host for StatsHost {
        async fn statistics(
            &self,
            dimension: &str,
            start: i64,
            end: i64,
        ) -> Result<DiagnosticCosts, PluginFault> {
            self.calls
                .lock()
                .unwrap()
                .push((dimension.into(), start, end));
            if *self.fail.lock().unwrap() {
                return Err(fault("synthetic statistics failure"));
            }
            let key = if dimension == "account" {
                "account"
            } else {
                "key"
            };
            parse_diagnostics(&serde_json::to_vec(&serde_json::json!({"code":200,"data":{"items":[{"key":key,"estimatedCost":"2.5"}]}})).unwrap())
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
    #[test]
    fn names_mask_unicode_and_short_names() {
        assert_eq!(
            [mask_name("甲"), mask_name("甲乙"), mask_name("甲乙丙丁")],
            ["*", "甲*", "甲**丁"]
        );
    }
    #[test]
    fn shares_preserve_privacy_and_clamp_other() {
        let stats = ShareStats {
            account_id: "a".into(),
            computed_at: 1,
            start_ms: 0,
            end_ms: 2,
            account_cost: "10".into(),
            keys: BTreeMap::from([("me".into(), "8".into()), ("other".into(), "5".into())]),
        };
        let names = BTreeMap::from([
            ("me".into(), "我的名字".into()),
            ("other".into(), "其他成员".into()),
        ]);
        let result = member_shares(&stats, &names, "me");
        assert!(result.items[0].me);
        assert_eq!(result.items[0].label, "我的名字");
        assert_eq!(result.items[1].label, "其**员");
        assert_eq!(result.other_percent, 0.0);
        assert_eq!(result.items[0].percent, 80.0);
    }
    #[test]
    fn zero_total_gives_zero_percentages() {
        let stats = ShareStats {
            account_id: "a".into(),
            computed_at: 1,
            start_ms: 0,
            end_ms: 2,
            account_cost: "0".into(),
            keys: BTreeMap::from([("key".into(), "1".into())]),
        };
        let result = member_shares(&stats, &BTreeMap::new(), "key");
        assert_eq!((result.items[0].percent, result.other_percent), (0.0, 0.0));
    }
    #[test]
    fn diagnostics_require_complete_costs() {
        let complete = parse_diagnostics(br#"{"code":200,"data":{"items":[{"key":"k","estimatedCost":"1.23"},{"key":"unknown","estimatedCost":null}]}}"#).unwrap();
        assert_eq!(complete.cost("k").unwrap(), "1.23");
        assert!(complete.cost("unknown").is_err());
        assert_eq!(complete.cost("absent").unwrap(), "0");
        let items: Vec<_> = (0..100)
            .map(|i| serde_json::json!({"key":format!("k{i}"), "estimatedCost":"1"}))
            .collect();
        let truncated = parse_diagnostics(
            &serde_json::to_vec(&serde_json::json!({"code":200,"data":{"items":items}})).unwrap(),
        )
        .unwrap();
        assert_eq!(truncated.cost("k0").unwrap(), "1");
        assert!(truncated.cost("absent").is_err());
    }
    #[tokio::test]
    async fn refresh_uses_identical_windows_respects_interval_and_preserves_cache_on_failure() {
        let host = StatsHost {
            calls: Mutex::new(vec![]),
            fail: Mutex::new(false),
        };
        let state: State = serde_json::from_value(serde_json::json!({
            "early_auto":true,
            "links":{"key":{"key_id":"key","account_id":"account","baseline":{"observed":1000,"reset":WEEK+1000,"used":null},"paused":false,"pending":null,"events":[]}},
            "accounts":{}
        })).unwrap();
        let cache = ShareCache::default();
        let deadline = || tokio::time::Instant::now() + Duration::from_secs(2);
        cache.refresh(&host, &state, 2_000, deadline()).await;
        assert_eq!(
            *host.calls.lock().unwrap(),
            vec![
                ("account".into(), 1_000, 2_000),
                ("apiKey".into(), 1_000, 2_000)
            ]
        );
        assert_eq!(cache.snapshot()["account"].keys["key"], "2.5");
        cache.refresh(&host, &state, 2_001, deadline()).await;
        assert_eq!(host.calls.lock().unwrap().len(), 2);
        *host.fail.lock().unwrap() = true;
        cache.refresh(&host, &state, 602_001, deadline()).await;
        assert_eq!(host.calls.lock().unwrap().len(), 3);
        assert_eq!(cache.snapshot()["account"].computed_at, 2_000);
    }
}
