# 9. Loop expressions

Value-producing loops are a core feature. The initial language supports three
loop forms:

```text
loop {
    // Infinite loop.
}

while condition {
    // Conditional loop.
}

for index in 0..10 {
    // Ascending integer range loop.
}
```

Every loop form is an expression and may produce a value with `break value`.

## 9.1 Infinite loops

An infinite loop can only complete by breaking or transferring control elsewhere:

```text
const command = loop {
    const input = read_line();

    if input != "" {
        break input;
    }
};
```

All reachable `break value` expressions associated with the loop must have
compatible types.

## 9.2 Range loops

Range loops use Rust-style exclusive and inclusive end bounds:

```text
for index in 0..10 {
    // Visits 0 through 9.
}

for index in 0..=10 {
    // Visits 0 through 10.
}
```

Both bounds are `int` expressions and are evaluated exactly once from left to
right before iteration begins. Iteration always advances upward by one. An
exclusive range is empty when its start is greater than or equal to its end; an
inclusive range is empty when its start is greater than its end. An inclusive
range ending at the maximum `int` value terminates after visiting that value and
does not overflow while advancing.

An unparenthesized bound is limited to a primary expression, its postfix chains,
or unary negation. Infix, assignment, lambda, and block-like expressions must be
parenthesized. Grouping makes the boundary between a complex end bound and the
loop body explicit:

```text
for index in -start..items.length() {}
for index in (start + offset)..(if ready { limit } else { fallback }) {}
```

The induction binding is immutable and controlled by the loop. It is introduced
without `const`, and neither `const` nor `mut` is accepted after `for`:

```text
for index in start..end {
    index = 0; // Type error: the induction binding is immutable.
}
```

The `..` and `..=` forms initially exist only in `for` headers and do not create
first-class range values. Descending ranges, configurable steps, open-ended
ranges, and traditional three-clause loops are unsupported. Collection
iteration may later reuse `for item in collection` after SAO defines iterable
types and their iteration protocol.

## 9.3 Naturally terminating loops

`while` and range `for` loops can terminate without executing `break`. When such
a loop is used to produce a non-unit value, it must have an `else` block that
supplies the natural-completion value:

```text
const divisor = for candidate in 2..value {
    if value % candidate == 0 {
        break candidate;
    }
} else {
    value
};
```

The `else` block executes only when the loop completes naturally, not after a
`break`. This includes natural completion of an empty range.

## 9.4 Loop result typing

- `break expression` contributes the expression's type to the loop result.
- Bare `break;` contributes `()`.
- The `else` block contributes its final expression's type.
- Without an expected type, all contributed values must resolve to the same
  single type.
- The compiler never synthesizes a union from differing `break` and `else`
  values.
- An explicit variable annotation or enclosing function return type may supply
  an expected union type, and every contributed value must be assignable to it.
- A naturally terminating loop without `else` has type `()`.
- A naturally terminating loop used where a non-unit value is expected requires
  `else`.
- `continue` does not contribute a result value.

SAO has no loop labels. `break` and `continue` always target the innermost
enclosing loop.
