import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'

const mockUsePreferences = vi.fn()

const projectsState = {
  selectedProjectId: null as string | null,
  selectedWorktreeId: null as string | null,
  setAddProjectDialogOpen: vi.fn(),
}

const chatState = {
  activeWorktreePath: undefined as string | undefined,
  viewingCanvasTab: {} as Record<string, boolean>,
}

const uiState = {
  setGitHubDashboardOpen: vi.fn(),
  setCommandPaletteOpen: vi.fn(),
}

const useProjectsStoreMock = Object.assign(
  (selector?: (state: typeof projectsState) => unknown) =>
    selector ? selector(projectsState) : projectsState,
  {
    getState: () => projectsState,
  }
)

const useChatStoreMock = Object.assign(
  (selector?: (state: typeof chatState) => unknown) =>
    selector ? selector(chatState) : chatState,
  {
    getState: () => chatState,
  }
)

const useUIStoreMock = Object.assign(
  (selector?: (state: typeof uiState) => unknown) =>
    selector ? selector(uiState) : uiState,
  {
    getState: () => uiState,
  }
)

vi.mock('@/services/preferences', () => ({
  usePreferences: () => mockUsePreferences(),
}))

vi.mock('@/store/projects-store', () => ({
  useProjectsStore: useProjectsStoreMock,
}))

vi.mock('@/store/chat-store', () => ({
  useChatStore: useChatStoreMock,
}))

vi.mock('@/store/ui-store', () => ({
  useUIStore: useUIStoreMock,
}))

vi.mock('@/components/titlebar/AiProxyStatusPopover', () => ({
  AiProxyStatusPopover: () => (
    <button type="button" aria-label="AI Proxy Status">
      AI Proxy
    </button>
  ),
}))

describe('FloatingDock', () => {
  it('shows AI proxy status icon only when ai_proxy_enabled is true', async () => {
    const { FloatingDock } = await import('./floating-dock')

    mockUsePreferences.mockReturnValue({
      data: {
        ai_proxy_enabled: true,
        show_keybinding_hints: false,
        keybindings: {},
      },
    })

    const { rerender } = render(<FloatingDock />)
    expect(
      screen.getByRole('button', { name: /ai proxy status/i })
    ).toBeInTheDocument()

    mockUsePreferences.mockReturnValue({
      data: {
        ai_proxy_enabled: false,
        show_keybinding_hints: false,
        keybindings: {},
      },
    })

    rerender(<FloatingDock />)
    expect(
      screen.queryByRole('button', { name: /ai proxy status/i })
    ).toBeNull()
  })
})
