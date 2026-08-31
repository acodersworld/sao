# 6. Go-like structural interfaces

Interfaces describe required behaviour through method signatures:

```text
interface Describable {
    fn describe(self) -> string;
}
```

Interface satisfaction is structural and implicit. A type satisfies an
interface when it has the required method set with compatible signatures. No
explicit `implements` declaration is required.

Every interface function requirement must declare `self`, `mut self`, `&self`,
`&mut self`, `*self`, or `*mut self` as its first parameter. The GC-qualified
receiver forms permit retention, while tracked receiver forms participate in
tracked result lifetime links. Each storage form is a distinct signature
requirement. Interfaces describe behaviour of values and cannot require or
expose receiverless associated functions. A named struct's associated
functions therefore do not participate in interface satisfaction.

When an expression is expected to have an interface or interface-intersection
type, a satisfying named or anonymous struct value is implicitly exposed
through a non-escaping view of that type. GC-qualified interface contexts require a
GC-qualified implementation. Interface names do not provide construction expressions of their
own. An interface with no method requirements is valid and is satisfied by
every struct.

The same structural rule applies to existing interface and intersection
values. A source may be viewed through a destination whose canonical method
requirements are a subset of those already guaranteed by the source. This
preserves the same borrowed object reference and concrete dispatch metadata;
it does not copy or convert the concrete object.

```text
struct User {
    name: string,
    age: int,

    fn describe(self) -> string {
        f"{self.name} is {self.age}"
    }
}

fn display(value: Describable) -> () {
    print(value.describe());
}
```

Struct fields do not participate in interface satisfaction. Interfaces are
satisfied through methods, not matching storage layout.

Nominal structs and structural interfaces deliberately coexist:

- Two structs with identical fields remain different data types.
- Both structs can independently satisfy the same interface.
- An interface accepts any present or future nominal type with the required
  behaviour.

Methods can only be declared inside the named or anonymous struct that owns
them. Method matching and variance still need formal specification. The initial
direction is exact signature matching and no method overloading.

## 6.1 Runtime method identity and interface dispatch

Every concrete struct type, including a compiler-generated anonymous type, has
one runtime vtable. Its address is the concrete runtime type identity and is
unique even when another nominal type has the same fields and methods. The
vtable also contains the type's GC tracing function, diagnostic identity, and a
single sorted array of all interface-callable methods. The type descriptor and
method table are therefore one shared object, not separate per-object pointers.

Each method has a canonical method identity derived from:

- Its name.
- Whether its receiver is const or `mut`.
- Whether its receiver storage is plain, GC-owned, or tracked.
- Its ordered parameter types and access capabilities.
- Its return type and access capability.

Parameter names, the owning concrete struct, and the interface requesting the
method are not part of this identity. Consequently, one concrete method can
satisfy the same requirement in any number of structural interfaces.

The initial whole-program compiler interns canonical method signatures and
assigns them collision-free integer IDs within the linked program. A future
separate-compilation scheme may use stable signature hashes, but a hash match
must then be verified against the canonical signature so correctness never
depends on the absence of a hash collision.

A method entry conceptually contains:

```text
+-----------+------------------+
| method ID | function pointer |
+-----------+------------------+
```

There is one such method array per concrete type, not per object and not per
interface that the type happens to satisfy. For a small method set the backend
may use a sequential search; for a larger sorted set it may use binary search.
The threshold is an implementation detail.

A plain interface value is a non-escaping object-address and dispatch-metadata
pair, allowing it to borrow either inline or GC-backed implementations without
allocation. An `&Interface` requires a GC-backed concrete value and may escape;
dispatch can obtain its method dictionary from the common object header.
Intersection types follow the same plain-versus-GC distinction.

The portable C backend must generate appropriately typed receiver-adapter
functions rather than call through an incompatible C function-pointer type.
The adapter accepts an erased object pointer, converts it to the concrete
receiver type, and calls the implementation with the method signature expected
at that call site.

Static struct-to-interface conversions are checked by the compiler. A missing
method during a statically valid interface call is therefore a compiler or
runtime invariant failure, not a recoverable program condition.

Interface metadata is not used for runtime type tests. Interface satisfaction,
requirement reduction, and concrete-to-interface view formation are all
checked statically. The `is` operator is reserved for exact member or member-
subset tests on unions; SAO does not provide runtime interface downcasts.
