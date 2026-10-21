# Demo worker

Retries cap (`MAX_RETRIES = 3`, `src/lib.rs:4`) guards the loop.

Run `run_job` (`src/lib.rs:7-9`) after `helper_inner` in
`src/lib.rs:11-13` passes its check.

Bare shorthand `lib.rs:7-9` resolves through the crate `src/` dir.

Full `scripts/check.py:1-2` first, then shorthand `check.py:1-2`.

Multi-pin `src/lib.rs:4,7,11` covers const and fns.

Bare `:999` continuations are out of scope, as are `localhost:3000`
and `12:00:00` timestamps.

```md
`src/does-not-exist.rs:1` in fences never flags.
```
