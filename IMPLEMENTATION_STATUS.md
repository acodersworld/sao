# Implementation status

This file records what the source tree currently implements. Active work and
its recommended order are tracked separately in
[`CURRENT_WORK.md`](CURRENT_WORK.md).

The design documents remain the language specification, and the source remains
the implementation. Periodically compare all three and update this file when
their status diverges.

Last reviewed: 2026-08-20

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
  intersection type syntax, plus explicit `&T` and `&mut T` GC qualification.
- Literals, identifiers, `self`, grouping, and expression-oriented blocks.
- Prefix, binary, assignment, type-test, and postfix operators.
- Prefix GC allocation with `&expression`, while retaining infix bitwise-and
  and reserving `&&` exclusively for logical-and.
- Calls, value-member access with `.`, type-associated access with `::`,
  indexing, exclusive open or bounded slicing, and postfix error propagation
  with `?`.
- Dedicated `int`, `float`, `bool`, `char`, and `string` conversion expressions
  with exactly one argument.
- Fixed-arity `Queue<T>`, `Vector<T>`, `Map<K, V>`, and `Error<T>` types, plus
  associated access on those types. Their constructors use ordinary associated
  calls such as `Queue<T>::new()`; both inferred `Error::new(value)` and explicit
  `Error<T>::new(value)` forms are represented for later type checking.
- Named struct construction and unconstrained anonymous `struct { ... }`
  expressions, including initialized fields and methods.
- Immutable and mutable bindings.
- Named and nested functions, receiverless named-struct functions, method
  receivers including `&self` and `&mut self`, returns, and lambdas.
- Call-only `defer` and coroutine-start `co` statements.
- `if`/`else if`/`else`, `loop`, `while`, and integer range `for` expressions.
- `break`, `continue`, and value-producing loop syntax.
- Exclusive and inclusive range headers using `..` and `..=`.

Every parser entry point intentionally fails on the first lexical or syntax
error. Whole-program parsing does not produce a partial AST or collect multiple
diagnostics.

Source files are registered as immutable, shared source modules. Module zero is
reserved for compiler-provided prelude definitions, ordinary module allocation
begins at one, and registration does not choose a compilation entry module.
Lexer spans and all AST node identities are module-qualified. Parser entry
points allocate node IDs through a caller-owned per-module parse context and
reject token streams belonging to another module.

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
a local annotation or parameter may later borrow its hidden concrete type as a
satisfied plain interface. Field, capture, and return contexts require a
GC-qualified interface and implementation. `Writer { ... }` is not an interface
implementation expression.

## Semantic analysis status

Name and scope resolution is implemented. The resolver:

- Assigns stable symbol identities to top-level declarations, nested named
  functions, parameters, local bindings, and range induction bindings.
- Maintains separate nested value and type namespaces, with compiler-known
  values supplied through a shadowable prelude scope.
- Collects top-level declarations and immediate nested functions before
  resolving their uses, allowing documented forward references and recursion.
- Resolves ordinary bindings only after their initializer, preserving
  sequential same-block shadowing semantics.
- Records every lexical value and named-type reference by module-qualified AST
  node identity against its resolved symbol identity. The type qualifier in an
  associated access resolves in the type namespace, while member names and
  `self` remain for later passes.
- Diagnoses unknown names, invalid duplicate declarations, and missing or
  non-unique top-level `main` functions.

Nested named-function references still use ordinary lexical resolution. The
later capture-analysis pass will use those resolutions to distinguish legal
global and self references from forbidden enclosing-function captures.

Context resolution is implemented as an AST-only pass. It:

- Classifies top-level and nested functions, named-struct methods and associated
  functions, anonymous-struct methods, lambdas, and interface requirements.
- Requires first-position receivers where appropriate and diagnoses forbidden,
  missing, misplaced, and duplicate receivers.
- Validates `self`, `return`, `break`, `continue`, `defer`, and `co` against
  their lexical method, callable, loop, and executable-block contexts.
- Records owning methods for `self`, target callables for `return`, and target
  loops for `break` and `continue`, all keyed by module-qualified AST node ID.
- Rejects assignment targets other than ungrouped identifiers, member accesses,
  and index expressions. Direct assignment to `self` is invalid, while mutation
  through `self.member` remains subject to later type checking.
- Leaves every `const` and `mut` restriction, including range induction-binding
  immutability, to the type checker.

The semantic type foundation and source type resolution are implemented. They
provide a program-local
canonical type store for capability-qualified primitives, callables, nominal
named and anonymous structs, interfaces, compiler-known parameterized types,
unions, intersections, canonical explicit GC references, and internal recovery
and divergence types. Union and
intersection construction is associative, commutative, and idempotent, with an
outer capability distinct from member capabilities. The store exposes exact
identity, equality that ignores only the outer capability, safe structural
lookup, inline/borrowed/GC storage semantics, compiler-defined copy semantics,
and typed-expression value categories. Source type resolution predeclares named
structs and interfaces, resolves all explicit annotations including mutable and
GC-qualified forms, records canonical types by syntax and declaration identity,
supports forward and recursive references, and diagnoses invalid named type
arguments, compiler-known arity, queue element types, and non-interface
intersection members. Unknown type names are diagnosed earlier by name
resolution.

Declaration and signature collection, expression type checking and inference,
assignability, finite-layout validation, capture and escape analysis, hidden-root
calculation, and typed IR production are not yet implemented.

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

## Deferred features

Do not treat the following as immediate implementation work unless the design
scope changes:

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

When synchronizing design, status, current work, and source:

1. Confirm each non-deferred design syntax has an AST representation.
2. Confirm each AST form is parsed and pretty-printed.
3. Confirm positive, composition, malformed-input, and diagnostic tests exist.
4. Remove completed items from the active queue in `CURRENT_WORK.md` and update
   the relevant status section here.
5. Record newly deferred or newly agreed syntax in the design documents and the
   appropriate tracking file.
6. Update the review dates in each tracking file that was checked.
