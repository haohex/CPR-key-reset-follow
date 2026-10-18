//! 关联、周期与发送任务的持久状态
use crate::cycle::{Decision, Sample};
use gateway_plugin_sdk::call::key_budgets::KeyBudget;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: i64,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pending {
    Changed {
        windows: Vec<gateway_plugin_sdk::call::data::QuotaWindowFacts>,
    },
    Confirm {
        sample: Sample,
        reason: Decision,
    },
    Ready {
        sample: Sample,
        reason: String,
    },
    Unknown {
        sample: Sample,
        reason: String,
        before: KeyBudget,
        error: Option<String>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Link {
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub deliveries: Vec<Delivery>,
    pub key_id: String,
    pub account_id: String,
    pub baseline: Sample,
    pub paused: bool,
    pub pending: Option<Pending>,
    pub events: VecDeque<Event>,
    #[serde(default)]
    pub retry_at: i64,
    #[serde(default)]
    pub phase: Phase,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase {
    Tracking,
    Settling { candidate: Option<Sample> },
}
impl Default for Phase {
    fn default() -> Self {
        Self::Settling { candidate: None }
    }
}
impl Link {
    pub fn event(&mut self, at: i64, reason: impl Into<String>) {
        self.events.push_back(Event {
            at,
            reason: reason.into(),
        });
        while self.events.len() > 10 {
            self.events.pop_front();
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Account {
    pub sample: Option<Sample>,
    pub next_check: i64,
    pub failure_since: Option<i64>,
    pub failures: u32,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub probe_enabled: bool,
    #[serde(default)]
    pub smtp: SmtpSettings,
    pub early_auto: bool,
    #[serde(default)]
    pub display_timezone: DisplayTimezone,
    pub links: BTreeMap<String, Link>,
    pub accounts: BTreeMap<String, Account>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            probe_enabled: false,
            smtp: SmtpSettings::default(),
            early_auto: true,
            display_timezone: DisplayTimezone::default(),
            links: BTreeMap::new(),
            accounts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
pub enum DisplayTimezone {
    #[default]
    #[serde(rename = "browser")]
    Browser,
    #[serde(rename = "Asia/Shanghai")]
    Shanghai,
    #[serde(rename = "UTC")]
    Utc,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpSettings {
    pub enabled: bool,
    pub server: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub username: String,
    pub password: String,
    pub from: String,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    Tls,
    #[default]
    Starttls,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    Skipped,
    Queued,
    Unknown,
    Succeeded,
    Failed,
}
impl SendStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Skipped => "未启用",
            Self::Queued => "等待发送",
            Self::Unknown => "结果未知，可能已发送",
            Self::Succeeded => "成功",
            Self::Failed => "失败",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: u64,
    pub reset_at: i64,
    pub recipient: String,
    pub probe: SendStatus,
    pub mail: SendStatus,
}
