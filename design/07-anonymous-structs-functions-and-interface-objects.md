# 7. Anonymous structs, lambdas, and interface objects

## 7.1 Interface-constrained anonymous structs

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

## 7.2 Capture semantics

Anonymous structs and lambdas automatically capture referenced
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
functions and lambdas share the same callable `fn(...) -> ...` types. A nested
function has no capture environment and is lowered like any other named
function.

## 7.4 Lambdas

Lambdas are anonymous function expressions written with `lambda` and an
explicit signature:

```text
const factor = 1.5;

const scale = lambda(value: float) -> float {
    value * factor
};
```

The inferred callable type of `scale` is the single type
`fn(float) -> float`. A function value contains both callable code and any
captured environment.

Mutable captures are shared:

```text
mut count = 0;

const next = lambda() -> int {
    count += 1;
    count
};

next(); // 1
next(); // 2
// count is now 2
```

If several lambdas capture the same mutable binding, they observe
the same storage. SAO has no ownership-transfer or `move` capture modifier.

A `const` binding containing a lambda does not make that function pure. Calling
it may still mutate a `mut` binding captured by the lambda.

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

Every method receives that object as `self`, so all methods share the same
captures. Interface-constrained anonymous structs use this same representation
behind their interface value.

SAO execution is single-threaded. Closure environments, shared capture cells,
and collector state use no atomic operations, locks, or thread-safety marker
types. Future shared-memory concurrency would require an explicit new design;
these values are not implicitly safe to share with concurrently executing SAO
threads.
