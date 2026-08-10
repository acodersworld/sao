# 4. Expression-oriented blocks

Blocks are expressions. The final expression without a semicolon is the value of
the block:

```text
const distance = {
    const x = 10.0;
    const y = 20.0;

    sqrt(x * x + y * y)
};
```

A semicolon discards the expression's value:

```text
{ 42 }     // type: int
{ 42; }    // type: ()
```

Function bodies use the same rule. The final expression must be compatible with
the explicitly declared return type, or with `()` when the annotation is omitted:

```text
fn absolute(value: int) -> int {
    if value < 0 {
        -value
    } else {
        value
    }
}
```

Early `return` remains available:

```text
fn divide(left: float, right: float) -> float {
    if right == 0.0 {
        return 0.0;
    }

    left / right
}
```

`if` is an expression. When no expected type is provided, its branches must
resolve to the same single type:

```text
const value = if condition {
    42
} else {
    3.14
};

// Type error: inference cannot choose one type.
```

Different branch types require an explicit type that accepts them:

```text
const value: int | float = if condition {
    42
} else {
    3.14
};
```

The annotation supplies the expected union type; the compiler does not invent
the union. The same rule applies to other expression forms with multiple result
paths.
