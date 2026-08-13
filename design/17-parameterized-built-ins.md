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
Queue<T>::new()
Vector<T>::new()
Map<K, V>::new()
Error::new(value)
Error<T>::new(value)
```

Queue, vector, and map construction takes no value arguments. Error
construction takes exactly one payload expression. `Error::new(value)` leaves
its payload type for semantic inference; `Error<T>::new(value)` supplies it
explicitly.

`new` is a compiler-known associated function selected with the same `::`
syntax as an associated function on a named struct. Its function value may be
selected without immediately calling it, and calling it uses ordinary call
syntax. The compiler gives these four `new` functions dedicated type inference,
arity, and lowering rules.

The former direct-call spellings such as `Queue<T>()` and `Error(value)` are not
valid. Construction always selects `new` explicitly.

Section 10 defines the initial Queue operations, and Section 8 defines Error
payload and propagation behaviour. Vector and Map APIs, mutation rules, runtime
representation, and lowering are intentionally left for later design. This
chapter fixes only their names, type arities, and construction syntax.
