# Contributing to Hourmint

Thanks for helping. Bug reports, ideas, docs fixes and code are all welcome.

## Ways to help

- **Report a bug.** Use the [bug report form](https://github.com/abdurrahmanriyad/hourmint/issues/new?template=bug_report.yml). Include your macOS version, your Hourmint version and the steps to reproduce it.
- **Suggest a feature.** Use the [feature request form](https://github.com/abdurrahmanriyad/hourmint/issues/new?template=feature_request.yml) and describe the problem you're trying to solve, not only the solution.
- **Pick up an issue.** Issues labelled [good first issue](https://github.com/abdurrahmanriyad/hourmint/labels/good%20first%20issue) are small and well described. Leave a comment saying you're working on one so nobody duplicates the work.
- **Translate.** All UI text is in `src/strings.ts`, ready for translation. Open an issue first so we can agree on how languages are loaded.

For larger changes, open an issue before you start so we can agree on the approach. It saves you from writing code that doesn't fit.

## Product principles

Hourmint is a small, focused app for solo freelancers. Changes should keep to these:

- **Local-first and private.** No accounts, no servers, no analytics. The only network request is the update check.
- **Your records can be trusted.** Never silently change past entries or issued invoices.
- **Simple over configurable.** No team features or dashboards to set up.

## Run it locally

Requirements: macOS, Node 22 (`nvm use`), Rust stable and the Xcode command line tools.

```sh
nvm use              # Node 22 from .nvmrc; npm install refuses older Node
npm install
npm run app          # tauri dev: Vite + the native app with hot reload
npm run test:all     # typecheck + frontend tests + Rust tests
npm run demo         # the UI in a normal browser with fake data (src/demo/), for design checks and screenshots
npm run build:local  # Hourmint.app + .dmg for this Mac in src-tauri/target/release/bundle/
npm run record:tour  # re-records the README demo animation (needs Chrome and `brew install webp`)
```

`npm run demo` opens `/demo.html`. Add `?window=popover&frame=340x500`, `?window=idle&frame=440x260`, `&theme=dark`, `&onboarded=0` or `&clean=1` (hides the menu bar hint). It's the quickest way to work on UI without building the native app.

Data from the dev app lives in `~/Library/Application Support/app.hourmint.desktop/` (`hourmint.sqlite`, `backups/`, `logos/`). Logs are in `~/Library/Logs/app.hourmint.desktop/`.

## Code layout

| Path | What |
|---|---|
| `src-tauri/src/db.rs` | SQLite open, WAL, migrations (`PRAGMA user_version`), backup and restore, daily backups (keep 7) |
| `src-tauri/src/services/` | Business logic as plain functions over a `Connection` (unit-tested): settings, clients and projects, entries and timer, reports and CSV, invoices, export |
| `src-tauri/src/commands.rs` | Tauri commands. Every mutation emits `data-changed` so all windows refetch |
| `src-tauri/src/tray.rs`, `windows.rs` | Menu bar icon with elapsed time, popover window, idle prompt window |
| `src-tauri/src/idle.rs` | Background monitor: idle detection, long-timer warning, start reminder, daily backup |
| `src-tauri/src/shortcut.rs` | Global start/stop shortcut |
| `src/strings.ts` | All UI text (translation-ready) |
| `src/lib/` | Typed API, formatting (currency minor units, durations, dates), React Query hooks, PDF templates |
| `src/pages/`, `src/windows/` | Screens. The same bundle renders the main window, popover or idle prompt depending on the window label |
| `src/lib/updates.ts` | In-app updates (Tauri updater) from GitHub Releases |
| `site/` | Landing page and privacy policy (static HTML), published to <https://hourmint.abdur.tech> by `.github/workflows/pages.yml` |

Conventions: UUID text ids, `created_at/updated_at/deleted_at` on every table with soft deletes, UTC ISO 8601 timestamps, money as integer minor units, durations in seconds. The running timer is the single entry with `ended_at IS NULL` (enforced by a unique partial index), so it survives quit, crash and sleep.

### Decisions worth knowing

- **PDF** is generated on device with `@react-pdf/renderer` (paginated, selectable text, embedded Noto Sans so symbols like ₹ ₺ ₦ render). The invoice builder shows an HTML live preview that mirrors the PDF layout.
- **Rates are snapshotted** onto each entry (`time_entries.invoice_rate`) when it's added to an invoice, and onto `invoice_lines`, so later rate changes never alter existing invoices.
- **Locking.** An entry is locked as soon as it's on any invoice, including a draft. Unchecking it in a draft or deleting the invoice releases it. "Billed" in filters means "on an invoice".
- **Rounding "none"** rounds each line to the nearest minute so hours × rate always matches the amount shown.
- **The global shortcut** is off by default. Set one in Settings > Timer.
- **Schema changes** go in a new migration in `db.rs`. Never edit an existing migration, because installed copies have already run it.

### Builds

The same code builds two ways. Only the direct download is published today.

| | Direct download | Mac App Store |
|---|---|---|
| Cargo features | `direct` (default) | `--no-default-features` |
| Sandbox | No | Yes (`src-tauri/appstore/entitlements.plist`) |
| Updates | In-app, from GitHub Releases | By the App Store |
| Data folder | `~/Library/Application Support/app.hourmint.desktop/` | `~/Library/Containers/app.hourmint.desktop/` |
| Build | `npm run build:local` or the release workflow | `npm run build:appstore` (see [docs/APP_STORE.md](docs/APP_STORE.md)) |

## Sending a pull request

1. Fork the repo and create a branch from `main`.
2. Make your change. Put UI text in `src/strings.ts` rather than inline.
3. Add or update tests. Business logic in `src-tauri/src/services/` has unit tests next to it, and formatting helpers have Vitest tests.
4. Run `npm run test:all`, and `cargo fmt` in `src-tauri/` if you changed Rust code. CI runs the same checks on every pull request.
5. For UI changes, add a before and after screenshot to the pull request. `npm run demo` is handy for this.
6. Open the pull request and link the issue it fixes (`Fixes #123`).

Keep pull requests focused on one change. Small ones get reviewed faster.

If you used an AI assistant, that's fine. Say so in the pull request, and make sure you understand and have tested every line you submit.

## Releases

Maintainers cut releases. The steps are in [docs/RELEASING.md](docs/RELEASING.md).

## Code of conduct

Be kind and assume good intent. This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
