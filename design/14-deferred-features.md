# 14. Deferred features

These ideas are desirable but are explicitly not part of the immediate language
core:

- Interface extension/default methods.
- A `satisfies` operator that preserves an anonymous object's exact hidden type.
- Nominal data-carrying enums.
- General `match` expressions and exhaustive pattern matching.
- General iterable types and collection-based `for item in collection` loops;
  initial `for` loops iterate only over integer ranges written in the header.
- General user-defined parameterized nominal declarations and generic
  interfaces, general-purpose generic inference, compile-time values other than
  types, arbitrary compile-time execution, compile-time duck typing, and
  associated types. Chapter 19's explicit bounded runtime templates and
  type-factory-generated nominal structs are implemented separately.
- `errdefer`.
- Vector and Map APIs, mutation rules, and runtime representations beyond their
  reserved type and empty-construction syntax.
- Multi-error program parsing and partial-AST production. A future
  declaration-level recovery pass should abandon a malformed braced declaration
  and synchronize at its matching outer `}`, rather than the next arbitrary
  brace. Recovery within statement blocks is a separate concern.
- A Cranelift JIT or native object backend.
- A built-in linker or executable writer.
- Native threads and shared-memory concurrency.
- Coroutine handles, joining, cancellation, blocking queue operations, and
  scheduler preemption.
- Modules, imports, visibility and access control, external package management,
  and separate compilation.

The IR should avoid preventing these additions, but the first implementation
does not need to support them.
