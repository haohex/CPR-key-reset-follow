import assert from 'node:assert/strict'
import { it } from 'node:test'
import { DISPLAY_TIMEZONE, displayDate, money, percent, timezoneLabel } from '../src/display.ts'

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
