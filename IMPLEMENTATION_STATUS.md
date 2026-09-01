# Implementation status

This file records what the source tree currently implements. Active work and
its recommended order are tracked separately in
[`CURRENT_WORK.md`](CURRENT_WORK.md).

The design documents remain the language specification, and the source remains
the implementation. Periodically compare all three and update this file when
their status diverges.

Last reviewed: 2026-09-01

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
After member-access `.`, consecutive non-negative integer tuple fields are
tokenized separately without changing ordinary decimal float literals.

## Parser status

The parser currently supports:

- Single-expression, single-type, single-statement, and whole-program entry
  points.
- File-level programs containing ordered top-level function, named struct,
  structural interface, and transparent type-alias declarations.
- Semicolon-terminated interface method requirements with receiver, parameter,
  and return-type syntax.
- Primitive, named, parameterized, mutable, grouped, callable, tuple, union,
  and intersection type syntax, plus explicit `&T`/`&mut T` GC qualification
  and `*T`/`*mut T` tracked-reference qualification. Comma-bearing parentheses form tuples, singleton tuples
  require their comma, and `()` remains unit.
- Literals, identifiers, `self`, grouping, tuple values, and expression-oriented
  blocks.
- Prefix, binary, assignment, type-test, and postfix operators.
- Prefix GC allocation with `&expression`, while retaining infix bitwise-and
  and reserving `&&` exclusively for logical-and.
- Calls, value-member access with `.`, type-associated access with `::`,
  indexing, exclusive open or bounded slicing, and postfix error propagation
  with `?`.
- Explicit primitive conversion ascriptions for the supported numeric and
  character conversions; primitive call syntax is not a conversion form.
- Fixed-arity `Queue(T)`, `Vector(T)`, `Map(K, V)`, and `Error(T)` types, plus
  associated access on those types. Their constructors use ordinary associated
  calls such as `Queue(T)::new()`; both inferred `Error::new(value)` and explicit
  `Error(T)::new(value)` forms are represented for later type checking.
- File-level and receiverless associated type factories returning `type`,
  generated nominal struct type expressions, `comptime` type parameters,
  explicit runtime-template calls, and named, intersected, or anonymous
  interface constraints in `where` clauses.
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
- Maintains one nested lexical declaration namespace shared by named value and
  type declarations, with context-sensitive value/type lookup and
  compiler-known values supplied through a shadowable prelude scope.
- Collects top-level declarations and immediate nested functions before
  resolving their uses, allowing documented forward references and recursion.
- Resolves ordinary bindings only after their initializer, preserving
  sequential same-block shadowing semantics.
- Records every lexical value and named-type reference by module-qualified AST
  node identity against its resolved symbol identity. The type qualifier in an
  associated access resolves in the type namespace, while member names and
  `self` remain for later passes.
- Resolves leading explicit type arguments on top-level template calls and
  provisionally on member calls whose final declaration requires receiver type
  information.
- Diagnoses unknown names, cross-kind duplicate declarations, duplicate or
  invalid template constraints, and missing or non-unique top-level `main`
  functions.

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
canonical type store for capability-qualified primitives, callables, ordered
structural tuples, nominal
named, anonymous, and factory-generated structs, interfaces, compiler-known parameterized types,
unions, intersections, canonical explicit GC references, canonical tracked
non-GC references, and internal recovery
and divergence types. Union and
intersection construction is associative, commutative, and idempotent, with an
outer capability distinct from member capabilities. The store exposes exact
identity, equality that ignores only the outer capability, safe structural
lookup, inline/borrowed/tracked/GC storage semantics, compiler-defined copy semantics,
and typed-expression value categories. Source type resolution predeclares named
structs and interfaces, resolves all explicit annotations including mutable and
GC- and tracked-qualified forms, records canonical types by syntax and declaration identity,
supports forward and recursive references, transparent forward aliases,
compile-time type factories, cached generated struct applications, symbolic
bounded-template parameters, and owner-specialized generated syntax. It
diagnoses invalid applications and constraints, alias cycles, expanding factory
recursion, compiler-known arity, queue element types, and non-interface
intersection members. Unknown type names are diagnosed earlier by name
resolution. Tracked-reference expression checking forms `*T` borrows from
compatible plain and GC-backed storage, preserves or reduces target capability,
automatically dereferences tracked aggregate, tuple, interface, and supported
primitive members through `.`, and records stable physical roots plus
field/tuple paths. Direct tracked bindings and reference-slot assignments reject
plain and GC temporary sources, while a non-escaping call may borrow one.
There is no general `*T` into `T` or `&T` conversion; a `*T` argument may supply
the existing address for a plain by-reference aggregate parameter `T`, but not
for a binding, field, return, or GC parameter. Callables returning a tracked
reference link it conservatively to every tracked parameter and tracked
`*self`/`*mut self` receiver. Returned paths must originate exclusively from
those tracked inputs, and caller-side bindings or reference assignments reject
linked results whose lifetime intersection includes a plain or GC temporary.
Plain structs, tuples, unions, and nested inline values may contain tracked
references and preserve the full origin intersection through construction,
bindings, copying, projection, union conversion, calls, and returns. Direct
tracked returns retain their stricter tracked-parameter rule, while aggregate
returns may also propagate origins already carried by borrow-containing inputs.
GC allocation, GC fields, and `Queue`/`Vector`/`Map` external-buffer storage of
transitively borrow-containing types are rejected. Tracked origins merge
through conditionals and loop fixed points. Redirected reference slots preserve
distinct displaced backing storage, replacement of an ancestor with a live
interior reference is rejected, last-use analysis ends constraints
non-lexically, and GC-backed origins record the owner roots required for the
tracked holder's live range. Ordinary callables, generated methods, and runtime
callable specializations retain the same complete private-origin,
lifetime-intersection, GC-owner-root, and borrow-validity metadata for post-type
escape analysis, typed IR, and lowering.

Declaration and signature collection is implemented for source and generated
struct fields and functions, callable headers, interface requirements,
owner-independent structural method identities, compiler-known signatures,
template constraints, and owner-specialized generated callables.

Expression type checking and inference is implemented through the current
frontend. It covers value categories and transfers, places and mutability,
calls and receiver validation, structural interfaces, unions and narrowing,
control flow and loop values, lambdas and capture/escape rules, explicit GC
allocation, built-in sequences and conversions, formatted strings, finite
inline-layout validation, type-factory-generated values, and bounded runtime
templates. Top-level and method templates require explicit type arguments;
method specialization identity includes the concrete named or generated owner.
Specializations reuse exact recursion, reject expanding recursion, validate
constraints before checking ordinary arguments, and retain per-specialization
analysis metadata for later typed IR and lowering.

Tuple checking includes contextual and inferred construction, ordered
structural identity, numeric element places, transitive capability, owning
transfers, recursive copying, explicit GC storage, non-escaping element
propagation, and finite inline-layout traversal. Tuple types compose with
aliases, type-factory results, explicit runtime-template specialization,
callable signatures, exact union injection and narrowing, and legal
compiler-known parameterized type arguments. Destructuring, iteration, spreads,
variadics, named elements, tuple operators, and tuple-specific library
operations remain deferred.

Typed IR production, backend lowering, object emission, and runtime integration
are not yet implemented.

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
- General user-defined parameterized nominal declarations and generic
  interfaces, generic inference, and compile-time values other than types.
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
