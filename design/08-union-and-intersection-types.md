# 8. Union and intersection types

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

When one source value could satisfy more than one destination member, an
expression type ascription selects the intended member explicitly:

```text
const selected: Reader | Writer = file: Reader;
```

Here `file: Reader` forms a borrowed interface view; it does not copy or move
the concrete file. The surrounding union stores the `Reader` member choice
while preserving the concrete type and vtable used for dispatch.

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

A value of `Reader & Writer` must satisfy both interfaces. An intersection type
can contextually accept an anonymous struct that satisfies every member:

```text
mut stream: Reader & Writer = struct {
    fn read(mut self, count: int) -> bytes {
        // ...
    }

    fn write(mut self, data: bytes) -> int {
        // ...
    }
};
```

SAO has no named interface-composition syntax. Call sites express combined
requirements directly with intersection types such as `Reader & Writer`.

Runtime interface narrowing, such as narrowing a `Reader` to
`Reader & Writer` after `value is Writer`, uses the runtime method metadata
described in Section 6.1.

## 8.1 Flow-sensitive type narrowing

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
interface value compares its concrete runtime vtable pointer. An interface
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

## 8.2 Recoverable errors and propagation

Recoverable errors are ordinary union values. SAO provides the built-in nominal
parameterized type `Error<T>`, whose value carries error information of type
`T`:

```text
fn myfunc() -> int | Error<string> {
    if operation_failed() {
        Error::new("operation failed")
    } else {
        42
    }
}
```

SAO does not initially support user-defined generic structs, functions, or
interfaces. `Error<T>` is a compiler-known type constructor with dedicated
type-checking and lowering rules, not an instance of a general source-language
generic facility. The other initial compiler-known parameterized types are
described in Section 17. They do not enable user-defined generics.

Each `Error<T>` instantiation is distinct from its payload type, from every
non-error member of a union, and from `Error<U>` when `T` and `U` differ.
Both `Error::new(value)` and `Error<T>::new(value)` are construction syntax. In
the first form, without an expected type, the payload type is the exact type of
`value`:

```text
const error = Error::new("operation failed"); // Error<string>
```

The explicit form supplies the payload type directly and requires the value to
be assignable to it:

```text
const error = Error<string>::new("operation failed");
```

An expected `Error<T>` type may instead guide construction when the value is
assignable to `T`. The compiler does not invent a payload union without such an
expected type.

The payload held by an `Error<T>` is immutable. If the payload is a reference,
accessing it cannot grant more than const access. This permits the built-in
widening conversion `Error<A>` to `Error<B>` whenever `A` is assignable to `B`.
This covariance is a specific rule for `Error`, not a general variance feature:

```text
const specific: Error<IoError> = Error::new(IoError { /* ... */ });
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

The postfix Try operator, written `?`, propagates an error without exceptions:

```text
fn caller() -> int | Error<string> {
    const value = myfunc()?;
    value + 1
}
```

For an operand of type `S | Error<E>`, the Try operator evaluates the operand
once. If it is an `S`, the expression produces that value with type `S`. If it
is an `Error<E>`, the current function returns that error immediately. The
enclosing function's declared return type must accept the propagated `Error<E>`,
including through the built-in payload-widening conversion. `S` may itself be a
union of several non-error types.

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
