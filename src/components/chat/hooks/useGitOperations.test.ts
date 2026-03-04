import { describe, expect, it } from 'vitest'
import { shouldAutoSaveContextOnCommit } from './useGitOperations'
import { defaultPreferences } from '@/types/preferences'

describe('shouldAutoSaveContextOnCommit', () => {
  it('returns false when the preference is disabled', () => {
    expect(
      shouldAutoSaveContextOnCommit(defaultPreferences, 'abc1234')
    ).toBe(false)
  })

  it('returns false when no commit was created', () => {
    const prefs = {
      ...defaultPreferences,
      auto_save_context_on_commit: true,
    }

    expect(shouldAutoSaveContextOnCommit(prefs, '')).toBe(false)
  })

  it('returns true when enabled and commit hash exists', () => {
    const prefs = {
      ...defaultPreferences,
      auto_save_context_on_commit: true,
    }

    expect(shouldAutoSaveContextOnCommit(prefs, 'abc1234')).toBe(true)
  })
})
