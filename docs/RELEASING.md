# Releasing Wren

`.github/workflows/release.yml` runs on every push to `main` (and by hand). It builds the Windows
installer and the Apple Silicon app, signs the update files, writes `latest.json` and publishes a
GitHub Release. This page lists every switch it reads and what each one needs.

## Always required

| Secret | What it is |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` (+ `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) | Ed25519 (minisign) key that signs `.exe` / `.app.tar.gz` for the in-app updater. Installed copies verify it against the public key built into the app before running anything. |

## Code signing (optional, fail closed)

Signing is off unless asked for. When it is asked for, a missing secret, a failed signature check,
or a missing proof file stops the run before anything is published: a requested-signed release can
never go out unsigned. When it is not asked for, builds stay unsigned (Windows) / ad-hoc signed
(macOS) exactly as before, and the run shows a warning.

### Windows — Authenticode

| Kind | Name | Value |
| --- | --- | --- |
| Variable | `WREN_SIGN_WINDOWS` | `true` to sign |
| Secret | `WINDOWS_CERTIFICATE` | base64 of the code-signing `.pfx` (`[Convert]::ToBase64String([IO.File]::ReadAllBytes('cert.pfx'))`) |
| Secret | `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` password |
| Variable (optional) | `WINDOWS_TIMESTAMP_URL` | RFC 3161 timestamp server (default `http://timestamp.digicert.com`) |

`scripts/sign-windows.ps1 -Import` loads the certificate (never printing it), `-Config` makes
`tauri build` sign `wren.exe`, the installer and the uninstaller with signtool (SHA-256, timestamped),
and `-Verify -RequireTrusted` checks each file's signature is by that certificate and chains to a
trusted root. The updater signature is made afterwards, over the signed installer.

A cloud HSM (Azure Trusted Signing, SignPath, …) instead of a `.pfx`: replace the `-Config` overlay
with a Tauri `bundle.windows.signCommand`; keep the `-Verify` step.

### macOS — Developer ID + notarization

| Kind | Name | Value |
| --- | --- | --- |
| Variable | `WREN_SIGN_MACOS` | `true` to sign and notarize |
| Secret | `APPLE_CERTIFICATE` | base64 of the "Developer ID Application" `.p12` |
| Secret | `APPLE_CERTIFICATE_PASSWORD` | its password |
| Secret | `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| Secret | `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`, `APPLE_API_KEY_P8` | App Store Connect API key for `notarytool` (key id, issuer id, contents of the `.p8`) |

The certificate goes into a throwaway keychain; Tauri signs `Wren.app` with the Hardened Runtime and
`src-tauri/Entitlements.plist` (microphone, Apple Events) via `src-tauri/tauri.macos-release.json`,
then notarizes and staples it. The DMG is signed, notarized and stapled too. The run checks
`codesign --verify --deep --strict`, the Developer ID authority, the `runtime` flag,
`stapler validate` and `spctl --assess` before publishing. The in-app updater keeps a Developer ID
signature as shipped (it only re-signs ad-hoc/development builds locally).

## Where installed copies look for updates

Baked in at build time (`src-tauri/build.rs` re-runs when these change):

| Variable | Default | Meaning |
| --- | --- | --- |
| `WREN_UPDATE_REPO` | this repository in CI; `Kamo2323/wren` for local builds | `owner/repo` whose `releases/latest/download/latest.json` is checked, or `none` for a build that never checks |
| `WREN_UPDATE_PUBKEY` | the official key | base64 minisign public key matching that repository's `TAURI_SIGNING_PRIVATE_KEY` |
| `WREN_UPDATE_PRIVATE` | off | `1` lets the updater fall back to the GitHub API with `WREN_GITHUB_TOKEN` / `GITHUB_TOKEN` / `gh auth token` when the release page is not public. Also honoured at run time. |

Checks are anonymous unless private mode is on. A fork that publishes its own releases sets the
`WREN_UPDATE_PUBKEY` variable (the prepare job refuses to build a non-official update source without
it) — or `WREN_UPDATE_REPO=none`. At run time `WREN_DISABLE_UPDATER=1` stops background checks.

**While the official repository is private** its releases are not public, so ordinary builds cannot
see them. Either set the repository variable `WREN_UPDATE_PRIVATE=1` (CI builds then keep the token /
`gh` fallback; remove it before going public) or set the user environment variable
`WREN_UPDATE_PRIVATE=1` on your own machines.

## What has been proved where

| Check | Where it can run |
| --- | --- |
| Updater: anonymous by default, token only in private mode, fork sources, `none` | `cargo test --lib updater` (any OS) |
| Signing switches fail closed; YAML valid | reading the workflow; a real run needs the repository's Actions |
| Authenticode wiring (`sign-windows.ps1`, Tauri overlay, verify) | any Windows PC with a throwaway self-signed certificate (status `UnknownError`, not trusted) |
| Authenticode with a trusted publisher certificate (`-RequireTrusted`) | needs a real code-signing certificate |
| Developer ID signing, Hardened Runtime entitlements, notarization, stapling, Gatekeeper | needs a Mac, an Apple Developer account and the secrets above |
