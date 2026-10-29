<script setup lang="ts">
import type { MemberView } from './api'
import {
  BaseButton,
  BaseCard,
  BaseCheckbox,
  BaseFormItem,
  BaseInput,
  BasePageHeader,
  BaseTag,
} from '@codex-proxy/ui'
import { LogOut, RefreshCw } from '@lucide/vue'
import { computed, ref } from 'vue'
import {
  cardStatusText,
  cardStatusTone,
  DISPLAY_TIMEZONE,
  displayDate,
  formatCycleRange,
  formatMemberError,
  isSessionActive,
  money,
  percent,
  sortShareItems,
  usageRatio,
  usageTone,
} from '../display'
import { loadMember, MemberApiError, useCard } from './api'

const sessionId = ref(0)
const keyInput = ref('')
const authenticatedKey = ref('')
const loading = ref(false)
const error = ref('')

const member = ref<MemberView | null>(null)
const confirmUseCard = ref(false)
const useCardBusy = ref(false)
const actionMessage = ref('')
const actionError = ref('')

const cycleText = computed(() => formatCycleRange(member.value?.cycle))
const activeCard = computed(() => member.value?.card)
const cardStatus = computed(() => cardStatusText(activeCard.value?.status, !!activeCard.value))
const cardTone = computed(() => cardStatusTone(activeCard.value?.status, !!activeCard.value))

const sortedShares = computed(() => {
  if (!member.value?.shares)
    return []
  return sortShareItems(member.value.shares.items, member.value.shares.other_percent)
})

async function handleLogin() {
  const trimmed = keyInput.value.trim()
  if (!trimmed || loading.value)
    return
  const sid = ++sessionId.value
  loading.value = true
  error.value = ''
  try {
    const res = await loadMember(trimmed)
    if (!isSessionActive(sid, sessionId.value))
      return
    authenticatedKey.value = trimmed
    keyInput.value = ''
    member.value = res
  }
  catch (e) {
    if (!isSessionActive(sid, sessionId.value))
      return
    if (e instanceof MemberApiError)
      error.value = formatMemberError(e.status, e.message)
    else
      error.value = (e as Error).message
  }
  finally {
    if (isSessionActive(sid, sessionId.value))
      loading.value = false
  }
}

function handleLogout() {
  sessionId.value++
  authenticatedKey.value = ''
  keyInput.value = ''
  member.value = null
  confirmUseCard.value = false
  actionMessage.value = ''
  actionError.value = ''
  error.value = ''
  loading.value = false
  useCardBusy.value = false
}

async function handleRefresh() {
  if (!authenticatedKey.value || loading.value)
    return
  const sid = sessionId.value
  loading.value = true
  actionError.value = ''
  actionMessage.value = ''
  try {
    const res = await loadMember(authenticatedKey.value)
    if (!isSessionActive(sid, sessionId.value))
      return
    member.value = res
  }
  catch (e) {
    if (!isSessionActive(sid, sessionId.value))
      return
    if (e instanceof MemberApiError)
      actionError.value = formatMemberError(e.status, e.message)
    else
      actionError.value = (e as Error).message
  }
  finally {
    if (isSessionActive(sid, sessionId.value))
      loading.value = false
  }
}

async function handleUseCard() {
  if (!confirmUseCard.value || useCardBusy.value || !member.value?.can_use_card || !authenticatedKey.value)
    return
  const sid = sessionId.value
  useCardBusy.value = true
  actionError.value = ''
  actionMessage.value = ''
  try {
    const res = await useCard(authenticatedKey.value)
    if (!isSessionActive(sid, sessionId.value))
      return
    actionMessage.value = res.result === 'used'
      ? (res.message || '重置卡使用成功，今日用量已清零')
      : (res.message || '重置结果未知，已提交核实')
    confirmUseCard.value = false
  }
  catch (e) {
    if (!isSessionActive(sid, sessionId.value))
      return
    if (e instanceof MemberApiError)
      actionError.value = formatMemberError(e.status, e.message)
    else
      actionError.value = (e as Error).message
  }
  finally {
    try {
      if (isSessionActive(sid, sessionId.value)) {
        const updated = await loadMember(authenticatedKey.value)
        if (isSessionActive(sid, sessionId.value))
          member.value = updated
      }
    }
    catch (reloadErr) {
      if (isSessionActive(sid, sessionId.value) && !actionError.value) {
        if (reloadErr instanceof MemberApiError)
          actionError.value = formatMemberError(reloadErr.status, reloadErr.message)
        else
          actionError.value = (reloadErr as Error).message
      }
    }
    finally {
      if (isSessionActive(sid, sessionId.value))
        useCardBusy.value = false
    }
  }
}
</script>

