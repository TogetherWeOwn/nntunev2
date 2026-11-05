import { test, expect } from '@playwright/test';
import {
  PROVIDER_BUDGET_DEFAULT,
  PROVIDER_BUDGET_ENV,
  PROVIDER_BUDGET_HARD_MAX,
  isNntpCode,
  mintProbeMessageId,
  readProviderBudget,
} from '../helpers/provider-budget.js';

// Offline pins for the staging provider connection budget (the 430 control
// leg reads this; the control itself needs the live provider and is pinned
// only structurally in tests/workflows.spec.ts). Pure and offline: no
// network, no secrets, no NZB content anywhere here.

test.describe('provider connection budget (offline pins)', () => {
  test('env name, default and hard max are the documented values', () => {
    expect(PROVIDER_BUDGET_ENV).toBe('E2E_NNTP_MAX_CONNECTIONS');
    expect(PROVIDER_BUDGET_DEFAULT).toBe(2);
    expect(PROVIDER_BUDGET_HARD_MAX).toBe(10);
  });

  test('unset budget resolves the 2-connection default, unclamped', () => {
    expect(readProviderBudget({})).toEqual({
      maxConnections: 2,
      requested: null,
      clamped: false,
    });
  });

  test('values 1..10 pass through unclamped', () => {
    for (const n of [1, 2, 10]) {
      expect(readProviderBudget({ [PROVIDER_BUDGET_ENV]: String(n) })).toEqual({
        maxConnections: n,
        requested: n,
        clamped: false,
      });
    }
  });

  test('above the hard max clamps to 10 and reports the request', () => {
    expect(readProviderBudget({ [PROVIDER_BUDGET_ENV]: '50' })).toEqual({
      maxConnections: 10,
      requested: 50,
      clamped: true,
    });
  });

  test('non-numeric, blank, zero and negative values fall back to the default', () => {
    for (const raw of ['', '   ', 'many', '2.5', '0', '-3']) {
      const budget = readProviderBudget({ [PROVIDER_BUDGET_ENV]: raw });
      expect(budget.maxConnections, `raw ${JSON.stringify(raw)}`).toBe(2);
      expect(budget.clamped, `raw ${JSON.stringify(raw)}`).toBe(true);
    }
  });

  test(' whitespace-padded integers still parse', () => {
    expect(readProviderBudget({ [PROVIDER_BUDGET_ENV]: '  3  ' })).toEqual({
      maxConnections: 3,
      requested: 3,
      clamped: false,
    });
  });

  test('probe message-IDs are unique and never-posted (.invalid)', () => {
    const first = mintProbeMessageId();
    const second = mintProbeMessageId();
    expect(first).not.toBe(second);
    for (const id of [first, second]) {
      expect(id).toMatch(/^<e2e-430-probe-[0-9a-f-]+@nntune\.invalid>$/);
    }
  });

  test('reply-code matcher anchors on the 3-digit code', () => {
    expect(isNntpCode('430 no such article', '430')).toBe(true);
    expect(isNntpCode('281 authentication accepted', '281')).toBe(true);
    expect(isNntpCode('502 too many connections', '502')).toBe(true);
    expect(isNntpCode('430 no such article', '43')).toBe(false);
    expect(isNntpCode('1430 wrapped', '430')).toBe(false);
    expect(isNntpCode('', '430')).toBe(false);
  });
});
