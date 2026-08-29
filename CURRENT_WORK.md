# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-29

## Current phase

Frontend syntax work is complete for the currently designed language. The
active phase is semantic analysis, followed by typed IR and lowering.

For the semantic-analysis phase, use a hands-on, guided workflow. The project
owner should write a substantial portion of the implementation in small,
reviewable increments, while the assisting agent explains unfamiliar semantic
analysis concepts, helps define each increment, reviews the resulting code, and
supports diagnosis and testing. Do not implement an entire semantic pass on the
owner's behalf unless explicitly asked.

## Completed semantic analysis

Name, scope, and context resolution are implemented. Together they provide
semantic symbol identities, one unified lexical declaration namespace, forward declaration
collection, complete name resolution, sequential local-binding shadowing,
callable classification, structural control targets, and diagnostics for
invalid names, declarations, receivers, contextual control flow, `self`, and
assignment-target shapes. AST node identities and source spans are
module-qualified; source registration remains separate from future entry-module
selection.

The semantic type foundation is also complete: program-local canonical type
identities, capability-qualified value types, normalized unions and
intersections, recovery and divergence types, exact identity, outer-capability-
insensitive shape equality, explicit GC qualification, storage/copy semantics,
and typed-expression value-category metadata are implemented.

Source type resolution is complete. It predeclares nominal struct and interface
types, resolves every concrete type-syntax node to a canonical type identity,
leaves the unspecialized `Error::new` owner for built-in inference, supports
forward and recursive references, and diagnoses invalid named type
arguments, built-in arity, queue element types, and intersection members. Unknown
type names remain diagnostics of the preceding name-resolution pass.

Declaration and signature collection is complete. It records named and anonymous
struct members, every explicit callable header, interface requirements,
owner-independent canonical method identities, callable value types, and the
specified compiler-known signature catalogue. It also validates shared member
namespaces and the required `main` signature while preserving deferred anonymous
field inference.

## Semantic analysis work queue

Semantic analysis is one compiler subsystem with ordered internal passes.
Validation is performed by the earliest pass that has enough information for
the rule rather than collected into one miscellaneous final pass.

### Type-checking roadmap

Implement type checking and inference in the following independently
reviewable phases:

1. Semantic type foundation (complete)
   - Complete: a program-local canonical type interner with opaque type
     identities; capability-qualified primitives and callables; nominal named
     and anonymous structs; declared structural interfaces; compiler-known
     built-ins; canonical explicit GC qualification; inline, borrowed-view,
     and GC storage metadata; compiler-defined copy metadata; typed-expression
     value categories; and internal recovery and divergence types.
   - Complete: capability-qualified canonical union and intersection identities
     with associative flattening, exact-member deduplication,
     order-independent identity, and capability-preserving singleton collapse.
   - Complete: store-validated exact identity, equality that ignores only the
     outer capability, safe structural lookup, and capability, storage, and
     copy metadata on the canonical type representation.
2. Declarations and signatures (complete)
   - Complete: resolve concrete source `TypeSyntax`, including `&T` and `&mut T`,
     into canonical `TypeId`s and record every resolved type-syntax node while
     leaving the unspecialized `Error::new` owner for built-in inference.
     Predeclare named structs and interfaces for forward and recursive references;
     validate named arguments, compiler-known type arguments, queue element
     types, and interface-only intersections while preserving recovery types for
     independent diagnostics.
   - Complete: collect named and anonymous struct members, methods, interface
     requirements, all explicit callable signatures, and the currently specified
     built-in signature templates before checking bodies.
   - Complete: support recursive and forward-referenced declarations, intern
     canonical owner-independent method identities, and retain pending inferred
     anonymous fields for expression checking.
   - Complete: validate shared member namespaces and the required `main`
     signature.
3. Core expression checking (complete)
   - Complete: expected-type-driven checking and local inference.
   - Complete: literals, identifiers, `self`, functions, lambdas, calls,
     operators, conversions, GC allocation, blocks, conditionals, returns, and
     ordinary bindings.
   - Complete: fresh-temporary, owned-inline, borrowed-place, and GC-reference
     categories plus moves, implicit return copies, and object traversal
     metadata.
   - Complete: differing result paths do not synthesize unions.
