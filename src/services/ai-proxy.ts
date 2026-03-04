import { hasBackend } from '@/lib/environment'
import { invoke } from '@/lib/transport'
import type {
  AccountUsage,
  NormalizedAccountUsage,
  ProviderAccount,
  ProxyModelInfo,
  ProxyStatus,
  UsageStats,
} from '@/types/ai-proxy'

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function readString(source: Record<string, unknown>, key: string): string {
  const value = source[key]
  return typeof value === 'string' ? value.trim() : ''
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0
  return Math.max(0, Math.min(100, value))
}

export function computeLeftPercent(usedPercent: number | null): number | null {
  if (usedPercent == null) return null
  return clampPercent(100 - clampPercent(usedPercent))
}

export function normalizeAccountUsage(
  usage: AccountUsage[]
): NormalizedAccountUsage[] {
  return usage.map(row => ({
    ...row,
    primary_left_percent: computeLeftPercent(row.primary_used_percent),
    secondary_left_percent: computeLeftPercent(row.secondary_used_percent),
  }))
}

export function normalizeModelsResponse(raw: unknown): ProxyModelInfo[] {
  const rawItems = Array.isArray(raw)
    ? raw
    : isObject(raw) && Array.isArray(raw.data)
      ? raw.data
      : isObject(raw) && Array.isArray(raw.models)
        ? raw.models
        : []

  const seen = new Set<string>()
  const models: ProxyModelInfo[] = []

  for (const item of rawItems) {
    if (!isObject(item)) continue

    let model =
      readString(item, 'model') ||
      readString(item, 'id') ||
      readString(item, 'name')
    let provider =
      readString(item, 'provider') ||
      readString(item, 'owned_by') ||
      readString(item, 'owner')

    if (!model) continue

    if (model.includes('/')) {
      const [first = '', ...rest] = model.split('/')
      if (rest.length > 0) {
        if (!provider) provider = first
        model = rest.join('/')
      }
    }

    if (!provider) provider = 'unknown'

    const key = `${provider}::${model}`
    if (seen.has(key)) continue
    seen.add(key)

    models.push({
      id: `${provider}/${model}`,
      model,
      provider,
    })
  }

  models.sort((a, b) => {
    const modelCmp = a.model.localeCompare(b.model)
    if (modelCmp !== 0) return modelCmp
    return a.provider.localeCompare(b.provider)
  })

  return models
}

export function getUniqueProviderForModel(
  model: string,
  models: ProxyModelInfo[]
): string | null {
  if (!model.trim()) return null
  const providers = new Set(
    models
      .filter(entry => entry.model === model)
      .map(entry => entry.provider)
      .filter(provider => provider !== 'unknown')
  )
  const [provider] = [...providers]
  return providers.size === 1 && provider ? provider : null
}

async function tryFetchModelsFromProxyPort(
  status: ProxyStatus | null
): Promise<ProxyModelInfo[]> {
  const port = status?.proxy_port
  if (!port) return []

  const response = await fetch(`http://127.0.0.1:${port}/v1/models`)
  if (!response.ok) return []
  const payload = await response.json()
  return normalizeModelsResponse(payload)
}

export async function fetchAiProxyStatus(): Promise<ProxyStatus | null> {
  if (!hasBackend()) return null
  return invoke<ProxyStatus>('get_ai_proxy_status')
}

export async function fetchAiProxyAccounts(): Promise<ProviderAccount[]> {
  if (!hasBackend()) return []
  try {
    return await invoke<ProviderAccount[]>('get_ai_proxy_accounts')
  } catch {
    return []
  }
}

export async function fetchAiProxyUsage(): Promise<UsageStats | null> {
  if (!hasBackend()) return null
  try {
    return await invoke<UsageStats>('get_ai_proxy_usage')
  } catch {
    return null
  }
}

export async function fetchAiProxyAccountUsage(): Promise<
  NormalizedAccountUsage[]
> {
  if (!hasBackend()) return []
  try {
    const usage = await invoke<AccountUsage[]>('get_ai_proxy_account_usage')
    return normalizeAccountUsage(usage)
  } catch {
    return []
  }
}

export async function fetchAiProxyModels(): Promise<ProxyModelInfo[]> {
  if (!hasBackend()) return []

  try {
    const response = await invoke<unknown>('get_ai_proxy_models')
    return normalizeModelsResponse(response)
  } catch {
    try {
      const status = await fetchAiProxyStatus()
      return await tryFetchModelsFromProxyPort(status)
    } catch {
      return []
    }
  }
}
