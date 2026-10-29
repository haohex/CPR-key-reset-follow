import assert from 'node:assert/strict'
import { it } from 'node:test'
import {
  calculateSharePercent,
  cardMailStatusText,
  cardStatusText,
  cardStatusTone,
  countCardNotices,
  DISPLAY_TIMEZONE,
  displayDate,
  formatCardNoticeSummary,
  formatCycleRange,
  formatMemberError,
  isSessionActive,
  isShareStatsValid,
  money,
  percent,
  relativeReset,
  sortShareItems,
  timezoneLabel,
  usageRatio,
  usageTone,
} from '../src/display.ts'

it('时区只改变显示，同一时间跨日和夏令时仍正确', () => {
  const value = Date.parse('2026-10-03T02:10:00Z')
  assert.equal(displayDate(value, 'Asia/Shanghai'), '10/03 10:10')
  assert.equal(displayDate(value, 'UTC'), '10/03 02:10')
  assert.equal(displayDate(value, 'America/Los_Angeles'), '10/02 19:10')
  assert.equal(displayDate(value, 'Asia/Shanghai', true), '2026/10/03 10:10（UTC+8）')
  assert.equal(timezoneLabel('America/Los_Angeles', value), 'America/Los_Angeles（UTC-7）')
  assert.equal(timezoneLabel('America/Los_Angeles', Date.parse('2026-12-01T00:00:00Z')), 'America/Los_Angeles（UTC-8）')
  assert.equal(DISPLAY_TIMEZONE, 'Asia/Shanghai')
  assert.equal(timezoneLabel(DISPLAY_TIMEZONE, value), '北京时间（UTC+8）')
  assert.equal(displayDate(null, 'UTC'), '暂无时间')
})

it('金额和账号用量区分空值与零，并控制展示精度', () => {
  assert.equal(money('66.08130564'), '$66.08')
  assert.equal(money('239.8797087727'), '$239.88')
  assert.equal(money('0'), '$0.00')
  assert.equal(money(undefined), '暂无数据')
  assert.equal(percent(null), '暂无数据')
  assert.equal(percent(0), '0%')
  assert.equal(percent(45.123), '45.1%')
})

it('用量比例与相对时间格式化正确处理各种边界', () => {
  assert.ok(Math.abs(usageRatio('27.08', '100.00')! - 27.08) < 1e-9)
  // 阈值按实际比例判断，不能因舍入提前变色
  assert.equal(usageTone(usageRatio('79.96', '100')), 'primary')
  assert.equal(usageTone(usageRatio('94.96', '100')), 'warning')
  assert.equal(usageTone(usageRatio('95', '100')), 'danger')
  assert.equal(usageRatio('0', '100'), 0)
  assert.equal(usageRatio('150', '100'), 100)
  assert.equal(usageRatio('50', '0'), null)
  assert.equal(usageRatio(undefined, '100'), null)
  assert.equal(usageRatio('50', null), null)

  const baseNow = Date.parse('2026-10-28T12:00:00Z')
  assert.equal(relativeReset(baseNow + 3 * 86400000, baseNow), '3 天后')
  assert.equal(relativeReset(baseNow + 5 * 3600000, baseNow), '5 小时后')
  assert.equal(relativeReset(baseNow + 30 * 60000, baseNow), '30 分钟后')
  assert.equal(relativeReset(baseNow - 1000, baseNow), '已到期')
  assert.equal(relativeReset(null, baseNow), '')
  assert.equal(relativeReset(undefined, baseNow), '')
})

it('用量比例阈值着色准确划分主色、警告色与危险色', () => {
  assert.equal(usageTone(null), 'primary')
  assert.equal(usageTone(undefined), 'primary')
  assert.equal(usageTone(Number.NaN), 'primary')
  assert.equal(usageTone(0), 'primary')
  assert.equal(usageTone(50), 'primary')
  assert.equal(usageTone(79.9), 'primary')
  assert.equal(usageTone(80), 'warning')
  assert.equal(usageTone(85.5), 'warning')
  assert.equal(usageTone(94.9), 'warning')
  assert.equal(usageTone(95), 'danger')
  assert.equal(usageTone(99.9), 'danger')
  assert.equal(usageTone(100), 'danger')
  assert.equal(usageTone(120), 'danger')
})

