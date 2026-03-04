import { describe, expect, it, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { AiProxyStatusPopover } from './AiProxyStatusPopover'

const mockFetchAccounts = vi.fn()
const mockFetchAccountUsage = vi.fn()

vi.mock('@/services/ai-proxy', () => ({
  fetchAiProxyAccounts: () => mockFetchAccounts(),
  fetchAiProxyAccountUsage: () => mockFetchAccountUsage(),
}))

describe('AiProxyStatusPopover', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mockFetchAccounts.mockResolvedValue([
      {
        provider: 'claude',
        account_id: 'acc-1',
        display_name: 'acc@example.com',
        enabled: true,
        expired: false,
      },
    ])
    mockFetchAccountUsage.mockResolvedValue([
      {
        account_id: 'acc-1',
        provider: 'claude',
        primary_used_percent: 65,
        primary_left_percent: 35,
        primary_reset_seconds: 3600,
        secondary_used_percent: null,
        secondary_left_percent: null,
        secondary_reset_seconds: null,
        plan_type: 'pro',
        status: 'loaded',
      },
    ])
  })

  it('refreshes when opened and allows manual refresh while open', async () => {
    const user = userEvent.setup()
    render(<AiProxyStatusPopover />)

    expect(mockFetchAccounts).not.toHaveBeenCalled()
    expect(mockFetchAccountUsage).not.toHaveBeenCalled()
    expect(screen.queryByRole('button', { name: /refresh/i })).toBeNull()

    await user.click(screen.getByRole('button', { name: /ai proxy status/i }))

    await waitFor(() => {
      expect(mockFetchAccounts).toHaveBeenCalledTimes(1)
      expect(mockFetchAccountUsage).toHaveBeenCalledTimes(1)
    })

    expect(screen.getByText('acc@example.com')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /refresh/i }))

    await waitFor(() => {
      expect(mockFetchAccounts).toHaveBeenCalledTimes(2)
      expect(mockFetchAccountUsage).toHaveBeenCalledTimes(2)
    })
  })
})
