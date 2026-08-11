# Current work

This file is a concise implementation snapshot. It exists to avoid repeatedly
reconstructing project status from the design documents and source tree.

The design documents remain the language specification, and the source remains
the implementation. Periodically compare all three and update this file when
their status diverges.

Last reviewed: 2026-08-11

## Lexer status

The lexer implements the lexical grammar in `design/16-lexical-grammar.md`,
including ASCII identifiers and literals, nested block comments, conservative
decimal numbers, every documented operator, byte spans, recoverable lexical
errors, and an explicit end-of-file token.

`co` and `defer` are reserved and tokenized for their implemented call-only
statement forms. The compiler-known names `Queue`, `Vector`, `Map`, and `Error`
are also reserved and tokenized distinctly from identifiers. `..` and `..=` are
tokenized for the implemented range-`for` grammar, and `..` also delimits
exclusive `string` and `bytes` slices. Inclusive `..=` slices are rejected.

## Parser status

The parser currently supports:

- Single-expression, single-type, single-statement, and whole-program entry
  points.
- File-level programs containing ordered top-level function, named struct, and
  structural interface declarations.
- Semicolon-terminated interface method requirements with receiver, parameter,
  and return-type syntax.
- Primitive, named, parameterized, mutable, grouped, callable, union, and
  intersection type syntax.
- Literals, identifiers, `self`, grouping, and expression-oriented blocks.
- Prefix, binary, assignment, type-test, and postfix operators.
- Calls, member access, indexing, exclusive open or bounded slicing, and postfix
  error propagation with `?`.
- Dedicated `int`, `float`, `bool`, `char`, and `string` conversion expressions
  with exactly one argument.
- Fixed-arity `Queue<T>`, `Vector<T>`, `Map<K, V>`, and `Error<T>` types, plus
  their canonical construction expressions. Both inferred `Error(value)` and
  explicit `Error<T>(value)` forms are supported.
- Named struct construction and unconstrained anonymous `struct { ... }`
  expressions, including initialized fields and methods.
- Immutable and mutable bindings.
- Named and nested functions, method receivers, returns, and lambdas.
- Call-only `defer` and coroutine-start `co` statements.
- `if`/`else if`/`else`, `loop`, `while`, and integer range `for` expressions.
- `break`, `continue`, and value-producing loop syntax.
- Exclusive and inclusive range headers using `..` and `..=`.

Every parser entry point intentionally fails on the first lexical or syntax
error. Whole-program parsing does not produce a partial AST or collect multiple
diagnostics.

Range bounds use a deliberately restricted grammar. An unparenthesized bound may
be a primary expression, a postfix chain, or unary negation. Infix, assignment,
lambda, struct construction, and block-like bounds must be parenthesized. For
example:

```text
for index in -start..items.length() {}
for index in (start + offset)..(if ready { limit } else { fallback }) {}
```

Brace-based struct construction is also disabled directly in `if`, `while`,
and range `for` heads so the following brace always begins the control-flow
body. Parentheses re-enable it, as in `if (Position { x: 1.0, y: 2.0 }) {}`.

Interfaces use contextual structural conversion rather than dedicated
construction syntax. `struct { ... }` is the only anonymous-struct expression;
an annotation, parameter, or return type may later convert its hidden concrete
type to a satisfied interface during semantic analysis. `Writer { ... }` is not
an interface implementation expression.

## Parser work queue

No additional parser work is currently queued.

## Runtime prototype status

The C code under `runtime/` is an isolated runtime prototype; the compiler does
not emit or link against it yet. It currently provides:

- An intrusive FIFO list used by the scheduler and C test harness.
- Resumable functions with explicit call, yield, and return statuses.
- Copied, aligned activation records held on a bounded task-owned byte stack.
- A FIFO cooperative scheduler in which the first successfully queued task is
  `main`, completed non-main tasks yield to the next ready task, and completion
  of `main` abandons the remaining tasks.
- A temporary universally tagged `SaoValue` used to pass prototype call results.

The scheduler behaviour matches the ordering and main-termination rules in the
coroutine design. The storage and value representations do not yet implement the
final design: there is no tracing collector, heap-linked activation-frame chain,
specialized union layout, concrete object model, queue object, deferred-call
storage, or compiler integration. In particular, `SaoValue` and the bounded byte
stack are prototype mechanisms rather than the language ABI described in
`design/12-backend-oriented-lowering.md` and
`design/13-runtime-representation-and-memory-management.md`.

## Work outside the parser

The following require semantic analysis or later compiler phases rather than
additional parsing:

- Name and scope resolution.
- Entry-point validation, including the required unique `main` signature.
- Type checking and inference.
- Parameterized built-in element/key/payload validation, `Error(value)` payload
  inference, and the eventual Vector and Map APIs and lowering.
- Primitive conversion validation. The accepted inputs and result of
  `bool(value)` still require a design decision before semantic implementation.
- Assignment-target and mutability validation.
- Slice receiver and bound type validation, runtime negative-bound
  normalization and bounds checks, and allocating copy implementation.
- Validating `self`, `return`, `break`, and `continue` placement.
- Range-bound types and immutable induction bindings.
- Loop result and `else` typing.
- Lambda, nested-function, and anonymous-object capture analysis.
- Struct field validation, interface satisfaction, and contextual
  struct-to-interface conversion.
- Error propagation, coroutine, and `defer` lowering.
- Integrating or replacing the runtime prototype as typed IR and code generation
  adopt the designed frame, value, object, queue, and garbage-collection models.

## Deferred language features

Do not treat the following as immediate parser work unless the design scope
changes:

- Collection-based `for item in collection` loops.
- General `match` expressions and pattern matching.
- User-defined generics.
- Vector and Map APIs and runtime representations.
- Multi-error program parsing and partial-AST recovery. If declaration-level
  recovery is added, it should abandon the malformed braced declaration and
  synchronize at that declaration's matching outer `}`, not the next arbitrary
  brace. Statement-level recovery remains a separate deferred feature.
- Nominal data-carrying enums.
- Modules, imports, and visibility.
- `errdefer`.

See `design/14-deferred-features.md` for the complete deferred-feature list.

## Synchronization checklist

When synchronizing design, current work, and source:

1. Confirm each non-deferred design syntax has an AST representation.
2. Confirm each AST form is parsed and pretty-printed.
3. Confirm positive, composition, malformed-input, and diagnostic tests exist.
4. Remove completed items from the work queue and update the parser-status list.
5. Record newly deferred or newly agreed syntax in both the design documents and
   this file.
6. Update the review date above.
