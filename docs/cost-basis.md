# Infrastructure cost basis (public pricing worksheet)

Read-only research artifact: public list prices plus formulas, so measured
staging volumes can be plugged in later to get a monthly cost estimate.
No environment was accessed or changed to produce this document, and no
secret values appear in it. Prices were checked on **2026-10-17** against
the provider pages linked in each section; re-check before relying on any
number, because list prices change.

This worksheet covers only the services listed below. It is **not** an
all-in bill: Durable Objects (used by the `stream` Worker pool), Containers
(repair path), Workers Logs, domain/DNS and any non-Cloudflare,
non-PlanetScale services are out of scope here and must be added before
any total is quoted as complete. Hyperdrive itself carries no separate
line item beyond the Workers plan per the Workers pricing page, but the
database behind it is priced in the PlanetScale section.

All amounts are USD per month unless stated otherwise. `max(0, x)` means
"zero when usage is inside the included allowance".

## 1. Cloudflare Workers (Paid plan)

Source: [Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/),
checked 2026-10-17. The account entitlement is Workers Paid (see
`docs/ops.md`).

| Item | Included | Overage unit price |
|---|---|---|
| Account base | — | $5.00 / month |
| Requests (invocations) | 10 million / month | $0.30 / additional million |
| CPU time | 30 million CPU-ms / month | $0.02 / additional million CPU-ms |
| Duration (wall clock) | — | No charge |

Formula (`R` = requests/month, `C` = CPU-ms/month):

```text
workers = 5.00
        + max(0, R/1e6 - 10) * 0.30
        + max(0, C/1e6 - 30) * 0.02
```

Volume sources at staging time: per-Worker request and CPU metrics in the
Cloudflare dashboard / GraphQL Analytics API, summed over every deployed
Worker (`web`, `api`, `stream`, `mb`, `ingest`).

## 2. Cloudflare R2

