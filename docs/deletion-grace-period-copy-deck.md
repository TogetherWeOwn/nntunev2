# Account deletion grace-period copy deck (web)

> **Status: DRAFT policy input.** This deck drafts user-facing wording for
> the web account-deletion journey. It is **not** approved policy, not a
> legal commitment, and not shipped copy. Final approval — including the
> grace duration, the retained-records wording in §9, and every bracketed
> decision in §11 — happens with the web-journey work that first renders
> this text. Do not quote this deck to users before that approval.

Design ground truth this deck must stay consistent with: ADR 0002 rule 3
(deletion states, grace window, cancel compare-and-set) and ADR 0006 §1
(`scheduled`/`pending`/`running`/`failed` operation states, `erase_after`,
step-up proof, confirmation email). Where this deck and an ADR disagree,
the ADR wins and the deck is wrong.

## 1. How to use this deck

- Each state in §§3–8 gives one entry per surface: **Heading**, **Body**,
  primary action, secondary action. Render the body verbatim unless the
  approving change records a deliberate edit.
- `{{placeholder}}` values are filled by the client at render time; see
  the table in §2. Never hard-code a duration or a date into a string.
- One idea per sentence. Short words. No sentence in user copy should
  need a second read.

## 2. Placeholders

| Placeholder | Meaning | Example rendering |
| --- | --- | --- |
| `{{grace_duration}}` | Length of the cancellable grace period, in words | `24 hours` |
| `{{erase_after}}` | Local date and time erasure can start, from the operation's `erase_after` | `3 May at 14:20` |
| `{{time_remaining}}` | Time left to cancel, rounded down | `about 6 hours` |
| `{{email}}` | The account email address | `you@example.com` |
| `{{support_contact}}` | How to reach support | *(undecided — §11)* |
| `{{retained_records_sentence}}` | Approved disclosure of any records kept after erasure; omitted until approved | *(undecided — §11)* |
| `{{re_registration_sentence}}` | Approved sentence about registering again; omitted until approved | *(undecided — §11)* |

Rules: `{{time_remaining}}` always rounds **down** (never promise more
time than is left), and when less than one hour remains it renders in
minutes. `{{erase_after}}` renders in the viewer's local timezone and is
always paired with the words "can start", never "will start" — the exact
start time after the deadline is not promised.

## 3. State: before the request (account settings)

Surface: danger-zone section of account settings.

- **Heading:** Delete your account
- **Body:** Deleting your account is permanent. Because it cannot be
  undone, nothing is erased straight away: you get a grace period of
  {{grace_duration}} to change your mind.
- **Primary action:** Delete account…
- **Secondary action:** *(none)*

## 4. State: request (confirmation step)

Surface: confirmation dialog, shown before the step-up check.

- **Heading:** Delete your account?
- **Body:** If you go ahead, your deletion is **scheduled** — nothing is
  erased yet. You then have {{grace_duration}} to cancel. If you do not
  cancel, erasure can start after {{erase_after}}. Once erasure is
  finished, your account, library and listening history cannot be
  restored. To confirm this is you, we will ask you to verify your
  identity on the next step.
- **Primary action:** Continue
- **Secondary action:** Keep my account

After the identity check succeeds, the confirm button is labelled:

- **Primary action:** Schedule deletion

Do not label this button "Delete now" or "Erase my data" — the request
schedules erasure; it does not perform it.

## 5. State: scheduled — grace period running

This is the state for the whole grace window (operation `scheduled`).

### 5.1 Confirmation screen (immediately after the request)

- **Heading:** Deletion scheduled
- **Body:** We have **not** erased anything yet. Your account is
  scheduled for deletion, and erasure can start after {{erase_after}}.
  Changed your mind? You can cancel any time before then and keep your
  account as it is. We have sent a confirmation email to {{email}}. If
  this was not you, sign in and cancel straight away.
- **Primary action:** Cancel deletion
- **Secondary action:** Sign out

### 5.2 Confirmation email (request received)

- **Subject:** Your account deletion is scheduled
- **Body:** You asked us to delete your account. Nothing has been erased
  yet. Erasure can start after {{erase_after}}. To keep your account,
  sign in before then and cancel the deletion. If you did not ask for
  this, sign in and cancel now, then contact {{support_contact}}.

### 5.3 Status banner (shown while signed in during grace)

- **Body:** Your account is scheduled for deletion. Nothing has been
  erased yet. Erasure can start after {{erase_after}} ({{time_remaining}}
  left to cancel).
- **Primary action:** Cancel deletion

### 5.4 Status page (status during grace)

The operation state here is `scheduled` for the whole window; do not
branch this page on the `pending` operation state.

- **Heading:** Deletion scheduled
- **Body:** Status: **scheduled**. Your grace period ends at
  {{erase_after}}; erasure can start any time after that. Until erasure
  starts, you can cancel and your account stays unchanged. While deletion
  is scheduled, some changes to your account are paused.
- **Primary action:** Cancel deletion

### 5.5 Blocked change (a write refused while deletion is pending)

- **Body:** You cannot make changes while your account is scheduled for
  deletion. Cancel the deletion first if you want to keep using your
  account.

## 6. State: cancel the deletion

### 6.1 Cancel confirmation

Surface: confirmation dialog; cancelling needs the same identity check
as the request. If the session has lapsed, the user signs in again first —
the copy must say so up front, not after a failure.

