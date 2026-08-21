NPPS4-DLAPI
=====

[![NPPS4 DLAPI Spec.: Version 1.1](https://img.shields.io/badge/NPPS4%20DLAPI%20Spec.-Version%201.1-bf88ba)](https://github.com/DarkEnergyProcessor/NPPS4-DLAPI)
[![Language: Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)

A Rust rewrite of the [NPPS4-DLAPI reference implementation](https://github.com/DarkEnergyProcessor/NPPS4-DLAPI) — a CDN/download API server for Love Live! School Idol Festival (SIF) game assets, implementing the NPPS4 Download API protocol v1.1.

The original Python/FastAPI implementation is preserved upstream. This fork rewrites the server in Rust for:

- **Memory safety** — Rust's ownership model eliminates entire classes of memory vulnerabilities (use-after-free, buffer overflows) at compile time, with no runtime GC pauses.
- **Single binary deployment** — `cargo build --release` produces one self-contained binary. No Python interpreter, no venv, no pip. Game database decryption (honkypy V2/V3/V4) is implemented natively in Rust — no external tools required at runtime.
- **Lower resource usage** — significantly lower memory footprint and faster cold starts than uvicorn/FastAPI.
- **Security hardened** — path traversal protection, input sanitization, and request size limits built in. See [Security](#security).

The API is 100% wire-compatible with the original — any client or tool that works with the Python version works here unchanged.

---

Prerequisites
-----

- [Nix](https://nixos.org/) with flakes enabled (the dev environment provides everything else)
- A prepared `archive-root` directory (see [Archive Structure](#archive-structure) below)

Getting Started
-----

### 1. Enter the development shell

```bash
nix develop
```

This drops you into a shell with the Rust stable toolchain, `cargo`, and `cargo-watch` ready to use.

### 2. Create a config file

Copy the sample and edit it:

```bash
cp config.sample.toml config.toml
$EDITOR config.toml
```

Minimum config:

```toml
[main]
public = true
shared_key = ""
archive_root = "/absolute/path/to/archive-root"
```

### 3. Build and run

```bash
# Development (with logging)
RUST_LOG=n4dlapi=info,tower_http=info cargo run

# Production build
cargo build --release
./target/release/n4dlapi
```

The server listens on `127.0.0.1:8000` by default.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `N4DLAPI_CONFIG_FILE` | `config.toml` | Path to the TOML config file |
| `N4DLAPI_ARCHIVE_ROOT` | *(from config)* | Override the archive root directory |
| `N4DLAPI_LISTEN` | `127.0.0.1:8000` | Listen address and port |
| `RUST_LOG` | `n4dlapi=info` | Log level filter |

### Live reload during development

```bash
cargo watch -x run
```

---

Configuration
-----

Full reference for `config.toml`:

```toml
[main]
# Make all API endpoints public by default (no shared key required).
public = true

# Optional shared key. Clients must send this in the DLAPI-Shared-Key header.
# Empty string disables the shared key (all endpoints public).
shared_key = ""

# Path to the archive-root directory. Relative paths are resolved from CWD.
# Absolute paths recommended. Overridden by N4DLAPI_ARCHIVE_ROOT env var.
archive_root = "/srv/sif/archive-root"

# Base URL used when constructing download links in API responses.
# Set this when running behind a reverse proxy to prevent Host header injection.
# Include any path prefix if archive-root is served under a subpath.
# Example: base_url = "https://dl.example.com/myapp"
# base_url = ""

# Per-endpoint visibility overrides:

# Always serve /api/publicinfo publicly, even when public = false above.
[api.publicinfo]
public = true

# Allow /api/v1/update without a shared key.
# [api.v1.update]
# public = true

# Restrict /api/v1/getdb even when public = true.
# [api.v1.getdb]
# public = false
```

---

CLI Tools
-----

The `n4dlapi` binary provides three subcommands. Run without a subcommand (or with `serve`) to start the API server.

### `n4dlapi serve [options]`

Starts the API server (this is also the default when no subcommand is given).

| Flag | Default | Description |
|---|---|---|
| `--getdb-cors` | — | Enable permissive (wildcard) CORS on `/api/v1/getdb/{name}` so browser-based tools (e.g. sqlite-viewer) can fetch the game database cross-origin |

### `n4dlapi upgrade <archive-root>`

Upgrades an archive to the latest generation (currently **1.2**), running the required stages in order. This is required before running the server.

Generation 1.0 → 1.1 (equivalent to upstream `update_v1.1.py`):

- Scans all update and package directories
- Computes MD5/SHA256 hashes and writes `infov2.json` metadata files
- Extracts micro-download files from package type 4 archives
- Decrypts game databases using a native Rust implementation of the honkypy algorithm (no external tools required)

Generation 1.1 → 1.2 (equivalent to upstream `update_v1.2.py`):

- Renames numeric archive names (`1.zip`) to unique `1_<sha256>.zip` names, so a new game version can never reuse a URL for different content (this corrupted in-game downloads through caches)

```bash
n4dlapi upgrade /path/to/archive-root
```

Only needs to be run once per archive (and once more after this update, to move 1.1 archives to 1.2). A `generation.json` file is written when complete; subsequent runs exit immediately if the archive is already at the latest generation.

### `n4dlapi clone <destination> <mirror> [options]`

Clones a full game archive from a remote NPPS4-DLAPI v1.1 server to a local directory. Useful for setting up a mirror.

```bash
# Basic clone
n4dlapi clone /srv/sif/archive-root https://your-mirror.example.com

# With shared key authentication
n4dlapi clone /srv/sif/archive-root https://your-mirror.example.com \
    --shared-key "mysecretkey"

# iOS only, starting from game version 60.0
n4dlapi clone /srv/sif/archive-root https://your-mirror.example.com \
    --no-android --base-version 60.0
```

| Flag | Default | Description |
|---|---|---|
| `--shared-key <KEY>` | *(empty)* | Shared key for the remote server |
| `--no-ios` | — | Skip iOS downloads |
| `--no-android` | — | Skip Android downloads |
| `--base-version <VER>` | `59.0` | Oldest game version to fetch updates from |
| `--jobs <N>` | `4` | Number of parallel download connections |

Downloads are resumable: if interrupted, re-run the same command and it will continue from where it left off (resume state is stored as `update.json` / `package_N.json` files in the destination).

After cloning, the archive is ready to use with `n4dlapi serve` directly — no separate `upgrade` step is needed.

---

Archive Structure
-----

The `archive-root` directory must be at generation **1.1** or newer (1.2 recommended). Use `n4dlapi upgrade` to bring an older archive up to date, or `n4dlapi clone` to create a fresh one from a remote server. See [CLI Tools](#cli-tools) below.

```
archive-root/
├── {iOS,Android}/
│   ├── update/
│   │   ├── infov2.json              # List of available update versions
│   │   └── <version>/               # e.g. "59.4"
│   │       ├── 1.zip
│   │       ├── 2.zip
│   │       ├── ...
│   │       ├── info.json
│   │       └── infov2.json          # [{name, size, md5, sha256}, ...]
│   └── package/
│       ├── info.json                # List of available package versions
│       └── <version>/
│           ├── db/                  # Pre-decrypted database files
│           │   └── *.db_
│           ├── microdl/             # Extracted micro-download files
│           │   ├── assets/
│           │   ├── config/
│           │   ├── en/
│           │   └── info.json        # {filepath: {size, md5, sha256}}
│           ├── microdl_map.json
│           └── <package_type>/      # 0–6
│               ├── info.json        # [package_id, ...]
│               └── <package_id>/
│                   ├── 1.zip
│                   ├── 2.zip
│                   ├── ...
│                   ├── info.json
│                   └── infov2.json  # [{name, size, md5, sha256}, ...]
├── release_info.json                # {package_id: base64_key}
└── generation.json                  # {"major": 1, "minor": 1}
```

### Package types

| Value | Name | Package ID source |
|---|---|---|
| 0 | Bootstrap | Always 0 |
| 1 | Live | `live_track_id` in `live/live.db_` |
| 2 | Scenario | `scenario_chapter_id` in `scenario/scenario.db_` |
| 3 | Subscenario | `unit_id` in `subscenario/subscenario.db_` |
| 4 | Micro | Exposed via `release_info.json` |
| 5 | Event Scenario | `event_scenario_id` in `event/event_common.db_` |
| 6 | Multi Unit Scenario | `multi_unit_scenario_id` in `multi_unit_scenario/multi_unit_scenario.db_` |

---

API Reference
-----

All endpoints require the `DLAPI-Shared-Key` header if a shared key is configured, unless the endpoint is marked public. On failure the server returns HTTP 404 with `{"detail": "Not found."}` to avoid leaking information.

<details>
<summary><code>GET</code> <code><b>/api/publicinfo</b></code></summary>

Returns server metadata. Typically configured as always-public.

#### Response `200`
```jsonc
{
    "publicApi": true,
    "dlapiVersion": { "major": 1, "minor": 1 },
    "serveTimeLimit": 0,
    "gameVersion": "59.4",
    "application": {
        "NPPS4DLAPICommit": "abc123...",
        "NPPS4DLAPIVersion": "2023.05.14"
    }
}
```

</details>

<details>
<summary><code>POST</code> <code><b>/api/v1/update</b></code></summary>

Returns download links for all update packages between the client's current version and the latest available.

#### Request body
```json
{ "version": "59.0", "platform": 2 }
```

| Field | Type | Description |
|---|---|---|
| `version` | string | Client's current version |
| `platform` | int | `1` = iOS, `2` = Android |

#### Response `200`
```jsonc
[
    {
        "url": "https://dl.example.com/myapp/archive-root/iOS/update/59.4/1.zip",
        "size": 12345,
        "checksums": { "md5": "...", "sha256": "..." },
        "version": "59.4"
    }
]
```

Updates are not incremental — the response includes all intermediate versions in order.

</details>

<details>
<summary><code>POST</code> <code><b>/api/v1/batch</b></code></summary>

Returns download links for all packages of a given type.

#### Request body
```json
{ "package_type": 1, "platform": 1, "exclude": [578, 579] }
```

| Field | Type | Description |
|---|---|---|
| `package_type` | int | See package types table |
| `platform` | int | `1` = iOS, `2` = Android |
| `exclude` | int[] | Package IDs to skip (optional) |

#### Response `200`
```jsonc
[
    {
        "url": "https://dl.example.com/myapp/archive-root/iOS/package/59.4/1/580/1.zip",
        "size": 12345,
        "checksums": { "md5": "...", "sha256": "..." },
        "packageId": 580
    }
]
```

#### Response `404`
```json
{ "detail": "Package type not found" }
```

</details>

<details>
<summary><code>POST</code> <code><b>/api/v1/download</b></code></summary>

Returns download links for a specific package.

#### Request body
```json
{ "package_type": 1, "package_id": 747, "platform": 1 }
```

#### Response `200`
```jsonc
[
    {
        "url": "https://dl.example.com/myapp/archive-root/iOS/package/59.4/1/747/1.zip",
        "size": 12345,
        "checksums": { "md5": "...", "sha256": "..." }
    }
]
```

#### Response `404`
```json
{ "detail": "Package not found" }
```

</details>

<details>
<summary><code>GET</code> <code><b>/api/v1/getdb/{name}</b></code></summary>

Returns a pre-decrypted SQLite3 database file.

The `name` parameter is sanitized to alphanumeric characters and underscores only.

#### Response `200`
Raw SQLite3 bytes. `Content-Type: application/vnd.sqlite3`

#### Response `404`
```json
{ "detail": "Database not found" }
```

</details>

<details>
<summary><code>POST</code> <code><b>/api/v1/getfile</b></code></summary>

Returns download info for individual micro-download files (package type 4).

#### Request body
```json
{ "files": ["assets/image/tx_foo.texb", "en/assets/sound/vo_bar.mp3"], "platform": 1 }
```

#### Response `200`
```jsonc
[
    {
        "url": "https://dl.example.com/myapp/archive-root/iOS/package/59.4/microdl/assets/image/tx_foo.texb",
        "size": 12345,
        "checksums": { "md5": "...", "sha256": "..." }
    }
]
```

If a file is not found, its entry still appears with `size: 0` and the MD5/SHA256 of empty input.

</details>

<details>
<summary><code>GET</code> <code><b>/api/v1/release_info</b></code></summary>

Returns the decryption key map for package type 4.

#### Response `200`
```jsonc
{
    "423": "UDKkj/dmBRbz+CIB+Ekqyg==",
    "1874": "T18sDsU+81wLXTjCURNxJw=="
}
```

</details>

<details>
<summary><code>GET</code> <code><b>/health</b></code></summary>

Liveness/readiness probe for reverse proxies and monitoring. Not part of the DLAPI spec; always accessible (no shared key). Returns `200` with `{"status": "ok", "gameVersion": "59.4"}` when the archive is readable, or `503` with `{"status": "degraded", "detail": "..."}` when the process is up but the archive is not servable.

</details>

### Static files

Archive files under `/archive-root/*` are served by the Rust application itself, exactly like the original Python implementation. A standalone deployment works out of the box: the download URLs produced by the API point back at this server.

For high-traffic deployments, a reverse proxy (nginx) can still serve `/archive-root/` directly from disk and bypass the application — see the nginx example below. The application route is simply never reached in that case.

The legacy `/v7/micro_download/:platform/:version/*path` endpoint is an exception: it is handled by the Rust app and returns `200` directly, for clients that use the old CDN path format.

---

Security
-----

Compared to the original Python implementation this rewrite adds the following hardening:

- **No memory unsafety** — Rust's compile-time guarantees eliminate buffer overflows, use-after-free, and data races entirely.
- **Path traversal prevention** — database names are stripped to `[a-zA-Z0-9_]`; micro-download paths are normalised and all `..` components removed before any filesystem access.
- **Host header injection fix** — set `base_url` in config to pin the base URL used in responses, preventing cache poisoning attacks in CDN deployments.
- **Request body cap** — request bodies are limited to 64 MiB (the original has no limit at all).
- **Panic isolation** — a panicking request handler returns HTTP 500 instead of dropping the connection (which reverse proxies would surface to users as 502 Bad Gateway).
- **Input validation** — platform and package type are validated as known integers before any file I/O.
- **No shell execution** — no user input is ever passed to a subprocess or shell.

---

Reverse Proxy (nginx example)
-----

```nginx
server {
    listen 443 ssl;
    server_name dl.example.com;

    # Serve archive files directly from disk — clients never hit the Rust app
    # for these, which avoids the overhead of going through the application.
    # Clients do not follow 3xx redirects, so this must return 200 directly.
    location /myapp/archive-root/ {
        alias /srv/sif/archive-root/;
        autoindex off;
    }

    # API and legacy v7 routes go to the Rust application
    location /myapp/ {
        proxy_pass http://127.0.0.1:8000;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

The server shuts down gracefully on `SIGTERM`/`SIGINT`, draining in-flight requests first — restarts under systemd won't drop active downloads.

Set `base_url = "https://dl.example.com/myapp"` in `config.toml` to match the path prefix nginx uses. The generated download URLs will then be `https://dl.example.com/myapp/archive-root/...`, which nginx resolves directly to disk without touching the Rust app.

---

License
-----

This Rust implementation is licensed under the zlib/libpng license, matching the original upstream project.
