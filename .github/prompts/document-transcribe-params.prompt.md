---
name: "Document Transcribe Params"
description: "Generate the German Markdown reference for all streaming transcribe provider parameters"
argument-hint: "Optional: output path (default: /tmp/context-switch-transcribe-parameter.md)"
agent: "agent"
---

Generate a German parameter reference for every streaming transcribe provider in this workspace.

Write the result to `/tmp/context-switch-transcribe-parameter.md` unless the user supplies a different output path.

Treat the current Rust `Params` types as the authoritative contract:

1. Start from `examples/transcribe.rs` to identify the providers wired by the example.
2. Read each corresponding `services/*/src/transcribe.rs` file and any immediate mapping code needed to determine how every `Params` field is sent to the provider.
3. Include every accepted parameter, including serde aliases, defaults, nested types, and provider-neutral settings that are only partially forwarded.
4. Use the official provider documentation to refine descriptions with verified defaults, valid ranges, constraints, model requirements, audio requirements, or language formats. Do not invent details when no official source is available.
5. Verify external documentation links before including them. Prefer official provider documentation; where unavailable, use the relevant official crate, repository, or example.

Use this output structure:

- A German title and a short introduction explaining that JSON names follow the serde wire format and that code blocks are JSONC documentation rather than sendable JSON.
- One `##` section per provider, named after the provider.
- In each provider section, a JSONC block containing every `Params` field. Put an adjacent German `//` comment on each field so the parameters remain the focal point.
- Follow each block with only the relevant provider behavior, integration defaults, audio restrictions, and environment-variable or CLI wiring notes.
- Add a `## CLI-Parameter nach Provider` table for the flags in `examples/transcribe.rs`. Explicitly call out flags that are accepted or validated but not passed into a provider's `Params` instance.
- Include a compact `Quelle` or `Quellen` line in every provider section with working documentation links.

Keep the document precise and parameter-oriented:

- Use the exact camelCase JSON field names from serde.
- Mark required versus optional fields, defaults, accepted values, units, and constraints where known.
- Explain adapter-specific transformations near the affected parameter, such as BCP 47 conversion, endpoint aliases, model selection, or ignored turn-detection fields.
- Distinguish clearly between context-switch behavior and external provider API settings.
- Do not modify repository source files for this task unless the user explicitly asks for code documentation updates too.

Before finishing, validate that the file is non-empty, has a section for every discovered provider, contains the CLI table, and does not retain known-invalid links. Report the output path and validation result concisely.