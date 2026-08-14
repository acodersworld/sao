# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-14

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

Name, scope, and context resolution are implemented. Together they
provide semantic symbol identities, nested value and type scopes, forward
declaration collection, complete name resolution, sequential local-binding
shadowing, callable classification, structural control targets, and diagnostics
for invalid names, declarations, receivers, contextual control flow, `self`, and
assignment-target shapes. AST node identities and source spans are
module-qualified; source registration remains separate from future entry-module
selection.

## Semantic analysis work queue

Semantic analysis is one compiler subsystem with ordered internal passes.
Validation is performed by the earliest pass that has enough information for
the rule rather than collected into one miscellaneous final pass.

Recommended implementation order:

1. Type checking and inference (next)
   - Define semantic types and produce type information for expressions,
     bindings, declarations, and callable signatures.
   - Check operators, calls, assignments and mutability, returns, blocks, loop
     results, ranges, slices, conversions, parameterized built-ins, unions,
     intersections, structs, and structural interface satisfaction.
   - Validate all `const` and `mut` restrictions in one place, including
     immutable range induction bindings and mutable receiver access.
   - Validate the required `main` signature after its parameter and return types
     are known.
   - Infer local binding types and `Error::new(value)` payloads within the
     documented inference boundary. Validate the compiler-known `new` associated
     functions and their call arities. Settle `bool(value)` before implementing
     its primitive conversion rules.
2. Post-type semantic analysis
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
