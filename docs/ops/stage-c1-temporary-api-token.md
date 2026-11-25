# Stage C1 temporary Cloudflare API token

This worked example uses placeholders for the dedicated PostgreSQL trial.
The assigned C1 executor issues one account-owned API token at C1 time,
once the existing setup prerequisites pass, including the merged disposable
proof. Use it only for the approved trial operations. The token is separate
from the trial's Access service tokens.

## Issue and verify

Use the approved account key in secure custody only to create and delete
this temporary token. For `POST /accounts/<trial-account-id>/tokens`, use the
name `nntune-trial-c1-<date>` and an explicit UTC `expires_on` at most seven
days after issuance. Record the intended policy, issue time, expiry and the
single zone ID bound to the approved DNS zone in the protected trial record.
Resolve the six group IDs against the approved names and resource types before
issuance. On success, retain `result.id` in that record and save `result.value`
directly to `<c1-secure-drop-file>` with mode `0600`. Keep the value and the
raw create response out of logs and evidence. If issuance has an unknown
outcome, hold issuance and trial use while reconciling the original attempt.
A name match alone cannot identify the attempt's token.
[Create account token](https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/create/)

With existing authorized token-metadata access in the same account, read
`GET /accounts/<trial-account-id>/tokens/<c1-token-id>` before using the
temporary token. Require `success: true` and match `result.id`, `result.name`
and `result.status` (`active`). Require `result.expires_on` to be in the
future, match the recorded expiry and be no more than seven days after
issuance. Match the full `result.policies` by `effect`, permission group ID
and resource selector, regardless of array order. This is the relevant
excerpt from a matching response.
[Get account token](https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/get/)

```json
{
  "success": true,
  "result": {
    "id": "<c1-token-id>",
    "name": "nntune-trial-c1-<date>",
    "status": "active",
    "expires_on": "<approved-utc-expiry>",
    "policies": [
      {
        "id": "<account-policy-id>",
        "effect": "allow",
        "permission_groups": [
          { "id": "<hyperdrive-write-group-id>" },
          { "id": "<cloudflare-tunnel-write-group-id>" },
          { "id": "<access-apps-and-policies-write-group-id>" },
          { "id": "<access-service-tokens-write-group-id>" },
          { "id": "<account-ssl-and-certificates-write-group-id>" }
        ],
        "resources": { "com.cloudflare.api.account.<trial-account-id>": "*" }
      },
      {
        "id": "<zone-policy-id>",
        "effect": "allow",
        "permission_groups": [{ "id": "<dns-write-group-id>" }],
        "resources": { "com.cloudflare.api.account.zone.<trial-zone-id>": "*" }
      }
    ]
  }
}
```

The five account groups are Hyperdrive Write, Cloudflare Tunnel Write,
Access: Apps and Policies Write, Access: Service Tokens Write, and Account:
SSL and Certificates Write. DNS Write belongs only to the approved zone.
The `*` values apply to the exact resource keys shown. Reject additional
policies, permission groups or resource selectors, including all-account or
all-zone selectors. Missing, malformed, denied or mismatched readback leaves
the credential unverified. Account-token `GET /accounts/<trial-account-id>/tokens/verify`
reports liveness and can include expiry, but supplies no policy or resources.
Use the exact token policy readback for the C1 record. Record only evidence
references and the identity, policy, scope and expiry verdicts.
[Verify account token](https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/verify/)

## Dispose and confirm

Dispose with the trial after the last authorized operation that uses the
token. Follow the existing schedule of same-day disposal, with lead-approved
retention of up to seven days for reruns. Use the approved account key to
`DELETE /accounts/<trial-account-id>/tokens/<c1-token-id>`. If the response
includes `result.id`, match it to the recorded ID. Confirm deletion with the
same authorized metadata access and exact account-scoped GET used above.
Require a definitive token-not-found result in the verified account scope.
Denial, timeout or an ambiguous lookup leaves disposal unverified. An expired
or disabled token returned by GET is still present. Remove the exact secure
file and confirm that the path has no remaining entry, including a dangling
symlink, without reading its contents. Record provider and file absence in the
protected disposal outcome and carry forward any unresolved item for
reconciliation.
[Delete account token](https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/delete/)
