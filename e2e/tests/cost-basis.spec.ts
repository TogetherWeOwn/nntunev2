import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

// Cost-basis worksheet arithmetic pins (offline, no staging).
//
// `docs/cost-basis.md` §1–§7 define the Cloudflare Workers / R2 / KV /
// Queues / Analytics Engine / Workers Logs / PlanetScale formulas and the
// §7 total (Workers Logs lives in §5b so §6–§8 keep their numbers); §8
// works them through with invented illustrative volumes. Nothing on
// staging is touched: these tests re-encode each formula in the helpers
// below and assert the §8 row totals plus the `max(0, x)` / `ceil(x)`
// boundary behavior at every included allowance.
//
// The spec also reads the worksheet text and asserts it still carries the
// formulas, §8 rows and totals the helpers encode, so a worksheet edit
// that changes a price, formula or §8 figure breaks loudly instead of
// passing against a stale local copy. Expected §8 numbers live once in
// `section8ExpectedTotals` below and every illustrative-total assertion
// reads from it.
//
// This pins the illustrative arithmetic, not the measured bill: the §10
// first measured staging bill rows are evidence, not formula outputs, and
// are deliberately not asserted here (see the last describe block, which
// enforces the §8 fixture scope). Only this file pins the worksheet; no
// worksheet numbers, prices, formulas or scope statements are changed by it.
//
// If the worksheet gains a machine-readable formula source later, prefer
// importing it over this re-encoding (follow-up, not this card).

const clamp = (x: number): number => Math.max(0, x);

// §1: workers = 5.00 + max(0, R/1e6 - 10) * 0.30 + max(0, C/1e6 - 30) * 0.02
function workersCost(requests: number, cpuMs: number): number {
  return 5.0 + clamp(requests / 1e6 - 10) * 0.3 + clamp(cpuMs / 1e6 - 30) * 0.02;
}

// §2: r2 = max(0, ceil(G) - 10) * 0.015 + max(0, ceil(A/1e6) - 1) * 4.50
//        + max(0, ceil(B/1e6) - 10) * 0.36
function r2Cost(gbMonths: number, classA: number, classB: number): number {
  return (
    clamp(Math.ceil(gbMonths) - 10) * 0.015 +
    clamp(Math.ceil(classA / 1e6) - 1) * 4.5 +
    clamp(Math.ceil(classB / 1e6) - 10) * 0.36
  );
}

// §3: kv = max(0, KR/1e6 - 10) * 0.50 + max(0, KW/1e6 - 1) * 5.00
//        + max(0, KD/1e6 - 1) * 5.00 + max(0, KL/1e6 - 1) * 5.00
//        + max(0, KG - 1) * 0.50
function kvCost(
  reads: number,
  writes: number,
  deletes: number,
  lists: number,
  storedGb: number,
): number {
  return (
    clamp(reads / 1e6 - 10) * 0.5 +
    clamp(writes / 1e6 - 1) * 5.0 +
    clamp(deletes / 1e6 - 1) * 5.0 +
    clamp(lists / 1e6 - 1) * 5.0 +
    clamp(storedGb - 1) * 0.5
  );
}

// §4: queues = max(0, Ops/1e6 - 1) * 0.40, where Ops ≈ 3 * M * chunks
// for the single-delivery, no-retry estimate.
function queuesOpsEstimate(messages: number, chunksPerMessage = 1): number {
  return 3 * messages * chunksPerMessage;
}

function queuesCost(operations: number): number {
  return clamp(operations / 1e6 - 1) * 0.4;
}

// §5 (prospective, excluded from the §7 current total):
// analytics_engine_prospective = max(0, W/1e6 - 10) * 0.25 + max(0, Q/1e6 - 1) * 1.00
function analyticsEngineProspective(dataPointsWritten: number, readQueries: number): number {
  return clamp(dataPointsWritten / 1e6 - 10) * 0.25 + clamp(readQueries / 1e6 - 1) * 1.0;
}

// §5b (prospective, excluded from the §7 current total):
// workers_logs_prospective = max(0, L/1e6 - 20) * 0.60
function workersLogsProspective(logEventsWritten: number): number {
  return clamp(logEventsWritten / 1e6 - 20) * 0.6;
}

