<script setup lang="ts">
import type { Link, Sample, SendStatus, SmtpSettings, Snapshot } from './api/modules/follow'
import {
  ZButton,
  ZCard,
  ZCheckbox,
  ZDialog,
  ZEmpty,
  ZForm,
  ZFormItem,
  ZIconButton,
  ZInput,
  ZPageHeader,
  ZSegmented,
  ZSelect,
  ZSwitch,
  ZTag,
} from '@codex-proxy/ui'
import { AlertCircle, ArrowLeft, ChevronRight, Plus, RefreshCw, Settings } from '@lucide/vue'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { command, load } from './api/modules/follow'
import {
  calculateSharePercent,
  cardMailStatusText,
  cardStatusText,
  cardStatusTone,
  countCardNotices,
  DISPLAY_TIMEZONE,
  displayDate,
  formatCardNoticeSummary,
  formatCredits,
  formatResetCardsAvailable,
  formatResetCardsFailedHint,
  formatResetCardStatus,
  formatResetCardTitle,
  formatResetCardType,
  isShareStatsValid,
  money,
  percent,
  relativeReset,
  resetCardStatusTone,
  sortResetCards,
  tagType,
  timezoneLabel,
  usageRatio,
  usageTone,
} from './display'

type DialogType = 'default' | 'info' | 'warning' | 'danger' | 'success'

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
const cardRefundAcknowledge = ref(false)
const cardNoticeAcknowledge = ref(false)
const copiedCarpool = ref(false)
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

const detailTab = ref('overview')
const detailTabs = [
  { label: '概览', value: 'overview' },
  { label: '通知与测试', value: 'notify_test' },
  { label: '记录与诊断', value: 'audit_diag' },
]

const modalOpen = computed({
  get: () => !!modal.value,
  set: (value) => {
    if (!value)
      modal.value = ''
  },
})

const rows = computed(() => Object.values(data.value?.state.links || {}))
const link = computed(() => data.value?.state.links[selected.value])

const modalTitle = computed(() => {
  const map: Record<string, string> = {
    add: '添加关联',
    detail: '关联详情',
    process: '处理关联',
    delete: '删除关联',
    manual_probe: '发送测试消息',
    manual_mail: '发送测试邮件',
  }
  return map[modal.value] || '关联'
})

const modalWidth = computed(() => {
  if (modal.value === 'detail')
    return '48rem'
  if (modal.value === 'delete')
    return '28rem'
  return '36rem'
})

const modalType = computed<DialogType>(() => {
  if (modal.value === 'delete')
    return 'danger'
  if (modal.value === 'process') {
    const k = link.value?.pending?.kind
    if (k === 'confirm' || k === 'changed')
      return 'warning'
    if (k === 'unknown')
      return 'danger'
    return 'default'
  }
  return 'default'
})

const smtpReady = computed(() => !!data.value?.state.smtp.server.trim() && !!data.value?.state.smtp.port && !!data.value?.state.smtp.from.trim())
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
const hasRetryableDeliveries = computed(() => (link.value?.deliveries || []).some(j => ['unknown', 'failed'].includes(j.probe) || ['unknown', 'failed'].includes(j.mail)))

const hasActiveCard = computed(() => !!link.value?.card && link.value?.cycle_seq != null && link.value.card.seq === link.value.cycle_seq)
const currentCard = computed(() => (hasActiveCard.value ? link.value?.card : null))
const cardStatus = computed(() => cardStatusText(currentCard.value?.status, hasActiveCard.value))
const cardTone = computed(() => cardStatusTone(currentCard.value?.status, hasActiveCard.value))

