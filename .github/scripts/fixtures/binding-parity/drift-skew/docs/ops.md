# Fixture ops inventory (synthetic; proves the binding-parity CI job)

### KV namespaces (fixture)

| Namespace | ID | Env |
|---|---|---|
| `fixture-state-staging` | `fixture-kv-staging` | staging |
| `fixture-state-prod` | `fixture-kv-prod` | prod |

### Hyperdrive bindings

| Config | ID | Intended binding |
|---|---|---|
| `demo-staging` | `fixture-hyperdrive-staging` | staging app |
| `demo-prod` | `fixture-hyperdrive-prod` | prod app |

## Environments matrix

| Concern / intended consumer | staging | prod |
|---|---|---|
| R2 audio / demo Worker | `fixture-audio-staging` | `fixture-audio-prod` |
| Ingest Queue | `fixture-ingest-staging` | `fixture-ingest-prod` |
