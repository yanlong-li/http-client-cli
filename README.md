# HTTP Client CLI

Run `.http` and `.rest` HTTP request files from the terminal, CI, or editor task integrations.

## Install

Build from a checkout:

```powershell
cargo install --path crates/cli
http-client --help
```

The installed executable is `http-client`. It is also used by the companion Zed language extension's inline request runnable.

## Usage

```powershell
http-client run examples/demo.http --line 5
http-client run examples/demo.http --name "Create resource"
http-client run examples/demo.http --all
http-client list examples/demo.http
http-client env examples/demo.http dev
```

Use `http-client.env.json` for public values and `http-client.private.env.json` for secrets. Select an environment with `http-client env <file> <name>` or set `HTTP_CLIENT_ENV`. The persisted selection is stored in `.http-client-env` at the project root. The CLI supports a practical compatibility subset of common `.http` conventions; the format is used by multiple editors and tools.

Runtime artifacts are stored under `.http-client/` in the project root. The CLI maintains `http-client.cookies` using curl's standard cookie-jar format, automatically sends matching cookies on later requests, and updates the jar from `Set-Cookie` responses. An explicit `Cookie` header in a request takes precedence for that request. Use `--no-cookies` to disable cookie loading and persistence. Saved responses and `http-requests-log.http` are also written there. This location is editor-independent and is ignored by Git.

## Development

```powershell
cargo fmt --check
cargo test -p http-client-core
cargo clippy --workspace --all-targets -- -D warnings
```

This repository contains the CLI and its shared core library. The Zed language extension belongs in a separate repository and invokes this executable through its bundled task template.
