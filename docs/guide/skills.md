# Skills

A skill is a Markdown file of instructions you invoke as a slash command:
`~/.config/harness/skills/release-notes.md` (or `skills/<name>/SKILL.md`).

```markdown skill
---
name: release-notes
description: Draft release notes from commits since the last tag
args: "[since-tag]"
model_invocable: true
---
Collect the commits since $1 (default: the latest tag) with `git log`,
group them into Features, Fixes and Other, and draft release notes.
$ARGUMENTS
```

`/release-notes v1.2` sends the body as your message with `$1` and
`$ARGUMENTS` filled in. Without frontmatter the name is the file name and
the description is the first line of text. `model_invocable: true` lists the
skill (name and description only) in the system prompt so the agent can load
it with `read` when it applies.

When a global and a project skill share a name, both stay reachable as
`/global:name` and `/project:name`.
