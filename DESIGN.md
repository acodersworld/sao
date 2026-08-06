# SAO Language Design

Status: early design notes  
Working name: SAO  
Last updated: 2026-08-06

This document records the current direction for SAO, a statically typed,
expression-oriented toy language. It distinguishes agreed design decisions from
provisional syntax and ideas intentionally deferred until later.

## 1. Design goals

SAO should be:

- Statically typed, with useful local type inference.
- Explicit at function boundaries.
- Expression-oriented, especially around blocks and control flow.
- Flexible enough to express union and intersection types.
- Nominal for data definitions and structural for behavioural interfaces.
- Equipped with first-class anonymous functions and capturing anonymous structs.
- Pleasant for small programs and experimentation.
- Implementable through a backend-neutral IR.
- Initially compilable through portable C using an external compiler driver such
  as Clang or GCC.

The first implementation should favour clear semantics and good diagnostics over
advanced optimization.

SAO functions execute synchronously from call to return. The language does not
provide coroutines, generators, async functions, `co`, or `yield`, and the IR and
runtime do not need to preserve suspended function activations.

An initial SAO program consists of one source file. All named declarations share
that file's program namespace. The language does not initially provide modules,
imports, access-control modifiers, external packages, or separate compilation.

## 2. Compilation model

The intended compiler pipeline is:

```text
source
  -> lexer
  -> parser
  -> AST
  -> name resolution and type checking
  -> typed high-level IR
  -> lowered middle IR
  -> C backend
  -> clang/gcc
  -> native executable
```

The IR should belong to SAO rather than mirror C. This leaves room for an
interpreter or a Cranelift JIT/AOT backend in the future:

```text
                       +-> IR interpreter
source -> typed IR ----+-> C backend -> clang/gcc -> executable
                       +-> Cranelift backend (future)
```

The C compiler driver is responsible for assembling and linking. SAO does not
initially need its own linker.

The initial compiler exposes one build command:

```text
sao build --cc clang program.sao
```

`--cc` is required and names the C compiler executable to invoke. The compiler
is selected explicitly; SAO does not consult the `CC` environment variable or
automatically search for Clang or GCC. Another compatible compiler may be used
by passing its executable name or path to `--cc`.

The SAO compiler is implemented in Rust.

## 3. Static typing and inference

SAO is statically typed. Programs are rejected when their operations cannot be
shown to be type-safe.

Local binding types may be inferred:

```text
const count = 10;
const ratio = 0.5;
```

Types may also be written on the declaration:

```text
const count: int = 10;
const ratio: float = 0.5;
```

Binding mutability is explicit:

```text
mut position: int = 0;
position += 1;

const origin: int = 0;
origin = 1; // Type error: a const binding cannot be reassigned.
```

`const` and `mut` declare local bindings; SAO has no `let` keyword and no
unqualified local declaration. A `const` binding cannot be reassigned or used
to mutate a referenced object. A `mut` binding can be reassigned and permits
mutation through its reference.

For reference types, `const` is a transitive read-only access capability rather
than a deep-immutability guarantee:

```text
mut user = User { name: "Ben" };
const view = user;       // Allowed: mut access may be reduced to const.

user.name = "Benjamin"; // Allowed through the mut reference.
view.name = "Robert";   // Type error: view is const.
view = another_user;    // Type error: the binding is const.
```

Access through a `const` reference remains const through fields and other
references reached from it. A `const` reference cannot be assigned or passed to
a location requiring `mut`. Multiple aliases are allowed, so the object observed
through `view` may still change through `user` or another `mut` alias. SAO does
not enforce uniqueness, ownership, borrowing, or lifetimes.

These capability restrictions apply to references. Independently copied value
types may use a different binding qualifier because changing the copy cannot
affect the source:

```text
const original = 10;
mut copy = original; // Allowed: int is copied by value.
copy += 1;
```

Function parameters and implicit bindings such as iterator variables are const
by default and omit the `const` keyword. A parameter that requires mutable
access is marked `mut`:

```text
fn display(user: User) -> () {
    print(user.name);
}

fn rename(mut user: User, name: string) -> () {
    user.name = name;
}
```

A `mut` argument may be passed to either parameter form. A `const` argument may
only be passed to the default const form.

Reference return types are also const by default. Returning mutable access is
explicit with `mut`:

```text
fn current_user() -> User {
    // Returns const access.
}

fn create_user(name: string) -> mut User {
    User { name: name }
}
```

A function declared to return a const reference may return either const or mut
access, reducing mut to const when necessary. A function declared `-> mut T`
must return mut access. The qualifier is part of function and method signatures,
including anonymous-function types and interface requirements. A return
capability qualifier is unnecessary for copied value types such as `int`.
Within a union, `mut` qualifies the immediately following reference member, so
`-> mut User | none` returns either mutable access to a `User` or `none`.

