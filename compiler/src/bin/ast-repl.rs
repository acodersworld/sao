use std::io::{self, BufRead, Write};

use sao_compiler::lexer::{Lexer, Span};
use sao_compiler::parser::{
    FrontendError, ParseError, ParseErrorKind, parse_expression, parse_statement, parse_type,
};
use sao_compiler::pretty::{format_expression, format_statement, format_type};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut line = String::new();

    writeln!(output, "SAO AST REPL")?;
    writeln!(
        output,
        "Enter an expression, or use :stmt STATEMENT or :type TYPE. Type :help for commands."
    )?;

    loop {
        write!(output, "sao> ")?;
        output.flush()?;

        line.clear();
        if input.read_line(&mut line)? == 0 {
            writeln!(output)?;
            break;
        }

        let source = line.trim();
        if source.is_empty() {
            continue;
        }

        match source {
            ":help" => print_help(&mut output)?,
            ":quit" | ":q" => break,
            _ => {
                if let Some(type_source) = command_argument(source, ":type") {
                    if type_source.is_empty() {
                        writeln!(output, "usage: :type TYPE")?;
                    } else {
                        print_type(&mut output, type_source)?;
                    }
                } else if let Some(statement_source) = command_argument(source, ":stmt") {
                    if statement_source.is_empty() {
                        writeln!(output, "usage: :stmt STATEMENT")?;
                    } else {
                        print_statement(&mut output, statement_source)?;
                    }
                } else if let Some(expression_source) = command_argument(source, ":expr") {
                    if expression_source.is_empty() {
                        writeln!(output, "usage: :expr EXPRESSION")?;
                    } else {
                        print_expression(&mut output, expression_source)?;
                    }
                } else if source.starts_with(':') {
                    writeln!(output, "unknown command; type :help for commands")?;
                } else {
                    print_expression(&mut output, source)?;
                }
            }
        }
    }

    Ok(())
}

fn command_argument<'source>(source: &'source str, command: &str) -> Option<&'source str> {
    let rest = source.strip_prefix(command)?;

    if rest.is_empty() {
        return Some(rest);
    }

    if rest.chars().next().is_some_and(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn print_help(output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "Commands:")?;
    writeln!(output, "  :expr EXPRESSION  parse an expression")?;
    writeln!(output, "  :stmt STATEMENT   parse a statement")?;
    writeln!(output, "  :type TYPE        parse a type expression")?;
    writeln!(output, "  :help             show this help")?;
    writeln!(output, "  :quit or :q       exit")?;
    Ok(())
}

fn print_expression(output: &mut impl Write, source: &str) -> io::Result<()> {
    match parse_expression(Lexer::new(source)) {
        Ok(expression) => writeln!(output, "{}", format_expression(source, &expression)),
        Err(error) => print_error(output, source, error),
    }
}

fn print_statement(output: &mut impl Write, source: &str) -> io::Result<()> {
    match parse_statement(Lexer::new(source)) {
        Ok(statement) => writeln!(output, "{}", format_statement(source, &statement)),
        Err(error) => print_error(output, source, error),
    }
}

fn print_type(output: &mut impl Write, source: &str) -> io::Result<()> {
    match parse_type(Lexer::new(source)) {
        Ok(type_syntax) => writeln!(output, "{}", format_type(source, &type_syntax)),
        Err(error) => print_error(output, source, error),
    }
}

fn print_error(output: &mut impl Write, source: &str, error: FrontendError) -> io::Result<()> {
    let (message, span) = match error {
        FrontendError::Lexical(error) => (error.kind.to_string(), error.span),
        FrontendError::Syntax(error) => (syntax_error_message(error), error.span),
    };

    writeln!(output, "error: {message}")?;
    writeln!(output, "  {source}")?;

    let start = character_count(source, Span::new(0, span.start));
    let length = character_count(source, span).max(1);
    writeln!(output, "  {}{}", " ".repeat(start), "^".repeat(length))
}

fn syntax_error_message(error: ParseError) -> String {
    match error.kind {
        ParseErrorKind::ExpectedExpression { found } => {
            format!("expected an expression, found {found:?}")
        }
        ParseErrorKind::ExpectedType { found } => {
            format!("expected a type, found {found:?}")
        }
        ParseErrorKind::ExpectedElseBranch { found } => {
            format!("expected a block or if after else, found {found:?}")
        }
        ParseErrorKind::ExpectedRangeOperator { found } => {
            format!("expected .. or ..= in for range, found {found:?}")
        }
        ParseErrorKind::ExpectedTopLevelDeclaration { found } => {
            format!("expected a top-level declaration, found {found:?}")
        }
        ParseErrorKind::RangeBoundRequiresGrouping => {
            "complex range bounds must be parenthesized".to_owned()
        }
        ParseErrorKind::ExpectedToken { expected, found } => {
            format!("expected {expected:?}, found {found:?}")
        }
        ParseErrorKind::UnexpectedToken { found } => format!("unexpected token {found:?}"),
    }
}

fn character_count(source: &str, span: Span) -> usize {
    source
        .get(span.start..span.end)
        .map(|text| text.chars().count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_arguments_require_a_command_boundary() {
        assert_eq!(command_argument(":type int", ":type"), Some("int"));
        assert_eq!(command_argument(":type", ":type"), Some(""));
        assert_eq!(command_argument(":typescript", ":type"), None);
        assert_eq!(
            command_argument(":stmt const value = 1;", ":stmt"),
            Some("const value = 1;")
        );
        assert_eq!(command_argument(":statement", ":stmt"), None);
    }

    #[test]
    fn describes_invalid_else_branches() {
        assert_eq!(
            syntax_error_message(ParseError {
                kind: ParseErrorKind::ExpectedElseBranch {
                    found: sao_compiler::lexer::TokenKind::Identifier,
                },
                span: Span::new(0, 5),
            }),
            "expected a block or if after else, found Identifier"
        );
    }

    #[test]
    fn describes_missing_range_operators() {
        assert_eq!(
            syntax_error_message(ParseError {
                kind: ParseErrorKind::ExpectedRangeOperator {
                    found: sao_compiler::lexer::TokenKind::LeftBrace,
                },
                span: Span::new(0, 1),
            }),
            "expected .. or ..= in for range, found LeftBrace"
        );
    }

    #[test]
    fn describes_invalid_top_level_syntax() {
        assert_eq!(
            syntax_error_message(ParseError {
                kind: ParseErrorKind::ExpectedTopLevelDeclaration {
                    found: sao_compiler::lexer::TokenKind::Const,
                },
                span: Span::new(0, 5),
            }),
            "expected a top-level declaration, found Const"
        );
    }

    #[test]
    fn describes_ungrouped_complex_range_bounds() {
        assert_eq!(
            syntax_error_message(ParseError {
                kind: ParseErrorKind::RangeBoundRequiresGrouping,
                span: Span::new(0, 1),
            }),
            "complex range bounds must be parenthesized"
        );
    }
}
