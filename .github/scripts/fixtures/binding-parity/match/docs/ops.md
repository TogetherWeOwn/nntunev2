# Fixture ops inventory (synthetic; proves the binding-parity CI job)

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
