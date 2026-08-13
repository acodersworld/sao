# 10. Cooperative coroutines and queues

SAO provides single-threaded cooperative coroutines. A coroutine is started by
placing `co` before an ordinary function or method call:

```text
co run_worker(messages);
co service.process(request);
```

`co` uses call-only syntax. It prepares the call immediately using the same
left-to-right evaluation order as an ordinary call: the function value or method
receiver is evaluated first, followed by its arguments. The resulting function,
receiver, and argument values are saved in a new coroutine, which is appended to
the ready queue. The called function body does not begin executing as part of
the `co` statement.

The called function may have any return type. Its eventual return value,
including an `Error<T>`, is discarded. The `co` statement itself produces `()`.
There are initially no coroutine handles, joins, cancellation operations, or
parent-child lifetime relationships.

The program begins with `main` as its initial coroutine. Ready coroutines are
scheduled in first-in, first-out order. The built-in function:

```text
yield() -> ()
```

appends the current coroutine to the end of the ready queue and resumes the
oldest other ready coroutine. If there is no other ready coroutine, `yield()`
returns immediately. A coroutine that does not yield can starve every other
coroutine.

Returning from a non-main coroutine discards its result and resumes the oldest
ready coroutine. Once `main` has completed its lexical deferred actions and
returns, the process terminates immediately and abandons every remaining
coroutine, whether ready or suspended. Deferred actions belonging to abandoned
coroutines do not run.

`yield()` is the only voluntary scheduling point. Creating a coroutine, sending
or receiving through a queue, allocating memory, and performing I/O do not
implicitly yield. A blocking operating-system or C runtime call therefore blocks
the entire SAO program. A panic in any coroutine follows the ordinary panic rule
and terminates the entire process without unwinding any coroutine.

Yielding is transitive through ordinary calls. If `outer` calls `inner` and
`inner` executes `yield()`, both activations remain suspended as part of the same
coroutine:

```text
fn outer() -> () {
    inner();
}

fn inner() -> () {
    yield();
}
```

## 10.1 Queues

Coroutines communicate through the compiler-known built-in reference type
`Queue<T>`. Like `Error<T>`, this is a dedicated parameterized type and does not
enable user-defined generics. A fresh queue is constructed with:

```text
mut messages = Queue<int>::new();
```

Queues are unbounded and preserve first-in, first-out message order. Their
initial operations are conceptually:

```text
fn send(mut self, value: T) -> ();
fn try_receive(mut self) -> T | none;
```

`send` appends a value and completes without scheduling another coroutine.
`try_receive` removes and returns the oldest value when one exists, or returns
`none` immediately when the queue is empty. It never waits and never invokes the
scheduler. A function waiting for a message must explicitly call `yield()` and
try again:

```text
const message: int = loop {
    const received = messages.try_receive();

    if received is int {
        break received;
    }

    yield();
};
```

The initial `Queue<T>` requires that `T` not contain `none`; otherwise an empty
queue could not be distinguished from a successfully received `none` value.
A future explicitly tagged receive-result type may remove this restriction.

Sending follows ordinary assignment semantics. Independently copied value types
are copied into the queue; reference values copy their object reference and
preserve the access capability admitted by `T`. Queue references themselves may
be shared between coroutines under SAO's existing aliasing and capability rules.
Because scheduling never occurs during a queue operation, each operation
completes before another coroutine can access the queue.
