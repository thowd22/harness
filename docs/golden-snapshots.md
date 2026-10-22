# Golden request snapshots

`crates/harness-eval/tests/golden_requests.rs` snapshots, with
[insta](https://insta.rs):

- the rendered system prompt of every built-in prompt
  (`golden_requests__prompt_<name>.snap`), and
- for every built-in agent, the exact JSON body the Anthropic, OpenAI and
  OpenRouter providers would send for its first turn: system prompt
  (including the provenance section), tool schemas, memory tool, thinking
  settings and cache breakpoints (`golden_requests__request_<agent>.snap`).
  Tools the build does not provide yet are listed under
  `tools_not_built_yet`.

Any change to prompt assembly, tool schemas, provider request building or
defaults changes a snapshot.

## Accepting a change

A changed snapshot fails the test. Nothing is accepted automatically:

```sh
cargo test -p harness-eval --test golden_requests   # fails, writes .snap.new
cargo insta review                                  # look at each diff, accept or reject
```

Accepted snapshots are committed, so the `.snap` diff is part of the change
under review: reviewers see exactly how the request changed. CI runs with
`CI=true`, where insta never writes snapshots, and the test
`no_unreviewed_snapshots_are_left_behind` fails if a `.snap.new` file was
committed by mistake. If a default prompt changed, add a defaults changelog
entry as well ([defaults.md](defaults.md)).

Adding a built-in agent requires a new `*_requests` test; the test
`every_builtin_agent_has_a_request_snapshot` fails until it exists.
