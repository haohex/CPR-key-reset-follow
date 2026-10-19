//! 周期检查、不可逆清零及后续任务的持久化编排
use crate::{
    cycle::{self, Decision, Sample},
    host::{Host, fault},
    model::{Delivery, Link, ManualTest, Pending, Phase, SendStatus, State},
};
use gateway_plugin_sdk::PluginFault;

pub struct Engine<'a, H> {
    pub host: &'a H,
    pub state: State,
    pub version: Option<u64>,
}
impl<'a, H: Host> Engine<'a, H> {
    pub async fn load(host: &'a H) -> Result<Self, PluginFault> {
        let (state, version) = host.load().await?;
        Ok(Self {
            host,
            state,
            version,
        })
    }
    pub async fn save(&mut self) -> Result<(), PluginFault> {
        self.version = Some(self.host.save(&self.state, self.version).await?);
        Ok(())
    }
    pub async fn fresh(&self, account: &str) -> Result<Sample, PluginFault> {
        let facts = self.host.refresh(account).await?;
        if facts.account_id != account {
            return Err(fault("上游返回了其他账号的样本"));
        }
        cycle::sample(&facts).map_err(fault)
    }
    pub async fn add(&mut self, key: &str, account: &str, now: i64) -> Result<(), PluginFault> {
        if self.state.links.contains_key(key) {
            return Err(fault("该 Key 已有关联"));
        }
        self.host.budget(key).await?;
        let baseline = self.fresh(account).await?;
        let phase = if cycle::floating(&baseline) {
            Phase::default()
        } else {
            Phase::Tracking
        };
        self.state.accounts.entry(account.into()).or_default();
        self.state.links.insert(
            key.into(),
            Link {
                last_manual_probe: None,
                last_manual_mail: None,
                recipient: String::new(),
                deliveries: vec![],
                key_id: key.into(),
                account_id: account.into(),
                baseline,
                paused: false,
                pending: None,
                events: Default::default(),
                retry_at: 0,
                phase,
            },
        );
        self.state
            .links
            .get_mut(key)
            .unwrap()
            .event(now, "建立基线，不清零");
        self.save().await
    }
    pub async fn check(&mut self, account_id: &str, now: i64) -> Result<(), PluginFault> {
        let facts = self.host.refresh(account_id).await;
        if let Ok(facts) = &facts {
            let changed = facts.windows.iter().any(|w| {
                w.key.starts_with("codex:604800s")
                    && (w.key != "codex:604800s" || w.window_seconds != Some(604800))
            });
            if facts.account_id == account_id && changed {
                for link in self
                    .state
                    .links
                    .values_mut()
                    .filter(|l| l.account_id == account_id && !l.paused && l.pending.is_none())
                {
                    if facts
                        .observed_at_ms
                        .is_some_and(|t| t > link.baseline.observed)
                    {
                        link.pending = Some(Pending::Changed {
                            windows: facts.windows.clone(),
                        });
                    }
                }
            }
        }
        let result = facts.and_then(|facts| {
            if facts.account_id != account_id {
                return Err(fault("账号样本不匹配"));
            }
            cycle::sample(&facts).map_err(fault)
        });
        let account = self.state.accounts.entry(account_id.into()).or_default();
        let sample = match result {
            Ok(s)
                if account
                    .sample
                    .as_ref()
                    .is_none_or(|prev| s.observed > prev.observed) =>
            {
                s
            }
            result => {
                account.failure_since.get_or_insert(now);
                account.failures = account.failures.saturating_add(1);
                account.next_check =
                    now + (30_000_i64.saturating_mul(1 << account.failures.min(6))).min(900_000);
                // 不透传可能包含代理地址或上游响应正文的错误详情。
                account.error = Some(match result {
                    Ok(_) => "上游仍返回旧样本".into(),
                    Err(e) => format!("无法刷新账号额度（{:?}）", e.code),
                });
                return self.save().await;
            }
        };
        account.sample = Some(sample.clone());
        account.next_check = now + 300_000;
        account.failure_since = None;
        account.failures = 0;
        account.error = None;
        for link in self
            .state
            .links
            .values_mut()
            .filter(|l| l.account_id == account_id && !l.paused && l.pending.is_none())
        {
            if let Phase::Settling { candidate } = &mut link.phase {
                // 下一个完整周周期已越过上次结束时间，可独立处理正常换周。
                if cycle::classify(&link.baseline, &sample) == Decision::Normal {
                    link.pending = Some(Pending::Ready {
                        sample: sample.clone(),
                        reason: "正常换周".into(),
                    });
                    continue;
                }
                if cycle::floating(&sample) {
                    *candidate = None;
                } else if let Some(first) = candidate {
                    let reset_shift = i128::from(sample.reset) - i128::from(first.reset);
                    let stable = sample.observed - first.observed >= cycle::SETTLE_INTERVAL
                        && reset_shift.abs() <= 1_000;
                    if stable {
                        link.baseline = sample.clone();
                        link.phase = Phase::Tracking;
                    } else if reset_shift.abs() > 1_000 {
                        *candidate = Some(sample.clone());
                    }
                } else {
                    *candidate = Some(sample.clone());
                }
                continue;
            }
            let decision = cycle::classify(&link.baseline, &sample);
            match decision {
                Decision::Same => link.baseline.observed = sample.observed,
                Decision::Normal | Decision::Early
                    if decision == Decision::Normal || self.state.early_auto =>
                {
                    link.pending = Some(Pending::Ready {
                        sample: sample.clone(),
                        reason: if decision == Decision::Normal {
                            "正常换周"
                        } else {
                            "提前重置"
                        }
                        .into(),
                    });
                }
                Decision::Early | Decision::Unexplained => {
                    link.pending = Some(Pending::Confirm {
                        sample: sample.clone(),
                        reason: decision,
                    })
                }
                Decision::Stale | Decision::Normal => {}
            }
        }
        self.save().await?;
        self.drain(now).await
    }
    pub async fn drain(&mut self, now: i64) -> Result<(), PluginFault> {
        let keys: Vec<_> = self
            .state
            .links
            .iter()
            .filter(|(_, l)| {
                !l.paused && l.retry_at <= now && matches!(l.pending, Some(Pending::Ready { .. }))
            })
            .take(8)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.execute(&key, now).await?;
        }
        Ok(())
    }
    pub async fn execute(&mut self, key: &str, now: i64) -> Result<(), PluginFault> {
        let Some(Pending::Ready { sample, reason }) =
            self.state.links.get(key).and_then(|l| l.pending.clone())
        else {
            return Ok(());
        };
        // 停机或预算读取失败后，不能用过期的排队样本补做旧周期清零。
        if i128::from(now) - i128::from(sample.observed) > 300_000 {
            let link = self.state.links.get_mut(key).unwrap();
            link.pending = None;
            link.event(now, "排队样本已过期，等待重新检查当前周期");
            self.state
                .accounts
                .entry(link.account_id.clone())
                .or_default()
                .next_check = now;
            return self.save().await;
        }
        let before = match self.host.budget(key).await {
            Ok(b) => b,
            Err(_) => {
                self.state.links.get_mut(key).unwrap().retry_at = now + 300_000;
                self.state
                    .links
                    .get_mut(key)
                    .unwrap()
                    .event(now, "读取预算失败，尚未调用清零");
                return self.save().await;
            }
        };
        // 未完成意图就是未知结果，重启无需猜测调用是否曾发送。
        self.state.links.get_mut(key).unwrap().pending = Some(Pending::Unknown {
            sample: sample.clone(),
            reason: reason.clone(),
            before,
            error: None,
        });
        self.save().await?;
        let result = self.host.reset(key).await;
        while self
            .state
            .links
            .values()
            .map(|l| l.deliveries.len())
            .sum::<usize>()
            >= 128
        {
            let oldest = self
                .state
                .links
                .iter()
                .flat_map(|(key, l)| {
                    l.deliveries
                        .iter()
                        .enumerate()
                        .filter(|(_, d)| {
                            !matches!(
                                d.probe,
                                SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                            ) && !matches!(
                                d.mail,
                                SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                            )
                        })
                        .map(move |(i, d)| (d.reset_at, key.clone(), i))
                })
                .min();
            let Some((_, old_key, i)) = oldest else {
                break;
            };
            self.state
                .links
                .get_mut(&old_key)
                .unwrap()
                .deliveries
                .remove(i);
        }
        let deliveries_full = self
            .state
            .links
            .values()
            .map(|l| l.deliveries.len())
            .sum::<usize>()
            >= 128;
        let link = self.state.links.get_mut(key).unwrap();
        match result {
            Ok(()) => {
                link.baseline = sample;
                link.phase = Phase::default();
                link.pending = None;
                link.event(now, reason);
                let id = link
                    .deliveries
                    .iter()
                    .map(|d| d.id)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                // 完成记录只保留最近十条，未确认任务不丢弃；积压满时跳过新增通知
                let mut completed = link
                    .deliveries
                    .iter()
                    .filter(|d| {
                        !matches!(
                            d.probe,
                            SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                        ) && !matches!(
                            d.mail,
                            SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                        )
                    })
                    .count();
                link.deliveries.retain(|d| {
                    if completed > 10
                        && !matches!(
                            d.probe,
                            SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                        )
                        && !matches!(
                            d.mail,
                            SendStatus::Queued | SendStatus::Unknown | SendStatus::Failed
                        )
                    {
                        completed -= 1;
                        false
                    } else {
                        true
                    }
                });
                if link.deliveries.len() < 32 && !deliveries_full {
                    link.deliveries.push(Delivery {
                        id,
                        reset_at: now,
                        recipient: link.recipient.clone(),
                        probe: if self.state.probe_enabled {
                            SendStatus::Queued
                        } else {
                            SendStatus::Skipped
                        },
                        mail: if self.state.smtp.enabled && !link.recipient.is_empty() {
                            SendStatus::Queued
                        } else {
                            SendStatus::Skipped
                        },
                    });
                } else {
                    link.event(now, "后续任务积压已满，本次未排队测试与邮件");
                }
            }
            Err(e) => {
                if let Some(Pending::Unknown { error, .. }) = &mut link.pending {
                    *error = Some(format!("{:?}", e.code));
                }
                link.event(now, "结果未知，已停止自动重试");
            }
        }
        self.save().await
    }
    // 每次维护最多执行一个外部发送，先保存 Unknown，重启后不会盲目重发
    pub async fn deliver_one(&mut self) -> Result<(), PluginFault> {
        let next = self
            .state
            .links
            .iter()
            .filter(|(_, l)| !l.paused)
            .find_map(|(key, l)| {
                l.deliveries
                    .iter()
                    .position(|d| {
                        (d.probe == SendStatus::Queued && self.state.probe_enabled)
                            || ((d.probe != SendStatus::Queued || !self.state.probe_enabled)
                                && d.mail == SendStatus::Queued
                                && self.state.smtp.enabled)
                    })
                    .map(|i| (key.clone(), i, l.account_id.clone()))
            });
        let Some((key, i, account)) = next else {
            return Ok(());
        };
        if !self.state.probe_enabled
            && self.state.links[&key].deliveries[i].probe == SendStatus::Queued
        {
            self.state.links.get_mut(&key).unwrap().deliveries[i].probe = SendStatus::Skipped;
        }
        let probe = self.state.probe_enabled
            && self.state.links[&key].deliveries[i].probe == SendStatus::Queued;
        if probe && !self.state.probe_enabled || !probe && !self.state.smtp.enabled {
            return Ok(());
        }
        let job = self.state.links[&key].deliveries[i].clone();
        let status = if probe {
            &mut self.state.links.get_mut(&key).unwrap().deliveries[i].probe
        } else {
            &mut self.state.links.get_mut(&key).unwrap().deliveries[i].mail
        };
        *status = SendStatus::Unknown;
        self.save().await?;
        let result = if probe {
            tokio::time::timeout(
                std::time::Duration::from_secs(20),
                self.host.probe(&key, &account),
            )
            .await
        } else {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                let name = self
                    .host
                    .key_name(&key)
                    .await
                    .unwrap_or_else(|_| "关联 Key".into());
                let reset_time =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(job.reset_at)
                        .and_then(|t| {
                            chrono::FixedOffset::east_opt(8 * 3600)
                                .map(|offset| t.with_timezone(&offset))
                        })
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC+8").to_string())
                        .unwrap_or_else(|| "时间不可用".into());
                let body = format!(
                    "Key：{name}\n周额度已重置\n重置时间：{reset_time}\n模型测试：{}\n",
                    job.probe.label()
                );
                self.host
                    .email(
                        &self.state.smtp,
                        &job.recipient,
                        "CPR Key 周额度已重置",
                        &body,
                    )
                    .await
            })
            .await
        };
        // 超时与外部错误都不能证明未发送，保留未知，管理员显式确认后才能重试
        if matches!(result, Ok(Ok(()))) {
            let d = &mut self.state.links.get_mut(&key).unwrap().deliveries[i];
            if probe {
                d.probe = SendStatus::Succeeded;
            } else {
                d.mail = SendStatus::Succeeded;
            }
        }
        self.save().await
    }
    pub async fn manual_test(
        &mut self,
        key: &str,
        probe: bool,
        acknowledge: bool,
        expected_version: Option<u64>,
        now: i64,
    ) -> Result<(), PluginFault> {
        if expected_version != self.version {
            return Err(fault("页面状态已变化，请刷新后重试"));
        }
        if !acknowledge {
            return Err(fault("请确认测试会实际发送，重复操作可能重复计费或投递"));
        }
        let link = self
            .state
            .links
            .get(key)
            .ok_or_else(|| fault("关联不存在"))?;
        let account = link.account_id.clone();
        let recipient = link.recipient.clone();
        if !probe {
            crate::delivery::validate_connection(&self.state.smtp)?;
            if recipient.is_empty() {
                return Err(fault("请先保存该关联的收件邮箱"));
            }
            crate::delivery::validate_recipient(&recipient)?;
        }
        let record = ManualTest {
            at: now,
            status: SendStatus::Unknown,
            recipient: (!probe).then_some(recipient.clone()),
        };
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or_else(|| fault("关联不存在"))?;
        if probe {
            link.last_manual_probe = Some(record);
        } else {
            link.last_manual_mail = Some(record);
        }
        // 精确版本写入意图后才发出一次，维护不会领取手动测试记录
        self.save().await?;
        let result = if probe {
            tokio::time::timeout(
                std::time::Duration::from_secs(20),
                self.host.probe(key, &account),
            )
            .await
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(8),
                self.host.email(
                    &self.state.smtp,
                    &recipient,
                    "CPR SMTP测试",
                    "这是一封由管理员手动发送的 SMTP测试邮件，用于验证已保存的邮件配置与收件地址",
                ),
            )
            .await
        };
        if matches!(result, Ok(Ok(()))) {
            let link = self
                .state
                .links
                .get_mut(key)
                .ok_or_else(|| fault("关联不存在"))?;
            let record = if probe {
                &mut link.last_manual_probe
            } else {
                &mut link.last_manual_mail
            };
            if let Some(record) = record {
                record.status = SendStatus::Succeeded;
            }
        }
        self.save().await
    }
    pub async fn retry_delivery(
        &mut self,
        key: &str,
        id: u64,
        probe: bool,
        acknowledge: bool,
    ) -> Result<(), PluginFault> {
        if !acknowledge {
            return Err(fault("请确认重试可能产生重复请求或重复邮件"));
        }
        let link = self
            .state
            .links
            .get_mut(key)
            .ok_or_else(|| fault("关联不存在"))?;
        if link.paused {
            return Err(fault("关联已暂停"));
        }
        let job = link
            .deliveries
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| fault("发送记录不存在"))?;
        let status = if probe { &mut job.probe } else { &mut job.mail };
        if !matches!(status, SendStatus::Unknown | SendStatus::Failed) {
            return Err(fault("只有失败或未知任务可以重试"));
        }
        *status = SendStatus::Queued;
        self.save().await
    }
    pub async fn action(
        &mut self,
        key: &str,
        action: &str,
        acknowledge: bool,
        now: i64,
    ) -> Result<(), PluginFault> {
        let link = self
            .state
            .links
            .get(key)
            .cloned()
            .ok_or_else(|| fault("关联不存在，请刷新页面"))?;
        match action {
            "pause" => {
                self.state.links.get_mut(key).unwrap().paused = true;
            }
            "delete" if acknowledge => {
                self.state.links.remove(key);
                self.state
                    .accounts
                    .retain(|id, _| self.state.links.values().any(|l| &l.account_id == id));
            }
            "resume" | "skip" => {
                if action == "resume" && !link.paused {
                    return Err(fault("关联未暂停"));
                }
                if action == "skip" && link.pending.is_none() {
                    return Err(fault("没有待处理的变化"));
                }
                let fresh = self.fresh(&link.account_id).await?;
                let l = self.state.links.get_mut(key).unwrap();
                l.baseline = fresh;
                l.phase = if cycle::floating(&l.baseline) {
                    Phase::default()
                } else {
                    Phase::Tracking
                };
                l.pending = None;
                l.paused = false;
                l.event(now, "保留现有用量并重新建立基线");
            }
            "confirm" | "retry" => {
                if link.paused {
                    return Err(fault("关联已暂停"));
                }
                let fresh = self.fresh(&link.account_id).await?;
                let expected = match &link.pending {
                    Some(Pending::Changed { .. }) if action == "confirm" => &fresh,
                    Some(Pending::Confirm { sample, .. }) if action == "confirm" => sample,
                    Some(Pending::Unknown { sample, .. }) if action == "retry" && acknowledge => {
                        sample
                    }
                    _ => return Err(fault("状态已变化或尚未确认不可逆后果，请刷新页面")),
                };
                if (i128::from(fresh.reset) - i128::from(expected.reset)).abs()
                    > i128::from(cycle::TOLERANCE)
                {
                    return Err(fault("上游周期已变化，请先跳过并重新建立基线"));
                }
                self.state.links.get_mut(key).unwrap().pending = Some(Pending::Ready {
                    sample: fresh,
                    reason: if action == "retry" {
                        "人工再次清零"
                    } else {
                        "手动确认"
                    }
                    .into(),
                });
                self.save().await?;
                return self.execute(key, now).await;
            }
            _ => return Err(fault("无效操作或缺少删除确认")),
        }
        self.save().await
    }
}
