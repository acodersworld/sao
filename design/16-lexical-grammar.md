# 16. Lexical grammar

SAO source is UTF-8, but identifiers and literal values are deliberately ASCII.
Byte offsets are used for token and diagnostic spans.

## 16.1 Identifiers and keywords

Identifiers match `[A-Za-z_][A-Za-z0-9_]*` and are case-sensitive. The reserved
words are:

```text
fn lambda struct interface const mut self if else loop while for in break continue
return is co defer true false none int float bool char string bytes
Queue Vector Map Error
```

The capitalized names `Queue`, `Vector`, `Map`, and `Error` are reserved
compiler-known parameterized type constructors. They are tokens distinct from
ordinary identifiers, so declarations and bindings cannot reuse them.

Whitespace is insignificant. Spaces, horizontal tabs, form feeds, carriage
returns, and line feeds are skipped.

## 16.2 Comments

`//` begins a comment that continues to the next carriage return, line feed, or
end of file. `/* ... */` comments may be nested. Comment text may contain
Unicode even though identifiers and literals may not. An unclosed block comment
is a lexical error covering the complete comment.

## 16.3 Numeric literals

Integer literals contain one or more decimal digits. Leading zeroes do not
change the base. A leading sign is always a separate unary operator token.

A floating-point literal is either decimal digits followed by a decimal point
and one or more decimal digits, or decimal digits followed by an exponent. Both
forms may have an exponent introduced by `e` or `E`, followed by an optional
sign and one or more digits. Therefore `1.0`, `1e3`, and `1.0e-3` are floats;
`.5` and `1.` are not.

The longest punctuation match distinguishes the `..` and `..=` range
delimiters, and the `..` slice delimiter, from member-access `.`. Consequently,
`0..10` is an integer, `..`, and another integer rather than a floating-point
literal. The `..=` token is valid for inclusive range headers but is rejected
as an inclusive slice delimiter.

Digit separators and binary, octal, or hexadecimal integer literals are not
initially supported. Range checking and numeric conversion belong to semantic
analysis rather than lexing.

## 16.4 String and character literals

Strings use double quotes and characters use single quotes. Literal contents
must decode to ASCII bytes from 0 through 127. Character literals must decode to
exactly one character.

The initial escape set is:

```text
\\  \"  \'  \n  \r  \t  \0  \xNN
```

`NN` is exactly two hexadecimal digits and must denote an ASCII value. Raw
control characters are rejected; newlines terminate an unclosed literal.
Literal tokens retain their original source spelling. Decoding and allocating
their runtime value occurs in a later compiler phase.

## 16.5 Errors and recovery

The lazy lexer yields `Result<Token, LexError>` items and continues after an
error. A malformed quoted literal and an unclosed block comment are each
consumed as one error item so the next iteration resumes at a meaningful
boundary. The eager `lex` helper returns all tokens on success or all lexical
errors on failure. Every token carries a half-open byte span, and a final
zero-length `Eof` token is always emitted.

Parser entry points intentionally stop at the first lexical or syntax error.
Whole-program parsing does not currently collect multiple diagnostics or return
a partial syntax tree; declaration-level and statement-level recovery are
deferred.