Source: [R2 pricing](https://developers.cloudflare.com/r2/pricing/),
checked 2026-10-17.

| Item (Standard storage) | Included / month | Unit price |
|---|---|---|
| Storage | 10 GB-month | $0.015 / GB-month |
| Class A operations (writes, lists, multipart) | 1 million | $4.50 / million |
| Class B operations (reads, HEAD) | 10 million | $0.36 / million |
| Egress to Internet | — | $0 |

Infrequent Access (not currently used; no free tier): $0.01 / GB-month,
Class A $9.00 / million, Class B $0.90 / million, data retrieval
$0.01 / GB, egress $0.

Formula (`G` = Standard GB-months, `A`, `B` = Class A/B operations):

```text
r2 = max(0, G - 10) * 0.015
   + max(0, A/1e6 - 1) * 4.50
   + max(0, B/1e6 - 10) * 0.36
```

GB-months are time-weighted: an object stored for half the month counts
half. Sum `G`, `A`, `B` over all buckets in the environment (audio, art,
archive, ingest-scratch; see the bucket inventory in `docs/ops.md`).
Volume sources: R2 bucket metrics / GraphQL Analytics per bucket.

## 3. Cloudflare Workers KV

Source: [KV pricing](https://developers.cloudflare.com/kv/platform/pricing/),
checked 2026-10-17 (Paid plan column; same table on the Workers pricing
page).

| Item | Included / month | Overage unit price |
|---|---|---|
| Keys read | 10 million | $0.50 / million |
| Keys written | 1 million | $5.00 / million |
| Keys deleted | 1 million | $5.00 / million |
| List requests | 1 million | $5.00 / million |
| Stored data | 1 GB | $0.50 / GB-month |

Reads/writes/deletes are billed per key, so one bulk read of N keys counts
as N key reads.

Formula (`KR`, `KW`, `KD`, `KL` = reads/writes/deletes/lists, `KG` =
stored GB):

```text
kv = max(0, KR/1e6 - 10) * 0.50
   + max(0, KW/1e6 - 1) * 5.00
   + max(0, KD/1e6 - 1) * 5.00
   + max(0, KL/1e6 - 1) * 5.00
   + max(0, KG - 1) * 0.50
```

Volume sources: KV analytics per namespace (`nntunev2-config-*`).

## 4. Cloudflare Queues

Source: [Queues pricing](https://developers.cloudflare.com/queues/platform/pricing/),
checked 2026-10-17 (Paid plan).

| Item | Included / month | Overage unit price |
|---|---|---|
| Operations | 1 million | $0.40 / million |
| Egress / throughput | — | $0 |

An operation is counted per 64 KB chunk written, read or deleted (KB =
1,000 bytes, ~100 bytes internal metadata per message; operations are per
message, not per batch). A normal delivery of a ≤64 KB message costs 3
operations (write + read + delete); each retry redelivery adds read/delete
operations, and a dead-letter write adds another write.

Formula (`M` = messages delivered once, `chunks` = ceil of message size
per 64 KB, `Ops` = total operations when retries/DLQ traffic are known):

```text
queues = max(0, Ops/1e6 - 1) * 0.40
         where Ops ≈ 3 * M * chunks   (single-delivery, no-retry estimate)
```

Volume sources: Queue metrics per queue (ingest + DLQ, both
environments); count retries separately where the metrics expose them.

## 5. Cloudflare Analytics Engine

Source: [Analytics Engine pricing](https://developers.cloudflare.com/analytics/analytics-engine/pricing/),
checked 2026-10-17 (Workers Paid). Priced on two metrics: data points
written and read queries.

| Item | Included / month | Overage unit price |
|---|---|---|
| Data points written | 10 million | $0.25 / million |
| Read queries | 1 million | $1.00 / million |

Formula (`W` = data points written, `Q` = read queries):

```text
analytics_engine = max(0, W/1e6 - 10) * 0.25
                 + max(0, Q/1e6 - 1) * 1.00
```

The `stream` Worker writes metering points to the `nntune_meter` dataset
(`workers/stream/wrangler.toml`). Adaptive sampling can reduce stored
points at volume, so use the billed data-point count from account usage,
not the raw event count in the Worker.

## 6. PlanetScale Postgres

Sources: [PlanetScale pricing](https://planetscale.com/pricing) and
[PlanetScale Postgres pricing docs](https://planetscale.com/docs/postgres/pricing),
checked 2026-10-17. Regional rates below are for AWS `us-east-1`, the
region recorded in `docs/ops.md`, read from PlanetScale's live public
rate endpoints on the same date:
[`storage-rates?region=us-east`](https://api.planetscale.com/www/storage-rates?region=us-east)
returned `{"rate":"0.125"}` and
[`egress-rates?region=us-east`](https://api.planetscale.com/www/egress-rates?region=us-east)
returned `{"rate":"0.06"}`.

Cluster compute is a fixed SKU price, not per-row/read metering (that
model was the retired serverless MySQL pricing; it does not apply to
PlanetScale Postgres). Smallest SKUs (network-attached/EBS clusters):

| SKU | Single-node | HA (1 primary + 2 replicas) |
|---|---|---|
| PS-5 | $5 / month | $15 / month |
| PS-10 | $10 (ARM) – $13 (x86) / month | $30 (ARM) – $39 (x86) / month |
| PS-20 | $17 (ARM) – $20 (x86) / month | $50 (ARM) – $59 (x86) / month |

Metered add-ons per cluster:

| Item | Included | Unit price (us-east) |
|---|---|---|
| Disk storage (gp3, network-attached) | First 10 GB per cluster | $0.125 / GB-month over 10 GB |
| Provisioned IOPS / throughput | 3,000 IOPS and 125 MiB/s baseline (AWS gp3) | Only provisioned amounts above baseline are charged; rates shown in the PlanetScale dashboard, not on the public page — treat as $0 only while at baseline |
| Backup + WAL storage | 2× disk size | $0.023 / GB-month over the included amount (compressed size) |
| Public egress | 100 GB / month per production branch; 10 GB / month for PS-5 non-HA and PS-DEV branches | $0.06 / GB over the allowance |
| Ingress | — | $0 |
| Extra replicas | No 10 GB storage allowance for additional replicas beyond the HA set | Same storage rate, billed per instance |

Storage is billed per database instance, so an HA cluster pays the
storage line for the primary and each replica volume; the 10 GB allowance
is per cluster, not per replica. Resizes are prorated.

Formula per cluster (`SKU` = monthly cluster price from the table,
`D` = provisioned disk GB, `N` = instance count (1 single-node, 3 HA),
`Bak` = compressed backup+WAL GB-months, `E` = public egress GB,
`EA` = the branch egress allowance from the table):

```text
planetscale_cluster = SKU
  + max(0, N*D - 10) * 0.125
  + max(0, Bak - 2*D) * 0.023
  + max(0, E - EA) * 0.06
```

For the common single-node case (`N = 1`) the storage term reduces to
`max(0, D - 10) * 0.125`. Before using the HA form, confirm against the
current invoice how the 10 GB allowance nets against replica volumes;
the public docs state per-instance billing and no allowance for *extra*
replicas, and this worksheet does not guess beyond that.

Current shape to price at staging time (from `docs/ops.md`, host-reported
there): two clusters — the app cluster (PS-5 single-node, staging +
future prod databases on one branch) and the separate MusicBrainz mirror
cluster (reported $5/month compute; final SKU not specified there, so
confirm the SKU before pricing it as PS-5). The ~100 GB MusicBrainz
import target means the mirror cluster's storage term dominates its SKU
price; it is a capacity target in `docs/ops.md`, not yet imported data.

Volume sources: PlanetScale dashboard/billing per cluster (provisioned
disk GB, backup GB, egress GB), and query/row counts from PlanetScale
Insights only as capacity signals — rows read/written do not enter the
price formula directly; they drive SKU size and egress.

## 7. Total

```text
monthly_total = workers + r2 + kv + queues + analytics_engine
              + sum(planetscale_cluster over app + MusicBrainz clusters)
```

Staging and production share one Cloudflare account and one PlanetScale
app cluster, so included allowances are consumed account-wide (Cloudflare)
or per cluster/branch (PlanetScale), not per environment. Price each
environment by computing the account total with and without that
environment's volumes, or accept an account-level total — do not apply
the included allowances twice.

## 8. Worked example (illustrative volumes only)

These volumes are invented to make every formula hand-checkable; they are
not measured staging data and not a demand or revenue estimate.

Inputs: `R` = 12M requests, `C` = 50M CPU-ms; R2 `G` = 500 GB-months,
`A` = 2M, `B` = 20M; KV `KR` = 15M, `KW` = 0.5M, `KD` = `KL` = 0,
`KG` = 2 GB; Queues `M` = 2M messages ≤64 KB (`Ops` = 6M); Analytics
Engine `W` = 12M, `Q` = 0.5M; PlanetScale: two PS-5 single-node clusters
at $5, app disk `D` = 20 GB, MusicBrainz disk `D` = 110 GB, backups
inside 2× disk, egress `E` = 30 GB (app) + 100 GB (MusicBrainz) against
`EA` = 10 GB each (PS-5 non-HA allowance, assumed for both branches).

| Service | Arithmetic | $/month |
|---|---|---|
| Workers | 5.00 + (12−10)×0.30 + (50−30)×0.02 | 6.00 |
| R2 | (500−10)×0.015 + (2−1)×4.50 + (20−10)×0.36 = 7.35 + 4.50 + 3.60 | 15.45 |
| KV | (15−10)×0.50 + (2−1)×0.50 | 3.00 |
| Queues | (6−1)×0.40 | 2.00 |
| Analytics Engine | (12−10)×0.25 | 0.50 |
| PlanetScale compute | 2 × 5.00 | 10.00 |
| PlanetScale storage | (20−10)×0.125 + (110−10)×0.125 = 1.25 + 12.50 | 13.75 |
| PlanetScale egress | [(30−10) + (100−10)] × 0.06 | 6.60 |
| **Total** | | **57.30** |

Spot-check (R2, by hand): 490 billable GB-months × $0.015 = $7.35;
1M billable Class A × $4.50/M = $4.50; 10M billable Class B ×
$0.36/M = $3.60; $7.35 + $4.50 + $3.60 = $15.45. ✓

Sensitivities worth watching once real volumes land: R2 Class A
operations ($4.50/M) and KV writes ($5.00/M) are the steepest per-unit
slopes; PlanetScale storage grows linearly with the MusicBrainz mirror
($0.125 per GB-month per instance); Workers CPU is comparatively flat
($0.02 per million CPU-ms).

## 9. Plugging in staging volumes (later, not part of this worksheet)

1. Pull one full calendar month (or normalize a shorter window) of the
   volume sources named in each section.
2. Substitute into the formulas; keep the raw inputs next to the result
   so the estimate stays auditable.
3. Re-check every unit price against its source link on the day of the
   estimate and update the "checked" dates here if any price changed.
4. Add the out-of-scope services named at the top before quoting a total
   as a complete infrastructure cost.
