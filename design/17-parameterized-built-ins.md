# 17. Parameterized built-ins

SAO reserves four compiler-known parameterized type constructors:

```text
Queue<T>
Vector<T>
Map<K, V>
Error<T>
```

`Queue`, `Vector`, `Map`, and `Error` are case-sensitive reserved names rather
than user-defined generic declarations. `Queue`, `Vector`, and `Error` take
exactly one type argument; `Map` takes exactly two. Other named parameterized
type syntax remains available to compiler-known or future declarations, but the
initial language does not provide user-defined generics.

The initial construction forms are deliberately narrow:

```text
Queue<T>()
Vector<T>()
Map<K, V>()
Error(value)
Error<T>(value)
```

Queue, vector, and map construction takes no value arguments. Error
construction takes exactly one payload expression. `Error(value)` leaves its
payload type for semantic inference; `Error<T>(value)` supplies it explicitly.
All four constructions are primary expressions and may be followed by ordinary
postfix or infix syntax.

Built-in construction is not an ordinary function or method call. It therefore
cannot be used directly as the call operand of `defer` or `co`.

Section 10 defines the initial Queue operations, and Section 8 defines Error
payload and propagation behaviour. Vector and Map APIs, mutation rules, runtime
representation, and lowering are intentionally left for later design. This
chapter fixes only their names, type arities, and empty construction syntax.
