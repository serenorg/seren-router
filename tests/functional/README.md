<!-- ABOUTME: Documents the one-time setup for live seren-router functional gates.
ABOUTME: Makes clear that these tests use real processes and never network mocks. -->

# Live functional tests

These ignored tests run the real Rust router, the pinned stock agentgateway binary, and
a real OpenAI-compatible model server. Ordinary `cargo test` runs do not need a model
server.

## LM Studio

Install and start LM Studio, then load a small chat model with a stable API identifier:

```bash
lms server start --port 1234 --bind 127.0.0.1
lms load <model-key> --identifier seren-functional -y
./scripts/fetch-sidecar.sh
SEREN_TEST_MODEL=seren-functional \
  cargo test --test functional -- --ignored --test-threads=1
```

The defaults are:

- `SEREN_TEST_UPSTREAM_URL=http://127.0.0.1:1234/v1`
- `SEREN_TEST_MODEL=gemma-3-1b-it-glm-4.7-flash-heretic-uncensored-thinking_gguf`
- `SEREN_TEST_SIDECAR_BIN=sidecar/bin/agentgateway`

Override the model unless the default model is already loaded under that exact
identifier.

## Ollama

Start Ollama's OpenAI-compatible server, pull a chat model, and point the harness at
its `/v1` endpoint:

```bash
ollama serve
ollama pull <model>
./scripts/fetch-sidecar.sh
SEREN_TEST_UPSTREAM_URL=http://127.0.0.1:11434/v1 \
SEREN_TEST_MODEL=<model> \
  cargo test --test functional -- --ignored --test-threads=1
```

## Lifecycle

Every test allocates its own loopback ports, compiles an isolated test registry, polls
agentgateway's real `GET /healthz/ready` endpoint, and owns all child/task/temp state.
Teardown stops the router and sidecar, waits for the child, removes artifacts, and
verifies that all runtime ports can be rebound. The deliberately dead provider also
uses an OS-assigned unused port.
