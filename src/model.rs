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
    #[serde(default)]
    pub last_manual_probe: Option<ManualTest>,
    #[serde(default)]
    pub last_manual_mail: Option<ManualTest>,
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
    /// 日限重置卡开关，旧状态缺失时为关闭
    #[serde(default)]
    pub card_enabled: bool,
    /// 插件确认的账号周期序号，只在 docs/design.md"发卡"列出的事件中加一
    #[serde(default)]
    pub cycle_seq: u64,
    /// 当前或最近一张卡；seq 不等于 cycle_seq 时已作废
    #[serde(default)]
    pub card: Option<DailyCard>,
    /// 用卡邮件独立保存，换周或补卡不会丢失尚未完成的通知
    #[serde(default)]
    pub card_notices: Vec<CardNotice>,
}
/// 使用中与结果未知都视为已用，不自动重试
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Available,
    Using,
    Used,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct DailyCard {
    pub seq: u64,
    pub issued_at: i64,
    pub status: CardStatus,
    #[serde(default)]
    pub used_at: Option<i64>,
    /// 用卡通知；None 表示用卡时 SMTP 未启用或没有收件邮箱，不发送
    #[serde(default)]
    pub mail: Option<SendStatus>,
    #[serde(default)]
    pub recipient: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CardNotice {
    pub id: u64,
    pub seq: u64,
    pub used_at: i64,
    pub result: CardStatus,
    pub status: SendStatus,
    pub recipient: String,
}
/// 维护任务刷新的本周期花费统计，只存内存，不写入私有状态
#[derive(Clone, Serialize, Deserialize)]
pub struct ShareStats {
    pub account_id: String,
    pub computed_at: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    /// 管理统计返回的十进制 USD 文本，保持原精度
    pub account_cost: String,
    /// Key ID 到本周期花费；未出现在统计中的拼车 Key 视为 0
    pub keys: BTreeMap<String, String>,
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

#[derive(Clone, Serialize, Deserialize)]
pub struct ManualTest {
    pub at: i64,
    pub status: SendStatus,
    pub recipient: Option<String>,
}
