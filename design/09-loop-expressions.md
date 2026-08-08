# 9. Loop expressions

Value-producing loops are a core feature. SAO supports four loop forms:

```text
loop {
    // Infinite loop.
}

while condition {
    // Conditional loop.
}

for item in items {
    // Iterator loop.
}

for mut index = 0; index < 10; index += 1 {
    // Traditional three-clause loop.
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

## 9.2 Naturally terminating loops

`while`, iterator `for`, and three-clause `for` loops can terminate without
executing `break`. When such a loop is used to produce a non-unit value, it must
have an `else` block that supplies the natural-completion value:

```text
const admin: User | none = for user in users {
    if user.is_admin {
        break user;
    }
} else {
    none
};
```

The `else` block executes only when the loop completes naturally, not after a
`break`.

Traditional loops follow the same rule:

```text
const divisor = for mut candidate = 2;
                  candidate < value;
                  candidate += 1
{
    if value % candidate == 0 {
        break candidate;
    }
} else {
    value
};
```

## 9.3 Loop result typing

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
