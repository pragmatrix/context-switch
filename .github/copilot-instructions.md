# Copilot Instructions

## Rust Style
- Prefer imports over deeply-qualified module paths. As a rule of thumb, avoid using more than one module prefix inline (for example, prefer importing a type and using `TypeName` instead of writing `foo::bar::TypeName` repeatedly).
- Prefer high-level flow first: when practical, place local supporting definitions (for example helper structs, impls, functions, and type aliases) below their first use.
- Keep imports grouped and sorted to match existing file style.

## Change Communication
- Include a short rationale for each non-trivial code change.

## Code Minimalism
- Avoid defensive code unless there is concrete evidence it is necessary.
- Avoid redundant logic and repeated calls; keep only the minimal behavior required for correctness.
- Do not add tests unless explicitly requested by the user.
