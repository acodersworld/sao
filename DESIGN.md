# SAO Language Design

Status: early design notes  
Working name: SAO  
Last updated: 2026-08-05

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

Although coroutines are deferred, the IR should not assume that every function
activation runs continuously from call to return. Control flow, live locals,
and lexical cleanup scopes should remain explicit enough that a future lowering
pass can transform a suspendable function into a resumable state machine.

The C compiler driver is responsible for assembling and linking. SAO does not
initially need its own linker.

Possible commands:

```text
sao emit-c program.sao
sao build program.sao
sao build --cc clang program.sao
sao build --cc gcc program.sao
```

Compiler selection should eventually follow this order:

1. An explicit `--cc` argument.
2. The `CC` environment variable.
3. `clang` if available.
4. `gcc` if available.
5. A clear diagnostic explaining that `emit-c` remains available.

The compiler implementation language has not been finalized. Rust remains a
strong candidate.

## 3. Static typing and inference

SAO is statically typed. Programs are rejected when their operations cannot be
shown to be type-safe.

Local variables may be inferred:

```text
let count = 10;
let ratio = 0.5;
```

Types may also be written on the declaration:

```text
let count: int = 10;
let ratio: float = 0.5;
```

Mutation is explicit:

```text
let mut position: int = 0;
position += 1;
```

### 3.1 Primitive types

SAO has a deliberately small, fixed primitive set:

- `int` is exactly a signed 64-bit integer. It is not platform-dependent, and
  there are initially no other integer widths or unsigned integer types.
- `float` is exactly an IEEE 754 binary64 floating-point value. There are
  initially no other floating-point widths.
- `bool` has the values `true` and `false`. Conditions require `bool`; SAO has
  no implicit truthiness conversions.
- `char` is one Unicode scalar value rather than an eight-bit byte.
- `string` is a garbage-collected Unicode text type.
- `()` is the unit type and has one value, also written `()`.
- `none` is the singleton absence type and value described with unions in
  Section 8.

The names `int` and `float` expose their fixed meanings directly; `i64` and
`f64` are not separate source-language type names. Integer literals have type
`int`, and floating-point literals have type `float`. An integer literal outside
the `int` range is a compile-time error.

Binary data will use a separate byte-sequence abstraction rather than another
scalar primitive. Its exact name and API remain to be designed.

### 3.2 Integer arithmetic

Integer arithmetic is trapping. Overflow detected at compile time is a
diagnostic; overflow at runtime causes an immediate panic in every build mode.
There are no initial checked, wrapping, overflowing, or saturating arithmetic
APIs.

Division by zero, dividing the minimum `int` by `-1`, and a shift count outside
the range `0` through `63` also panic. Floating-point overflow follows IEEE 754
and may produce infinity rather than panicking.

Current inference boundary:

- Local variable types may be inferred from their initializer and context.
- Inference must resolve to one unambiguous type.
- Inference never synthesizes a union merely because different paths produce
  different types.
- Named function parameter types are explicit.
- Every named function has an explicit return type.
- Function return types are never inferred.
- Anonymous function parameter and return types are explicit in the function
  expression; contextual lambda signature inference is deferred.
- Generic inference is deferred.

For example:

```text
fn add(left: int, right: int) -> int {
    left + right
}

fn print_user(user: User) -> () {
    print(user.name);
}
```

The unit type is provisionally spelled `()`.

## 4. Expression-oriented blocks

Blocks are expressions. The final expression without a semicolon is the value of
the block:

```text
let distance = {
    let x = 10.0;
    let y = 20.0;

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
let value = if condition {
    42
} else {
    3.14
};

// Type error: inference cannot choose one type.
```

Different branch types require an explicit type that accepts them:

```text
let value: int | float = if condition {
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
allocates a garbage-collected object. Assignment, parameter passing, returning,
and capture copy the reference rather than the object's fields. SAO performs no
implicit deep copies and has no ownership, borrowing, or move semantics. If
mutation is permitted, every alias observes changes to the same object.

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

let velocity = Velocity { x: 1.0, y: 2.0 };
teleport(velocity); // Type error.
```

Conversions between nominal structs are explicit.

Struct construction is provisionally written:

```text
let position = Position {
    x: 10.0,
    y: 20.0,
};
```

Methods are declared directly in the struct body alongside its fields. SAO has
no separate `impl` block and does not attach methods to a struct after its
definition. This keeps a type's complete method set local to its declaration.

Anonymous structs use the same model. A `struct { ... }` expression declares a
hidden nominal type and constructs a value of that type:

```text
let position = struct {
    let x: float = 10.0;
    let y: float = 20.0;

    fn magnitude(self) -> float {
        sqrt(self.x * self.x + self.y * self.y)
    }
};
```

The source program cannot name the anonymous struct's generated type, but local
inference may retain that one exact hidden type. It may be passed to any
structural interface that its methods satisfy.

The method receiver `self` is a reference to the original object. Field
mutability and the permissions granted through `self` are not yet defined.

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
them. Method matching, variance, and visibility still need formal
specification. The initial direction is exact signature matching and no method
overloading.

