# Contributing

## Development

```sh
cargo test                                         # server, CLI and HTTP tests (no node needed)
cargo run -p lineup -- serve --data-dir ./data     # http://127.0.0.1:9300
npm i -g corepack && corepack enable               # once
pnpm -C web install && pnpm -C web dev             # UI dev server, proxies /api and /p to :9300
```

`cargo build` embeds `web/dist`; without it, the server gets a placeholder page, so the Rust side
builds without node.

After you change the API:

```sh
cargo run -q -p lineup-server --bin openapi > web/src/api/openapi.json
corepack pnpm -C web gen:api
```

CI checks that both generated files are up to date.

## Branches

Name a pull request's branch `feat/…` or `fix/…`; the prefix sets the label that puts the pull
request in a release notes section:

| Prefix | Label | Section |
|---|---|---|
| `feat/` | `enhancement` | Features: anything new or changed, including CI and tooling |
| `fix/` | `bug` | Fixes: anything broken, including builds and tests |
| `docs/` | `documentation` | Documentation: only for pull requests that change nothing but docs |
| `release/` | `skip-changelog` | left out: the version bump of a release |

Renovate labels its own pull requests `dependencies`. The label is set when a pull request opens;
to move one to another section, change its label.

## Releases

Push a `v*` tag; a tag with a `-`, like `v1.0.0-rc.1`, becomes a pre-release. The release workflow:

1. runs the same checks as CI;
2. builds `ghcr.io/<owner>/lineup` and `ghcr.io/<owner>/lineup-gateway` for amd64 and arm64;
3. builds archives of `lineup`, the client-only `lineup-cli` and `lineup-gateway` for macOS and
   Linux, x86_64 and arm64 (the Linux gateway is a static musl build; `lineup` and `lineup-cli`
   target glibc);
4. installs the Homebrew formula from those archives, and only then publishes the GitHub release
   with the archives, `SHA256SUMS` and notes;
5. for a stable release, updates `Formula/lineup.rb` in `alexey-lapin/homebrew-tap`, never
   replacing a newer version.

The notes (`.github/scripts/release-notes.sh`) start with `docs/releases/<tag>.md` when the tag has
that file, then the install instructions, then the changes: GitHub's notes generated from the
merged pull requests' labels (`.github/release.yml`), or, when GitHub cannot generate them, the
commits since the previous tag.

To rebuild an existing tag, run the workflow by hand with that tag; tick `cli` to rebuild the
archives and refresh the release files too, and `tap` as well to publish the formula again.
