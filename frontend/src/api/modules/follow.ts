import { request } from '../request'

export interface Sample { observed: number, reset: number, used: number | null }
export type DisplayTimezone = 'browser' | 'Asia/Shanghai' | 'UTC'
export interface Budget { daily_limit_usd: string, daily_used_usd: string, weekly_limit_usd: string, weekly_used_usd: string, weekly_resets_at_ms: number | null }
export interface SmtpSettings { enabled: boolean, server: string, port: number, security: 'tls' | 'starttls', username: string, password: string, from: string }
export type SendStatus = 'skipped' | 'queued' | 'unknown' | 'succeeded' | 'failed'
export interface Delivery { id: number, reset_at: number, recipient: string, probe: SendStatus, mail: SendStatus }
export interface ManualTest { at: number, status: SendStatus, recipient: string | null }
export interface Link { last_manual_probe?: ManualTest | null, last_manual_mail?: ManualTest | null, recipient: string, deliveries: Delivery[], key_id: string, account_id: string, baseline: Sample, paused: boolean, pending: null | { kind: 'confirm' | 'ready' | 'unknown' | 'changed', sample: Sample, reason: string, before?: Budget, error?: string, windows?: { key: string, window_seconds: number | null, reset_at_ms: number | null }[] }, events: { at: number, reason: string }[] }
export interface Snapshot { smtp_password_configured: boolean, version: number | null, state: { probe_enabled: boolean, smtp: SmtpSettings, early_auto: boolean, display_timezone: DisplayTimezone, links: Record<string, Link>, accounts: Record<string, { sample: Sample | null, error: string | null, failure_since: number | null }> }, keys: { id: string, name: string }[], accounts: { account_id: string, name: string, email: string | null, provider_id: string, authentication_kind: string }[], budgets: Record<string, Budget> }
export const load = () => request<Snapshot>('GET', 'state')
export const command = (body: object) => request<{ ok: boolean, sample?: Sample }>('POST', 'action', body)