### 3.1 Primitive types

SAO has a deliberately small, fixed primitive set:

- `int` is exactly a signed 64-bit integer. It is not platform-dependent, and
  there are initially no other integer widths or unsigned integer types.
- `float` is exactly an IEEE 754 binary64 floating-point value. There are
  initially no other floating-point widths.
- `bool` has the values `true` and `false`. Conditions require `bool`; SAO has
  no implicit truthiness conversions.
- `char` is a one-byte ASCII value in the range 0 through 127.
- `string` is a garbage-collected mutable sequence of one-byte ASCII
  characters. It is a reference type despite being part of the primitive set.
- `()` is the unit type and has one value, also written `()`.
- `none` is the singleton absence type and value described with unions in
  Section 8.

The names `int` and `float` expose their fixed meanings directly; `i64` and
`f64` are not separate source-language type names. Integer literals have type
`int`, and floating-point literals have type `float`. An integer literal outside
the `int` range is a compile-time error.

Binary data uses the separate built-in `bytes` sequence type described below;
it is not another scalar primitive.

### 3.2 Integer arithmetic

Integer arithmetic is trapping. Overflow detected at compile time is a
diagnostic; overflow at runtime causes an immediate panic in every build mode.
There are no initial checked, wrapping, overflowing, or saturating arithmetic
APIs.

Division or remainder by zero, dividing or taking the remainder of the minimum
`int` by `-1`, and a shift count outside the range `0` through `63` also panic.
Floating-point overflow follows IEEE 754 and may produce infinity rather than
panicking.

### 3.3 Operators

SAO has a small fixed operator set. Operators cannot be declared or overloaded
by structs or interfaces, and operands are never implicitly converted.

| Operators | Operand types | Result type |
| --- | --- | --- |
| `+` | `int`, `int` | `int` |
| `+` | `float`, `float` | `float` |
| `+` | `string`, `string` | `string` |
| `-`, `*`, `/` | `int`, `int` | `int` |
| `-`, `*`, `/` | `float`, `float` | `float` |
| `%` | `int`, `int` | `int` |
| unary `-` | `int` or `float` | the operand type |
| `<`, `<=`, `>`, `>=` | two `int`, two `float`, or two `char` values | `bool` |
| `!` | `bool` | `bool` |
| `&&`, `\|\|` | `bool`, `bool` | `bool` |
| `&`, `\|`, `^`, `<<`, `>>` | `int`, `int` | `int` |
| `~` | `int` | `int` |

`==` and `!=` are available only when both operands have the same primitive
type, as defined in Section 3.7. They do not perform numeric coercion or invoke
user-defined operations. Strings support `+`, `==`, and `!=`, but not ordering.
`bytes` values support neither equality nor `+`; binary concatenation uses the
explicit built-in `bytes.concat(left, right)` operation, which creates a new
mutable byte sequence.

All operator operands are evaluated from left to right. `&&` and `||`
short-circuit and evaluate their right operand only when required. Chained
comparisons are not a special form, so `a < b < c` is rejected because the
first comparison produces `bool`.

Integer division truncates toward zero. Integer remainder is defined by
`left == (left / right) * right + (left % right)` and has the dividend's sign
when nonzero. The trapping cases in Section 3.2 apply before the backend performs
an operation that could invoke C undefined behaviour.

Bitwise integer operations use the two's-complement 64-bit representation of
`int`. Left shift inserts zero bits and discards bits shifted past the high end;
it does not perform an arithmetic-overflow check. Right shift is arithmetic and
replicates the sign bit. A shift count outside `0` through `63` panics.

SAO has no unary `+`, `++`, or `--`, and initially has no floating-point
remainder operator.

Assignment and compound assignment produce `()`. Compound forms `+=`, `-=`,
`*=`, `/=`, and `%=` use the corresponding operator rules and require a mutable
destination. The bitwise compound forms `&=`, `|=`, `^=`, `<<=`, and `>>=` are
also available for `int`. A compound-assignment destination is evaluated once,
before its right operand. `+=` through mut access to a string appends the right
string to the existing object; all aliases observe the mutation.

Examples of rejected implicit conversions include:

```text
1 + 2.0
1 == 1.0
'a' + 1
"count: " + 10
true + true
```

### 3.4 Explicit conversions

SAO performs no implicit numeric conversions. `int` and `float` remain distinct
through assignment, argument passing, returns, arithmetic, comparisons, and
contextual typing:

```text
const count: int = 10;
const ratio: float = 0.5;

const invalid = count + ratio;          // Type error.
const valid = float(count) + ratio;     // Explicit conversion.
const integer: int = int(ratio);        // Explicit conversion.
```

Literals also retain their natural types rather than being coerced by an
annotation:

