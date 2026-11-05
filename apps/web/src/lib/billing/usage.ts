/**
 * Usage-vs-cap evaluation for display only.
 *
 * Enforcement lives in the API worker (server reads `subscriptions` +
 * `plans` + the stream-play ledger fold; spec §3–§4). This module turns
 * the same numbers into UI states: a soft nudge at 80%+, at-cap messaging
 * at 100%+, and upgrade prompts.
 *
 * Overage policy (spec §3.3): no overage billing, ever. At the monthly
 * cap, *new* playback is refused (`402 plan_limit_exceeded`) until the
 * period resets (1st, 00:00 UTC) or the user upgrades; a track already
 * playing finishes. There is no degraded-bitrate tier. Browsing, search
 * and library stay fully usable in every state (spec §2.2/§4).
 */

import { nextPlan, streamCapSeconds, type Plan } from './plans';

export interface UsageSnapshot {
	/** Seconds streamed this UTC calendar month (SUM over the stream-play ledger). */
	streamSecondsMonth: number;
}

export type LimitState = 'ok' | 'soft' | 'hard';

export interface LimitStatus {
	state: LimitState;
	/** 0..1+ fraction of the monthly stream cap consumed. */
	streamFraction: number;
	/** At or past the plan's monthly stream cap. */
	streamHardCapped: boolean;
	/** Plan id to suggest, if any. */
	upgradeTo: string | null;
	/** Short human-readable line for banners. */
	headline: string;
	/** Longer at-cap/upgrade explanation. Empty when `ok`. */
	detail: string;
}

const SOFT_AT = 0.8;

function pct(fraction: number): string {
	return `${Math.min(999, Math.round(fraction * 100))}%`;
}

export function evaluateUsage(plan: Plan, usage: UsageSnapshot): LimitStatus {
	const cap = streamCapSeconds(plan);
	const streamFraction = cap > 0 ? usage.streamSecondsMonth / cap : 0;
	// A 0-hour plan (the reserved `free` row) is at cap by definition, so
	// never-subscribed display never reports headroom it does not have.
	const streamHardCapped = usage.streamSecondsMonth >= cap;

	const upgrade = nextPlan(plan.id);
	const upgradeTo = upgrade ? upgrade.id : null;

	if (streamHardCapped) {
		return {
			state: 'hard',
			streamFraction,
			streamHardCapped,
			upgradeTo,
			headline: `You've used ${pct(streamFraction)} of your ${plan.name} listening hours.`,
			detail:
				`New playback pauses until your hours reset — a track already playing finishes. ` +
				`No surprise bills, ever.` +
				(upgrade ? ` Upgrade to ${upgrade.name} for ${upgrade.streamHoursMonth} hrs/mo.` : '')
		};
	}

	if (streamFraction >= SOFT_AT) {
		return {
			state: 'soft',
			streamFraction,
			streamHardCapped,
			upgradeTo,
			headline: `You're at ${pct(streamFraction)} of your ${plan.name} listening hours.`,
			detail: upgrade
				? `Heads up before you reach the cap: ${upgrade.name} raises your hours.`
				: `Heads up before you reach the cap.`
		};
	}

	return {
		state: 'ok',
		streamFraction,
		streamHardCapped: false,
		upgradeTo,
		headline: `${pct(streamFraction)} of ${plan.name} listening hours used.`,
		detail: ''
	};
}

/** Remaining whole stream-minutes for meter display. Never negative. */
export function remainingStreamMinutes(plan: Plan, usedSeconds: number): number {
	return Math.max(0, Math.floor((streamCapSeconds(plan) - usedSeconds) / 60));
}

const MONTH_NAMES_SHORT = [
	'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
	'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'
];

/**
 * Spec §3.2 reset line, e.g. `Resets 1 Dec, 00:00 UTC`. The instant is
 * rendered in UTC (the period is a UTC calendar month) from the server-
 * provided `resetsAt` — never recomputed client-side. An unparseable
 * value yields an empty string so callers omit the line rather than
 * rendering a wrong date.
 */
export function formatResetInstant(resetsAt: string | null | undefined): string {
	if (!resetsAt) return '';
	const at = new Date(resetsAt);
	if (Number.isNaN(at.getTime())) return '';
	const day = at.getUTCDate();
	const month = MONTH_NAMES_SHORT[at.getUTCMonth()];
	const hh = String(at.getUTCHours()).padStart(2, '0');
	const mm = String(at.getUTCMinutes()).padStart(2, '0');
	return `Resets ${day} ${month}, ${hh}:${mm} UTC`;
}
