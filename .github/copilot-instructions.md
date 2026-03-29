# Copilot Instructions

## Rust Style
- Prefer imports over deeply-qualified module paths. As a rule of thumb, avoid using more than one module prefix inline (for example, prefer importing a type and using `TypeName` instead of writing `foo::bar::TypeName` repeatedly).
- Prefer high-level flow first: when practical, place local supporting definitions (for example helper structs, impls, functions, and type aliases) below their first use.
- Keep imports grouped and sorted to match existing file style.