```text
const invalid: float = 1;   // Type error: 1 has type int.
const valid: float = 1.0;
```

The initial conversion syntax treats a target primitive type as a conversion
function, such as `float(value)`, `int(value)`, `char(value)`, or
`string(value)`. Text parsing is not a numeric conversion; parsing functions
return an explicit error union for invalid input.

`int(value)` converts a finite, in-range `float` by truncating toward zero:

```text
int(3.9)   // 3
int(-3.9)  // -3
```

Negative floating-point zero converts to integer zero. Conversion from NaN,
positive or negative infinity, or a value outside the `int` range panics. An
invalid constant conversion is a compile-time diagnostic when detectable.

`float(value)` converts an `int` to the closest representable binary64 value,
using round-to-nearest with ties to even when precision is lost. `char(value)`
accepts only an integer from 0 through 127 and panics otherwise.

### 3.5 Floating-point behavior

Float arithmetic follows strict IEEE 754 binary64 semantics:

- Overflow produces positive or negative infinity.
- Underflow produces a subnormal value or signed zero.
- Floating-point division by zero produces infinity or NaN rather than
  panicking.
- NaN compares unequal to every value, including itself.
- Ordered comparisons involving NaN are false.
- Positive and negative zero compare equal.
- Ordinary operations use round-to-nearest with ties to even.
- The backend must not enable unsafe fast-math transformations or silently use
  extra intermediate precision.

### 3.6 ASCII strings and byte sequences

SAO deliberately does not provide Unicode text semantics. A `char` occupies one
byte but permits only ASCII values 0 through 127. A `string` stores an explicit
length and a contiguous sequence of `char` values. Embedded zero characters are
valid because strings are not sentinel-terminated.

String rules:

- String and character literals must contain only ASCII; other characters are
  compile-time diagnostics.
- Evaluating a string literal creates a fresh mutable string object. Binding it
  with `const` reduces that access to a read-only reference; literals are not
  shared mutable singleton objects.
- `string[index]` performs constant-time character indexing and returns `char`.
- Indexed assignment through mut access accepts a `char`.
- Out-of-range indexing or indexed assignment panics.
- String length and encoded byte length are identical and available in constant
  time.
- A const string reference is transitively read-only. A mut string reference
  supports indexed mutation, append, extend, and resize.
- Assignment, parameter passing, returning, and capture copy the string object
  reference. Mutations are visible through every alias to that object.
- Concatenation with `+` creates a new mutable string and does not modify either
  operand.

The built-in `bytes` type is a garbage-collected sequence of arbitrary byte
values from 0 through 255. It is separate from `string`:

- `bytes[index]` returns an `int` in the range 0 through 255.
- Indexed assignment accepts only an `int` in that range and panics otherwise.
- Out-of-range indexing panics.
- A `const bytes` reference is transitively read-only.
- A `mut bytes` reference supports indexed mutation, append, extend, and resize.
- `length()` returns the byte count in constant time.
- Slicing initially creates an independent copy.
- `bytes.concat(left, right)` creates a new mutable byte sequence containing the
  contents of `left` followed by `right`.
- Files and other binary I/O use `bytes`.

ASCII conversion is explicit. Encoding always succeeds; decoding fails when a
byte exceeds 127:

```text
ascii.encode(text) -> mut bytes
ascii.decode(data) -> string | Error<string>
```

Current inference boundary:

- Local binding types may be inferred from their initializer and context.
- Inference must resolve to one unambiguous type.
- Inference never synthesizes a union merely because different paths produce
  different types.
- Named function parameter types are explicit.
- Every named function has an explicit return type.
- Function return types are never inferred.
- Anonymous function parameter and return types are explicit in the function
  expression; contextual lambda signature inference is deferred.
- User-defined generic declarations and general-purpose generic inference are
  not part of the initial language. Built-in parameterized types such as
  `Error<T>` have their own limited inference rules.

For example:

```text
fn add(left: int, right: int) -> int {
    left + right
}

fn print_user(user: User) -> () {
    print(user.name);
}
```

The unit type and its sole value are both spelled `()`.

### 3.7 Equality

Equality is deliberately limited to primitive values. The operands of `==` and
`!=` must have the same primitive base type; differing const and mut access to a
string does not prevent comparison. There is no implicit conversion or
user-defined equality.

- `int`, `bool`, and `char` compare their values.
- `float` uses IEEE equality: NaN is unequal to every value, including itself,
  and positive and negative zero compare equal.
- `string` compares character contents, not its runtime representation or
  storage address.
- The sole values of `()` and `none` compare equal to values of their respective
  types.
- `!=` is the logical negation of `==`.

Equality is not defined for `bytes`, named or anonymous structs, interfaces,
callable values, unions, intersections, or `Error<T>`. Attempting to compare
such values is a compile-time error. A union must first be narrowed to a
primitive member before that member can be compared; a union does not become
comparable merely because all of its members are primitive.

