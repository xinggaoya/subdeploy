# Repository Guidelines

## Project Structure & Module Organization
`subdeploy` is a Rust workspace. The CLI entrypoint lives in `apps/subdeploy/src/main.rs`. Core deployment orchestration and health checks are in `crates/subdeploy-core/`. Packaging logic for file discovery, `.gitignore` handling, and `tar.gz` creation is in `crates/subdeploy-packager/`. SSH and SFTP transport code lives in `crates/subdeploy-remote/`. Keep new functionality in the crate that owns the behavior instead of growing the CLI crate.

## Build, Test, and Development Commands
Run all commands from the repository root.

`cargo fmt --all` formats the workspace.

`cargo fmt --all -- --check` verifies formatting in CI-style checks.

`cargo test --workspace` runs all unit tests across crates.

`cargo run -p sd -- --help` starts the CLI locally.

`cargo install --path apps/subdeploy` installs the CLI for manual testing.

## Coding Style & Naming Conventions
Use Rust 2021 defaults and let `rustfmt` drive formatting. Follow standard Rust naming: `snake_case` for functions and modules, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep error messages actionable and user-facing text consistent with the existing Chinese CLI output. Prefer small functions, explicit structs for request data, and `thiserror`/`anyhow` for error handling at crate boundaries.

## Testing Guidelines
Tests currently live next to implementation in `#[cfg(test)]` modules. Add focused unit tests for packaging rules, remote script rendering, and health-check behavior when changing those areas. Name tests by behavior, for example `inspect_project_respects_gitignore`. Before opening a PR, run `cargo test --workspace` and `cargo fmt --all -- --check`.

## Commit & Pull Request Guidelines
Git history is minimal and currently uses a short imperative subject (`Initial commit`). Continue with concise, imperative commit titles and keep each commit scoped to one change. PRs should explain the behavior change, note affected crates, and list the validation commands you ran. Include CLI examples when flags, output, or deployment flow changes.

## Security & Configuration Tips
Do not commit SSH passwords or real hostnames used in production. Prefer environment variables or shell prompts for local secrets. Deployment requires tracked `Dockerfile` and Compose files; ignored deployment files will fail validation and packaging.
