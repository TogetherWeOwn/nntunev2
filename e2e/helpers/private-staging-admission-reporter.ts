import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { FullResult, Reporter, TestCase, TestResult, TestStep } from '@playwright/test/reporter';

const identities = ['build-identity', 'build-identity-final'];
const proofs = {
  'invite-acceptance': [],
  'upload-to-library': ['private-publication', 'private-library'],
  play: ['private-play'],
  seek: [],
  queue: [],
  'anonymous-refused': ['private-anonymous-refusal'],
  'unlisted-refused': ['private-unlisted-refusal'],
  'revoked-or-expired-refused': ['private-cutoff-refusal'],
  'video-off': [],
};
const sha = /^[0-9a-f]{40}$/;
const digest = /^[0-9a-f]{64}$/;
const uuid = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;
const decimal = /^[1-9][0-9]{0,15}$/;
const isoTime = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/;

function* journeySteps(steps: TestStep[]): Generator<TestStep> {
  for (const step of steps) {
    if (step.category === 'test.step') yield step;
    yield* journeySteps(step.steps);
  }
}

function exactlyOne<T>([value, ...rest]: T[]): T {
  if (value === undefined || rest.length !== 0) throw new Error();
  return value;
}

function properties(result: TestResult, type: string, fields: Record<string, RegExp>) {
  return result.annotations.filter((annotation) => annotation.type === type).map(({ description }) => {
    if (!description || description.length > 1024) throw new Error();
    const observed: Record<string, unknown> = JSON.parse(description);
    return Object.fromEntries(Object.entries(fields).map(([key, shape]) => {
      const value = observed[key];
      if (typeof value !== 'string' || !shape.test(value)) throw new Error();
      return [key, value];
    }));
  });
}

function property(name: string, value: Record<string, string>) {
  const encoded = JSON.stringify(value).replaceAll('&', '&amp;').replaceAll('"', '&quot;')
    .replaceAll('<', '&lt;').replaceAll('>', '&gt;');
  return `<property name="${name}" value="${encoded}"/>`;
}

export default class PrivateStagingAdmissionReporter implements Reporter {
  private results: { test: TestCase; result: TestResult }[] = [];

  // Playwright otherwise adds a default reporter that prints failure details.
  printsToStdio() { return true; }

  onTestEnd(test: TestCase, result: TestResult) {
    this.results.push({ test, result });
  }

  async onEnd(run: FullResult): Promise<{ status: 'failed' } | void> {
    if (run.status !== 'passed') return;
    try {
      const { test, result } = exactlyOne(this.results);
      if (test.expectedStatus !== 'passed' || result.status !== 'passed' || result.retry !== 0) throw new Error();
      const steps = [...journeySteps(result.steps)];
      const trackFields = { track_id: uuid, sha256: digest, build_sha: sha, run_id: decimal, run_attempt: decimal };
      const observed = properties(result, 'candidate-identity', {
        phase: /^(build-identity|build-identity-final)$/, web: sha, api: sha, expected: sha, observed_at: isoTime,
      });
      if (observed.length !== identities.length) throw new Error();

      const cases = [...identities, ...Object.keys(proofs)].map((name) => {
        const title = name === 'invite-acceptance' ? 'invite redemption and Library arrival' : name;
        const step = exactlyOne(steps.filter((step) => step.title === title));
        if (step.error || step.duration < 0 || step.annotations.some(({ type }) => type === 'skip')) throw new Error();
        const annotations = [];
        if (identities.includes(name)) {
          const identity = exactlyOne(observed.filter(({ phase }) => phase === name));
          annotations.push(property('candidate-identity', identity));
        }
        for (const proof of proofs[name as keyof typeof proofs] ?? []) {
          const fields = proof === 'private-cutoff-refusal'
            ? { ...trackFields, cutoff_listener: uuid, refused_listener: uuid }
            : trackFields;
          const value = exactlyOne(properties(result, proof, fields));
          annotations.push(property(proof, value));
        }
        const evidence = annotations.length ? `<properties>${annotations.join('')}</properties>` : '';
        return `<testcase classname="private-music" name="${name}" retry="${result.retry}">${evidence}</testcase>`;
      });

      const directory = new URL('../private-staging-results/', import.meta.url);
      mkdirSync(directory, { recursive: true });
      writeFileSync(fileURLToPath(new URL('admission.xml', directory)), `<testsuite>${cases.join('')}</testsuite>\n`, { mode: 0o600 });
    } catch {
      process.stderr.write('Private staging admission evidence is incomplete.\n');
      return { status: 'failed' };
    }
  }
}
