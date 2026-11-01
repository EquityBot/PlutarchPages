# What this fork carries

PlutarchPages is upstream [PdfCraft](https://github.com/storytold/pdfcraft) plus the changes below.
[Fork changes](README.md#fork-changes) describes them for users; this file is for whoever merges
upstream next.

Last upstream merge: `f137a289` (31 October 2026).

## Stricter than upstream on purpose

Upstream's tests for these were changed to assert the fork's behaviour. Keep them that way.

- **Redaction** refuses when content under the marks is also used where the marks don't reach.
  Upstream goes ahead. Tests in `crates/redact` and `crates/automation`.
- **Copy-restricted documents** refuse text extraction through the tools, including `text_find`.
- **An empty password** never skips the Document Open prompt or lifts restrictions. `pdfcraft-cli
  check-one` checks before it renders.
- **CSV exports** write formula-like cells as text, and import doesn't strip the apostrophe, so a
  neutralised cell doesn't round-trip back into a formula.

## Only here

- The PDF-XChange-style converters, speech, bookmark, optimiser, colour and Excel features.
- Mermaid colour scheme: `crates/ui-egui/src/theme.rs` holds every interface colour, and
  `chrome_literals_stay_in_the_theme` fails when a merge brings a colour literal back. The catalogue
  hues in `crates/engine/src/catalog.rs`, the app icon and `UPDATE_APP`'s accent follow it. The test
  only sees hexadecimal literals: named colours such as `Color32::WHITE` on chrome need searching by
  hand when upstream moves or adds a control.
- Brand: no ArtCraft name or logos (`AGENTS.md` section 1.2). Upstream merges add them back, in new
  files too, so search for them after every merge.
- Update check and repository links point at EquityBot. `new_updater` in
  `crates/ui-egui/src/platform/updates.rs` ignores ArtCraft Launcher's version pins, which are keyed
  by the `pdfcraft` id. Don't give `UPDATE_APP` another id to avoid them: it also names the packages
  and binaries the updater installs.
- Update results: `crates/ui-egui/src/platform/updates.rs` keeps the fork's themed, translated dialog
  over `craft_update`'s `CheckResultDialog`, which is English only with its own colours. Upstream
  merges delete its catalogue rows and change `crates/ui-egui/tests/it/updates.rs`, so restore both.
- Flatpak manifests don't carry upstream's `--filesystem=xdg-data/ArtCraft/versions:ro` grant, which
  lets an upstream launcher's version choices reach the app.
- Shared CI: workflows from `storytold/craft-common-actions` are pinned to a commit, and SonarCloud
  receives only `SONAR_TOKEN`. Upstream merges restore `@main` and `secrets: inherit`, so reapply both.
- Releases: `.github/workflows/release.yml` is this fork's own and uses nothing from
  `craft-common-actions`. It builds unsigned packages, tests an installed MSI and an upgrade from
  v0.15.0 with `packaging/windows/test-install.ps1`, and only drafts the release. Keep it over
  upstream's. Its FreeBSD, riscv64, web and Flatpak jobs are ported from that repository's
  `release.yml` at `2b3fb9dc`, without the FreeBSD Cargo cache action, and
  `packaging/linux/arch-package.sh` replaces its `arch-package` action. All are first included in the
  release after v0.15.0, and the `assets` job fails unless all 30 packages are there.
- Linux identity: the Flatpak's id is `io.github.EquityBot.PlutarchPages` and the Arch package is `plutarchpages`, so
  neither updates or replaces upstream's. If a merge brings back
  `packaging/linux/flatpak/ai.storyteller.pdfcraft*.yml`, move its changes across and delete it. The
  other packages still use `ai.storyteller.pdfcraft` and the `pdfcraft` package name, which v0.15.0
  shipped, so they can't be installed alongside upstream's either.
- `packaging/linux/nfpm.yaml` takes its glibc floors from `package.sh`, because riscv64 builds on Ubuntu
  24.04 and needs 2.39. Upstream hard-codes 2.35, which understates it there.
- Windows registration: `PlutarchPages.*` names and its own context-menu component GUIDs, so it doesn't
  share registry state with an upstream install. `pdfcraft.exe` stays, so `App Paths\pdfcraft.exe` and
  `Applications\pdfcraft.exe` are still shared: installing it alongside upstream isn't supported.
- CodeQL: `.github/codeql/codeql-config.yml` excludes tests and `vendor/`.
- Windows printing with Type 3 fonts (upstream #2684, from upstream PR #3021 as it stood at
  `ed6252a6`): those pages go as pictures. Unlike the PR, a printer's job is built on its worker
  (`Session::print_source`) and the dialog's check runs in the background, because rendering on the
  UI thread froze the app. If upstream merges its version, keep the worker.

## Merging upstream

1. `git merge --no-ff` upstream's `main` on a `ryan/` branch. Never rewrite history.
2. Search for ArtCraft names (packaging too), trademark-licensed assets, `storytold/pdfcraft` links in
   app code and new colour literals, named colours included.
3. Run `cargo xtask ci`. `cargo xtask assets --write` regenerates `ATTRIBUTION.md`.
4. A failure that also occurs on plain upstream isn't the merge's fault: check before patching it.
5. Update the commit above.
