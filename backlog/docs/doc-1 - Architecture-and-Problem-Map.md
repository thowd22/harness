---
id: doc-1
title: Architecture and Problem Map
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-22 14:26'
---
# Architecture & Problem Map

`harness` is a terminal-native (Rust + ratatui) coding-agent harness. The model is treated as interchangeable; the harness — tool orchestration, context, permissions, verification, hygiene, instructions — is the product.

## Design principles

1. **Core is headless.** Everything (agent loop, workflows, memory, remote, verification) lives in library crates that emit typed events. The TUI is one consumer; the headless CLI, eval runner and remote helper are others.
2. **Event-sourced runs.** Every run is an append-only `events.jsonl` + blob store. All views (chat, map, detail, history, replay, evals) are folds over events. Nothing is shown that isn't in the log, and nothing important is missing from it.
3. **Enforce in code, not in prose.** Scope, protected paths, staging rules, budgets and verification are enforced by the harness. Prompts describe what *is* allowed, never blanket prohibitions.
4. **The repo can ask; only the user can grant.** Project YAML (`.harness/`) can tighten policy, never loosen it. Loosening lives only in the user-owned INI. Project config loads only after explicit trust.
5. **Small by default.** Four core tools (read, write, edit, bash) with compact schemas; extra tools (spawn_agent, memory) appear only when the agent config enables them. Initial context budget is tested in CI.
6. **Cost is a first-class metric.** Tokens and dollars are tracked per call / node / run / session and appear in the status line, the map and eval reports.
7. **Credentials never leave the local process.** Model calls are always made locally; remote boxes only receive tool operations.
8. **Simple UI.** Few keys (Enter, Esc, Tab, Ctrl+P, Ctrl+C, arrows), everything else in the command palette.

## Workspace layout (Cargo)

| Crate | Responsibility |
|---|---|
| `harness-config` | INI loader (`harness.ini`), secrets, YAML discovery/layering, schemas, trust store, lint |
| `harness-events` | Event schema (versioned), run store (JSONL + blobs), redaction, reducers (RunState) |
| `harness-providers` | Normalized message model, `Provider` trait, Anthropic / OpenAI / OpenRouter / Fake, model registry, pricing, HTTP+SSE+retry |
| `harness-tools` | `Workspace`/`Exec` traits, Local impl, read/write/edit/bash, output spill |
| `harness-policy` | Rules engine, shell parsing, protected paths, approvals, sandbox |
| `harness-agent` | Agent loop, tool dispatch, subagents, context mgmt/compaction, budgets |
| `harness-workflow` | Workflow schema, templating, DAG scheduler, gates, resume |
| `harness-hygiene` | Change ledger, checkpoints (shadow git), scope, hygiene report, verify runner, review |
| `harness-memory` | Memory drive (git), index/search, memory tool, extraction, dreaming |
| `harness-remote` | SSH transport, remote helper protocol, remote Workspace impl, sync, host scheduling |
| `harness-tui` | ratatui app: chat, map, detail, palette, approvals, diff |
| `harness-eval` | Eval suite format, runner, baseline comparison |
| `harness` (bin) | CLI entry: `harness` (TUI), `harness run`, `harness wf`, `harness eval`, `harness doctor`, `harness remote-agent --stdio` |

## Data flow

```
 user input / slash cmd ──► Command registry ──► Session
                                                  │
           ┌──────────── Agent loop ◄─────────────┘
           │   build request (stable prefix: system → tools → memory index → skills list → history)
           │   Provider.stream() ──► normalized deltas ──► events
           │   tool calls ──► Policy (allow/ask/deny, scope, sandbox) ──► Workspace (Local | Remote/SSH)
           │                                     └─► Change ledger / checkpoints
           │   end_turn ──► Verify gate ──► Hygiene report ──► Done (verified | unverified)
           ▼
     EventBus ──► RunStore (events.jsonl, blobs/) ──► Reducer ──► TUI views / headless JSON / evals
```

Workflows wrap this: the DAG scheduler launches agent/shell/verify/approval/workflow steps, each producing a node in the same event stream. Subagents are child nodes spawned by `spawn_agent`.

## Problem → mitigation map

Labels `problem-1` … `problem-9` on tasks trace back to this table.

| # | Problem | Mitigations (milestone) |
|---|---|---|
| 1 | Tool/API orchestration failures | Normalized provider layer + conformance fixtures; tool-arg schema validation, JSON repair, actionable tool errors; retries/backoff/stall detection; process-group kill, timeouts, output spill (M2, M3) |
| 2 | Instruction conflicts / over-literal reading | Positive-scope default prompts; policy enforced in code; prompt lint for blanket prohibitions & cross-layer contradictions; `/context` inspector; trap evals (M5, M12) |
| 3 | Change hygiene / over-engineering | Change ledger, scope globs, hygiene report (temp/debug files, unrelated edits, churn), git staging guard, checkpoints & /undo, independent reviewer with over-engineering rubric (M8) |
| 4 | Context loss, context anxiety, weak memory | Stable cache-friendly prefix, thinking preservation tests, tool-result elision, structured handoff compaction, never exposing remaining-context to the model, Devin-style memory drive + dreaming (M9, M10) |
| 5 | Weak self-verification, regression tax | Verify baseline/after with failing-set diff, done-gating, bug-fix proof (fail-before/pass-after/revert), independent checker agent, honest verified/unverified status (M8) |
| 6 | Token inefficiency / harness tax | 4 tools, compact schemas, initial-context CI budget, subagents get briefs not full prompts, prompt caching, budgets, cost in every view and eval (M2, M6, M9, M12) |
| 7 | Approval fatigue / control | Tiered policy, session-pattern approvals, autonomy modes, outward-action guard (push/publish), explicit per-agent model selection (no black-box router), batched approvals (M3, M6) |
| 8 | Security / boundary violations | Protected paths (incl. harness.ini, .harness/, trust store, memory internals), secrets redaction & env scrub, project trust, landlock/bwrap sandbox, untrusted-content labeling, remote: creds stay local (M1, M3, M11) |
| 9 | Configuration fragility | Config hash stamped on every run, golden request snapshots, eval baselines, `harness doctor`, effective-config view, defaults changelog (M12) |

## Key decisions (ADR summary)

- **ADR-1 INI for harness config.** One `harness.ini` holds everything user-owned: providers + credentials, models/aliases, pricing overrides, keys, UI, policy, sandbox, budgets, memory, remotes. YAML is for *authored* artifacts (prompts, agents, workflows). Skills are Markdown.
- **ADR-2 Event sourcing.** Enables map view, replay, resume, evals and debugging from a single source of truth.
- **ADR-3 Remote = remote tools, local brain.** Model calls stay local; SSH carries only tool ops via a small JSON-RPC helper. Uses system OpenSSH (respects `~/.ssh/config`, agents, ProxyJump) with ControlMaster multiplexing.
- **ADR-4 Memory = git-backed Markdown drive** modeled on Devin Memory + Dreaming, stored locally.
- **ADR-5 Workflows look like GitHub Actions** (steps, needs, if, matrix, outputs, retries, runs_on) so the mental model and the visualization map onto each other.
- **ADR-6 Policy precedence:** built-in hard denies > ini > agent/workflow YAML (tighten-only) > session approvals.

## Non-functional targets

- Cold start to interactive TUI < 150 ms; keystroke-to-render < 16 ms; idle CPU ~0 (render on dirty only).
- Map view of 200+ nodes renders in < 16 ms.
- Default agent initial context (system + tools + memory index) < 4k tokens, asserted in CI.
- No panics leave the terminal in raw mode.
