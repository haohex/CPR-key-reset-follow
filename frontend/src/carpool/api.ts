// 成员页接口合同，见 docs/design.md"管理与成员接口"。Key 只作为参数传入，不在此处保存
export type CardStatus = 'available' | 'using' | 'used' | 'unknown'
export interface MemberCard { status: CardStatus, issued_at: number, used_at: number | null }
export interface MemberBudget { daily_used_usd: string, daily_limit_usd: string, weekly_used_usd: string, weekly_limit_usd: string }
export interface ShareItem { label: string, me: boolean, percent: number }
export interface MemberShares { computed_at: number, items: ShareItem[], other_percent: number }
export interface MemberView {
  key_name: string
  paused: boolean
  card_enabled: boolean
  card: MemberCard | null
  cycle: { start_ms: number, end_ms: number } | null
  budget: MemberBudget | null
  can_use_card: boolean
  card_block_reason: string | null
  shares: MemberShares | null
}
export interface UseCardResult { result: 'used' | 'unknown', message: string }

export class MemberApiError extends Error {
  constructor(public status: number, message: string) {
    super(message)
  }
}

async function call<T>(method: 'GET' | 'POST', path: string, key: string): Promise<T> {
  // 相对路径：页面位于 /carpool/，接口位于 /carpool/api/*
  const response = await fetch(`./api/${path}`, { method, headers: { Authorization: `Bearer ${key}` }, cache: 'no-store', credentials: 'omit' })
  const body = await response.json().catch(() => ({}))
  if (!response.ok)
    throw new MemberApiError(response.status, body.error || '请求失败，请稍后重试')
  return body as T
}

export const loadMember = (key: string) => call<MemberView>('GET', 'me', key)
export const useCard = (key: string) => call<UseCardResult>('POST', 'card/use', key)
