<script setup lang="ts">
import type { Link, Sample, SendStatus, SmtpSettings, Snapshot } from './api/modules/follow'
import { BaseButton, BaseCheckbox, BaseIconButton, BaseInput, BaseModal, BaseSelect, BaseSwitch, BaseTag } from '@codex-proxy/ui'
import { ArrowLeft, ChevronRight, Plus, RefreshCw, Settings } from '@lucide/vue'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { command, load } from './api/modules/follow'
import { DISPLAY_TIMEZONE, displayDate, money, percent, timezoneLabel } from './display'

const data = ref<Snapshot>()
const busy = ref(false)
const error = ref('')
const modal = ref('')
const selected = ref('')
const keyId = ref('')
const accountId = ref('')
const preview = ref<Sample>()
const previewError = ref('')
const previewBusy = ref(false)
const acknowledge = ref(false)
const manualAcknowledge = ref(false)
const settingsOpen = ref(false)
const draftEarly = ref(true)
const saved = ref(false)
const draftPort = ref('587')
const draftProbe = ref(false)
const clearSmtpPassword = ref(false)
const draftRecipient = ref('')
const draftSmtp = ref<SmtpSettings>({ enabled: false, server: '', port: 587, security: 'starttls', username: '', password: '', from: '' })
const sendLabel = (status: SendStatus) => ({ skipped: '未启用', queued: '等待发送', unknown: '结果未知，可能已发送', succeeded: '成功', failed: '失败' })[status]
const securityOptions = [{ value: 'starttls', label: 'STARTTLS' }, { value: 'tls', label: 'TLS' }]
const timezoneText = timezoneLabel(DISPLAY_TIMEZONE)
const modalOpen = computed({ get: () => !!modal.value, set: (value) => {
  if (!value)
    modal.value = ''
} })
const smtpReady = computed(() => !!data.value?.state.smtp.server.trim() && !!data.value?.state.smtp.port && !!data.value?.state.smtp.from.trim())
const rows = computed(() => Object.values(data.value?.state.links || {}))
const link = computed(() => data.value?.state.links[selected.value])
const account = (l: Link) => data.value?.accounts.find(a => a.account_id === l.account_id)
const accountName = (l: Link) => account(l)?.name?.trim() || account(l)?.email || '账号已删除'
const observation = (l: Link) => data.value?.state.accounts[l.account_id]
const sample = (l: Link) => observation(l)?.sample || l.baseline
const budget = (l: Link) => data.value?.budgets[l.key_id]
const keyName = (l: Link) => data.value?.keys.find(k => k.id === l.key_id)?.name || 'Key 已删除'
const date = (value: number | null | undefined, full = false) => displayDate(value, DISPLAY_TIMEZONE, full)
function status(l: Link): '监测中' | '等待确认' | '需处理' | '已暂停' {
  if (l.paused)
    return '已暂停'
  if (l.pending?.kind === 'unknown' || l.pending?.kind === 'ready' || (observation(l)?.failure_since != null && Date.now() - observation(l)!.failure_since! >= 3600000))
    return '需处理'
  return ['confirm', 'changed'].includes(l.pending?.kind || '') ? '等待确认' : '监测中'
}
const tone = (l: Link) => ({ 监测中: 'success', 等待确认: 'warning', 需处理: 'danger', 已暂停: 'neutral' } as const)[status(l)]
const needs = computed(() => rows.value.filter(l => ['等待确认', '需处理'].includes(status(l))))
const keys = computed(() => (data.value?.keys || []).map(k => ({ value: k.id, label: k.name, disabled: !!data.value?.state.links[k.id], description: data.value?.state.links[k.id] ? `已关联 ${account(data.value.state.links[k.id])?.name || '账号'}` : undefined })))
const accounts = computed(() => (data.value?.accounts || []).map(a => ({ value: a.account_id, label: a.name || a.email || a.account_id, description: (a.provider_id !== 'openai' || a.authentication_kind !== 'oauth') ? '仅支持 Codex 账号周窗口' : a.name !== a.email ? a.email || undefined : undefined, disabled: (a.provider_id !== 'openai' || a.authentication_kind !== 'oauth') })))
function openSettings() {
  clearSmtpPassword.value = false
  draftProbe.value = data.value!.state.probe_enabled || false
  draftSmtp.value = { ...data.value!.state.smtp, port: data.value!.state.smtp?.port || 587, password: '' }
  draftPort.value = String(draftSmtp.value.port)
  draftEarly.value = data.value!.state.early_auto
  error.value = ''
  saved.value = false
  settingsOpen.value = true
}
function closeSettings() {
  settingsOpen.value = false
  error.value = ''
  saved.value = false
}
async function saveSettings() {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  saved.value = false
  let committed = false
  try {
    await command({ action: 'settings', version: data.value?.version, enabled: draftEarly.value, probe_enabled: draftProbe.value, clear_smtp_password: clearSmtpPassword.value, smtp: { ...draftSmtp.value, port: Number(draftPort.value) } })
    committed = true
    data.value = await load()
    saved.value = true
  }
  catch (e) {
    const message = (e as Error).message
    // 保留草稿，更新状态版本，管理员再次保存时不必丢掉已填写的设置。
    try {
      data.value = await load()
    }
    catch { /* 保留原始错误供用户重试。 */ }
    error.value = committed ? `设置已保存，但页面刷新失败：${message}` : message
  }
  finally { busy.value = false }
}
watch([draftEarly, draftProbe, draftSmtp, draftPort, clearSmtpPassword], () => {
  saved.value = false
}, { deep: true })
async function refresh() {
  try {
    data.value = await load()
    error.value = ''
  }
  catch (e) { error.value = (e as Error).message }
}
function open(kind: string, l?: Link) {
  selected.value = l?.key_id || ''
  draftRecipient.value = l?.recipient || ''
  modal.value = kind
  acknowledge.value = false
  error.value = ''
  if (kind === 'add') {
    keyId.value = ''
    accountId.value = ''
    preview.value = undefined
    previewError.value = ''
  }
}
async function act(action: string, extra: object = {}, close = true) {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await command({ action, key_id: selected.value, version: data.value?.version, acknowledge: acknowledge.value, ...extra })
    if (close)
      modal.value = ''
    await refresh()
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
function confirmManual(probe: boolean) {
  manualAcknowledge.value = false
  error.value = ''
  modal.value = probe ? 'manual_probe' : 'manual_mail'
}
async function sendManual() {
  if (busy.value || !manualAcknowledge.value)
    return
  busy.value = true
  error.value = ''
  let failure = ''
  try {
    await command({ action: modal.value, key_id: selected.value, version: data.value?.version, acknowledge: manualAcknowledge.value })
  }
  catch (e) { failure = (e as Error).message }
  try {
    data.value = await load()
  }
  catch (e) { failure = `${failure ? `${failure}；` : ''}无法刷新测试记录：${(e as Error).message}，请刷新查看持久结果，勿直接重复发送` }
  error.value = failure
  modal.value = 'detail'
  manualAcknowledge.value = false
  busy.value = false
}
let previewGeneration = 0
watch(accountId, async (id) => {
  const generation = ++previewGeneration
  preview.value = undefined
  previewError.value = ''
  if (!id)
    return
  previewBusy.value = true
  try {
    const result = await command({ action: 'preview', account_id: id })
    if (generation === previewGeneration)
      preview.value = result.sample
  }
  catch (e) {
    if (generation === previewGeneration)
      previewError.value = (e as Error).message
  }
  finally {
    if (generation === previewGeneration)
      previewBusy.value = false
  }
})
let timer: ReturnType<typeof setInterval>
onMounted(() => {
  refresh()
  timer = setInterval(() => {
    if (!document.hidden && !modal.value && !busy.value && !settingsOpen.value)
      refresh()
  }, 30000)
})
onUnmounted(() => clearInterval(timer))
</script>

<template>
  <main class="panel">
    <template v-if="settingsOpen">
      <div class="settings-heading">
        <BaseButton :disabled="busy" @click="closeSettings">
          <ArrowLeft :size="16" />返回关联列表
        </BaseButton>
        <h2>插件设置</h2>
        <p class="muted">
          设置对所有管理员生效
        </p>
      </div>
      <form class="settings-form" @submit.prevent="saveSettings">
        <section class="setting-row">
          <div>
            <h3>提前重置时自动重置额度</h3>
            <p>上游提前重置或者使用重置卡，识别后自动重置额度</p>
            <p class="muted">
              关闭后需要手动确认；正常换周仍自动处理
            </p>
          </div>
          <BaseSwitch v-model="draftEarly" label="提前重置时自动重置额度" :disabled="busy" />
        </section>
        <section class="setting-row">
          <div>
            <h3>重置后发送测试消息</h3><p class="muted">
              使用关联 Key 和账号向 gpt-6.1-sol 发送 hello，沿用默认思考强度，正常计费
            </p>
          </div>
          <BaseSwitch v-model="draftProbe" label="重置后发送测试消息" :disabled="busy" />
        </section>
        <section>
          <h3>SMTP 邮件提醒</h3>
          <BaseSwitch v-model="draftSmtp.enabled" show-label label="启用邮件提醒" :disabled="busy" />
          <p class="muted">
            收件邮箱在每个关联详情中设置；密码留空保留已保存值
          </p>
          <label class="field">服务器<BaseInput v-model="draftSmtp.server" :disabled="busy" /></label>
          <label class="field">端口<BaseInput v-model="draftPort" type="number" :disabled="busy" /></label>
          <label class="field">连接安全<BaseSelect v-model="draftSmtp.security" :options="securityOptions" :disabled="busy" /></label>
          <label class="field">用户名<BaseInput v-model="draftSmtp.username" :disabled="busy" autocomplete="off" /></label>
          <label class="field">密码<BaseInput v-model="draftSmtp.password" type="password" :disabled="busy" autocomplete="new-password" /></label>
          <p class="muted">
            SMTP 密码：{{ data?.smtp_password_configured ? '已设置' : '未设置' }}
          </p>
          <BaseCheckbox v-model="clearSmtpPassword" show-label label="清除已保存的 SMTP 密码" :disabled="busy" />
          <label class="field">发件地址<BaseInput v-model="draftSmtp.from" :disabled="busy" /></label>
        </section>
        <p v-if="error" role="alert" class="error">
          {{ error }}
        </p>
        <p class="muted">
          保存设置后，到关联详情发送测试消息或测试邮件
        </p>
        <div class="settings-actions">
          <span v-if="saved" role="status" class="saved">设置已保存</span>
          <BaseButton :disabled="busy" @click="closeSettings">
            取消
          </BaseButton>
          <BaseButton variant="primary" :loading="busy" @click="saveSettings">
            保存设置
          </BaseButton>
        </div>
      </form>
    </template>
    <template v-else>
      <p v-if="error && !modal" role="alert" class="error">
        {{ error }} <BaseButton size="sm" @click="refresh">
          重试
        </BaseButton>
      </p>
      <div class="toolbar">
        <div><span class="count">{{ rows.length }} 个关联</span><small class="timezone-caption">时间：{{ timezoneText }}</small></div>
        <div class="tools toolbar-actions">
          <BaseIconButton label="刷新列表" title="刷新列表，不检查账号额度" :disabled="busy" @click="refresh">
            <RefreshCw :size="17" />
          </BaseIconButton>
          <BaseButton :disabled="busy || !data" @click="openSettings">
            <Settings :size="16" />设置
          </BaseButton>
          <BaseButton variant="primary" :disabled="busy || !data" @click="open('add')">
            <Plus :size="16" />添加关联
          </BaseButton>
        </div>
      </div>
      <div v-if="needs.length" class="banner">
        <span>{{ needs.length }} 个关联需要处理</span><BaseButton size="sm" @click="open('process', needs[0])">
          查看并处理
        </BaseButton>
      </div>
      <p v-if="!data && !error" class="empty" role="status">
        正在加载关联
      </p>
      <div v-else-if="data && !rows.length" class="empty">
        <p>尚未建立关联</p><small>选择 Key 和对应账号，开始跟随周额度重置</small><BaseButton variant="primary" @click="open('add')">
          <Plus :size="16" />添加关联
        </BaseButton>
      </div>
      <table v-else-if="data">
        <thead><tr><th>Key / 关联账号</th><th>本周用量</th><th>账号使用情况</th><th>账号预计重置</th><th>状态</th><th><span class="sr-only">操作</span></th></tr></thead>
        <tbody>
          <tr v-for="l in rows" :key="l.key_id">
            <td class="identity">
              <strong>{{ keyName(l) }}</strong><small>{{ accountName(l) }}</small>
            </td>
            <td data-label="本周用量" class="numeric">
              <span v-if="budget(l)"><span class="amount">{{ money(budget(l)!.weekly_used_usd) }}</span><span class="limit"> / {{ Number(budget(l)!.weekly_limit_usd) === 0 ? '不限额' : money(budget(l)!.weekly_limit_usd) }}</span></span><span v-else class="muted">无法读取预算</span>
            </td>
            <td data-label="账号使用情况" class="numeric">
              {{ percent(sample(l).used) }}
            </td>
            <td data-label="账号预计重置" class="reset-time">
              <span :title="date(sample(l).reset, true)">{{ date(sample(l).reset) }}</span><small v-if="observation(l)?.error">最近检查失败</small><small v-else-if="sample(l).reset <= Date.now()">等待账号更新周期</small>
            </td>
            <td data-label="状态">
              <div class="status">
                <BaseTag :type="tone(l)" size="sm">
                  {{ status(l) }}
                </BaseTag><BaseButton v-if="['等待确认', '需处理'].includes(status(l))" size="sm" :disabled="busy" @click="open('process', l)">
                  处理
                </BaseButton>
              </div>
            </td>
            <td class="row-actions">
              <div class="tools">
                <BaseIconButton label="立即检查" title="立即检查账号额度" :disabled="busy || l.paused" @click="selected = l.key_id; act('check')">
                  <RefreshCw :size="16" />
                </BaseIconButton><BaseIconButton label="查看详情" title="查看详情" :disabled="busy" @click="open('detail', l)">
                  <ChevronRight :size="17" />
                </BaseIconButton>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </template>
  </main>
  <BaseModal v-model="modalOpen" :title="({ add: '添加关联', detail: '关联详情', process: '处理关联', delete: '删除关联', manual_probe: '发送测试消息', manual_mail: '发送测试邮件' })[modal] || '关联'" :dismissible="!busy" :draggable="false">
    <p v-if="error" role="alert" class="error">
      {{ error }}
    </p>
    <template v-if="modal === 'add'">
      <label class="field">Key<BaseSelect v-model="keyId" :options="keys" filterable placeholder="选择现有 Key" :disabled="busy" /></label>
      <p v-if="keyId && data?.budgets[keyId] && Number(data.budgets[keyId].weekly_limit_usd) === 0 && Number(data.budgets[keyId].daily_limit_usd) === 0" class="muted">
        该 Key 未设日限额和周限额，清零无实际作用
      </p>
      <label class="field">额度来源账号<BaseSelect v-model="accountId" :options="accounts" filterable placeholder="选择账号名称或邮箱" :disabled="busy" /></label>
      <p v-if="previewBusy" role="status">
        正在识别周窗口
      </p><p v-else-if="previewError" class="error">
        {{ previewError }}
      </p><p v-else-if="preview">
        已识别 Codex 周窗口，上游预计重置 {{ date(preview.reset) }}
      </p>
    </template>
    <template v-else-if="link && modal === 'detail'">
      <h3>{{ keyName(link) }}</h3><h4>关联账号</h4><p>{{ accountName(link) }}</p><p v-if="account(link)?.email && account(link)?.email !== accountName(link)" class="muted">
        {{ account(link)?.email }}
      </p>
      <dl><dt>账号使用情况</dt><dd>{{ percent(sample(link).used) }}</dd><dt>账号预计重置</dt><dd>{{ date(sample(link).reset, true) }}</dd></dl>
      <h4>Key 本周用量</h4><p v-if="budget(link)">
        {{ money(budget(link)!.weekly_used_usd) }} / {{ Number(budget(link)!.weekly_limit_usd) === 0 ? '不限额' : money(budget(link)!.weekly_limit_usd) }}
      </p><p v-else>
        无法读取预算
      </p>
      <h4>Key 今日用量</h4><p v-if="budget(link)">
        {{ money(budget(link)!.daily_used_usd) }} / {{ Number(budget(link)!.daily_limit_usd) === 0 ? '不限额' : money(budget(link)!.daily_limit_usd) }}
      </p>
      <dl><dt>Key 自身的周用量重置时间</dt><dd>{{ budget(link)?.weekly_resets_at_ms ? date(budget(link)!.weekly_resets_at_ms, true) : '尚未开启或已到期' }}</dd></dl><p class="muted">
        由宿主独立计算，可能早于账号重置；这不是 Key 失效时间
      </p>
      <h4>手动测试</h4>
      <p class="muted">
        立即使用已保存的配置发送，不清零额度；自动开关关闭或关联暂停时也可测试
      </p>
      <div class="tools">
        <BaseButton :disabled="busy" @click="confirmManual(true)">
          发送测试消息
        </BaseButton>
        <BaseButton :disabled="busy || !smtpReady || !link.recipient || draftRecipient.trim() !== link.recipient" @click="confirmManual(false)">
          发送测试邮件
        </BaseButton>
      </div>
      <p v-if="!smtpReady" class="muted">
        请先在设置中保存 SMTP 服务器、端口和发件地址
      </p>
      <p v-if="draftRecipient.trim() !== link.recipient" class="muted">
        收件邮箱已修改，请先保存邮箱再测试邮件
      </p>
      <p v-else-if="!link.recipient" class="muted">
        请先保存收件邮箱和 SMTP 设置再测试邮件
      </p>
      <p v-if="link.last_manual_probe" role="status">
        最近手动消息：{{ date(link.last_manual_probe.at) }} · {{ link.last_manual_probe.status === 'succeeded' ? '已确认成功' : '未确认成功，可能已发送，不自动重试' }}
      </p>
      <p v-if="link.last_manual_mail" role="status">
        最近手动邮件：{{ date(link.last_manual_mail.at) }} · {{ link.last_manual_mail.recipient }} · {{ link.last_manual_mail.status === 'succeeded' ? '已确认成功' : '未确认成功，可能已发送，不自动重试' }}
      </p>
      <h4>重置后通知</h4>
      <label class="field">收件邮箱<BaseInput v-model="draftRecipient" type="email" :disabled="busy" /></label>
      <BaseButton :disabled="busy" @click="act('recipient', { recipient: draftRecipient }, false)">
        保存邮箱
      </BaseButton>
      <p class="muted">
        只影响后续重置；已排队邮件使用重置时的收件地址
      </p>
      <div v-for="job in [...(link.deliveries || [])].reverse()" :key="job.id">
        <p>{{ date(job.reset_at) }} · 测试：{{ sendLabel(job.probe) }} · 邮件：{{ sendLabel(job.mail) }}</p>
        <template v-if="['unknown', 'failed'].includes(job.probe) || ['unknown', 'failed'].includes(job.mail)">
          <BaseCheckbox v-model="acknowledge" show-label label="我了解重试可能产生重复请求或重复邮件" />
          <BaseButton v-if="['unknown', 'failed'].includes(job.probe)" :disabled="busy || !acknowledge" @click="act('retry_probe', { delivery_id: job.id }, false)">
            重试测试
          </BaseButton>
          <BaseButton v-if="['unknown', 'failed'].includes(job.mail)" :disabled="busy || !acknowledge" @click="act('retry_mail', { delivery_id: job.id }, false)">
            重试邮件
          </BaseButton>
        </template>
      </div>
      <h4>最近事件</h4><p v-for="(event, index) in [...link.events].reverse()" :key="index" class="muted">
        {{ date(event.at) }} · {{ event.reason }}
      </p>
      <details>
        <summary>判定与诊断</summary><p>窗口 codex:604800s · 604800 秒</p><p>已确认周期结束 {{ date(link.baseline.reset, true) }}</p><p>最近样本 {{ date(sample(link).observed, true) }}</p><p v-if="link.pending">
          待处理原因 {{ link.pending.reason }}
        </p><p v-if="link.pending?.before">
          重置前日用量 ${{ link.pending.before.daily_used_usd }} · 周用量 ${{ link.pending.before.weekly_used_usd }}
        </p><p v-if="budget(link)">
          完整金额：${{ budget(link)!.weekly_used_usd }} / {{ Number(budget(link)!.weekly_limit_usd) === 0 ? '不限额' : `$${budget(link)!.weekly_limit_usd}` }}
        </p><p>{{ link.pending?.error || observation(link)?.error }}</p>
      </details>
      <h4>操作</h4><div class="tools">
        <BaseButton :disabled="busy" @click="act(link.paused ? 'resume' : 'pause')">
          {{ link.paused ? '恢复并建立新基线' : '暂停关联' }}
        </BaseButton><BaseButton variant="destructive" @click="modal = 'delete'; acknowledge = false">
          删除关联
        </BaseButton>
      </div>
    </template>
    <template v-else-if="link && ['manual_probe', 'manual_mail'].includes(modal)">
      <p v-if="modal === 'manual_probe'">
        将使用 {{ keyName(link) }} 和关联账号向 gpt-6.1-sol 发送 hello，思考强度沿用默认，正常计费
      </p>
      <p v-else>
        将使用已保存的 SMTP 配置向 {{ link.recipient }} 发送一封 SMTP测试邮件
      </p>
      <p class="muted">
        本操作不清零额度；结果未知时不会自动重试，再次发送可能重复计费或投递
      </p>
      <BaseCheckbox v-model="manualAcknowledge" show-label label="确认立即发送，并了解重复发送的风险" :disabled="busy" />
    </template>
    <template v-else-if="link && modal === 'process'">
      <h3>{{ keyName(link) }}</h3><p class="muted">
        {{ accountName(link) }}
      </p>
      <template v-if="link.pending?.kind === 'unknown'">
        <p>重置结果未知，已停止自动重试</p><p class="muted">
          保留现有用量并跳过会重新建立基线
        </p><BaseCheckbox v-model="acknowledge" show-label label="我了解可能清掉期间新产生的消费" />
      </template>
      <template v-else-if="link.pending?.kind === 'changed'">
        <p>目标窗口标识或时长发生变化，已停止自动清零</p>
        <p v-for="window in link.pending.windows" :key="window.key">
          {{ window.key }} · {{ window.window_seconds }} 秒 · {{ date(window.reset_at_ms, true) }}
        </p>
        <p class="muted">
          请在宿主检查账号套餐，恢复唯一 Codex 周窗口后再确认或更新基线
        </p>
      </template>
      <template v-else-if="link.pending?.kind === 'confirm'">
        <p>{{ link.pending.reason === 'early' ? '账号提前进入新周期，自动清零开关已关闭' : '周期边界变化无法解释，需要你确认' }}</p><dl><dt>原边界</dt><dd>{{ date(link.baseline.reset, true) }}</dd><dt>新边界</dt><dd>{{ date(link.pending.sample.reset, true) }}</dd><dt>推算的新窗口起点</dt><dd>{{ date(link.pending.sample.reset - 604800000, true) }}</dd></dl><p class="muted">
          确认后会同时清零当前日用量和周用量，此操作无法撤销
        </p>
      </template>
      <p v-else>
        {{ observation(link)?.error || '读取预算失败，尚未调用清零' }}
      </p>
    </template>
    <template v-else-if="modal === 'delete'">
      <p>仅删除本插件的关联记录，不撤销已执行的重置</p><BaseCheckbox v-model="acknowledge" show-label label="确认删除此关联" />
    </template>
    <template #footer>
      <span v-if="modal === 'add'" class="muted">仅建立基线，不清零</span><BaseButton :disabled="busy" @click="modal = ''">
        {{ modal === 'detail' ? '关闭' : '取消' }}
      </BaseButton>
      <BaseButton v-if="['manual_probe', 'manual_mail'].includes(modal)" variant="primary" :loading="busy" :disabled="!manualAcknowledge || busy" @click="sendManual">
        确认发送
      </BaseButton>
      <BaseButton v-if="modal === 'add'" variant="primary" :loading="busy" :disabled="!keyId || !preview || previewBusy" @click="act('add', { key_id: keyId, account_id: accountId })">
        添加
      </BaseButton>
      <template v-if="modal === 'process' && link?.pending">
        <BaseButton :disabled="busy" @click="act('skip')">
          {{ link.pending.kind === 'unknown' ? '保留现有用量并跳过' : '忽略并更新基线' }}
        </BaseButton>
        <BaseButton v-if="link.pending.kind === 'unknown'" variant="destructive" :disabled="!acknowledge || busy" @click="act('retry')">
          再清零一次
        </BaseButton>
        <BaseButton v-if="['confirm', 'changed'].includes(link.pending.kind)" variant="primary" :loading="busy" @click="act('confirm')">
          确认并清零
        </BaseButton>
      </template>
      <BaseButton v-if="modal === 'process' && !link?.pending" :loading="busy" @click="act('check')">
        立即检查
      </BaseButton>
      <BaseButton v-if="modal === 'delete'" variant="destructive" :disabled="!acknowledge || busy" @click="act('delete')">
        删除关联
      </BaseButton>
    </template>
  </BaseModal>
</template>
