# Project trust

A repository can ship its own prompts, agents, workflows and skills in a
`.harness/` directory at its root. That directory comes from whoever wrote
the repository, so harness **ignores it until you trust the project**.
Opening a malicious repository therefore cannot run or load anything from it.

## What happens

| Project state | Behaviour |
|---|---|
| No `.harness/` | Nothing to trust. Global config only. |
| Not trusted yet | harness shows what `.harness/` contains and asks. Until you accept, the session uses your global config only. |
| Trusted, unchanged | `.harness/` is loaded. |
| Trusted, but changed since | harness shows a diff against what you trusted and asks again. Until you accept, the session uses global config only. |

When you trust a project, harness records a SHA-256 fingerprint and a text
copy of every file in `.harness/`. A changed file, a new file or a deleted file
all change the fingerprint. If the files change between the prompt and your
answer, harness asks again, so you only ever trust what you were shown.

Notes:

- A `.harness` that is itself a symlink is rejected.
- A symlinked file inside `.harness/` is fingerprinted by the content it
  points to.
- A symlinked directory inside `.harness/` is fingerprinted by its target path
  and is never entered.
- If the trust store cannot be read, harness treats the project as untrusted.

## The trust store

Records live in `<data_dir>/trust/projects/`, one JSON file per project
(mode `0600`). The store is a protected path: agent tools can neither read
nor change it, so an agent cannot trust a project for you. Revoking trust
removes the record, and the next session asks again.
