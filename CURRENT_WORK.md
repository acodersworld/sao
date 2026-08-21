# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-21

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
semantic symbol identities, nested value and type scopes, forward declaration
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
3. Core expression checking (next)
   - Add expected-type-driven checking and local inference.
   - Cover literals, identifiers, `self`, functions, lambdas, calls, operators,
     conversions, GC allocation, blocks, conditionals, returns, and ordinary
     bindings.
   - Record fresh-temporary, owned-inline, borrowed-place, and GC-reference
     categories plus moves, implicit return copies, and hidden GC-owner roots.
   - Do not synthesize unions when result paths disagree.
4. Places and mutability
   - Model writable locations separately from ordinary values while retaining
     capability in semantic types.
   - Check binding, parameter, receiver, field, index, assignment,
     compound-assignment, and range-binding mutability.
   - Permit copied values to acquire independent mutable storage without
     allowing borrowed or GC-reference capabilities to increase.
5. Aggregates and structural typing
   - Check named and anonymous struct construction, fields, methods, associated
     functions, member selection, and structural interface satisfaction.
   - Record resolved member and call targets and implicit conversions required
     by typed IR.
   - Implement exact interface method-signature matching.
   - Validate finite inline layouts, reserve the `copy` member name, synthesize
     `.copy()`, and restrict plain erased interfaces and capturing callables to
     non-owning local and parameter positions.
6. Type algebra and flow
   - Implement union and intersection assignability, contextual conversions,
     branch and loop result typing, type tests, and flow-sensitive narrowing.
   - Cover value-producing loops and explicitly expected union types.
7. Built-ins and completion
   - Check strings, bytes, indexing, slicing, primitive conversions, `Queue`,
     `Vector`, `Map`, `Error`, `?`, `ascii`, output, `panic`, `yield`, `co`, and
     `defer`.
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
     values reaching retained positions and record frame tracing, hidden roots,
     recursive copies, and cleanup requirements for typed IR.

## Later compiler work

After semantic analysis, the remaining compiler work includes:

- Runtime slice negative-bound normalization, bounds checks, and allocating
  copy implementation.
- Error propagation, coroutine, and `defer` lowering.
- Typed IR and code generation, including integration or replacement of the
  runtime prototype with the designed frame, value, object, queue, and
  garbage-collection models. Lowering must use caller-owned result slots for
  plain aggregates, inline frame/object layouts, GC payload moves for prefix
  `&`, recursive tracing, hidden borrow-owner roots, and runtime-only cleanup
  for frame-owned auxiliary storage.

Deferred features are recorded in `design/14-deferred-features.md` and summarized
in [`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md).
