# Current work

This file tracks the active implementation queue and the work expected to
follow it. The stable inventory of implemented features lives in
[`IMPLEMENTATION_STATUS.md`](IMPLEMENTATION_STATUS.md), and the design documents
remain the language specification.

Last reviewed: 2026-08-12

## Current phase

Frontend syntax work is complete for the currently designed language. The
active phase is semantic analysis, followed by typed IR and lowering.

For the semantic-analysis phase, use a hands-on, guided workflow. The project
owner should write a substantial portion of the implementation in small,
reviewable increments, while the assisting agent explains unfamiliar semantic
analysis concepts, helps define each increment, reviews the resulting code, and
supports diagnosis and testing. Do not implement an entire semantic pass on the
owner's behalf unless explicitly asked.

## Semantic analysis work queue

Semantic analysis is one compiler subsystem with ordered internal passes.
Validation is performed by the earliest pass that has enough information for
the rule rather than collected into one miscellaneous final pass.

Recommended implementation order:

1. Name and scope resolution (implemented)
   - Introduce semantic symbol identities and nested value/type scopes.
   - Collect top-level and nested named declarations, resolve every name, and
     diagnose unknown names, invalid duplicate declarations, and a missing or
     non-unique top-level `main` entry point. Permit sequential local bindings
     to shadow earlier bindings in the same block after their initializer has
     been resolved.
2. Context validation (next)
   - Validate `self`, `return`, `break`, `continue`, `defer`, and `co` against
     their documented enclosing function, method, loop, and executable-block
     contexts.
   - Validate assignment-target shape and binding-controlled restrictions such
     as immutable range induction variables.
3. Type checking and inference
   - Define semantic types and produce type information for expressions,
     bindings, declarations, and callable signatures.
   - Check operators, calls, assignments and mutability, returns, blocks, loop
     results, ranges, slices, conversions, parameterized built-ins, unions,
     intersections, structs, and structural interface satisfaction.
   - Validate the required `main` signature after its parameter and return types
     are known.
   - Infer local binding types and `Error(value)` payloads within the documented
     inference boundary. Settle `bool(value)` before implementing its primitive
     conversion rules.
4. Post-type semantic analysis
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
