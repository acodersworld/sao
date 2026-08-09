use std::iter::Peekable;

use crate::ast::{
    AssignmentOperator, BinaryOperator, BindingMutability, Block, Expression, ExpressionKind,
    LiteralKind, PrimitiveType, Statement, StatementKind, TypeKind, TypeSyntax, UnaryOperator,
};
use crate::lexer::{LexError, Span, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseErrorKind {
    ExpectedExpression {
        found: TokenKind,
    },
    ExpectedType {
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

/// Parses one complete type-expression token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_type<I>(tokens: I) -> ParseResult<TypeSyntax>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens);
    let type_syntax = parser.type_expression()?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    Ok(type_syntax)
}

/// Parses one complete statement token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_statement<I>(tokens: I) -> ParseResult<Statement>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens);
    let statement = parser.statement()?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    Ok(statement)
}

struct Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    tokens: Peekable<I>,
    /// Holds the second `>` when type parsing splits a `>>` token that closes
    /// two nested parameterized types.
    pending: Option<Token>,
    last_end: usize,
}

impl<I> Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    fn new(tokens: I) -> Self {
        Self {
            tokens: tokens.peekable(),
            pending: None,
            last_end: 0,
        }
    }

    fn statement(&mut self) -> ParseResult<Statement> {
        match self.current()?.kind {
            TokenKind::Const => self.binding_statement(BindingMutability::Const),
            TokenKind::Mut => self.binding_statement(BindingMutability::Mut),
            _ => self.expression_statement(),
        }
    }

    fn binding_statement(&mut self, mutability: BindingMutability) -> ParseResult<Statement> {
        let keyword = self.advance()?;
        let name = self.expect(TokenKind::Identifier)?;
        let type_annotation = if self.current()?.kind == TokenKind::Colon {
            self.advance()?;
            Some(self.type_expression()?)
        } else {
            None
        };

        self.expect(TokenKind::Assign)?;
        let initializer = self.expression(LOWEST_BINDING_POWER)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Binding {
                mutability,
                name: name.span,
                type_annotation,
                initializer,
            },
            Span::new(keyword.span.start, semicolon.span.end),
        ))
    }

    fn expression_statement(&mut self) -> ParseResult<Statement> {
        let expression = self.expression(LOWEST_BINDING_POWER)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;
        let span = Span::new(expression.span.start, semicolon.span.end);
        Ok(Statement::new(StatementKind::Expression(expression), span))
    }

    fn type_expression(&mut self) -> ParseResult<TypeSyntax> {
        let first = self.intersection_type()?;

        if self.current()?.kind != TokenKind::Pipe {
            return Ok(first);
        }

        let start = first.span.start;
        let mut members = vec![first];

        while self.current()?.kind == TokenKind::Pipe {
            self.advance()?;
            members.push(self.intersection_type()?);
        }

        let end = members.last().expect("a union has members").span.end;
        Ok(TypeSyntax::new(
            TypeKind::Union { members },
            Span::new(start, end),
        ))
    }

    fn intersection_type(&mut self) -> ParseResult<TypeSyntax> {
        let first = self.prefix_type()?;

        if self.current()?.kind != TokenKind::Ampersand {
            return Ok(first);
        }

        let start = first.span.start;
        let mut members = vec![first];

        while self.current()?.kind == TokenKind::Ampersand {
            self.advance()?;
            members.push(self.prefix_type()?);
        }

        let end = members
            .last()
            .expect("an intersection has members")
            .span
            .end;
        Ok(TypeSyntax::new(
            TypeKind::Intersection { members },
            Span::new(start, end),
        ))
    }

    fn prefix_type(&mut self) -> ParseResult<TypeSyntax> {
        let token = self.current()?;

        if token.kind != TokenKind::Mut {
            return self.primary_type();
        }

        self.advance()?;
        let inner = self.prefix_type()?;
        let span = Span::new(token.span.start, inner.span.end);
        Ok(TypeSyntax::new(TypeKind::Mutable(Box::new(inner)), span))
    }

    fn primary_type(&mut self) -> ParseResult<TypeSyntax> {
        let token = self.current()?;

        match token.kind {
            TokenKind::Int => self.primitive_type(PrimitiveType::Int),
            TokenKind::Float => self.primitive_type(PrimitiveType::Float),
            TokenKind::Bool => self.primitive_type(PrimitiveType::Bool),
            TokenKind::Char => self.primitive_type(PrimitiveType::Char),
            TokenKind::String => self.primitive_type(PrimitiveType::String),
            TokenKind::Bytes => self.primitive_type(PrimitiveType::Bytes),
            TokenKind::None => self.primitive_type(PrimitiveType::None),
            TokenKind::Identifier => self.named_type(),
            TokenKind::Fn => self.callable_type(),
            TokenKind::LeftParen => self.parenthesized_type(),
            _ => Err(ParseError {
                kind: ParseErrorKind::ExpectedType { found: token.kind },
                span: token.span,
            }
            .into()),
        }
    }

    fn primitive_type(&mut self, primitive: PrimitiveType) -> ParseResult<TypeSyntax> {
        let token = self.advance()?;
        Ok(TypeSyntax::new(TypeKind::Primitive(primitive), token.span))
    }

    fn named_type(&mut self) -> ParseResult<TypeSyntax> {
        let name = self.expect(TokenKind::Identifier)?;
        let mut arguments = Vec::new();
        let mut end = name.span.end;

        if self.current()?.kind == TokenKind::Less {
            self.advance()?;
            arguments.push(self.type_expression()?);

            while self.current()?.kind == TokenKind::Comma {
                self.advance()?;

                if matches!(
                    self.current()?.kind,
                    TokenKind::Greater | TokenKind::ShiftRight
                ) {
                    break;
                }

                arguments.push(self.type_expression()?);
            }

            end = self.expect_type_argument_close()?.span.end;
        }

        Ok(TypeSyntax::new(
            TypeKind::Named {
                name: name.span,
                arguments,
            },
            Span::new(name.span.start, end),
        ))
    }

    fn callable_type(&mut self) -> ParseResult<TypeSyntax> {
        let function = self.expect(TokenKind::Fn)?;
        self.expect(TokenKind::LeftParen)?;
        let mut parameters = Vec::new();

        if self.current()?.kind != TokenKind::RightParen {
            loop {
                parameters.push(self.type_expression()?);

                if self.current()?.kind != TokenKind::Comma {
                    break;
                }

                self.advance()?;

                if self.current()?.kind == TokenKind::RightParen {
                    break;
                }
            }
        }

        self.expect(TokenKind::RightParen)?;
        self.expect(TokenKind::Arrow)?;
        let return_type = self.type_expression()?;
        let span = Span::new(function.span.start, return_type.span.end);

        Ok(TypeSyntax::new(
            TypeKind::Callable {
                parameters,
                return_type: Box::new(return_type),
            },
            span,
        ))
    }

    fn parenthesized_type(&mut self) -> ParseResult<TypeSyntax> {
        let left_parenthesis = self.expect(TokenKind::LeftParen)?;

        if self.current()?.kind == TokenKind::RightParen {
            let right_parenthesis = self.advance()?;
            return Ok(TypeSyntax::new(
                TypeKind::Primitive(PrimitiveType::Unit),
                Span::new(left_parenthesis.span.start, right_parenthesis.span.end),
            ));
        }

        let inner = self.type_expression()?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;
        Ok(TypeSyntax::new(
            TypeKind::Group(Box::new(inner)),
            Span::new(left_parenthesis.span.start, right_parenthesis.span.end),
        ))
    }

    fn expect_type_argument_close(&mut self) -> ParseResult<Token> {
        let token = self.current()?;

        match token.kind {
            TokenKind::Greater => self.advance(),
            TokenKind::ShiftRight => {
                self.advance()?;
                let first = Token::new(
                    TokenKind::Greater,
                    Span::new(token.span.start, token.span.start + 1),
                );
                self.pending = Some(Token::new(
                    TokenKind::Greater,
                    Span::new(token.span.start + 1, token.span.end),
                ));
                Ok(first)
            }
            _ => Err(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Greater,
                    found: token.kind,
                },
                span: token.span,
            }
            .into()),
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
            TokenKind::LeftBrace => self.block(),
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

    fn block(&mut self) -> ParseResult {
        let left_brace = self.expect(TokenKind::LeftBrace)?;
        let mut statements = Vec::new();
        let mut value = None;

        let right_brace = loop {
            let token = self.current()?;

            match token.kind {
                TokenKind::RightBrace => break self.advance()?,
                TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                TokenKind::Const => {
                    statements.push(self.binding_statement(BindingMutability::Const)?);
                }
                TokenKind::Mut => {
                    statements.push(self.binding_statement(BindingMutability::Mut)?);
                }
                _ => {
                    let expression = self.expression(LOWEST_BINDING_POWER)?;
                    let following = self.current()?;

                    match following.kind {
                        TokenKind::Semicolon => {
                            let semicolon = self.advance()?;
                            let span = Span::new(expression.span.start, semicolon.span.end);
                            statements
                                .push(Statement::new(StatementKind::Expression(expression), span));
                        }
                        TokenKind::RightBrace => {
                            value = Some(Box::new(expression));
                            break self.advance()?;
                        }
                        TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                        _ => {
                            return Err(ParseError {
                                kind: ParseErrorKind::ExpectedToken {
                                    expected: TokenKind::Semicolon,
                                    found: following.kind,
                                },
                                span: following.span,
                            }
                            .into());
                        }
                    }
                }
            }
        };

        let span = Span::new(left_brace.span.start, right_brace.span.end);
        Ok(Expression::new(
            ExpressionKind::Block(Block::new(statements, value, span)),
            span,
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
        if let Some(token) = self.pending {
            return Ok(token);
        }

        match self.tokens.peek().copied() {
            Some(result) => result.map_err(FrontendError::Lexical),
            None => Ok(self.synthetic_eof()),
        }
    }

    fn advance(&mut self) -> ParseResult<Token> {
        if let Some(token) = self.pending.take() {
            self.last_end = token.span.end;
            return Ok(token);
        }

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

    fn parse_type_source(source: &str) -> ParseResult<TypeSyntax> {
        parse_type(Lexer::new(source))
    }

    fn parse_statement_source(source: &str) -> ParseResult<Statement> {
        parse_statement(Lexer::new(source))
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
    fn parses_const_binding_with_an_inferred_type() {
        assert_eq!(
            parse_statement_source("const count = 10;"),
            Ok(Statement::new(
                StatementKind::Binding {
                    mutability: BindingMutability::Const,
                    name: Span::new(6, 11),
                    type_annotation: None,
                    initializer: integer(Span::new(14, 16)),
                },
                Span::new(0, 17),
            ))
        );
    }

    #[test]
    fn parses_mut_binding_with_an_explicit_type() {
        let statement = parse_statement_source("mut value: int = 1 + 2;")
            .expect("annotated mutable binding should parse");
        let StatementKind::Binding {
            mutability,
            name,
            type_annotation: Some(type_annotation),
            initializer,
        } = statement.kind
        else {
            panic!("expected an annotated binding statement");
        };

        assert_eq!(mutability, BindingMutability::Mut);
        assert_eq!(name, Span::new(4, 9));
        assert_eq!(
            type_annotation,
            TypeSyntax::new(TypeKind::Primitive(PrimitiveType::Int), Span::new(11, 14),)
        );
        assert!(matches!(
            initializer.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(initializer.span, Span::new(17, 22));
        assert_eq!(statement.span, Span::new(0, 23));
    }

    #[test]
    fn binding_initializers_reuse_the_complete_expression_parser() {
        let statement = parse_statement_source("const item = service.worker(1)[0]?;")
            .expect("binding initializer should parse");
        let StatementKind::Binding { initializer, .. } = statement.kind else {
            panic!("expected a binding statement");
        };

        let ExpressionKind::Try { expression } = initializer.kind else {
            panic!("expected Try at the initializer root");
        };
        let ExpressionKind::Index { object, .. } = expression.kind else {
            panic!("expected indexing before Try");
        };
        assert!(matches!(object.kind, ExpressionKind::Call { .. }));
    }

    #[test]
    fn parses_semicolon_terminated_expression_statements() {
        let statement =
            parse_statement_source("target += value * 2;").expect("expression should parse");
        let StatementKind::Expression(expression) = statement.kind else {
            panic!("expected an expression statement");
        };

        assert!(matches!(
            expression.kind,
            ExpressionKind::Assignment {
                operator: AssignmentOperator::Add,
                value,
                ..
            } if matches!(
                value.kind,
                ExpressionKind::Binary {
                    operator: BinaryOperator::Multiply,
                    ..
                }
            )
        ));
        assert_eq!(expression.span, Span::new(0, 19));
        assert_eq!(statement.span, Span::new(0, 20));
    }

    #[test]
    fn reports_malformed_binding_statements() {
        for (source, expected_kind, expected_span) in [
            (
                "const = 1;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Assign,
                },
                Span::new(6, 7),
            ),
            (
                "const value;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Assign,
                    found: TokenKind::Semicolon,
                },
                Span::new(11, 12),
            ),
            (
                "const value = ;",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Semicolon,
                },
                Span::new(14, 15),
            ),
            (
                "mut value: = 1;",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Assign,
                },
                Span::new(11, 12),
            ),
        ] {
            assert_eq!(
                parse_statement_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn statements_require_a_semicolon() {
        for source in ["const value = 1", "value"] {
            assert!(matches!(
                parse_statement_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::Semicolon,
                        found: TokenKind::Eof,
                    },
                    ..
                }))
            ));
        }
    }

    #[test]
    fn statement_entry_point_rejects_trailing_input() {
        assert_eq!(
            parse_statement_source("first; second;"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Identifier,
                },
                span: Span::new(7, 13),
            }))
        );
    }

    #[test]
    fn parses_empty_and_value_producing_blocks() {
        let empty_span = Span::new(0, 2);
        assert_eq!(
            parse("{}"),
            Ok(Expression::new(
                ExpressionKind::Block(Block::new(Vec::new(), None, empty_span)),
                empty_span,
            ))
        );

        let value_span = Span::new(0, 6);
        assert_eq!(
            parse("{ 42 }"),
            Ok(Expression::new(
                ExpressionKind::Block(Block::new(
                    Vec::new(),
                    Some(Box::new(integer(Span::new(2, 4)))),
                    value_span,
                )),
                value_span,
            ))
        );
    }

    #[test]
    fn a_semicolon_discards_a_blocks_last_expression() {
        let expression = parse("{ 42; }").expect("statement-ended block should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected a block expression");
        };

        assert_eq!(block.statements.len(), 1);
        assert_eq!(
            block.statements[0],
            Statement::new(
                StatementKind::Expression(integer(Span::new(2, 4))),
                Span::new(2, 5),
            )
        );
        assert_eq!(block.value, None);
        assert_eq!(block.span, Span::new(0, 7));
        assert_eq!(expression.span, block.span);
    }

    #[test]
    fn parses_statements_followed_by_a_block_value() {
        let source = "{ const x = 1; x += 2; x * 3 }";
        let expression = parse(source).expect("mixed block should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected a block expression");
        };

        assert_eq!(block.statements.len(), 2);
        assert_eq!(block.statements[0].span, Span::new(2, 14));
        assert_eq!(block.statements[1].span, Span::new(15, 22));
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Binding {
                mutability: BindingMutability::Const,
                ..
            }
        ));
        assert!(matches!(
            &block.statements[1].kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::Assignment {
                    operator: AssignmentOperator::Add,
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Binary {
                    operator: BinaryOperator::Multiply,
                    ..
                },
                ..
            })
        ));
        assert_eq!(
            block
                .value
                .as_ref()
                .expect("block should have a value")
                .span,
            Span::new(23, 28),
        );
        assert_eq!(block.span, Span::new(0, source.len()));
        assert_eq!(expression.span, block.span);
    }

    #[test]
    fn blocks_nest_and_compose_with_postfix_and_infix_expressions() {
        let expression = parse("{{ 1 }}").expect("nested block should parse");
        let ExpressionKind::Block(outer) = expression.kind else {
            panic!("expected an outer block");
        };
        assert!(matches!(
            outer.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Block(_),
                ..
            })
        ));

        let expression = parse("{ service }().member").expect("postfix block should parse");
        let ExpressionKind::MemberAccess { object, .. } = expression.kind else {
            panic!("expected member access");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before member access");
        };
        assert!(matches!(callee.kind, ExpressionKind::Block(_)));

        let expression = parse("1 + { 2 }").expect("infix block should parse");
        let ExpressionKind::Binary { right, .. } = expression.kind else {
            panic!("expected a binary expression");
        };
        assert!(matches!(right.kind, ExpressionKind::Block(_)));
    }

    #[test]
    fn blocks_parse_as_binding_initializers() {
        let statement = parse_statement_source("const result = { const value = 1; value };")
            .expect("block initializer should parse");
        let StatementKind::Binding { initializer, .. } = statement.kind else {
            panic!("expected a binding statement");
        };
        let ExpressionKind::Block(block) = initializer.kind else {
            panic!("expected a block initializer");
        };

        assert_eq!(block.statements.len(), 1);
        assert!(block.value.is_some());
    }

    #[test]
    fn reports_missing_block_separators_and_stray_semicolons() {
        assert_eq!(
            parse("{ first second }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Identifier,
                },
                span: Span::new(8, 14),
            }))
        );

        assert_eq!(
            parse("{ ; }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Semicolon,
                },
                span: Span::new(2, 3),
            }))
        );
    }

    #[test]
    fn reports_unclosed_blocks() {
        for source in ["{", "{ 42", "{ 42;"] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::RightBrace,
                        found: TokenKind::Eof,
                    },
                    span: Span::new(source.len(), source.len()),
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn complete_expression_entry_point_rejects_input_after_a_block() {
        assert_eq!(
            parse("{} value"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Identifier,
                },
                span: Span::new(3, 8),
            }))
        );
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

    #[test]
    fn parses_primitive_and_named_types() {
        for source in ["int", "float", "bool", "char", "string", "bytes", "none"] {
            assert!(
                parse_type_source(source).is_ok(),
                "failed to parse {source}"
            );
        }

        assert_eq!(
            parse_type_source("User"),
            Ok(TypeSyntax::new(
                TypeKind::Named {
                    name: Span::new(0, 4),
                    arguments: Vec::new(),
                },
                Span::new(0, 4),
            ))
        );
    }

    #[test]
    fn parses_unit_and_grouped_types() {
        assert_eq!(
            parse_type_source("()"),
            Ok(TypeSyntax::new(
                TypeKind::Primitive(PrimitiveType::Unit),
                Span::new(0, 2),
            ))
        );

        let type_syntax = parse_type_source("(int | none)").expect("grouped type should parse");
        let TypeKind::Group(inner) = type_syntax.kind else {
            panic!("expected a grouped type");
        };
        assert!(matches!(inner.kind, TypeKind::Union { .. }));
        assert_eq!(type_syntax.span, Span::new(0, 12));
    }

    #[test]
    fn parses_parameterized_and_nested_parameterized_types() {
        let type_syntax =
            parse_type_source("Map<string, Error<int | none>>").expect("named type should parse");
        let TypeKind::Named { arguments, .. } = type_syntax.kind else {
            panic!("expected a named type");
        };

        assert_eq!(arguments.len(), 2);
        let TypeKind::Named {
            arguments: error_arguments,
            ..
        } = &arguments[1].kind
        else {
            panic!("expected a nested named type");
        };
        assert_eq!(error_arguments.len(), 1);
        assert!(matches!(&error_arguments[0].kind, TypeKind::Union { .. }));
    }

    #[test]
    fn mutable_qualifier_applies_to_the_following_union_member() {
        let type_syntax =
            parse_type_source("mut User | none").expect("mutable union type should parse");
        let TypeKind::Union { members } = type_syntax.kind else {
            panic!("expected a union type");
        };

        assert_eq!(members.len(), 2);
        assert!(matches!(&members[0].kind, TypeKind::Mutable(_)));
        assert!(matches!(
            &members[1].kind,
            TypeKind::Primitive(PrimitiveType::None)
        ));
    }

    #[test]
    fn parses_callable_types() {
        let type_syntax = parse_type_source("fn(int, mut User,) -> string | none")
            .expect("callable type should parse");
        let TypeKind::Callable {
            parameters,
            return_type,
        } = type_syntax.kind
        else {
            panic!("expected a callable type");
        };

        assert_eq!(parameters.len(), 2);
        assert!(matches!(&parameters[1].kind, TypeKind::Mutable(_)));
        assert!(matches!(return_type.kind, TypeKind::Union { .. }));
    }

    #[test]
    fn intersections_bind_more_tightly_than_unions() {
        let type_syntax = parse_type_source("A | B & C | D").expect("combined type should parse");
        let TypeKind::Union { members } = type_syntax.kind else {
            panic!("expected a union type");
        };

        assert_eq!(members.len(), 3);
        let TypeKind::Intersection {
            members: intersection_members,
        } = &members[1].kind
        else {
            panic!("expected an intersection in the union");
        };
        assert_eq!(intersection_members.len(), 2);
    }

    #[test]
    fn direct_union_and_intersection_chains_use_member_lists() {
        let union = parse_type_source("A | B | C").expect("union should parse");
        let TypeKind::Union { members } = union.kind else {
            panic!("expected a union type");
        };
        assert_eq!(members.len(), 3);

        let intersection = parse_type_source("A & B & C").expect("intersection should parse");
        let TypeKind::Intersection { members } = intersection.kind else {
            panic!("expected an intersection type");
        };
        assert_eq!(members.len(), 3);
    }

    #[test]
    fn reports_incomplete_types() {
        assert_eq!(
            parse_type_source("int |"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedType {
                    found: TokenKind::Eof,
                },
                span: Span::new(5, 5),
            }))
        );

        assert_eq!(
            parse_type_source("Error<int"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Greater,
                    found: TokenKind::Eof,
                },
                span: Span::new(9, 9),
            }))
        );
    }
}
