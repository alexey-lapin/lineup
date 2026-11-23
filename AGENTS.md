# AGENTS.md

lineup is a Rust workspace (`crates/`: the `lineup` server and CLI, one binary, and `lineup-gateway`)
with a Vue web UI in `web/`, embedded into the server at build time.

- `docs/spec.md` is the behaviour contract: data model, outputs, events, API, CLI contract. Read
  the sections a change touches before you change the code.
- `docs/deploy.md` (lineup) and `docs/gateway.md` (lineup-gateway, including the rule config
  reference) are the operator guides.
- `CONTRIBUTING.md` has the development commands, the generated-file steps, branch prefixes and the
  release process.

## Boundaries

- lineup knows no streaming protocol or site. Anything specific to a site, service or protocol
  belongs in a gateway rule (config) or `crates/gateway`. lineup's server, CLI, API docs, web UI
  and docs speak only of routes, gateways, rules and refs.
- `crates/gateway-contract` is the wire contract between lineup and any gateway. Its JSON fixtures
  are the examples other gateways are written against; keep them in step with the types.
- Serving playlists and guides never calls a gateway.

## Public repository

Every host, address, channel and id in code, tests, fixtures and docs is synthetic: `media-host`,
`gateway-host`, `10.0.0.x`, `example.com`, made-up channel names. Plans, review briefs and scratch
notes stay outside the repository.

## v0

lineup is v0 and breaks freely between versions: write the simplest change, with no compatibility
code, fallbacks for older releases, upgrade notes or comments about old behaviour.

The one fixed point is the schema history: a schema change is a new file in
`crates/server/migrations`, because sqlx checksums applied migrations. Queries are runtime sqlx
(no `query!` macros), so every query needs a test.

## Making a change

- **API**: after changing a handler or `crates/api-types`, regenerate `web/src/api/openapi.json`
  and `web/src/api/schema.d.ts` (CONTRIBUTING.md). CI fails on stale copies.
- **Behaviour**: update `docs/spec.md` and whichever of README, deploy.md and gateway.md describe
  it, in the same pull request. `lineup --help` is the CLI reference; docs show examples and the
  contract, not full command listings.
- **Home-screen app**: iOS reads the `apple-mobile-web-app-*` meta tags when the icon is added to
  the home screen. After changing them, remove the icon and add it again before judging the result.
- **Tests**: server and CLI tests use `lineup_server::testing::StubGateway` (feature
  `test-support`) in place of a real gateway.

A change is done when what CI runs for the parts you touched passes locally:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy -p lineup --no-default-features --locked -- -D warnings   # client-only CLI
cargo test --locked
corepack pnpm -C web build && corepack pnpm -C web test                # web/ changes
```

## Commits and pull requests

- Branch names carry the prefix that labels the pull request for release notes (CONTRIBUTING.md):
  `feat/` or `fix/` for any change, `docs/` for docs only, `release/` for a version bump.
- Commit titles are short sentences without a type prefix ("Drop pre-0.6 compatibility"); the
  body says what changed and why, wrapped at 72 columns.
- Commits and pull requests carry no AI attribution: no `Co-Authored-By`, session links or
  "Generated with" lines.
- Hand-written release notes go in `docs/releases/vX.Y.Z.md`. The release prints them first, above
  the install section and the generated changelog; they add to the changelog, not replace it.
