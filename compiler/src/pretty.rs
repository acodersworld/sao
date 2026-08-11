use std::fmt::{Arguments, Write};

use crate::ast::{
    AnonymousStructField, AnonymousStructMember, Block, ConditionalElse, Declaration, Expression,
    ExpressionKind, Function, FunctionParameter, FunctionParameterKind, InterfaceDeclaration,
    InterfaceMethodRequirement, Program, Statement, StatementKind, StructDeclaration, StructField,
    StructFieldInitializer, StructMember, TypeKind, TypeSyntax,
};
use crate::lexer::Span;

/// Formats one complete program as an indented syntax tree.
#[must_use]
pub fn format_program(source: &str, program: &Program) -> String {
    let mut output = String::new();
    format_program_into(&mut output, source, program, 0);
    output.truncate(output.len().saturating_sub(1));
    output
}

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

/// Formats a statement as an indented syntax tree.
#[must_use]
pub fn format_statement(source: &str, statement: &Statement) -> String {
    let mut output = String::new();
    format_statement_into(&mut output, source, statement, 0);
    output.truncate(output.len().saturating_sub(1));
    output
}

fn format_program_into(output: &mut String, source: &str, program: &Program, depth: usize) {
    line(
        output,
        depth,
        format_args!("Program {}", location(program.span)),
    );
    line(output, depth + 1, format_args!("declarations:"));

    if program.declarations.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, declaration) in program.declarations.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_declaration_into(output, source, declaration, depth + 3);
    }
}

fn format_declaration_into(
    output: &mut String,
    source: &str,
    declaration: &Declaration,
    depth: usize,
) {
    match declaration {
        Declaration::Function(function) => format_function_into(output, source, function, depth),
        Declaration::Struct(structure) => {
            format_struct_declaration_into(output, source, structure, depth);
        }
        Declaration::Interface(interface) => {
            format_interface_declaration_into(output, source, interface, depth);
        }
    }
}

fn format_interface_declaration_into(
    output: &mut String,
    source: &str,
    interface: &InterfaceDeclaration,
    depth: usize,
) {
    line(
        output,
        depth,
        format_args!(
            "Interface {:?} {}",
            text(source, interface.name),
            location(interface.span)
        ),
    );
    line(output, depth + 1, format_args!("requirements:"));

    if interface.requirements.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, requirement) in interface.requirements.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_interface_requirement_into(output, source, requirement, depth + 3);
    }
}

fn format_interface_requirement_into(
    output: &mut String,
    source: &str,
    requirement: &InterfaceMethodRequirement,
    depth: usize,
) {
    line(
        output,
        depth,
        format_args!(
            "MethodRequirement {:?} {}",
            text(source, requirement.name),
            location(requirement.span)
        ),
    );
    parameter_list(output, source, &requirement.parameters, depth);
    optional_return_type(output, source, requirement.return_type.as_ref(), depth);
}

fn format_struct_declaration_into(
    output: &mut String,
    source: &str,
    structure: &StructDeclaration,
    depth: usize,
) {
    line(
        output,
        depth,
        format_args!(
            "Struct {:?} {}",
            text(source, structure.name),
            location(structure.span)
        ),
    );
    line(output, depth + 1, format_args!("members:"));

    if structure.members.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, member) in structure.members.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        match member {
            StructMember::Field(field) => {
                format_struct_field_into(output, source, field, depth + 3);
            }
            StructMember::Method(method) => {
                format_function_into(output, source, method, depth + 3);
            }
        }
    }
}

fn format_struct_field_into(output: &mut String, source: &str, field: &StructField, depth: usize) {
    line(
        output,
        depth,
        format_args!(
            "Field {:?} {}",
            text(source, field.name),
            location(field.span)
        ),
    );
    child_type(output, source, "type", &field.type_annotation, depth);
}

