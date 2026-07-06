# Release Metadata Action

This composite action centralizes release metadata validation for the repository.

It performs four tasks:

- normalizes the requested version or tag into a release version such as `X.Y.Z` or `X.Y.Z-beta.1`
- validates that `Cargo.toml` carries the expected workspace version
- extracts the target release section from `CHANGELOG.md` when a version is provided
- uploads both a markdown summary and a JSON metadata artifact for downstream review

## Inputs

- `version`: required release version or tag, for example `0.1.1`, `1.0.0-beta.1`, or `v1.0.0-beta.1`
- `artifact_prefix`: optional artifact name prefix, default `release-metadata`

## Outputs

- `version`: normalized release version without the leading `v`
- `tag`: normalized git tag in the form `vX.Y.Z` or `vX.Y.Z-PRERELEASE`
- `cargo_version`: version read from `Cargo.toml`
- `previous_stable_tag`: most recent stable tag found in git, if any
- `release_notes`: extracted `CHANGELOG.md` notes for the target release section

## Uploaded Artifacts

- `release-metadata-summary.md`: human-readable release summary
- `release-metadata.json`: machine-readable metadata for follow-up automation

## Typical Usage

Used from release-oriented workflows such as:

- `.github/workflows/release-preparation.yml` (after `resolve-release-pr` validates the upstream release PR)
- `.github/workflows/release.yml`
