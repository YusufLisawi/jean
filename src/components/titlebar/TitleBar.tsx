import type React from 'react'
import { useState, useEffect } from 'react'
import { cn } from '@/lib/utils'
import { MacOSWindowControls } from './MacOSWindowControls'
import { WindowsWindowControls } from './WindowsWindowControls'
import { isMacOS, openExternal } from '@/lib/platform'
import { Button } from '@/components/ui/button'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useUIStore } from '@/store/ui-store'
import { useCommandContext } from '@/lib/commands'
import {
  ArrowUpCircle,
  Github,
  Heart,
  PanelLeft,
  PanelLeftClose,
  Settings,
} from 'lucide-react'
import { usePreferences } from '@/services/preferences'
import { formatShortcutDisplay, DEFAULT_KEYBINDINGS } from '@/types/keybindings'
import { isNativeApp } from '@/lib/environment'
import { UnreadBell } from '@/components/unread/UnreadBell'

interface TitleBarProps {
  className?: string
  title?: string
  hideTitle?: boolean
}

export function TitleBar({
  className,
  title = 'Jean',
  hideTitle = false,
}: TitleBarProps) {
  const { leftSidebarVisible, toggleLeftSidebar } = useUIStore()
  const commandContext = useCommandContext()
  const { data: preferences } = usePreferences()

  const sidebarShortcut = formatShortcutDisplay(
    (preferences?.keybindings?.toggle_left_sidebar ||
      DEFAULT_KEYBINDINGS.toggle_left_sidebar) as string
  )
  const native = isNativeApp()

  const [appVersion, setAppVersion] = useState<string>('')
  useEffect(() => {
    if (native) {
      import('@tauri-apps/api/app').then(({ getVersion }) =>
        getVersion().then(setAppVersion)
      )
    }
  }, [native])

  return (
    <div
      {...(native ? { 'data-tauri-drag-region': true } : {})}
      className={cn(
        'relative flex h-8 w-full shrink-0 items-center justify-between bg-sidebar',
        native && 'z-[60]',
        className
      )}
    >
      {/* Left side - Window Controls + Left Actions */}
      <div
        className="flex items-center"
        style={{ WebkitAppRegion: 'no-drag' } as React.CSSProperties}
      >
        {native && isMacOS && <MacOSWindowControls />}

        {/* Left Action Buttons */}
        <div
          className={cn(
            'flex items-center gap-1',
            (!native || !isMacOS) && 'pl-2'
          )}
        >
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                onClick={toggleLeftSidebar}
                variant="ghost"
                size="icon"
                className="h-6 w-6 text-foreground/70 hover:text-foreground"
              >
                {leftSidebarVisible ? (
                  <PanelLeftClose className="h-3 w-3" />
                ) : (
                  <PanelLeft className="h-3 w-3" />
                )}
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              {leftSidebarVisible ? 'Hide' : 'Show'} Left Sidebar{' '}
              <kbd className="ml-1 text-[0.625rem] opacity-60">
                {sidebarShortcut}
              </kbd>
            </TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                onClick={commandContext.openPreferences}
                variant="ghost"
                size="icon"
                className="h-6 w-6 text-foreground/70 hover:text-foreground"
              >
                <Settings className="h-3 w-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              Settings{' '}
              <kbd className="ml-1 text-[0.625rem] opacity-60">
                {formatShortcutDisplay(
                  (preferences?.keybindings?.open_preferences ||
                    DEFAULT_KEYBINDINGS.open_preferences) as string
                )}
              </kbd>
            </TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                onClick={() => openExternal('https://jean.build/sponsorships/')}
                variant="ghost"
                size="icon"
                className="h-6 w-6 text-pink-500 hover:text-pink-400"
              >
                <Heart className="h-3 w-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Sponsor</TooltipContent>
          </Tooltip>
        </div>
      </div>

      {/* Center - Title / Unread indicator */}
      <div
        className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 max-w-[50%] px-2"
        style={{ WebkitAppRegion: 'no-drag' } as React.CSSProperties}
      >
        <UnreadBell title={title} hideTitle={hideTitle} />
      </div>

      {/* Right side - Version + Windows/Linux window controls */}
      <div
        className="flex items-center"
        style={{ WebkitAppRegion: 'no-drag' } as React.CSSProperties}
      >
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              onClick={() => openExternal('https://github.com/coollabsio/jean')}
              variant="ghost"
              size="icon"
              className="h-6 w-6 text-foreground/70 hover:text-foreground"
            >
              <Github className="h-3 w-3" />
            </Button>
          </TooltipTrigger>
          <TooltipContent>GitHub</TooltipContent>
        </Tooltip>
        {appVersion && <UpdateIndicator />}
        {appVersion && (
          <button
            onClick={() =>
              openExternal(
                `https://github.com/coollabsio/jean/releases/tag/v${appVersion}`
              )
            }
            className="pr-2 text-[0.625rem] text-foreground/40 hover:text-foreground/60 transition-colors cursor-pointer"
          >
            v{appVersion}
          </button>
        )}
        {native && !isMacOS && <WindowsWindowControls />}
      </div>
    </div>
  )
}

function UpdateIndicator() {
  const pendingVersion = useUIStore(state => state.pendingUpdateVersion)
  if (!pendingVersion) return null

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          onClick={() =>
            window.dispatchEvent(new Event('install-pending-update'))
          }
          className="mr-1.5 flex items-center gap-1 rounded-md bg-primary/15 px-1.5 py-0.5 text-[0.625rem] font-medium text-primary hover:bg-primary/25 transition-colors cursor-pointer"
        >
          <ArrowUpCircle className="size-3" />
          Update available
        </button>
      </TooltipTrigger>
      <TooltipContent side="bottom">Update to v{pendingVersion}</TooltipContent>
    </Tooltip>
  )
}

export default TitleBar
