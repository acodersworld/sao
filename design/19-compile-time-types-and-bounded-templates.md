# 19. Compile-time types and bounded templates

SAO supports a deliberately restricted form of compile-time programming in
which compile-time values are types. This feature provides transparent aliases,
type-producing functions, generated nominal structs, and explicitly specialized
runtime functions and methods. It is not general compile-time execution or a
general-purpose generic type system.

## Declaration namespace and aliases

Named value and type declarations share one lexical namespace. A same-scope
function, type factory, struct, interface, or type alias cannot reuse another
named declaration's name. Ordinary sequential local-binding shadowing remains
permitted.

A file-level alias is transparent:

```text
type IntBox = Box(int);
```

The alias introduces no nominal identity or runtime value. Its identity and
capability are exactly those of its resolved target. Aliases may refer forward,
but direct and indirect alias cycles are errors. Local aliases are deferred.

## Type factories and generated structs

A function returning `type` is a type factory:

```text
fn Box(comptime T: type) -> type {
    struct {
        inner: T,

        fn get(self) -> T {
            self.inner
        }
    }
}
```

Type factories are permitted at file level and as receiverless struct members.
They have no receiver or runtime parameters and currently accept only
unconstrained `comptime T: type` parameters. Their body contains one final type
expression or an explicit type-valued return. Locals, mutation, runtime
expressions, control flow, and arbitrary compile-time execution are not
permitted in a factory body.

Factory application uses ordinary parentheses in type position: `Box(int)`.
Applications may compose existing types, aliases, built-in type factories, and
other user type factories. A generated `struct` may declare typed fields,
ordinary methods, receiverless associated functions, and receiverless
associated type factories. It may capture the enclosing factory's compile-time
type parameters but cannot capture runtime values.

Each generated struct is nominal. Its canonical identity is derived from the
generating struct declaration and the canonical type arguments captured by the
factory application. Repeating the same application reuses that identity;
different arguments produce different types even when their resulting fields
have the same shape. Exact recursive requests reuse an in-progress identity,
while argument-expanding recursion is rejected. Every inline layout must still
be finite under Chapter 5's rules.

Generated types support construction and associated selection directly through
their application:

```text
const boxed = Box(int) { inner: 10 };
const made = Box(int)::make(20);
```

An associated type factory may be called only through a statically known
concrete owner, not through a symbolic type parameter.

## Bounded runtime templates

A top-level runtime function or struct method may declare leading compile-time
type parameters. A receiver, when present, comes first; all compile-time
parameters follow it and precede every runtime parameter:

```text
interface Reader {
    fn read(self) -> int;
}

fn inspect(comptime T: Reader, value: T) -> int {
    value.read()
}

struct Inspector {
    fn inspect(self, comptime T: Reader, value: T) -> int {
        value.read()
    }
}
```

An inline `T: Interface` constraint or one `where` entry may bound a parameter.
A `where` constraint may name an interface, intersect interfaces, or declare a
private anonymous interface:

```text
fn inspect(comptime T: type, value: T) -> int
where T: Reader & Logger, {
    value.read()
}
```

Interfaces remain runtime-oriented. Requirements use a receiver and only
runtime parameter and return types; interface requirements cannot themselves
declare compile-time parameters or return `type`.

Every template is checked symbolically when declared, whether or not it is ever
specialized. A bounded parameter exposes only the methods promised by its
constraint. Specialization does not retroactively permit concrete-only fields,
methods, constructors, associated functions, `.copy()`, or primitive operators.
An unconstrained `T: type` exposes no value members.

## Explicit specialization

Type arguments are always explicit and occupy the leading declared argument
positions. They are compile-time syntax and are not evaluated as runtime
expressions:

```text
const result = inspect(File, file);
const other = inspector.inspect(File, file);
```

The compiler does not infer template arguments. A bounded argument must be a
named or generated concrete struct, or its GC-qualified form, and must satisfy
the declared structural interface. Independent runtime arguments are still
checked when a type argument or constraint is invalid so diagnostics recover
deterministically.

A top-level callable specialization is identified by its source declaration
and ordered canonical type arguments. A method specialization additionally
includes its concrete owner identity. Consequently, the same generated method
body specialized on `Box(int)` and `Box(string)` produces distinct callable
specializations even if its own explicit type arguments match. Repeated
requests for an identical identity reuse one specialization.

Specialization substitutes concrete types through signatures, annotations,
generated owners, expression analysis, value capabilities, places, transfers,
returns, layouts, and nested specialization requests. Exact recursion reuses
the in-progress callable specialization. Recursion that repeatedly expands the
same declaration's owner or type arguments is rejected.

Unspecialized templates are not first-class callable values. Local template
declarations, compile-time values other than types, arbitrary compile-time
execution, generic inference, compile-time duck typing, generic interfaces, and
general user-defined parameterized nominal declarations remain deferred.
