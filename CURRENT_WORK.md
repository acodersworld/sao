# Current work

This file is a concise implementation snapshot. It exists to avoid repeatedly
reconstructing project status from the design documents and source tree.

The design documents remain the language specification, and the source remains
the implementation. Periodically compare all three and update this file when
their status diverges.

Last reviewed: 2026-08-10

## Parser status

The parser currently supports:

- Single-expression, single-type, and single-statement entry points.
- Primitive, named, parameterized, mutable, grouped, callable, union, and
  intersection type syntax.
- Literals, identifiers, `self`, grouping, and expression-oriented blocks.
- Prefix, binary, assignment, type-test, and postfix operators.
- Calls, member access, indexing, and postfix error propagation with `?`.
- Immutable and mutable bindings.
- Named and nested functions, method receivers, returns, and lambdas.
- `if`/`else if`/`else`, `loop`, `while`, and integer range `for` expressions.
- `break`, `continue`, and value-producing loop syntax.
- Exclusive and inclusive range headers using `..` and `..=`.

Range bounds use a deliberately restricted grammar. An unparenthesized bound may
be a primary expression, a postfix chain, or unary negation. Infix, assignment,
lambda, and block-like bounds must be parenthesized. For example:

```text
for index in -start..items.length() {}
for index in (start + offset)..(if ready { limit } else { fallback }) {}
```

## Parser work queue

Recommended implementation order:

1. Primitive conversion calls
   - Accept reserved primitive names as conversion callees, including
     `int(value)`, `float(value)`, `char(value)`, and `string(value)`.

2. Whole-program parsing
   - Add a `Program` or source-file AST node.
   - Add an entry point that accepts multiple top-level declarations instead of
     requiring EOF after one statement.

3. Struct declarations and construction
   - Parse named struct declarations, fields, and methods.
   - Parse named construction such as `Position { x: 1.0, y: 2.0 }`.
   - Parse unconstrained anonymous `struct { ... }` expressions.

4. Interface declarations and anonymous implementations
   - Parse semicolon-terminated interface method requirements.
   - Parse interface-constrained anonymous objects such as `Writer { ... }`.
   - Define how brace-based construction is disambiguated in `if`, `while`, and
     `for` heads.

5. `defer` statements
   - Add the AST and parser representation.
   - Enforce the call-only syntax `defer function_call();`.

6. `co` coroutine calls
   - Add the AST and parser representation.
   - Restrict the operand to a function or method call.

7. Parameterized built-in construction
   - Support expression syntax such as `Queue<int>()` while retaining existing
     parameterized type parsing.

8. Slicing syntax
   - First settle the source syntax in the design documents.
   - The lexer and parser currently reserve `..` and `..=` for range `for`
     headers, although byte slicing semantics are mentioned in the design.

9. Program-level syntax recovery
   - After whole-program parsing exists, synchronize after malformed declarations
     so one parse can report multiple useful syntax errors.

## Work outside the parser

The following require semantic analysis or later compiler phases rather than
additional parsing:

- Name and scope resolution.
- Type checking and inference.
- Primitive conversion validation.
- Assignment-target and mutability validation.
- Validating `self`, `return`, `break`, and `continue` placement.
- Range-bound types and immutable induction bindings.
- Loop result and `else` typing.
- Lambda, nested-function, and anonymous-object capture analysis.
- Struct field validation and interface satisfaction.
- Error propagation, coroutine, and `defer` lowering.
- Runtime representation and code generation.

## Deferred language features

Do not treat the following as immediate parser work unless the design scope
changes:

- Collection-based `for item in collection` loops.
- General `match` expressions and pattern matching.
- User-defined generics.
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
