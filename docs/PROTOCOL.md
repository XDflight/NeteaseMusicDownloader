# Protocol notes: the desktop-client API ("eapi")

The downloader talks to the same HTTP API as the official **desktop** client, not the web
client (which exposes less: no download endpoint, fewer quality levels, weaker authentication).
This document records how that protocol works, what was verified where, and what is still
best-effort. Everything here is implemented in [`crates/ncm-api`](../crates/ncm-api).

## How it was verified

| Method | What it established |
| --- | --- |
| Read-only string analysis of the installed client (`cloudmusic.dll`, Microsoft Store build 3.1.23) | The eapi key, the `-36cd479b6b5-` separator, the `nobody…use…md5forencrypt` digest layout, the client build string `3.1.23.204814`, the header/cookie field names (`os`, `osver`, `appver`, `deviceId`, `clientSign`, `requestId`, `MUSIC_U`, `MUSIC_A`) and the hosts (`interface.music.163.com`, `interface3.music.163.com`, `interfacepc.music.163.com`). The native layer builds the `params=` body and decrypts the response. There is no `weapi` code path in it. |
| Anonymous requests against the live service | Every endpoint below answers to requests built this way; response shapes, status codes and field names were checked against real data (see `crates/ncm-api/examples/probe.rs`). |
| Not verified | Phone/password and SMS login (the service applies risk control and may demand a captcha) and any behaviour that needs a logged-in account. QR login is verified up to the "waiting for scan" state. |