## 7. Anonymous structs, functions, and interface objects

### 7.1 Interface-constrained anonymous structs

An interface can be used to construct an anonymous implementation:

```text
interface Greeter {
    fn greet(self, name: string) -> string;
}

let greeter = Greeter {
    let prefix: string = "Hello";

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
- `let` declarations at object scope define hidden fields.
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
interface Predicate<T> {
    fn test(self, value: T) -> bool;
}

fn greater_than(limit: int) -> Predicate<int> {
    Predicate<int> {
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
- An immutable `let` binding is captured as the value it holds when the
  anonymous value is created.
- A mutable `let mut` binding is captured as a shared binding. Mutations are
  visible to the outer scope and to every anonymous value that captures it.
- A captured binding remains alive for as long as any capturing value can use
  it, even after its original lexical scope has returned.
- Captures are hidden storage and do not become fields accessible through an
  interface.
- Named structs and named functions do not capture lexical state.
- Parameters and locals inside a method or anonymous function shadow captures
  with the same name.

Explicit fields and captures are distinct. A `let` declared at anonymous-struct
scope is an owned field and is accessed through `self`; a bare reference to an
outer binding is a capture:

```text
let prefix = "log: ";

let formatter = struct {
    let suffix = "\n";

    fn format(self, message: string) -> string {
        prefix + message + self.suffix
    }
};
```

The implementation may lift mutable captured bindings into shared
garbage-collected heap cells. The exact cell and environment layouts remain an
implementation detail, but they must preserve these observable semantics.

### 7.3 Anonymous functions

Anonymous functions are expressions written with `fn` and an explicit
signature:

```text
let factor = 1.5;

let scale = fn(value: float) -> float {
    value * factor
};
```

The inferred callable type of `scale` is the single type
`fn(float) -> float`. A function value contains both callable code and any
captured environment.

Mutable captures are shared:

```text
let mut count = 0;

let next = fn() -> int {
    count += 1;
    count
};

next(); // 1
next(); // 2
// count is now 2
```

If several anonymous functions capture the same mutable binding, they observe
the same storage. Explicit capture lists are deferred; SAO has no ownership
transfer or `move` capture modifier.

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
let result: int | none = none;
```

An optional value is therefore an ordinary union such as `int | none`; SAO does
not require a separate built-in `Option<T>` type. Operations on the non-`none`
member require the same narrowing as other union types.

SAO uses `&` for intersection types:

```text
fn copy(stream: Reader & Writer) -> () {
    let data = stream.read(4096);
    stream.write(data);
}
```

A value of `Reader & Writer` must satisfy both interfaces. Intersections can also
be used for anonymous implementations:

