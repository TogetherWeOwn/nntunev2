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
| `jobs` | An object keyed by decimal run ID. For each first-attempt automatic `main` push with `queued` or `pending` status, save all pages of `runs/<id>/attempts/1/jobs?per_page=100`. Other runs already stay outside selection. |

A complete report gives the newest eligible run ID and one disposition per
listed run. The selector compares GitHub workflow run numbers. The eligible
pool consists of first-attempt, same-repository `main` pushes in `queued` or
`pending` with no job activity. An empty job listing or exclusively unstarted
jobs qualifies. An active or completed job, runner assignment, step records,
or non-null job timestamps keep the run. The newest eligible run and every pin
are kept. `superseded` means a different eligible run has a higher run number.
Dispatches, reruns, other refs and underway runs stay outside selection.

Missing pages, changing totals, duplicate run identities, job evidence bound
to a different run or attempt, and inputs beyond the supported limits return
`indeterminate` with exit status 1 and no dispositions. Capture the responses
close together. REST pages are a point-in-time observation and can change
between requests or after the capture. Treat the report as operator evidence
only. The CLI reads stdin and writes its report to stdout.
