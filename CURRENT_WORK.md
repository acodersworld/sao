# Current work

This file is a concise implementation snapshot. It exists to avoid repeatedly
reconstructing project status from the design documents and source tree.

The design documents remain the language specification, and the source remains
the implementation. Periodically compare all three and update this file when
their status diverges.

Last reviewed: 2026-08-11

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
- Calls, member access, indexing, and postfix error propagation with `?`.
- Dedicated `int`, `float`, `bool`, `char`, and `string` conversion expressions
  with exactly one argument.
- Named struct construction and unconstrained anonymous `struct { ... }`
  expressions, including initialized fields and methods.
- Immutable and mutable bindings.
- Named and nested functions, method receivers, returns, and lambdas.
- `if`/`else if`/`else`, `loop`, `while`, and integer range `for` expressions.
- `break`, `continue`, and value-producing loop syntax.
- Exclusive and inclusive range headers using `..` and `..=`.

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

Recommended implementation order:

1. `defer` statements
   - Add the AST and parser representation.
   - Enforce the call-only syntax `defer function_call();`.

2. `co` coroutine calls
   - Add the AST and parser representation.
   - Restrict the operand to a function or method call.

3. Parameterized built-in construction
   - Support expression syntax such as `Queue<int>()` while retaining existing
     parameterized type parsing.

4. Slicing syntax
   - First settle the source syntax in the design documents.
   - The lexer and parser currently reserve `..` and `..=` for range `for`
     headers, although byte slicing semantics are mentioned in the design.

5. Program-level syntax recovery
   - After whole-program parsing exists, synchronize after malformed declarations
     so one parse can report multiple useful syntax errors.

## Work outside the parser

The following require semantic analysis or later compiler phases rather than
additional parsing:

- Name and scope resolution.
- Entry-point validation, including the required unique `main` signature.
- Type checking and inference.
- Primitive conversion validation, including defining the accepted inputs and
  result of `bool(value)`.
- Assignment-target and mutability validation.
- Validating `self`, `return`, `break`, and `continue` placement.
- Range-bound types and immutable induction bindings.
- Loop result and `else` typing.
- Lambda, nested-function, and anonymous-object capture analysis.
- Struct field validation, interface satisfaction, and contextual
  struct-to-interface conversion.
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
