<!-- ABOUTME: Defines the validated two-container deployment boundary for seren-router. -->
<!-- ABOUTME: Separates repository-owned artifacts from owner-controlled production choices. -->

# 10 — Deployment Boundary

M7 packages the cluster-neutral pieces needed to deploy seren-router. It does **not**
choose or mutate a cluster, namespace, DNS name, managed database, secrets backend,
canary split, or Seren publisher. Those remain owner-controlled decisions.

## Validated pod shape

One Kubernetes pod contains two long-running containers and one init container:

1. **Config renderer init container** — the production seren-router image runs:

   ```text
   /app/seren-router render-sidecar-config /config/agentgateway.yaml
   ```

   It reads `SEREN_ROUTER_REGISTRY_PATH` from a ConfigMap mount, validates the registry,
   compiles the AgentGateway YAML, and atomically replaces the file on a shared
   `emptyDir`. The output contains `$SEREN_ROUTER_KEY_*` environment references, never
   resolved secret values.

2. **AgentGateway sidecar** — the official image runs:

   ```text
   /app/agentgateway -f /config/agentgateway.yaml
   ```

   It receives provider-key environment variables from Kubernetes Secrets and listens
   on localhost ports 4000 (LLM) and 19001 (readiness).

3. **seren-router app** — the production image runs its default command on port 8000,
   receives the Gateway key and database URL from Secrets, and reads the same registry
   ConfigMap for its catalog, prices, and routing candidates.

The init container and sidecar share only the rendered-config volume. The registry is
mounted into the renderer and app. Provider credentials are mounted only into
AgentGateway; `SEREN_ROUTER_GATEWAY_KEY` and `DATABASE_URL` are mounted only into the
app. When beta validation is enabled, the app also receives the distinct
`SEREN_ROUTER_BETA_GATEWAY_KEY`; it is never mounted into AgentGateway.

The exact security context, service account, namespace, resource requests, replica
count, disruption budget, topology spread, and secret references belong in the
environment overlay after the owner selects the production cluster.

## Pinned sidecar image

The official AgentGateway v1.4.0-alpha.2 OCI index is:

```text
ghcr.io/agentgateway/agentgateway:v1.4.0-alpha.2@sha256:1513b296cba467017249f08aa63d3449396e120750fb625eb3b3b2f1a2c0b58e
```

`sidecar/PINNED_IMAGE` records the index plus Linux amd64/arm64 manifest digests.
`scripts/verify-sidecar-image.sh` reads the public GHCR manifest API and fails if the
tag, binary version, index digest, media type, or either platform digest drifts. A
duplicate Seren-built sidecar image is unnecessary.

## Runtime configuration

The app requires:

- `DATABASE_URL`
- `SEREN_ROUTER_GATEWAY_KEY`
- `SEREN_ROUTER_COMBINED_PRICE_CEILING_PER_MTOK`
- `SEREN_ROUTER_HYSTERESIS_FRACTION`
- `SEREN_ROUTER_MAX_SHARE`
- `SEREN_ROUTER_SHARE_WINDOW`

The pod-local defaults are:

- `SEREN_ROUTER_REGISTRY_PATH=registry/providers.yaml`
- `SEREN_ROUTER_SIDECAR_URL=http://127.0.0.1:4000`
- `SEREN_ROUTER_SIDECAR_READINESS_URL=http://127.0.0.1:19001/healthz/ready`

`SEREN_ROUTER_BETA_GATEWAY_KEY` is optional. If present, it must be nonempty and
different from `SEREN_ROUTER_GATEWAY_KEY`.
Removing it and disabling beta-only registry rows is the beta isolation rollback; it
does not alter the production OpenRouter route set.

The readiness URL accepts only HTTP(S), must have a host, and rejects credentials,
queries, and fragments.

## Probes and shutdown

- `GET /livez` is dependency-free. It remains 200 when PostgreSQL or AgentGateway is
  unavailable so a sidecar outage does not cause an app restart loop.
- `GET /readyz` requires a successful AgentGateway readiness response. PostgreSQL is
  an asynchronous generation-ledger dependency: its `starting`, `ok`, or `degraded`
  state is included in the readiness JSON but does not remove an otherwise healthy
  inference pod from service. A sidecar failure returns 503 with the stable reason
  `sidecar`.
- The app image health check targets `/readyz`.
- SIGTERM drains application connections for up to 30 seconds.

The ingress/load balancer idle timeout for chat and completion routes must be at least
10 minutes. The app already applies a 10-minute inference timeout; a shorter upstream
proxy timeout would still truncate valid streams.

## Local production smoke

The smoke builds the production app image, uses that image for the init renderer,
starts the real official sidecar by immutable digest plus PostgreSQL 17, and shares a
network namespace between app and sidecar to reproduce pod-localhost behavior:

```bash
./scripts/verify-sidecar-image.sh
./scripts/deployment-smoke.sh
```

It verifies renderer exit status, secret references without resolved values,
PostgreSQL migrations, inference `/readyz`, dependency-free `/livez`, and production
`/metrics`. It uses fixture-only credentials and sends no provider request.

Production metrics also expose
`seren_router_proxy_segment_duration_seconds{endpoint,profile,provider,segment}` for successful
costed completions. The bounded `segment` values separate work before the sidecar,
sidecar response headers, first output, the response body, post-first-output time,
active Rust response transformation, and the complete request. Compare
`pre_sidecar` plus `app_processing` with AgentGateway's own processing metrics before
attributing provider or network tail latency to the proxy stack.

Database observability is exposed separately through
`seren_router_database_available`,
`seren_router_database_recovery_attempts_total{phase}`, and
`seren_router_database_operation_failures_total{operation}`. Migrations retry with
bounded backoff in the background. After recovery, the supervisor sleeps until a
ledger operation reports another failure; it does not poll PostgreSQL or keep
scale-to-zero compute awake.

Page on sustained `seren_router_database_available == 0`, a continuing increase in
operation failures, or reconciliation lag beyond the billing SLO. A brief degraded
status during a cold wake does not page while inference, AgentGateway readiness, and
automatic recovery remain healthy. Escalate immediately if database degradation is
paired with inference errors, sidecar unavailability, or unreconciled usage beyond
the accepted loss window.

The separate ignored functional gate uses the pinned host binary to stop the real
sidecar and prove `/readyz` changes to 503 while `/livez` remains 200:

```bash
DATABASE_URL=postgresql://... cargo test --test functional \
  harness::functional_composite_readiness_tracks_real_sidecar_lifecycle \
  -- --ignored --exact --test-threads=1
```

## Owner-controlled next step

After the cluster, database, secrets binding, DNS, and canary mechanism are selected,
an environment overlay can instantiate this pod contract. Deployment and the Phase 2
publisher canary require explicit approval; neither is performed by repository tests.