4. Places and mutability (complete)
   - Complete: model root and field places separately from ordinary values while
     retaining capability in semantic types.
   - Complete: check binding, parameter, receiver, and field mutability;
     reference-slot rebinding; owning field replacement; assignment and
     compound assignment; and reassignment metadata.
   - Complete: permit copied values to acquire independent mutable storage
     without allowing borrowed or GC-reference capabilities to increase.
   - Range-binding mutability is checked with range loops in phase 6. Index-place
     mutability is checked with indexed built-ins in phase 7.
5. Aggregates and structural typing (complete)
   - Complete: named and anonymous struct construction, fields, methods,
     associated functions, member selection, and structural interface
     satisfaction.
   - Complete: resolved member and call targets, interface dispatch metadata,
     and implicit conversions required by typed IR.
   - Complete: exact interface method-signature matching.
   - Complete: finite inline-layout validation, the reserved `copy` member name,
     and compiler-provided `.copy()`.
   - Authoritative capturing-callable escape validation remains in post-type
     semantic analysis.
6. Type algebra and flow (complete)
   - Complete: `loop`, `while`, and integer range `for` expressions, including
     resolved `break` and `continue`, result typing, `else`, divergence,
     range-binding typing and mutability, expected unions, transfers, and
     flow-sensitive binding-provenance merging.
   - Complete: expression type ascription with `expression: Type`, using
     parentheses only when grouping is required. Ascription binds below
     ordinary binary operators and above assignment, consumes a complete union
     or intersection type after `:`, and cannot be chained even through
     parentheses. It checks the expression under the stated expected type,
     allowing an explicit interface or interface-intersection view to
     disambiguate a destination union member.
     Such a view borrows the underlying object without copying or moving it,
     preserves concrete type and vtable identity, permits capability
     preservation or reduction but not escalation, and performs neither
     runtime downcasts nor primitive conversions.
   - Complete: general union and intersection contextual assignability,
     including unambiguous member injection, narrow-to-wide union tag
     remapping when the source member set is an exact `TypeId` subset of the
     destination, structural interface requirement reduction, and member-wise
     union-to-interface views. Plain interface results borrow the active object
     without copying or moving it; GC-qualified results require and reuse an
     existing active GC reference. Inline payloads are never implicitly copied
     or promoted into GC storage.
   - Complete: union-only `is` type tests, TypeScript-style flow-sensitive
     narrowing, and non-lexical runtime tag locks.
     - Accept only an exact normalized member or member subset of the tested
       union. Do not use structural overlap, runtime interface tests, or
       interface downcasts for `is`.
     - Compose narrowing through grouping, `!`, `&&`, and `||`, respecting
       short-circuit evaluation, impossible paths, progressive `else if`
       subtraction, and guard facts that remain valid after a conditional.
     - Track narrowed identifier and resolved-field places by stable symbol and
       field identities. Preserve source capabilities and lexical shadowing.
     - Record private narrowing facts and control-flow-edge lock operations for
       typed IR and lowering. Entering a narrower state increments the physical
       union storage's narrowing counter; the lock remains active while the
       fact is valid, including beyond its originating `if` and across calls.
       Joins retain only facts guaranteed on every reaching path. Reassignment,
       `return`, `break`, `continue`, loop backedges, and callable completion
       release locks that no longer apply. Nested tests and aliases acquire and
       release the same runtime counter independently.
     - Permit mutation of the active payload and replacement by the same union
       member while locked. A replacement that changes the active tag panics
       when the counter is nonzero. A statically visible tag-changing assignment
       first releases its own flow fact; independent locks held through aliases
       remain active and may still reject the mutation.
     - Metadata tests cover branch acquisition and release, surviving guard
       locks, joins, nested locks, exact subset tests, and `!`/`&&`/`||`
       composition.
     - Assignment metadata covers place invalidation, same-tag replacement,
       visible tag changes, root rebinding, field replacement and descendant
       facts, unrelated fields, shadowed symbols, and capability preservation.
     - Lock metadata preserves facts across ordinary arguments and receivers;
       balanced cleanup for `return`, `break`, `continue`, loop backedges, loop
       `else`, callable completion, recovery, and unreachable tails; and
       runtime-facing metadata that distinguishes payload mutation, same-tag
       replacement, and guarded tag changes.
     - Invariant-style helpers simulate every recorded control-flow edge,
       proving counters never become negative and each edge reaches its stated
       destination lock depths. The complex-program test exercises direct,
       compound, and post-guard narrowing.