<template>
  <main>
    <div v-if="!member" class="carpool-auth-box">
      <BaseCard title="拼车成员登录" description="请输入你的 API Key 查看用量与日限重置卡">
        <form @submit.prevent="handleLogin">
          <BaseFormItem label="API Key">
            <BaseInput
              v-model="keyInput"
              type="password"
              autocomplete="off"
              placeholder="请输入你的 API Key"
              :disabled="loading"
            />
          </BaseFormItem>
          <p v-if="error" role="alert" class="error">
            {{ error }}
          </p>
          <div class="mt-4">
            <BaseButton
              variant="primary"
              class="w-full"
              :loading="loading"
              :disabled="!keyInput.trim() || loading"
              @click="handleLogin"
            >
              查看用量
            </BaseButton>
          </div>
          <p class="muted text-xs mt-3">
            Key 仅保存在当前页面内存中，用于查询与用卡，不写入本地存储。
          </p>
        </form>
      </BaseCard>
    </div>

    <div v-else class="carpool-container">
      <BasePageHeader
        title="拼车用量与重置卡"
        :description="`当前 Key：${member.key_name}`"
      >
        <template #actions>
          <div class="flex items-center gap-2">
            <BaseButton size="sm" :disabled="loading || useCardBusy" @click="handleRefresh">
              <RefreshCw :size="14" :class="{ 'animate-spin': loading }" />刷新
            </BaseButton>
            <BaseButton size="sm" :disabled="useCardBusy" @click="handleLogout">
              <LogOut :size="14" />退出
            </BaseButton>
          </div>
        </template>
      </BasePageHeader>

      <div class="cycle-banner mt-4">
        <div class="metric-group">
          <span class="metric-label">本周期起止（北京时间 UTC+8）</span>
          <span class="font-medium text-sm">{{ cycleText }}</span>
        </div>
        <div v-if="member.paused">
          <BaseTag type="neutral" size="sm">
            关联已暂停
          </BaseTag>
        </div>
      </div>

      <p v-if="actionError" role="alert" class="error mt-3">
        {{ actionError }}
      </p>
      <p v-if="actionMessage" class="success-text mt-3">
        {{ actionMessage }}
      </p>

      <div class="grid-2col mt-4">
        <BaseCard title="本人用量" padding="compact">
          <div class="metric-group">
            <span class="metric-label">今日用量</span>
            <div class="metric-value">
              <p v-if="member.budget">
                {{ money(member.budget.daily_used_usd) }} / {{ Number(member.budget.daily_limit_usd) === 0 ? '不限额' : money(member.budget.daily_limit_usd) }}
              </p>
              <p v-else class="muted">
                暂无数据
              </p>
            </div>
            <div
              v-if="member.budget && usageRatio(member.budget.daily_used_usd, member.budget.daily_limit_usd) !== null"
              class="progress-track"
            >
              <div
                class="progress-fill"
                :class="`progress--${usageTone(usageRatio(member.budget.daily_used_usd, member.budget.daily_limit_usd))}`"
                :style="{ width: `${usageRatio(member.budget.daily_used_usd, member.budget.daily_limit_usd)}%` }"
              />
            </div>
          </div>

          <div class="metric-group mt-3">
            <span class="metric-label">本周用量</span>
            <div class="metric-value">
              <p v-if="member.budget">
                {{ money(member.budget.weekly_used_usd) }} / {{ Number(member.budget.weekly_limit_usd) === 0 ? '不限额' : money(member.budget.weekly_limit_usd) }}
              </p>
              <p v-else class="muted">
                暂无数据
              </p>
            </div>
            <div
              v-if="member.budget && usageRatio(member.budget.weekly_used_usd, member.budget.weekly_limit_usd) !== null"
              class="progress-track"
            >
              <div
                class="progress-fill"
                :class="`progress--${usageTone(usageRatio(member.budget.weekly_used_usd, member.budget.weekly_limit_usd))}`"
                :style="{ width: `${usageRatio(member.budget.weekly_used_usd, member.budget.weekly_limit_usd)}%` }"
              />
            </div>
          </div>
        </BaseCard>

        <BaseCard title="日限重置卡" padding="compact">
          <div class="metric-group">
            <span class="metric-label">当前卡状态</span>
            <div class="metric-value mt-1">
              <BaseTag :type="cardTone" size="sm">
                {{ cardStatus }}
              </BaseTag>
            </div>
          </div>

          <div v-if="activeCard?.used_at" class="metric-group mt-3">
            <span class="metric-label">使用时间</span>
            <div class="metric-value">
              {{ displayDate(activeCard.used_at, DISPLAY_TIMEZONE, true) }}
            </div>
          </div>

          <div v-if="!member.card_enabled" class="mt-3">
            <p class="muted text-xs">
              当前关联未开启日限重置卡功能
            </p>
          </div>

          <div class="card-action-box">
            <BaseCheckbox
              v-model="confirmUseCard"
              show-label
              label="确认清零今日用量，此操作不可撤销"
              :disabled="!member.can_use_card || useCardBusy"
            />
            <BaseButton
              variant="primary"
              :loading="useCardBusy"
              :disabled="!member.can_use_card || !confirmUseCard || useCardBusy"
              @click="handleUseCard"
            >
              使用日限重置卡
            </BaseButton>
            <p v-if="!member.can_use_card && member.card_block_reason" class="muted text-xs">
              {{ member.card_block_reason }}
            </p>
          </div>
        </BaseCard>
      </div>

      <div class="mt-4">
        <BaseCard title="同车占比" padding="compact">
          <div v-if="!member.shares" class="empty-stats">
            <p class="muted">
              统计生成中
            </p>
          </div>
          <template v-else>
            <div class="flex items-center justify-between mb-3">
              <span class="metric-label">本周期同车成员花费占比</span>
              <span class="muted text-xs">
                统计时间：{{ displayDate(member.shares.computed_at, DISPLAY_TIMEZONE, true) }}
              </span>
            </div>

            <div class="share-list">
              <div
                v-for="(item, idx) in sortedShares"
                :key="idx"
                class="share-item-box"
              >
                <div class="share-item-row">
                  <span class="share-item-label">
                    {{ item.label }}
                    <BaseTag v-if="item.me" type="primary" size="sm">
                      我
                    </BaseTag>
                  </span>
                  <span class="share-item-percent">
                    {{ percent(item.percent) }}
                  </span>
                </div>
                <div class="progress-track">
                  <div
                    class="progress-fill"
                    :class="item.me ? `progress--${usageTone(item.percent)}` : 'progress--neutral'"
                    :style="{ width: `${Math.min(100, Math.max(0, item.percent))}%` }"
                  />
                </div>
              </div>
            </div>
          </template>
        </BaseCard>
      </div>
    </div>
  </main>
</template>