fn format_statement_into(output: &mut String, source: &str, statement: &Statement, depth: usize) {
    let span = statement.span;

    match &statement.kind {
        StatementKind::Binding {
            mutability,
            name,
            type_annotation,
            initializer,
        } => {
            line(
                output,
                depth,
                format_args!(
                    "Binding {mutability:?} {:?} {}",
                    text(source, *name),
                    location(span)
                ),
            );
            if let Some(type_annotation) = type_annotation {
                child_type(output, source, "type", type_annotation, depth);
            }
            child_expression(output, source, "initializer", initializer, depth);
        }
        StatementKind::Expression(expression) => {
            line(
                output,
                depth,
                format_args!("ExpressionStatement {}", location(span)),
            );
            child_expression(output, source, "expression", expression, depth);
        }
        StatementKind::Function(function) => {
            format_function_into(output, source, function, depth);
        }
        StatementKind::Break(value) => {
            line(output, depth, format_args!("Break {}", location(span)));
            optional_expression(output, source, "value", value.as_ref(), depth);
        }
        StatementKind::Continue => {
            line(output, depth, format_args!("Continue {}", location(span)));
        }
        StatementKind::Return(value) => {
            line(output, depth, format_args!("Return {}", location(span)));
            optional_expression(output, source, "value", value.as_ref(), depth);
        }
    }
}

fn format_function_into(output: &mut String, source: &str, function: &Function, depth: usize) {
    line(
        output,
        depth,
        format_args!(
            "Function {:?} {}",
            text(source, function.name),
            location(function.span)
        ),
    );
    parameter_list(output, source, &function.parameters, depth);
    optional_return_type(output, source, function.return_type.as_ref(), depth);
    child_block(output, source, "body", &function.body, depth);
}

fn parameter_list(
    output: &mut String,
    source: &str,
    parameters: &[FunctionParameter],
    depth: usize,
) {
    line(output, depth + 1, format_args!("parameters:"));

    if parameters.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, parameter) in parameters.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_parameter_into(output, source, parameter, depth + 3);
    }
}

fn format_parameter_into(
    output: &mut String,
    source: &str,
    parameter: &FunctionParameter,
    depth: usize,
) {
    match &parameter.kind {
        FunctionParameterKind::Named {
            name,
            type_annotation,
        } => {
            line(
                output,
                depth,
                format_args!(
                    "Parameter {:?} {:?} {}",
                    parameter.mutability,
                    text(source, *name),
                    location(parameter.span)
                ),
            );
            child_type(output, source, "type", type_annotation, depth);
        }
        FunctionParameterKind::Receiver { .. } => {
            line(
                output,
                depth,
                format_args!(
                    "Parameter {:?} Self {}",
                    parameter.mutability,
                    location(parameter.span)
                ),
            );
        }
    }
}

fn optional_return_type(
    output: &mut String,
    source: &str,
    return_type: Option<&TypeSyntax>,
    depth: usize,
) {
    line(output, depth + 1, format_args!("return_type:"));
    if let Some(return_type) = return_type {
        format_type_into(output, source, return_type, depth + 2);
    } else {
        line(output, depth + 2, format_args!("(default ())"));
    }
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
        ExpressionKind::Block(block) => format_block_into(output, source, block, depth),
        ExpressionKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            line(output, depth, format_args!("If {}", location(span)));
            child_expression(output, source, "condition", condition, depth);
            child_block(output, source, "then_branch", then_branch, depth);
            line(output, depth + 1, format_args!("else_branch:"));
            match else_branch {
                Some(ConditionalElse::Block(block)) => {
                    format_block_into(output, source, block, depth + 2);
                }
                Some(ConditionalElse::If(conditional)) => {
                    format_expression_into(output, source, conditional, depth + 2);
                }
                None => line(output, depth + 2, format_args!("(none)")),
            }
        }
        ExpressionKind::Loop { body } => {
            line(output, depth, format_args!("Loop {}", location(span)));
            child_block(output, source, "body", body, depth);
        }
        ExpressionKind::While {
            condition,
            body,
            else_branch,
        } => {
            line(output, depth, format_args!("While {}", location(span)));
            child_expression(output, source, "condition", condition, depth);
            child_block(output, source, "body", body, depth);
            line(output, depth + 1, format_args!("else_branch:"));
            if let Some(else_branch) = else_branch {
                format_block_into(output, source, else_branch, depth + 2);
            } else {
                line(output, depth + 2, format_args!("(none)"));
            }
        }
        ExpressionKind::RangeFor {
            binding,
            start,
            end,
            inclusivity,
            body,
            else_branch,
        } => {
            line(
                output,
                depth,
                format_args!(
                    "RangeFor {inclusivity:?} {:?} {}",
                    text(source, *binding),
                    location(span)
                ),
            );
            child_expression(output, source, "start", start, depth);
            child_expression(output, source, "end", end, depth);
            child_block(output, source, "body", body, depth);
            line(output, depth + 1, format_args!("else_branch:"));
            if let Some(else_branch) = else_branch {
                format_block_into(output, source, else_branch, depth + 2);
            } else {
                line(output, depth + 2, format_args!("(none)"));
            }
        }
        ExpressionKind::Lambda {
            parameters,
            return_type,
            body,
        } => {
            line(output, depth, format_args!("Lambda {}", location(span)));
            parameter_list(output, source, parameters, depth);
            optional_return_type(output, source, return_type.as_ref(), depth);
            child_block(output, source, "body", body, depth);
        }
        ExpressionKind::PrimitiveConversion { target, value } => {
            line(
                output,
                depth,
                format_args!("PrimitiveConversion {target:?} {}", location(span)),
            );
            child_expression(output, source, "value", value, depth);
        }
        ExpressionKind::StructConstruction { name, fields } => {
            line(
                output,
                depth,
                format_args!(
                    "StructConstruction {:?} {}",
                    text(source, *name),
                    location(span)
                ),
            );
            struct_initializer_list(output, source, fields, depth);
        }
        ExpressionKind::AnonymousStruct { members } => {
            line(
                output,
                depth,
                format_args!("AnonymousStruct {}", location(span)),
            );
            anonymous_struct_member_list(output, source, members, depth);
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
        ExpressionKind::TypeTest { value, type_syntax } => {
            line(output, depth, format_args!("TypeTest {}", location(span)));
            child_expression(output, source, "value", value, depth);
            child_type(output, source, "type", type_syntax, depth);
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

fn struct_initializer_list(
    output: &mut String,
    source: &str,
    fields: &[StructFieldInitializer],
    depth: usize,
) {
    line(output, depth + 1, format_args!("fields:"));
    if fields.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, field) in fields.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        line(
            output,
            depth + 3,
            format_args!(
                "FieldInitializer {:?} {}",
                text(source, field.name),
                location(field.span)
            ),
        );
        child_expression(output, source, "value", &field.value, depth + 3);
    }
}

