import { Fragment, useState, useEffect, useCallback } from 'react'
import {
  LayoutDashboard,
  Command,
  CircleHelp,
  Menu,
  Plus,
  Archive,
  Terminal,
} from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Kbd } from '@/components/ui/kbd'
import {
  Tooltip,
  TooltipTrigger,
  TooltipContent,
} from '@/components/ui/tooltip'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Popover,
  PopoverTrigger,
  PopoverContent,
} from '@/components/ui/popover'
import { AiProxyStatusPopover } from '@/components/titlebar/AiProxyStatusPopover'
import { useUIStore } from '@/store/ui-store'
import { useChatStore } from '@/store/chat-store'
import { useProjectsStore } from '@/store/projects-store'
import { chatQueryKeys } from '@/services/chat'
import { usePreferences } from '@/services/preferences'
import type { WorktreeSessions } from '@/types/chat'
import { DEFAULT_KEYBINDINGS, formatShortcutDisplay } from '@/types/keybindings'
import type { KeybindingHint } from '@/components/ui/keybinding-hints'
import { getResumeCommand } from '@/components/chat/session-card-utils'

// Canvas-specific hints (same set used in both WorktreeCanvasView and ProjectCanvasView)
const CANVAS_HINTS: KeybindingHint[] = [
  { shortcut: 'Enter', label: 'open' },
  {
    shortcut: DEFAULT_KEYBINDINGS.open_in_modal as string,
    label: 'open in...',
  },
  {
    shortcut: DEFAULT_KEYBINDINGS.new_worktree as string,
    label: 'new worktree',
  },
  { shortcut: DEFAULT_KEYBINDINGS.new_session as string, label: 'new session' },
  {
    shortcut: DEFAULT_KEYBINDINGS.toggle_session_label as string,
    label: 'label',
  },
  { shortcut: DEFAULT_KEYBINDINGS.open_magic_modal as string, label: 'magic' },
  {
    shortcut: DEFAULT_KEYBINDINGS.close_session_or_worktree as string,
    label: 'close',
  },
]