it('日限重置卡状态文本与色调映射正确', () => {
  assert.equal(cardStatusText('available', true), '可用')
  assert.equal(cardStatusText('using', true), '使用中')
  assert.equal(cardStatusText('used', true), '已使用')
  assert.equal(cardStatusText('unknown', true), '结果未知')
  assert.equal(cardStatusText('available', false), '无卡')
  assert.equal(cardStatusText(null, true), '无卡')
  assert.equal(cardStatusText(undefined, false), '无卡')
  assert.equal(cardStatusText('other', true), '无卡')

  assert.equal(cardStatusTone('available', true), 'success')
  assert.equal(cardStatusTone('using', true), 'warning')
  assert.equal(cardStatusTone('used', true), 'neutral')
  assert.equal(cardStatusTone('unknown', true), 'danger')
  assert.equal(cardStatusTone('available', false), 'neutral')
  assert.equal(cardStatusTone(null, true), 'neutral')
})

it('周期起止时间格式化正确', () => {
  const cycle = {
    start_ms: Date.parse('2026-10-21T02:00:00Z'),
    end_ms: Date.parse('2026-10-28T02:00:00Z'),
  }
  assert.equal(formatCycleRange(cycle), '10/21 10:00 至 10/28 10:00')
  assert.equal(formatCycleRange(null), '暂无周期数据')
  assert.equal(formatCycleRange(undefined), '暂无周期数据')
  assert.equal(formatCycleRange({ start_ms: 100, end_ms: null as any }), '暂无周期数据')
})

it('成员页接口状态码对应中文提示', () => {
  assert.equal(formatMemberError(401), 'Key 无效')
  assert.equal(formatMemberError(404), '未加入拼车')
  assert.equal(formatMemberError(409, '今日用量为 0'), '今日用量为 0')
  assert.equal(formatMemberError(409), '不满足用卡条件')
  assert.equal(formatMemberError(429), '请求过多，请稍后重试')
  assert.equal(formatMemberError(503), '服务繁忙或不可用，请稍后重试')
  assert.equal(formatMemberError(500, '未知错误'), '未知错误')
  assert.equal(formatMemberError(500), '请求失败，请稍后重试')
})

it('同车占比排序降序且正确包含其他', () => {
  const items = [
    { label: 'Key A', me: false, percent: 15.5 },
    { label: 'Key B', me: true, percent: 45.2 },
  ]
  const sorted = sortShareItems(items, 39.3)
  assert.equal(sorted.length, 3)
  assert.equal(sorted[0].label, 'Key B')
  assert.equal(sorted[0].me, true)
  assert.equal(sorted[1].label, '其他')
  assert.equal(sorted[1].percent, 39.3)
  assert.equal(sorted[2].label, 'Key A')

  // other_percent 为 0 或 null 时不产生其他
  const sortedNoOther = sortShareItems(items, 0)
  assert.equal(sortedNoOther.length, 2)
  assert.equal(sortedNoOther[0].label, 'Key B')

  const emptySorted = sortShareItems(null, 10.0)
  assert.equal(emptySorted.length, 1)
  assert.equal(emptySorted[0].label, '其他')
})

it('计算占比正确处理边界条件', () => {
  assert.equal(calculateSharePercent('25', '100'), 25)
  assert.equal(calculateSharePercent('0', '100'), 0)
  assert.equal(calculateSharePercent('50', '0'), 0)
  assert.equal(calculateSharePercent('50', '-10'), 0)
  assert.equal(calculateSharePercent(null, '100'), null)
  assert.equal(calculateSharePercent('50', null), null)
  assert.equal(calculateSharePercent('120', '100'), 100)
})

