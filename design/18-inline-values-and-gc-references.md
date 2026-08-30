# 18. Inline values and explicit GC references

## 18.1 Storage classes

An unqualified type `T` denotes a non-escapable value or borrowed view. A value
constructed in a function normally owns storage inline in that function's
activation frame. A field of type `T` owns storage inline in its containing
value. Because resumable activation frames may themselves be heap objects,
these are called *frame-owned* or *inline* values rather than native-stack
values.

`&T` denotes a stable reference to an independently traced garbage-collected
allocation. It is shareable and may escape the function that produced it.
Access capability is written inside the GC qualifier: `&T` is const access and
`&mut T` is mutable access. `mut T` remains mutable access to a plain value or
view. `&&` remains exclusively the logical-and operator and is not GC syntax.

GC qualification is available for every value type. It is independent of the
type's shape and of binding mutability. When an intersection is GC-qualified,
it is grouped: `&mut (Reader & Writer)`.

## 18.2 Construction, borrowing, and copying

Prefix `&` moves a fresh temporary into a new GC allocation:

```text
const local = Point { x: 1, y: 2 };
const shared = &Point { x: 1, y: 2 };
const returned_then_shared = &new_point();
const copied_then_shared = &local.copy();
```

Applying `&` to an already GC-qualified value is an identity operation.
Applying it directly to a named plain value is invalid: GC promotion never
changes the storage or identity of an existing value.

Binding a named, object-like plain value to another local creates a borrow.
An `&T` may likewise be viewed as plain `T` in a local or parameter context;
the view preserves whether its object is inline or garbage collected through
the object's universal runtime attributes. Scalar primitives retain trivial
copy behavior.

The reserved compiler-provided `value.copy()` operation creates an independent
value. It recursively copies inline fields and copies nested `&T` fields as
shared references. It never calls user code or recursively clones GC targets.
When invoked directly on an `&T`, it materializes a plain `T` copy of the
payload; this is how GC storage crosses an owning inline boundary. No user field
or function may be named `copy`.

An inline field owns its value. A fresh temporary can move into the field, but
a named source requires `.copy()`. Plain returns have value semantics: a fresh
temporary may be constructed or moved into caller result storage, while a named
source is copied implicitly. Recursive inline layouts are invalid; recursion
must cross a GC reference, for example `next: &Node | none`.

## 18.3 Parameters, receivers, interfaces, and callables

A parameter of plain type `T` is a non-escapable borrowed view. A parameter of
type `&T` may be retained. A plain `self` or `mut self` receiver follows the
same non-escaping rule. `&self` and `&mut self` require a GC receiver and allow
the method to retain it.

A plain interface is a borrowed pair of object address and dispatch/type
metadata. It allows inline values to satisfy interface locals and parameters
without allocation. Plain interfaces and intersections containing them cannot
be stored in fields, captured, or returned; those positions require a GC-
qualified interface.

Interface satisfaction never promotes inline storage into GC storage. A plain
struct, field, or active inline union payload may form a plain borrowed
interface view, but a GC-qualified interface requires an existing GC reference.
Creating an independent GC object from inline storage requires an explicit copy
and allocation such as `&value.copy()`.

A capturing callable is similarly plain and non-escaping by default. Storing,
capturing, or returning it requires `&fn(...)`. Named functions and
non-capturing callables have no environment and may remain plain.

Capturing a statically sized plain value copies it into the callable or
anonymous-object environment. A plain erased interface or capturing callable
cannot be copied this way and must be GC-qualified before the boundary.

## 18.4 Escape analysis and runtime obligations

After type checking and capture discovery, each function classifies expressions
as fresh temporaries, owned inline places, borrowed places, or GC references.
It records whether borrowed places refer to inline or GC-backed objects so
typed IR can emit the correct traversal operation.

A borrow escapes if it is returned as a GC reference, passed to an `&T`
parameter, stored in a retained field or queue, or retained by an escaping
callable, anonymous object, or `&self` method. Such a program is rejected; the
compiler never implicitly promotes the original storage. Where appropriate,
the diagnostic suggests copying and then allocating with `&`.

All source locals remain alive for the complete function invocation, including
across coroutine suspension. Tracked references use the `*T` type relationship
described in Chapter 21 without introducing named lifetime parameters. Frame
and object tracing metadata recursively visits inline fields containing GC
references and follows live borrowed object views. The viewed object's universal
attributes include a traversal epoch and storage kind. Every reached object is
marked as visited so cycles terminate; the storage kind determines whether it
also participates in sweeping. Frame-owned strings, collections, closures, and
other values with auxiliary raw allocations receive compiler-generated runtime
cleanup metadata. Cleanup cannot invoke SAO code.

The portable ABI uses caller-provided result storage for plain aggregate
returns. A GC allocation begins with the common object header and contains the
moved temporary payload. The collector remains non-moving, keeping borrowed
addresses stable while live views remain reachable from traced frames or
objects.
