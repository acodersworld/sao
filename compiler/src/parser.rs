use std::iter::Peekable;

use crate::ast::{
    AssignmentOperator, BinaryOperator, Expression, ExpressionKind, LiteralKind, UnaryOperator,
};
use crate::lexer::{LexError, Span, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseErrorKind {
    ExpectedExpression {
        found: TokenKind,
    },
    ExpectedToken {
        expected: TokenKind,
        found: TokenKind,
    },
    UnexpectedToken {
        found: TokenKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrontendError {
    Lexical(LexError),
    Syntax(ParseError),
}

impl From<LexError> for FrontendError {
    fn from(error: LexError) -> Self {
        Self::Lexical(error)
    }
}

impl From<ParseError> for FrontendError {
    fn from(error: ParseError) -> Self {
        Self::Syntax(error)
    }
}

pub type ParseResult<T = Expression> = Result<T, FrontendError>;

/// Parses one complete expression token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_expression<I>(tokens: I) -> ParseResult
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens);
    let expression = parser.expression(LOWEST_BINDING_POWER)?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    Ok(expression)
}

struct Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    tokens: Peekable<I>,
    last_end: usize,
}

impl<I> Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    fn new(tokens: I) -> Self {
        Self {
            tokens: tokens.peekable(),
            last_end: 0,
        }
    }

    fn expression(&mut self, minimum_binding_power: u8) -> ParseResult {
        let mut left = self.prefix()?;

        loop {
            left = match self.current()?.kind {
                TokenKind::LeftParen => self.call(left)?,
                TokenKind::Dot => self.member_access(left)?,
                TokenKind::LeftBracket => self.index(left)?,
                TokenKind::Question => self.try_expression(left)?,
                _ => break,
            };
        }

        while let Some(binding_power) = infix_binding_power(self.current()?.kind) {
            if binding_power.left_binding_power < minimum_binding_power {
                break;
            }

            self.advance()?;
            let right = self.expression(binding_power.right_binding_power)?;
            let span = Span::new(left.span.start, right.span.end);

            let kind = match binding_power.operator {
                InfixOperator::Binary(operator) => ExpressionKind::Binary {
                    left: Box::new(left),
                    operator,
                    right: Box::new(right),
                },
                InfixOperator::Assignment(operator) => ExpressionKind::Assignment {
                    target: Box::new(left),
                    operator,
                    value: Box::new(right),
                },
            };

            left = Expression::new(kind, span);
        }

        Ok(left)
    }

    fn call(&mut self, callee: Expression) -> ParseResult {
        self.expect(TokenKind::LeftParen)?;
        let mut arguments = Vec::new();

        if self.current()?.kind != TokenKind::RightParen {
            loop {
                arguments.push(self.expression(LOWEST_BINDING_POWER)?);

                if self.current()?.kind != TokenKind::Comma {
                    break;
                }

                self.advance()?;

                if self.current()?.kind == TokenKind::RightParen {
                    break;
                }
            }
        }

        let right_parenthesis = self.expect(TokenKind::RightParen)?;
        let span = Span::new(callee.span.start, right_parenthesis.span.end);

        Ok(Expression::new(
            ExpressionKind::Call {
                callee: Box::new(callee),
                arguments,
            },
            span,
        ))
    }

    fn member_access(&mut self, object: Expression) -> ParseResult {
        self.expect(TokenKind::Dot)?;
        let member = self.expect(TokenKind::Identifier)?;
        let span = Span::new(object.span.start, member.span.end);

        Ok(Expression::new(
            ExpressionKind::MemberAccess {
                object: Box::new(object),
                member: member.span,
            },
            span,
        ))
    }

    fn index(&mut self, object: Expression) -> ParseResult {
        self.expect(TokenKind::LeftBracket)?;
        let index = self.expression(LOWEST_BINDING_POWER)?;
        let right_bracket = self.expect(TokenKind::RightBracket)?;
        let span = Span::new(object.span.start, right_bracket.span.end);

        Ok(Expression::new(
            ExpressionKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            },
            span,
        ))
    }

    fn try_expression(&mut self, expression: Expression) -> ParseResult {
        let question = self.expect(TokenKind::Question)?;
        let span = Span::new(expression.span.start, question.span.end);

        Ok(Expression::new(
            ExpressionKind::Try {
                expression: Box::new(expression),
            },
            span,
        ))
    }

    fn prefix(&mut self) -> ParseResult {
        let token = self.current()?;

        match token.kind {
            TokenKind::Minus => {
                self.advance()?;
                let operand = self.expression(prefix_binding_power(token.kind))?;
                let span = Span::new(token.span.start, operand.span.end);

                Ok(Expression::new(
                    ExpressionKind::Unary {
                        operator: UnaryOperator::Negate,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            TokenKind::Bang | TokenKind::Tilde => {
                self.advance()?;
                let operand = self.expression(prefix_binding_power(token.kind))?;
                let span = Span::new(token.span.start, operand.span.end);
                let operator = match token.kind {
                    TokenKind::Bang => UnaryOperator::LogicalNot,
                    TokenKind::Tilde => UnaryOperator::BitwiseNot,
                    _ => unreachable!(),
                };

                Ok(Expression::new(
                    ExpressionKind::Unary {
                        operator,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            TokenKind::LeftParen => self.group(),
            TokenKind::Identifier => self.primary(ExpressionKind::Identifier),
            TokenKind::SelfValue => self.primary(ExpressionKind::SelfValue),
            TokenKind::IntegerLiteral => self.literal(LiteralKind::Integer),
            TokenKind::FloatLiteral => self.literal(LiteralKind::Float),
            TokenKind::True => self.literal(LiteralKind::Boolean(true)),
            TokenKind::False => self.literal(LiteralKind::Boolean(false)),
            TokenKind::CharacterLiteral => self.literal(LiteralKind::Character),
            TokenKind::StringLiteral => self.literal(LiteralKind::String),
            TokenKind::None => self.literal(LiteralKind::None),
            _ => Err(ParseError {
                kind: ParseErrorKind::ExpectedExpression { found: token.kind },
                span: token.span,
            }
            .into()),
        }
    }

    fn group(&mut self) -> ParseResult {
        let left_parenthesis = self.advance()?;

        if self.current()?.kind == TokenKind::RightParen {
            let right_parenthesis = self.advance()?;
            return Ok(Expression::new(
                ExpressionKind::Literal(LiteralKind::Unit),
                Span::new(left_parenthesis.span.start, right_parenthesis.span.end),
            ));
        }

        let expression = self.expression(LOWEST_BINDING_POWER)?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;

        Ok(Expression::new(
            ExpressionKind::Group(Box::new(expression)),
            Span::new(left_parenthesis.span.start, right_parenthesis.span.end),
        ))
    }

    fn primary(&mut self, kind: ExpressionKind) -> ParseResult {
        let token = self.advance()?;
        Ok(Expression::new(kind, token.span))
    }

    fn literal(&mut self, kind: LiteralKind) -> ParseResult {
        self.primary(ExpressionKind::Literal(kind))
    }

    fn expect(&mut self, expected: TokenKind) -> ParseResult<Token> {
        let token = self.current()?;

        if token.kind == expected {
            self.advance()
        } else {
            Err(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected,
                    found: token.kind,
                },
                span: token.span,
            }
            .into())
        }
    }

    fn current(&mut self) -> ParseResult<Token> {
        match self.tokens.peek().copied() {
            Some(result) => result.map_err(FrontendError::Lexical),
            None => Ok(self.synthetic_eof()),
        }
    }

    fn advance(&mut self) -> ParseResult<Token> {
        match self.tokens.next() {
            Some(Ok(token)) => {
                self.last_end = token.span.end;
                Ok(token)
            }
            Some(Err(error)) => Err(error.into()),
            None => Ok(self.synthetic_eof()),
        }
    }

    fn synthetic_eof(&self) -> Token {
        Token::new(TokenKind::Eof, Span::new(self.last_end, self.last_end))
    }
}

const LOWEST_BINDING_POWER: u8 = 0;
const ASSIGNMENT_BINDING_POWER: u8 = 1;
const LOGICAL_OR_BINDING_POWER: u8 = 3;
const LOGICAL_AND_BINDING_POWER: u8 = 5;
const BITWISE_OR_BINDING_POWER: u8 = 7;
const BITWISE_XOR_BINDING_POWER: u8 = 9;
const BITWISE_AND_BINDING_POWER: u8 = 11;
const EQUALITY_BINDING_POWER: u8 = 13;
const RELATIONAL_BINDING_POWER: u8 = 15;
const SHIFT_BINDING_POWER: u8 = 17;
const ADDITIVE_BINDING_POWER: u8 = 19;
const MULTIPLICATIVE_BINDING_POWER: u8 = 21;
const PREFIX_BINDING_POWER: u8 = 23;

const fn prefix_binding_power(kind: TokenKind) -> u8 {
    match kind {
        TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde => PREFIX_BINDING_POWER,
        _ => LOWEST_BINDING_POWER,
    }
}

#[derive(Clone, Copy)]
enum InfixOperator {
    Binary(BinaryOperator),
    Assignment(AssignmentOperator),
}

struct InfixBindingPower {
    /// Determines whether the operator can bind to the expression on its left.
    left_binding_power: u8,
    /// Sets the minimum binding power while parsing the operator's right operand.
    right_binding_power: u8,
    operator: InfixOperator,
}

impl InfixBindingPower {
    const fn left_associative(left_binding_power: u8, operator: InfixOperator) -> Self {
        Self {
            left_binding_power,
            right_binding_power: left_binding_power + 1,
            operator,
        }
    }

    const fn right_associative(binding_power: u8, operator: InfixOperator) -> Self {
        Self {
            left_binding_power: binding_power,
            right_binding_power: binding_power,
            operator,
        }
    }

    const fn binary(binding_power: u8, operator: BinaryOperator) -> Self {
        Self::left_associative(binding_power, InfixOperator::Binary(operator))
    }

    const fn assignment(binding_power: u8, operator: AssignmentOperator) -> Self {
        Self::right_associative(binding_power, InfixOperator::Assignment(operator))
    }
}

const fn infix_binding_power(kind: TokenKind) -> Option<InfixBindingPower> {
    match kind {
        TokenKind::Assign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Assign,
        )),
        TokenKind::PlusAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Add,
        )),
        TokenKind::MinusAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Subtract,
        )),
        TokenKind::StarAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Multiply,
        )),
        TokenKind::SlashAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Divide,
        )),
        TokenKind::PercentAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::Remainder,
        )),
        TokenKind::AmpersandAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::BitwiseAnd,
        )),
        TokenKind::CaretAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::BitwiseXor,
        )),
        TokenKind::PipeAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::BitwiseOr,
        )),
        TokenKind::ShiftLeftAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::ShiftLeft,
        )),
        TokenKind::ShiftRightAssign => Some(InfixBindingPower::assignment(
            ASSIGNMENT_BINDING_POWER,
            AssignmentOperator::ShiftRight,
        )),
        TokenKind::LogicalOr => Some(InfixBindingPower::binary(
            LOGICAL_OR_BINDING_POWER,
            BinaryOperator::LogicalOr,
        )),
        TokenKind::LogicalAnd => Some(InfixBindingPower::binary(
            LOGICAL_AND_BINDING_POWER,
            BinaryOperator::LogicalAnd,
        )),
        TokenKind::Pipe => Some(InfixBindingPower::binary(
            BITWISE_OR_BINDING_POWER,
            BinaryOperator::BitwiseOr,
        )),
        TokenKind::Caret => Some(InfixBindingPower::binary(
            BITWISE_XOR_BINDING_POWER,
            BinaryOperator::BitwiseXor,
        )),
        TokenKind::Ampersand => Some(InfixBindingPower::binary(
            BITWISE_AND_BINDING_POWER,
            BinaryOperator::BitwiseAnd,
        )),
        TokenKind::Equal => Some(InfixBindingPower::binary(
            EQUALITY_BINDING_POWER,
            BinaryOperator::Equal,
        )),
        TokenKind::NotEqual => Some(InfixBindingPower::binary(
            EQUALITY_BINDING_POWER,
            BinaryOperator::NotEqual,
        )),
        TokenKind::Less => Some(InfixBindingPower::binary(
            RELATIONAL_BINDING_POWER,
            BinaryOperator::Less,
        )),
        TokenKind::LessEqual => Some(InfixBindingPower::binary(
            RELATIONAL_BINDING_POWER,
            BinaryOperator::LessEqual,
        )),
        TokenKind::Greater => Some(InfixBindingPower::binary(
            RELATIONAL_BINDING_POWER,
            BinaryOperator::Greater,
        )),
        TokenKind::GreaterEqual => Some(InfixBindingPower::binary(
            RELATIONAL_BINDING_POWER,
            BinaryOperator::GreaterEqual,
        )),
        TokenKind::ShiftLeft => Some(InfixBindingPower::binary(
            SHIFT_BINDING_POWER,
            BinaryOperator::ShiftLeft,
        )),
        TokenKind::ShiftRight => Some(InfixBindingPower::binary(
            SHIFT_BINDING_POWER,
            BinaryOperator::ShiftRight,
        )),
        TokenKind::Plus => Some(InfixBindingPower::binary(
            ADDITIVE_BINDING_POWER,
            BinaryOperator::Add,
        )),
        TokenKind::Minus => Some(InfixBindingPower::binary(
            ADDITIVE_BINDING_POWER,
            BinaryOperator::Subtract,
        )),
        TokenKind::Star => Some(InfixBindingPower::binary(
            MULTIPLICATIVE_BINDING_POWER,
            BinaryOperator::Multiply,
        )),
        TokenKind::Slash => Some(InfixBindingPower::binary(
            MULTIPLICATIVE_BINDING_POWER,
            BinaryOperator::Divide,
        )),
        TokenKind::Percent => Some(InfixBindingPower::binary(
            MULTIPLICATIVE_BINDING_POWER,
            BinaryOperator::Remainder,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{LexErrorKind, Lexer};

    fn parse(source: &str) -> ParseResult {
        parse_expression(Lexer::new(source))
    }

    fn integer(span: Span) -> Expression {
        Expression::new(ExpressionKind::Literal(LiteralKind::Integer), span)
    }

    fn binary(
        left: Expression,
        operator: BinaryOperator,
        right: Expression,
        span: Span,
    ) -> Expression {
        Expression::new(
            ExpressionKind::Binary {
                left: Box::new(left),
                operator,
                right: Box::new(right),
            },
            span,
        )
    }

    #[test]
    fn parses_primary_expressions() {
        for source in [
            "name", "self", "()", "42", "1.5", "true", "false", "'a'", "\"text\"", "none",
        ] {
            assert!(parse(source).is_ok(), "failed to parse {source}");
        }
    }

    #[test]
    fn multiplication_binds_more_tightly_than_addition() {
        assert_eq!(
            parse("1 + 2 * 3"),
            Ok(binary(
                integer(Span::new(0, 1)),
                BinaryOperator::Add,
                binary(
                    integer(Span::new(4, 5)),
                    BinaryOperator::Multiply,
                    integer(Span::new(8, 9)),
                    Span::new(4, 9),
                ),
                Span::new(0, 9),
            ))
        );
    }

    #[test]
    fn binary_operators_associate_to_the_left() {
        assert_eq!(
            parse("8 - 3 - 1"),
            Ok(binary(
                binary(
                    integer(Span::new(0, 1)),
                    BinaryOperator::Subtract,
                    integer(Span::new(4, 5)),
                    Span::new(0, 5),
                ),
                BinaryOperator::Subtract,
                integer(Span::new(8, 9)),
                Span::new(0, 9),
            ))
        );
    }

    #[test]
    fn parentheses_override_precedence() {
        let expression = parse("(1 + 2) * 3").expect("expression should parse");
        let ExpressionKind::Binary { left, operator, .. } = expression.kind else {
            panic!("expected a binary expression");
        };

        assert_eq!(operator, BinaryOperator::Multiply);
        assert!(matches!(left.kind, ExpressionKind::Group(_)));
        assert_eq!(left.span, Span::new(0, 7));
        assert_eq!(expression.span, Span::new(0, 11));
    }

    #[test]
    fn unary_negation_binds_more_tightly_than_multiplication() {
        let expression = parse("-1 * 2").expect("expression should parse");
        let ExpressionKind::Binary { left, operator, .. } = expression.kind else {
            panic!("expected a binary expression");
        };

        assert_eq!(operator, BinaryOperator::Multiply);
        assert!(matches!(
            left.kind,
            ExpressionKind::Unary {
                operator: UnaryOperator::Negate,
                ..
            }
        ));
    }

    #[test]
    fn parses_calls_with_empty_and_multiple_argument_lists() {
        let expression = parse("run()").expect("empty call should parse");
        let ExpressionKind::Call { arguments, .. } = expression.kind else {
            panic!("expected a call expression");
        };
        assert!(arguments.is_empty());
        assert_eq!(expression.span, Span::new(0, 5));

        let expression =
            parse("run(first, second + third,)").expect("call with a trailing comma should parse");
        let ExpressionKind::Call { arguments, .. } = expression.kind else {
            panic!("expected a call expression");
        };
        assert_eq!(arguments.len(), 2);
        assert!(matches!(
            arguments[1].kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
    }

    #[test]
    fn parses_member_access_and_indexing() {
        let expression = parse("items[1 + 2].length").expect("postfix expression should parse");
        let ExpressionKind::MemberAccess { object, member } = expression.kind else {
            panic!("expected member access at the root");
        };
        assert_eq!(member, Span::new(13, 19));

        let ExpressionKind::Index { index, .. } = object.kind else {
            panic!("expected indexing before member access");
        };
        assert!(matches!(
            index.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
    }

    #[test]
    fn postfix_expressions_chain_from_left_to_right() {
        let expression = parse("service.worker(1)[0]?").expect("postfix chain should parse");
        let ExpressionKind::Try { expression } = expression.kind else {
            panic!("expected Try at the root");
        };
        let ExpressionKind::Index { object, .. } = expression.kind else {
            panic!("expected indexing before Try");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before indexing");
        };
        assert!(matches!(callee.kind, ExpressionKind::MemberAccess { .. }));
    }

    #[test]
    fn postfix_expressions_bind_more_tightly_than_prefix_and_infix_operators() {
        let expression = parse("-value.member + other").expect("expression should parse");
        let ExpressionKind::Binary {
            left,
            operator: BinaryOperator::Add,
            ..
        } = expression.kind
        else {
            panic!("expected addition at the root");
        };
        let ExpressionKind::Unary { operand, .. } = left.kind else {
            panic!("expected unary negation on the left");
        };
        assert!(matches!(operand.kind, ExpressionKind::MemberAccess { .. }));
    }

    #[test]
    fn reports_incomplete_postfix_expressions() {
        assert_eq!(
            parse("value."),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span: Span::new(6, 6),
            }))
        );

        assert_eq!(
            parse("items[]"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::RightBracket,
                },
                span: Span::new(6, 7),
            }))
        );
    }

    #[test]
    fn parses_all_prefix_operators() {
        for (source, expected) in [
            ("-value", UnaryOperator::Negate),
            ("!value", UnaryOperator::LogicalNot),
            ("~value", UnaryOperator::BitwiseNot),
        ] {
            let expression = parse(source).expect("prefix expression should parse");
            let ExpressionKind::Unary { operator, .. } = expression.kind else {
                panic!("expected a unary expression for {source}");
            };

            assert_eq!(operator, expected, "incorrect operator for {source}");
        }
    }

    #[test]
    fn parses_all_binary_operators() {
        for (source, expected) in [
            ("a + b", BinaryOperator::Add),
            ("a - b", BinaryOperator::Subtract),
            ("a * b", BinaryOperator::Multiply),
            ("a / b", BinaryOperator::Divide),
            ("a % b", BinaryOperator::Remainder),
            ("a << b", BinaryOperator::ShiftLeft),
            ("a >> b", BinaryOperator::ShiftRight),
            ("a < b", BinaryOperator::Less),
            ("a <= b", BinaryOperator::LessEqual),
            ("a > b", BinaryOperator::Greater),
            ("a >= b", BinaryOperator::GreaterEqual),
            ("a == b", BinaryOperator::Equal),
            ("a != b", BinaryOperator::NotEqual),
            ("a & b", BinaryOperator::BitwiseAnd),
            ("a ^ b", BinaryOperator::BitwiseXor),
            ("a | b", BinaryOperator::BitwiseOr),
            ("a && b", BinaryOperator::LogicalAnd),
            ("a || b", BinaryOperator::LogicalOr),
        ] {
            let expression = parse(source).expect("binary expression should parse");
            let ExpressionKind::Binary { operator, .. } = expression.kind else {
                panic!("expected a binary expression for {source}");
            };

            assert_eq!(operator, expected, "incorrect operator for {source}");
        }
    }

    #[test]
    fn parses_all_assignment_operators() {
        for (source, expected) in [
            ("a = b", AssignmentOperator::Assign),
            ("a += b", AssignmentOperator::Add),
            ("a -= b", AssignmentOperator::Subtract),
            ("a *= b", AssignmentOperator::Multiply),
            ("a /= b", AssignmentOperator::Divide),
            ("a %= b", AssignmentOperator::Remainder),
            ("a &= b", AssignmentOperator::BitwiseAnd),
            ("a ^= b", AssignmentOperator::BitwiseXor),
            ("a |= b", AssignmentOperator::BitwiseOr),
            ("a <<= b", AssignmentOperator::ShiftLeft),
            ("a >>= b", AssignmentOperator::ShiftRight),
        ] {
            let expression = parse(source).expect("assignment expression should parse");
            let ExpressionKind::Assignment { operator, .. } = expression.kind else {
                panic!("expected an assignment expression for {source}");
            };

            assert_eq!(operator, expected, "incorrect operator for {source}");
        }
    }

    #[test]
    fn observes_precedence_across_operator_groups() {
        let expression = parse("a || b && c").expect("logical expression should parse");
        let ExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            right,
            ..
        } = expression.kind
        else {
            panic!("expected logical OR at the root");
        };
        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::LogicalAnd,
                ..
            }
        ));

        let expression = parse("a < b + c").expect("relational expression should parse");
        let ExpressionKind::Binary {
            operator: BinaryOperator::Less,
            right,
            ..
        } = expression.kind
        else {
            panic!("expected comparison at the root");
        };
        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
    }

    #[test]
    fn assignments_are_right_associative_and_bind_loosely() {
        let expression = parse("a = b = c || d").expect("assignment expression should parse");
        let ExpressionKind::Assignment { value, .. } = expression.kind else {
            panic!("expected an assignment at the root");
        };
        let ExpressionKind::Assignment { value, .. } = value.kind else {
            panic!("expected a nested assignment on the right");
        };

        assert!(matches!(
            value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::LogicalOr,
                ..
            }
        ));
    }

    #[test]
    fn reports_missing_expressions_and_parentheses() {
        assert_eq!(
            parse("1 +"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                span: Span::new(3, 3),
            }))
        );

        assert_eq!(
            parse("(1 + 2"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Eof,
                },
                span: Span::new(6, 6),
            }))
        );
    }

    #[test]
    fn rejects_tokens_after_the_expression() {
        assert_eq!(
            parse("1 2"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::IntegerLiteral,
                },
                span: Span::new(2, 3),
            }))
        );
    }

    #[test]
    fn returns_lexical_errors_from_the_iterator() {
        assert_eq!(
            parse("\"bad\\q\""),
            Err(FrontendError::Lexical(LexError {
                kind: LexErrorKind::InvalidEscape,
                span: Span::new(4, 6),
            }))
        );
    }
}
