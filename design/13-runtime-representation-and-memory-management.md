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
- Values explicitly qualified with `&`, coroutine objects, and activation
  frames are traced heap objects. Frame metadata traces GC references contained
  by inline locals and hidden roots retained for GC-derived borrows. Queues and
  queued values follow their source-level plain or `&` storage qualification.
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
- Union values have shallow-copy semantics: copying a union duplicates its tag
  and active payload, but any references in that payload continue to refer to
  the same shared storage.
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

Plain `string` and `bytes` values contain sequence metadata inline. Their
`&string` and `&bytes` forms prefix the same payload with a GC header:

```text
+-------------------+
| common GC header  |
| length            |
| capacity          |
| data pointer      |
+-------------------+
```

The data pointer refers to raw storage obtained with `malloc` and grown with
`realloc`. Resizing can change the data pointer without changing the owning
inline or GC payload, so every borrow observes the new length and contents. A
frame cleanup function frees a plain value's buffer; the runtime release
function does the same when the collector sweeps a GC allocation. String
storage contains ASCII bytes; `bytes` storage contains unrestricted byte values.

Slicing a `string` or `bytes` value produces a fresh plain sequence value with a
separate raw buffer. A slice does not share its buffer with the source,
including for empty and full slices; prefix `&` may move that result into GC
storage.

A plain struct value is stored inline in its frame, containing aggregate, or
caller-provided result slot. An `&Struct` is a stable pointer to a GC allocation
whose common header is followed by those same inline fields in declaration
order, with backend-required padding. Anonymous structs place copied hidden
captures after their declared fields. Plain values do not carry a GC header.

A plain callable is a non-escaping code-and-environment view. Its environment is
owned inline by the creating frame. `&fn(...)` places the callable environment
in GC storage and may escape. Named functions and non-capturing lambdas use a
null environment pointer and allocate no environment. Code pointers are never
GC references.

A plain interface or intersection is a non-escaping pair containing a borrowed
object address and dispatch/type metadata, so it can view inline storage.
`&Interface` uses a GC-backed concrete object whose header supplies its vtable;
it may use a compact one-pointer representation. Anonymous interface objects
are compiler-generated structs following the same qualification rules.

A queue object stores its length, capacity, head position, and a pointer to raw
ring-buffer storage specialized for its element type. Its generated tracing
function visits the occupied elements that contain references. Its runtime
release function frees the raw buffer when the collector sweeps the queue.
