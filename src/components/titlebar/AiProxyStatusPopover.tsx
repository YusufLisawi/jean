import { useCallback, useEffect, useMemo, useState } from 'react'
import { ChevronRight, CircleGauge, Loader2, RefreshCw, Settings } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  fetchAiProxyAccountUsage,
  fetchAiProxyAccounts,
} from '@/services/ai-proxy'
import { usePreferences } from '@/services/preferences'
import type { NormalizedAccountUsage, ProviderAccount } from '@/types/ai-proxy'
import { PROXY_PROVIDERS } from '@/types/ai-proxy'
import { getProviderIcon } from '@/components/icons/provider-icons'
import { useUIStore } from '@/store/ui-store'
import { cn } from '@/lib/utils'

function formatPercent(value: number | null): string {
  if (value == null) return '—'
  return `${Math.round(value)}%`
}

function clampPercent(value: number | null): number {
  if (value == null || !Number.isFinite(value)) return 0
  return Math.max(0, Math.min(100, value))
}

function formatResetTime(seconds: number | null): string {
  if (seconds == null || seconds <= 0) return 'now'
  const days = Math.floor(seconds / 86_400)
  const hours = Math.floor((seconds % 86_400) / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  return `${minutes}m`
}

/** Returns true if an account has meaningful usage data to display. */
function hasUsageData(u: NormalizedAccountUsage | undefined): boolean {
  if (!u || u.status !== 'loaded') return false
  return u.primary_used_percent != null || u.secondary_used_percent != null
}

function getProviderDisplayName(providerId: string): string {
  return (
    PROXY_PROVIDERS.find(p => p.id === providerId)?.name ?? providerId
  )
}

function UsageBar({
  label,
  usedPercent,
  resetSeconds,
  highlight,
}: {
  label: string
  usedPercent: number | null
  resetSeconds: number | null
  highlight?: boolean
}) {
  if (usedPercent == null) return null

  const used = clampPercent(usedPercent)

  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-1 text-[11px] font-medium text-foreground/90">
        <span>{label}</span>
        {highlight && (
          <span className="inline-block size-1.5 rounded-full bg-yellow-400" />
        )}
      </div>
      <div className="h-2 overflow-hidden rounded-full bg-muted/60">
        <div
          className={cn(
            'h-full rounded-full transition-[width]',
            highlight ? 'bg-yellow-400' : 'bg-foreground/80'
          )}
          style={{ width: `${used}%` }}
        />
      </div>
      <div className="flex items-center justify-between text-[11px] text-muted-foreground">
        <span>{formatPercent(usedPercent)}</span>
        {resetSeconds != null && (
          <span>Resets in {formatResetTime(resetSeconds)}</span>
        )}
      </div>
    </div>
  )
}

interface ProviderGroup {
  providerId: string
  displayName: string
  accounts: {
    account: ProviderAccount
    usage: NormalizedAccountUsage
  }[]
}

