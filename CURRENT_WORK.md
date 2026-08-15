# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-15

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
insensitive shape equality, and value-semantics metadata are implemented.

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
     built-ins; shallow-copied/reference value metadata; and internal recovery
     and divergence types.
   - Complete: capability-qualified canonical union and intersection identities
     with associative flattening, exact-member deduplication,
     order-independent identity, and capability-preserving singleton collapse.
   - Complete: store-validated exact identity, equality that ignores only the
     outer capability, safe structural lookup, and capability and value-
     semantics metadata on the canonical type representation.
2. Declarations and signatures (next)
   - Next increment: resolve source `TypeSyntax` into canonical `TypeId`s and
     record the resolved type for each type-syntax node. Diagnose unknown named
     types and invalid compiler-known type arguments while preserving recovery
     types so independent errors can still be collected.
   - Resolve type syntax and collect struct fields, methods, interface
     requirements, callable signatures, and built-in signatures before checking
     bodies.
   - Support recursive and forward-referenced declarations.
   - Validate type arguments, member namespaces, and the required `main`
     signature.
3. Core expression checking
   - Add expected-type-driven checking and local inference.
   - Cover literals, identifiers, `self`, functions, lambdas, calls, operators,
     conversions, blocks, conditionals, returns, and ordinary bindings.
   - Do not synthesize unions when result paths disagree.
4. Places and mutability
   - Model writable locations separately from ordinary values while retaining
     capability in semantic types.
   - Check binding, parameter, receiver, field, index, assignment,
     compound-assignment, and range-binding mutability.
   - Permit shallow-copied values to acquire independent mutable storage
     without allowing reference capabilities to increase.
5. Aggregates and structural typing
   - Check named and anonymous struct construction, fields, methods, associated
     functions, member selection, and structural interface satisfaction.
   - Record resolved member and call targets and implicit conversions required
     by typed IR.
   - Implement exact interface method-signature matching.
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

## Later compiler work

After semantic analysis, the remaining compiler work includes:

- Runtime slice negative-bound normalization, bounds checks, and allocating
  copy implementation.
- Error propagation, coroutine, and `defer` lowering.
- Typed IR and code generation, including integration or replacement of the
  runtime prototype with the designed frame, value, object, queue, and
  garbage-collection models.

Deferred features are recorded in `design/14-deferred-features.md` and summarized
in [`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md).
