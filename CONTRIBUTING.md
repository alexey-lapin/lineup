# Contributing

Name a pull request's branch with a prefix; it sets the label that puts the pull request in a
release notes section:

| Prefix | Label | Section |
|---|---|---|
| `feat/` | `enhancement` | Features |
| `fix/` | `bug` | Fixes (user-facing bugs) |
| `docs/` | `documentation` | Documentation |
| `deps/` | `dependencies` | Dependencies |
| `release/` | `skip-changelog` | left out |
| anything else, e.g. `chore/`, `ci/`, `refactor/`, `test/` | none | Other changes |

The label is set when the pull request opens. To move one to another section, change its label.