export function AiProxyStatusPopover() {
  const [open, setOpen] = useState(false)
  const [isRefreshing, setIsRefreshing] = useState(false)
  const [accounts, setAccounts] = useState<ProviderAccount[]>([])
  const [usage, setUsage] = useState<NormalizedAccountUsage[]>([])
  const { data: preferences } = usePreferences()
  const visibleProviders = preferences?.ai_proxy_visible_providers ?? []

  const usageByAccount = useMemo(
    () =>
      new Map<string, NormalizedAccountUsage>(
        usage.map(entry => [entry.account_id, entry])
      ),
    [usage]
  )

  /** Group accounts by provider, only including accounts with usage data. */
  const providerGroups = useMemo<ProviderGroup[]>(() => {
    const connected = accounts.filter(a => a.enabled && !a.expired)
    const grouped = new Map<string, ProviderGroup>()

    for (const account of connected) {
      const u = usageByAccount.get(account.account_id)
      if (!hasUsageData(u)) continue
      // Filter by visible providers preference (empty = show all)
      if (visibleProviders.length > 0 && !visibleProviders.includes(account.provider)) continue

      let group = grouped.get(account.provider)
      if (!group) {
        group = {
          providerId: account.provider,
          displayName: getProviderDisplayName(account.provider),
          accounts: [],
        }
        grouped.set(account.provider, group)
      }
      // u is guaranteed non-null by hasUsageData check above
      group.accounts.push({ account, usage: u as NormalizedAccountUsage })
    }

    return [...grouped.values()]
  }, [accounts, usageByAccount, visibleProviders])

  const totalAccountsWithData = providerGroups.reduce(
    (sum, g) => sum + g.accounts.length,
    0
  )

  const refresh = useCallback(async () => {
    setIsRefreshing(true)
    try {
      const [nextAccounts, nextUsage] = await Promise.all([
        fetchAiProxyAccounts(),
        fetchAiProxyAccountUsage(),
      ])
      setAccounts(nextAccounts)
      setUsage(nextUsage)
    } finally {
      setIsRefreshing(false)
    }
  }, [])

  useEffect(() => {
    if (!open) return
    refresh()
  }, [open, refresh])

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-7 w-7 rounded-full text-muted-foreground hover:text-foreground"
          aria-label="Usage"
        >
          <CircleGauge className="size-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-96 space-y-3 p-3">
        <div className="flex items-center justify-between">
          <div>
            <p className="text-xs font-medium text-foreground">Usage</p>
            <p className="text-[11px] text-muted-foreground">
              {totalAccountsWithData} account
              {totalAccountsWithData === 1 ? '' : 's'} with usage data
            </p>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={refresh}
              disabled={isRefreshing}
              aria-label="Refresh"
            >
              {isRefreshing ? (
                <Loader2 className="mr-1 h-3 w-3 animate-spin" />
              ) : (
                <RefreshCw className="mr-1 h-3 w-3" />
              )}
              Refresh
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7"
              onClick={() => useUIStore.getState().openPreferencesPane('ai-proxy')}
              aria-label="AI Proxy Settings"
            >
              <Settings className="size-3.5" />
            </Button>
          </div>
        </div>

        <div className="max-h-[420px] space-y-1.5 overflow-auto">
          {providerGroups.length === 0 ? (
            <p className="rounded-md border border-muted px-2 py-3 text-center text-xs text-muted-foreground">
              No usage data available yet.
            </p>
          ) : (
            providerGroups.map(group => (
              <Collapsible key={group.providerId} defaultOpen>
                <CollapsibleTrigger className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs font-medium text-foreground hover:bg-muted/50">
                  <ChevronRight className="size-3 shrink-0 transition-transform duration-200 [[data-state=open]>&]:rotate-90" />
                  {(() => {
                    const Icon = getProviderIcon(group.providerId)
                    return Icon ? <Icon className="size-3.5 shrink-0" /> : null
                  })()}
                  <span>{group.displayName}</span>
                  <span className="text-muted-foreground font-normal">
                    ({group.accounts.length})
                  </span>
                </CollapsibleTrigger>
                <CollapsibleContent>
                  <div className="space-y-2 pb-1 pl-5 pr-1 pt-1">
                    {group.accounts.map(({ account, usage: u }) => (
                      <div key={account.account_id} className="space-y-2">
                        <p className="truncate text-[11px] text-muted-foreground">
                          {account.display_name}
                        </p>
                        <UsageBar
                          label="Session"
                          usedPercent={u.primary_used_percent}
                          resetSeconds={u.primary_reset_seconds}
                        />
                        <UsageBar
                          label="Weekly"
                          usedPercent={u.secondary_used_percent}
                          resetSeconds={u.secondary_reset_seconds}
                          highlight
                        />
                      </div>
                    ))}
                  </div>
                </CollapsibleContent>
              </Collapsible>
            ))
          )}
        </div>
      </PopoverContent>
    </Popover>
  )
}

export default AiProxyStatusPopover