SAO has no general object-identity or pointer-equality operator. Runtime object
pointers used to implement garbage-collected values and interfaces are not
observable through `==` or `!=`.

## 4. Expression-oriented blocks

Blocks are expressions. The final expression without a semicolon is the value of
the block:

```text
const distance = {
    const x = 10.0;
    const y = 20.0;

    sqrt(x * x + y * y)
};
```

A semicolon discards the expression's value:

```text
{ 42 }     // type: int
{ 42; }    // type: ()
```

Function bodies use the same rule. The final expression must be compatible with
the explicitly declared return type:

```text
fn absolute(value: int) -> int {
    if value < 0 {
        -value
    } else {
        value
    }
}
```

Early `return` remains available:

```text
fn divide(left: float, right: float) -> float {
    if right == 0.0 {
        return 0.0;
    }

    left / right
}
```

`if` is an expression. When no expected type is provided, its branches must
resolve to the same single type:

```text
const value = if condition {
    42
} else {
    3.14
};

// Type error: inference cannot choose one type.
```

Different branch types require an explicit type that accepts them:

```text
const value: int | float = if condition {
    42
} else {
    3.14
};
```

The annotation supplies the expected union type; the compiler does not invent
the union. The same rule applies to other expression forms with multiple result
paths.

## 5. Nominal structs

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

Methods are declared directly in the struct body alongside its fields. SAO has
no separate `impl` block and does not attach methods to a struct after its
definition. This keeps a type's complete method set local to its declaration.

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

The method receiver `self` is a const reference to the original object by
default. A method requiring mutable access declares `mut self`:

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

## 6. Go-like structural interfaces

Interfaces describe required behaviour through method signatures:

```text
interface Describable {
    fn describe(self) -> string;
}
```

Interface satisfaction is structural and implicit. A type satisfies an
interface when it has the required method set with compatible signatures. No
explicit `implements` declaration is required.

```text
struct User {
    name: string,
    age: int,

    fn describe(self) -> string {
        self.name + " is " + string(self.age)
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

### 6.1 Runtime method identity and interface dispatch

Every concrete struct type, including a compiler-generated anonymous type, has
one runtime type descriptor. Its identity is unique even when another nominal
type has the same fields and methods. The descriptor contains the type's GC
tracing information, diagnostic identity, and a single sorted array of all
interface-callable methods.

Each method has a canonical method identity derived from:

- Its name.
- Whether its receiver is const or `mut`.
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

Because every initial interface implementation is a garbage-collected struct,
an interface value uses the same object pointer as the concrete struct
reference. Dispatch loads the object's type descriptor, looks up the canonical
method ID, and indirectly calls the resulting function. Converting a concrete
reference to an interface therefore creates no wrapper and no interface-specific
method table. Intersection types use the same representation and lookup path.

The portable C backend must generate appropriately typed receiver-adapter
functions rather than call through an incompatible C function-pointer type.
The adapter accepts an erased object pointer, converts it to the concrete
receiver type, and calls the implementation with the method signature expected
at that call site.

Static struct-to-interface conversions are checked by the compiler. A missing
method during a statically valid interface call is therefore a compiler or
runtime invariant failure, not a recoverable program condition.

Runtime type tests use the same metadata:

- `value is NamedStruct` compares the concrete type descriptor with the named
  struct's descriptor and narrows to that exact nominal type on success.
- `value is Interface` checks that the concrete method array contains every
  required canonical method ID. On success, the value is narrowed to the
  intersection of its existing interface type and the tested interface.

Narrowing preserves the source access capability. Testing or downcasting a
const interface reference can never recover `mut` access. Anonymous concrete
types have descriptors and can be tested against additional interfaces, but
cannot be named for an exact concrete-type test in source code.

## 7. Anonymous structs, functions, and interface objects

### 7.1 Interface-constrained anonymous structs

An interface can be used to construct an anonymous implementation:

```text
interface Greeter {
    fn greet(self, name: string) -> string;
}

const greeter = Greeter {
    prefix: string = "Hello";

    fn greet(self, name: string) -> string {
        self.prefix + ", " + name
    }
};
```

This is the interface-constrained form of an anonymous struct expression. It
does not instantiate the interface itself. The compiler creates a hidden nominal
struct containing the declared fields and methods, verifies that it satisfies
the interface, and converts it to an interface value.

Anonymous interface object rules:

- `Interface { ... }` constructs a hidden implementation.
- General `struct { ... }` expressions construct unconstrained anonymous
  structs.
- Field initializers at object scope define hidden fields and are written
  `name: Type = expression;`. The type annotation may be omitted when it can be
  inferred unambiguously.
- Hidden field types may be inferred or explicitly annotated.
- All required interface methods must be present.
- Method return types remain explicit.
- Hidden fields are not accessible through the interface value.
- Extra implementation methods are not visible through the interface value.
- The source program cannot name the compiler-generated backing type.

### 7.2 Capture semantics

Anonymous structs and anonymous functions automatically capture referenced
bindings from their surrounding lexical scope. Captures do not need to be
redeclared as fields:

```text
interface IntPredicate {
    fn test(self, value: int) -> bool;
}

