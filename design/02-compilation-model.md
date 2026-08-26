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
and per-function escape analysis. Escape analysis consumes typed value-category
and capture metadata before typed IR is emitted. Each validation rule belongs
to the earliest stage with enough information to decide it; validation is not a
separate catch-all pass.

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

## Source-module identity

One source file is one SAO module. During a compilation session, the source
registry assigns each source module a monotonically increasing `ModuleId` and
shares its immutable text through `Arc<str>`. Module ID zero is reserved for
compiler-provided prelude definitions; ordinary source modules begin at one.
These IDs are session-local identities, not stable cross-build keys.

AST identities and source locations are qualified by their module:

```rust
NodeId { module_id, node_id }
Span { module_id, start, end }
```

The node portion is allocated sequentially within one module. Consequently,
the same numeric node ID in two modules denotes two different AST nodes, while
a span can always be routed back to the source module whose byte offsets it
uses.

Source registration does not select a root or entry module. A runnable build's
future compilation/module-graph state records exactly one `entry_module_id`;
registration order has no entry-point meaning. There is therefore no reserved
root module ID.

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
