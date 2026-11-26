# Automatic staging push readback

When a prerequisite squash has no usable staging run, use saved GitHub REST
responses to find an ordinary main push that contains the squash. Pass the full
squash SHA and the independently known staging repository name and numeric ID.

```sh
python3 -E -s ci/staging_push_readback.py "$squash" "$repository" "$repository_id" < evidence.json
```

The input is one JSON object, limited to 4 MiB. Save the following fields from
reads in the same repository (`repos/<owner>/<repo>`):

| Field | Saved REST response |
| --- | --- |
| `workflow` | `actions/workflows/deploy.yml` |
| `runs` | One complete listing from `actions/workflows/deploy.yml/runs?branch=main&event=push&per_page=100&page=1` with `total_count` and at most 100 rows. The `created` filter can bound the search to a range whose results fit on one page. |
| `comparisons` | Object keyed by `compare/<squash>...<head_sha>` for completed successful first-attempt main pushes under consideration. Use the unpaginated comparison. The head and squash are full commit SHAs. You can retain just `base_commit`, `merge_base_commit`, `status`, `ahead_by`, `behind_by`, `total_commits` and `commits`, with only `sha` in each commit object. |
| `jobs` | Object keyed by `runs/<id>/attempts/<attempt>/jobs`. Save the complete attempt jobs listing with `total_count` from `actions/` for each qualifying comparison. |

The readback examines completed successful first attempts. For a SHA with
multiple runs, only the highest run ID is eligible,
matching the existing verifier's selection. The comparison must show that the
push head is the squash itself or is ahead of it with the squash as the merge
base. The ordinary Web, API and full-journey job records must match the push's
SHA, run and attempt and must have succeeded.

When several pushes qualify, the readback nominates the one whose post-deploy
journey completed most recently in the snapshot. Required jobs conclusively
skipped or failed disqualify that push. Missing or contradictory job evidence
leaves the readback pending. The snapshot supplies candidate discovery. The
verifier checks intervening writes and current staging identity independently.

The result `nominated` contains `sha`, `run_id` and `run_attempt` for the
existing [staging admission verifier](staging-admission.md). The synthetic
handoff exercises its ordinary receipt checks with a fixture policy and matches
all three fields against the verified receipt. The installed approval list is
empty. Application promotion requires an approved private-music contract and
the existing protected verification path.
A readback with insufficient data or no eligible run returns
`{"status":"pending"}` and exits 1.