fn greater_than(limit: int) -> IntPredicate {
    IntPredicate {
        fn test(self, value: int) -> bool {
            value > limit
        }
    }
}
```

Here `limit` is a hidden capture, not a public field. Every method in the
anonymous struct shares the same capture environment.

Capture rules:

- Captures are discovered automatically from free-variable references.
- Capture lists are always implicit; SAO has no explicit capture-list syntax.
- A `const` binding is captured as the value it holds when the anonymous value
  is created. Value types are copied directly; reference values copy the
  reference and preserve its access capability.
- A captured `mut` binding is lifted into a shared garbage-collected cell.
  Mutations are visible to the outer scope and to every anonymous value that
  captures it.
- A captured binding remains alive for as long as any capturing value can use
  it, even after its original lexical scope has returned.
- Captures are hidden storage and do not become fields accessible through an
  interface.
- Named structs and named functions do not capture lexical state.
- Parameters and locals inside a method or anonymous function shadow captures
  with the same name.

Explicit fields and captures are distinct. A field initializer at
anonymous-struct scope creates owned storage accessed through `self`; a bare
reference to an outer binding is a capture:

```text
const prefix = "log: ";

const formatter = struct {
    suffix = "\n";

    fn format(self, message: string) -> string {
        prefix + message + self.suffix
    }
};
```

Mutable captured bindings are always represented by shared garbage-collected
heap cells. The original scope and every capturing anonymous value access the
same cell.

### 7.3 Anonymous functions

Anonymous functions are expressions written with `fn` and an explicit
signature:

```text
const factor = 1.5;

const scale = fn(value: float) -> float {
    value * factor
};
```

The inferred callable type of `scale` is the single type
`fn(float) -> float`. A function value contains both callable code and any
captured environment.

Mutable captures are shared:

```text
mut count = 0;

const next = fn() -> int {
    count += 1;
    count
};

