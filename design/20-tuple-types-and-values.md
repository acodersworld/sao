# 20. Tuple types and values

Tuples are fixed-length, ordered structural aggregates. They provide unnamed
inline product storage without declaring a nominal struct. A tuple's type is
determined by the number, order, and exact canonical identities of its element
types:

```text
const entry: (int, float, MyType) = (1, 2.5, MyType {});
```

Tuples are compiler-native aggregate types, not applications of a reserved
parameterized built-in such as `Vector(T)`. Two tuple types with the same
ordered element types have the same canonical identity wherever they occur.
Changing the order or number of elements produces a different type.

## 20.1 Syntax

Parentheses remain grouping syntax unless they contain a top-level comma. The
unit type and value keep their existing `()` spelling:

```text
()                 // Unit value or unit type.
(value)            // Grouped expression.
(Type)             // Grouped type.
(value,)           // Singleton tuple value.
(Type,)            // Singleton tuple type.
(first, second)    // Tuple value.
(First, Second)    // Tuple type.
```

A singleton tuple requires its trailing comma. Tuples with two or more elements
may also use a trailing comma. Nested tuples follow the same rule:

```text
const nested: ((int,), (float, string)) = ((1,), (2.5, "ready"));
```

Commas belonging to a call or declaration list do not construct a tuple. A
singleton tuple passed as one argument therefore needs its own parentheses:

```text
consume((value,)); // One tuple argument.
consume(value,);   // One non-tuple argument with a trailing comma.
```

Elements are selected with zero-based numeric fields:

```text
const first = entry.0;
const payload = entry.2;
```

The field designator is a compile-time non-negative integer. Access beyond the
tuple's arity is a compile-time error. Tuples do not support dynamic bracket
indexing or slicing because heterogeneous elements do not have one result type.

## 20.2 Construction and contextual typing

Tuple elements are evaluated exactly once from left to right. Without an
expected tuple type, each element is inferred independently and the resulting
ordered types form the tuple's canonical type.

An expected tuple type must have the same arity. Each literal element is checked
directly under its corresponding expected type, including ordinary capability
reduction, union injection, interface viewing, and other conversions already
valid at that expression boundary:

```text
const result: (int | float, string) = (1, "ready");
```

This contextual behavior applies only while constructing a tuple literal. An
existing tuple value is assignable only to the same ordered element-type shape,
apart from an allowed reduction of the tuple's outer access capability. SAO
does not implicitly rebuild an existing tuple element by element:

```text
const source = (1, "ready");
const invalid: (int | float, string) = source; // Type error.
const explicit: (int | float, string) = (source.0, source.1.copy());
```

Primitive conversion ascriptions remain explicit. An expected `float` element
does not convert an `int` literal, just as an ordinary annotated binding does
not perform that conversion.

Each tuple element is an owning inline slot and follows the same transfer rules
as an inline struct field. Scalars and GC references copy according to their
existing rules, fresh object-like values move into their element slots, and a
named object-like source requires an explicit `.copy()`:

```text
const point = Point { x: 1, y: 2 };
const pair = (point.copy(), 10);
```

All tuple values are object-like aggregates, including tuples whose elements
are all trivial primitives. Binding a named tuple creates a borrow. A fresh
tuple may move into owned storage, a named tuple return is recursively copied
into caller-owned result storage, and `tuple.copy()` explicitly creates
independent inline storage while preserving shared nested GC references.

## 20.3 Access and capability

A numeric tuple field is a place. Inline element access inherits the tuple's
outer access capability, while nested GC references also retain their own
declared capability. Const access is transitively read-only and cannot be used
to recover mutable access:

```text
mut pair = (1, Point { x: 2, y: 3 });
pair.0 = 4;
pair.1.x = 5;

const view = pair;
view.0 = 6; // Type error: const tuple access.
```

Simple and compound assignment use the selected element's ordinary place and
operator rules. A replacement element must have the declared element type and
must satisfy the same owning-transfer requirements as construction.

Tuples have no user-declared fields, methods, or associated functions and do
not act as concrete structural-interface implementations. The compiler-provided
`.copy()` operation is their only named member in the initial design.

Tuple equality and ordering are not defined. SAO's primitive-only `==` and `!=`
rules remain unchanged even when every tuple element is individually
comparable.

## 20.4 Storage, escape, and type-system integration

Plain tuples use inline storage in element order. Padding, alignment, and byte
offsets are backend-private. A tuple participates in recursive copy, cleanup,
and GC-tracing metadata through every element. Inline layout validation follows
tuple elements when detecting direct or indirect recursion; recursion must
cross a GC reference or another representation boundary.

Explicit GC qualification is available as for every other value type. Prefix
`&` moves a fresh tuple into GC storage, while a named tuple requires an
explicit copy before allocation:

```text
const managed = &(1, Point { x: 2, y: 3 });
const copied_then_managed = &pair.copy();
```

A tuple may locally contain a plain interface or capturing-callable view. That
borrowed provenance propagates through the tuple, unions containing it, and
other inline aggregates. It cannot be returned, GC-allocated, queued, captured
by an escaping callable, or placed in another retaining position unless the
relevant element already uses escapable GC storage.

Tuple types may appear in transparent aliases, type-factory results, explicit
template specializations, callable signatures, union members, and legal
compiler-known built-in type arguments. Template substitution recursively
replaces element types. Tuples expose no method set for a bounded interface
parameter. Union `is` tests and narrowing use the tuple's exact canonical type.

Tracked references introduced by Phase 7.6 may appear in tuple elements under
the same rules as tracked-reference fields in other plain aggregates. The tuple
transitively carries the intersection of their source lifetimes and is rejected
from GC or external-buffer storage. Mutation, movement, returns, control-flow
joins, and GC-owner rooting preserve the tracked origins of every element.

## 20.5 Deferred tuple features

The initial tuple design does not include:

- destructuring declarations, assignments, parameters, or patterns;
- dynamic indexing, slicing, or iteration;
- spreads, concatenation, or variadic tuple types;
- named elements or tuple declarations;
- tuple equality, ordering, or formatting; or
- tuple-specific library operations.

These features require separate syntax or semantics and may be designed later
without changing the fixed-length structural identity defined here.
