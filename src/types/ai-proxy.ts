export interface ProxyStatus {
  running: boolean
  proxy_port: number | null
  backend_port: number | null
  backend_installed: boolean
}

export interface BackendStatus {
  installed: boolean
  path: string | null
}

export interface ProviderAccount {
  provider: string
  account_id: string
  display_name: string
  enabled: boolean
  expired: boolean
}

export interface UsageStats {
  total_requests: number
  per_model: Record<string, number>
  per_provider: Record<string, number>
  per_account: Record<string, number>
  by_request: UsageRequestSummary[]
}

export interface UsageRequestSummary {
  provider: string
  model: string
  account: string
  requests: number
}

export interface AccountUsage {
  account_id: string
  provider: string
  primary_used_percent: number | null
  primary_reset_seconds: number | null
  secondary_used_percent: number | null
  secondary_reset_seconds: number | null
  plan_type: string | null
  status: string // "loaded", "error", "invalid_credentials", "unsupported"
}

export interface NormalizedAccountUsage extends AccountUsage {
  primary_left_percent: number | null
  secondary_left_percent: number | null
}

export interface ProxyModelInfo {
  id: string
  model: string
  provider: string
}

export interface LoginStatusEvent {
  provider: string
  status: 'started' | 'completed' | 'failed' | 'timed_out'
}

export interface LoginOutputEvent {
  provider: string
  line: string
}

export type ProxyProvider =
  | 'claude'
  | 'codex'
  | 'github-copilot'
  | 'gemini'
  | 'antigravity'
  | 'qwen'
  | 'zai'
  | 'kiro'

export interface ProxyProviderInfo {
  id: ProxyProvider
  name: string
  /** true if provider uses API keys instead of OAuth */
  apiKeyOnly?: boolean
}

export const PROXY_PROVIDERS: ProxyProviderInfo[] = [
  { id: 'claude', name: 'Claude' },
  { id: 'codex', name: 'Codex / OpenAI' },
  { id: 'github-copilot', name: 'GitHub Copilot' },
  { id: 'gemini', name: 'Gemini' },
  { id: 'antigravity', name: 'Antigravity' },
  { id: 'qwen', name: 'Qwen' },
  { id: 'zai', name: 'Z.AI', apiKeyOnly: true },
  { id: 'kiro', name: 'Kiro' },
]

export interface ProxyModelGroup {
  name: string
  models: ProxyModelGroupMember[]
  enabled: boolean
  strategy: 'round_robin' | 'fill_first'
}

export interface ProxyModelGroupMember {
  model: string
  provider: string
  enabled: boolean
}
