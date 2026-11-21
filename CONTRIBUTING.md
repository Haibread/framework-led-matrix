# Contributing

Contributions are welcome through issues and pull requests on
[GitHub](https://github.com/Haibread/framework-led-matrix). The short version:
get the hooks and the tests green locally, and open a pull request.

## Development setup

You need:

- the Rust toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml),
  which `rustup` installs on the first `cargo` call
- [`pre-commit`](https://pre-commit.com/) — `pipx install pre-commit`, or
  `brew install pre-commit` on macOS
- [`actionlint`](https://github.com/rhysd/actionlint), which a hook runs
  against the workflows — `brew install actionlint` on macOS, your
  distribution's package or the release tarball on Linux

No Framework 16 is needed to develop or test: scenes are tested headless and
`ledmat --simulate` draws in the terminal. See [README.md](README.md) for
running the project.

```bash
git clone https://github.com/Haibread/framework-led-matrix.git
```

```bash
pre-commit install
```

```bash
cargo test
```

A green `cargo test` means the setup works.

## Running the tests

The whole suite, which must be green before a pull request:

```bash
cargo test --all-targets
```

One test, or the tests whose name contains a word:

```bash
cargo test snake
```

Every test runs without hardware, a display or a network. The games are
simulated for thousands of steps and checked for invariants, the render loop
runs against a mocked `Matrix`, and [`tests/packaging.rs`](tests/packaging.rs)
checks that the files in `packaging/` agree with the install steps in the
README.

New behaviour comes with tests, and a fix with a regression test.

## Pre-commit hooks

Hooks must pass before a commit is pushed. They are listed in
[`.pre-commit-config.yaml`](.pre-commit-config.yaml): general file hygiene,
`yamllint`, `actionlint`, `cargo fmt`, `cargo clippy` with warnings denied, and
`cargo test`.

```bash
pre-commit run --all-files
```

```bash
pre-commit run cargo-clippy --all-files
```

Formatters fix the files and fail the first run; run again. A linter finding is
a real finding: fix the code rather than using `--no-verify` or `SKIP=`. If a
rule is wrong for this repository, change the configuration in the same pull
request and say why.

## Continuous integration

| Workflow | Purpose | Reproduce locally |
| --- | --- | --- |
| [`quality.yml`](.github/workflows/quality.yml) | every hook, then the test suite | `pre-commit run --all-files` and `cargo test --all-targets` |

It runs on pull requests and on pushes to `main`, needs no secrets, and runs
the same hooks as the commit gate, so skipping them locally only moves the
failure somewhere slower.

[`dependabot.yaml`](.github/dependabot.yaml) opens monthly pull requests for
Cargo dependencies and for the pinned GitHub Actions; the quality workflow
checks them like any other change.

## Submitting a change

Branch from `main` and keep the change small. Commit subjects are imperative
and lowercase, in the style of `git log --oneline`. Update the documentation
in the same pull request, and wait for the quality workflow to pass.
