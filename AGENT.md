<!-- ABOUTME: Operating contract for agents implementing and maintaining seren-router.
ABOUTME: Captures the project invariants, evidence gates, and owner-controlled boundaries. -->

# Agent Guide

## Mission

Build `seren-router`: Seren's private, OpenRouter-compatible routing layer in front of a
pinned stock `agentgateway` sidecar. Preserve the existing Gateway and Desktop contract
while moving inference traffic to Seren-owned provider accounts.

Prefer the smallest correct implementation. Do not add speculative abstractions, a UI,
customer billing, user accounts, multi-tenancy, or a second proxy runtime.

Address the project owner as Taariq. Be direct and evidence-led: surface uncertainty,
bad assumptions, and product risks without flattery or invented estimates.

## Sources of truth

Read these before changing the associated behavior:

1. `docs/plans/20260724_plan_seren_router_build.md` — implementation tasks and gates.
2. `docs/02-routing.md`, `docs/03-provider-registry.md`, and `docs/04-billing.md` —
   routing, registry, and money-path contracts.
3. `docs/09-agentgateway-evaluation.md` — verified sidecar behavior and sharp edges.
4. `docs/00-overview.md` through `docs/08-implementation-plan.md` — product context,
   architecture, migration, and operations.

When documents conflict, follow the most recent explicit owner decision, then the
detailed build plan, then the topic design document, then the README. Do not conceal a
conflict. Resolve routine implementation gaps with the smallest reversible choice and
record it; ask Taariq before changing an external API, billing, routing, migration, or
deployment contract.

The detailed plan describes desired outcomes, not permission to preserve a known
dependency error. In particular, actual served-provider attribution must exist before
cost injection or ledger recording can be considered correct.

## Locked architecture

- Rust service based on `serenorg/seren-template-rust`; preserve its server, route,
  database, middleware, Docker, and health-check conventions.
- Pinned stock `agentgateway` sidecar. Do not fork it or reimplement provider adapters,
  connection pooling, TLS, or provider streaming transport.
- Routing policy belongs in the Seren layer; the sidecar executes provider calls and
  supplies pre-response retry/failover.
- Static bearer authentication from `SEREN_ROUTER_GATEWAY_KEY`, compared in constant
  time.
- Postgres generation ledger; `rust_decimal::Decimal` for every price and cost.
- In-memory health, EWMA throughput/latency, and traffic-share measurements; inject
  time and RNG into deterministic logic.
- OpenRouter is the initial lowest-priority fallback. Provider keys are environment
  variables referenced by name, never repository values.

Do not improvise the production cluster, DNS, database provisioning, secrets binding,
canary mechanics, Gateway cutover, provider ToS approval, or provider spend. Those are
owner-controlled operations.

## Required external contract

Preserve the OpenRouter-shaped API under `/api/v1`:

- `POST /chat/completions`
- `POST /completions`
- `GET /models`
- `GET /models/{model}/endpoints`
- `GET /generation?id=...`
- `GET /auth/key`
- `GET /credits`

The money-path invariant is `usage.cost`: report the provider's true USD cost and let
the Gateway apply its fee. Prefer a trustworthy upstream-reported cost; use registry
Decimal rates as the defined fallback. The recorded `provider_id`, reported cost, and
actual provider that served the response must agree.

Streaming responses include cost in the final usage event. Do not buffer an LLM stream
except for the minimal SSE framing needed to transform complete events. Failover is
allowed only before response bytes have been committed; never replay an established
stream after a malformed or interrupted event.

Every generated concrete sidecar route carries `health.eviction`. Preserve
`reasoning: { effort }`, `provider.sort`, `:nitro`, and `:floor` behavior defined by the
routing contract.

## Work protocol

1. Audit the relevant code paths, specifications, prior issues, and prior pull requests
   before editing. Record the audit in the issue.
2. Create an issue only after identifying a definitive feature or bug with a clear code
   path to add or repair functionality. Do not create speculative, umbrella, or
   duplicate issues.
3. Classify the issue with exactly one of `feature` or `bug`, plus
   `audited code-paths` and every affected-area label. Assign it to `taariq`.
4. Create a task-owned branch in `.worktrees/<task>` from current `main`. Do not
   implement task work in the primary checkout.
5. Read the complete active milestone and its linked specification. Implement in
   dependency order; if a later numbered task is required for correctness, move it
   forward and explain the dependency in the issue and pull request.
6. Add only the critical contract or regression coverage that protects the changed
   behavior. Do not add broad TDD exercises, duplicate coverage, or tests unrelated to
   a demonstrated risk.
7. Open a narrowly scoped pull request linked with `Closes #<issue>`. Include a
   reviewer walkthrough, applicable check results, and functional evidence.
8. After opening the pull request, perform the real functional walkthrough. Classify
   every finding as P0, P1, P2, or non-blocking.
