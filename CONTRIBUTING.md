# Contributing

Report bugs and ask for features in [GitHub issues](https://github.com/EquityBot/PlutarchPages/issues).
Report a potential security vulnerability privately, as [SECURITY.md](SECURITY.md) describes.

## Pull requests

Open pull requests against `main`. [AGENTS.md](AGENTS.md) holds the rules every contributor
follows, and the [pull request template](.github/pull_request_template.md) lists what to check.

## Checks

CI runs the full gate on every pull request and every push to `main`:

```sh
cargo xtask ci    # fmt --check, clippy -D warnings, tests, deny and the repository checks
```

Locally, the crates you changed are enough: `cargo fmt --check`, clippy with `-D warnings` and
`cargo test -p <crate>`.

## Tests

New functionality and bug fixes come with automated tests in the crate's unit tests or its
`tests/` directory, run by `cargo xtask test` in CI. A bug fix's test must fail without the fix.
