# Contributing

Name a pull request's branch with a prefix; it sets the label that puts the pull request in a
release notes section:

| Prefix | Label | Section |
|---|---|---|
| `feat/` | `enhancement` | Features: anything new or changed, including CI and tooling |
| `fix/` | `bug` | Fixes: anything broken, including builds and tests |
| `docs/` | `documentation` | Documentation |
| `deps/` | `dependencies` | Dependencies: version updates only |
| `release/` | `skip-changelog` | left out |

Other prefixes get no label and land in Other changes; use `feat/` or `fix/` instead. The label is
set when the pull request opens. To move one to another section, change its label.
