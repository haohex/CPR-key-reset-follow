//! 成员 HTTP 入口，其他路径在调用本模块前直接委托宿主
use gateway_plugin_sdk::{
    call::{host::KeyListRequest, middleware::MiddlewareHeader},
    client::{HttpBody, HttpCall, HttpResponse},
};
use key_reset_follow::{
    card::{self, UseError, UseResult},
    engine::Engine,
    host::{Host, SdkHost},
    stats::{self, ShareCache},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;

pub fn is_member_path(uri: &str) -> bool {
    let path = uri.split('?').next().unwrap_or("");
    path == "/carpool" || path.starts_with("/carpool/")
}

#[derive(Default)]
pub struct Resources {
    index: Option<Vec<u8>>,
    js: Option<Vec<u8>>,
    css: Option<Vec<u8>>,
}
impl Resources {
    pub fn load() -> Self {
        let directory = std::env::current_exe().ok().and_then(|exe| {
            exe.parent()
                .and_then(|bin| bin.parent())
                .map(|root| root.join("web/carpool"))
        });
        let read = |name: &str| {
            directory
                .as_ref()
                .and_then(|dir: &PathBuf| std::fs::read(dir.join(name)).ok())
        };
        Self {
            index: read("index.html"),
            js: read("carpool.js"),
            css: read("carpool.css"),
        }
    }
}

#[derive(Default)]
pub struct Limiter {
    inner: Mutex<LimitState>,
}
#[derive(Default)]
struct LimitState {
    global: VecDeque<Instant>,
    ips: BTreeMap<String, VecDeque<Instant>>,
    inflight: BTreeMap<String, usize>,
    total_inflight: usize,
}
struct Reservation {
    limiter: Arc<Limiter>,
    ip: String,
    failed: bool,
}
impl Limiter {
    fn reserve(self: &Arc<Self>, ip: &str) -> Option<Reservation> {
        let now = Instant::now();
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while state
            .global
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            state.global.pop_front();
        }
        state.ips.retain(|_, timestamps| {
            while timestamps
                .front()
                .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
            {
                timestamps.pop_front();
            }
            !timestamps.is_empty()
        });
        let failures = state.ips.get(ip).map_or(0, VecDeque::len);
        let active = state.inflight.get(ip).copied().unwrap_or(0);
        if failures + active >= 10 || state.global.len() + state.total_inflight >= 200 {
            return None;
        }
        *state.inflight.entry(ip.into()).or_default() += 1;
        state.total_inflight += 1;
        Some(Reservation {
            limiter: self.clone(),
            ip: ip.into(),
            failed: false,
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.limiter.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = state.inflight.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                state.inflight.remove(&self.ip);
            }
        }
        state.total_inflight -= 1;
        if self.failed {
            let now = Instant::now();
            state.global.push_back(now);
            state.ips.entry(self.ip.clone()).or_default().push_back(now);
        }
    }
}

struct CatchPanic<F>(Pin<Box<F>>);
impl<F: Future> Future for CatchPanic<F> {
    type Output = Result<F::Output, ()>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match catch_unwind(AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(Err(())),
        }
    }
}
fn header(name: &str, value: &str) -> MiddlewareHeader {
    MiddlewareHeader {
        name: name.into(),
        value: value.as_bytes().to_vec(),
    }
}
fn response(status: u16, content_type: &str, body: Vec<u8>) -> HttpResponse {
    let mut response = HttpResponse::new(status, HttpBody::from_bytes(body));
    response.headers = vec![
        header("content-type", content_type),
        header("cache-control", "no-store"),
    ];
    response
}
fn json_response(status: u16, value: Value) -> HttpResponse {
    response(
        status,
        "application/json; charset=utf-8",
        serde_json::to_vec(&value).unwrap_or_default(),
    )
}
fn error(status: u16, message: &str) -> HttpResponse {
    json_response(status, json!({"error": message}))
}

