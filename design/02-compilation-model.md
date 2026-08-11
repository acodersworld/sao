# 2. Compilation model

The intended compiler pipeline is:

```text
source
  -> lexer
  -> parser
  -> AST
  -> semantic analysis
  -> typed high-level IR
  -> lowered middle IR
  -> C backend
  -> clang/gcc
  -> native executable
```

Semantic analysis is internally staged as name and scope resolution, contextual
validation, type checking and inference, and post-type analyses such as capture
analysis. Each validation rule belongs to the earliest stage with enough
information to decide it; validation is not a separate catch-all pass.

The IR should belong to SAO rather than mirror C. This leaves room for an
interpreter or a Cranelift JIT/AOT backend in the future:

```text
                       +-> IR interpreter
source -> typed IR ----+-> C backend -> clang/gcc -> executable
                       +-> Cranelift backend (future)
```

The C compiler driver is responsible for assembling and linking. SAO does not
initially need its own linker.

The initial compiler exposes one build command:

```text
sao build --cc clang program.sao
sao build --debug --cc clang program.sao
```

`--cc` is required and names the C compiler executable to invoke. The compiler
is selected explicitly; SAO does not consult the `CC` environment variable or
automatically search for Clang or GCC. Another compatible compiler may be used
by passing its executable name or path to `--cc`.

Builds are optimized release builds by default. The optional `--debug` flag
selects an unoptimized build with debug information.

The SAO compiler is implemented in Rust.

Every runnable program declares exactly one top-level entry function with this
signature:

```text
fn main() {
    // Program body.
}
```

`main` takes no arguments and must return `()`. The omitted return annotation
defaults to `()`; spelling it explicitly as `fn main() -> ()` is equivalent.
Normal completion returns process status zero; a panic returns a nonzero status.
A missing entry function or a different `main` signature is a compile-time error.