function KeybindingHintsButton({ hints }: { hints: KeybindingHint[] }) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-7 w-7 rounded-full text-muted-foreground hover:text-foreground"
        >
          <CircleHelp className="size-4" />
          <span className="sr-only">Keyboard shortcuts</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="start"
        className="w-auto min-w-[200px] p-3"
      >
        <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 items-center">
          {hints.map(hint => (
            <Fragment key={hint.shortcut}>
              <Kbd className="h-5 px-1.5 text-[11px]">
                {formatShortcutDisplay(hint.shortcut)}
              </Kbd>
              <span className="text-xs text-muted-foreground">
                {hint.label}
              </span>
            </Fragment>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  )
}

export function FloatingDock() {
  const { data: preferences } = usePreferences()
  const queryClient = useQueryClient()

  const selectedProjectId = useProjectsStore(state => state.selectedProjectId)
  const selectedWorktreeId = useProjectsStore(state => state.selectedWorktreeId)
  const activeWorktreePath = useChatStore(state => state.activeWorktreePath)
  const isViewingCanvasTab = useChatStore(state =>
    selectedWorktreeId
      ? (state.viewingCanvasTab?.[selectedWorktreeId] ?? true)
      : false
  )

  // Show hints button on canvas views: worktree canvas OR project-level canvas (no active chat)
  const isCanvasView =
    isViewingCanvasTab || (!!selectedProjectId && !activeWorktreePath)
  const showHints = isCanvasView && preferences?.show_keybinding_hints !== false

  const [menuOpen, setMenuOpen] = useState(false)
  const [resumeCommand, setResumeCommand] = useState<string | null>(null)

  const getActiveResumeCommand = useCallback(() => {
    const { selectedWorktreeId: currentWorktreeId } = useProjectsStore.getState()
    if (!currentWorktreeId) return null

    const activeSessionId = useChatStore.getState().activeSessionIds[currentWorktreeId]
    if (!activeSessionId) return null

    const cached =
      queryClient.getQueryData<WorktreeSessions>(
        chatQueryKeys.sessions(currentWorktreeId)
      ) ??
      queryClient.getQueryData<WorktreeSessions>([
        ...chatQueryKeys.sessions(currentWorktreeId),
        'with-counts',
      ])
    const session = cached?.sessions.find(s => s.id === activeSessionId)
    return session ? getResumeCommand(session) : null
  }, [queryClient])

  const handleQuickMenuOpenChange = useCallback(
    (open: boolean) => {
      setMenuOpen(open)
      if (open) {
        setResumeCommand(getActiveResumeCommand())
      }
    },
    [getActiveResumeCommand]
  )

  const toggleMenu = useCallback(() => {
    setMenuOpen(prev => {
      const next = !prev
      if (next) {
        setResumeCommand(getActiveResumeCommand())
      }
      return next
    })
  }, [getActiveResumeCommand])

  const handleCopyResumeCommand = useCallback(() => {
    const commandToCopy = getActiveResumeCommand() ?? resumeCommand
    if (!commandToCopy) return
    void navigator.clipboard
      .writeText(commandToCopy)
      .then(() => toast.success('Resume command copied'))
      .catch(() => toast.error('Failed to copy resume command'))
  }, [getActiveResumeCommand, resumeCommand])

  // Listen for keyboard shortcut event
  useEffect(() => {
    const handler = () => toggleMenu()
    window.addEventListener('toggle-quick-menu', handler)
    return () => window.removeEventListener('toggle-quick-menu', handler)
  }, [toggleMenu])

  const githubShortcut = formatShortcutDisplay(
    (preferences?.keybindings?.open_github_dashboard ??
      DEFAULT_KEYBINDINGS.open_github_dashboard) as string
  )

  const menuShortcut = formatShortcutDisplay(
    (preferences?.keybindings?.open_quick_menu ??
      DEFAULT_KEYBINDINGS.open_quick_menu) as string
  )

  return (
    <div className="absolute bottom-4 left-4 z-10 hidden sm:flex items-center gap-0.5 rounded-full border border-border/30 bg-background/60 backdrop-blur-md px-1 py-0.5">
      <DropdownMenu open={menuOpen} onOpenChange={handleQuickMenuOpenChange}>
        <Tooltip>
          <TooltipTrigger asChild>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7 rounded-full text-muted-foreground hover:text-foreground"
              >
                <Menu className="size-4" />
                <span className="sr-only">Quick menu</span>
              </Button>
            </DropdownMenuTrigger>
          </TooltipTrigger>
          <TooltipContent side="top">
            Menu{' '}
            <kbd className="ml-1 text-[0.625rem] opacity-60">
              {menuShortcut}
            </kbd>
          </TooltipContent>
        </Tooltip>
        <DropdownMenuContent
          side="top"
          align="start"
          className="min-w-[200px]"
          onEscapeKeyDown={e => e.stopPropagation()}
        >
          <DropdownMenuItem
            onClick={() =>
              useProjectsStore.getState().setAddProjectDialogOpen(true)
            }
          >
            <Plus className="mr-2 h-4 w-4" />
            Add Project
          </DropdownMenuItem>
          <DropdownMenuItem
            onClick={() =>
              window.dispatchEvent(
                new CustomEvent('command:open-archived-modal')
              )
            }
          >
            <Archive className="mr-2 h-4 w-4" />
            Archives
          </DropdownMenuItem>
          <DropdownMenuItem
            onClick={() => useUIStore.getState().setGitHubDashboardOpen(true)}
          >
            <LayoutDashboard className="mr-2 h-4 w-4" />
            GitHub Dashboard
            <DropdownMenuShortcut>{githubShortcut}</DropdownMenuShortcut>
          </DropdownMenuItem>
          {resumeCommand && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={handleCopyResumeCommand}>
                <Terminal className="mr-2 h-4 w-4" />
                Copy Resume Command
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7 rounded-full text-muted-foreground hover:text-foreground"
            onClick={() => useUIStore.getState().setCommandPaletteOpen(true)}
          >
            <Command className="size-4" />
            <span className="sr-only">Command Palette</span>
          </Button>
        </TooltipTrigger>
        <TooltipContent side="top">
          Command Palette{' '}
          <kbd className="ml-1 text-[0.625rem] opacity-60">⌘K</kbd>
        </TooltipContent>
      </Tooltip>

      {preferences?.ai_proxy_enabled && <AiProxyStatusPopover />}

      {showHints && <KeybindingHintsButton hints={CANVAS_HINTS} />}
    </div>
  )
}
