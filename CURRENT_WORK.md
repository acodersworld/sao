# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-28

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
   - Phase 7.3, formatted strings (next):
     - Add Python-style `f"...{expression}..."` interpolation as the language's
       primitive-to-text formatting mechanism, replacing `string(value)`.
     - Preserve ordinary string escapes, support literal braces with `{{` and
       `}}`, parse embedded SAO expressions with original source spans, and
       evaluate interpolations once from left to right.
     - Initially format `string`, `int`, `float`, `bool`, `char`, unit, and
       `none`; reject aggregates, interfaces, unions, callables, bytes, and
       compiler-known parameterized built-ins unless narrowed or otherwise
       converted by an explicit operation.
     - Produce a fresh mutable string and record resolved interpolation and
       formatting metadata for typed IR and lowering.
     - Reserve Python's colon format specifications, conversion flags, debug
       syntax, and nested format specifications for a later design increment.
   - Phase 7.4, remaining built-ins and completion:
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
