# Design notes

## Crates

| Crate | Responsibility |
| --- | --- |
| `ncm-api` | eapi crypto, HTTP client, session (device + cookies), login flows, catalog / URL / lyric endpoints, share-link parser. No UI, no disk access. |
| `ncm-core` | Download engine (adaptive connection control, chunked fetching, per-track pipeline), tagging, lyrics, file naming, encrypted credential store, settings. |
| `ncm-update` | GitHub Releases update check, verified download and platform-specific installation. |
| `netease-music-downloader` (`crates/app`) | The egui application: pages, settings, glue between UI and async tasks. |
| `asset-tool` | Build-time tool that converts source images into the icon and installer formats the packagers need. |

`ncm-core` and `ncm-api` are UI-independent; the engine communicates through a channel of
`Event` values, so a CLI front end would be a thin addition.

## Download engine

```
resolve URL (batched, cached) ──► chunked download to <name>.<ext>.part ──► verify size + MD5
      │                                        │                                   │
      └─ lyrics + cover fetched in parallel ───┴──────────────► tag ──► rename ──► .lrc or .txt / cover files
```

* **Chunks.** Each file is split into 512 KiB `Range` requests that go into one global FIFO. A
  fixed pool of eight workers takes chunks, but a chunk may only be requested after acquiring a
  permit from an adjustable gate. Because the queue is FIFO, files finish in order instead of
  every file being half done.
* **Adaptive concurrency.** A controller looks at the aggregate throughput every 3 s window:
  * it adds one connection while the previous increase raised throughput by at least 15 %;
  * an increase that did not pay off is reverted and the controller holds for six windows before
    probing again, so it follows changes of the network;
  * errors (timeouts, resets, HTTP 429/503) or a throughput collapse (below 80 % of the reference)
    halve the connection count;
  * windows in which the connections were not busy (nothing to download) are ignored.
  The count stays within 1–8 and starts at 2. The policy is a pure state machine
  (`ncm-core/src/adaptive.rs`) tested against modelled networks.
* **HTTP/2 is deliberately off** so that parallel chunk requests use separate TCP connections
  instead of being multiplexed onto one.
* **Retries.** Eight retries per chunk with jittered exponential backoff (0.5 s to 15 s), 15 s
  connect timeout, 30 s stall timeout. The gate permit is released while backing off. A `403`
  means the link expired while queued: the URL is fetched again once.
* **Look-ahead.** Tracks start in order through a second gate whose size is the connection limit
  plus two, so connections do not idle while the next track resolves its URL.

The numbers above are the project owner's explicit choices; they live in one file,
`ncm-core/src/tuning.rs`.

## Credentials

`credentials.bin` holds the session (cookies, account, device identity):

```
"NMDC" | version | mode | salt(16) | nonce(12) | AES-256-GCM(ciphertext)
key = Argon2id(password, salt, secret = machine identifier)      19 MiB, 2 passes, 1 lane
```

* `password` is the optional user passphrase; without one a constant is used, which binds the
  file to this machine and user profile without prompting.
* The header is authenticated as associated data; writes are atomic with owner-only permissions on
  Unix (Windows relies on the per-user profile ACL).
* No OS keychain is involved. This protects against copying the file and against casual
  inspection. It does not protect against malware running as the same user while the app is
  unlocked.

## Updates

* Release assets are named `netease-music-downloader-<version>-<platform>-<arch>[-suffix].<ext>`
  and accompanied by `SHA256SUMS.txt`. A download without a matching digest is refused.
* How an update is applied depends on how the copy was installed: the NSIS installer is run
  silently (`/S /UPDATE`), the portable Windows executable and the Linux binary are replaced with
  `self-replace`, the macOS `.app` bundle is swapped for the one in the release zip, and an
  AppImage replaces itself. System-package installs are told to use their package manager.
* `NMD_UPDATE_API_BASE` overrides the API host (mirrors, GitHub Enterprise, or a local test server).
