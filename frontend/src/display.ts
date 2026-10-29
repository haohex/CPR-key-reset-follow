// 页面与邮件统一按北京时间显示，旧状态中的 display_timezone 仅为兼容保留，不再生效
export const DISPLAY_TIMEZONE = 'Asia/Shanghai'

export function displayDate(value: number | null | undefined, timezone: string, full = false): string {
  if (value == null)
    return '暂无时间'
  const formatted = new Intl.DateTimeFormat('zh-CN', {
    timeZone: timezone,
    ...(full ? { year: 'numeric' } as const : {}),
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).format(value)
  return full ? `${formatted}（${timezoneOffset(timezone, value)}）` : formatted
}

function timezoneOffset(timezone: string, value: number): string {
  return new Intl.DateTimeFormat('en', { timeZone: timezone, timeZoneName: 'shortOffset' })
    .formatToParts(value)
    .find(part => part.type === 'timeZoneName')!
    .value
    .replace('GMT', 'UTC')
}

export function timezoneLabel(timezone: string, value = Date.now()): string {
  const offset = timezoneOffset(timezone, value)
  const name = timezone === 'Asia/Shanghai' ? '北京时间' : timezone === 'UTC' ? '协调世界时' : timezone
  return `${name}（${offset}）`
}

export function money(value: string | undefined): string {
  if (value == null || !Number.isFinite(Number(value)))
    return '暂无数据'
  return `$${Number(value).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`
}

export function percent(value: number | null | undefined): string {
  return value == null ? '暂无数据' : `${new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 }).format(value)}%`
}

export function usageRatio(
  used: string | number | null | undefined,
  limit: string | number | null | undefined,
): number | null {
  if (used == null || limit == null)
    return null
  const u = Number(used)
  const l = Number(limit)
  if (!Number.isFinite(u) || !Number.isFinite(l) || l <= 0)
    return null
  // 不舍入：颜色阈值按实际比例判断，避免 79.96% 被提前显示为警告色
  return Math.max(0, Math.min(100, (u / l) * 100))
}

export function relativeReset(resetMs: number | null | undefined, now = Date.now()): string {
  if (resetMs == null || !Number.isFinite(resetMs))
    return ''
  const diff = resetMs - now
  if (diff <= 0)
    return '已到期'
  const minutes = Math.floor(diff / 60000)
  if (minutes < 60)
    return `${Math.max(1, minutes)} 分钟后`
  const hours = Math.floor(diff / 3600000)
  if (hours < 24)
    return `${hours} 小时后`
  const days = Math.floor(diff / 86400000)
  return `${Math.max(1, days)} 天后`
}

export type UsageTone = 'primary' | 'warning' | 'danger'

// 用量比例 >=80% 呈警告色，>=95% 呈危险色，其余使用主色
export function usageTone(ratio: number | null | undefined): UsageTone {
  if (ratio == null || !Number.isFinite(ratio))
    return 'primary'
  if (ratio >= 95)
    return 'danger'
  if (ratio >= 80)
    return 'warning'
  return 'primary'
}

export type CardStatusText = '可用' | '使用中' | '已使用' | '结果未知' | '无卡'
export type CardTone = 'success' | 'warning' | 'danger' | 'neutral'

export function cardStatusText(
  status: string | null | undefined,
  hasActiveCard = true,
): CardStatusText {
  if (!hasActiveCard || !status)
    return '无卡'
  switch (status) {
    case 'available':
      return '可用'
    case 'using':
      return '使用中'
    case 'used':
      return '已使用'
    case 'unknown':
      return '结果未知'
    default:
      return '无卡'
  }
}

export function cardStatusTone(
  status: string | null | undefined,
  hasActiveCard = true,
): CardTone {
  if (!hasActiveCard || !status)
    return 'neutral'
  switch (status) {
    case 'available':
      return 'success'
    case 'using':
      return 'warning'
    case 'used':
      return 'neutral'
    case 'unknown':
      return 'danger'
    default:
      return 'neutral'
  }
}

export function formatCycleRange(
  cycle: { start_ms: number, end_ms: number } | null | undefined,
  timezone = DISPLAY_TIMEZONE,
): string {
  if (!cycle || cycle.start_ms == null || cycle.end_ms == null)
    return '暂无周期数据'
  return `${displayDate(cycle.start_ms, timezone)} 至 ${displayDate(cycle.end_ms, timezone)}`
}

export function formatMemberError(status: number, message?: string): string {
  switch (status) {
    case 401:
      return 'Key 无效'
    case 404:
      return '未加入拼车'
    case 409:
      return message || '不满足用卡条件'
    case 429:
      return '请求过多，请稍后重试'
    case 503:
      return '服务繁忙或不可用，请稍后重试'
    default:
      return message || '请求失败，请稍后重试'
  }
}

export interface SharePercentItem {
  label: string
  me: boolean
  percent: number
}

export function sortShareItems(
  items: SharePercentItem[] | null | undefined,
  otherPercent?: number | null,
): SharePercentItem[] {
  const result: SharePercentItem[] = items ? [...items] : []
  if (otherPercent != null && otherPercent > 0) {
    result.push({
      label: '其他',
      me: false,
      percent: otherPercent,
    })
  }
  return result.sort((a, b) => b.percent - a.percent)
}

export function calculateSharePercent(
  keyCost: string | number | null | undefined,
  accountCost: string | number | null | undefined,
): number | null {
  if (keyCost == null || accountCost == null)
    return null
  const kc = Number(keyCost)
  const ac = Number(accountCost)
  if (!Number.isFinite(kc) || !Number.isFinite(ac))
    return null
  if (ac <= 0)
    return 0
  return Math.max(0, Math.min(100, (kc / ac) * 100))
}

/**
 * 校验异步请求的会话标识是否与当前活跃会话一致。
 * 退出登录或重新登录将递增会话标识，从而丢弃迟到的过期响应。
 */
export function isSessionActive(requestSessionId: number, currentSessionId: number): boolean {
  return requestSessionId === currentSessionId && currentSessionId > 0
}

export interface ShareStatsValidationTarget {
  end_ms: number
  keys?: Record<string, string> | null
}

/**
 * 校验管理页账号占比缓存是否有效：
 * 只有当 shares[account_id] 的 end_ms 与关联账号当前样本的周窗口结束时间相差不超过 2 分钟、
 * 且该 Key 出现在 keys 映射中时，才显示金额与占比。
 */
export function isShareStatsValid(
  share: ShareStatsValidationTarget | null | undefined,
  sampleResetMs: number | null | undefined,
  keyId: string | null | undefined,
  maxDiffMs = 2 * 60 * 1000,
): boolean {
  if (!share || sampleResetMs == null || !keyId || !share.keys)
    return false
  if (!Number.isFinite(share.end_ms) || !Number.isFinite(sampleResetMs))
    return false
  if (Math.abs(share.end_ms - sampleResetMs) > maxDiffMs)
    return false
  return Object.hasOwn(share.keys, keyId) && share.keys[keyId] !== undefined
}

export function cardMailStatusText(status: string | null | undefined): string {
  if (status == null)
    return ''
  if (status === 'skipped')
    return '未发送（未启用或积压已满，见最近事件）'
  const map: Record<string, string> = {
    queued: '等待发送',
    unknown: '结果未知，可能已发送',
    succeeded: '成功',
    failed: '失败',
  }
  return map[status] || status
}

export function countCardNotices(notices?: { status: string }[] | null): { queued: number, unknown: number } {
  let queued = 0
  let unknown = 0
  if (notices) {
    for (const notice of notices) {
      if (notice.status === 'queued')
        queued++
      else if (notice.status === 'unknown')
        unknown++
    }
  }
  return { queued, unknown }
}

export function formatCardNoticeSummary(queued: number, unknown: number): string {
  return `等待发送 ${queued} 条 · 结果未知 ${unknown} 条`
}