```text
let stream = Reader & Writer {
    fn read(self, count: int) -> Bytes {
        // ...
    }

    fn write(self, data: Bytes) -> int {
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
`Reader & Writer` after `value is Writer`, is a desired future feature but not
required for the first implementation.

### 8.1 Recoverable errors and propagation

Recoverable errors are ordinary union values. SAO provides a built-in nominal
generic type `Error<T>` whose value carries error information of type `T`:

```text
fn myfunc() -> int | Error<string> {
    if operation_failed() {
        Error("operation failed")
    } else {
        42
    }
}
```

`Error<T>` is distinct from its payload type and from every non-error member of
the union. The built-in generic does not depend on user-defined generics being
available in the first implementation. `Error(value)` is the initial
construction syntax.

The postfix `?` operator propagates an error without exceptions:

```text
fn caller() -> int | Error<string> {
    let value = myfunc()?;
    value + 1
}
```

For an operand of type `S | Error<E>`, `?` evaluates the operand once. If it is
an `S`, the expression produces that value with type `S`. If it is an
`Error<E>`, the current function returns that error immediately. The enclosing
function's declared return type must accept the propagated `Error<E>`. `S` may
itself be a union of several non-error types.

Error payloads can use ordinary SAO types and unions:

```text
fn load() -> Config | Error<ParseError | IoError> {
    let text = read_file("config.sao")?;
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

for let mut index = 0; index < 10; index += 1 {
    // Traditional three-clause loop.
}
```

Every loop form is an expression and may produce a value with `break value`.

### 9.1 Infinite loops

An infinite loop can only complete by breaking or transferring control elsewhere:

```text
let command = loop {
    let input = read_line();

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
let admin: User | none = for user in users {
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
let divisor = for let mut candidate = 2;
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

### 9.4 Labeled loops

Labels allow a nested loop to produce the result of an outer loop:

```text
let result: Cell | none = outer: for row in grid {
    for cell in row {
        if cell.matches(target) {
            break outer cell;
        }
    }
} else {
    none
};
```

The exact label grammar is provisional, but labels and labeled value breaks are
part of the intended loop design.

## 10. Lexical `defer`

SAO has Go-like `defer` syntax with lexical block scope:

```text
fn read_file(path: string) -> string {
    let file = File.open(path);
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

Error propagation is an ordinary early return and performs lexical cleanup.
Panics terminate without unwinding, so deferred actions do not run after a
panic begins.

Block form is also supported:

```text
defer {
    transaction.release_lock();
    logger.debug("lock released");
}
```

Lexical scope means a defer inside a loop iteration runs at the end of that
iteration:

```text
for path in paths {
    let file = File.open(path);
    defer file.close();

    process(file);
}
```

Still to specify:

- Whether deferred call arguments are evaluated at registration or execution.
- Whether `defer` is allowed at every block scope.
- Restrictions on control flow inside deferred blocks.

## 11. Backend-oriented lowering

SAO's semantics should not depend on non-standard C extensions.

Expression blocks can lower to temporaries:

```text
let result = {
    let value = calculate();
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
- The C backend must provide precise roots, likely through compiler-generated
  shadow-stack frames, rather than relying on conservative scanning of the C
  stack.
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

### 12.2 Provisional value representations

Other runtime representations are not finalized.

Likely initial representations include:

- `int` and `float` values represented as unboxed 64-bit values.
- Struct values represented as stable pointers to garbage-collected objects.
- Union values represented by a tag and payload when a runtime distinction is
  necessary.
- Interface values represented by an object/data pointer and method-table
  pointer.
- Anonymous interface objects represented by compiler-generated structs and
  method tables.
- Anonymous functions represented by a code pointer and captured-environment
  pointer, or an equivalent backend-specific layout.

Conceptual interface representation:

```text
+--------------+----------------------+
| data pointer | method-table pointer |
+--------------+----------------------+
```

These layouts remain provisional and will be refined alongside SAO's value and
receiver semantics.

## 13. Deferred features

These ideas are desirable but are explicitly not part of the immediate language
core:

- Interface extension/default methods.
- A `satisfies` operator that preserves an anonymous object's exact hidden type.
- Nominal data-carrying enums.
- Exhaustive pattern matching.
- Runtime interface tests and intersection-aware narrowing.
- Generic interfaces and associated types.
- `errdefer`.
- A Cranelift JIT or native object backend.
- A built-in linker or executable writer.
- First-class coroutines or generators.

Coroutine syntax and semantics are deferred from the first implementation, but
they are an intended future language feature. The IR and runtime should make it
possible to preserve suspended activation state, captured references, and
lexical cleanup scopes. Suspension itself should not be treated as leaving a
scope: deferred actions would run when the coroutine completes or is otherwise
closed, not merely when it yields. Cancellation and abandonment semantics remain
to be designed.

The IR should avoid preventing these additions, but the first implementation
does not need to support them.

## 14. Open design questions

The following decisions are intentionally unresolved:

1. **Mutation:** field mutability, whether `let` only prevents rebinding, and
   whether methods need an explicit mutable receiver form.
2. **Numeric and text details:** numeric conversion rules, floating-point edge
   cases, string encoding and indexing, and the byte-sequence API.
3. **Union layout:** specialized unions versus a universal tagged `Value`.
4. **Interface values:** equality, downcasting, and runtime type metadata.
5. **Generics:** syntax, constraints, inference, monomorphization, and interface
   interaction.
6. **Modules:** imports, visibility, access control, and separate compilation.
7. **Pattern matching:** whether and when it joins the initial implementation.
8. **Closure representation:** exact environment layout, thread safety, and
   possible explicit capture-list syntax.
9. **Coroutines:** syntax, yielded and resumed value types, cancellation,
   abandonment, cleanup, and whether coroutines are stackless or stackful.
10. **Concurrency and async:** intentionally postponed; their relationship to
    coroutines remains to be designed.

## 15. Current language sketch

The following example combines the currently agreed ideas. Some library types and
error syntax remain illustrative:

```text
interface Reader {
    fn read(self, count: int) -> Bytes;
}

interface Writer {
    fn write(self, data: Bytes) -> int;
}

struct Buffer {
    data: Bytes,
    position: int,

    fn read(self, count: int) -> Bytes {
        // Implementation omitted.
    }

    fn write(self, data: Bytes) -> int {
        // Implementation omitted.
    }
}

fn find_non_empty(streams: List<Reader>) -> Reader | none {
    for stream in streams {
        let data = stream.read(1);

        if !data.is_empty() {
            break stream;
        }
    } else {
        none
    }
}

fn copy_once(stream: Reader & Writer) -> int {
    let data = stream.read(4096);
    stream.write(data)
}

fn prefixed_writer(prefix: Bytes, destination: Writer) -> Writer {
    Writer {
        fn write(self, data: Bytes) -> int {
            destination.write(prefix + data)
        }
    }
}

fn make_prefixer(prefix: Bytes) -> fn(Bytes) -> Bytes {
    fn(data: Bytes) -> Bytes {
        prefix + data
    }
}

fn write_file(path: string, data: Bytes) -> int {
    let file = File.create(path);
    defer file.close();

    file.write(data)
}
```

The anonymous writer and function examples use automatic lexical capture. Their
observable behaviour is specified, while their exact environment layout remains
open. Their environments are garbage-collected. The file example shows the
intended lexical cleanup behaviour without allowing a resource to escape its
scope.
