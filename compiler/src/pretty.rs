use std::fmt::{Arguments, Write};

use crate::ast::{Expression, ExpressionKind, TypeKind, TypeSyntax};
use crate::lexer::Span;

/// Formats an expression as an indented syntax tree.
#[must_use]
pub fn format_expression(source: &str, expression: &Expression) -> String {
    let mut output = String::new();
    format_expression_into(&mut output, source, expression, 0);
    output.truncate(output.len().saturating_sub(1));
    output
}

/// Formats a type expression as an indented syntax tree.
#[must_use]
pub fn format_type(source: &str, type_syntax: &TypeSyntax) -> String {
    let mut output = String::new();
    format_type_into(&mut output, source, type_syntax, 0);
    output.truncate(output.len().saturating_sub(1));
    output
}

fn format_expression_into(
    output: &mut String,
    source: &str,
    expression: &Expression,
    depth: usize,
) {
    let span = expression.span;

    match &expression.kind {
        ExpressionKind::Identifier => line(
            output,
            depth,
            format_args!("Identifier {:?} {}", text(source, span), location(span)),
        ),
        ExpressionKind::SelfValue => line(output, depth, format_args!("Self {}", location(span))),
        ExpressionKind::Literal(kind) => line(
            output,
            depth,
            format_args!(
                "Literal {kind:?} {:?} {}",
                text(source, span),
                location(span)
            ),
        ),
        ExpressionKind::Group(inner) => {
            line(output, depth, format_args!("Group {}", location(span)));
            child_expression(output, source, "expression", inner, depth);
        }
        ExpressionKind::Call { callee, arguments } => {
            line(output, depth, format_args!("Call {}", location(span)));
            child_expression(output, source, "callee", callee, depth);
            expression_list(output, source, "arguments", arguments, depth);
        }
        ExpressionKind::MemberAccess { object, member } => {
            line(
                output,
                depth,
                format_args!(
                    "MemberAccess {:?} {}",
                    text(source, *member),
                    location(span)
                ),
            );
            child_expression(output, source, "object", object, depth);
        }
        ExpressionKind::Index { object, index } => {
            line(output, depth, format_args!("Index {}", location(span)));
            child_expression(output, source, "object", object, depth);
            child_expression(output, source, "index", index, depth);
        }
        ExpressionKind::Try { expression: inner } => {
            line(output, depth, format_args!("Try {}", location(span)));
            child_expression(output, source, "expression", inner, depth);
        }
        ExpressionKind::Unary { operator, operand } => {
            line(
                output,
                depth,
                format_args!("Unary {operator:?} {}", location(span)),
            );
            child_expression(output, source, "operand", operand, depth);
        }
        ExpressionKind::Binary {
            left,
            operator,
            right,
        } => {
            line(
                output,
                depth,
                format_args!("Binary {operator:?} {}", location(span)),
            );
            child_expression(output, source, "left", left, depth);
            child_expression(output, source, "right", right, depth);
        }
        ExpressionKind::Assignment {
            target,
            operator,
            value,
        } => {
            line(
                output,
                depth,
                format_args!("Assignment {operator:?} {}", location(span)),
            );
            child_expression(output, source, "target", target, depth);
            child_expression(output, source, "value", value, depth);
        }
    }
}

fn child_expression(
    output: &mut String,
    source: &str,
    label: &str,
    expression: &Expression,
    depth: usize,
) {
    line(output, depth + 1, format_args!("{label}:"));
    format_expression_into(output, source, expression, depth + 2);
}

fn expression_list(
    output: &mut String,
    source: &str,
    label: &str,
    expressions: &[Expression],
    depth: usize,
) {
    line(output, depth + 1, format_args!("{label}:"));

    if expressions.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, expression) in expressions.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_expression_into(output, source, expression, depth + 3);
    }
}

fn format_type_into(output: &mut String, source: &str, type_syntax: &TypeSyntax, depth: usize) {
    let span = type_syntax.span;

    match &type_syntax.kind {
        TypeKind::Primitive(primitive) => line(
            output,
            depth,
            format_args!("Primitive {primitive:?} {}", location(span)),
        ),
        TypeKind::Named { name, arguments } => {
            line(
                output,
                depth,
                format_args!("Named {:?} {}", text(source, *name), location(span)),
            );
            if !arguments.is_empty() {
                type_list(output, source, "arguments", arguments, depth);
            }
        }
        TypeKind::Mutable(inner) => {
            line(output, depth, format_args!("Mutable {}", location(span)));
            child_type(output, source, "type", inner, depth);
        }
        TypeKind::Group(inner) => {
            line(output, depth, format_args!("Group {}", location(span)));
            child_type(output, source, "type", inner, depth);
        }
        TypeKind::Callable {
            parameters,
            return_type,
        } => {
            line(output, depth, format_args!("Callable {}", location(span)));
            type_list(output, source, "parameters", parameters, depth);
            child_type(output, source, "return_type", return_type, depth);
        }
        TypeKind::Intersection { members } => {
            line(
                output,
                depth,
                format_args!("Intersection {}", location(span)),
            );
            type_list(output, source, "members", members, depth);
        }
        TypeKind::Union { members } => {
            line(output, depth, format_args!("Union {}", location(span)));
            type_list(output, source, "members", members, depth);
        }
    }
}

fn child_type(
    output: &mut String,
    source: &str,
    label: &str,
    type_syntax: &TypeSyntax,
    depth: usize,
) {
    line(output, depth + 1, format_args!("{label}:"));
    format_type_into(output, source, type_syntax, depth + 2);
}

fn type_list(output: &mut String, source: &str, label: &str, types: &[TypeSyntax], depth: usize) {
    line(output, depth + 1, format_args!("{label}:"));

    if types.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, type_syntax) in types.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_type_into(output, source, type_syntax, depth + 3);
    }
}

fn line(output: &mut String, depth: usize, arguments: Arguments<'_>) {
    for _ in 0..depth {
        output.push_str("  ");
    }

    output
        .write_fmt(arguments)
        .expect("writing to a String cannot fail");
    output.push('\n');
}

fn text(source: &str, span: Span) -> &str {
    source.get(span.start..span.end).unwrap_or("<invalid span>")
}

fn location(span: Span) -> String {
    format!("@ {}..{}", span.start, span.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::{parse_expression, parse_type};

    #[test]
    fn formats_expression_structure_and_source_text() {
        let source = "1 + value";
        let expression = parse_expression(Lexer::new(source)).expect("expression should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Binary Add @ 0..9\n  left:\n    Literal Integer \"1\" @ 0..1\n  right:\n    Identifier \"value\" @ 4..9"
        );
    }

    #[test]
    fn formats_type_lists_and_named_types() {
        let source = "Reader & Writer | none";
        let type_syntax = parse_type(Lexer::new(source)).expect("type should parse");
        let output = format_type(source, &type_syntax);

        assert!(output.starts_with("Union @ 0..22"));
        assert!(output.contains("Intersection @ 0..15"));
        assert!(output.contains("Named \"Reader\" @ 0..6"));
        assert!(output.contains("Primitive None @ 18..22"));
    }
}