fn anonymous_struct_member_list(
    output: &mut String,
    source: &str,
    members: &[AnonymousStructMember],
    depth: usize,
) {
    line(output, depth + 1, format_args!("members:"));
    if members.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, member) in members.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        match member {
            AnonymousStructMember::Field(field) => {
                format_anonymous_struct_field_into(output, source, field, depth + 3);
            }
            AnonymousStructMember::Method(method) => {
                format_function_into(output, source, method, depth + 3);
            }
        }
    }
}

fn format_anonymous_struct_field_into(
    output: &mut String,
    source: &str,
    field: &AnonymousStructField,
    depth: usize,
) {
    line(
        output,
        depth,
        format_args!(
            "Field {:?} {}",
            text(source, field.name),
            location(field.span)
        ),
    );
    if let Some(type_annotation) = &field.type_annotation {
        child_type(output, source, "type", type_annotation, depth);
    }
    child_expression(output, source, "initializer", &field.initializer, depth);
}

fn format_block_into(output: &mut String, source: &str, block: &Block, depth: usize) {
    line(
        output,
        depth,
        format_args!("Block {}", location(block.span)),
    );
    statement_list(output, source, "statements", &block.statements, depth);
    line(output, depth + 1, format_args!("value:"));
    if let Some(value) = &block.value {
        format_expression_into(output, source, value, depth + 2);
    } else {
        line(output, depth + 2, format_args!("(none)"));
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

fn optional_expression(
    output: &mut String,
    source: &str,
    label: &str,
    expression: Option<&Expression>,
    depth: usize,
) {
    line(output, depth + 1, format_args!("{label}:"));
    if let Some(expression) = expression {
        format_expression_into(output, source, expression, depth + 2);
    } else {
        line(output, depth + 2, format_args!("(none)"));
    }
}

fn child_block(output: &mut String, source: &str, label: &str, block: &Block, depth: usize) {
    line(output, depth + 1, format_args!("{label}:"));
    format_block_into(output, source, block, depth + 2);
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

fn statement_list(
    output: &mut String,
    source: &str,
    label: &str,
    statements: &[Statement],
    depth: usize,
) {
    line(output, depth + 1, format_args!("{label}:"));

    if statements.is_empty() {
        line(output, depth + 2, format_args!("(empty)"));
        return;
    }

    for (index, statement) in statements.iter().enumerate() {
        line(output, depth + 2, format_args!("[{index}]:"));
        format_statement_into(output, source, statement, depth + 3);
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
    use crate::parser::{parse_expression, parse_program, parse_statement, parse_type};

    #[test]
    fn formats_programs() {
        let source = "fn first() {}\nfn second() {}";
        let program = parse_program(Lexer::new(source)).expect("program should parse");
        let output = format_program(source, &program);

        assert!(output.starts_with("Program @ 0..28\n  declarations:"));
        assert!(output.contains("[0]:\n      Function \"first\" @ 0..13"));
        assert!(output.contains("[1]:\n      Function \"second\" @ 14..28"));
    }

    #[test]
    fn formats_empty_programs() {
        let source = "// no declarations";
        let program = parse_program(Lexer::new(source)).expect("empty program should parse");

        assert_eq!(
            format_program(source, &program),
            "Program @ 0..18\n  declarations:\n    (empty)"
        );
    }

    #[test]
    fn formats_named_struct_declarations() {
        let source = "struct Point { x: float, fn get_x(self) -> float { self.x } }";
        let program = parse_program(Lexer::new(source)).expect("struct should parse");
        let output = format_program(source, &program);

        assert!(output.contains("Struct \"Point\" @ 0..61"));
        assert!(output.contains("Field \"x\" @ 15..24"));
        assert!(output.contains("Primitive Float @ 18..23"));
        assert!(output.contains("Function \"get_x\" @ 25..59"));
        assert!(output.contains("Parameter Const Self"));
    }

    #[test]
    fn formats_interface_declarations() {
        let source = concat!(
            "interface Writer { ",
            "fn write(mut self, data: bytes) -> int; ",
            "fn close(self);",
            " }",
        );
        let program = parse_program(Lexer::new(source)).expect("interface should parse");
        let output = format_program(source, &program);

        assert!(output.contains(&format!("Interface \"Writer\" @ 0..{}", source.len())));
        assert!(output.contains("requirements:"));
        assert!(output.contains("MethodRequirement \"write\""));
        assert!(output.contains("Parameter Mut Self"));
        assert!(output.contains("Parameter Const \"data\""));
        assert!(output.contains("Primitive Bytes"));
        assert!(output.contains("Primitive Int"));
        assert!(output.contains("MethodRequirement \"close\""));
        assert!(output.contains("(default ())"));

        let source = "interface Empty {}";
        let program = parse_program(Lexer::new(source)).expect("empty interface should parse");
        assert_eq!(
            format_program(source, &program),
            "Program @ 0..18\n  declarations:\n    [0]:\n      Interface \"Empty\" @ 0..18\n        requirements:\n          (empty)"
        );
    }

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
    fn formats_primitive_conversions() {
        let source = "string(value + 1)";
        let expression = parse_expression(Lexer::new(source)).expect("conversion should parse");

        assert_eq!(
            format_expression(source, &expression),
            "PrimitiveConversion String @ 0..17\n  value:\n    Binary Add @ 7..16\n      left:\n        Identifier \"value\" @ 7..12\n      right:\n        Literal Integer \"1\" @ 15..16"
        );
    }

    #[test]
    fn formats_named_and_anonymous_struct_expressions() {
        let source = "Point { x: 1 + 2 }";
        let expression = parse_expression(Lexer::new(source)).expect("construction should parse");
        let output = format_expression(source, &expression);

        assert!(output.starts_with("StructConstruction \"Point\" @ 0..18"));
        assert!(output.contains("FieldInitializer \"x\" @ 8..16"));
        assert!(output.contains("Binary Add @ 11..16"));

        let source = "struct { value: int = 1; fn get(self) { self.value } }";
        let expression =
            parse_expression(Lexer::new(source)).expect("anonymous struct should parse");
        let output = format_expression(source, &expression);

        assert!(output.starts_with("AnonymousStruct @ 0..54"));
        assert!(output.contains("Field \"value\" @ 9..24"));
        assert!(output.contains("Literal Integer \"1\" @ 22..23"));
        assert!(output.contains("Function \"get\" @ 25..52"));
    }

    #[test]
    fn formats_type_test_expressions() {
        let source = "result is Error<string> | none";
        let expression = parse_expression(Lexer::new(source)).expect("type test should parse");

        assert_eq!(
            format_expression(source, &expression),
            "TypeTest @ 0..30\n  value:\n    Identifier \"result\" @ 0..6\n  type:\n    Union @ 10..30\n      members:\n        [0]:\n          Named \"Error\" @ 10..23\n            arguments:\n              [0]:\n                Primitive String @ 16..22\n        [1]:\n          Primitive None @ 26..30"
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

    #[test]
    fn formats_binding_statements() {
        let source = "mut total: int = first + second;";
        let statement = parse_statement(Lexer::new(source)).expect("statement should parse");

        assert_eq!(
            format_statement(source, &statement),
            "Binding Mut \"total\" @ 0..32\n  type:\n    Primitive Int @ 11..14\n  initializer:\n    Binary Add @ 17..31\n      left:\n        Identifier \"first\" @ 17..22\n      right:\n        Identifier \"second\" @ 25..31"
        );
    }

    #[test]
    fn formats_expression_statements() {
        let source = "run();";
        let statement = parse_statement(Lexer::new(source)).expect("statement should parse");

        assert_eq!(
            format_statement(source, &statement),
            "ExpressionStatement @ 0..6\n  expression:\n    Call @ 0..5\n      callee:\n        Identifier \"run\" @ 0..3\n      arguments:\n        (empty)"
        );
    }

    #[test]
    fn formats_empty_blocks() {
        let source = "{}";
        let expression = parse_expression(Lexer::new(source)).expect("block should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Block @ 0..2\n  statements:\n    (empty)\n  value:\n    (none)"
        );
    }

    #[test]
    fn formats_blocks_with_statements_and_a_value() {
        let source = "{ const x = 1; x + 2 }";
        let expression = parse_expression(Lexer::new(source)).expect("block should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Block @ 0..22\n  statements:\n    [0]:\n      Binding Const \"x\" @ 2..14\n        initializer:\n          Literal Integer \"1\" @ 12..13\n  value:\n    Binary Add @ 15..20\n      left:\n        Identifier \"x\" @ 15..16\n      right:\n        Literal Integer \"2\" @ 19..20"
        );
    }

    #[test]
    fn formats_conditionals_without_else_branches() {
        let source = "if ready { 1 }";
        let expression = parse_expression(Lexer::new(source)).expect("conditional should parse");

        assert_eq!(
            format_expression(source, &expression),
            "If @ 0..14\n  condition:\n    Identifier \"ready\" @ 3..8\n  then_branch:\n    Block @ 9..14\n      statements:\n        (empty)\n      value:\n        Literal Integer \"1\" @ 11..12\n  else_branch:\n    (none)"
        );
    }

    #[test]
    fn formats_conditionals_with_braced_else_branches() {
        let source = "if ready { 1 } else { 2 }";
        let expression = parse_expression(Lexer::new(source)).expect("conditional should parse");

        assert_eq!(
            format_expression(source, &expression),
            "If @ 0..25\n  condition:\n    Identifier \"ready\" @ 3..8\n  then_branch:\n    Block @ 9..14\n      statements:\n        (empty)\n      value:\n        Literal Integer \"1\" @ 11..12\n  else_branch:\n    Block @ 20..25\n      statements:\n        (empty)\n      value:\n        Literal Integer \"2\" @ 22..23"
        );
    }

    #[test]
    fn formats_else_if_branches_as_nested_conditionals() {
        let source = "if a {} else if b {}";
        let expression = parse_expression(Lexer::new(source)).expect("else-if should parse");

        assert_eq!(
            format_expression(source, &expression),
            "If @ 0..20\n  condition:\n    Identifier \"a\" @ 3..4\n  then_branch:\n    Block @ 5..7\n      statements:\n        (empty)\n      value:\n        (none)\n  else_branch:\n    If @ 13..20\n      condition:\n        Identifier \"b\" @ 16..17\n      then_branch:\n        Block @ 18..20\n          statements:\n            (empty)\n          value:\n            (none)\n      else_branch:\n        (none)"
        );
    }

    #[test]
    fn formats_break_and_continue_statements() {
        let source = "break value + 1;";
        let statement = parse_statement(Lexer::new(source)).expect("break should parse");
        assert_eq!(
            format_statement(source, &statement),
            "Break @ 0..16\n  value:\n    Binary Add @ 6..15\n      left:\n        Identifier \"value\" @ 6..11\n      right:\n        Literal Integer \"1\" @ 14..15"
        );

        let source = "continue;";
        let statement = parse_statement(Lexer::new(source)).expect("continue should parse");
        assert_eq!(format_statement(source, &statement), "Continue @ 0..9");
    }

    #[test]
    fn formats_infinite_loops() {
        let source = "loop { break 42; }";
        let expression = parse_expression(Lexer::new(source)).expect("loop should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Loop @ 0..18\n  body:\n    Block @ 5..18\n      statements:\n        [0]:\n          Break @ 7..16\n            value:\n              Literal Integer \"42\" @ 13..15\n      value:\n        (none)"
        );
    }

    #[test]
    fn formats_while_loops_with_else_blocks() {
        let source = "while ready {} else { 2 }";
        let expression = parse_expression(Lexer::new(source)).expect("while loop should parse");

        assert_eq!(
            format_expression(source, &expression),
            "While @ 0..25\n  condition:\n    Identifier \"ready\" @ 6..11\n  body:\n    Block @ 12..14\n      statements:\n        (empty)\n      value:\n        (none)\n  else_branch:\n    Block @ 20..25\n      statements:\n        (empty)\n      value:\n        Literal Integer \"2\" @ 22..23"
        );
    }

    #[test]
    fn formats_exclusive_range_for_loops() {
        let source = "for i in 0..10 {}";
        let expression = parse_expression(Lexer::new(source)).expect("range loop should parse");

        assert_eq!(
            format_expression(source, &expression),
            "RangeFor Exclusive \"i\" @ 0..17\n  start:\n    Literal Integer \"0\" @ 9..10\n  end:\n    Literal Integer \"10\" @ 12..14\n  body:\n    Block @ 15..17\n      statements:\n        (empty)\n      value:\n        (none)\n  else_branch:\n    (none)"
        );
    }

    #[test]
    fn formats_inclusive_range_for_loops_with_else_blocks() {
        let source = "for i in 0..=1 {} else { 2 }";
        let expression = parse_expression(Lexer::new(source)).expect("range loop should parse");

        assert_eq!(
            format_expression(source, &expression),
            "RangeFor Inclusive \"i\" @ 0..28\n  start:\n    Literal Integer \"0\" @ 9..10\n  end:\n    Literal Integer \"1\" @ 13..14\n  body:\n    Block @ 15..17\n      statements:\n        (empty)\n      value:\n        (none)\n  else_branch:\n    Block @ 23..28\n      statements:\n        (empty)\n      value:\n        Literal Integer \"2\" @ 25..26"
        );
    }

    #[test]
    fn formats_default_unit_lambdas() {
        let source = "lambda() {}";
        let expression = parse_expression(Lexer::new(source)).expect("lambda should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Lambda @ 0..11\n  parameters:\n    (empty)\n  return_type:\n    (default ())\n  body:\n    Block @ 9..11\n      statements:\n        (empty)\n      value:\n        (none)"
        );
    }

    #[test]
    fn formats_value_returning_lambdas() {
        let source = "lambda(value: int) -> int { value }";
        let expression = parse_expression(Lexer::new(source)).expect("lambda should parse");

        assert_eq!(
            format_expression(source, &expression),
            "Lambda @ 0..35\n  parameters:\n    [0]:\n      Parameter Const \"value\" @ 7..17\n        type:\n          Primitive Int @ 14..17\n  return_type:\n    Primitive Int @ 22..25\n  body:\n    Block @ 26..35\n      statements:\n        (empty)\n      value:\n        Identifier \"value\" @ 28..33"
        );
    }

    #[test]
    fn formats_named_functions_and_return_statements() {
        let source = "fn add(left: int, mut right: int) -> int { return left + right; }";
        let statement = parse_statement(Lexer::new(source)).expect("function should parse");

        assert_eq!(
            format_statement(source, &statement),
            "Function \"add\" @ 0..65\n  parameters:\n    [0]:\n      Parameter Const \"left\" @ 7..16\n        type:\n          Primitive Int @ 13..16\n    [1]:\n      Parameter Mut \"right\" @ 18..32\n        type:\n          Primitive Int @ 29..32\n  return_type:\n    Primitive Int @ 37..40\n  body:\n    Block @ 41..65\n      statements:\n        [0]:\n          Return @ 43..63\n            value:\n              Binary Add @ 50..62\n                left:\n                  Identifier \"left\" @ 50..54\n                right:\n                  Identifier \"right\" @ 57..62\n      value:\n        (none)"
        );
    }

    #[test]
    fn formats_receivers_and_bare_returns() {
        let source = "fn stop(mut self) { return; }";
        let statement = parse_statement(Lexer::new(source)).expect("function should parse");
        let output = format_statement(source, &statement);

        assert!(output.contains("Parameter Mut Self @ 8..16"));
        assert!(output.contains("return_type:\n    (default ())"));
        assert!(output.contains("Return @ 20..27\n            value:\n              (none)"));
    }
}