next(); // 1
next(); // 2
// count is now 2
```

If several anonymous functions capture the same mutable binding, they observe
the same storage. SAO has no ownership-transfer or `move` capture modifier.

A `const` binding containing an anonymous function does not make that function
pure. Calling it may still mutate a `mut` binding captured by the function.

### 7.4 Closure and environment representation

Every anonymous-function expression has a compiler-generated environment type.
An anonymous function value has a uniform two-word representation:

```text
+--------------+---------------------+
| code pointer | environment pointer |
+--------------+---------------------+
```

The code pointer uses the function value's statically known signature and
accepts the environment pointer as a hidden first argument. The environment is
a non-moving garbage-collected object specialized for that expression:

```text
+---------------------------+
| GC object header          |
+---------------------------+
| directly stored captures  |
| shared-cell pointers      |
| ...                       |
+---------------------------+
```

The environment stores `const` captures directly and stores a pointer to the
shared cell for each `mut` capture. Compiler-generated tracing metadata records
which slots contain references. Environment field order, padding, and byte
offsets are backend-private details. A non-capturing anonymous function retains
the same two-word callable representation but uses an empty environment.

Anonymous structs do not need a separate environment allocation. Their
compiler-generated garbage-collected object contains declared fields and hidden
captures together:

```text
+------------------+
| GC object header |
| declared fields  |
| hidden captures  |
+------------------+
```

Every method receives that object as `self`, so all methods share the same
captures. Interface-constrained anonymous structs use this same representation
behind their interface value.

SAO execution is single-threaded. Closure environments, shared capture cells,
and collector state use no atomic operations, locks, or thread-safety marker
types. Future shared-memory concurrency would require an explicit new design;
these values are not implicitly safe to share with concurrently executing SAO
threads.

## 8. Union and intersection types

SAO uses `|` for union types:

```text
fn convert(input: int | float | MyStruct)
    -> MyOtherStruct | DifferentStruct
{
    // ...
}
```

A value of `A | B` is accepted when it is an `A`, a `B`, or both where the
types overlap. Operations require narrowing unless they are valid for every
member of the union.

SAO uses the built-in singleton `none` to represent the absence of a value.
`none` is both the spelling of the singleton type in a type expression and its
only value. Types are not implicitly optional: absence must be included
explicitly with a union.

```text
const result: int | none = none;
```

An optional value is therefore an ordinary union such as `int | none`; SAO does
not require a separate built-in `Option<T>` type. Operations on the non-`none`
member require the same narrowing as other union types.

SAO uses `&` for intersection types:

```text
fn copy(mut stream: Reader & Writer) -> () {
    const data = stream.read(4096);
    stream.write(data);
}
```

A value of `Reader & Writer` must satisfy both interfaces. Intersections can also
be used for anonymous implementations:

```text
mut stream = Reader & Writer {
    fn read(mut self, count: int) -> bytes {
        // ...
    }

    fn write(mut self, data: bytes) -> int {
        // ...
    }
};
```

Named interface composition may eventually use embedded interfaces:

```text
interface ReadWriter {
    Reader;
    Writer;
}
```

Runtime interface narrowing, such as narrowing a `Reader` to
`Reader & Writer` after `value is Writer`, uses the runtime method metadata
described in Section 6.1.

### 8.1 Flow-sensitive type narrowing

The initial language uses `is` tests and ordinary `if` expressions rather than
a general pattern-matching construct. A successful test narrows the tested
binding within the true branch:

```text
fn display(value: int | float | none) -> () {
    if value is int {
        // value has type int here.
        print(string(value));
    } else if value is float {
        // value has type float here.
        print(string(value));
    } else {
        // value has type none here.
        print("none");
    }
}
```

When a test selects a normalized union member, the false branch removes that
member from the binding's type. This subtraction continues through an `else if`
chain, so the final `else` contains the remaining members. `none` is tested in
the same way as any other union member:

```text
if result is none {
    // result is none.
} else {
    // none has been removed from result's type.
}
```

A union-member test inspects the union's active tag. A nominal type test on an
interface value compares its concrete runtime type descriptor. An interface
test on another interface value consults the concrete type's method metadata and
narrows the true branch to an intersection. A failed runtime interface test
does not create a negative interface type, so its false branch retains the
original interface type.

Narrowing applies to the existing binding, does not evaluate its initializer
again, and preserves its access capability. It can never recover `mut` access
from a const reference.

General `match` expressions, destructuring patterns, literal patterns, guards,
and nested patterns are not part of the initial language. Exhaustive union
dispatch is expressed with `if`/`else if`/`else`; a union-only exhaustive
`match` may be added later as ergonomic syntax over the same tag tests and
projections.

### 8.2 Recoverable errors and propagation

Recoverable errors are ordinary union values. SAO provides the built-in nominal
parameterized type `Error<T>`, whose value carries error information of type
`T`:

```text
fn myfunc() -> int | Error<string> {
    if operation_failed() {
        Error("operation failed")
    } else {
        42
    }
}
```

SAO does not initially support user-defined generic structs, functions, or
interfaces. `Error<T>` is a compiler-known type constructor with dedicated
type-checking and lowering rules, not an instance of a general source-language
generic facility. Other built-in parameterized types may be added later without
enabling user-defined generics.

Each `Error<T>` instantiation is distinct from its payload type, from every
non-error member of a union, and from `Error<U>` when `T` and `U` differ.
`Error(value)` is the initial construction syntax. Without an expected type,
the payload type is the exact type of `value`:

```text
const error = Error("operation failed"); // Error<string>
```

An expected `Error<T>` type may instead guide construction when the value is
assignable to `T`. The compiler does not invent a payload union without such an
expected type.

The payload held by an `Error<T>` is immutable. If the payload is a reference,
accessing it cannot grant more than const access. This permits the built-in
widening conversion `Error<A>` to `Error<B>` whenever `A` is assignable to `B`.
This covariance is a specific rule for `Error`, not a general variance feature:

```text
const specific: Error<IoError> = Error(IoError { /* ... */ });
const combined: Error<IoError | ParseError> = specific;
```

The payload is available through the built-in const field `value`. Error unions
can therefore be handled with ordinary narrowing:

```text
const result = myfunc();

if result is Error<string> {
    print(result.value);
} else {
    print(string(result));
}
```

The postfix `?` operator propagates an error without exceptions:

```text
fn caller() -> int | Error<string> {
    const value = myfunc()?;
    value + 1
}
```

For an operand of type `S | Error<E>`, `?` evaluates the operand once. If it is
an `S`, the expression produces that value with type `S`. If it is an
`Error<E>`, the current function returns that error immediately. The enclosing
function's declared return type must accept the propagated `Error<E>`, including
through the built-in payload-widening conversion. `S` may itself be a union of
several non-error types.

Error payloads can use ordinary SAO types and unions:

```text
fn load() -> Config | Error<ParseError | IoError> {
    const text = read_file("config.sao")?;
    parse(text)
}
```

SAO has no exceptions, `throw`, `catch`, or exception unwinding. All recoverable
failure paths are explicit in function return types. A future type-alias feature
may provide shorter names for commonly used success/error unions, but no special
`Result` container is required.

`panic(message)` is reserved for unrecoverable failures. It prints a helpful
SAO-level stack trace and terminates the process with a nonzero status. A panic
cannot be caught or recovered from and does not unwind the stack. Consequently,
deferred actions do not run during a panic.

## 9. Loop expressions

Value-producing loops are a core feature. SAO supports four loop forms:

```text
loop {
    // Infinite loop.
}

