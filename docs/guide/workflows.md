# Workflows

A workflow is a YAML file of steps run as a graph; each step is a node on
the agent map. It becomes a slash command (`command`, default `name`).

```yaml workflow
name: fix-bug
description: Reproduce, fix and prove a bug fix
command: fix
inputs:
  issue: { type: string, required: true, positional: 0 }
goal: "Fix: {{ inputs.issue }}"
steps:
  - id: repro
    agent: coder
    prompt: "Write a failing test that reproduces: {{ inputs.issue }}. Do not fix it yet."
    scope: ["tests/**"]
    outputs: [test_name]
  - id: fix
    needs: [repro]
    agent: coder
    prompt: "Make {{ steps.repro.outputs.test_name }} pass with a minimal change."
  - id: review
    needs: [fix]
    agent: reviewer
```

`/fix "parser crashes on empty input"` runs it. Step types include agent
steps, `shell`, `verify`, `bugfix-proof` and `approval`; see the YAML spec
(doc-3) for every key.

The built-in `dream` workflow holds the memory consolidation settings
([dreaming.md](../dreaming.md)); override it like any other workflow.