Endpoint paths are not stored as plain strings in the native library (they are supplied by the
client's UI layer), so they were confirmed against the service instead.

## Request format

```
POST https://interface.music.163.com/eapi/<path>
Content-Type: application/x-www-form-urlencoded
Cookie: osver=…; deviceId=…; os=pc; appver=3.1.23.204814; versioncode=140; mobilename=;
        buildver=<unix seconds>; resolution=1920x1080; __csrf=; channel=netease;
        requestId=<millis>_<0000-9999>; MUSIC_U=…            (once logged in)

params=<HEX>
```

`<HEX>` is the upper-case hex of `AES-128-ECB/PKCS7` under the key `e82ckenh8dichen8` of

```
<api-path>-36cd479b6b5-<json>-36cd479b6b5-<md5>
md5 = MD5("nobody" + <api-path> + "use" + <json> + "md5forencrypt")
```

* `<api-path>` is the endpoint with an **`/api`** prefix (`/api/song/enhance/player/url/v1`) while the
  request URL uses **`/eapi`**.
* `<json>` is the parameter object plus a `header` object with the same fields as the cookie.
* Responses are plain JSON, or AES-128-ECB ciphertext with the same key when the request asks for
  it (`e_r`); the client accepts both.
* Login state is carried by the `MUSIC_U` cookie (set through `Set-Cookie` after a QR or phone
  login). `MUSIC_A` is the anonymous token.

## Endpoints used

| Purpose | Path (after `/eapi`) | Notes |
| --- | --- | --- |
| Account status | `/w/nuser/account/get` | `code 200` with `profile` when logged in; otherwise `profile: null`. |
| QR key | `/login/qrcode/unikey` `{type:1}` | Returns `unikey`; QR content is `https://music.163.com/login?codekey=<unikey>`. |
| QR poll | `/login/qrcode/client/login` `{key,type:1}` | `800` expired, `801` waiting, `802` scanned, `803` confirmed (cookies in `Set-Cookie`). |
| SMS code | `/sms/captcha/sent` `{cellphone,ctcode}` | Best effort. |
| Phone login | `/w/login/cellphone` | `password` is the MD5 hex of the password, or `captcha`. Code `8821` means a behaviour captcha is required. |
| Logout | `/logout` | |
| User playlists | `/user/playlist` `{uid,limit,offset,includeVideo}` | Includes private playlists for the logged-in user; `specialType 5` is "liked songs". |
| Playlist | `/v6/playlist/detail` `{id,n:100000,s:8}` | `trackIds` lists every song; `tracks` may be truncated, so details are fetched by id. |
| Song details | `/v3/song/detail` `{c:"[{\"id\":…},…]"}` | 300 ids per request; `privileges[]` parallels `songs[]`. |
| Album | `/v1/album/<id>` | |
| Stream URL | `/song/enhance/player/url/v1` `{ids:"[…]",level,encodeType:"flac"}` | Batched. `immerseType:"c51"` for `sky`. |
| Download URL | `/song/enhance/download/url/v1` `{id,level,encodeType}` | Used as a fallback when the stream endpoint returns a trial. |
| Lyrics | `/song/lyric/v1` `{id, lv/kv/tv/rv/yv/ytv/yrv = -1}` | `lrc`, `tlyric` (translation), `romalrc`, `yrc` (word by word). |

## Semantics worth knowing

**Quality levels** (`level`): `standard` (MP3 128k), `higher` (192k), `exhigh` (320k), `lossless`
(FLAC), `hires`, `jyeffect`, `sky`, `jymaster`. The server answers with the best tier the account
may use *up to* the requested one and reports it in the `level` field, so a request for
`lossless` on a song that only has 320k returns `exhigh`.

**Availability.** For a VIP song seen by a non-VIP (or anonymous) session the catalog data looks
like `fee 1`, `privilege.st -100`, `pl 0`, `dl 0` and
`freeTrialPrivilege.cannotListenReason 1`. The stream endpoint then returns no URL and a
`freeTrialInfo` block (a preview clip). The downloader treats `pl > 0 || dl > 0` as playable and
`fee 1` or `cannotListenReason 1` as "VIP required"; the URL lookup is the final authority.

**Integrity.** The URL response carries `size` and `md5` of the file. The downloader verifies
both after the transfer.

**Lyrics.** The first lines of `lrc.lyric` may be JSON objects instead of LRC
(`{"t":1000,"c":[{"tx":"作曲: "},{"tx":"…"}]}`) carrying credits; they are converted to
`[mm:ss.xx]` lines. Translations share timestamps with the original and are merged line by line.

## Client identity and behaviour

The service tells clients apart by the fields above, by what a session looks like over time and by
how it behaves. The goal is to look like one ordinary desktop client, not to hide.

Taken from the official 3.1.23 client:

* the `User-Agent` verbatim (`… Windows NT 10.0; WOW64 … NeteaseMusicDesktop/3.1.23.204814`),
  used for API calls and CDN downloads alike; no `Referer` (web pages send one, the desktop client
  does not on eapi calls);
* the header/cookie fields (`os`, `osver`, `deviceId`, `appver`, `versioncode`, `mobilename`,
  `buildver`, `resolution`, `__csrf`, `channel`, `requestId`, `MUSIC_U` / `MUSIC_A`);
* an anonymous **guest token** (`MUSIC_A`) obtained with `/register/anonimous` before the first
  request, because the official client always carries either a login or a guest token (it logs an
  error when neither is present). The `username` is `base64(id + " " + base64(md5(id XOR key)))`
  with the key the client uses for its `encodeAnonymousId` function. Signing out returns to a fresh
  guest token.

Chosen so that one installation looks like one stable machine:

* `deviceId` is derived (salted MD5) from the operating system's machine id, so reinstalling does
  not create a "new device"; it is never regenerated except when the saved session cannot be read.
* `osver` reflects the real Windows build and edition; `resolution` is a common value picked once;
  `buildver` is a build timestamp fixed per installation (not the current time on every request).
* API calls are serialised with a gap of at least 120 ms **plus jitter** (0.75x to 1.5x), the QR
  poll interval is randomised (1.2 to 1.9 s), URL lookups are batched, and throttling responses
  (`-460`, `405`, `8821`, …) are honoured with exponential back-off.

Not done on purpose:

* No imitation of the client's TLS / HTTP fingerprint (this program uses rustls, the official
  client uses Chromium's network stack), no forging of the anti-fraud / behaviour-captcha tokens,
  no solving of captchas, no device-id rotation. Those amount to defeating the service's bot
  detection; they are also fragile. When the service demands a captcha (code `8821`) the app asks
  the user to sign in with the QR code instead.
* No guarantee. A third-party client can always be told apart by a determined service, and the
  terms of service may forbid it. Keep the risk down by signing in with the QR code, using one
  instance and one network per account, and not re-resolving thousands of tracks over and over.

## Throttling

The service answers abusive traffic with codes such as `-460` ("cheating"), `405`, `406` or `8821`.
The client spaces API calls by 120 ms, resolves download URLs in batches of 20 and backs off
exponentially (3 s to 48 s, six attempts) when it is throttled.