while condition {
    // Conditional loop.
}

for item in items {
    // Iterator loop.
}

for mut index = 0; index < 10; index += 1 {
    // Traditional three-clause loop.
}
```

Every loop form is an expression and may produce a value with `break value`.

### 9.1 Infinite loops

An infinite loop can only complete by breaking or transferring control elsewhere:

```text
const command = loop {
    const input = read_line();

    if input != "" {
        break input;
    }
};
```

All reachable `break value` expressions associated with the loop must have
compatible types.

### 9.2 Naturally terminating loops

`while`, iterator `for`, and three-clause `for` loops can terminate without
executing `break`. When such a loop is used to produce a non-unit value, it must
have an `else` block that supplies the natural-completion value:

```text
const admin: User | none = for user in users {
    if user.is_admin {
        break user;
    }
} else {
    none
};
```

The `else` block executes only when the loop completes naturally, not after a
`break`.

Traditional loops follow the same rule:

```text
const divisor = for mut candidate = 2;
                  candidate < value;
                  candidate += 1
{
    if value % candidate == 0 {
        break candidate;
    }
} else {
    value
};
```

### 9.3 Loop result typing

- `break expression` contributes the expression's type to the loop result.
- Bare `break;` contributes `()`.
- The `else` block contributes its final expression's type.
- Without an expected type, all contributed values must resolve to the same
  single type.
- The compiler never synthesizes a union from differing `break` and `else`
  values.
- An explicit variable annotation or enclosing function return type may supply
  an expected union type, and every contributed value must be assignable to it.
- A naturally terminating loop without `else` has type `()`.
- A naturally terminating loop used where a non-unit value is expected requires
  `else`.
- `continue` does not contribute a result value.

SAO has no loop labels. `break` and `continue` always target the innermost
enclosing loop.

## 10. Lexical `defer`

SAO has call-only `defer` syntax with lexical block scope:

```text
fn read_file(path: string) -> string {
    mut file = File.open(path);
    defer file.close();

    file.read_all()
}
```

Deferred actions execute:

- In reverse registration order.
- When their lexical block completes normally.
- Before a `return` exits their scope.
- Before `break` or `continue` exits their scope.
- Before `?` propagation exits their scope.

`defer` is permitted in the statement list of every executable lexical block.
It belongs to the innermost block containing the statement and is registered
only if execution reaches it. A function body, an `if` branch, a loop body or
iteration, and a standalone expression block therefore each establish their own
defer scope.

The only valid form is `defer` followed by a function or method call. A block or
any other statement or expression is rejected. The function or method value,
receiver, and arguments are evaluated immediately at the defer statement and
saved. Only the invocation is delayed, and its eventual result is discarded:

```text
mut value = 1;
defer print(value); // Saves 1.
value = 2;
// Prints 1 when this block exits.
```

Before an early transfer, any associated value expression is evaluated first,
then the deferred actions for each exited scope run from innermost to outermost,
and then the transfer occurs. This applies to `return expression`,
`break expression`, and error propagation with `?`.

Error propagation is an ordinary early return and performs lexical cleanup.
Panics terminate without unwinding, so deferred actions do not run after a
panic begins.

Lexical scope means a defer inside a loop iteration runs at the end of that
iteration:

```text
for path in paths {
    mut file = File.open(path);
    defer file.close();

    process(file);
}
```

## 11. Backend-oriented lowering

SAO's semantics should not depend on non-standard C extensions.

Expression blocks can lower to temporaries:

```text
const result = {
    const value = calculate();
    value + 1
};
```

Conceptually lowers to:

```c
int64_t result;
{
    int64_t value = calculate();
    result = value + 1;
}
```

Loop expressions similarly lower to a result temporary plus ordinary branches.
The IR, rather than the C syntax, should represent block parameters, loop result
values, and cleanup edges.

Generated C should eventually include `#line` directives so compiler diagnostics
refer back to the original SAO source.

Debug and release builds can initially map to conventional C compiler modes:

```text
debug:   -O0 -g
release: -O2
```

The backend must implement SAO's trapping integer overflow and shift bounds
without first invoking C undefined behaviour. It must also preserve SAO's
defined evaluation order rather than accidentally inheriting C's unspecified
or undefined behaviour.

## 12. Runtime representation and memory management

### 12.1 Initial memory-management model

