# Staging queue readback

Use this offline report when automatic main pushes accumulate in the staging
queue. Run it on saved GitHub Actions REST pages. Each optional pin can be
repeated. A SHA pin keeps every matching run in the inventory.

```sh
python3 -I ci/staging_queue_readback.py --pin-run 1234 \
  --pin-sha 0123456789abcdef0123456789abcdef01234567 < queue-evidence.json
```

Build one JSON object with these fields. Use `per_page=100` on each listing
request, follow pagination from page 1 through the last page, and keep the
response bodies in page order, including the first page for an empty listing.
GitHub CLI's `gh api --paginate --slurp` yields this array of page objects.
Use fresh read-only responses from `repos/TogetherWeOwn/nntunev2/actions/`.

| Field | Saved REST data |
| --- | --- |
| `workflow` | `workflows/deploy.yml`, as a single object |
| `runs` | An object with `queued`, `pending`, `requested`, `waiting` and `in_progress` keys. For each, save all pages of `workflows/deploy.yml/runs?status=<key>&per_page=100` without filtering by branch or event. |
| `jobs` | An object keyed by the attempt-specific route, such as `runs/1234/attempts/1/jobs`. For each first-attempt automatic `main` push with `queued` or `pending` status, save all pages from that route with `?per_page=100`. Other runs already stay outside selection. |

Keep the route key even if a job row omits `run_attempt`. A supplied attempt
must match the route. Job rows must also match the listed run ID, URL and SHA.

A complete report gives the newest eligible run ID and one disposition per
listed run. The selector compares GitHub workflow run numbers. The eligible
pool consists of first-attempt, same-repository `main` pushes in `queued` or
`pending` with a complete, empty job listing. Any job record keeps the run.
A queued gate can hold the staging writer group while it waits for a runner.
The newest eligible run and every listed pin are kept. `superseded` means a
different eligible run has a higher run number. Dispatches, reruns, other refs
and underway runs stay outside selection. Terminal runs stay outside this
active inventory. The `unlisted_pins` field accounts for any supplied run ID
or SHA absent from the inventory, including pins for completed or rejected runs.

Missing pages, changing totals, duplicate run identities, job evidence bound
to a different run or attempt, and inputs beyond the supported limits return
`indeterminate` with exit status 1 and no dispositions. Capture the responses
close together. REST pages are a point-in-time observation and can change
between requests or after the capture. Treat the report as operator evidence
only. The CLI reads stdin and writes its report to stdout.

### Checking an approval for the selected run

When recovery depends on staging approval, include these two objects in the
same input alongside the REST pages. This synthetic example names run `1234`,
attempt `2`.

```json
{
  "approval_candidate": {
    "run_id": 1234,
    "run_attempt": 2,
    "head_sha": "0123456789abcdef0123456789abcdef01234567"
  },
  "approval": {
    "run_id": 1234,
    "run_attempt": 2,
    "head_sha": "0123456789abcdef0123456789abcdef01234567",
    "environment": "staging",
    "state": "approved"
  }
}
```

Take the candidate identity from the selected Actions run. Record the approval
identity from independently checked staging review evidence for that exact
attempt. GitHub deployment review history is scoped to the run ID. If the
decision cannot be attributed to the attempt, omit `approval` and keep the
preview indeterminate. An operator pin only preserves the named run or SHA.

When both records match the current inventory, `matched_approval` reports the
run ID, attempt and SHA and the run gets `keep_approved`. If the approval names
an earlier run with the same SHA or an earlier attempt than the selected one,
the report is `indeterminate` without any dispositions, even with a pin. The
CLI compares the supplied records offline. Environment authorization stays
with GitHub.
