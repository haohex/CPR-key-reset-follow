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
