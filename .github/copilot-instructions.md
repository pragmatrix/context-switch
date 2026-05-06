# Copilot Instructions

## Rust Style
- Prefer imports over deeply-qualified module paths. As a rule of thumb, avoid using more than one module prefix inline (for example, prefer importing a type and using `TypeName` instead of writing `foo::bar::TypeName` repeatedly).
- Prefer high-level flow first: when practical, place local supporting definitions (for example helper structs, impls, functions, and type aliases) below their first use.
- Keep imports grouped and sorted to match existing file style.
- Avoid `maybe_` prefixes for optional variables; use neutral names and rely on type/context for optionality.
- Avoid `_ref` suffixes for local variable names; use descriptive neutral names instead.

## Change Communication
- Include a short rationale for each non-trivial code change.

## Code Minimalism
- Avoid defensive code unless there is concrete evidence it is necessary.
- Avoid redundant logic and repeated calls; keep only the minimal behavior required for correctness.
- Do not add tests unless explicitly requested by the user.

## Control Flow Style
- Prefer exhaustive `match` statements for enum-based control flow instead of `if matches!(...)` shortcuts.

## Memory Promotion
- When a durable repository-specific preference is learned during a session, write it into this file as a concise bullet if it can help future sessions.
- Keep additions short, actionable, and scoped to coding behavior in this repository.
- Do not add temporary experiment details or one-off debugging notes.