async fn member_names(
    host: &SdkHost,
    wanted: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, gateway_plugin_sdk::PluginFault> {
    let mut names = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = host
            .0
            .list_keys(KeyListRequest { cursor, limit: 200 })
            .await?;
        for key in page.keys {
            if wanted.contains(&key.id) {
                names.insert(key.id, key.name);
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() || names.len() == wanted.len() {
            break;
        }
    }
    Ok(names)
}

pub async fn run(
    call: HttpCall,
    lock: Arc<AsyncMutex<()>>,
    resources: Arc<Resources>,
    limiter: Arc<Limiter>,
    shares: Arc<ShareCache>,
) -> HttpResponse {
    match tokio::time::timeout(
        Duration::from_secs(15),
        CatchPanic(Box::pin(handle(call, lock, resources, limiter, shares))),
    )
    .await
    {
        Ok(Ok(response)) => response,
        _ => error(503, "服务暂不可用，请稍后重试"),
    }
}

async fn handle(
    call: HttpCall,
    lock: Arc<AsyncMutex<()>>,
    resources: Arc<Resources>,
    limiter: Arc<Limiter>,
    shares: Arc<ShareCache>,
) -> HttpResponse {
    let path = call.request.uri.split('?').next().unwrap_or("");
    if path == "/carpool" {
        let mut redirect = response(302, "text/plain; charset=utf-8", Vec::new());
        redirect.headers.push(header("location", "/carpool/"));
        return redirect;
    }
    let resource = match path {
        "/carpool/" => Some((&resources.index, "text/html; charset=utf-8")),
        "/carpool/carpool.js" => Some((&resources.js, "text/javascript; charset=utf-8")),
        "/carpool/carpool.css" => Some((&resources.css, "text/css; charset=utf-8")),
        _ => None,
    };
    if let Some((asset, content_type)) = resource {
        return match asset {
            Some(data) => response(200, content_type, data.clone()),
            None => error(503, "成员页面资源不可用"),
        };
    }
    if !matches!(
        (call.request.method.as_str(), path),
        ("GET", "/carpool/api/me") | ("POST", "/carpool/api/card/use")
    ) {
        return error(404, "页面不存在");
    }
    let ip = call
        .context
        .request
        .as_ref()
        .and_then(|request| request.client_ip.as_deref())
        .unwrap_or("unknown");
    let Some(mut reservation) = limiter.reserve(ip) else {
        return error(429, "尝试过多，请稍后重试");
    };
    let mut authorizations = call
        .request
        .headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("authorization"));
    let authorization = authorizations
        .next()
        .and_then(|h| std::str::from_utf8(&h.value).ok())
        .filter(|v| v.strip_prefix("Bearer ").is_some_and(|key| !key.is_empty()));
    if authorizations.next().is_some() || authorization.is_none() {
        reservation.failed = true;
        return error(401, "Key 无效");
    }
    let host = SdkHost(call.host);
    let key = match host.verify_client(authorization.unwrap_or_default()).await {
        Ok(key) => key,
        Err(fault)
            if matches!(
                fault.code,
                gateway_plugin_sdk::ErrorCode::PermissionDenied
                    | gateway_plugin_sdk::ErrorCode::Rejected
                    | gateway_plugin_sdk::ErrorCode::InvalidInput
            ) =>
        {
            reservation.failed = true;
            return error(401, "Key 无效");
        }
        Err(_) => return error(503, "认证服务暂不可用"),
    };
    drop(reservation);
    let guard = match tokio::time::timeout(Duration::from_secs(5), lock.lock()).await {
        Ok(guard) => guard,
        Err(_) => return error(503, "服务繁忙，请稍后重试"),
    };
    let mut engine = match Engine::load(&host).await {
        Ok(engine) => engine,
        Err(_) => return error(503, "服务暂不可用"),
    };
    if !engine.state.links.contains_key(&key) {
        return error(404, "未加入拼车");
    }
    if path == "/carpool/api/card/use" {
        return match engine.use_card(&key, crate::now()).await {
            Ok(UseResult::Used) => {
                json_response(200, json!({"result":"used", "message":"日用量已清零"}))
            }
            Ok(UseResult::Unknown) => json_response(
                200,
                json!({"result":"unknown", "message":"结果未知，请联系管理员核实"}),
            ),
            Err(UseError::Blocked(message)) => error(409, message),
            Err(UseError::Unavailable) => error(503, "服务暂不可用，请稍后重试"),
        };
    }
    let state = engine.state;
    drop(guard);
    let link = &state.links[&key];
    let budget = match host.budget(&key).await {
        Ok(budget) if budget.client_key_id == key => budget,
        _ => return error(503, "预算暂不可用"),
    };
    let reason = card::block_reason(link, &budget);
    let sample = state
        .accounts
        .get(&link.account_id)
        .and_then(|account| account.sample.as_ref())
        .unwrap_or(&link.baseline);
    let cycle = sample
        .reset
        .checked_sub(key_reset_follow::cycle::WEEK)
        .map(|start| json!({"start_ms":start, "end_ms":sample.reset}));
    let current_card = link.card.as_ref().filter(|card| card.seq == link.cycle_seq).map(|card| json!({"status":card.status, "issued_at":card.issued_at, "used_at":card.used_at}));
    let cache = shares.snapshot();
    let selected = cache
        .get(&link.account_id)
        .filter(|stats| stats.keys.contains_key(&key) && stats.end_ms == sample.reset)
        .map(|stats| {
            let mut stats = stats.clone();
            stats.keys.retain(|id, _| {
                state
                    .links
                    .get(id)
                    .is_some_and(|member| member.account_id == link.account_id)
            });
            stats
        });
    let mut wanted: BTreeSet<String> = selected
        .as_ref()
        .map(|stats| stats.keys.keys().cloned().collect())
        .unwrap_or_default();
    wanted.insert(key.clone());
    let names = match member_names(&host, &wanted).await {
        Ok(names) => names,
        Err(_) => return error(503, "Key 信息暂不可用"),
    };
    let Some(name) = names.get(&key) else {
        return error(503, "Key 信息暂不可用");
    };
    let member_shares = selected
        .as_ref()
        .map(|stats| stats::member_shares(stats, &names, &key));
    json_response(
        200,
        json!({
            "key_name": name, "paused":link.paused, "card_enabled":link.card_enabled, "card":current_card,
            "cycle":cycle, "budget":{"daily_used_usd":budget.daily_used_usd, "daily_limit_usd":budget.daily_limit_usd, "weekly_used_usd":budget.weekly_used_usd, "weekly_limit_usd":budget.weekly_limit_usd},
            "can_use_card":reason.is_none(), "card_block_reason":reason, "shares":member_shares
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn non_member_path_classification_never_waits_for_lock() {
        let lock = AsyncMutex::new(());
        let _guard = lock.lock().await;
        assert!(!is_member_path("/v1/responses?x=1"));
        assert!(is_member_path("/carpool/api/me"));
    }
    #[test]
    fn member_path_requires_exact_segment_and_ignores_query() {
        for uri in [
            "/carpool",
            "/carpool?x",
            "/carpool/",
            "/carpool/?x=1",
            "/carpool/api/me?x=/other",
        ] {
            assert!(is_member_path(uri), "{uri}");
        }
        for uri in [
            "/carpoolxyz",
            "/carpool-other/api",
            "/carpoolx",
            "/carpoolx?x=1",
            "/Carpool",
            "/CARPOOL/api",
            "/?next=/carpool/",
        ] {
            assert!(!is_member_path(uri), "{uri}");
        }
    }
    #[test]
    fn limiter_counts_failures_and_releases_successes() {
        let limiter = Arc::new(Limiter::default());
        drop(limiter.reserve("a").unwrap());
        for _ in 0..10 {
            let mut reservation = limiter.reserve("a").unwrap();
            reservation.failed = true;
        }
        assert!(limiter.reserve("a").is_none());
        assert!(limiter.reserve("b").is_some());
    }
    #[test]
    fn limiter_enforces_global_failure_limit() {
        let limiter = Arc::new(Limiter::default());
        for ip in 0..200 {
            let mut reservation = limiter.reserve(&ip.to_string()).unwrap();
            reservation.failed = true;
        }
        assert!(limiter.reserve("new-client").is_none());
    }
}