async function toggleCard(enabled: boolean) {
  if (busy.value || !link.value)
    return
  busy.value = true
  error.value = ''
  try {
    await command({
      action: 'card_toggle',
      key_id: link.value.key_id,
      version: data.value?.version,
      card_enabled: enabled,
    })
    await refresh()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function refundCard() {
  if (busy.value || !cardRefundAcknowledge.value || !link.value)
    return
  busy.value = true
  error.value = ''
  try {
    await command({
      action: 'card_refund',
      key_id: link.value.key_id,
      version: data.value?.version,
      acknowledge: cardRefundAcknowledge.value,
    })
    cardRefundAcknowledge.value = false
    await refresh()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

const cardNotices = computed(() => link.value?.card_notices || [])
const cardNoticeCounts = computed(() => countCardNotices(cardNotices.value))
const queuedCardNoticesCount = computed(() => cardNoticeCounts.value.queued)
const unknownCardNoticesCount = computed(() => cardNoticeCounts.value.unknown)

async function clearUnknownCardNotices() {
  if (busy.value || !cardNoticeAcknowledge.value || !link.value)
    return
  busy.value = true
  error.value = ''
  try {
    await command({
      action: 'card_notice_clear',
      key_id: link.value.key_id,
      version: data.value?.version,
      acknowledge: cardNoticeAcknowledge.value,
    })
    cardNoticeAcknowledge.value = false
    await refresh()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function copyCarpoolUrl() {
  const url = typeof window !== 'undefined' && window.location?.origin
    ? `${window.location.origin}/carpool/`
    : '/carpool/'
  try {
    await navigator.clipboard.writeText(url)
    copiedCarpool.value = true
    setTimeout(() => {
      copiedCarpool.value = false
    }, 2000)
  }
  catch {
    // 降级处理
  }
}

const currentShare = computed(() => (link.value ? data.value?.shares?.[link.value.account_id] : undefined))
const keyShareCost = computed(() => {
  if (!link.value || !currentShare.value)
    return '0'
  return currentShare.value.keys?.[link.value.key_id] ?? '0'
})
const keySharePercent = computed(() => {
  if (!currentShare.value)
    return null
  return calculateSharePercent(keyShareCost.value, currentShare.value.account_cost)
})
const isShareValid = computed(() => {
  if (!link.value)
    return false
  return isShareStatsValid(currentShare.value, sample(link.value)?.reset, link.value.key_id)
})
const currentAccountInfo = computed(() => (link.value ? data.value?.account_info?.[link.value.account_id] : undefined))

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
  cardRefundAcknowledge.value = false
  cardNoticeAcknowledge.value = false
  copiedCarpool.value = false
  error.value = ''
  if (kind === 'detail')
    detailTab.value = 'overview'
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
      <div class="settings-view">
        <ZPageHeader title="插件设置" description="设置对所有管理员生效">
          <template #actions>
            <ZButton :disabled="busy" @click="closeSettings">
              <ArrowLeft :size="16" />返回关联列表
            </ZButton>
          </template>
        </ZPageHeader>

        <form class="settings-form" @submit.prevent="saveSettings">
          <ZCard title="自动重置" description="换周与提前重置的处理方式，以及重置后的测试消息" class="mb-5">
            <div class="setting-item">
              <div class="setting-item-text">
                <h4>提前重置时自动重置额度</h4>
                <p>上游提前重置或者使用重置卡，识别后自动重置额度</p>
                <p class="muted">
                  关闭后需要手动确认；正常换周仍自动处理
                </p>
              </div>
              <ZSwitch v-model="draftEarly" label="提前重置时自动重置额度" :disabled="busy" />
            </div>
            <div class="setting-item-divider" />
            <div class="setting-item">
              <div class="setting-item-text">
                <h4>重置后发送测试消息</h4>
                <p class="muted">
                  使用关联 Key 和账号向 gpt-6.1-sol 发送 hello，沿用默认思考强度，正常计费
                </p>
              </div>
              <ZSwitch v-model="draftProbe" label="重置后发送测试消息" :disabled="busy" />
            </div>
          </ZCard>

          <ZCard title="SMTP 邮件提醒" description="配置全局邮件服务，发信通知将经由此服务发出" class="mb-5">
            <div class="setting-item">
              <div class="setting-item-text">
                <h4>启用邮件提醒</h4>
                <p class="muted">
                  收件邮箱在每个关联详情中设置；密码留空保留已保存值
                </p>
              </div>
              <ZSwitch v-model="draftSmtp.enabled" label="启用邮件提醒" :disabled="busy" />
            </div>

            <div class="smtp-grid">
              <ZFormItem label="服务器">
                <ZInput v-model="draftSmtp.server" :disabled="busy" />
              </ZFormItem>
              <ZFormItem label="端口">
                <ZInput v-model="draftPort" type="number" :disabled="busy" />
              </ZFormItem>
              <ZFormItem label="连接安全">
                <ZSelect v-model="draftSmtp.security" :options="securityOptions" :disabled="busy" class="smtp-select" />
              </ZFormItem>
              <ZFormItem label="用户名">
                <ZInput v-model="draftSmtp.username" :disabled="busy" autocomplete="off" />
              </ZFormItem>
              <ZFormItem label="密码">
                <ZInput v-model="draftSmtp.password" type="password" :disabled="busy" autocomplete="new-password" />
                <div class="password-meta">
                  <span class="muted">SMTP 密码：{{ data?.smtp_password_configured ? '已设置' : '未设置' }}</span>
                  <ZCheckbox v-model="clearSmtpPassword" label="清除已保存的 SMTP 密码" :disabled="busy" />
                </div>
              </ZFormItem>
              <ZFormItem label="发件地址">
                <ZInput v-model="draftSmtp.from" :disabled="busy" />
              </ZFormItem>
            </div>
            <p class="muted mt-3">
              保存设置后，到关联详情发送测试消息或测试邮件
            </p>
          </ZCard>

          <p v-if="error" role="alert" class="error">
            {{ error }}
          </p>

          <div class="settings-actions">
            <span v-if="saved" role="status" class="saved">设置已保存</span>
            <ZButton :disabled="busy" @click="closeSettings">
              取消
            </ZButton>
            <ZButton type="primary" :loading="busy" @click="saveSettings">
              保存设置
            </ZButton>
          </div>
        </form>
      </div>
    </template>

    <template v-else>
      <p v-if="error && !modal" role="alert" class="error">
        {{ error }} <ZButton size="small" @click="refresh">
          重试
        </ZButton>
      </p>

      <ZPageHeader title="拼车一体化管理">
        <template #description>
          <span class="header-meta"><span class="count">{{ rows.length }} 个关联</span> · <small class="timezone-caption">时间：{{ timezoneText }}</small></span>
        </template>
        <template #actions>
          <ZIconButton aria-label="刷新列表" title="刷新列表，不检查账号额度" :disabled="busy" @click="refresh">
            <RefreshCw :size="17" />
          </ZIconButton>
          <ZButton :disabled="busy || !data" @click="openSettings">
            <Settings :size="16" />设置
          </ZButton>
          <ZButton type="primary" :disabled="busy || !data" @click="open('add')">
            <Plus :size="16" />添加关联
          </ZButton>
        </template>
      </ZPageHeader>

      <div v-if="needs.length" class="banner" role="status">
        <div class="banner-title">
          <AlertCircle :size="18" class="banner-icon" />
          <span>{{ needs.length }} 个关联需要处理</span>
        </div>
        <ZButton size="small" @click="open('process', needs[0])">
          查看并处理
        </ZButton>
      </div>

      <p v-if="!data && !error" class="empty" role="status">
        正在加载关联
      </p>

      <ZEmpty
        v-else-if="data && !rows.length"
        title="尚未建立关联"
        description="选择 Key 和对应账号，开始跟随周额度重置"
        surface="subtle"
      >
        <template #action>
          <ZButton type="primary" :disabled="busy || !data" @click="open('add')">
            <Plus :size="16" />添加关联
          </ZButton>
        </template>
      </ZEmpty>

      <div v-else-if="data" class="table-container">
        <table class="data-table">
          <thead>
            <tr>
              <th>Key / 关联账号</th>
              <th>Key 用量</th>
              <th>账号使用情况</th>
              <th>账号预计重置</th>
              <th>状态</th>
              <th class="text-right">
                <span class="sr-only">操作</span>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="l in rows"
              :key="l.key_id"
              :class="{ 'row--needs': ['等待确认', '需处理'].includes(status(l)) }"
            >
              <td class="cell-identity">
                <div class="identity-wrapper">
                  <strong>{{ keyName(l) }}</strong>
                  <small>{{ accountName(l) }}</small>
                </div>
              </td>
              <td data-label="Key 用量" class="numeric cell-weekly">
                <div v-if="budget(l)" class="usage-stack">
                  <div class="usage-primary">
                    <span class="amount">{{ money(budget(l)!.weekly_used_usd) }}</span>
                    <span class="limit"> / {{ Number(budget(l)!.weekly_limit_usd) === 0 ? '不限额' : money(budget(l)!.weekly_limit_usd) }}</span>
                  </div>
                  <div
                    v-if="usageRatio(budget(l)!.weekly_used_usd, budget(l)!.weekly_limit_usd) !== null"
                    class="progress-track"
                  >
                    <div
                      class="progress-fill"
                      :class="`progress--${usageTone(usageRatio(budget(l)!.weekly_used_usd, budget(l)!.weekly_limit_usd))}`"
                      :style="{ width: `${usageRatio(budget(l)!.weekly_used_usd, budget(l)!.weekly_limit_usd)}%` }"
                    />
                  </div>
                  <div class="usage-secondary">
                    今日 {{ money(budget(l)!.daily_used_usd) }} / {{ Number(budget(l)!.daily_limit_usd) === 0 ? '不限额' : money(budget(l)!.daily_limit_usd) }}
                  </div>
                </div>
                <span v-else class="muted">无法读取预算</span>
              </td>
              <td data-label="账号使用情况" class="numeric cell-account">
                <div class="usage-stack">
                  <div class="usage-primary">
                    {{ percent(sample(l).used) }}
                  </div>
                  <div
                    v-if="sample(l).used != null"
                    class="progress-track"
                  >
                    <div
                      class="progress-fill"
                      :class="`progress--${usageTone(sample(l).used)}`"
                      :style="{ width: `${Math.min(100, Math.max(0, sample(l).used!))}%` }"
                    />
                  </div>
                </div>
              </td>
              <td data-label="账号预计重置" class="reset-time cell-reset">
                <div class="reset-stack">
                  <div class="reset-main">
                    <span :title="date(sample(l).reset, true)">{{ date(sample(l).reset) }}</span>
                    <span v-if="relativeReset(sample(l).reset)" class="relative-time">
                      ({{ relativeReset(sample(l).reset) }})
                    </span>
                  </div>
                  <small v-if="observation(l)?.error" class="error-hint">最近检查失败</small>
                  <small v-else-if="sample(l).reset <= Date.now()" class="waiting-hint">等待账号更新周期</small>
                </div>
              </td>
              <td data-label="状态" class="cell-status">
                <div class="status-cell">
                  <ZTag :type="tagType(tone(l))" size="small">
                    {{ status(l) }}
                  </ZTag>
                  <ZButton
                    v-if="['等待确认', '需处理'].includes(status(l))"
                    size="small"
                    :disabled="busy"
                    @click="open('process', l)"
                  >
                    处理
                  </ZButton>
                </div>
              </td>
              <td class="row-actions">
                <div class="tools">
                  <ZIconButton
                    aria-label="立即检查"
                    title="立即检查账号额度"
                    :disabled="busy || l.paused"
                    @click="selected = l.key_id; act('check')"
                  >
                    <RefreshCw :size="16" />
                  </ZIconButton>
                  <ZIconButton
                    aria-label="查看详情"
                    title="查看详情"
                    :disabled="busy"
                    @click="open('detail', l)"
                  >
                    <ChevronRight :size="17" />
                  </ZIconButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </main>

  <ZDialog
    v-model="modalOpen"
    :title="modalTitle"
    :width="modalWidth"
    :type="modalType"
    :show-close="!busy"
    :close-on-click-modal="!busy"
    :close-on-press-escape="!busy"
    :draggable="false"
  >
    <p v-if="error" role="alert" class="error">
      {{ error }}
    </p>

    <!-- 添加弹窗 -->
    <template v-if="modal === 'add'">
      <ZForm>
        <ZFormItem
          label="Key"
          :description="keyId && data?.budgets[keyId] && Number(data.budgets[keyId].weekly_limit_usd) === 0 && Number(data.budgets[keyId].daily_limit_usd) === 0 ? '该 Key 未设日限额和周限额，清零无实际作用' : undefined"
        >
          <ZSelect v-model="keyId" :options="keys" filterable placeholder="选择现有 Key" :disabled="busy" />
        </ZFormItem>
        <ZFormItem label="额度来源账号">
          <ZSelect v-model="accountId" :options="accounts" filterable placeholder="选择账号名称或邮箱" :disabled="busy" />
        </ZFormItem>
      </ZForm>
      <div v-if="previewBusy || previewError || preview" class="preview-box mt-3">
        <p v-if="previewBusy" role="status" class="muted">
          正在识别周窗口
        </p>
        <p v-else-if="previewError" role="alert" class="error">
          {{ previewError }}
        </p>
        <div v-else-if="preview" class="preview-card">
          <p>已识别 Codex 周窗口，上游预计重置 {{ date(preview.reset) }}</p>
        </div>
      </div>
    </template>

    <!-- 详情弹窗 -->
    <template v-else-if="link && modal === 'detail'">
      <div class="detail-modal-wrapper">
        <div class="detail-header-card">
          <div class="detail-header-identity">
            <h3>{{ keyName(link) }}</h3>
            <p class="muted">
              {{ accountName(link) }}
              <span v-if="account(link)?.email && account(link)?.email !== accountName(link)">
                ({{ account(link)?.email }})
              </span>
            </p>
          </div>
          <ZSegmented
            v-model="detailTab"
            aria-label="详情分类"
            :options="detailTabs"
            class="detail-segmented"
          />
        </div>

        <!-- 概览标签 -->
        <div v-show="detailTab === 'overview'" class="tab-pane">
          <div class="grid-2col">
            <ZCard title="Key 用量" padding="compact">
              <div class="metric-group">
                <span class="metric-label">本周用量</span>
                <div class="metric-value">
                  <p v-if="budget(link)">
                    {{ money(budget(link)!.weekly_used_usd) }} / {{ Number(budget(link)!.weekly_limit_usd) === 0 ? '不限额' : money(budget(link)!.weekly_limit_usd) }}
                  </p>
                  <p v-else>
                    无法读取预算
                  </p>
                </div>
                <div
                  v-if="budget(link) && usageRatio(budget(link)!.weekly_used_usd, budget(link)!.weekly_limit_usd) !== null"
                  class="progress-track"
                >
                  <div
                    class="progress-fill"
                    :class="`progress--${usageTone(usageRatio(budget(link)!.weekly_used_usd, budget(link)!.weekly_limit_usd))}`"
                    :style="{ width: `${usageRatio(budget(link)!.weekly_used_usd, budget(link)!.weekly_limit_usd)}%` }"
                  />
                </div>
              </div>

              <div class="metric-group mt-3">
                <span class="metric-label">今日用量</span>
                <div class="metric-value">
                  <p v-if="budget(link)">
                    {{ money(budget(link)!.daily_used_usd) }} / {{ Number(budget(link)!.daily_limit_usd) === 0 ? '不限额' : money(budget(link)!.daily_limit_usd) }}
                  </p>
                  <p v-else>
                    无法读取预算
                  </p>
                </div>
                <div
                  v-if="budget(link) && usageRatio(budget(link)!.daily_used_usd, budget(link)!.daily_limit_usd) !== null"
                  class="progress-track"
                >
                  <div
                    class="progress-fill"
                    :class="`progress--${usageTone(usageRatio(budget(link)!.daily_used_usd, budget(link)!.daily_limit_usd))}`"
                    :style="{ width: `${usageRatio(budget(link)!.daily_used_usd, budget(link)!.daily_limit_usd)}%` }"
                  />
                </div>
              </div>

              <div class="key-reset-box mt-3">
                <span class="metric-label">Key 自身的周用量重置时间</span>
                <p>{{ budget(link)?.weekly_resets_at_ms ? date(budget(link)!.weekly_resets_at_ms, true) : '尚未开启或已到期' }}</p>
                <p class="muted">
                  由宿主独立计算，可能早于账号重置；这不是 Key 失效时间
                </p>
              </div>
            </ZCard>

            <ZCard title="账号周期与使用率" padding="compact">
              <div class="metric-group">
                <span class="metric-label">账号使用情况</span>
                <div class="metric-value">
                  {{ percent(sample(link).used) }}
                </div>
                <div
                  v-if="sample(link).used != null"
                  class="progress-track"
                >
                  <div
                    class="progress-fill"
                    :class="`progress--${usageTone(sample(link).used)}`"
                    :style="{ width: `${Math.min(100, Math.max(0, sample(link).used!))}%` }"
                  />
                </div>
              </div>

              <div class="metric-group mt-3">
                <span class="metric-label">账号预计重置</span>
                <div class="metric-value">
                  {{ date(sample(link).reset, true) }}
                  <span v-if="relativeReset(sample(link).reset)" class="relative-time">
                    ({{ relativeReset(sample(link).reset) }})
                  </span>
                </div>
              </div>

              <div class="metric-group mt-3">
                <span class="metric-label">关联状态</span>
                <div class="mt-1">
                  <ZTag :type="tagType(tone(link))" size="small">
                    {{ status(link) }}
                  </ZTag>
                </div>
              </div>
            </ZCard>

            <ZCard title="日限重置卡" padding="compact">
              <div class="setting-item-inline">
                <div class="metric-group">
                  <span class="metric-label">重置卡功能</span>
                  <div class="metric-value mt-1">
                    <ZSwitch
                      :model-value="!!link.card_enabled"
                      label="日限重置卡开关"
                      :disabled="busy"
                      @update:model-value="toggleCard"
                    />
                  </div>
                </div>
              </div>

              <div class="metric-group mt-3">
                <span class="metric-label">当前卡状态</span>
                <div class="metric-value mt-1">
                  <ZTag :type="tagType(cardTone)" size="small">
                    {{ cardStatus }}
                  </ZTag>
                </div>
              </div>

              <div v-if="currentCard?.used_at" class="metric-group mt-3">
                <span class="metric-label">使用时间</span>
                <div class="metric-value">
                  {{ date(currentCard.used_at, true) }}
                </div>
              </div>

              <div v-if="currentCard?.mail" class="metric-group mt-3">
                <span class="metric-label">通知邮件</span>
                <div class="metric-value">
                  {{ cardMailStatusText(currentCard.mail) }}
                </div>
              </div>

              <div v-if="currentCard?.status === 'unknown'" class="card-refund-box mt-3">
                <div class="alert-box alert-danger mb-2">
                  <p>重置结果未知，可由管理员核实后退还卡</p>
                </div>
                <ZCheckbox
                  v-model="cardRefundAcknowledge"
                  label="已核实该 Key 当日用量未被清零"
                  :disabled="busy"
                />
                <div class="mt-2">
                  <ZButton
                    size="small"
                    type="danger"
                    :disabled="busy || !cardRefundAcknowledge"
                    @click="refundCard"
                  >
                    退还卡
                  </ZButton>
                </div>
              </div>

              <div class="metric-group mt-3">
                <span class="metric-label">未完成卡通知</span>
                <div class="metric-value">
                  {{ formatCardNoticeSummary(queuedCardNoticesCount, unknownCardNoticesCount) }}
                </div>
              </div>

              <div v-if="unknownCardNoticesCount > 0" class="card-notice-clear-box mt-3">
                <ZCheckbox
                  v-model="cardNoticeAcknowledge"
                  label="已核实这些通知邮件的投递情况，删除记录且不重发"
                  :disabled="busy"
                />
                <div class="mt-2">
                  <ZButton
                    size="small"
                    type="danger"
                    :disabled="busy || !cardNoticeAcknowledge"
                    @click="clearUnknownCardNotices"
                  >
                    清理未知通知
                  </ZButton>
                </div>
              </div>

              <div class="carpool-link-box mt-3">
                <span class="metric-label">拼车页地址</span>
                <div class="carpool-link-row mt-1">
                  <code class="carpool-path">/carpool/</code>
                  <ZButton size="small" :disabled="busy" @click="copyCarpoolUrl">
                    {{ copiedCarpool ? '已复制' : '复制' }}
                  </ZButton>
                </div>
              </div>
            </ZCard>

            <ZCard title="本周期占比" padding="compact">
              <div v-if="!isShareValid" class="empty-stats">
                <p class="muted">
                  统计生成中
                </p>
                <p v-if="currentShare?.computed_at" class="muted text-xs mt-1">
                  缓存时间：{{ date(currentShare.computed_at, true) }}
                </p>
              </div>
              <template v-else>
                <div class="metric-group">
                  <span class="metric-label">该 Key 花费</span>
                  <div class="metric-value">
                    {{ money(keyShareCost) }}
                  </div>
                </div>

                <div class="metric-group mt-3">
                  <span class="metric-label">账号花费</span>
                  <div class="metric-value">
                    {{ money(currentShare!.account_cost) }}
                  </div>
                </div>

                <div class="metric-group mt-3">
                  <span class="metric-label">本周期占比</span>
                  <div class="metric-value">
                    {{ percent(keySharePercent) }}
                  </div>
                  <div
                    v-if="keySharePercent !== null"
                    class="progress-track"
                  >
                    <div
                      class="progress-fill"
                      :class="`progress--${usageTone(keySharePercent)}`"
                      :style="{ width: `${Math.min(100, Math.max(0, keySharePercent))}%` }"
                    />
                  </div>
                </div>

                <div class="metric-group mt-3">
                  <span class="metric-label">统计时间</span>
                  <div class="metric-value text-xs muted">
                    {{ date(currentShare!.computed_at, true) }}
                  </div>
                </div>
              </template>
            </ZCard>

            <ZCard title="账号点数与重置卡" padding="compact" class="account-info-card">
              <div class="account-info-grid">
                <div class="metric-group">
                  <span class="metric-label">额度点数</span>
                  <div class="metric-value">
                    {{ formatCredits(currentAccountInfo?.credits) }}
                  </div>
                  <p class="muted text-xs mt-1">
                    获取时间：{{ currentAccountInfo?.credits_at ? displayDate(currentAccountInfo.credits_at, DISPLAY_TIMEZONE, true) : '暂无时间' }}
                  </p>
                </div>

                <div class="metric-group">
                  <span class="metric-label">可用重置卡</span>
                  <div class="metric-value">
                    <ZTag v-if="currentAccountInfo?.reset_cards" size="small" type="success">
                      {{ formatResetCardsAvailable(currentAccountInfo.reset_cards.available_count) }}
                    </ZTag>
                    <span v-else class="muted text-xs">
                      获取中
                    </span>
                  </div>
                  <p class="muted text-xs mt-1">
                    获取时间：{{ currentAccountInfo?.reset_cards_at ? displayDate(currentAccountInfo.reset_cards_at, DISPLAY_TIMEZONE, true) : '暂无时间' }}
                  </p>
                </div>
              </div>

              <div v-if="currentAccountInfo?.reset_cards_failed_at" class="alert-box alert-warning mt-3">
                <p>{{ formatResetCardsFailedHint(currentAccountInfo.reset_cards_failed_at, currentAccountInfo.reset_cards_at) }}</p>
              </div>

              <div class="reset-cards-section mt-3">
                <span class="metric-label">重置卡列表</span>
                <div v-if="!currentAccountInfo?.reset_cards" class="muted text-xs mt-2" role="status">
                  {{ currentAccountInfo?.reset_cards_failed_at ? '获取失败，稍后自动重试' : '获取中...' }}
                </div>
                <div v-else-if="!currentAccountInfo.reset_cards.cards || currentAccountInfo.reset_cards.cards.length === 0" class="muted text-xs mt-2">
                  暂无重置卡
                </div>
                <ul v-else class="reset-card-list mt-2">
                  <li
                    v-for="(c, idx) in sortResetCards(currentAccountInfo.reset_cards.cards)"
                    :key="idx"
                    class="reset-card-item"
                  >
                    <div class="reset-card-info">
                      <div class="reset-card-header">
                        <span class="reset-card-title">{{ formatResetCardTitle(c.title) }}</span>
                        <div class="reset-card-tags">
                          <ZTag v-if="c.reset_type" size="small" type="default">
                            {{ formatResetCardType(c.reset_type) }}
                          </ZTag>
                          <ZTag size="small" :type="resetCardStatusTone(c.status)">
                            {{ formatResetCardStatus(c.status) }}
                          </ZTag>
                        </div>
                      </div>
                      <p class="reset-card-expiry">
                        {{ c.expires_at ? `${displayDate(c.expires_at, DISPLAY_TIMEZONE, true)} 到期` : '无到期时间' }}
                      </p>
                    </div>
                  </li>
                </ul>
              </div>
            </ZCard>
          </div>
        </div>

        <!-- 通知与测试标签 -->
        <div v-show="detailTab === 'notify_test'" class="tab-pane">
          <ZCard title="重置后通知" padding="compact" class="mb-4">
            <ZFormItem label="收件邮箱">
              <div class="input-with-button">
                <ZInput v-model="draftRecipient" type="email" :disabled="busy" />
                <ZButton :disabled="busy" @click="act('recipient', { recipient: draftRecipient }, false)">
                  保存邮箱
                </ZButton>
              </div>
              <p class="muted">
                只影响后续重置；已排队邮件使用重置时的收件地址
              </p>
            </ZFormItem>
          </ZCard>

          <ZCard title="手动测试" padding="compact" class="mb-4">
            <p class="muted">
              立即使用已保存的配置发送，不清零额度；自动开关关闭或关联暂停时也可测试
            </p>
            <div class="tools mt-2">
              <ZButton :disabled="busy" @click="confirmManual(true)">
                发送测试消息
              </ZButton>
              <ZButton :disabled="busy || !smtpReady || !link.recipient || draftRecipient.trim() !== link.recipient" @click="confirmManual(false)">
                发送测试邮件
              </ZButton>
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
          </ZCard>

          <ZCard title="重置后发送记录" padding="compact">
            <div v-if="hasRetryableDeliveries" class="mb-3">
              <ZCheckbox v-model="acknowledge" label="我了解重试可能产生重复请求或重复邮件" />
            </div>
            <div v-if="link.deliveries && link.deliveries.length" class="deliveries-list">
              <div v-for="job in [...link.deliveries].reverse()" :key="job.id" class="delivery-row">
                <div class="delivery-info">
                  <span>{{ date(job.reset_at) }}</span>
                  <span class="delivery-sep">·</span>
                  <span>测试：{{ sendLabel(job.probe) }}</span>
                  <span class="delivery-sep">·</span>
                  <span>邮件：{{ sendLabel(job.mail) }}</span>
                </div>
                <div v-if="['unknown', 'failed'].includes(job.probe) || ['unknown', 'failed'].includes(job.mail)" class="delivery-actions">
                  <ZButton v-if="['unknown', 'failed'].includes(job.probe)" size="small" :disabled="busy || !acknowledge" @click="act('retry_probe', { delivery_id: job.id }, false)">
                    重试测试
                  </ZButton>
                  <ZButton v-if="['unknown', 'failed'].includes(job.mail)" size="small" :disabled="busy || !acknowledge" @click="act('retry_mail', { delivery_id: job.id }, false)">
                    重试邮件
                  </ZButton>
                </div>
              </div>
            </div>
            <p v-else class="muted">
              暂无发送记录
            </p>
          </ZCard>
        </div>

        <!-- 记录与诊断标签 -->
        <div v-show="detailTab === 'audit_diag'" class="tab-pane">
          <ZCard title="最近事件" padding="compact" class="mb-4">
            <div v-if="link.events && link.events.length" class="events-list">
              <p v-for="(event, index) in [...link.events].reverse()" :key="index" class="muted">
                {{ date(event.at) }} · {{ event.reason }}
              </p>
            </div>
            <p v-else class="muted">
              暂无事件记录
            </p>
          </ZCard>

          <ZCard title="判定与诊断" padding="compact">
            <dl class="diag-dl">
              <dt>窗口规则</dt>
              <dd>窗口 codex:604800s · 604800 秒</dd>
              <dt>基线周期</dt>
              <dd>已确认周期结束 {{ date(link.baseline.reset, true) }}</dd>
              <dt>最近样本</dt>
              <dd>{{ date(sample(link).observed, true) }}</dd>
              <template v-if="link.pending">
                <dt>待处理原因</dt>
                <dd>{{ link.pending.reason }}</dd>
              </template>
              <template v-if="link.pending?.before">
                <dt>重置前用量</dt>
                <dd>重置前日用量 ${{ link.pending.before.daily_used_usd }} · 周用量 ${{ link.pending.before.weekly_used_usd }}</dd>
              </template>
              <template v-if="budget(link)">
                <dt>完整预算金额</dt>
                <dd>完整金额：${{ budget(link)!.weekly_used_usd }} / {{ Number(budget(link)!.weekly_limit_usd) === 0 ? '不限额' : `$${budget(link)!.weekly_limit_usd}` }}</dd>
              </template>
              <template v-if="link.pending?.error || observation(link)?.error">
                <dt>异常说明</dt>
                <dd class="error">
                  {{ link.pending?.error || observation(link)?.error }}
                </dd>
              </template>
            </dl>
          </ZCard>
        </div>
      </div>
    </template>

    <!-- 手动测试确认弹窗 -->
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
      <ZCheckbox v-model="manualAcknowledge" label="确认立即发送，并了解重复发送的风险" :disabled="busy" />
    </template>

    <!-- 处理关联弹窗 -->
    <template v-else-if="link && modal === 'process'">
      <div class="process-modal-body">
        <div class="process-heading">
          <h3>{{ keyName(link) }}</h3>
          <p class="muted">
            {{ accountName(link) }}
          </p>
        </div>

        <template v-if="link.pending?.kind === 'unknown'">
          <div class="alert-box alert-danger">
            <p>重置结果未知，已停止自动重试</p>
            <p class="muted">
              保留现有用量并跳过会重新建立基线
            </p>
          </div>
          <ZCheckbox v-model="acknowledge" label="我了解可能清掉期间新产生的消费" :disabled="busy" />
        </template>

        <template v-else-if="link.pending?.kind === 'changed'">
          <div class="alert-box alert-warning">
            <p>目标窗口标识或时长发生变化，已停止自动清零</p>
          </div>
          <div class="window-list">
            <div v-for="w in link.pending.windows" :key="w.key" class="window-card">
              <span>{{ w.key }}</span> · <span>{{ w.window_seconds }} 秒</span> · <span>{{ date(w.reset_at_ms, true) }}</span>
            </div>
          </div>
          <p class="muted">
            请在宿主检查账号套餐，恢复唯一 Codex 周窗口后再确认或更新基线
          </p>
        </template>

        <template v-else-if="link.pending?.kind === 'confirm'">
          <div class="alert-box alert-warning">
            <p>{{ link.pending.reason === 'early' ? '账号提前进入新周期，自动清零开关已关闭' : '周期边界变化无法解释，需要你确认' }}</p>
          </div>
          <div class="boundary-grid">
            <dl>
              <dt>原边界</dt>
              <dd>{{ date(link.baseline.reset, true) }}</dd>
              <dt>新边界</dt>
              <dd>{{ date(link.pending.sample.reset, true) }}</dd>
              <dt>推算的新窗口起点</dt>
              <dd>{{ date(link.pending.sample.reset - 604800000, true) }}</dd>
            </dl>
          </div>
          <p class="muted">
            确认后会同时清零当前日用量和周用量，此操作无法撤销
          </p>
        </template>

        <p v-else>
          {{ observation(link)?.error || '读取预算失败，尚未调用清零' }}
        </p>
      </div>
    </template>

    <!-- 删除关联弹窗 -->
    <template v-else-if="modal === 'delete'">
      <p>仅删除本插件的关联记录，不撤销已执行的重置</p>
      <ZCheckbox v-model="acknowledge" label="确认删除此关联" :disabled="busy" />
    </template>

    <template #footer>
      <!-- 添加关联 footer -->
      <template v-if="modal === 'add'">
        <span class="muted mr-auto">仅建立基线，不清零</span>
        <ZButton :disabled="busy" @click="modal = ''">
          取消
        </ZButton>
        <ZButton type="primary" :loading="busy" :disabled="!keyId || !preview || previewBusy" @click="act('add', { key_id: keyId, account_id: accountId })">
          添加
        </ZButton>
      </template>

      <!-- 详情弹窗 footer: 暂停/恢复 与 删除 在左侧，关闭在右侧 -->
      <template v-else-if="modal === 'detail' && link">
        <div class="modal-footer-left">
          <ZButton :disabled="busy" @click="act(link.paused ? 'resume' : 'pause')">
            {{ link.paused ? '恢复并建立新基线' : '暂停关联' }}
          </ZButton>
          <ZButton type="danger" :disabled="busy" @click="modal = 'delete'; acknowledge = false">
            删除关联
          </ZButton>
        </div>
        <ZButton :disabled="busy" @click="modal = ''">
          关闭
        </ZButton>
      </template>

      <!-- 手动测试确认 footer -->
      <template v-else-if="['manual_probe', 'manual_mail'].includes(modal)">
        <ZButton :disabled="busy" @click="modal = ''">
          取消
        </ZButton>
        <ZButton type="primary" :loading="busy" :disabled="!manualAcknowledge || busy" @click="sendManual">
          确认发送
        </ZButton>
      </template>

      <!-- 处理关联 footer -->
      <template v-else-if="modal === 'process'">
        <ZButton :disabled="busy" @click="modal = ''">
          取消
        </ZButton>
        <template v-if="link?.pending">
          <ZButton :disabled="busy" @click="act('skip')">
            {{ link.pending.kind === 'unknown' ? '保留现有用量并跳过' : '忽略并更新基线' }}
          </ZButton>
          <ZButton v-if="link.pending.kind === 'unknown'" type="danger" :disabled="!acknowledge || busy" @click="act('retry')">
            再清零一次
          </ZButton>
          <ZButton v-if="['confirm', 'changed'].includes(link.pending.kind)" type="primary" :loading="busy" @click="act('confirm')">
            确认并清零
          </ZButton>
        </template>
        <ZButton v-else :loading="busy" @click="act('check')">
          立即检查
        </ZButton>
      </template>

      <!-- 删除关联 footer -->
      <template v-else-if="modal === 'delete'">
        <ZButton :disabled="busy" @click="modal = ''">
          取消
        </ZButton>
        <ZButton type="danger" :disabled="!acknowledge || busy" @click="act('delete')">
          删除关联
        </ZButton>
      </template>

      <!-- 通用 fallback footer -->
      <template v-else>
        <ZButton :disabled="busy" @click="modal = ''">
          关闭
        </ZButton>
      </template>
    </template>
  </ZDialog>
</template>
