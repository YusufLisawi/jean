import React, { useCallback, useEffect, useMemo, useState } from 'react'
import {
  Copy,
  Download,
  FolderOpen,
  Info,
  Key,
  Loader2,
  Plus,
  Trash2,
  RefreshCw,
} from 'lucide-react'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { usePreferences, useSavePreferences } from '@/services/preferences'
import { invoke, listen, useWsConnectionStatus } from '@/lib/transport'
import { toast } from 'sonner'
import { isNativeApp } from '@/lib/environment'
import {
  fetchAiProxyAccountUsage,
  fetchAiProxyAccounts,
  fetchAiProxyModels,
  fetchAiProxyStatus,
  fetchAiProxyUsage,
  getUniqueProviderForModel,
} from '@/services/ai-proxy'
import type {
  ProxyStatus,
  ProviderAccount,
  UsageStats,
  ProxyModelGroup,
  NormalizedAccountUsage,
  LoginStatusEvent,
  LoginOutputEvent,
  ProxyModelInfo,
} from '@/types/ai-proxy'
import { PROXY_PROVIDERS } from '@/types/ai-proxy'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Format seconds into "2h 15m" or "3d 4h" */
function formatResetTime(seconds: number | null): string {
  if (seconds == null || seconds <= 0) return 'now'
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${m}m`
  return `${m}m`
}

/** Color class for usage percentage */
function usageColor(percent: number | null): string {
  if (percent == null) return 'text-muted-foreground'
  if (percent >= 80) return 'text-red-500'
  if (percent >= 50) return 'text-yellow-500'
  return 'text-green-500'
}

function formatPercent(percent: number | null): string {
  if (percent == null) return '—'
  return `${Math.round(percent)}%`
}

// ---------------------------------------------------------------------------
// Shared layout primitives (same pattern as WebAccessPane)
// ---------------------------------------------------------------------------

const SettingsSection: React.FC<{
  title: string
  action?: React.ReactNode
  children: React.ReactNode
}> = ({ title, action, children }) => (
  <div className="space-y-4">
    <div>
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-medium text-foreground">{title}</h3>
        {action}
      </div>
      <Separator className="mt-2" />
    </div>
    {children}
  </div>
)

const InlineField: React.FC<{
  label: string
  description?: React.ReactNode
  children: React.ReactNode
}> = ({ label, description, children }) => (
  <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:gap-4">
    <div className="space-y-0.5 sm:w-96 sm:shrink-0">
      <Label className="text-sm text-foreground">{label}</Label>
      {description && (
        <div className="text-xs text-muted-foreground">{description}</div>
      )}
    </div>
    {children}
  </div>
)

const HelperCard: React.FC<{
  title: string
  children: React.ReactNode
}> = ({ title, children }) => (
  <div className="rounded-md border border-blue-500/20 bg-blue-500/5 p-3">
    <div className="flex items-center gap-1.5 text-xs font-medium text-blue-700 dark:text-blue-300">
      <Info className="size-3.5" />
      <span>{title}</span>
    </div>
    <div className="mt-1.5 space-y-1 text-xs text-muted-foreground">
      {children}
    </div>
  </div>
)

// ---------------------------------------------------------------------------
// Main pane
// ---------------------------------------------------------------------------

export const AiProxyPane: React.FC = () => {
  const { data: preferences } = usePreferences()
  const savePreferences = useSavePreferences()

  const [proxyStatus, setProxyStatus] = useState<ProxyStatus | null>(null)
  const [accounts, setAccounts] = useState<ProviderAccount[]>([])
  const [usage, setUsage] = useState<UsageStats | null>(null)
  const [accountUsage, setAccountUsage] = useState<NormalizedAccountUsage[]>([])
  const [availableModels, setAvailableModels] = useState<ProxyModelInfo[]>([])
  const [isToggling, setIsToggling] = useState(false)
  const [isRefreshingUsage, setIsRefreshingUsage] = useState(false)
  const [isRefreshingModels, setIsRefreshingModels] = useState(false)

  // Z.AI API key input state
  const [zaiKeyInput, setZaiKeyInput] = useState('')
  const [zaiKeySaving, setZaiKeySaving] = useState(false)
  const [qwenEmailInput, setQwenEmailInput] = useState('')

  // Install progress tracking (same pattern as onboarding CLI installs)
  const [installProgress, setInstallProgress] = useState<{
    stage: string
    message: string
    percent: number
  } | null>(null)
  const [isInstalling, setIsInstalling] = useState(false)
  const [installError, setInstallError] = useState<string | null>(null)
  const wsConnected = useWsConnectionStatus()

  // Track providers with active OAuth login flows
  const [loggingInProviders, setLoggingInProviders] = useState<Set<string>>(
    new Set()
  )

  // Last output line per provider (e.g. device codes for GitHub Copilot)
  const [loginOutput, setLoginOutput] = useState<Record<string, string>>({})

  // Restore active login state on mount (handles pane reopen mid-login)
  useEffect(() => {
    if (!isNativeApp()) return
    invoke<string[]>('get_ai_proxy_active_logins')
      .then(providers => {
        if (providers.length > 0) {
          setLoggingInProviders(new Set(providers))
        }
      })
      .catch(() => {
        // ignore — command may not exist in web mode
      })
  }, [])

  // Listen for install progress events
  useEffect(() => {
    if (!isNativeApp()) return

    let unlistenFn: (() => void) | null = null

    const setupListener = async () => {
      try {
        unlistenFn = await listen<{
          stage: string
          message: string
          percent: number
        }>('ai-proxy:install-progress', event => {
          setInstallProgress(event.payload)
        })
      } catch {
        // ignore
      }
    }

    setupListener()
    return () => {
      if (unlistenFn) unlistenFn()
    }
  }, [wsConnected])

  // Local port inputs (synced on blur)
  const [proxyPortInput, setProxyPortInput] = useState(
    String(preferences?.ai_proxy_port ?? 8317)
  )
  const [backendPortInput, setBackendPortInput] = useState(
    String(preferences?.ai_proxy_backend_port ?? 8318)
  )

  // Local model groups state
  const [modelGroups, setModelGroups] = useState<ProxyModelGroup[]>(
    preferences?.ai_proxy_model_groups ?? []
  )

  // Sync local state when preferences load
  useEffect(() => {
    if (preferences?.ai_proxy_port != null)
      setProxyPortInput(String(preferences.ai_proxy_port))
  }, [preferences?.ai_proxy_port])

  useEffect(() => {
    if (preferences?.ai_proxy_backend_port != null)
      setBackendPortInput(String(preferences.ai_proxy_backend_port))
  }, [preferences?.ai_proxy_backend_port])

  useEffect(() => {
    if (preferences?.ai_proxy_model_groups)
      setModelGroups(preferences.ai_proxy_model_groups)
  }, [preferences?.ai_proxy_model_groups])

  // ---------------------------------------------------------------------------
  // Polling
  // ---------------------------------------------------------------------------

  const refreshStatus = useCallback(async () => {
    if (!isNativeApp()) return
    const status = await fetchAiProxyStatus()
    setProxyStatus(status)
  }, [])

  const refreshAccounts = useCallback(async () => {
    if (!isNativeApp()) return
    const accs = await fetchAiProxyAccounts()
    setAccounts(accs)
  }, [])

  const refreshUsage = useCallback(async () => {
    if (!isNativeApp()) return
    const stats = await fetchAiProxyUsage()
    setUsage(stats)
  }, [])

  const refreshAccountUsage = useCallback(async () => {
    if (!isNativeApp()) return
    const data = await fetchAiProxyAccountUsage()
    setAccountUsage(data)
  }, [])

  const refreshModels = useCallback(async () => {
    if (!isNativeApp()) return
    setIsRefreshingModels(true)
    try {
      const models = await fetchAiProxyModels()
      setAvailableModels(models)
    } finally {
      setIsRefreshingModels(false)
    }
  }, [])

  const refreshUsageData = useCallback(async () => {
    setIsRefreshingUsage(true)
    try {
      await Promise.all([refreshUsage(), refreshAccountUsage()])
    } finally {
      setIsRefreshingUsage(false)
    }
  }, [])

  // Listen for login status events (after refreshAccounts is defined)
  useEffect(() => {
    if (!isNativeApp()) return

    let unlistenFn: (() => void) | null = null

    const setupListener = async () => {
      try {
        unlistenFn = await listen<LoginStatusEvent>(
          'ai-proxy:login-status',
          event => {
            const { provider, status } = event.payload

            if (status === 'started') {
              setLoggingInProviders(prev => new Set(prev).add(provider))
            } else {
              setLoggingInProviders(prev => {
                const next = new Set(prev)
                next.delete(provider)
                return next
              })

              // Dismiss the persistent "waiting" toast and clear output
              toast.dismiss(`login-${provider}`)
              setLoginOutput(prev => {
                const { [provider]: _, ...rest } = prev
                return rest
              })

              if (status === 'completed') {
                toast.success(`${provider} account connected`)
                if (provider === 'qwen') setQwenEmailInput('')
                refreshAccounts()
              } else if (status === 'failed') {
                toast.error(`${provider} login failed. Check login output.`)
                refreshAccounts()
              } else if (status === 'timed_out') {
                toast.error(`${provider} login timed out (5 min)`)
              }
            }
          }
        )
      } catch {
        // ignore
      }
    }

    setupListener()
    return () => {
      if (unlistenFn) unlistenFn()
    }
  }, [wsConnected, refreshAccounts])

  // Listen for login output events (device codes, etc.)
  useEffect(() => {
    if (!isNativeApp()) return

    let unlistenFn: (() => void) | null = null

    const setupListener = async () => {
      try {
        unlistenFn = await listen<LoginOutputEvent>(
          'ai-proxy:login-output',
          event => {
            const { provider, line } = event.payload
            setLoginOutput(prev => ({ ...prev, [provider]: line }))
          }
        )
      } catch {
        // ignore
      }
    }

    setupListener()
    return () => {
      if (unlistenFn) unlistenFn()
    }
  }, [wsConnected])

  useEffect(() => {
    refreshStatus()
    refreshAccounts()
    refreshUsageData()
    refreshModels()

    const statusInterval = setInterval(refreshStatus, 3000)
    const accountsInterval = setInterval(refreshAccounts, 60000)
    const usageInterval = setInterval(refreshUsageData, 300000)
    const modelsInterval = setInterval(refreshModels, 300000)

    return () => {
      clearInterval(statusInterval)
      clearInterval(accountsInterval)
      clearInterval(usageInterval)
      clearInterval(modelsInterval)
    }
  }, [refreshStatus, refreshAccounts, refreshUsageData, refreshModels])

  // ---------------------------------------------------------------------------
  // Install backend
  // ---------------------------------------------------------------------------

  const handleInstallBackend = useCallback(async () => {
    setIsInstalling(true)
    setInstallError(null)
    setInstallProgress(null)
    try {
      await invoke('install_ai_proxy_backend')
      await refreshStatus()
    } catch (error) {
      setInstallError(String(error))
    } finally {
      setIsInstalling(false)
    }
  }, [refreshStatus])

  // ---------------------------------------------------------------------------
  // Start / stop proxy
  // ---------------------------------------------------------------------------

  const handleToggleProxy = useCallback(async () => {
    if (!preferences) return
    setIsToggling(true)
    try {
      const willRun = !proxyStatus?.running
      if (proxyStatus?.running) {
        await invoke('stop_ai_proxy')
        toast.success('AI proxy stopped')
      } else {
        await invoke('start_ai_proxy')
        toast.success('AI proxy started')
      }
      savePreferences.mutate({ ...preferences, ai_proxy_enabled: willRun })
      await refreshStatus()
    } catch (error) {
      toast.error(`Failed: ${error}`)
    } finally {
      setIsToggling(false)
    }
  }, [preferences, proxyStatus?.running, refreshStatus, savePreferences])

  // ---------------------------------------------------------------------------
  // Port handlers
  // ---------------------------------------------------------------------------

  const handlePortBlur = useCallback(
    (field: 'ai_proxy_port' | 'ai_proxy_backend_port', raw: string) => {
      const port = parseInt(raw, 10)
      if (preferences && !isNaN(port) && port >= 1024 && port <= 65535) {
        savePreferences.mutate({ ...preferences, [field]: port })
      } else {
        // Reset
        if (field === 'ai_proxy_port')
          setProxyPortInput(String(preferences?.ai_proxy_port ?? 8317))
        else
          setBackendPortInput(
            String(preferences?.ai_proxy_backend_port ?? 8318)
          )
      }
    },
    [preferences, savePreferences]
  )

  // ---------------------------------------------------------------------------
  // Copy proxy URL
  // ---------------------------------------------------------------------------

  const handleCopyUrl = useCallback(() => {
    const port = proxyStatus?.proxy_port ?? preferences?.ai_proxy_port ?? 8317
    navigator.clipboard.writeText(`http://127.0.0.1:${port}`)
    toast.success('Proxy URL copied')
  }, [proxyStatus?.proxy_port, preferences?.ai_proxy_port])

  // ---------------------------------------------------------------------------
  // Open auth folder
  // ---------------------------------------------------------------------------

  const handleOpenAuthFolder = useCallback(async () => {
    try {
      await invoke('open_ai_proxy_auth_folder')
    } catch (error) {
      toast.error(`Failed: ${error}`)
    }
  }, [])

  // ---------------------------------------------------------------------------
  // Provider login / Z.AI API key
  // ---------------------------------------------------------------------------

  const handleLogin = useCallback(
    async (providerId: string, qwenEmail?: string) => {
      if (loggingInProviders.has(providerId)) return

      if (providerId === 'qwen') {
        const email = qwenEmail?.trim()
        if (!email) {
          toast.error('Qwen login requires an email address')
          return
        }
        qwenEmail = email
      }

      try {
        await invoke('ai_proxy_login', { provider: providerId, qwenEmail })
        toast.info('Waiting for browser authorization...', {
          duration: Infinity,
          id: `login-${providerId}`,
        })
      } catch (error) {
        toast.error(`Login failed: ${error}`)
      }
    },
    [loggingInProviders]
  )

  const handleSaveZaiKey = useCallback(async () => {
    if (!zaiKeyInput.trim()) return
    setZaiKeySaving(true)
    try {
      await invoke('save_zai_api_key', { apiKey: zaiKeyInput.trim() })
      setZaiKeyInput('')
      toast.success('Z.AI API key saved')
      await refreshAccounts()
    } catch (error) {
      toast.error(`Failed: ${error}`)
    } finally {
      setZaiKeySaving(false)
    }
  }, [zaiKeyInput, refreshAccounts])

  const handleToggleAccount = useCallback(
    async (accountId: string, enable: boolean) => {
      try {
        if (enable) {
          await invoke('enable_ai_proxy_account', { accountId })
        } else {
          await invoke('disable_ai_proxy_account', { accountId })
        }
        await refreshAccounts()
      } catch (error) {
        toast.error(`Failed: ${error}`)
      }
    },
    [refreshAccounts]
  )

  const handleDeleteAccount = useCallback(
    async (account: ProviderAccount) => {
      const confirmed = window.confirm(
        `Delete account "${account.display_name}"? This removes stored credentials.`
      )
      if (!confirmed) return

      try {
        await invoke('delete_ai_proxy_account', {
          accountId: account.account_id,
        })
        toast.success('Account deleted')
        await refreshAccounts()
      } catch (error) {
        toast.error(`Failed to delete account: ${error}`)
      }
    },
    [refreshAccounts]
  )

  // ---------------------------------------------------------------------------
  // Model groups CRUD
  // ---------------------------------------------------------------------------

  const handleAddGroup = useCallback(() => {
    setModelGroups(prev => [
      ...prev,
      { name: '', models: [], enabled: true, strategy: 'round_robin' },
    ])
  }, [])

  const handleRemoveGroup = useCallback((index: number) => {
    setModelGroups(prev => prev.filter((_, i) => i !== index))
  }, [])

  const handleUpdateGroup = useCallback(
    (index: number, patch: Partial<ProxyModelGroup>) => {
      setModelGroups(prev =>
        prev.map((g, i) => (i === index ? { ...g, ...patch } : g))
      )
    },
    []
  )

  const handleAddModel = useCallback((groupIndex: number) => {
    setModelGroups(prev =>
      prev.map((g, i) =>
        i === groupIndex
          ? {
              ...g,
              models: [...g.models, { model: '', provider: '', enabled: true }],
            }
          : g
      )
    )
  }, [])

  const handleRemoveModel = useCallback(
    (groupIndex: number, modelIndex: number) => {
      setModelGroups(prev =>
        prev.map((g, i) =>
          i === groupIndex
            ? { ...g, models: g.models.filter((_, mi) => mi !== modelIndex) }
            : g
        )
      )
    },
    []
  )

  const handleUpdateModel = useCallback(
    (
      groupIndex: number,
      modelIndex: number,
      patch: Partial<ProxyModelGroup['models'][number]>
    ) => {
      setModelGroups(prev =>
        prev.map((g, i) =>
          i === groupIndex
            ? {
                ...g,
                models: g.models.map((m, mi) =>
                  mi === modelIndex ? { ...m, ...patch } : m
                ),
              }
            : g
        )
      )
    },
    []
  )

  const handleModelValueChange = useCallback(
    (groupIndex: number, modelIndex: number, model: string) => {
      const uniqueProvider = getUniqueProviderForModel(model, availableModels)
      handleUpdateModel(groupIndex, modelIndex, {
        model,
        provider: uniqueProvider ?? '',
      })
    },
    [availableModels, handleUpdateModel]
  )

  const handleSaveModelGroups = useCallback(async () => {
    if (!preferences) return

    for (const group of modelGroups) {
      for (const member of group.models) {
        if (!member.model.trim() || !member.provider.trim()) {
          toast.error('Each model entry requires both provider and model.')
          return
        }
      }
    }

    try {
      await invoke('update_ai_proxy_model_groups', {
        groups: modelGroups,
      })
      savePreferences.mutate({
        ...preferences,
        ai_proxy_model_groups: modelGroups,
      })
      toast.success('Model groups saved')
    } catch (error) {
      toast.error(`Failed to save: ${error}`)
    }
  }, [preferences, savePreferences, modelGroups])

  // ---------------------------------------------------------------------------
  // Usage reset
  // ---------------------------------------------------------------------------

  const handleResetUsage = useCallback(async () => {
    try {
      await invoke('reset_ai_proxy_usage')
      await refreshUsageData()
      toast.success('Usage stats reset')
    } catch (error) {
      toast.error(`Failed: ${error}`)
    }
  }, [refreshUsageData])

  const modelNameOptions = useMemo(
    () => [...new Set(availableModels.map(model => model.model))],
    [availableModels]
  )

  // ---------------------------------------------------------------------------
  // Non-native guard
  // ---------------------------------------------------------------------------

  if (!isNativeApp()) {
    return (
      <div className="space-y-6">
        <div className="rounded-lg border border-muted p-4">
          <p className="text-sm text-muted-foreground">
            AI Proxy settings are only available in the desktop app.
          </p>
        </div>
      </div>
    )
  }

  const running = proxyStatus?.running ?? false
  const backendInstalled = proxyStatus?.backend_installed ?? false

  return (
    <div className="space-y-6">
      {/* Experimental banner */}
      <div className="rounded-lg border border-yellow-500/20 bg-yellow-500/5 p-4">
        <p className="text-sm text-muted-foreground">
          <strong className="text-yellow-600 dark:text-yellow-400">
            Experimental.
          </strong>{' '}
          Route AI requests through a local proxy that rotates across multiple
          provider accounts. Supports Claude, Codex, GitHub Copilot, Gemini, and
          more.
        </p>
      </div>

      <HelperCard title="When to use AI Proxy">
        <p>
          Use this when you want one local endpoint for many provider accounts.
        </p>
        <p>
          Good for reducing rate-limit interruptions and spreading traffic
          across connected accounts.
        </p>
      </HelperCard>

      {/* Backend installation */}
      {!backendInstalled && (
        <SettingsSection title="Backend">
          <div className="space-y-4">
            {isInstalling ? (
              <div className="space-y-3">
                <div className="flex items-center gap-2 text-sm">
                  <Loader2 className="size-4 animate-spin" />
                  <span>
                    {installProgress?.message ?? 'Preparing installation...'}
                  </span>
                </div>
                <div className="w-full bg-secondary rounded-full h-2">
                  <div
                    className="bg-primary h-2 rounded-full transition-[width] duration-300"
                    style={{ width: `${installProgress?.percent ?? 0}%` }}
                  />
                </div>
              </div>
            ) : installError ? (
              <div className="space-y-3">
                <div className="rounded-md border border-destructive/30 bg-destructive/5 p-3">
                  <p className="text-sm font-medium text-destructive">
                    Installation Failed
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {installError}
                  </p>
                </div>
                <Button onClick={handleInstallBackend} className="w-full">
                  <Download className="mr-1.5 size-4" />
                  Try Again
                </Button>
              </div>
            ) : (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">
                  The AI proxy backend needs to be downloaded before use. This
                  is a one-time download (~44 MB).
                </p>
                <Button onClick={handleInstallBackend} className="w-full">
                  <Download className="mr-1.5 size-4" />
                  Download Backend
                </Button>
              </div>
            )}
          </div>
        </SettingsSection>
      )}

      {/* ------------------------------------------------------------------ */}
      {/* Section 1: Proxy Server                                            */}
      {/* ------------------------------------------------------------------ */}
      <SettingsSection title="Proxy Server">
        <div className="space-y-4">
          {/* Toggle */}
          <InlineField
            label="Enable AI proxy"
            description="Start the local proxy server"
          >
            <div className="flex items-center gap-3">
              <Switch
                checked={running}
                onCheckedChange={handleToggleProxy}
                disabled={isToggling || !backendInstalled}
              />
              <div className="flex items-center gap-1.5">
                <div
                  className={`h-2 w-2 rounded-full ${
                    running ? 'bg-green-500' : 'bg-muted-foreground/40'
                  }`}
                />
                <span className="text-xs text-muted-foreground">
                  {running ? 'Running' : 'Stopped'}
                </span>
              </div>
            </div>
          </InlineField>

          {/* Proxy port */}
          <InlineField
            label="Proxy port"
            description="Port the proxy listens on (1024-65535)"
          >
            <Input
              type="number"
              min={1024}
              max={65535}
              className="w-28"
              value={proxyPortInput}
              onChange={e => setProxyPortInput(e.target.value)}
              onBlur={() => handlePortBlur('ai_proxy_port', proxyPortInput)}
              disabled={running}
            />
          </InlineField>

          {/* Backend port */}
          <InlineField
            label="Backend port"
            description="Port for the backend process (1024-65535)"
          >
            <Input
              type="number"
              min={1024}
              max={65535}
              className="w-28"
              value={backendPortInput}
              onChange={e => setBackendPortInput(e.target.value)}
              onBlur={() =>
                handlePortBlur('ai_proxy_backend_port', backendPortInput)
              }
              disabled={running}
            />
          </InlineField>

          {/* Auto-start */}
          <InlineField
            label="Auto-start"
            description="Start the proxy automatically when Jean launches"
          >
            <Switch
              checked={preferences?.ai_proxy_auto_start ?? false}
              onCheckedChange={checked => {
                if (preferences) {
                  savePreferences.mutate({
                    ...preferences,
                    ai_proxy_auto_start: checked,
                  })
                }
              }}
            />
          </InlineField>

          {/* Copy proxy URL */}
          <InlineField
            label="Proxy URL"
            description={`http://127.0.0.1:${proxyStatus?.proxy_port ?? preferences?.ai_proxy_port ?? 8317}`}
          >
            <Button variant="outline" size="sm" onClick={handleCopyUrl}>
              <Copy className="mr-1.5 h-3.5 w-3.5" />
              Copy URL
            </Button>
          </InlineField>

          {/* Rotation strategy */}
          <InlineField
            label="Rotation strategy"
            description="How requests are distributed across accounts"
          >
            <Select
              value={preferences?.ai_proxy_rotation_strategy ?? 'round_robin'}
              onValueChange={value => {
                if (preferences) {
                  savePreferences.mutate({
                    ...preferences,
                    ai_proxy_rotation_strategy: value as
                      | 'round_robin'
                      | 'fill_first',
                  })
                }
              }}
            >
              <SelectTrigger className="w-40">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="round_robin">Round Robin</SelectItem>
                <SelectItem value="fill_first">Fill First</SelectItem>
              </SelectContent>
            </Select>
          </InlineField>
        </div>
      </SettingsSection>

      {/* ------------------------------------------------------------------ */}
      {/* Section 2: Providers                                               */}
      {/* ------------------------------------------------------------------ */}
      <SettingsSection
        title="Providers"
        action={
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="sm"
              onClick={async () => {
                await Promise.all([refreshAccounts(), refreshUsageData()])
              }}
              className="text-xs text-muted-foreground"
              disabled={isRefreshingUsage}
            >
              <RefreshCw
                className={`mr-1 h-3.5 w-3.5 ${isRefreshingUsage ? 'animate-spin' : ''}`}
              />
              Refresh
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={handleOpenAuthFolder}
              className="text-xs text-muted-foreground"
            >
              <FolderOpen className="mr-1 h-3.5 w-3.5" />
              Open Auth Folder
            </Button>
          </div>
        }
      >
        <div className="space-y-4">
          {PROXY_PROVIDERS.map(provider => {
            const providerAccounts = accounts.filter(
              a => a.provider === provider.id
            )
            return (
              <div
                key={provider.id}
                className="space-y-2 rounded-md border border-muted p-3"
              >
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">{provider.name}</span>
                  {provider.apiKeyOnly ? (
                    <div className="flex items-center gap-2">
                      <Input
                        type="password"
                        placeholder="API key"
                        className="w-48 h-8 text-xs"
                        value={zaiKeyInput}
                        onChange={e => setZaiKeyInput(e.target.value)}
                        onKeyDown={e => {
                          if (e.key === 'Enter') handleSaveZaiKey()
                        }}
                      />
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={handleSaveZaiKey}
                        disabled={!zaiKeyInput.trim() || zaiKeySaving}
                      >
                        <Key className="mr-1 h-3.5 w-3.5" />
                        Add Key
                      </Button>
                    </div>
                  ) : provider.id === 'qwen' ? (
                    <div className="flex items-center gap-2">
                      <Input
                        type="email"
                        placeholder="your.email@example.com"
                        className="w-52 h-8 text-xs"
                        value={qwenEmailInput}
                        onChange={e => setQwenEmailInput(e.target.value)}
                        onKeyDown={e => {
                          if (e.key === 'Enter')
                            handleLogin(provider.id, qwenEmailInput)
                        }}
                      />
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => handleLogin(provider.id, qwenEmailInput)}
                        disabled={
                          loggingInProviders.has(provider.id) ||
                          !qwenEmailInput.trim()
                        }
                      >
                        {loggingInProviders.has(provider.id) ? (
                          <>
                            <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                            Waiting...
                          </>
                        ) : (
                          'Connect Account'
                        )}
                      </Button>
                    </div>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleLogin(provider.id)}
                      disabled={loggingInProviders.has(provider.id)}
                    >
                      {loggingInProviders.has(provider.id) ? (
                        <>
                          <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                          Waiting...
                        </>
                      ) : (
                        'Connect Account'
                      )}
                    </Button>
                  )}
                </div>
                {loginOutput[provider.id] && (
                  <div className="flex items-center gap-2 rounded bg-muted/50 px-3 py-2 text-xs font-mono">
                    <span className="truncate text-muted-foreground">
                      {loginOutput[provider.id]}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-6 w-6 shrink-0"
                      onClick={() => {
                        navigator.clipboard.writeText(
                          loginOutput[provider.id] ?? ''
                        )
                        toast.success('Copied to clipboard')
                      }}
                    >
                      <Copy className="h-3 w-3" />
                    </Button>
                  </div>
                )}
                {providerAccounts.length > 0 && (
                  <div className="space-y-1.5 pl-1">
                    {providerAccounts.map(acc => {
                      const accUsage = accountUsage.find(
                        u => u.account_id === acc.account_id
                      )
                      return (
                        <div key={acc.account_id} className="space-y-1">
                          <div className="flex items-center justify-between text-sm">
                            <span className="text-muted-foreground">
                              {acc.display_name}
                              {acc.expired && (
                                <span className="ml-1.5 text-xs text-red-500">
                                  (expired)
                                </span>
                              )}
                            </span>
                            <div className="flex items-center gap-1">
                              <Switch
                                checked={acc.enabled}
                                onCheckedChange={checked =>
                                  handleToggleAccount(acc.account_id, checked)
                                }
                              />
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7 text-muted-foreground hover:text-red-500"
                                onClick={() => handleDeleteAccount(acc)}
                                title={`Delete ${acc.display_name}`}
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                              </Button>
                            </div>
                          </div>
                          {/* Per-account usage (Claude/Codex only) */}
                          {accUsage && accUsage.status === 'loaded' && (
                            <div className="flex items-center gap-4 pl-2 text-xs">
                              {accUsage.primary_used_percent != null && (
                                <span>
                                  <span
                                    className={usageColor(
                                      accUsage.primary_used_percent
                                    )}
                                  >
                                    {formatPercent(
                                      accUsage.primary_used_percent
                                    )}{' '}
                                    used
                                  </span>
                                  {accUsage.primary_reset_seconds != null && (
                                    <span className="text-muted-foreground ml-1">
                                      (resets{' '}
                                      {formatResetTime(
                                        accUsage.primary_reset_seconds
                                      )}
                                      )
                                    </span>
                                  )}
                                </span>
                              )}
                              {accUsage.secondary_used_percent != null && (
                                <span>
                                  <span
                                    className={usageColor(
                                      accUsage.secondary_used_percent
                                    )}
                                  >
                                    {formatPercent(
                                      accUsage.secondary_used_percent
                                    )}{' '}
                                    weekly used
                                  </span>
                                  {accUsage.secondary_reset_seconds != null && (
                                    <span className="text-muted-foreground ml-1">
                                      (resets{' '}
                                      {formatResetTime(
                                        accUsage.secondary_reset_seconds
                                      )}
                                      )
                                    </span>
                                  )}
                                </span>
                              )}
                              {accUsage.plan_type && (
                                <span className="text-muted-foreground">
                                  {accUsage.plan_type}
                                </span>
                              )}
                            </div>
                          )}
                          {accUsage &&
                            accUsage.status === 'invalid_credentials' && (
                              <div className="pl-2 text-xs text-red-500">
                                Invalid credentials — re-login required
                              </div>
                            )}
                        </div>
                      )
                    })}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </SettingsSection>

      {/* ------------------------------------------------------------------ */}
      {/* Section 3: Model Groups                                            */}
      {/* ------------------------------------------------------------------ */}
      <SettingsSection
        title="Model Groups"
        action={
          <Button
            variant="ghost"
            size="sm"
            onClick={refreshModels}
            className="text-xs text-muted-foreground"
            disabled={isRefreshingModels}
          >
            <RefreshCw
              className={`mr-1 h-3.5 w-3.5 ${isRefreshingModels ? 'animate-spin' : ''}`}
            />
            Refresh Models
          </Button>
        }
      >
        <div className="space-y-4">
          <HelperCard title="How model groups work">
            <p>
              Group name becomes a model alias exposed in{' '}
              <code>/v1/models</code>.
            </p>
            <p>
              Sending requests to that alias routes to one of the models in the
              group using the selected strategy.
            </p>
            <p>
              Use <strong>Round Robin</strong> to rotate fairly, or{' '}
              <strong>Fill First</strong> to use one account/model until it
              throttles.
            </p>
          </HelperCard>

          <div className="rounded-md border border-muted p-3 text-xs text-muted-foreground">
            {modelNameOptions.length > 0 ? (
              <div className="space-y-1">
                <p>
                  Live catalog: {modelNameOptions.length} models across{' '}
                  {new Set(availableModels.map(model => model.provider)).size}{' '}
                  providers.
                </p>
                <p>
                  Selecting a model auto-fills the provider when it is unique.
                </p>
              </div>
            ) : (
              <p>
                No live models available yet. Start the proxy and click Refresh
                Models.
              </p>
            )}
          </div>

          {modelGroups.map((group, gi) => (
            <div
              key={gi}
              className="space-y-3 rounded-md border border-muted p-3"
            >
              <div className="flex items-center gap-2">
                <Input
                  placeholder="Group name"
                  className="flex-1"
                  value={group.name}
                  onChange={e =>
                    handleUpdateGroup(gi, { name: e.target.value })
                  }
                />
                <Select
                  value={group.strategy}
                  onValueChange={v =>
                    handleUpdateGroup(gi, {
                      strategy: v as 'round_robin' | 'fill_first',
                    })
                  }
                >
                  <SelectTrigger className="w-36">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="round_robin">Round Robin</SelectItem>
                    <SelectItem value="fill_first">Fill First</SelectItem>
                  </SelectContent>
                </Select>
                <Switch
                  checked={group.enabled}
                  onCheckedChange={checked =>
                    handleUpdateGroup(gi, { enabled: checked })
                  }
                />
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => handleRemoveGroup(gi)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>

              {/* Models within group */}
              <div className="space-y-2 pl-2">
                {group.models.map((model, mi) => {
                  const providersForModel = [
                    ...new Set(
                      availableModels
                        .filter(entry => entry.model === model.model)
                        .map(entry => entry.provider)
                    ),
                  ]
                  const providerOptions =
                    providersForModel.length > 0
                      ? providersForModel
                      : [
                          ...new Set(
                            availableModels.map(entry => entry.provider)
                          ),
                        ]

                  return (
                    <div key={mi} className="space-y-1">
                      <div className="flex items-center gap-2">
                        <Input
                          placeholder="Model name"
                          className="flex-1"
                          value={model.model}
                          list={`ai-proxy-model-options-${gi}-${mi}`}
                          onChange={e =>
                            handleModelValueChange(gi, mi, e.target.value)
                          }
                        />
                        <datalist id={`ai-proxy-model-options-${gi}-${mi}`}>
                          {modelNameOptions.map(option => (
                            <option
                              key={`${gi}-${mi}-${option}`}
                              value={option}
                            />
                          ))}
                        </datalist>

                        <Input
                          placeholder="Provider"
                          className="w-36"
                          value={model.provider}
                          list={`ai-proxy-provider-options-${gi}-${mi}`}
                          onChange={e =>
                            handleUpdateModel(gi, mi, {
                              provider: e.target.value,
                            })
                          }
                        />
                        <datalist id={`ai-proxy-provider-options-${gi}-${mi}`}>
                          {providerOptions.map(option => (
                            <option
                              key={`${gi}-${mi}-${option}`}
                              value={option}
                            />
                          ))}
                        </datalist>

                        <Switch
                          checked={model.enabled}
                          onCheckedChange={checked =>
                            handleUpdateModel(gi, mi, { enabled: checked })
                          }
                        />
                        <Button
                          variant="ghost"
                          size="icon"
                          onClick={() => handleRemoveModel(gi, mi)}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                      {model.model && providersForModel.length > 0 && (
                        <p className="pl-1 text-[11px] text-muted-foreground">
                          Providers for{' '}
                          <span className="font-mono">{model.model}</span>:{' '}
                          {providersForModel.join(', ')}
                        </p>
                      )}
                    </div>
                  )
                })}

                {modelNameOptions.length > 0 && (
                  <datalist id={`ai-proxy-model-options-${gi}`}>
                    {modelNameOptions.map(option => (
                      <option key={`${gi}-${option}`} value={option} />
                    ))}
                  </datalist>
                )}

                {group.models.length === 0 && (
                  <p className="text-xs text-muted-foreground">
                    Add at least one provider/model pair to enable this group.
                  </p>
                )}

                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => handleAddModel(gi)}
                >
                  <Plus className="mr-1 h-3.5 w-3.5" />
                  Add Model
                </Button>
              </div>
            </div>
          ))}

          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={handleAddGroup}>
              <Plus className="mr-1 h-3.5 w-3.5" />
              Add Group
            </Button>
            <Button size="sm" onClick={handleSaveModelGroups}>
              Save Model Groups
            </Button>
          </div>
        </div>
      </SettingsSection>

      {/* ------------------------------------------------------------------ */}
      {/* Section 4: Usage Popover                                           */}
      {/* ------------------------------------------------------------------ */}
      <SettingsSection title="Usage Popover">
        <div className="space-y-2">
          <p className="text-xs text-muted-foreground">
            Choose which providers appear in the usage popover. If none are
            selected, all providers with data are shown.
          </p>
          <div className="grid grid-cols-2 gap-2">
            {PROXY_PROVIDERS.map(provider => {
              const visible = preferences?.ai_proxy_visible_providers ?? []
              const isChecked =
                visible.length === 0 || visible.includes(provider.id)

              return (
                <label
                  key={provider.id}
                  className="flex items-center gap-2 rounded-md border border-muted px-3 py-2 text-sm hover:bg-muted/50 cursor-pointer"
                >
                  <input
                    type="checkbox"
                    className="accent-foreground"
                    checked={isChecked}
                    onChange={e => {
                      if (!preferences) return
                      const current = [
                        ...(preferences.ai_proxy_visible_providers ?? []),
                      ]
                      if (e.target.checked) {
                        // If currently filtering and adding back, add to list
                        // If all were shown (empty) and unchecking one, switch to explicit: all minus this one
                        if (current.length === 0) {
                          // Was "show all" — now explicitly select all except this one
                          // Actually, checking means we want it, so no-op for empty → checked
                          return
                        }
                        if (!current.includes(provider.id)) {
                          current.push(provider.id)
                        }
                      } else {
                        if (current.length === 0) {
                          // "Show all" → uncheck one = select all except this one
                          const allExceptThis = PROXY_PROVIDERS.map(
                            p => p.id
                          ).filter(id => id !== provider.id)
                          savePreferences.mutate({
                            ...preferences,
                            ai_proxy_visible_providers: allExceptThis,
                          })
                          return
                        }
                        const idx = current.indexOf(provider.id)
                        if (idx !== -1) current.splice(idx, 1)
                      }
                      // If all are selected, store empty (= show all)
                      const allSelected =
                        current.length >= PROXY_PROVIDERS.length
                      savePreferences.mutate({
                        ...preferences,
                        ai_proxy_visible_providers: allSelected ? [] : current,
                      })
                    }}
                  />
                  <span>{provider.name}</span>
                </label>
              )
            })}
          </div>
        </div>
      </SettingsSection>

      {/* ------------------------------------------------------------------ */}
      {/* Section 5: Usage Stats                                             */}
      {/* ------------------------------------------------------------------ */}
      <SettingsSection title="Usage Stats">
        <div className="space-y-4">
          <InlineField label="Total requests">
            <span className="text-sm font-mono">
              {usage?.total_requests ?? 0}
            </span>
          </InlineField>

          {/* Per-provider table */}
          {usage && Object.keys(usage.per_provider).length > 0 && (
            <div className="space-y-1.5">
              <Label className="text-sm text-muted-foreground">
                By Provider
              </Label>
              <div className="rounded-md border border-muted">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-muted">
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Provider
                      </th>
                      <th className="px-3 py-1.5 text-right font-medium text-muted-foreground">
                        Requests
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {Object.entries(usage.per_provider)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([k, v]) => (
                        <tr
                          key={k}
                          className="border-b border-muted last:border-0"
                        >
                          <td className="px-3 py-1.5">{k}</td>
                          <td className="px-3 py-1.5 text-right font-mono">
                            {v}
                          </td>
                        </tr>
                      ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* Per-model table */}
          {usage && Object.keys(usage.per_model).length > 0 && (
            <div className="space-y-1.5">
              <Label className="text-sm text-muted-foreground">By Model</Label>
              <div className="rounded-md border border-muted">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-muted">
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Model
                      </th>
                      <th className="px-3 py-1.5 text-right font-medium text-muted-foreground">
                        Requests
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {Object.entries(usage.per_model)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([k, v]) => (
                        <tr
                          key={k}
                          className="border-b border-muted last:border-0"
                        >
                          <td className="px-3 py-1.5">{k}</td>
                          <td className="px-3 py-1.5 text-right font-mono">
                            {v}
                          </td>
                        </tr>
                      ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* Per-account table */}
          {usage && Object.keys(usage.per_account).length > 0 && (
            <div className="space-y-1.5">
              <Label className="text-sm text-muted-foreground">
                By Account
              </Label>
              <div className="rounded-md border border-muted">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-muted">
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Account
                      </th>
                      <th className="px-3 py-1.5 text-right font-medium text-muted-foreground">
                        Requests
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {Object.entries(usage.per_account)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([k, v]) => (
                        <tr
                          key={k}
                          className="border-b border-muted last:border-0"
                        >
                          <td className="px-3 py-1.5">{k}</td>
                          <td className="px-3 py-1.5 text-right font-mono">
                            {v}
                          </td>
                        </tr>
                      ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* Provider/model/account summary */}
          {usage && usage.by_request.length > 0 && (
            <div className="space-y-1.5">
              <Label className="text-sm text-muted-foreground">
                Request Summary
              </Label>
              <div className="rounded-md border border-muted overflow-x-auto">
                <table className="w-full text-sm min-w-[720px]">
                  <thead>
                    <tr className="border-b border-muted">
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Provider
                      </th>
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Model
                      </th>
                      <th className="px-3 py-1.5 text-left font-medium text-muted-foreground">
                        Account
                      </th>
                      <th className="px-3 py-1.5 text-right font-medium text-muted-foreground">
                        Requests
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {usage.by_request.map(row => (
                      <tr
                        key={`${row.provider}:${row.model}:${row.account}`}
                        className="border-b border-muted last:border-0"
                      >
                        <td className="px-3 py-1.5">{row.provider}</td>
                        <td className="px-3 py-1.5">{row.model}</td>
                        <td className="px-3 py-1.5">{row.account}</td>
                        <td className="px-3 py-1.5 text-right font-mono">
                          {row.requests}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={refreshUsageData}>
              <RefreshCw
                className={`mr-1 h-3.5 w-3.5 ${isRefreshingUsage ? 'animate-spin' : ''}`}
              />
              Refresh
            </Button>
            <Button variant="outline" size="sm" onClick={handleResetUsage}>
              Reset Stats
            </Button>
          </div>
        </div>
      </SettingsSection>
    </div>
  )
}