9. Give every P0, P1, or P2 regression or missed functional bug its own assigned,
   labeled issue and fix it through the same worktree and pull-request workflow. Add
   only the critical regression coverage, rerun applicable Rust/CI checks, and repeat
   the live walkthrough on the affected OS.
10. Merge only when applicable checks and required walkthroughs pass. Then remove
    task-owned stale worktrees and merged local and remote branches.
11. Return the primary checkout to current `main`. If it contains unrelated changes,
    preserve them with `stash`, fast-forward with `pull --ff-only`, then `stash pop`.
    Never lose or absorb another person's changes.

Use evidence-first debugging: capture the exact failure, reproduce it, locate the
failing layer, state one hypothesis, and test the smallest discriminating change. Read
the pinned sidecar schema and source before guessing about its configuration.

### Issue and pull-request content

Issues must state the type, problem, audit scope, exact reproduction path for bugs,
expected and actual behavior, evidence or logs, suspected code path, acceptance
criteria, and network-path audit. Feature issues describe the improved functionality
and its concrete code path; do not label them as bugs.

Pull requests must be understandable without reading the entire diff. Include:

- the linked issue and a one-paragraph outcome;
- the implementation boundary and important design decisions;
- a short, ordered file/code-path walkthrough for Taariq;
- exact commands and results for checks;
- the live functional path exercised, OS, and evidence;
- P0/P1/P2 findings and their linked follow-up issues, or an explicit statement that
  none were found.

Keep unrelated changes in separate issues and pull requests so feature and bug filters
remain meaningful.

### Networked feature paths

For every changed networked path, trace the action from its caller or UI entry point
through each outbound publisher/API slug, HTTP method, and path. Verify Seren publisher
slugs and operations against the live publisher list and enumerated metadata; verify
other APIs against official metadata. Record the verified chain in the issue and pull
request. If the changed feature has no networked path, state that explicitly.

## Test rules

- Unit tests are deterministic and cover pure behavior: parsing, validation, config
  generation, cost arithmetic, SSE framing, and policy math.
- Use checked-in goldens for generated YAML and stable wire-format fixtures.
- Functional gates use the real pinned sidecar and a real LM Studio or Ollama upstream.
  A verified-unused TCP port is the failure target. Local and in-app functional
  walkthroughs must be live; never replace the sidecar, provider, or network path with
  mocks.
- Allocate test ports, bound readiness polling, and clean child processes on success,
  failure, and panic.
- Never delete, weaken, or skip a failing test merely to make a change pass.
- Treat failures introduced or exposed by the task as part of the task. Report unrelated
  baseline failures with the exact command and output.
- Expected error output must be captured and asserted so successful test output remains
  clean.

Before each task commit, run the applicable checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Also run `make check`, sidecar config validation, database tests, or ignored functional
gates when the milestone requires them.

## Code conventions

- Match the surrounding template and project style.
- Make the smallest maintainable change; avoid one-implementation traits and premature
  generalization.
- Names describe domain behavior, not implementation history. Avoid `new`, `legacy`,
  `improved`, `enhanced`, `wrapper`, and pattern names that add no domain meaning.
- New project-owned source files start with two `// ABOUTME:` lines. Do not churn copied
  template files solely to add headers.
- Comments explain enduring intent or constraints, never refactoring history.
- Do not hand-edit formatter-owned whitespace.
- Validate external input, use parameterized SQL, and return errors without credentials
  or internal details.

## Secrets and safety

- Never commit API keys, bearer tokens, credentials, `.env` files, or secret values in
  YAML, fixtures, logs, snapshots, or errors.
- Before committing, inspect staged files and scan them for credential-like content.
- If a committed secret is found, stop, tell Taariq, and rotate or revoke it before
  continuing.
- Never expose provider keys to callers; only the sidecar/provider request may receive
  them.
- Verify repository visibility before pushing private business logic or configuration.

## Git

- Work in a task-owned `.worktrees` checkout and keep one coherent commit per issue
  when practical.
- Inspect `git status` before staging. Stage explicit paths, never unrelated files.
- Never bypass hooks, rewrite pushed history, force-push, or add AI attribution.
- Use conventional subjects. Plan-task commit bodies end with:

```text
Taariq Lewis, SerenAI, Paloma, and Volume at https://serendb.com
Email: hello@serendb.com
```

- The required issue/PR workflow authorizes routine task-branch pushes, pull-request
  creation, and merges after all gates pass. It does not authorize deployment, Gateway
  publisher mutation, production provider spend, DNS changes, or other production
  operations.

## Ask only when blocked

Proceed with routine, reversible implementation decisions. Stop and ask when a choice
changes an external contract, deletes or substantially restructures existing work,
requires credentials or production spend, performs a deployment or cutover, or depends
on missing owner/infrastructure decisions.

When blocked, state what is known, the exact decision needed, and the recommended
option. Never claim success without the required tests and gate evidence.
