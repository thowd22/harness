# Workflows

A workflow is a YAML file of steps run as a graph; each step is a node on
the agent map. It becomes a slash command (`command`, default `name`).

Add one by saving `<name>.yaml` in `~/.config/harness/workflows/` (for you)
or in a trusted project's `.harness/workflows/` (for the project). It
hot-reloads; run it with `/<command>` or `harness wf run <name>`.
