# 7. Anonymous structs, lambdas, and interface objects

## 7.1 Contextually typed anonymous structs

An ordinary anonymous struct can be converted to a structural interface through
an expected type:

```text
interface Greeter {
    fn greet(self, name: string) -> string;
}

const greeter: Greeter = struct {
    prefix: string = "Hello";

    fn greet(self, name: string) -> string {
        self.prefix + ", " + name
    }
};
```

The `struct { ... }` expression creates a hidden nominal struct containing the
declared fields and methods. The `Greeter` annotation supplies the expected
type, so the compiler verifies structural satisfaction and converts the result
to an interface value. Function arguments and return expressions receive the
same contextual conversion from their parameter or return type.

Contextual interface-conversion rules:

- `struct { ... }` is the only anonymous-struct construction syntax; an
  interface name is never itself constructed.
- An expected interface or interface-intersection type checks the hidden
  struct's method set and converts the resulting reference to that type.
- Without an expected interface type, local inference retains the anonymous
  struct's exact hidden type until it is used in an interface context.
- Field initializers at object scope define hidden fields and are written
  `name: Type = expression;`. The type annotation may be omitted when it can be
  inferred unambiguously.
- Hidden field types may be inferred or explicitly annotated.
- All required interface methods must be present.
- Non-unit method return types remain explicit; an omitted annotation defaults
  to `()`.
- Hidden fields are not accessible after conversion through the interface
  value.
- Extra implementation methods are not visible through the converted interface
  value.
- The source program cannot name the compiler-generated backing type.

## 7.2 Capture semantics

Anonymous structs and lambdas automatically capture referenced
bindings from their surrounding lexical scope. Captures do not need to be
redeclared as fields:

```text
interface IntPredicate {
    fn test(self, value: int) -> bool;
}

fn greater_than(limit: int) -> IntPredicate {
    struct {
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
- Named structs and named functions, including nested functions, do not capture
  lexical state.
- Parameters and locals inside a method or lambda shadow captures
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

## 7.3 Nested functions

Named functions may be declared inside executable blocks:

```text
fn calculate(value: int) -> int {
    fn double(input: int) -> int {
        input * 2
    }

    double(value)
}
```

A nested function is lexically scoped but does not capture bindings from its
enclosing function. It may reference top-level declarations, its own parameters
and locals, and its own name for recursion. Referencing an enclosing local is a
compile-time error; a lambda must be used when capture is required.

A nested function's name may be used as a first-class function value. Named
functions and lambdas share callable `fn(...) -> ...` and
`mut fn(...) -> ...` types. A nested function has no capture environment, so
its function value is const and is lowered like any other named function.

## 7.4 Lambdas

Lambdas are anonymous function expressions written with `lambda`. Parameter
types are explicit, while the return annotation may be omitted when it defaults
to `()`:

```text
const factor = 1.5;

const scale = lambda(value: float) -> float {
    value * factor
};
```

As with named functions, omitting `-> Type` means `-> ()`; the return type is
not inferred from the body.

The inferred callable type of `scale` is the single type
`fn(float) -> float`. A function value contains both callable code and any
captured environment.

Callable capability is determined solely from capture capabilities. A lambda
has a `mut fn(...) -> ...` type if any captured binding has mutable binding
storage or mutable value access, regardless of whether its body uses that
mutability. A lambda whose captures are const on both axes has a const
`fn(...) -> ...` type. This conservative rule avoids classifying callable
capability from the operations performed by the body.

Mutable captures are shared, and any lambda with a mutable capture must be held
with mutable value access:

```text
mut count = 0;

const vmut next = lambda() -> int {
    count += 1;
    count
};

next(); // 1
next(); // 2
// count is now 2
```

The inferred type of the lambda is `mut fn() -> int`. The `const` qualifier
keeps `next` bound to that callable, while `vmut` provides the mutable value
access required to invoke it. Writing only `const next` is a type error because
that binding would provide only const access to a mutable callable.

If several lambdas capture the same mutable binding, they observe
the same storage. SAO has no ownership-transfer or `move` capture modifier.

A const callable is not otherwise pure. It may perform I/O, mutate values
received through `mut` parameters, and have other effects that do not mutate
its captured environment.

## 7.5 Closure and environment representation

Every lambda expression has a compiler-generated environment type. A lambda
value has a uniform two-word representation:

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
offsets are backend-private details. A non-capturing lambda retains the same
two-word callable representation, uses a null environment pointer, and requires
no environment allocation. Named function values use the same representation
with a null environment pointer.

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

Every anonymous-struct function must receive that object as `self`, so all
methods share the same captures. Receiverless associated functions are limited
to named structs. Contextually converted anonymous structs use this same
representation behind their interface value.

SAO execution is single-threaded. Closure environments, shared capture cells,
and collector state use no atomic operations, locks, or thread-safety marker
types. Future shared-memory concurrency would require an explicit new design;
these values are not implicitly safe to share with concurrently executing SAO
threads.