// §6 per cluster: SKU + max(0, D - 10) * 0.125 * N + D * 0.125 * X
// + max(0, Bak - 2*D) * 0.023 + max(0, E - EA) * 0.06
function planetscaleClusterCost(args: {
  sku: number;
  diskGb: number;
  includedInstances: number;
  extraReplicas: number;
  backupGbMonths: number;
  egressGb: number;
  egressAllowanceGb: number;
}): number {
  const { sku, diskGb, includedInstances, extraReplicas, backupGbMonths, egressGb, egressAllowanceGb } = args;
  return (
    sku +
    clamp(diskGb - 10) * 0.125 * includedInstances +
    diskGb * 0.125 * extraReplicas +
    clamp(backupGbMonths - 2 * diskGb) * 0.023 +
    clamp(egressGb - egressAllowanceGb) * 0.06
  );
}

// Single source of truth for every §8 illustrative total this file asserts.
// Each value is read by its per-service / total test below; the last
// describe block enforces that this fixture covers §8 services only.
const section8ExpectedTotals = {
  workers: 6,
  r2: 15.45,
  kv: 3,
  queues: 2,
  analyticsEngineProspective: 0.5,
  workersLogsProspective: 3,
  planetscaleCompute: 10,
  planetscaleStorage: 13.75,
  planetscaleEgress: 6.6,
  totalCurrent: 56.8,
  totalWithProspectiveAe: 57.3,
  totalWithAllProspectives: 60.3,
};

// Worksheet text the helpers must stay in sync with. Read with the same
// `new URL(..., import.meta.url)` pattern as `workflows.spec.ts` so the
// spec fails when the worksheet drifts, not just when the local copy does.
const worksheetDoc = readFileSync(new URL('../../docs/cost-basis.md', import.meta.url), 'utf8');
const worksheetNorm = worksheetDoc.replace(/\s+/g, ' ');

test.describe('Worksheet drift guard (docs/cost-basis.md)', () => {
  test('§1–§7 fenced formulas match the encoded helpers', () => {
    expect(worksheetNorm).toContain(
      'workers = 5.00 + max(0, R/1e6 - 10) * 0.30 + max(0, C/1e6 - 30) * 0.02',
    );
    expect(worksheetNorm).toContain(
      'r2 = max(0, ceil(G) - 10) * 0.015 + max(0, ceil(A/1e6) - 1) * 4.50 + max(0, ceil(B/1e6) - 10) * 0.36',
    );
    expect(worksheetNorm).toContain(
      'kv = max(0, KR/1e6 - 10) * 0.50 + max(0, KW/1e6 - 1) * 5.00 + max(0, KD/1e6 - 1) * 5.00 + max(0, KL/1e6 - 1) * 5.00 + max(0, KG - 1) * 0.50',
    );
    expect(worksheetNorm).toContain(
      'queues = max(0, Ops/1e6 - 1) * 0.40 where Ops ≈ 3 * M * chunks',
    );
    expect(worksheetNorm).toContain(
      'analytics_engine_prospective = max(0, W/1e6 - 10) * 0.25 + max(0, Q/1e6 - 1) * 1.00',
    );
    expect(worksheetNorm).toContain(
      'workers_logs_prospective = max(0, L/1e6 - 20) * 0.60',
    );
    expect(worksheetNorm).toContain('planetscale_cluster = SKU');
    expect(worksheetNorm).toContain('max(0, D - 10) * 0.125 * N');
    expect(worksheetNorm).toContain('D * 0.125 * X');
    expect(worksheetNorm).toContain('max(0, Bak - 2*D) * 0.023');
    expect(worksheetNorm).toContain('max(0, E - EA) * 0.06');
    expect(worksheetNorm).toContain(
      'monthly_total_current = workers + r2 + kv + queues + sum(planetscale_cluster over app + MusicBrainz clusters)',
    );
    expect(worksheetNorm).toContain(
      'monthly_total_with_prospective_ae = monthly_total_current + analytics_engine_prospective',
    );
    expect(worksheetNorm).toContain(
      'monthly_total_with_all_prospectives = monthly_total_with_prospective_ae + workers_logs_prospective',
    );
  });

  test('§8 illustrative rows still carry their $ amounts', () => {
    expect(worksheetDoc).toMatch(/\|\s*Workers\s*\|[^|]*\|\s*6\.00\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*R2\s*\|[^|]*\|\s*15\.45\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*KV\s*\|[^|]*\|\s*3\.00\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*Queues\s*\|[^|]*\|\s*2\.00\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*Analytics Engine[^\n|]*\|[^|]*\|\s*0\.50\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*Workers Logs[^\n|]*\|[^|]*\|\s*3\.00\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*PlanetScale compute\s*\|[^|]*\|\s*10\.00\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*PlanetScale storage\s*\|[^|]*\|\s*13\.75\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*PlanetScale egress\s*\|[^|]*\|\s*6\.60\s*\|/);
  });

  test('§8 totals still read 56.80 / 57.30 / 60.30', () => {
    expect(worksheetDoc).toMatch(/\*\*56\.80\*\*/);
    expect(worksheetDoc).toMatch(/\|\s*Total including prospective Analytics Engine\s*\|[^|]*\|\s*57\.30\s*\|/);
    expect(worksheetDoc).toMatch(/\|\s*Total including all prospectives[^\n|]*\|[^|]*\|\s*60\.30\s*\|/);
  });
});

