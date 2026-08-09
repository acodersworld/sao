# 11. Lexical `defer`

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
- Before the Try operator (`?`) exits their scope through error propagation.

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
mut value = "first";
defer print(value); // Saves the first string object.
value = "second";
// Prints "first" when this block exits.
```

Before an early transfer, any associated value expression is evaluated first,
then the deferred actions for each exited scope run from innermost to outermost,
and then the transfer occurs. This applies to `return expression`,
`break expression`, and error propagation by the Try operator (`?`).

Error propagation is an ordinary early return and performs lexical cleanup.
Panics terminate without unwinding, so deferred actions do not run after a
panic begins.

A deferred call may itself execute `yield()`, directly or through another call.
In that case the coroutine suspends while exiting the scope. After it resumes,
that deferred call completes and the remaining deferred calls continue in
reverse registration order. If a deferred call in `main` yields, other ready
coroutines may run before `main` finishes; once all of `main`'s deferred calls
complete and `main` returns, the remaining coroutines are abandoned.

Lexical scope means a defer inside a loop iteration runs at the end of that
iteration:

```text
for path in paths {
    mut file = File.open(path);
    defer file.close();

    process(file);
}
```