- **Heading:** Cancel deletion and keep your account?
- **Body:** This stops the scheduled deletion. Your account, library and
  history stay as they are. Nothing has been erased, so there is nothing
  to restore. To confirm this is you, we will ask you to verify your
  identity. If you have been signed out, sign in again first — you still
  have {{time_remaining}} to cancel.
- **Primary action:** Keep my account
- **Secondary action:** Go back

### 6.2 Cancel succeeded

- **Heading:** Deletion cancelled
- **Body:** Your account is no longer scheduled for deletion. Nothing was
  erased. Everything works as before.
- **Primary action:** Continue

Email twin — **Subject:** Your account deletion was cancelled. **Body:**
The scheduled deletion of your account has been cancelled and your
account stays active. If you did not do this, contact
{{support_contact}}.

### 6.3 Cancel arrives too late (erasure has already started)

- **Heading:** It is too late to cancel
- **Body:** Erasure has already started, so this deletion can no longer
  be cancelled. We will email {{email}} when it is finished.
- **Primary action:** OK

Never show this wording during the grace window: until erasure starts,
cancelling always works.

## 7. State: grace expired — erasure in progress

Operation `running`. The user may still hold a session briefly, so this
copy exists for any surface that can still render.

- **Heading:** Erasure in progress
- **Body:** The grace period has ended and erasure has started. It is
  **not finished yet**. This can no longer be cancelled. We will email
  {{email}} when erasure is complete.
- **Primary action:** Sign out

## 8. State: erased

The account no longer exists, so there is no signed-in surface. The
confirmation email (ADR 0006) is the completion record.

### 8.1 Completion email

- **Subject:** Your account has been erased
- **Body:** Erasure of your account is complete. Your account, library
  and listening history have been erased and cannot be restored.
  {{retained_records_sentence}} {{re_registration_sentence}}
  If you did not ask us to delete your account, contact
  {{support_contact}}.

`{{retained_records_sentence}}` and `{{re_registration_sentence}}` are
deliberately not drafted here: what, if anything, is retained (for
example billing records), and whether the same address may register
again, are policy decisions for the approving change (§11). Until those
sentences are approved, no surface may claim that *every* record was
erased or invite re-registration with the same address.

### 8.2 Signing in afterwards

A sign-in attempt for an erased account gets the generic unknown-account
response, identical to an address that never had an account (ADR 0002
rule 3). Do not add copy that confirms the account once existed.

## 9. State: erasure delayed or failed

Operation `failed`, or `running` far beyond the expected window.

- **Heading:** Erasure is not finished yet
- **Body:** We have not finished erasing your account. We are still
  working on it and will email {{email}} when it is complete. You do not
  need to do anything. If you are worried, contact
  {{support_contact}}.

Do not give a new date unless engineering can stand behind it.

## 10. Language rules (every surface)

1. **Never claim erasure before completion.** Before §8, never
   affirmatively state that the account is "deleted", "erased" or
   "removed"; it is "scheduled for deletion" or "erasure in progress".
   Affirmative completion wording is reserved for the completion email
   in §8.1. Negations that reassure ("nothing has been erased yet") are
   required, not banned.
2. **Explain the grace period every time it matters.** Any surface that
   mentions the request also states, in plain words: nothing is erased
   yet, you have {{grace_duration}}, and you can cancel. Do not rely on
   the user having seen an earlier screen.
3. **No dark patterns.** Cancel-deletion is never hidden, shrunk, or
   labelled with guilt wording ("No, I love my data"). The destructive
   path and the cancel path get equally plain labels.
4. **No reassurance we cannot prove.** No "all copies everywhere are
   gone", no backup-erasure claims, no legal-basis claims — those are
   policy sentences (§11), not deck wording.
5. **Plain language.** Prefer "erase" / "scheduled" / "cancel" over
   "execute", "tombstone", "compare-and-set" or any internal term. Read
   each string aloud; if it sounds like a contract, rewrite it.
6. **Time honesty.** Deadlines are "can start after", remaining time
   rounds down (§2), and completion is announced only by the §8.1 email.

## 11. Decisions deferred to the approving (web-journey) change

- The value rendered as `{{grace_duration}}` (design default in ADR 0002
  rule 3) and whether it is stated on the settings surface in §3.
- `{{retained_records_sentence}}`: which records, if any, are retained
  after erasure (billing/tombstone and audit retention exist in the
  design), and the approved plain-language sentence disclosing them.
- `{{support_contact}}`: the real support route for deletion problems.
- Whether backups exist in the erasure story, and if so the approved
  wording — this deck makes no backup claim either way.
- Whether the completion email may invite re-registration with the same
  address (trial anti-abuse retention in the design may affect the
  wording).
- Translations: this deck is the English source; no string ships
  untranslated if the journey is localised.

## 12. Review checklist for the approving change

- [ ] Every §3–§9 string checked against the shipped state machine
  (`scheduled` through the whole grace window; cancel only before
  erasure starts).
- [ ] No pre-completion surface affirmatively states the account is
  "deleted"/"erased"/"removed" (rule 1); grep the shipped strings, not
  just this deck, and confirm every pre-completion hit is a negation.
- [ ] Placeholders filled from live operation data (`erase_after`),
  remaining time rounded down.
- [ ] Plain-language pass by a reader outside the deletion work.
- [ ] Data-rights/policy review signs off §11 before any string renders.
