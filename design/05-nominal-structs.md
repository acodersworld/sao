# 5. Nominal structs

Structs are nominal. Their identity comes from their declaration, not merely
from their fields.

All named and anonymous structs have reference semantics. Constructing a struct
allocates a garbage-collected object and initially produces mut access to it.
Assignment, parameter passing, returning, and capture copy the reference rather
than the object's fields, subject to the `mut`-to-`const` capability rules. SAO
performs no implicit deep copies and has no ownership, borrowing, or move
semantics. Every mutable alias observes and may change the same object.

```text
struct Position {
    x: float,
    y: float,

    fn magnitude(self) -> float {
        sqrt(self.x * self.x + self.y * self.y)
    }
}

struct Velocity {
    x: float,
    y: float,
}
```

Although `Position` and `Velocity` have identical fields, they are different
types:

```text
fn teleport(destination: Position) -> () {
    // ...
}

const velocity = Velocity { x: 1.0, y: 2.0 };
teleport(velocity); // Type error.
```

Conversions between nominal structs are explicit.

Struct construction is written:

```text
const position = Position {
    x: 10.0,
    y: 20.0,
};
```

Construction uses named fields. Every declared field must be initialized exactly
once; missing, duplicate, and unknown fields are compile-time errors. Field
initializers may appear in any order and are evaluated from top to bottom in the
order written in the construction expression, not in declaration order:

```text
const position = Position {
    y: calculate_y(), // Evaluated first.
    x: calculate_x(), // Evaluated second.
};
```

Field labels do not introduce local bindings, so one initializer cannot refer to
an earlier initializer merely by using its field name. Each initializer must be
assignable to the declared field type and must obey the reference-capability
rules.

Named struct declarations may refer to themselves and to structs declared later
in the same program. Recursive and mutually recursive struct types have finite
layouts because struct values are references:

```text
struct Node {
    value: int,
    next: Node | none,
}
```

The binding receiving a constructed value does not enter scope until its entire
initializer completes. A construction expression has no implicit `self`, and a
partially initialized object cannot be referenced or passed to SAO code. Cyclic
object graphs are instead created in multiple steps through mutable references:

```text
mut node = Node {
    value: 1,
    next: none,
};

node.next = node;
```

The implementation may allocate and root the object before evaluating its field
initializers, but that partially initialized storage is not observable by the
source program. A field initializer that exits through `?` leaves the incomplete
allocation unreachable and eligible for garbage collection.

Functions are declared directly in the struct body alongside its fields. SAO
has no separate `impl` block and does not attach functions to a struct after its
definition. This keeps a type's complete function set local to its declaration.

A struct function whose first parameter is `self` or `mut self` is an instance
method. A receiverless struct function is instead associated with the named
type. When present, the receiver must occur exactly once and must be the first
parameter:

```text
struct Position {
    x: float,
    y: float,

    fn origin() -> Position {
        Position { x: 0.0, y: 0.0 }
    }

    fn magnitude(self) -> float {
        sqrt(self.x * self.x + self.y * self.y)
    }
}

const origin = Position::origin();
const distance = origin.magnitude();
```

`Type::function` selects an associated function from a type and produces an
ordinary function value, so it may be stored or called. `value.method` selects
an instance method from a value. An associated function has no implicit object,
cannot use `self`, and cannot be called through a value. Conversely, an
instance method cannot be selected through its type as an unbound function.

Associated functions are initially available only on named structs. An
anonymous struct's generated type cannot be named, and each of its functions
must receive `self`. Associated functions do not participate in structural
interface satisfaction. All fields and functions declared by one struct share
one member-name namespace, so an associated function and an instance method
cannot reuse the same name.

Anonymous structs use the same model. A `struct { ... }` expression declares a
hidden nominal type and constructs a value of that type:

```text
const position = struct {
    x: float = 10.0;
    y: float = 20.0;

    fn magnitude(self) -> float {
        sqrt(self.x * self.x + self.y * self.y)
    }
};
```

Anonymous-struct field initializers follow the same top-to-bottom source order
and partial-initialization rules as named struct construction.

The source program cannot name the anonymous struct's generated type, but local
inference may retain that one exact hidden type. It may be passed to any
structural interface that its methods satisfy.

Fields do not have individual `const` or `mut` qualifiers. A field is writable
when reached through a `mut` reference and transitively read-only when reached
through a `const` reference. Reference-valued field initialization and
assignment must not convert const access back to mut; a const reference cannot
be placed where later mutable field access would recover mut capability.

An instance-method receiver `self` is a const reference to the original object
by default. A method requiring mutable access declares `mut self`:

```text
fn describe(self) -> string {
    self.name
}

fn rename(mut self, name: string) -> () {
    self.name = name;
}
```

A const method may be called through either capability. A `mut self` method may
only be called through a mut reference. Receiver capability is part of a method
signature and participates in interface matching.
