import { describe, expect, it, vi, beforeEach } from 'vitest'
import { invoke } from '@/lib/transport'
import {
  computeLeftPercent,
  fetchAiProxyModels,
  getUniqueProviderForModel,
  normalizeAccountUsage,
  normalizeModelsResponse,
} from './ai-proxy'
import type { AccountUsage } from '@/types/ai-proxy'

vi.mock('@/lib/transport', () => ({
  invoke: vi.fn(),
}))

describe('ai-proxy service', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  describe('normalizeModelsResponse', () => {
    it('normalizes v1/models OpenAI shape and infers provider from owned_by', () => {
      const models = normalizeModelsResponse({
        data: [
          { id: 'claude-sonnet-4-5', owned_by: 'claude' },
          { id: 'gpt-5-mini', owned_by: 'codex' },
        ],
      })

      expect(models).toEqual([
        {
          id: 'claude/claude-sonnet-4-5',
          model: 'claude-sonnet-4-5',
          provider: 'claude',
        },
        {
          id: 'codex/gpt-5-mini',
          model: 'gpt-5-mini',
          provider: 'codex',
        },
      ])
    })

    it('supports provider/model pairs and de-duplicates entries', () => {
      const models = normalizeModelsResponse([
        { model: 'claude-sonnet-4-5', provider: 'claude' },
        { id: 'claude/claude-sonnet-4-5' },
        { id: 'codex/gpt-5' },
      ])

      expect(models).toEqual([
        {
          id: 'claude/claude-sonnet-4-5',
          model: 'claude-sonnet-4-5',
          provider: 'claude',
        },
        {
          id: 'codex/gpt-5',
          model: 'gpt-5',
          provider: 'codex',
        },
      ])
    })
  })

  describe('getUniqueProviderForModel', () => {
    it('returns provider when model exists in exactly one provider', () => {
      const provider = getUniqueProviderForModel('gpt-5', [
        { id: 'codex/gpt-5', model: 'gpt-5', provider: 'codex' },
        {
          id: 'claude/claude-sonnet-4-5',
          model: 'claude-sonnet-4-5',
          provider: 'claude',
        },
      ])

      expect(provider).toBe('codex')
    })

    it('returns null when multiple providers expose the same model', () => {
      const provider = getUniqueProviderForModel('gpt-5', [
        { id: 'codex/gpt-5', model: 'gpt-5', provider: 'codex' },
        {
          id: 'github-copilot/gpt-5',
          model: 'gpt-5',
          provider: 'github-copilot',
        },
      ])

      expect(provider).toBeNull()
    })
  })

  describe('usage normalization', () => {
    it('computes left percent from used percent and clamps values', () => {
      expect(computeLeftPercent(63.4)).toBeCloseTo(36.6)
      expect(computeLeftPercent(120)).toBe(0)
      expect(computeLeftPercent(-10)).toBe(100)
      expect(computeLeftPercent(null)).toBeNull()
    })

    it('adds left-percent fields to account usage rows', () => {
      const usage: AccountUsage[] = [
        {
          account_id: 'a1',
          provider: 'claude',
          primary_used_percent: 70,
          primary_reset_seconds: 100,
          secondary_used_percent: 40,
          secondary_reset_seconds: 200,
          plan_type: 'pro',
          status: 'loaded',
        },
      ]

      expect(normalizeAccountUsage(usage)).toEqual([
        {
          account_id: 'a1',
          provider: 'claude',
          primary_used_percent: 70,
          primary_left_percent: 30,
          primary_reset_seconds: 100,
          secondary_used_percent: 40,
          secondary_left_percent: 60,
          secondary_reset_seconds: 200,
          plan_type: 'pro',
          status: 'loaded',
        },
      ])
    })
  })

  describe('fetchAiProxyModels', () => {
    it('returns empty list instead of throwing when command is unavailable', async () => {
      vi.mocked(invoke).mockRejectedValueOnce(new Error('unknown command'))

      await expect(fetchAiProxyModels()).resolves.toEqual([])
    })
  })
})