test.describe('Workers formula (§1)', () => {
  test('§8 illustrative volumes total $6.00', () => {
    // 5.00 + (12−10)×0.30 + (50−30)×0.02 = 5.00 + 0.60 + 0.40.
    expect(workersCost(12e6, 50e6)).toBeCloseTo(section8ExpectedTotals.workers, 9);
  });

  test('usage exactly at both allowances costs the $5.00 base only', () => {
    expect(workersCost(10e6, 30e6)).toBe(5);
  });

  test('zero usage clamps both overage terms to zero', () => {
    expect(workersCost(0, 0)).toBe(5);
  });

  test('one million requests over allowance adds $0.30', () => {
    expect(workersCost(11e6, 30e6)).toBeCloseTo(5.3, 9);
  });

  test('one million CPU-ms over allowance adds $0.02', () => {
    expect(workersCost(0, 31e6)).toBeCloseTo(5.02, 9);
  });
});

test.describe('R2 formula (§2)', () => {
  test('§8 illustrative volumes total $15.45', () => {
    // (500−10)×0.015 + (2−1)×4.50 + (20−10)×0.36 = 7.35 + 4.50 + 3.60.
    expect(r2Cost(500, 2e6, 20e6)).toBeCloseTo(section8ExpectedTotals.r2, 9);
  });

  test('§8 R2 parts match the worksheet spot-check', () => {
    expect(clamp(Math.ceil(500) - 10) * 0.015).toBeCloseTo(7.35, 9);
    expect(clamp(Math.ceil(2e6 / 1e6) - 1) * 4.5).toBe(4.5);
    expect(clamp(Math.ceil(20e6 / 1e6) - 10) * 0.36).toBeCloseTo(3.6, 9);
  });

  test('usage exactly at every allowance costs nothing', () => {
    expect(r2Cost(10, 1e6, 10e6)).toBe(0);
  });

  test('fractional GB-months round up before the allowance', () => {
    // 10.1 GB-months bills as 11, so one GB-month is over allowance.
    expect(r2Cost(10.1, 0, 0)).toBeCloseTo(0.015, 12);
  });

  test('one operation over an allowance bills a whole million', () => {
    expect(r2Cost(0, 1e6 + 1, 0)).toBe(4.5);
    expect(r2Cost(0, 0, 10e6 + 1)).toBeCloseTo(0.36, 12);
  });

  test('zero usage costs nothing', () => {
    expect(r2Cost(0, 0, 0)).toBe(0);
  });
});

test.describe('KV formula (§3)', () => {
  test('§8 illustrative volumes total $3.00', () => {
    // (15−10)×0.50 + (2−1)×0.50 = 2.50 + 0.50; writes/deletes/lists inside allowance.
    expect(kvCost(15e6, 0.5e6, 0, 0, 2)).toBe(section8ExpectedTotals.kv);
  });

  test('usage exactly at every allowance costs nothing', () => {
    expect(kvCost(10e6, 1e6, 1e6, 1e6, 1)).toBe(0);
  });

  test('half a million writes over allowance adds $2.50', () => {
    expect(kvCost(0, 1.5e6, 0, 0, 0)).toBe(2.5);
  });

  test('deletes and lists over allowance bill $5.00 per million', () => {
    expect(kvCost(0, 0, 2e6, 0, 0)).toBe(5);
    expect(kvCost(0, 0, 0, 2e6, 0)).toBe(5);
  });

  test('zero usage costs nothing', () => {
    expect(kvCost(0, 0, 0, 0, 0)).toBe(0);
  });
});

