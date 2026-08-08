# 1. Design goals

SAO should be:

- Statically typed, with useful local type inference.
- Explicit at function boundaries.
- Expression-oriented, especially around blocks and control flow.
- Flexible enough to express union and intersection types.
- Nominal for data definitions and structural for behavioural interfaces.
- Equipped with first-class anonymous functions and capturing anonymous structs.
- Equipped with explicitly scheduled cooperative coroutines and typed queues.
- Pleasant for small programs and experimentation.
- Implementable through a backend-neutral IR.
- Initially compilable through portable C using an external compiler driver such
  as Clang or GCC.

The first implementation should favour clear semantics and good diagnostics over
advanced optimization.

SAO execution is strictly single-threaded but supports cooperatively scheduled
coroutines. An ordinary call remains part of the current coroutine; if that call
eventually executes `yield()`, the whole calling coroutine is suspended. No
coroutine runs concurrently with another, and the scheduler can change the
running coroutine only at an explicit `yield()` or when a non-main coroutine
returns. SAO does not provide preemptive threads, generators, or async functions.

An initial SAO program consists of one source file. All named declarations share
that file's program namespace. The language does not initially provide modules,
imports, access-control modifiers, external packages, or separate compilation.
