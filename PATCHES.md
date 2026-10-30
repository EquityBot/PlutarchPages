# What this fork carries

PlutarchPages is upstream [PdfCraft](https://github.com/storytold/pdfcraft) plus the changes below.
[Fork changes](README.md#fork-changes) describes them for users; this file is for whoever merges
upstream next.

Last upstream merge: `1d1cbc4e` (30 October 2026).

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
  hues in `crates/engine/src/catalog.rs` and the app icon follow it.
- Brand: no ArtCraft name or logos (`AGENTS.md` section 1.2). Upstream merges add them back, in new
  files too, so search for them after every merge.
- Update check and repository links point at EquityBot.
- Shared CI: workflows from `storytold/craft-common-actions` are pinned to a commit, and SonarCloud
  receives only `SONAR_TOKEN`. Upstream merges restore `@main` and `secrets: inherit`, so reapply both.
- CodeQL: `.github/codeql/codeql-config.yml` excludes tests and `vendor/`.

## Merging upstream

1. `git merge --no-ff` upstream's `main` on a `ryan/` branch. Never rewrite history.
2. Search for ArtCraft names, trademark-licensed assets, `storytold/pdfcraft` links in app code and
   new colour literals.
3. Run `cargo xtask ci`. `cargo xtask assets --write` regenerates `ATTRIBUTION.md`.
4. A failure that also occurs on plain upstream isn't the merge's fault: check before patching it.
5. Update the commit above.
