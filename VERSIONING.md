# Versioning

The workspace uses a single shared version (`[workspace.package] version` in the
root `Cargo.toml`); all member crates inherit it via `version.workspace = true`.

## When to bump

- **Every behavioral change** bumps the version by at least a patch increment,
  in the commit that introduces the change. Behavioral changes include new or
  altered runtime behavior, protocol changes, new configuration, and new
  warnings or log outcomes that affect operation.
- **Non-behavioral changes** (comment updates, doc rewording, formatting,
  refactors without observable behavior change) do not require a bump.
- Bump the minor version for new features or compatible protocol extensions,
  and the major version for breaking protocol or interface changes, following
  Semantic Versioning.

## Release history

| Version | Change |
|---------|--------|
| 3.9.0   | google-dialog: Gemini 3.8 Live models and interaction semantics |
| 3.8.0   | google-transcribe: numerals support via digit-sequence class token (#99) |
| 3.8.1   | audio-knife: startup self-test warning for `GOOGLE_APPLICATION_CREDENTIALS` (#100) |
