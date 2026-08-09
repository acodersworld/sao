# 3. Static typing and inference

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
including callable types and interface requirements. A return
capability qualifier is unnecessary for copied value types such as `int`.
Within a union, `mut` qualifies the immediately following reference member, so
`-> mut User | none` returns either mutable access to a `User` or `none`.

## 3.1 Primitive types

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

## 3.2 Integer arithmetic

Integer arithmetic is trapping. Overflow detected at compile time is a
diagnostic; overflow at runtime causes an immediate panic in every build mode.
There are no initial checked, wrapping, overflowing, or saturating arithmetic
APIs.

Division or remainder by zero, dividing or taking the remainder of the minimum
`int` by `-1`, and a shift count outside the range `0` through `63` also panic.
Floating-point overflow follows IEEE 754 and may produce infinity rather than
panicking.

## 3.3 Operators

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

For operators that SAO shares with C, precedence and associativity follow C.
From highest to lowest, the binary precedence groups are multiplicative,
additive, shifts, relational (including `is`), equality, bitwise AND, bitwise
XOR, bitwise OR, logical AND, and logical OR. Binary operators associate left.
Postfix calls, member access, indexing, and `?` bind more tightly than unary
operators; parentheses override precedence.

SAO does not adopt C's unspecified function-argument evaluation order. An
ordinary call evaluates its function value, or its method receiver, first and
then evaluates arguments from left to right before invoking the function.

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

## 3.4 Explicit conversions

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

## 3.5 Floating-point behavior

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

## 3.6 ASCII strings and byte sequences

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
- Lambda parameter and return types are explicit in the lambda expression;
  contextual lambda signature inference is deferred.
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

## 3.7 Equality

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

## 3.8 Initial output built-in

The initial general-purpose output API consists only of:

```text
print(text: string) -> ()
println(text: string) -> ()
```

`print` writes the string's contents exactly to standard output and does not add
a newline. `println` writes the contents followed by exactly one newline. Other
primitive values must first be converted explicitly to `string`. File and other
I/O APIs will be designed separately and are not part of this initial built-in
surface.
