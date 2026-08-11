# 14. Deferred features

These ideas are desirable but are explicitly not part of the immediate language
core:

- Interface extension/default methods.
- A `satisfies` operator that preserves an anonymous object's exact hidden type.
- Nominal data-carrying enums.
- General `match` expressions and exhaustive pattern matching.
- General iterable types and collection-based `for item in collection` loops;
  initial `for` loops iterate only over integer ranges written in the header.
- User-defined generic structs, functions, and interfaces, including generic
  constraints, general-purpose generic inference, and associated types.
- `errdefer`.
- Vector and Map APIs, mutation rules, and runtime representations beyond their
  reserved type and empty-construction syntax.
- A Cranelift JIT or native object backend.
- A built-in linker or executable writer.
- Native threads and shared-memory concurrency.
- Coroutine handles, joining, cancellation, blocking queue operations, and
  scheduler preemption.
- Modules, imports, visibility and access control, external package management,
  and separate compilation.

The IR should avoid preventing these additions, but the first implementation
does not need to support them.
