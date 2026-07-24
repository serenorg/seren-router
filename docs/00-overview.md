<!-- ABOUTME: Strategic context and the top-level design decision for seren-router.
ABOUTME: Explains the OpenRouter/Stripe trigger and why we chose the invisible-swap shape. -->

# 00 — Overview & Decision

## The trigger

Stripe is in talks to acquire OpenRouter at a ~$10B valuation (WSJ, reported July 24 2026; not yet closed). OpenRouter is the marketplace that routes across ~70 inference providers behind one OpenAI-compatible API.

This matters to Seren specifically because:

1. **`seren-models` is backed by OpenRouter.** The live Gateway config for the `seren-models` publisher is a single-upstream reverse proxy:
   - `api_url: https://openrouter.ai/api/v1`
   - `auth_type: static`, `api_key_header: Authorization` (one Seren-owned OpenRouter key)
   - `gateway_fee_percent: 5.00`
   - `upstream_cost_response_path: usage.cost`
   - endpoints are verbatim OpenRouter: `/models`, `/chat/completions`, `/completions`, `/generation`, `/models/{model}/endpoints`, `/auth/key`, `/credits`

   Every public model in Seren Desktop (GPT, Claude, Gemini, Llama, DeepSeek, GLM, Kimi…) is just a `model` slug POSTed to this proxy. The multi-provider routing is entirely OpenRouter's.

2. **Stripe competes with SerenBucks.** A core dependency of Seren's billing loop would be owned by a payments company. That is a strategic risk worth removing regardless of whether the deal formally closes.

## What "replace OpenRouter" actually means

The Seren publisher abstraction is a **single-upstream** reverse proxy: one `api_url`, one static key, a markup, and metering. It has **no multi-provider routing** of its own — that capability lived entirely inside OpenRouter.

So replacing OpenRouter is not a config tweak. It requires rebuilding OpenRouter's core function: take a model slug, pick the right provider (of several serving it), call that provider directly with Seren's own key, fail over on error, and report a unified cost. That is a new service: **seren-router**.

## The decision: Shape A — invisible swap

We evaluated three shapes:

- **A) One publisher, Seren-routed underneath (chosen).** Keep a single `seren-models` publisher and one picker entry. seren-router replaces OpenRouter's aggregation with Seren's own direct-to-provider routing. Users see no new rows; the experience is identical but Seren-owned end to end.
- **B) Many user-facing publishers.** Each host (`blackbox-ai`, `together-ai`, …) becomes its own switchable publisher and picker row. Rejected: the same model appears ~16 times, and it is the largest build on both client and Gateway for little user benefit (most users do not care which host serves a model).
- **C) Hybrid — smart default + opt-in provider pinning.** A future extension of A. The routing seam is built now (see `docs/02`) so pinning can be layered on without rework.

Shape A kills the OpenRouter dependency, preserves the entire billing envelope, and touches the client only minimally. It is the lowest-risk path to owning the stack.

## Non-goals

- Not rebuilding OpenRouter's customer credit system (SerenBucks already exists; see `docs/04`).
- Not exposing 16 provider rows in the Seren Desktop UI (Shape B).
- Not changing the SerenBucks billing envelope, the 5% fee, or the publisher contract shape.
