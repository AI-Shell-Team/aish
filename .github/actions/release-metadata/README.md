# Release Metadata Action

This composite action centralizes release metadata validation for the repository.

It performs four tasks:

- normalizes the requested version or tag into a stable `X.Y.Z` version
- validates that `pyproject.toml` and `src/aish/__init__.py` carry the same version
- extracts the `Unreleased` section from `CHANGELOG.md`
- uploads both a markdown summary and a JSON metadata artifact for downstream review

## Inputs

- `version`: required stable version or tag, for example `0.1.1` or `v0.1.1`
- `artifact_prefix`: optional artifact name prefix, default `release-metadata`

## Outputs

- `version`: normalized stable version without the leading `v`
- `tag`: normalized git tag in the form `vX.Y.Z`
- `pyproject_version`: version read from `pyproject.toml`
- `runtime_version`: version read from `src/aish/__init__.py`
- `previous_stable_tag`: most recent stable tag found in git, if any
- `release_notes`: extracted `CHANGELOG.md` unreleased notes

## Uploaded Artifacts

- `release-metadata-summary.md`: human-readable release summary
- `release-metadata.json`: machine-readable metadata for follow-up automation

## Typical Usage

Used from release-oriented workflows such as:

- `.github/workflows/release-preparation.yml`
- `.github/workflows/release.yml`
