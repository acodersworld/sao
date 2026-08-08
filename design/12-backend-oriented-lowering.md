# 12. Backend-oriented lowering

SAO's semantics should not depend on non-standard C extensions.

Expression blocks can lower to temporaries:

```text
const result = {
    const value = calculate();
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

The initial coroutine implementation uses explicit compiler-generated activation
frames rather than native C stack suspension. Every generated C function accepts
a pointer to a frame struct containing its parameters, source locals,
intermediate values, saved deferred calls, caller link, and current execution
state. The first implementation may retain every local in the frame for
simplicity; a later optimization may retain only values live across possible
suspension points.

Every function uses a uniform resumable calling protocol, including functions
that never yield. A function that directly executes `yield()`, or that can call
another function which may yield, is lowered as a state machine with resume
states around its suspension-capable operations. This transitive rule applies to
ordinary calls, recursion, anonymous-function calls, and interface dispatch. The
uniform protocol avoids exposing a coroutine effect in source function types and
allows a single callable value or interface requirement to refer to both
yielding and non-yielding implementations.

Each coroutine owns a linked chain of activation frames representing its SAO
call stack. `yield()` preserves that chain and returns control to the runtime
scheduler. Resumption invokes the top frame at its saved state. Returning from a
function removes its frame and delivers its result to the saved caller state;
returning from the coroutine's root frame completes that coroutine.

Generated C includes standard `#line` directives that map generated statements
back to their originating SAO file and line. C compiler diagnostics and debug
locations therefore refer to the SAO source rather than the generated C file.

Build mode maps to these C compiler flags:

```text
default:  -O2
--debug:  -O0 -g
```

The backend must implement SAO's trapping integer overflow and shift bounds
without first invoking C undefined behaviour. It must also preserve SAO's
defined evaluation order rather than accidentally inheriting C's unspecified
or undefined behaviour.
