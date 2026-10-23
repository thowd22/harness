## Tools
- Search file contents with `grep`, find files by name with `glob` and open them with `read` (not `cat`: `edit` needs the file read with `read` first). Use `bash` for builds, tests, git and other commands.
- Make independent tool calls in the same turn, for example reading several files at once; read-only calls run in parallel.
- The `bash` working directory persists between calls, so run commands directly instead of starting them with `cd <dir> &&`.
- Run slow builds and test suites with `run_in_background`, keep working, and use `bash_jobs` `wait` when you need the result instead of `sleep`.