test.describe('Queues formula (§4)', () => {
  test('§8 illustrative volumes total $2.00', () => {
    // M = 2M messages ≤64 KB → Ops ≈ 3 × 2M × 1 = 6M; (6−1)×0.40.
    expect(queuesOpsEstimate(2e6)).toBe(6e6);
    expect(queuesCost(queuesOpsEstimate(2e6))).toBeCloseTo(section8ExpectedTotals.queues, 9);
  });

  test('chunksPerMessage scales the operation estimate', () => {
    expect(queuesOpsEstimate(1e6, 2)).toBe(6e6);
    expect(queuesCost(queuesOpsEstimate(1e6, 2))).toBeCloseTo(2.0, 9);
  });

  test('one million operations is exactly the allowance', () => {
    expect(queuesCost(1e6)).toBe(0);
  });

  test('zero operations cost nothing', () => {
    expect(queuesCost(0)).toBe(0);
  });
});

test.describe('Analytics Engine formula (§5, prospective)', () => {
  test('§8 illustrative volumes total $0.50, kept out of the current total', () => {
    // (12−10)×0.25 = 0.50; read queries inside allowance.
    expect(analyticsEngineProspective(12e6, 0.5e6)).toBe(section8ExpectedTotals.analyticsEngineProspective);
  });

  test('usage exactly at both allowances costs nothing', () => {
    expect(analyticsEngineProspective(10e6, 1e6)).toBe(0);
  });

  test('read queries over allowance bill $1.00 per million', () => {
    expect(analyticsEngineProspective(0, 2e6)).toBe(1);
  });
});

test.describe('Workers Logs formula (§5b, prospective)', () => {
  test('§8 illustrative volumes total $3.00, kept out of the current total', () => {
    // (25−20)×0.60 = 3.00.
    expect(workersLogsProspective(25e6)).toBeCloseTo(section8ExpectedTotals.workersLogsProspective, 9);
  });

  test('usage exactly at the allowance costs nothing', () => {
    expect(workersLogsProspective(20e6)).toBe(0);
  });

  test('one million events over allowance adds $0.60', () => {
    expect(workersLogsProspective(21e6)).toBeCloseTo(0.6, 9);
  });

  test('zero usage costs nothing', () => {
    expect(workersLogsProspective(0)).toBe(0);
  });
});

test.describe('PlanetScale formula (§6)', () => {
  const appCluster = {
    sku: 5,
    diskGb: 20,
    includedInstances: 1,
    extraReplicas: 0,
    backupGbMonths: 0,
    egressGb: 30,
    egressAllowanceGb: 10,
  };
  const mbCluster = {
    sku: 5,
    diskGb: 110,
    includedInstances: 1,
    extraReplicas: 0,
    backupGbMonths: 0,
    egressGb: 100,
    egressAllowanceGb: 10,
  };

  test('§8 compute subtotal is 2 × $5.00', () => {
    expect(appCluster.sku + mbCluster.sku).toBe(section8ExpectedTotals.planetscaleCompute);
  });

  test('§8 storage subtotal is $13.75', () => {
    // (20−10)×0.125 + (110−10)×0.125 = 1.25 + 12.50.
    const app = planetscaleClusterCost({ ...appCluster, egressGb: 10, backupGbMonths: 0 });
    const mb = planetscaleClusterCost({ ...mbCluster, egressGb: 10, backupGbMonths: 0 });
    expect(app).toBeCloseTo(6.25, 9);
    expect(mb).toBeCloseTo(17.5, 9);
    expect(app - appCluster.sku + (mb - mbCluster.sku)).toBeCloseTo(
      section8ExpectedTotals.planetscaleStorage,
      9,
    );
  });

  test('§8 egress subtotal is $6.60', () => {
    // [(30−10) + (100−10)] × 0.06 = 110 × 0.06.
    const billableEgress =
      clamp(appCluster.egressGb - appCluster.egressAllowanceGb) +
      clamp(mbCluster.egressGb - mbCluster.egressAllowanceGb);
    expect(billableEgress).toBe(110);
    expect(billableEgress * 0.06).toBeCloseTo(section8ExpectedTotals.planetscaleEgress, 9);
  });

  test('disk at the 10 GB allowance has no storage term', () => {
    expect(
      planetscaleClusterCost({ ...appCluster, diskGb: 10, egressGb: 10, backupGbMonths: 0 }),
    ).toBe(5);
  });

  test('backups inside 2× disk have no backup term; overage bills at $0.023', () => {
    const inside = planetscaleClusterCost({
      ...appCluster,
      diskGb: 20,
      backupGbMonths: 40,
      egressGb: 10,
    });
    expect(inside).toBeCloseTo(6.25, 9);
    const over = planetscaleClusterCost({
      ...appCluster,
      diskGb: 20,
      backupGbMonths: 140,
      egressGb: 10,
    });
    // 100 GB-months over 2× disk × $0.023 = $2.30 on top of the $6.25.
    expect(over).toBeCloseTo(8.55, 9);
  });

  test('egress at the allowance has no egress term', () => {
    // Disk over allowance isolates the egress term: 5 + (20−10)×0.125 = 6.25.
    expect(
      planetscaleClusterCost({ ...appCluster, diskGb: 20, egressGb: 10, backupGbMonths: 0 }),
    ).toBeCloseTo(6.25, 9);
  });

  test('extra replicas get no storage allowance', () => {
    expect(
      planetscaleClusterCost({
        ...appCluster,
        diskGb: 20,
        extraReplicas: 2,
        egressGb: 10,
        backupGbMonths: 0,
      }),
    ).toBeCloseTo(6.25 + 20 * 0.125 * 2, 9);
  });

  test('HA storage allowance scales with included instances', () => {
    // Single-node D = 20 GB: (20−10)×0.125×1 = 1.25; HA (N = 3): ×3 = 3.75.
    expect(
      planetscaleClusterCost({ ...appCluster, diskGb: 20, egressGb: 10, backupGbMonths: 0 }),
    ).toBeCloseTo(6.25, 9);
    expect(
      planetscaleClusterCost({
        ...appCluster,
        diskGb: 20,
        includedInstances: 3,
        egressGb: 10,
        backupGbMonths: 0,
      }),
    ).toBeCloseTo(8.75, 9);
  });
});

