use std::iter::Peekable;

use crate::ast::{
    AssignmentOperator, BinaryOperator, BindingMutability, Block, ConditionalElse, Declaration,
    Expression, ExpressionKind, Function, FunctionParameter, FunctionParameterKind, LiteralKind,
    PrimitiveType, Program, RangeInclusivity, Statement, StatementKind, TypeKind, TypeSyntax,
    UnaryOperator,
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
    ExpectedElseBranch {
        found: TokenKind,
    },
    ExpectedRangeOperator {
        found: TokenKind,
    },
    ExpectedTopLevelDeclaration {
        found: TokenKind,
    },
    RangeBoundRequiresGrouping,
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

/// Parses one complete source program.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_program<I>(tokens: I) -> ParseResult<Program>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    Parser::new(tokens).program()
}

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

    fn program(&mut self) -> ParseResult<Program> {
        let mut declarations = Vec::new();

        loop {
            let token = self.current()?;
            if token.kind == TokenKind::Eof {
                return Ok(Program::new(declarations, Span::new(0, token.span.end)));
            }

            declarations.push(self.declaration()?);
        }
    }

    fn declaration(&mut self) -> ParseResult<Declaration> {
        let token = self.current()?;
        match token.kind {
            TokenKind::Fn => Ok(Declaration::Function(self.function()?)),
            _ => Err(ParseError {
                kind: ParseErrorKind::ExpectedTopLevelDeclaration { found: token.kind },
                span: token.span,
            }
            .into()),
        }
    }

    fn statement(&mut self) -> ParseResult<Statement> {
        match self.current()?.kind {
            TokenKind::Fn => self.function_statement(),
            TokenKind::Const => self.binding_statement(BindingMutability::Const),
            TokenKind::Mut => self.binding_statement(BindingMutability::Mut),
            TokenKind::Break => self.break_statement(),
            TokenKind::Continue => self.continue_statement(),
            TokenKind::Return => self.return_statement(),
            _ => self.expression_statement(),
        }
    }

    fn function_statement(&mut self) -> ParseResult<Statement> {
        let function = self.function()?;
        let span = function.span;
        Ok(Statement::new(StatementKind::Function(function), span))
    }

    fn function(&mut self) -> ParseResult<Function> {
        let keyword = self.expect(TokenKind::Fn)?;
        let name = self.expect(TokenKind::Identifier)?;
        let parameters = self.function_parameters(true)?;
        let return_type = self.optional_return_type()?;
        let body = self.block()?;
        let span = Span::new(keyword.span.start, body.span.end);

        Ok(Function::new(
            name.span,
            parameters,
            return_type,
            body,
            span,
        ))
    }

    fn function_parameters(&mut self, allow_receiver: bool) -> ParseResult<Vec<FunctionParameter>> {
        self.expect(TokenKind::LeftParen)?;
        let mut parameters = Vec::new();

        if self.current()?.kind != TokenKind::RightParen {
            loop {
                parameters.push(self.function_parameter(allow_receiver)?);

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
        Ok(parameters)
    }

    fn optional_return_type(&mut self) -> ParseResult<Option<TypeSyntax>> {
        if self.current()?.kind == TokenKind::Arrow {
            self.advance()?;
            Ok(Some(self.type_expression()?))
        } else {
            Ok(None)
        }
    }

    fn function_parameter(&mut self, allow_receiver: bool) -> ParseResult<FunctionParameter> {
        let first = self.current()?;
        let (mutability, start) = if first.kind == TokenKind::Mut {
            self.advance()?;
            (BindingMutability::Mut, first.span.start)
        } else {
            (BindingMutability::Const, first.span.start)
        };
        let parameter = self.current()?;

        if allow_receiver && parameter.kind == TokenKind::SelfValue {
            let receiver = self.advance()?;
            return Ok(FunctionParameter::new(
                mutability,
                FunctionParameterKind::Receiver {
                    name: receiver.span,
                },
                Span::new(start, receiver.span.end),
            ));
        }

        let name = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::Colon)?;
        let type_annotation = self.type_expression()?;
        let span = Span::new(start, type_annotation.span.end);

        Ok(FunctionParameter::new(
            mutability,
            FunctionParameterKind::Named {
                name: name.span,
                type_annotation,
            },
            span,
        ))
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
        let span = if self.current()?.kind == TokenKind::Semicolon {
            let semicolon = self.advance()?;
            Span::new(expression.span.start, semicolon.span.end)
        } else if expression_may_omit_statement_semicolon(&expression)
            && self.current()?.kind == TokenKind::Eof
        {
            expression.span
        } else {
            let semicolon = self.expect(TokenKind::Semicolon)?;
            Span::new(expression.span.start, semicolon.span.end)
        };
        Ok(Statement::new(StatementKind::Expression(expression), span))
    }

    fn break_statement(&mut self) -> ParseResult<Statement> {
        let keyword = self.expect(TokenKind::Break)?;
        let token = self.current()?;
        let value = match token.kind {
            TokenKind::Semicolon => None,
            TokenKind::Eof | TokenKind::RightBrace => {
                return Err(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::Semicolon,
                        found: token.kind,
                    },
                    span: token.span,
                }
                .into());
            }
            _ => Some(self.expression(LOWEST_BINDING_POWER)?),
        };
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Break(value),
            Span::new(keyword.span.start, semicolon.span.end),
        ))
    }

    fn continue_statement(&mut self) -> ParseResult<Statement> {
        let keyword = self.expect(TokenKind::Continue)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;
        Ok(Statement::new(
            StatementKind::Continue,
            Span::new(keyword.span.start, semicolon.span.end),
        ))
    }

    fn return_statement(&mut self) -> ParseResult<Statement> {
        let keyword = self.expect(TokenKind::Return)?;
        let token = self.current()?;
        let value = match token.kind {
            TokenKind::Semicolon => None,
            TokenKind::Eof | TokenKind::RightBrace => {
                return Err(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::Semicolon,
                        found: token.kind,
                    },
                    span: token.span,
                }
                .into());
            }
            _ => Some(self.expression(LOWEST_BINDING_POWER)?),
        };
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Return(value),
            Span::new(keyword.span.start, semicolon.span.end),
        ))
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
            let (kind, span) = match binding_power.operator {
                InfixOperator::Binary(operator) => {
                    let right = self.expression(binding_power.right_binding_power)?;
                    let span = Span::new(left.span.start, right.span.end);
                    (
                        ExpressionKind::Binary {
                            left: Box::new(left),
                            operator,
                            right: Box::new(right),
                        },
                        span,
                    )
                }
                InfixOperator::Assignment(operator) => {
                    let right = self.expression(binding_power.right_binding_power)?;
                    let span = Span::new(left.span.start, right.span.end);
                    (
                        ExpressionKind::Assignment {
                            target: Box::new(left),
                            operator,
                            value: Box::new(right),
                        },
                        span,
                    )
                }
                InfixOperator::TypeTest => {
                    let type_syntax = self.type_expression()?;
                    let span = Span::new(left.span.start, type_syntax.span.end);
                    (
                        ExpressionKind::TypeTest {
                            value: Box::new(left),
                            type_syntax,
                        },
                        span,
                    )
                }
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
            TokenKind::LeftBrace => self.block_expression(),
            TokenKind::If => self.conditional(),
            TokenKind::Loop => self.loop_expression(),
            TokenKind::While => self.while_expression(),
            TokenKind::For => self.range_for_expression(),
            TokenKind::Lambda => self.lambda_expression(),
            TokenKind::Int => self.primitive_conversion(PrimitiveType::Int),
            TokenKind::Float => self.primitive_conversion(PrimitiveType::Float),
            TokenKind::Bool => self.primitive_conversion(PrimitiveType::Bool),
            TokenKind::Char => self.primitive_conversion(PrimitiveType::Char),
            TokenKind::String => self.primitive_conversion(PrimitiveType::String),
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

    fn lambda_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::Lambda)?;
        let parameters = self.function_parameters(false)?;
        let return_type = self.optional_return_type()?;
        let body = self.block()?;
        let span = Span::new(keyword.span.start, body.span.end);

        Ok(Expression::new(
            ExpressionKind::Lambda {
                parameters,
                return_type,
                body,
            },
            span,
        ))
    }

    fn primitive_conversion(&mut self, target: PrimitiveType) -> ParseResult {
        let keyword = self.advance()?;
        self.expect(TokenKind::LeftParen)?;
        let value = self.expression(LOWEST_BINDING_POWER)?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;

        Ok(Expression::new(
            ExpressionKind::PrimitiveConversion {
                target,
                value: Box::new(value),
            },
            Span::new(keyword.span.start, right_parenthesis.span.end),
        ))
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

    fn block_expression(&mut self) -> ParseResult {
        let block = self.block()?;
        let span = block.span;
        Ok(Expression::new(ExpressionKind::Block(block), span))
    }

    fn block(&mut self) -> ParseResult<Block> {
        let left_brace = self.expect(TokenKind::LeftBrace)?;
        let mut statements = Vec::new();
        let mut value = None;

        let right_brace = loop {
            let token = self.current()?;

            match token.kind {
                TokenKind::RightBrace => break self.advance()?,
                TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                TokenKind::Fn => {
                    statements.push(self.function_statement()?);
                }
                TokenKind::Const => {
                    statements.push(self.binding_statement(BindingMutability::Const)?);
                }
                TokenKind::Mut => {
                    statements.push(self.binding_statement(BindingMutability::Mut)?);
                }
                TokenKind::Break => {
                    statements.push(self.break_statement()?);
                }
                TokenKind::Continue => {
                    statements.push(self.continue_statement()?);
                }
                TokenKind::Return => {
                    statements.push(self.return_statement()?);
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
                            if expression_may_omit_statement_semicolon(&expression) {
                                let span = expression.span;
                                statements.push(Statement::new(
                                    StatementKind::Expression(expression),
                                    span,
                                ));
                            } else {
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
            }
        };

        let span = Span::new(left_brace.span.start, right_brace.span.end);
        Ok(Block::new(statements, value, span))
    }

    fn conditional(&mut self) -> ParseResult {
        let if_keyword = self.expect(TokenKind::If)?;
        let condition = self.expression(LOWEST_BINDING_POWER)?;
        let then_branch = self.block()?;
        let else_branch = if self.current()?.kind == TokenKind::Else {
            self.advance()?;
            let token = self.current()?;
            Some(match token.kind {
                TokenKind::LeftBrace => ConditionalElse::Block(self.block()?),
                TokenKind::If => ConditionalElse::If(Box::new(self.conditional()?)),
                _ => {
                    return Err(ParseError {
                        kind: ParseErrorKind::ExpectedElseBranch { found: token.kind },
                        span: token.span,
                    }
                    .into());
                }
            })
        } else {
            None
        };
        let end = match &else_branch {
            Some(ConditionalElse::Block(block)) => block.span.end,
            Some(ConditionalElse::If(conditional)) => conditional.span.end,
            None => then_branch.span.end,
        };

        Ok(Expression::new(
            ExpressionKind::If {
                condition: Box::new(condition),
                then_branch,
                else_branch,
            },
            Span::new(if_keyword.span.start, end),
        ))
    }

    fn loop_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::Loop)?;
        let body = self.block()?;
        let span = Span::new(keyword.span.start, body.span.end);
        Ok(Expression::new(ExpressionKind::Loop { body }, span))
    }

    fn while_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::While)?;
        let condition = self.expression(LOWEST_BINDING_POWER)?;
        let body = self.block()?;
        let else_branch = if self.current()?.kind == TokenKind::Else {
            self.advance()?;
            Some(self.block()?)
        } else {
            None
        };
        let end = else_branch
            .as_ref()
            .map_or(body.span.end, |branch| branch.span.end);

        Ok(Expression::new(
            ExpressionKind::While {
                condition: Box::new(condition),
                body,
                else_branch,
            },
            Span::new(keyword.span.start, end),
        ))
    }

    fn range_for_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::For)?;
        let binding = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::In)?;
        let start = self.range_bound_expression()?;
        let range_operator = self.current()?;
        let inclusivity = match range_operator.kind {
            TokenKind::DotDot => RangeInclusivity::Exclusive,
            TokenKind::DotDotEqual => RangeInclusivity::Inclusive,
            // A simple start bound stops before an infix operator. Report the
            // missing grouping instead of treating that operator as a range delimiter.
            kind if infix_binding_power(kind).is_some() => {
                return Err(ParseError {
                    kind: ParseErrorKind::RangeBoundRequiresGrouping,
                    span: range_operator.span,
                }
                .into());
            }
            _ => {
                return Err(ParseError {
                    kind: ParseErrorKind::ExpectedRangeOperator {
                        found: range_operator.kind,
                    },
                    span: range_operator.span,
                }
                .into());
            }
        };
        self.advance()?;
        let end = self.range_bound_expression()?;
        let following = self.current()?;
        // Likewise, an infix operator after a simple end bound means the full
        // bound needed grouping; it is not merely a missing loop body.
        if following.kind != TokenKind::LeftBrace && infix_binding_power(following.kind).is_some() {
            return Err(ParseError {
                kind: ParseErrorKind::RangeBoundRequiresGrouping,
                span: following.span,
            }
            .into());
        }
        let body = self.block()?;
        let else_branch = if self.current()?.kind == TokenKind::Else {
            self.advance()?;
            Some(self.block()?)
        } else {
            None
        };
        let end_span = else_branch
            .as_ref()
            .map_or(body.span.end, |branch| branch.span.end);

        Ok(Expression::new(
            ExpressionKind::RangeFor {
                binding: binding.span,
                start: Box::new(start),
                end: Box::new(end),
                inclusivity,
                body,
                else_branch,
            },
            Span::new(keyword.span.start, end_span),
        ))
    }

    fn range_bound_expression(&mut self) -> ParseResult {
        let expression = self.expression(PREFIX_BINDING_POWER)?;

        if !range_bound_is_simple(&expression) {
            return Err(ParseError {
                kind: ParseErrorKind::RangeBoundRequiresGrouping,
                span: expression.span,
            }
            .into());
        }

        Ok(expression)
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

fn expression_may_omit_statement_semicolon(expression: &Expression) -> bool {
    matches!(
        &expression.kind,
        ExpressionKind::Block(_)
            | ExpressionKind::If { .. }
            | ExpressionKind::Loop { .. }
            | ExpressionKind::While { .. }
            | ExpressionKind::RangeFor { .. }
    )
}

fn range_bound_is_simple(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::Identifier
        | ExpressionKind::SelfValue
        | ExpressionKind::Literal(_)
        | ExpressionKind::Group(_) => true,
        ExpressionKind::Call { callee, .. } => range_bound_is_simple(callee),
        ExpressionKind::MemberAccess { object, .. } | ExpressionKind::Index { object, .. } => {
            range_bound_is_simple(object)
        }
        ExpressionKind::Try { expression } => range_bound_is_simple(expression),
        ExpressionKind::PrimitiveConversion { .. } => true,
        ExpressionKind::Unary {
            operator: UnaryOperator::Negate,
            operand,
        } => range_bound_is_simple(operand),
        _ => false,
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
    TypeTest,
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
        TokenKind::Is => Some(InfixBindingPower::left_associative(
            RELATIONAL_BINDING_POWER,
            InfixOperator::TypeTest,
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

    fn parse_program_source(source: &str) -> ParseResult<Program> {
        parse_program(Lexer::new(source))
    }

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

    #[test]
    fn parses_empty_programs() {
        let source = " \n// no declarations\n";
        assert_eq!(
            parse_program_source(source),
            Ok(Program::new(Vec::new(), Span::new(0, source.len())))
        );
    }

    #[test]
    fn parses_multiple_top_level_function_declarations() {
        let source = concat!(
            "fn helper(value: int) -> int { value }\n",
            "fn main() { helper(1); }",
        );
        let program = parse_program_source(source).expect("program should parse");

        assert_eq!(program.span, Span::new(0, source.len()));
        assert_eq!(program.declarations.len(), 2);
        let Declaration::Function(helper) = &program.declarations[0];
        let Declaration::Function(main) = &program.declarations[1];
        assert_eq!(helper.name, Span::new(3, 9));
        assert_eq!(main.name, Span::new(42, 46));
        assert_eq!(helper.parameters.len(), 1);
        assert_eq!(main.body.statements.len(), 1);
    }

    #[test]
    fn rejects_non_declarations_at_top_level() {
        for (source, found, span) in [
            ("const value = 1;", TokenKind::Const, Span::new(0, 5)),
            ("run();", TokenKind::Identifier, Span::new(0, 3)),
            ("return;", TokenKind::Return, Span::new(0, 6)),
            (
                "fn first() {} 42",
                TokenKind::IntegerLiteral,
                Span::new(14, 16),
            ),
        ] {
            assert_eq!(
                parse_program_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedTopLevelDeclaration { found },
                    span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn whole_program_parser_propagates_lexical_errors() {
        assert_eq!(
            parse_program_source("fn main() {} @"),
            Err(FrontendError::Lexical(LexError {
                kind: LexErrorKind::UnexpectedCharacter,
                span: Span::new(13, 14),
            }))
        );
    }

    #[test]
    fn parses_empty_main_function_declarations() {
        let statement = parse_statement_source("fn main() {}").expect("main function should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function declaration");
        };

        assert_eq!(statement.span, Span::new(0, 12));
        assert_eq!(function.name, Span::new(3, 7));
        assert!(function.parameters.is_empty());
        assert_eq!(function.return_type, None);
        assert_eq!(
            function.body,
            Block::new(Vec::new(), None, Span::new(10, 12))
        );
    }

    #[test]
    fn parses_named_functions_with_typed_parameters() {
        let source = "fn add(left: int, mut right: int,) -> int { left + right }";
        let statement = parse_statement_source(source).expect("function should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function declaration");
        };

        assert_eq!(statement.span, Span::new(0, 58));
        assert_eq!(function.span, statement.span);
        assert_eq!(function.name, Span::new(3, 6));
        assert_eq!(function.parameters.len(), 2);
        assert_eq!(function.parameters[0].span, Span::new(7, 16));
        assert_eq!(function.parameters[0].mutability, BindingMutability::Const);
        assert!(matches!(
            &function.parameters[0].kind,
            FunctionParameterKind::Named {
                name: Span { start: 7, end: 11 },
                type_annotation: TypeSyntax {
                    kind: TypeKind::Primitive(PrimitiveType::Int),
                    span: Span { start: 13, end: 16 },
                },
            }
        ));
        assert_eq!(function.parameters[1].span, Span::new(18, 32));
        assert_eq!(function.parameters[1].mutability, BindingMutability::Mut);
        assert!(matches!(
            &function.return_type,
            Some(TypeSyntax {
                kind: TypeKind::Primitive(PrimitiveType::Int),
                span: Span { start: 38, end: 41 },
            })
        ));
        assert_eq!(function.body.span, Span::new(42, 58));
        assert!(function.body.statements.is_empty());
        assert!(matches!(
            function.body.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Binary {
                    operator: BinaryOperator::Add,
                    ..
                },
                span: Span { start: 44, end: 56 },
            })
        ));
    }

    #[test]
    fn parses_method_receivers_and_bare_returns() {
        let source = "fn rename(mut self, name: string) -> () { return; }";
        let statement =
            parse_statement_source(source).expect("method-shaped function should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function declaration");
        };

        assert_eq!(function.parameters.len(), 2);
        assert_eq!(function.parameters[0].span, Span::new(10, 18));
        assert_eq!(function.parameters[0].mutability, BindingMutability::Mut);
        assert!(matches!(
            &function.parameters[0].kind,
            FunctionParameterKind::Receiver {
                name: Span { start: 14, end: 18 }
            }
        ));
        assert_eq!(
            function
                .return_type
                .as_ref()
                .expect("return type should be explicit")
                .span,
            Span::new(37, 39)
        );
        assert_eq!(function.body.statements.len(), 1);
        assert_eq!(
            function.body.statements[0],
            Statement::new(StatementKind::Return(None), Span::new(42, 49))
        );
        assert!(function.body.value.is_none());
    }

    #[test]
    fn parses_value_bearing_return_statements() {
        assert_eq!(
            parse_statement_source("return;"),
            Ok(Statement::new(StatementKind::Return(None), Span::new(0, 7),))
        );

        let statement =
            parse_statement_source("return value + 1;").expect("value return should parse");
        assert_eq!(statement.span, Span::new(0, 17));
        let StatementKind::Return(Some(value)) = statement.kind else {
            panic!("expected a value-bearing return");
        };
        assert_eq!(value.span, Span::new(7, 16));
        assert!(matches!(
            value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
    }

    #[test]
    fn parses_nested_function_declarations() {
        let source = "{ fn double(input: int) -> int { input * 2 } double(value) }";
        let expression = parse(source).expect("nested function should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected a block expression");
        };

        assert_eq!(block.statements.len(), 1);
        assert_eq!(block.statements[0].span, Span::new(2, 44));
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Function(Function {
                name: Span { start: 5, end: 11 },
                ..
            })
        ));
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Call { .. },
                span: Span { start: 45, end: 58 },
            })
        ));
    }

    #[test]
    fn parses_empty_lambdas_with_default_unit_returns() {
        let expression = parse("lambda() {}").expect("lambda should parse");

        assert_eq!(expression.span, Span::new(0, 11));
        let ExpressionKind::Lambda {
            parameters,
            return_type,
            body,
        } = expression.kind
        else {
            panic!("expected a lambda expression");
        };
        assert!(parameters.is_empty());
        assert_eq!(return_type, None);
        assert_eq!(body, Block::new(Vec::new(), None, Span::new(9, 11)));
    }

    #[test]
    fn parses_typed_lambda_parameters_and_explicit_returns() {
        let source = "lambda(value: int, mut output: Writer,) -> int { return value; }";
        let expression = parse(source).expect("typed lambda should parse");
        let ExpressionKind::Lambda {
            parameters,
            return_type,
            body,
        } = expression.kind
        else {
            panic!("expected a lambda expression");
        };

        assert_eq!(expression.span, Span::new(0, 64));
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].mutability, BindingMutability::Const);
        assert_eq!(parameters[0].span, Span::new(7, 17));
        assert!(matches!(
            &parameters[0].kind,
            FunctionParameterKind::Named {
                name: Span { start: 7, end: 12 },
                type_annotation: TypeSyntax {
                    kind: TypeKind::Primitive(PrimitiveType::Int),
                    span: Span { start: 14, end: 17 },
                },
            }
        ));
        assert_eq!(parameters[1].mutability, BindingMutability::Mut);
        assert_eq!(parameters[1].span, Span::new(19, 37));
        assert!(matches!(
            &parameters[1].kind,
            FunctionParameterKind::Named {
                name: Span { start: 23, end: 29 },
                type_annotation: TypeSyntax {
                    kind: TypeKind::Named {
                        name: Span { start: 31, end: 37 },
                        ..
                    },
                    span: Span { start: 31, end: 37 },
                },
            }
        ));
        assert!(matches!(
            return_type,
            Some(TypeSyntax {
                kind: TypeKind::Primitive(PrimitiveType::Int),
                span: Span { start: 43, end: 46 },
            })
        ));
        assert_eq!(body.span, Span::new(47, 64));
        assert_eq!(body.statements.len(), 1);
        assert_eq!(body.statements[0].span, Span::new(49, 62));
        assert!(matches!(
            &body.statements[0].kind,
            StatementKind::Return(Some(Expression {
                kind: ExpressionKind::Identifier,
                span: Span { start: 56, end: 61 },
            }))
        ));
        assert!(body.value.is_none());
    }

    #[test]
    fn lambdas_nest_and_parse_as_binding_initializers() {
        let expression =
            parse("lambda() -> fn() -> () { lambda() {} }").expect("nested lambda should parse");
        let ExpressionKind::Lambda { body, .. } = expression.kind else {
            panic!("expected an outer lambda");
        };
        assert!(matches!(
            body.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Lambda { .. },
                ..
            })
        ));

        let expression = parse("{ lambda(value: int) -> int { value + 1 } }")
            .expect("lambda value should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected a block expression");
        };
        assert!(block.statements.is_empty());
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Lambda { .. },
                ..
            })
        ));

        let statement =
            parse_statement_source("const increment = lambda(value: int) -> int { value + 1 };")
                .expect("lambda initializer should parse");
        let StatementKind::Binding { initializer, .. } = statement.kind else {
            panic!("expected a binding statement");
        };
        assert!(matches!(initializer.kind, ExpressionKind::Lambda { .. }));
        assert_eq!(initializer.span, Span::new(18, 57));
    }

    #[test]
    fn lambdas_compose_with_postfix_and_infix_expressions() {
        let expression = parse("lambda(value: int) -> int { value }(1).member")
            .expect("postfix lambda should parse");
        let ExpressionKind::MemberAccess { object, .. } = expression.kind else {
            panic!("expected member access");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before member access");
        };
        assert!(matches!(callee.kind, ExpressionKind::Lambda { .. }));
        assert_eq!(callee.span, Span::new(0, 35));

        let expression = parse("1 + lambda() -> int { 2 }()").expect("infix lambda should parse");
        let ExpressionKind::Binary {
            operator: BinaryOperator::Add,
            right,
            ..
        } = expression.kind
        else {
            panic!("expected addition at the root");
        };
        let ExpressionKind::Call { callee, .. } = right.kind else {
            panic!("expected an immediately invoked lambda");
        };
        assert!(matches!(callee.kind, ExpressionKind::Lambda { .. }));
    }

    #[test]
    fn discarded_lambdas_require_semicolons() {
        let statement = parse_statement_source("lambda() {};")
            .expect("semicolon-terminated lambda statement should parse");
        let StatementKind::Expression(expression) = statement.kind else {
            panic!("expected an expression statement");
        };
        assert!(matches!(expression.kind, ExpressionKind::Lambda { .. }));
        assert_eq!(expression.span, Span::new(0, 11));
        assert_eq!(statement.span, Span::new(0, 12));

        assert_eq!(
            parse_statement_source("lambda() {}"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                span: Span::new(11, 11),
            }))
        );

        assert_eq!(
            parse("{ lambda() {} value }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Identifier,
                },
                span: Span::new(14, 19),
            }))
        );
    }

    #[test]
    fn reports_malformed_lambda_expressions() {
        for (source, expected_kind, expected_span) in [
            (
                "lambda",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftParen,
                    found: TokenKind::Eof,
                },
                Span::new(6, 6),
            ),
            (
                "lambda(value) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightParen,
                },
                Span::new(12, 13),
            ),
            (
                "lambda(: int) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Colon,
                },
                Span::new(7, 8),
            ),
            (
                "lambda(value:) {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::RightParen,
                },
                Span::new(13, 14),
            ),
            (
                "lambda(value: int {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::LeftBrace,
                },
                Span::new(18, 19),
            ),
            (
                "lambda() -> {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::LeftBrace,
                },
                Span::new(12, 13),
            ),
            (
                "lambda() -> int",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(15, 15),
            ),
            (
                "lambda() {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                Span::new(10, 10),
            ),
            (
                "lambda(self) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::SelfValue,
                },
                Span::new(7, 11),
            ),
            (
                "lambda(mut self) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::SelfValue,
                },
                Span::new(11, 15),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn reports_malformed_function_declarations() {
        for (source, expected_kind, expected_span) in [
            (
                "fn",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                Span::new(2, 2),
            ),
            (
                "fn name",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftParen,
                    found: TokenKind::Eof,
                },
                Span::new(7, 7),
            ),
            (
                "fn f(value) -> () {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightParen,
                },
                Span::new(10, 11),
            ),
            (
                "fn f(value:) -> () {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::RightParen,
                },
                Span::new(11, 12),
            ),
            (
                "fn f() () {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::LeftParen,
                },
                Span::new(7, 8),
            ),
            (
                "fn f() -> {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::LeftBrace,
                },
                Span::new(10, 11),
            ),
            (
                "fn f() -> ()",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(12, 12),
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
    fn return_statements_require_semicolons() {
        for (source, found, span) in [
            ("return", TokenKind::Eof, Span::new(6, 6)),
            ("return value", TokenKind::Eof, Span::new(12, 12)),
        ] {
            assert_eq!(
                parse_statement_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::Semicolon,
                        found,
                    },
                    span,
                })),
                "incorrect diagnostic for {source}",
            );
        }

        assert_eq!(
            parse("{ return }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span: Span::new(9, 10),
            }))
        );
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
    fn parses_conditionals_without_an_else_branch() {
        let expression = parse("if ready { 1 }").expect("conditional should parse");
        let ExpressionKind::If {
            condition,
            then_branch,
            else_branch,
        } = expression.kind
        else {
            panic!("expected a conditional expression");
        };

        assert!(matches!(&condition.kind, ExpressionKind::Identifier));
        assert_eq!(condition.span, Span::new(3, 8));
        assert_eq!(then_branch.span, Span::new(9, 14));
        assert_eq!(
            then_branch.value.as_deref(),
            Some(&integer(Span::new(11, 12)))
        );
        assert_eq!(else_branch, None);
        assert_eq!(expression.span, Span::new(0, 14));
    }

    #[test]
    fn parses_braced_else_branches() {
        let expression = parse("if ready { 1 } else { 2 }").expect("conditional should parse");
        let ExpressionKind::If {
            then_branch,
            else_branch: Some(ConditionalElse::Block(else_branch)),
            ..
        } = expression.kind
        else {
            panic!("expected a conditional with a braced else branch");
        };

        assert_eq!(then_branch.span, Span::new(9, 14));
        assert_eq!(else_branch.span, Span::new(20, 25));
        assert_eq!(expression.span, Span::new(0, 25));
    }

    #[test]
    fn parses_else_if_chains_recursively() {
        let source = "if first { 1 } else if second { 2 } else { 3 }";
        let expression = parse(source).expect("else-if chain should parse");
        let ExpressionKind::If {
            else_branch: Some(ConditionalElse::If(nested)),
            ..
        } = expression.kind
        else {
            panic!("expected an else-if branch");
        };
        let ExpressionKind::If {
            condition,
            else_branch: Some(ConditionalElse::Block(final_branch)),
            ..
        } = nested.kind
        else {
            panic!("expected a nested conditional with a final else block");
        };

        assert_eq!(expression.span, Span::new(0, source.len()));
        assert_eq!(nested.span, Span::new(20, source.len()));
        assert_eq!(condition.span, Span::new(23, 29));
        assert_eq!(final_branch.span, Span::new(41, 46));
    }

    #[test]
    fn conditional_conditions_reuse_expression_precedence() {
        let expression = parse("if a || b && c { 1 } else { 2 }")
            .expect("conditional with a complex condition should parse");
        let ExpressionKind::If { condition, .. } = expression.kind else {
            panic!("expected a conditional expression");
        };
        let ExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            right,
            ..
        } = condition.kind
        else {
            panic!("expected logical OR at the condition root");
        };

        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::LogicalAnd,
                ..
            }
        ));
    }

    #[test]
    fn conditionals_compose_with_postfix_and_infix_expressions() {
        let expression = parse("if ready { service } else { fallback }().member")
            .expect("postfix conditional should parse");
        let ExpressionKind::MemberAccess { object, .. } = expression.kind else {
            panic!("expected member access");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before member access");
        };
        assert!(matches!(callee.kind, ExpressionKind::If { .. }));

        let expression =
            parse("1 + if ready { 2 } else { 3 }").expect("infix conditional should parse");
        let ExpressionKind::Binary { right, .. } = expression.kind else {
            panic!("expected a binary expression");
        };
        assert!(matches!(right.kind, ExpressionKind::If { .. }));
    }

    #[test]
    fn block_like_expression_statements_may_omit_semicolons() {
        for source in ["{}", "if true {}"] {
            let statement =
                parse_statement_source(source).expect("block-like statement should parse");
            let StatementKind::Expression(expression) = statement.kind else {
                panic!("expected an expression statement");
            };

            assert!(expression_may_omit_statement_semicolon(&expression));
            assert_eq!(statement.span, expression.span);
        }

        let expression = parse("{ {} value }").expect("implicit block statement should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::Block(_),
                ..
            })
        ));
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Identifier,
                ..
            })
        ));

        let expression = parse("{ if true {} value }").expect("implicit if statement should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::If { .. },
                ..
            })
        ));
        assert!(block.value.is_some());
    }

    #[test]
    fn block_like_expressions_before_a_right_brace_remain_values() {
        let expression =
            parse("{ if true { 1 } else { 2 } }").expect("conditional value should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };

        assert!(block.statements.is_empty());
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::If { .. },
                ..
            })
        ));
    }

    #[test]
    fn semicolons_explicitly_discard_block_like_expressions() {
        let source = "{ if true {}; }";
        let expression = parse(source).expect("discarded conditional should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };

        assert_eq!(block.statements.len(), 1);
        assert!(block.value.is_none());
        assert_eq!(block.statements[0].span, Span::new(2, 13));
    }

    #[test]
    fn reports_malformed_conditionals() {
        for (source, expected_kind, expected_span) in [
            (
                "if",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                Span::new(2, 2),
            ),
            (
                "if true",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(7, 7),
            ),
            (
                "if true value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Identifier,
                },
                Span::new(8, 13),
            ),
            (
                "if true {} else value",
                ParseErrorKind::ExpectedElseBranch {
                    found: TokenKind::Identifier,
                },
                Span::new(16, 21),
            ),
            (
                "if true {} else",
                ParseErrorKind::ExpectedElseBranch {
                    found: TokenKind::Eof,
                },
                Span::new(15, 15),
            ),
            (
                "else {}",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Else,
                },
                Span::new(0, 4),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn parses_bare_and_value_bearing_break_statements() {
        assert_eq!(
            parse_statement_source("break;"),
            Ok(Statement::new(StatementKind::Break(None), Span::new(0, 6),))
        );

        let statement =
            parse_statement_source("break value + 1;").expect("valued break should parse");
        let StatementKind::Break(Some(value)) = statement.kind else {
            panic!("expected a value-bearing break");
        };
        assert!(matches!(
            value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(value.span, Span::new(6, 15));
        assert_eq!(statement.span, Span::new(0, 16));
    }

    #[test]
    fn parses_continue_statements() {
        assert_eq!(
            parse_statement_source("continue;"),
            Ok(Statement::new(StatementKind::Continue, Span::new(0, 9)))
        );
    }

    #[test]
    fn parses_infinite_loops() {
        let expression = parse("loop {}").expect("infinite loop should parse");
        let ExpressionKind::Loop { body } = expression.kind else {
            panic!("expected an infinite loop");
        };

        assert_eq!(body, Block::new(Vec::new(), None, Span::new(5, 7)));
        assert_eq!(expression.span, Span::new(0, 7));

        let expression = parse("loop { break 42; }").expect("loop with break should parse");
        let ExpressionKind::Loop { body } = expression.kind else {
            panic!("expected an infinite loop");
        };
        assert_eq!(body.statements.len(), 1);
        assert_eq!(body.statements[0].span, Span::new(7, 16));
        assert!(matches!(
            &body.statements[0].kind,
            StatementKind::Break(Some(Expression {
                kind: ExpressionKind::Literal(LiteralKind::Integer),
                ..
            }))
        ));
        assert!(body.value.is_none());
        assert_eq!(expression.span, Span::new(0, 18));
    }

    #[test]
    fn parses_while_loops_with_and_without_else_blocks() {
        let expression = parse("while ready { continue; }").expect("while loop should parse");
        let ExpressionKind::While {
            condition,
            body,
            else_branch,
        } = expression.kind
        else {
            panic!("expected a while loop");
        };

        assert!(matches!(&condition.kind, ExpressionKind::Identifier));
        assert_eq!(condition.span, Span::new(6, 11));
        assert_eq!(body.span, Span::new(12, 25));
        assert!(matches!(&body.statements[0].kind, StatementKind::Continue));
        assert_eq!(else_branch, None);
        assert_eq!(expression.span, Span::new(0, 25));

        let source = "while ready {} else { 2 }";
        let expression = parse(source).expect("while-else should parse");
        let ExpressionKind::While {
            body,
            else_branch: Some(else_branch),
            ..
        } = expression.kind
        else {
            panic!("expected a while loop with an else block");
        };
        assert_eq!(body.span, Span::new(12, 14));
        assert_eq!(else_branch.span, Span::new(20, 25));
        assert_eq!(expression.span, Span::new(0, source.len()));
    }

    #[test]
    fn while_conditions_reuse_expression_precedence() {
        let expression = parse("while a || b && c {}").expect("while loop should parse");
        let ExpressionKind::While { condition, .. } = expression.kind else {
            panic!("expected a while loop");
        };
        let ExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            right,
            ..
        } = condition.kind
        else {
            panic!("expected logical OR at the condition root");
        };
        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::LogicalAnd,
                ..
            }
        ));
    }

    #[test]
    fn parses_exclusive_range_for_loops() {
        let expression = parse("for i in 0..10 {}").expect("range loop should parse");
        let ExpressionKind::RangeFor {
            binding,
            start,
            end,
            inclusivity,
            body,
            else_branch,
        } = expression.kind
        else {
            panic!("expected a range for loop");
        };

        assert_eq!(expression.span, Span::new(0, 17));
        assert_eq!(binding, Span::new(4, 5));
        assert_eq!(start.as_ref(), &integer(Span::new(9, 10)));
        assert_eq!(end.as_ref(), &integer(Span::new(12, 14)));
        assert_eq!(inclusivity, RangeInclusivity::Exclusive);
        assert_eq!(body, Block::new(Vec::new(), None, Span::new(15, 17)));
        assert_eq!(else_branch, None);
    }

    #[test]
    fn parses_inclusive_range_for_loops_with_else_blocks() {
        let source = "for index in start..=limit { continue; } else { 42 }";
        let expression = parse(source).expect("inclusive range loop should parse");
        let ExpressionKind::RangeFor {
            binding,
            start,
            end,
            inclusivity,
            body,
            else_branch: Some(else_branch),
        } = expression.kind
        else {
            panic!("expected a range loop with an else block");
        };

        assert_eq!(expression.span, Span::new(0, 52));
        assert_eq!(binding, Span::new(4, 9));
        assert_eq!(start.span, Span::new(13, 18));
        assert_eq!(end.span, Span::new(21, 26));
        assert_eq!(inclusivity, RangeInclusivity::Inclusive);
        assert_eq!(body.span, Span::new(27, 40));
        assert!(matches!(&body.statements[0].kind, StatementKind::Continue));
        assert_eq!(else_branch.span, Span::new(46, 52));
        assert_eq!(
            else_branch.value.as_deref(),
            Some(&integer(Span::new(48, 50)))
        );
    }

    #[test]
    fn range_bounds_accept_unary_and_postfix_expressions() {
        let expression = parse("for i in -start()..limit.value {}")
            .expect("simple computed bounds should parse");
        let ExpressionKind::RangeFor { start, end, .. } = expression.kind else {
            panic!("expected a range loop");
        };

        assert_eq!(start.span, Span::new(9, 17));
        assert!(matches!(
            start.kind,
            ExpressionKind::Unary {
                operator: UnaryOperator::Negate,
                operand,
            } if matches!(operand.kind, ExpressionKind::Call { .. })
        ));
        assert_eq!(end.span, Span::new(19, 30));
        assert!(matches!(end.kind, ExpressionKind::MemberAccess { .. }));
    }

    #[test]
    fn range_bounds_accept_grouped_full_expressions() {
        let expression = parse("for i in (start - 1)..(limit + 1) {}")
            .expect("grouped infix bounds should parse");
        let ExpressionKind::RangeFor { start, end, .. } = expression.kind else {
            panic!("expected a range loop");
        };

        assert!(matches!(
            start.kind,
            ExpressionKind::Group(inner)
                if matches!(
                    inner.kind,
                    ExpressionKind::Binary {
                        operator: BinaryOperator::Subtract,
                        ..
                    }
                )
        ));
        assert!(matches!(
            end.kind,
            ExpressionKind::Group(inner)
                if matches!(
                    inner.kind,
                    ExpressionKind::Binary {
                        operator: BinaryOperator::Add,
                        ..
                    }
                )
        ));
    }

    #[test]
    fn range_bounds_accept_parenthesized_block_expressions() {
        let expression = parse("for i in ({ 0 })..({ 10 }) {}")
            .expect("parenthesized block bounds should parse");
        let ExpressionKind::RangeFor { start, end, .. } = expression.kind else {
            panic!("expected a range loop");
        };

        assert!(matches!(
            start.kind,
            ExpressionKind::Group(inner)
                if matches!(inner.kind, ExpressionKind::Block(_))
        ));
        assert!(matches!(
            end.kind,
            ExpressionKind::Group(inner)
                if matches!(inner.kind, ExpressionKind::Block(_))
        ));
    }

    #[test]
    fn range_loop_else_follows_an_else_in_the_end_bound() {
        let expression = parse("for i in 0..(if ready { 1 } else { 2 }) {} else { 3 }")
            .expect("conditional end bound and loop else should parse");
        let ExpressionKind::RangeFor {
            end,
            else_branch: Some(loop_else),
            ..
        } = expression.kind
        else {
            panic!("expected a range loop with an else block");
        };

        assert!(matches!(
            end.kind,
            ExpressionKind::Group(inner)
                if matches!(
                    inner.kind,
                    ExpressionKind::If {
                        else_branch: Some(ConditionalElse::Block(_)),
                        ..
                    }
                )
        ));
        assert_eq!(
            loop_else.value.as_deref(),
            Some(&integer(Span::new(50, 51)))
        );
    }

    #[test]
    fn range_loops_accept_existing_loop_transfers() {
        let expression = parse("for i in 0..10 { break i; continue; }")
            .expect("range loop transfers should parse");
        let ExpressionKind::RangeFor { body, .. } = expression.kind else {
            panic!("expected a range loop");
        };

        assert_eq!(body.statements.len(), 2);
        assert!(matches!(
            &body.statements[0].kind,
            StatementKind::Break(Some(Expression {
                kind: ExpressionKind::Identifier,
                ..
            }))
        ));
        assert!(matches!(&body.statements[1].kind, StatementKind::Continue));
    }

    #[test]
    fn range_loops_compose_with_postfix_and_infix_expressions() {
        let expression =
            parse("for i in 0..1 {}().member").expect("postfix range loop should parse");
        let ExpressionKind::MemberAccess { object, .. } = expression.kind else {
            panic!("expected member access");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before member access");
        };
        assert!(matches!(callee.kind, ExpressionKind::RangeFor { .. }));
        assert_eq!(callee.span, Span::new(0, 16));

        let expression =
            parse("1 + for i in 0..1 { 2 } else { 3 }").expect("infix range loop should parse");
        let ExpressionKind::Binary {
            operator: BinaryOperator::Add,
            right,
            ..
        } = expression.kind
        else {
            panic!("expected addition at the root");
        };
        assert!(matches!(right.kind, ExpressionKind::RangeFor { .. }));
    }

    #[test]
    fn range_loops_follow_block_like_statement_rules() {
        let statement = parse_statement_source("for i in 0..1 {}")
            .expect("range loop statement should parse without a semicolon");
        let StatementKind::Expression(expression) = statement.kind else {
            panic!("expected an expression statement");
        };
        assert!(expression_may_omit_statement_semicolon(&expression));
        assert_eq!(statement.span, expression.span);

        let expression = parse("{ for i in 0..1 {} value }")
            .expect("implicit range loop statement should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::RangeFor { .. },
                ..
            })
        ));
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::Identifier,
                ..
            })
        ));

        let expression = parse("{ for i in 0..1 {} }").expect("range loop value should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert!(block.statements.is_empty());
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::RangeFor { .. },
                ..
            })
        ));

        let expression = parse("{ for i in 0..1 {}; }").expect("discarded range loop should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(block.value.is_none());
        assert_eq!(block.statements[0].span, Span::new(2, 19));
    }

    #[test]
    fn reports_malformed_range_for_loops() {
        for (source, expected_kind, expected_span) in [
            (
                "for",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                Span::new(3, 3),
            ),
            (
                "for mut i in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Mut,
                },
                Span::new(4, 7),
            ),
            (
                "for const i in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Const,
                },
                Span::new(4, 9),
            ),
            (
                "for in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::In,
                },
                Span::new(4, 6),
            ),
            (
                "for i 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::In,
                    found: TokenKind::IntegerLiteral,
                },
                Span::new(6, 7),
            ),
            (
                "for i in ..1 {}",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::DotDot,
                },
                Span::new(9, 11),
            ),
            (
                "for i in {}..1 {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                Span::new(9, 11),
            ),
            (
                "for i in start + 1..limit {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                Span::new(15, 16),
            ),
            (
                "for i in 0 {}",
                ParseErrorKind::ExpectedRangeOperator {
                    found: TokenKind::LeftBrace,
                },
                Span::new(11, 12),
            ),
            (
                "for i in 0",
                ParseErrorKind::ExpectedRangeOperator {
                    found: TokenKind::Eof,
                },
                Span::new(10, 10),
            ),
            (
                "for i in 0.. {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                Span::new(13, 15),
            ),
            (
                "for i in 0..limit + 1 {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                Span::new(18, 19),
            ),
            (
                "for i in 0..if ready { 1 } else { 2 } {} else { 3 }",
                ParseErrorKind::RangeBoundRequiresGrouping,
                Span::new(12, 37),
            ),
            (
                "for i in 0..",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                Span::new(12, 12),
            ),
            (
                "for i in 0..1",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(13, 13),
            ),
            (
                "for i in 0..1 {} else if true {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::If,
                },
                Span::new(22, 24),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }

        assert_eq!(
            parse("for i in 0..1 {} trailing"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Identifier,
                },
                span: Span::new(17, 25),
            }))
        );

        assert_eq!(
            parse("0..10"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::DotDot,
                },
                span: Span::new(1, 3),
            }))
        );
    }

    #[test]
    fn loop_transfers_nest_inside_conditionals() {
        let expression = parse("loop { if ready { break 1; } continue; }")
            .expect("loop with nested transfers should parse");
        let ExpressionKind::Loop { body } = expression.kind else {
            panic!("expected an infinite loop");
        };

        assert_eq!(body.statements.len(), 2);
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::If { then_branch, .. },
            ..
        }) = &body.statements[0].kind
        else {
            panic!("expected an implicit conditional statement");
        };
        assert!(matches!(
            &then_branch.statements[0].kind,
            StatementKind::Break(Some(_))
        ));
        assert!(matches!(&body.statements[1].kind, StatementKind::Continue));
    }

    #[test]
    fn loops_compose_with_postfix_and_infix_expressions() {
        let expression = parse("loop {}().member").expect("postfix loop should parse");
        let ExpressionKind::MemberAccess { object, .. } = expression.kind else {
            panic!("expected member access");
        };
        let ExpressionKind::Call { callee, .. } = object.kind else {
            panic!("expected a call before member access");
        };
        assert!(matches!(callee.kind, ExpressionKind::Loop { .. }));

        let expression =
            parse("1 + while ready { 2 } else { 3 }").expect("infix while loop should parse");
        let ExpressionKind::Binary { right, .. } = expression.kind else {
            panic!("expected a binary expression");
        };
        assert!(matches!(right.kind, ExpressionKind::While { .. }));
    }

    #[test]
    fn loops_follow_block_like_statement_rules() {
        for source in ["loop {}", "while true {}"] {
            let statement =
                parse_statement_source(source).expect("block-like statement should parse");
            let StatementKind::Expression(expression) = statement.kind else {
                panic!("expected an expression statement");
            };
            assert!(expression_may_omit_statement_semicolon(&expression));
            assert_eq!(statement.span, expression.span);
        }

        let expression = parse("{ loop {} value }").expect("implicit loop statement should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Expression(Expression {
                kind: ExpressionKind::Loop { .. },
                ..
            })
        ));
        assert!(block.value.is_some());

        let expression = parse("{ while true {} }").expect("while value should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert!(block.statements.is_empty());
        assert!(matches!(
            block.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::While { .. },
                ..
            })
        ));

        let expression = parse("{ loop {}; }").expect("discarded loop should parse");
        let ExpressionKind::Block(block) = expression.kind else {
            panic!("expected an outer block");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(block.value.is_none());
        assert_eq!(block.statements[0].span, Span::new(2, 10));
    }

    #[test]
    fn reports_malformed_loops_and_transfers() {
        for (source, expected_kind, expected_span) in [
            (
                "loop",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(4, 4),
            ),
            (
                "while",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                Span::new(5, 5),
            ),
            (
                "while true",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                Span::new(10, 10),
            ),
            (
                "while true {} else value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Identifier,
                },
                Span::new(19, 24),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }

        for (source, expected_kind, expected_span) in [
            (
                "break",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                Span::new(5, 5),
            ),
            (
                "continue",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                Span::new(8, 8),
            ),
            (
                "continue value;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Identifier,
                },
                Span::new(9, 14),
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

        assert_eq!(
            parse("loop {} else {}"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Else,
                },
                span: Span::new(8, 12),
            }))
        );

        assert_eq!(
            parse("{ break }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span: Span::new(8, 9),
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
    fn parses_primitive_conversions() {
        for (source, expected_target) in [
            ("int(value)", PrimitiveType::Int),
            ("float(value)", PrimitiveType::Float),
            ("bool(value)", PrimitiveType::Bool),
            ("char(value)", PrimitiveType::Char),
            ("string(value)", PrimitiveType::String),
        ] {
            let expression = parse(source).expect("primitive conversion should parse");
            let ExpressionKind::PrimitiveConversion { target, value } = expression.kind else {
                panic!("expected a primitive conversion for {source}");
            };
            let value_start = source.find("value").expect("source contains value");

            assert_eq!(target, expected_target);
            assert_eq!(expression.span, Span::new(0, source.len()));
            assert_eq!(value.span, Span::new(value_start, value_start + 5));
            assert!(matches!(value.kind, ExpressionKind::Identifier));
        }
    }

    #[test]
    fn primitive_conversions_accept_full_and_nested_expressions() {
        let expression = parse("int(value + 1)").expect("full conversion argument should parse");
        let ExpressionKind::PrimitiveConversion {
            target: PrimitiveType::Int,
            value,
        } = expression.kind
        else {
            panic!("expected an int conversion");
        };
        assert!(matches!(
            value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));

        let expression = parse("string(int(value))").expect("nested conversion should parse");
        let ExpressionKind::PrimitiveConversion {
            target: PrimitiveType::String,
            value,
        } = expression.kind
        else {
            panic!("expected a string conversion");
        };
        assert!(matches!(
            value.kind,
            ExpressionKind::PrimitiveConversion {
                target: PrimitiveType::Int,
                ..
            }
        ));
    }

    #[test]
    fn primitive_conversions_compose_with_other_expressions() {
        let expression = parse("float(count).member + ratio").expect("conversion should compose");
        let ExpressionKind::Binary {
            operator: BinaryOperator::Add,
            left,
            ..
        } = expression.kind
        else {
            panic!("expected addition at the root");
        };
        let ExpressionKind::MemberAccess { object, .. } = left.kind else {
            panic!("expected member access on the conversion");
        };
        assert!(matches!(
            object.kind,
            ExpressionKind::PrimitiveConversion {
                target: PrimitiveType::Float,
                ..
            }
        ));

        let expression =
            parse("for i in 0..int(limit) {}").expect("conversion should be a simple range bound");
        let ExpressionKind::RangeFor { end, .. } = expression.kind else {
            panic!("expected a range loop");
        };
        assert!(matches!(
            end.kind,
            ExpressionKind::PrimitiveConversion {
                target: PrimitiveType::Int,
                ..
            }
        ));
    }

    #[test]
    fn reports_malformed_primitive_conversions() {
        for (source, expected_kind, expected_span) in [
            (
                "int",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftParen,
                    found: TokenKind::Eof,
                },
                Span::new(3, 3),
            ),
            (
                "float()",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::RightParen,
                },
                Span::new(6, 7),
            ),
            (
                "bool(first, second)",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Comma,
                },
                Span::new(10, 11),
            ),
            (
                "char(value,)",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Comma,
                },
                Span::new(10, 11),
            ),
            (
                "string(value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Eof,
                },
                Span::new(12, 12),
            ),
            (
                "bytes(value)",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Bytes,
                },
                Span::new(0, 5),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }

        let expression = parse("none(value)").expect("none call remains ordinary call syntax");
        assert!(matches!(expression.kind, ExpressionKind::Call { .. }));
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
    fn parses_type_test_expressions() {
        let expression = parse("value is int").expect("type test should parse");

        assert_eq!(expression.span, Span::new(0, 12));
        let ExpressionKind::TypeTest { value, type_syntax } = expression.kind else {
            panic!("expected a type test");
        };
        assert_eq!(value.span, Span::new(0, 5));
        assert!(matches!(value.kind, ExpressionKind::Identifier));
        assert_eq!(
            type_syntax,
            TypeSyntax::new(TypeKind::Primitive(PrimitiveType::Int), Span::new(9, 12),)
        );

        let expression =
            parse("result is Error<string> | none").expect("union type test should parse");
        let ExpressionKind::TypeTest { type_syntax, .. } = expression.kind else {
            panic!("expected a type test");
        };
        assert_eq!(type_syntax.span, Span::new(10, 30));
        assert!(matches!(type_syntax.kind, TypeKind::Union { .. }));
    }

    #[test]
    fn type_tests_use_relational_precedence() {
        let expression =
            parse("value + 1 is int == true").expect("composed type test should parse");
        let ExpressionKind::Binary {
            left,
            operator: BinaryOperator::Equal,
            right,
        } = expression.kind
        else {
            panic!("expected equality at the root");
        };
        assert!(matches!(
            right.kind,
            ExpressionKind::Literal(LiteralKind::Boolean(true))
        ));
        let ExpressionKind::TypeTest { value, type_syntax } = left.kind else {
            panic!("expected a type test before equality");
        };
        assert!(matches!(
            value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(type_syntax.span, Span::new(13, 16));

        let expression = parse("service.read() is bytes").expect("postfix operand should parse");
        let ExpressionKind::TypeTest { value, .. } = expression.kind else {
            panic!("expected a type test");
        };
        assert!(matches!(value.kind, ExpressionKind::Call { .. }));
        assert_eq!(value.span, Span::new(0, 14));
    }

    #[test]
    fn type_tests_parse_in_conditional_conditions() {
        let expression = parse("if value is int { value }").expect("conditional should parse");
        let ExpressionKind::If { condition, .. } = expression.kind else {
            panic!("expected a conditional");
        };
        assert!(matches!(condition.kind, ExpressionKind::TypeTest { .. }));
        assert_eq!(condition.span, Span::new(3, 15));
    }

    #[test]
    fn reports_missing_and_trailing_type_test_syntax() {
        for (source, expected_kind, expected_span) in [
            (
                "value is",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Eof,
                },
                Span::new(8, 8),
            ),
            (
                "value is + 1",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Plus,
                },
                Span::new(9, 10),
            ),
            (
                "value is int |",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Eof,
                },
                Span::new(14, 14),
            ),
            (
                "value is int trailing",
                ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Identifier,
                },
                Span::new(13, 21),
            ),
        ] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
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