it('成员页会话校验纯函数：模拟慢刷新 → 退出 → 响应返回', () => {
  // 1. 用户登录成功，会话标识递增为 1
  let currentSession = 1

  // 2. 发起慢刷新，捕获当时的会话标识
  const slowRefreshSession = currentSession

  // 3. 慢刷新在网络传输中，用户点击退出登录，会话标识递增且清空数据
  currentSession++

  // 4. 慢刷新响应返回，校验标识：不匹配，应当丢弃，不写回数据
  assert.equal(isSessionActive(slowRefreshSession, currentSession), false)

  // 5. 模拟新用户登录，会话标识再次递增为 3
  currentSession++
  assert.equal(isSessionActive(slowRefreshSession, currentSession), false)

  // 6. 新用户的正常请求返回，标识匹配，应当接收
  const newRequestSession = currentSession
  assert.equal(isSessionActive(newRequestSession, currentSession), true)
})

it('管理页占比缓存校验纯函数：周窗口结束时间相差不超过2分钟且Key存在', () => {
  const baseReset = Date.parse('2026-10-28T12:00:00Z')
  const validShare = {
    end_ms: baseReset,
    keys: { 'key-1': '25.00', 'key-2': '75.00' },
  }

  // 完全吻合
  assert.equal(isShareStatsValid(validShare, baseReset, 'key-1'), true)

  // 相差在 2 分钟以内（如 60 秒、刚好 120 秒）
  assert.equal(isShareStatsValid({ ...validShare, end_ms: baseReset + 60_000 }, baseReset, 'key-1'), true)
  assert.equal(isShareStatsValid({ ...validShare, end_ms: baseReset - 120_000 }, baseReset, 'key-1'), true)

  // 相差超过 2 分钟（如 120001 毫秒、跨周）
  assert.equal(isShareStatsValid({ ...validShare, end_ms: baseReset + 120_001 }, baseReset, 'key-1'), false)
  assert.equal(isShareStatsValid({ ...validShare, end_ms: baseReset - 7 * 86400_000 }, baseReset, 'key-1'), false)

  // Key 未出现在 keys 映射中
  assert.equal(isShareStatsValid(validShare, baseReset, 'key-not-found'), false)

  // 边界防御：空值、非数字、无 keys
  assert.equal(isShareStatsValid(null, baseReset, 'key-1'), false)
  assert.equal(isShareStatsValid(undefined, baseReset, 'key-1'), false)
  assert.equal(isShareStatsValid(validShare, null, 'key-1'), false)
  assert.equal(isShareStatsValid(validShare, baseReset, null), false)
  assert.equal(isShareStatsValid({ end_ms: baseReset, keys: null }, baseReset, 'key-1'), false)
})

it('卡邮件状态与未完成卡通知统计格式化正确', () => {
  assert.equal(cardMailStatusText('skipped'), '未发送（未启用或积压已满，见最近事件）')
  assert.equal(cardMailStatusText('queued'), '等待发送')
  assert.equal(cardMailStatusText('unknown'), '结果未知，可能已发送')
  assert.equal(cardMailStatusText('succeeded'), '成功')
  assert.equal(cardMailStatusText('failed'), '失败')
  assert.equal(cardMailStatusText(null), '')
  assert.equal(cardMailStatusText(undefined), '')

  const notices = [
    { status: 'queued' },
    { status: 'unknown' },
    { status: 'succeeded' },
    { status: 'unknown' },
  ]
  const counts = countCardNotices(notices)
  assert.equal(counts.queued, 1)
  assert.equal(counts.unknown, 2)
  assert.equal(formatCardNoticeSummary(counts.queued, counts.unknown), '等待发送 1 条 · 结果未知 2 条')

  const emptyCounts = countCardNotices(null)
  assert.equal(emptyCounts.queued, 0)
  assert.equal(emptyCounts.unknown, 0)
  assert.equal(formatCardNoticeSummary(0, 0), '等待发送 0 条 · 结果未知 0 条')
})