SAO will initially use a simple tracing garbage collector rather than reference
counting. The intended first implementation is a single-threaded,
stop-the-world, non-moving mark-and-sweep collector.

Initial collector rules:

- Heap allocations are managed by the collector.
- Compiler-generated metadata identifies references held by heap objects.
- The C backend provides precise roots through compiler-generated shadow-stack
  frames. Each active generated function links a frame containing its live GC
  references into the runtime shadow stack and unlinks it before returning.
  The collector does not conservatively scan the native C stack.
- Collection occurs only at well-defined safe points, initially allocation
  points.
- Anonymous-function environments, anonymous-struct environments, and shared
  cells created for mutable captures are ordinary traced heap objects.
- Reference cycles are collected naturally.
- The first implementation has no user-defined destructors, finalizers, weak
  references, generational collection, or incremental collection.
- `defer` provides deterministic resource cleanup. Garbage collection reclaims
  memory and must not be relied on to close files, release locks, or perform
  other timely cleanup.

The collector is deliberately non-moving at first so object and interface
pointers remain stable and the portable C backend stays straightforward. More
advanced collectors may be considered later without changing the language's
observable memory-safety guarantees.

### 12.2 Specialized union representation

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

### 12.3 Other provisional value representations

Other runtime representations are not finalized. Interface representation and
dispatch follow the decisions in Section 6.1.

Likely initial representations include:

- `int` and `float` values represented as unboxed 64-bit values.
- `char` values represented as unboxed unsigned bytes restricted to 0 through
  127.
- `string` values represented as stable pointers to garbage-collected mutable
  sequence objects containing a length, capacity, and replaceable ASCII storage
  pointer, using the same outer-object model as `bytes`.
- `bytes` values represented as stable pointers to garbage-collected mutable
  byte buffers.
- Struct values represented as stable pointers to garbage-collected objects.
- Struct objects carrying a pointer to their concrete runtime type descriptor in
  the GC object header.
- Interface values represented by the same stable object pointer used by a
  concrete struct reference. The descriptor and method dictionary are reached
  through the object header.
- Anonymous interface objects represented by compiler-generated structs and
  the same shared per-concrete-type method dictionary.

Conceptual interface representation and dispatch metadata:

```text
+----------------+       +-----------------------+
| object pointer | ----> | GC object header      |
+----------------+       | type descriptor ------+----+
                         | object fields         |    |
                         +-----------------------+    |
                                                      v
                         +-----------------------------+
                         | concrete type identity      |
                         | GC tracing metadata         |
                         | sorted method dictionary    |
                         +-----------------------------+
```

These other layouts remain provisional and will be refined as the implementation
develops.

## 13. Deferred features

These ideas are desirable but are explicitly not part of the immediate language
core:

- Interface extension/default methods.
- A `satisfies` operator that preserves an anonymous object's exact hidden type.
- Nominal data-carrying enums.
- General `match` expressions and exhaustive pattern matching.
- User-defined generic structs, functions, and interfaces, including generic
  constraints, general-purpose generic inference, and associated types.
- `errdefer`.
- A Cranelift JIT or native object backend.
- A built-in linker or executable writer.
- Native threads and shared-memory concurrency.
- Modules, imports, visibility and access control, external package management,
  and separate compilation.

The IR should avoid preventing these additions, but the first implementation
does not need to support them.

## 14. Current language sketch

The following example combines the currently agreed ideas. Some library types and
error syntax remain illustrative:

```text
interface Reader {
    fn read(mut self, count: int) -> bytes;
}

interface Writer {
    fn write(mut self, data: bytes) -> int;
}

struct Buffer {
    data: bytes,
    position: int,

    fn read(mut self, count: int) -> bytes {
        // Implementation omitted.
    }

    fn write(mut self, data: bytes) -> int {
        // Implementation omitted.
    }
}

fn find_nonzero(data: bytes) -> int | none {
    for value in data {
        if value != 0 {
            break value;
        }
    } else {
        none
    }
}

fn copy_once(mut stream: Reader & Writer) -> int {
    const data = stream.read(4096);
    stream.write(data)
}

fn prefixed_writer(prefix: bytes, mut destination: Writer) -> mut Writer {
    Writer {
        fn write(mut self, data: bytes) -> int {
            destination.write(bytes.concat(prefix, data))
        }
    }
}

fn make_prefixer(prefix: bytes) -> fn(bytes) -> bytes {
    fn(data: bytes) -> bytes {
        bytes.concat(prefix, data)
    }
}

fn write_file(path: string, data: bytes) -> int {
    mut file = File.create(path);
    defer file.close();

    file.write(data)
}
```

The anonymous writer and function examples use automatic lexical capture. Their
environments use the specialized garbage-collected layouts described above. The
file example shows the intended lexical cleanup behaviour without allowing a
resource to escape its scope.