7. Built-ins and completion (in progress)
   - Phase 7.1, string and byte sequences (complete):
     - Checks `string` and `bytes` indexing with `int` indices and records
       bounds-checked mutable index places. String elements accept `char`; byte
       elements accept `int`.
     - Checks all existing integer compound-assignment operators on mutable
       byte elements and records runtime `0..=255` validation for simple and
       compound writes.
     - Checks optional-`int`, end-exclusive slicing. Slice results are fresh
       mutable sequences with independent copied buffers; lowering metadata
       records negative-bound normalization and invalid-range checks.
     - Resolves `string.length()`, `bytes.length()`, and the first-class
       `bytes::concat` associated function without repeating lookup in typed IR.
     - Focused tests cover result types, categories, places, transfers, runtime
       checks, mutable access, invalid operands, recovery, and source order. The
       complex-program test exercises reads, writes, slicing, length, and byte
       concatenation.
     - Constant evaluation and compile-time range diagnostics remain deferred.
   - Phase 7.2, primitive conversion ascriptions, is complete:
     - Removed conversion-call syntax such as `int(value)`, `float(value)`,
       `char(value)`, and `string(value)` from the AST, parser, analyzer, tests,
       complex-program coverage, and language design.
     - Explicit type ascription now performs only the designed primitive
       conversions: `float -> int`, `int -> float`, `int -> char`, and
       `char -> int`.
       Ordinary annotations, arguments, returns, and other expected-type
       boundaries remain non-converting.
     - Existing exact checking, capability selection, union
       injection/widening, and structural interface-view formation through
       ascription remain unchanged; ascription is not a general cast or
       downcast.
     - Private lowering metadata identifies every actual primitive conversion.
       Runtime-check facts cover finite signed-64-bit validation for
       `float -> int` and ASCII `0..=127` validation for `int -> char`;
       `int -> float` uses its defined binary64 rounding without a check, and
       `char -> int` is total for ASCII characters.
     - Focused tests cover conversion result types, fresh categories, lowering
       and runtime-check metadata, exact ascriptions, invalid conversions,
       recovery without parent cascades, removed call syntax, and non-converting
       ordinary expected-type boundaries.
     - Constant evaluation and compile-time conversion diagnostics remain
       deferred.
   - Phase 7.3, formatted strings, is complete:
     - Python-style `f"...{expression}..."` interpolation is implemented as
       the language's primitive-to-text formatting mechanism, replacing
       `string(value)`. Ordinary string escapes are preserved, `{{` and `}}`
       produce literal braces, and embedded SAO expressions retain their
       original source spans and evaluate exactly once from left to right.
     - Interpolations accept ordinary value expressions but prohibit SAO's
       statement-bearing and direct control-transfer expressions: standalone
       blocks, loops, assignments, and `?`. An `if`/`else if`/`else` expression
       is the sole direct control-flow form; it requires a final `else`, and
       every branch must contain no statements and must explicitly produce a
       value. Any interpolation which nevertheless has the divergence type,
       including a compiler-known diverging call, is rejected rather than
       making the formatted string divergent.
     - Formatting accepts `string`, `int`, `float`, `bool`, `char`, unit, and
       `none`. Aggregates, interfaces, unions, callables, bytes, and
       compiler-known parameterized built-ins are rejected unless the source
       has first been narrowed or converted by an explicit operation.
     - The strict Python-compatible format-specification subset is
       `[[fill]align][sign][0][width][.precision f]`: ASCII fill characters;
       `<`, `>`, and `^` alignment; numeric `+`, `-`, and space signs; numeric
       zero-padding; literal minimum widths; and fixed-point float precision
       such as `.2f`.
     - A top-level `:` inside an interpolation begins its format
       specification. An ascription in that position must therefore be
       grouped, as in `f"{(value: int):>10}"`.
     - Formatted expressions produce a fresh mutable string and record resolved
       interpolation and formatting metadata for typed IR and lowering.
     - `=` alignment, `z`, alternate forms, digit grouping, additional
       presentation types, string precision, dynamic nested specifications,
       conversion flags, and debug syntax remain deferred.
     - Focused coverage exercises interpolation parsing, supported and rejected
       value types, escapes and braces, evaluation order, every supported
       format option, malformed specifications, result semantics, and lowering
       metadata. The complex-program test exercises formatted strings.
   - Phase 7.4, compile-time type factories and bounded templates (in progress):
     - Implement this phase as three independently reviewable changes. Stop
       after each change so it can be reviewed and committed before beginning
       the next one.
     - Change 1, syntax and a unified declaration namespace (complete):
       - Complete: unify type and value declarations into one lexical namespace.
         Reject same-scope named-declaration collisions regardless of kind,
         preserve sequential local-binding and nested lexical shadowing, and
         make the nearest declaration authoritative in both type and value
         contexts.
       - Complete: add `type`, `comptime`, and `where` syntax and the
         corresponding AST representation. Require a receiver first when
         present, followed by all compile-time parameters and then all runtime
         parameters.
       - Complete: add transparent file-level aliases such as
         `type IntBox = Box(int);`. An alias introduces no nominal identity,
         runtime storage, binding mutability, or independent value capability.
       - Complete: parse named and anonymous interface constraints in `where`
         clauses and update every frontend traversal, pretty-printer,
         diagnostic inventory, and focused parser and name-resolution test. A
         type parameter may be constrained either inline or in one `where`
         entry, but not by both forms or by repeated `where` entries. Named
         constraints must denote interfaces; concrete types are rejected.
       - Review and commit this change before implementing type-factory semantics.
     - Change 2, type factories, generated structs, and built-in migration (next):
       - Support type-producing functions at file level and as receiverless
         struct members. A function returning `type` has no receiver or runtime
         parameters and contains one final type expression or explicit
         type-valued return.
       - Restrict type-producing bodies to type parameters, existing type
         composition, generated struct type literals, aliases, and calls to
         known type factories. Defer locals, runtime expressions, conditionals,
         loops, mutation, and arbitrary compile-time execution.
       - Generated structs declare explicitly typed fields without initializers,
         may declare ordinary methods and receiverless associated type
         factories, may reference enclosing compile-time type parameters, and
         cannot capture runtime values.
       - Give generated structs nominal identity derived from their declaration
         and concrete type arguments. Cache factory applications by factory
         identity and canonical argument types, install in-progress placeholders
         for exact recursion, and diagnose expanding instantiation and invalid
         inline recursive layouts deterministically.
       - Permit construction and associated selection through applications such
         as `Box(int) { inner: 10 }` and `Box(int)::function()`. Permit
         associated type-factory calls through statically known concrete types,
         but reject them through a type parameter.
       - Resolve aliases forward, preserve their exact underlying identity, and
         diagnose direct and indirect alias cycles.
       - Replace the compiler-known angle-bracket forms with `Queue(T)`,
         `Vector(T)`, `Map(K, V)`, and `Error(T)` without compatibility aliases.
         Use `Queue(T)::new()`, `Vector(T)::new()`, `Map(K, V)::new()`, and
         `Error(T)::new(value)`, while retaining compiler-known
         `Error::new(value)` expected-type or payload inference.
       - Keep built-in runtime representations compiler-known while routing
         their arity and type application through the shared factory machinery.
       - Stop for review and commit before implementing runtime templates.
     - Change 3, bounded runtime templates and specialization:
       - Support top-level templated runtime functions and templated struct
         methods. Type arguments are explicit, appear in declared order, and
         are never inferred; unspecialized templates are not first-class
         callable values.
       - Support concise named constraints such as `comptime T: Reader` and
         private named, intersection, or anonymous interface constraints in a
         `where` clause.
       - Keep interfaces runtime-oriented: every requirement has `self`, uses
         only runtime parameters and return types, and cannot declare
         `comptime` parameters or return `type`.
       - Allow only named or generated concrete structs and their GC-qualified
         forms to satisfy bounded parameters. Reuse exact canonical method
         identities and existing receiver storage and capability rules.
       - Check template member use against declared constraints. Do not expose
         concrete-only fields, methods, constructors, `.copy()`, associated
         functions, or primitive operators after specialization.
       - At each requested specialization, substitute concrete type identities
         and reuse ordinary expression analysis for exact capabilities, places,
         transfers, returns, layouts, and diagnostics. Cache callable
         specializations and record their identities for typed IR and lowering.
       - Permit exact recursive specialization and diagnose unbounded
         type-expanding specialization deterministically. Defer local template
         declarations and local type aliases.
       - Add focused coverage for namespace collisions, aliases, factory
         composition and identity, generated structs, recursion, constraints,
         explicit specializations, specialization-dependent value semantics,
         diagnostics, recovery, and the migrated built-in syntax. Update the
         complex-program test to exercise the complete feature.
       - Update the language design and implementation-status documents and
         mark Phase 7.4 complete only after this third change has been reviewed.
       Stop for review and commit before beginning Phase 7.5.
     - Compile-time values other than types, arbitrary compile-time execution,
       generic inference, first-class template values, local templates, and
       compile-time duck typing remain deferred.
   - Phase 7.5, tracked non-GC references and lifetime links (pending):
     - Add `*T` as a first-class tracked borrowed-reference type. It describes
       a lifetime relationship, not a raw pointer or a distinct argument-passing
       convention. Continue passing ordinary plain aggregate parameters by
       reference: `value: T` is a call-scoped borrow, while `value: *T` is a
       tracked borrow which may contribute to an escaping `*` result.
     - Preserve the three distinct storage contracts: `T` is a plain value,
       `&T` is a GC-owned reference, and `*T` is a non-owning reference to
       storage owned elsewhere. Permit plain and GC-backed values to borrow as
       `*T`; never implicitly convert `*T` into `T` or `&T`, because those
       conversions would require copying or allocation. Continue using `.` for
       member access with automatic dereferencing rather than adding `->`.
     - Link a returned `*T` to every `*` parameter of the callable. At a call,
       the result receives the intersection (shortest remaining lifetime) of
       all tracked arguments. Non-`*` parameters do not contribute, even though
       plain aggregate parameters are passed by reference. This deliberately
       conservative rule avoids named lifetime parameters; a later feature may
       permit declaring a smaller contributing set if necessary.
     - Support `*self` as the tracked receiver form. It participates in the
       returned lifetime exactly like a named `*T` parameter, allowing methods
       to return references derived from stable inline receiver fields. An
       ordinary `self` receiver remains call-scoped and cannot supply an
       escaping tracked result.
     - Require every returned tracked reference to derive from at least one
       tracked parameter, through any number of stable inline fields. Reject
       references derived from ordinary parameters, GC parameters declared as
       `&T`, locals, fresh temporaries, or other callable-owned storage. For
       example, `fn inner(input: &Input) -> *Inner { input.inner }` is invalid,
       while `fn inner(input: *Input, other: Input) -> *Inner { input.inner }`
       is valid and links the result only to `input`.
     - Allow a caller's `T` or `&T` value to satisfy a `*T` parameter. Propagate
       the caller-side storage lifetime through the result, and keep a GC owner
       rooted while a tracked reference into its payload remains live. Do not
       treat an `&T` parameter as an implicit return-lifetime source merely
       because its value can be borrowed locally as `*T`.
     - Do not extend temporary lifetimes to make an escaping tracked reference
       valid. A temporary may be borrowed for the duration of a call, but a
       tracked result derived from it cannot escape the complete expression;
       therefore `const inner = project(Item {});` is invalid when `project`
       returns a reference into its argument. Apply the same rule to temporary
       GC allocations rather than creating hidden lifetime-extending storage.
     - Preserve or reduce target access capability through a tracked borrow but
       never increase it. Track the physical source storage and field path so
       rebinding a reference slot does not invalidate existing references to
       the old backing storage, while replacement or movement which could make
       an interior address invalid is rejected for the reference's live range.
       Do not implement Rust-style exclusive mutable borrowing: multiple
       tracked references may alias the same stable storage, and `*mut T`
       controls mutation through that particular view rather than proving that
       no other views exist. SAO remains single-threaded, and this phase tracks
       storage validity rather than data-race freedom.
     - Permit plain, stack-resident structs to contain `*T` fields. Such a value
       transitively carries the intersection of the lifetimes of all tracked
       references stored within it. A type containing a tracked reference,
       directly or through another inline aggregate or union, cannot be heap
       allocated: reject GC allocation, GC fields, and storage in external-
       buffer collections. Returning or otherwise propagating a borrow-
       containing plain value must preserve its tracked origins just as
       returning `*T` does.
     - Integrate tracked references with control-flow joins, returns, calls,
       assignments, conditionals, loops, union tag locks, and interface views.
       Record private origin, lifetime-intersection, GC-owner-root, and borrow-
       validity metadata for post-type escape analysis, typed IR, and lowering.
       Reject capturing `*T` or a transitively borrow-containing value in a
       lambda until authoritative capture and escape analysis can prove that
       the callable cannot outlive every source. Keep references into
       relocatable collection buffers deferred until their invalidation rules
       are defined with the corresponding built-ins.
     - Add focused tests for ordinary versus tracked parameters, field-derived
       references, multiple-input lifetime intersections, capability direction,
       plain and GC coercions, GC rooting, invalid local and temporary returns,
       flow joins, shadowing, mutation and rebinding, recovery, and callable
       composition. Update the complex-program test with direct, receiver-
       derived, multi-input, and GC-backed tracked references.
   - Phase 7.6, remaining built-ins and completion (pending):
     - Check `Queue`, `Vector`, `Map`, `Error`, `?`, `ascii`, output, `panic`,
       `yield`, `co`, and `defer`.
   - Aggregate deterministic diagnostics and expose successful semantic
     metadata for expressions, bindings, declarations, and callable signatures.
   - Add comprehensive tests and update implementation-status documents only
     after the complete pass is implemented.

