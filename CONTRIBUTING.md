# Contributing

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
