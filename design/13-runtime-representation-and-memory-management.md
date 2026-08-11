# 13. Runtime representation and memory management

## 13.1 Initial memory-management model

SAO will initially use a simple tracing garbage collector rather than reference
counting. The intended first implementation is a single-threaded,
stop-the-world, non-moving mark-and-sweep collector.

Initial collector rules:

- Heap allocations are managed by the collector.
- Compiler-generated metadata identifies references held by heap objects.
- Compiler-generated coroutine activation frames are traced heap objects. Each
  coroutine roots the top of its linked frame chain, and the scheduler roots the
  running coroutine and every coroutine in the ready queue. Frame metadata uses
  the saved execution state to identify initialized live references. The
  collector does not conservatively scan the native C stack.
- Collection occurs only at well-defined safe points, initially allocation
  points.
- Lambda environments, anonymous-struct environments, and shared
  cells created for mutable captures are ordinary traced heap objects. Coroutine
  objects, activation frames, queues, and queued values are traced as well.
- Reference cycles are collected naturally.
- The first implementation has no user-defined destructors, finalizers, weak
  references, generational collection, or incremental collection.
- A vtable may contain a runtime-only release function for auxiliary raw
  allocations such as string and `bytes` buffers. It cannot invoke SAO code and
  is not a user-visible destructor or finalizer.
- `defer` provides deterministic resource cleanup. Garbage collection reclaims
  memory and must not be relied on to close files, release locks, or perform
  other timely cleanup.

The collector is deliberately non-moving at first so object and interface
pointers remain stable and the portable C backend stays straightforward. More
advanced collectors may be considered later without changing the language's
observable memory-safety guarantees.

## 13.2 Specialized union representation

The initial C backend uses a specialized representation for every distinct
normalized union type. It does not use a universal tagged `Value` representation.

A union is normalized by flattening nested unions and removing duplicate member
types. The backend generates one layout for the resulting member set:

```text
+------+----------------------------------+
| tag  | payload sized for largest member |
+------+----------------------------------+
```

Initial union-layout rules:

- Every materialized union has an explicit tag identifying its active member.
- The payload has the size and alignment required by the largest member.
- Primitive members such as `int` and `float` remain unboxed in the payload.
- `none` has a tag but requires no payload data.
- Converting a narrower union to a wider union remaps the tag and copies its
  active payload.
- Compiler-generated GC tracing switches on the tag and traces only the active
  member when that member contains references.
- The first implementation does not use null-pointer niches, pointer tagging,
  NaN boxing, or other compact encodings.

The IR represents union construction, projection, and conversion without
embedding this layout. A future interpreter or other backend may use a different
internal representation without changing SAO semantics.

## 13.3 Concrete value representations

Every garbage-collected allocation begins with a common object header. The
object pointer addresses the start of this header, whose first word is the
vtable pointer:

```text
+-------------------+
| vtable pointer    |
| next GC object    |
| allocation size   |
| mark state        |
+-------------------+
```

The vtable combines concrete type identity, diagnostic information, a generated
GC tracing function, an optional runtime-only auxiliary-storage release
function, and the sorted method dictionary. Runtime-only object kinds such as
closure environments, shared capture cells, coroutine activation frames, and
queues use vtables with empty method dictionaries.

`int` and `float` are unboxed 64-bit values. `bool` is an unboxed Boolean, and
`char` is an unboxed unsigned byte restricted to 0 through 127.

Both `string` and `bytes` values are stable pointers to mutable sequence objects:

```text
+-------------------+
| common GC header  |
| length            |
| capacity          |
| data pointer      |
+-------------------+
```

The data pointer refers to raw storage obtained with `malloc` and grown with
`realloc`. Resizing can change the data pointer without changing the outer
object, so every alias observes the new length and contents. When the collector
sweeps the outer object, its runtime release function frees the raw buffer
before the object itself. String storage contains ASCII bytes; `bytes` storage
contains unrestricted byte values.

Slicing a `string` or `bytes` value allocates an ordinary sequence object of the
same type and a separate raw buffer containing the selected elements. A slice
does not share its buffer with the source, including for empty and full slices.

A struct value is a stable pointer to one garbage-collected object. Declared
fields follow the common header in declaration order, with backend-required
padding. Anonymous structs place hidden captures after their declared fields.
Every instance points to its concrete type's shared vtable; no method table is
stored directly in an instance.

A callable function value is two words: a signature-specific code pointer and
an environment pointer. Capturing lambdas point to a garbage-collected
environment object. Named functions and non-capturing lambdas use a null
environment pointer and allocate no environment. The compiler traces only the
environment word; code pointers are not GC references.

An interface or intersection value is exactly the same one-word object pointer
as its underlying struct reference. Dispatch follows the object header's vtable
pointer and searches its sorted method dictionary. Anonymous interface objects
are compiler-generated structs with the same representation.

A queue object stores its length, capacity, head position, and a pointer to raw
ring-buffer storage specialized for its element type. Its generated tracing
function visits the occupied elements that contain references. Its runtime
release function frees the raw buffer when the collector sweeps the queue.