test.describe('Total (§7)', () => {
  test('§8 currently-billed total is $56.80', () => {
    const current =
      workersCost(12e6, 50e6) +
      r2Cost(500, 2e6, 20e6) +
      kvCost(15e6, 0.5e6, 0, 0, 2) +
      queuesCost(queuesOpsEstimate(2e6)) +
      planetscaleClusterCost({
        sku: 5,
        diskGb: 20,
        includedInstances: 1,
        extraReplicas: 0,
        backupGbMonths: 0,
        egressGb: 30,
        egressAllowanceGb: 10,
      }) +
      planetscaleClusterCost({
        sku: 5,
        diskGb: 110,
        includedInstances: 1,
        extraReplicas: 0,
        backupGbMonths: 0,
        egressGb: 100,
        egressAllowanceGb: 10,
      });
    // 6.00 + 15.45 + 3.00 + 2.00 + 7.45 + 22.90.
    expect(current).toBeCloseTo(section8ExpectedTotals.totalCurrent, 9);
  });

  test('§8 total including prospective Analytics Engine is $57.30', () => {
    expect(
      section8ExpectedTotals.totalCurrent +
        analyticsEngineProspective(12e6, 0.5e6),
    ).toBeCloseTo(section8ExpectedTotals.totalWithProspectiveAe, 9);
  });

  test('§8 total including all prospectives is $60.30', () => {
    expect(
      section8ExpectedTotals.totalWithProspectiveAe +
        workersLogsProspective(25e6),
    ).toBeCloseTo(section8ExpectedTotals.totalWithAllProspectives, 9);
  });
});

test.describe('§10 measured bill is evidence, not arithmetic', () => {
  // The §10 first measured staging bill (2026-10-13 → 2026-10-17) records
  // staging-attributable readings — 17 requests in period, 0 KV keys,
  // backlog-0 queues estimate, host-reported PlanetScale compute — as
  // evidence with estimated/unverified rows marked as such. None of those
  // rows are formula outputs, so none are asserted here. The shared
  // `section8ExpectedTotals` fixture above lists exactly which §8 totals
  // this file expects; this guard enforces that fixture scope (it does not
  // scan other files for §10 assertions).
  test('expected totals cover §8 services only — no §10 measured rows', () => {
    expect(Object.keys(section8ExpectedTotals).sort()).toEqual(
      [
        'analyticsEngineProspective',
        'kv',
        'planetscaleCompute',
        'planetscaleEgress',
        'planetscaleStorage',
        'queues',
        'r2',
        'totalCurrent',
        'totalWithAllProspectives',
        'totalWithProspectiveAe',
        'workers',
        'workersLogsProspective',
      ].sort(),
    );
    // The §10 measured total exists in the worksheet but stays out of the fixture.
    expect(worksheetDoc).toContain('15.00');
    expect(Object.values(section8ExpectedTotals)).not.toContain(15);
  });
});
