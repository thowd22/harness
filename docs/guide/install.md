# Install

harness is a single binary. Prebuilt binaries for Linux (x86_64, aarch64;
static) and macOS (arm64, x86_64) are attached to every
[GitHub release](https://github.com/thowd22/harness/releases), each with a
`.sha256` file.

## One-line install

```sh
curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash
```

Piping a script into a shell runs it as you, so
[read install.sh](../../install.sh) first; it is about a hundred lines. It:

1. picks the release target from `uname`: `x86_64-unknown-linux-musl`,
   `aarch64-unknown-linux-musl`, `aarch64-apple-darwin` or
   `x86_64-apple-darwin` (an x86_64 shell under Rosetta gets the arm64
   build). Any other platform stops with a message and the
   `cargo install` command below;
2. looks up the latest release tag (unless `HARNESS_VERSION` is set) and
   downloads `harness-X.Y.Z-<target>.tar.gz` and its `.sha256` with `curl`
   or `wget`;
3. checks the sha256 with `sha256sum` or `shasum` and stops, installing
   nothing, if it does not match;
4. copies the binary next to its destination and renames it into place, so
   the swap is atomic and re-running the script upgrades safely;
5. prints a hint if the install directory is not on your `PATH`.

It never uses sudo. The whole script is inside a `main` function called on
its last line, so a download cut short runs nothing.

| Variable | Default | Meaning |
|---|---|---|
| `HARNESS_VERSION` | latest release | version to install, `0.2.0` or `v0.2.0` |
| `HARNESS_INSTALL_DIR` | `~/.local/bin` | directory to install `harness` into |

```sh
curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | HARNESS_VERSION=0.2.0 bash
curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | HARNESS_INSTALL_DIR=~/bin bash
```

Every download is HTTPS-only, redirects included: curl runs with
`--proto '=https' --tlsv1.2`, and with wget the script follows redirects
itself and refuses any that is not `https://`. The download line prints the
full URL.

`HARNESS_RELEASES_URL` and `HARNESS_API_URL` exist only so
`tests/install-test.sh` can point the script at a local fake release server.
They change where both the binary and its checksum come from, so the script
refuses to run when either is set unless `HARNESS_INSTALL_TEST=1` is set too,
and then prints a `WARNING` on stderr naming the URL. Only in that test mode
may the first request be plain http; redirects must still be HTTPS.

### Uninstall

```sh
rm ~/.local/bin/harness          # or $HARNESS_INSTALL_DIR/harness
rm -r ~/.config/harness          # optional: your harness.ini
```

### Manual download

See [docs/releasing.md](../releasing.md#release-assets) to verify and
install a release asset by hand.

## From source

Build from this repository with the pinned Rust toolchain
(`rust-toolchain.toml`; rustup installs it automatically), or on a platform
without a prebuilt binary:

```sh
cargo install --path crates/harness --locked
# or, without a checkout:
cargo install --git https://github.com/thowd22/harness --locked harness
harness --version
```

## Configure

Create `~/.config/harness/harness.ini` with at least one provider and
make it private, because it may hold credentials:

```ini
[general]
default_model = sonnet

[provider.anthropic]
api_key_env = ANTHROPIC_API_KEY
```

```sh
chmod 600 ~/.config/harness/harness.ini
harness config check
harness doctor
```

Instead of writing the provider section by hand you can run `harness login`
with the provider: it asks for the key, checks it with the provider and saves
it to `harness.ini` with mode 0600. For OpenAI it also offers Sign in with
ChatGPT, which uses a ChatGPT Plus or Pro plan instead of API billing. Inside
the TUI, `/login` does the same in a modal. See
[providers](../providers.md) for every option.

```sh
harness login anthropic
harness login openai --chatgpt
```

`harness config check` validates the file (every problem with file:line);
`harness doctor` also checks credentials, provider connectivity, the
sandbox, remote hosts and prompt lint, and prints a fix for each problem.

Start the TUI with `harness` in a project directory, or run one task
headless:

```sh
harness run "add a --verbose flag to the CLI"
```

The first time harness sees a project with a `.harness/` directory it asks
you to trust it; until then project prompts, agents and workflows are
ignored.
