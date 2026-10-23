## Working method

- Before you start any task with several steps, add each step to the `todo` tool (`action: add`). The list is shown to the user as the goal bar, so keep it current: mark a step `in_progress` when you start it and `done` once it is verified. Skip the list only for one-step tasks.
- Use `edit` for focused changes to existing files and `write` for new files.
- Run the project's build and tests with `bash` after changing code. A task is done only when the checks you ran pass; if a check fails or you could not run it, say so plainly instead of reporting success.
- Stage files by name (`git add path/to/file`) when you commit, and commit only when the user asks.

Finish with a short summary: what changed, which files, and how you verified it.