Type checking consumes successful name and context resolution. Capture analysis
and typed IR remain separate post-type work. Do not begin a later phase until
the current phase and its focused tests have been reviewed.

### After type checking

1. Post-type semantic analysis
   - Analyze lambda and anonymous-struct captures, shared mutable capture cells,
     and forbidden captures by nested named functions.
   - Record the Error-propagation, coroutine-call, and deferred-call metadata
     required by lowering, and emit a typed program representation.
   - Run per-function escape analysis after capture discovery. Reject borrowed
     values reaching retained positions and record frame tracing, borrowed-view
     traversal, recursive copies, and cleanup requirements for typed IR.

## Later compiler work

After semantic analysis, the remaining compiler work includes:

- Runtime slice negative-bound normalization, bounds checks, and allocating
  copy implementation.
- Error propagation, coroutine, and `defer` lowering.
- Typed IR and code generation, including integration or replacement of the
  runtime prototype with the designed frame, value, object, queue, and
  garbage-collection models. Lowering must use caller-owned result slots for
  plain aggregates, inline frame/object layouts, GC payload moves for prefix
  `&`, universal object storage attributes, recursive tracing, and runtime-only cleanup
  for frame-owned auxiliary storage.

Deferred features are recorded in `design/14-deferred-features.md` and summarized
in [`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md).
