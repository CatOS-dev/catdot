# Catdot contribution rules

## Scope

- Work only inside this repository unless the user explicitly authorizes another repository.
- Do not commit generated build output, package archives, or makepkg work directories.
- Preserve the product boundaries documented in `README.md` and the accepted Catdot design.

## Development process

- Before adding a test, state which real behavior or regression it protects.
- Add a failing regression test before changing implementation behavior.
- Do not replace behavioral tests with assertions over fixed source strings or configuration text.
- Keep each repair independently reviewable and run the relevant focused tests before the full test suite.
- Do not hide incomplete behavior behind documentation claims.

## Commits

Use Conventional Commits:

```text
<type>(<scope>): <summary>
```

Keep the first line under 72 characters. Use `fix` for verified bugs, `feat` for new product behavior, `test` for test-only changes, `refactor` for behavior-preserving restructuring, `docs` for documentation, and `chore` for repository or packaging maintenance.

Each repair stage must end in its own commit. Do not combine unrelated fixes, and do not rewrite or squash the established baseline commit.

Formatting-only changes must always use a separate `style` commit. Lockfile-only updates, documentation-only changes, generated metadata, and other changes unrelated to runtime behavior must likewise be committed separately from implementation changes.

## Required checks

Before reporting completion, run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Never run a test that installs or removes real packages on the host. Package
installation and prune integration tests must run inside a disposable container.
