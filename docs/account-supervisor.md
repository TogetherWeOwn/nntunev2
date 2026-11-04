# Account reservation and closure boundary

`workers/stream/src/account_supervisor.rs` is a portable, **runtime-unwired**
account-owner boundary. It is not a Durable Object deployment or live Worker
transport qualification. No public playback miss or provider operation is
activated by this module.

## Ownership and persistence

- Exactly one authoritative supervisor owns an account's journal. An eventual
  adapter must serialize mutations through the entire storage await, not merely
  lock while cloning a snapshot. Competing cached owners or detached writes do
  not satisfy the storage trait. Account/object identity comes from authenticated
  routing, not an untrusted request body.
- Before each admission, obtain and reconcile a fresh authenticated registry
  snapshot. Monotonic revision checks reject stale or conflicting snapshots;
  hints are not leases. No endpoint, username, password or resolver proof is
  stored in the journal. Credential references are internal identities only.
- Reserve saves a monotonic, account-bound session reference and a class-budget
  lease before returning permission to invoke a dial factory. The supervisor
  keeps the actual lease. Dropping a session reference or an operation future
  cannot release it. Stream and ingest still use the existing account pool's
  cap, reserve and circuit-breaker admission.
- The factory runs at most once per reservation. It returns independent physical
  close authority synchronously along with its setup future. A factory error or
  panic is uncertain, not proof that no socket was created. Only a reservation
  whose factory was never invoked can be cancelled without close confirmation.
- Any failed or cancelled save poisons the retained instance before another
  admission or authentication can occur. A save may have committed despite its
  failure response. The implementation never assumes otherwise and never
  retries a write in the background.

The storage adapter must atomically replace the complete journal, await durable
acknowledgement, reject corrupt/unknown formats and distinguish absent state
from a read failure. Missing state must never be substituted after a read error.
Durable Object binding, storage integration and request serialization remain to
be installed and tested before any runtime use.

## Closure and recovery

The runtime retains setup/I/O futures separately from the supervisor. It races
all setup, greeting, authentication and article I/O against one whole-session
20-second deadline, drops/cancels the operation on every terminal path, and calls
the supervisor's close method. Before each AUTHINFO command it uses the exact
current account/revision/credential-bound resolver proof and checks
`auth_permitted` against its original five-second expiry and current time.
Checking before dial alone is insufficient; the hook cannot retract an already
pending write. A disabled/rotated account or expired proof requires cancellation
and forced close, never a hostname fallback or credential retry.

Capacity is retired only after BOTH a successful forced-close result and
physical closure confirmation, then a durable journal-removal acknowledgement.
The Worker control implementation uses the raw socket's `close()` and `closed()`
with a bounded timeout. Generic async-write shutdown or transport Drop is not
confirmation. Close remains available on a storage-poisoned instance to drain
sockets, but cannot retire its uncertain reservations. This first supervisor
never parks a warm socket.

On restart/eviction, any outstanding journal reservation fences the **entire
account** indefinitely, even after TTL expiry, disabling, cap changes or
credential rotation. The old instance's control handles cannot be reconstructed
from JSON. This conservative behavior sacrifices availability to preserve the
physical account cap. There is no automatic reset, TTL cleanup, operator reset
endpoint or recovery override in this increment; a future recovery procedure
must establish closure evidence before it can clear that fence.

## Authentication quarantine

An authentication failure marks its credential identity quarantined in memory
before awaiting storage. Subsequent admission/auth checks refuse while the write
is pending or fails. Successful journal acknowledgement persists quarantine
across a clean close and restart. Settings-only revisions, enable toggles,
provider successes and cooldown expiry cannot clear it. A failure under the same
credential identity still quarantines after a settings revision; a late failure
from a genuinely rotated identity cannot quarantine the new secret. Only an
authoritative new credential reference can clear the auth marker, never an
outstanding physical-capacity fence.

## Evidence boundaries

Deterministic native storage/close controls test ordering, failures, cancellation,
restart, cap/class budgets and quarantine. They are not evidence that a real
Worker's TLS handshake, entropy, certificate time or physical close works.
Existing public-peer qualification and curated-track staging playback gates
remain required before real resolver-to-dial wiring or public NNTP misses.
Retired fixtures and real provider settings are not synthetic fault targets.

This backend/playback change follows AGENTS.md **Clean-room, YouTube and
MetaBrainz guardrails**. It adds no assets, video behavior or catalogue ingestion.
