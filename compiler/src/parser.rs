use std::iter::Peekable;

use crate::ast::{
    AnonymousStructField, AnonymousStructMember, AssignmentOperator, BinaryOperator,
    BindingMutability, BindingQualifiers, Block, BuiltinType, ConditionalElse, Declaration,
    Expression, ExpressionKind, Function, FunctionParameter, FunctionParameterKind,
    InterfaceDeclaration, InterfaceMethodRequirement, LiteralKind, NodeId, PrimitiveType, Program,
    RangeInclusivity, Statement, StatementKind, StructDeclaration, StructField,
    StructFieldInitializer, StructMember, TypeKind, TypeSyntax, UnaryOperator, ValueCapability,
};
use crate::lexer::{LexError, Token, TokenKind};
use crate::source::{ModuleId, Span};

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
    ExpectedStructMember {
        found: TokenKind,
    },
    ExpectedAnonymousStructMember {
        found: TokenKind,
    },
    ExpectedInterfaceMember {
        found: TokenKind,
    },
    ExpectedDeferredCall,
    ExpectedCoroutineCall,
    ExpectedBuiltinTypeArguments {
        builtin: BuiltinType,
        found: TokenKind,
    },
    InvalidBuiltinTypeArgumentCount {
        builtin: BuiltinType,
        expected: usize,
        found: usize,
    },
    ExpectedBuiltinAssociatedAccess {
        builtin: BuiltinType,
        found: TokenKind,
    },
    InclusiveSliceNotSupported,
    RangeBoundRequiresGrouping,
    InvalidBindingQualifiers {
        binding: TokenKind,
        value: TokenKind,
    },
    ValueCapabilityWithoutBinding {
        found: TokenKind,
    },
    InvalidReceiverQualifiers,
    BindingValueCapabilityMustPrecedeName,
    ExpectedToken {
        expected: TokenKind,
        found: TokenKind,
    },
    UnexpectedToken {
        found: TokenKind,
    },
    TokenModuleMismatch {
        expected: ModuleId,
        found: ModuleId,
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

/// Allocates AST node identities for repeated parser entry points in one module.
///
/// Reuse one context while parsing independently requested fragments from the
/// same module so their node IDs remain distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseContext {
    module_id: ModuleId,
    next_node_id: u32,
    #[cfg(test)]
    leave_ids_unassigned: bool,
}

impl ParseContext {
    #[must_use]
    pub const fn new(module_id: ModuleId) -> Self {
        Self {
            module_id,
            next_node_id: 0,
            #[cfg(test)]
            leave_ids_unassigned: false,
        }
    }

    #[must_use]
    pub const fn module_id(&self) -> ModuleId {
        self.module_id
    }

    fn next_node_id(&mut self) -> NodeId {
        #[cfg(test)]
        if self.leave_ids_unassigned {
            return NodeId::UNASSIGNED;
        }

        let id = NodeId {
            module_id: self.module_id,
            node_id: self.next_node_id,
        };
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .expect("node ID space exhausted");
        id
    }

    #[cfg(test)]
    const fn unassigned() -> Self {
        Self {
            module_id: ModuleId::PRELUDE,
            next_node_id: 0,
            leave_ids_unassigned: true,
        }
    }
}

/// Parses one complete source program.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_program<I>(context: &mut ParseContext, tokens: I) -> ParseResult<Program>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut program = Parser::new(tokens, context.module_id()).program()?;
    assign_program_ids(&mut program, context);
    Ok(program)
}

/// Parses one complete expression token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_expression<I>(context: &mut ParseContext, tokens: I) -> ParseResult
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens, context.module_id());
    let mut expression = parser.expression(LOWEST_BINDING_POWER, true)?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    assign_expression_ids(&mut expression, context);
    Ok(expression)
}

/// Parses one complete type-expression token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_type<I>(context: &mut ParseContext, tokens: I) -> ParseResult<TypeSyntax>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens, context.module_id());
    let mut type_syntax = parser.type_expression()?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    assign_type_ids(&mut type_syntax, context);
    Ok(type_syntax)
}

/// Parses one complete statement token stream.
///
/// The iterator may be the lexer itself and is expected to yield its explicit
/// [`TokenKind::Eof`] token.
pub fn parse_statement<I>(context: &mut ParseContext, tokens: I) -> ParseResult<Statement>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    let mut parser = Parser::new(tokens, context.module_id());
    let mut statement = parser.statement()?;
    let token = parser.current()?;

    if token.kind != TokenKind::Eof {
        return Err(ParseError {
            kind: ParseErrorKind::UnexpectedToken { found: token.kind },
            span: token.span,
        }
        .into());
    }

    assign_statement_ids(&mut statement, context);
    Ok(statement)
}

struct Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    tokens: Peekable<I>,
    module_id: ModuleId,
    /// Holds the second `>` when type parsing splits a `>>` token that closes
    /// two nested parameterized types.
    pending: Option<Token>,
    last_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallStatementKind {
    Defer,
    Coroutine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedBindingQualifiers {
    qualifiers: BindingQualifiers,
    binding_token: Token,
    value_token: Option<Token>,
}

impl<I> Parser<I>
where
    I: Iterator<Item = Result<Token, LexError>>,
{
    fn new(tokens: I, module_id: ModuleId) -> Self {
        Self {
            tokens: tokens.peekable(),
            module_id,
            pending: None,
            last_end: 0,
        }
    }

    fn program(&mut self) -> ParseResult<Program> {
        let mut declarations = Vec::new();

        loop {
            let token = self.current()?;
            if token.kind == TokenKind::Eof {
                return Ok(Program::new(
                    declarations,
                    Span::new(self.module_id, 0, token.span.end),
                ));
            }

            declarations.push(self.declaration()?);
        }
    }

    fn declaration(&mut self) -> ParseResult<Declaration> {
        let token = self.current()?;
        match token.kind {
            TokenKind::Fn => Ok(Declaration::Function(self.function()?)),
            TokenKind::Struct => Ok(Declaration::Struct(self.struct_declaration()?)),
            TokenKind::Interface => Ok(Declaration::Interface(self.interface_declaration()?)),
            _ => Err(ParseError {
                kind: ParseErrorKind::ExpectedTopLevelDeclaration { found: token.kind },
                span: token.span,
            }
            .into()),
        }
    }

    fn interface_declaration(&mut self) -> ParseResult<InterfaceDeclaration> {
        let keyword = self.expect(TokenKind::Interface)?;
        let name = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::LeftBrace)?;
        let mut requirements = Vec::new();

        let right_brace = loop {
            let token = self.current()?;
            match token.kind {
                TokenKind::RightBrace => break self.advance()?,
                TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                TokenKind::Fn => requirements.push(self.interface_method_requirement()?),
                _ => {
                    return Err(ParseError {
                        kind: ParseErrorKind::ExpectedInterfaceMember { found: token.kind },
                        span: token.span,
                    }
                    .into());
                }
            }
        };

        Ok(InterfaceDeclaration::new(
            name.span,
            requirements,
            Span::new(self.module_id, keyword.span.start, right_brace.span.end),
        ))
    }

    fn interface_method_requirement(&mut self) -> ParseResult<InterfaceMethodRequirement> {
        let keyword = self.expect(TokenKind::Fn)?;
        let name = self.expect(TokenKind::Identifier)?;
        let parameters = self.function_parameters(true)?;
        let return_type = self.optional_return_type()?;
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(InterfaceMethodRequirement::new(
            name.span,
            parameters,
            return_type,
            Span::new(self.module_id, keyword.span.start, semicolon.span.end),
        ))
    }

    fn struct_declaration(&mut self) -> ParseResult<StructDeclaration> {
        let keyword = self.expect(TokenKind::Struct)?;
        let name = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::LeftBrace)?;
        let mut members = Vec::new();

        let right_brace = loop {
            let token = self.current()?;
            match token.kind {
                TokenKind::RightBrace => break self.advance()?,
                TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                TokenKind::Fn => members.push(StructMember::Function(self.function()?)),
                TokenKind::Identifier => members.push(StructMember::Field(self.struct_field()?)),
                _ => {
                    return Err(ParseError {
                        kind: ParseErrorKind::ExpectedStructMember { found: token.kind },
                        span: token.span,
                    }
                    .into());
                }
            }
        };

        Ok(StructDeclaration::new(
            name.span,
            members,
            Span::new(self.module_id, keyword.span.start, right_brace.span.end),
        ))
    }

    fn struct_field(&mut self) -> ParseResult<StructField> {
        let name = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::Colon)?;
        let type_annotation = self.type_expression()?;
        let comma = self.expect(TokenKind::Comma)?;
        let span = Span::new(self.module_id, name.span.start, comma.span.end);
        Ok(StructField::new(name.span, type_annotation, span))
    }

    fn statement(&mut self) -> ParseResult<Statement> {
        let token = self.current()?;
        match token.kind {
            TokenKind::Fn => self.function_statement(),
            TokenKind::Const | TokenKind::Mut => self.binding_statement(),
            TokenKind::VConst | TokenKind::VMut => {
                Err(self.value_capability_without_binding_error(token).into())
            }
            TokenKind::Defer => self.call_statement(CallStatementKind::Defer),
            TokenKind::Co => self.call_statement(CallStatementKind::Coroutine),
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
        let span = Span::new(self.module_id, keyword.span.start, body.span.end);

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
        if allow_receiver && first.kind == TokenKind::SelfValue {
            let receiver = self.advance()?;
            return Ok(FunctionParameter::new(
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const),
                FunctionParameterKind::Receiver {
                    name: receiver.span,
                },
                receiver.span,
            ));
        }

        let parsed_qualifiers = self.optional_binding_qualifiers()?;
        let parameter = self.current()?;

        if allow_receiver && parameter.kind == TokenKind::SelfValue {
            let parsed = parsed_qualifiers.expect("a qualified receiver has a qualifier");
            if parsed.binding_token.kind != TokenKind::Mut || parsed.value_token.is_some() {
                let span = parsed
                    .value_token
                    .map_or(parsed.binding_token.span, |token| token.span);
                return Err(ParseError {
                    kind: ParseErrorKind::InvalidReceiverQualifiers,
                    span,
                }
                .into());
            }

            let receiver = self.advance()?;
            return Ok(FunctionParameter::new(
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut),
                FunctionParameterKind::Receiver {
                    name: receiver.span,
                },
                Span::new(
                    self.module_id,
                    parsed.binding_token.span.start,
                    receiver.span.end,
                ),
            ));
        }

        let qualifiers = parsed_qualifiers.map_or(
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const),
            |parsed| parsed.qualifiers,
        );
        let start = parsed_qualifiers.map_or(first.span.start, |parsed| {
            parsed.binding_token.span.start
        });
        let name = self.expect(TokenKind::Identifier)?;
        self.expect(TokenKind::Colon)?;
        let type_annotation = self.binding_type_annotation()?;
        let span = Span::new(self.module_id, start, type_annotation.span.end);

        Ok(FunctionParameter::new(
            qualifiers,
            FunctionParameterKind::Named {
                name: name.span,
                type_annotation,
            },
            span,
        ))
    }

    fn binding_statement(&mut self) -> ParseResult<Statement> {
        let parsed = self
            .optional_binding_qualifiers()?
            .expect("a binding statement starts with const or mut");
        let name = self.expect(TokenKind::Identifier)?;
        let type_annotation = if self.current()?.kind == TokenKind::Colon {
            self.advance()?;
            Some(self.binding_type_annotation()?)
        } else {
            None
        };

        self.expect(TokenKind::Assign)?;
        let initializer = self.expression(LOWEST_BINDING_POWER, true)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Binding {
                qualifiers: parsed.qualifiers,
                name: name.span,
                type_annotation,
                initializer,
            },
            Span::new(
                self.module_id,
                parsed.binding_token.span.start,
                semicolon.span.end,
            ),
        ))
    }

    fn optional_binding_qualifiers(&mut self) -> ParseResult<Option<ParsedBindingQualifiers>> {
        let binding_token = self.current()?;
        let (binding, default_value, allowed_override) = match binding_token.kind {
            TokenKind::Const => (
                BindingMutability::Const,
                ValueCapability::Const,
                TokenKind::VMut,
            ),
            TokenKind::Mut => (
                BindingMutability::Mut,
                ValueCapability::Mut,
                TokenKind::VConst,
            ),
            TokenKind::VConst | TokenKind::VMut => {
                return Err(self.value_capability_without_binding_error(binding_token).into());
            }
            _ => return Ok(None),
        };

        self.advance()?;
        let token = self.current()?;
        let (value, value_token) = match token.kind {
            TokenKind::VConst | TokenKind::VMut if token.kind == allowed_override => {
                self.advance()?;
                let value = if token.kind == TokenKind::VMut {
                    ValueCapability::Mut
                } else {
                    ValueCapability::Const
                };
                (value, Some(token))
            }
            TokenKind::VConst | TokenKind::VMut => {
                return Err(ParseError {
                    kind: ParseErrorKind::InvalidBindingQualifiers {
                        binding: binding_token.kind,
                        value: token.kind,
                    },
                    span: token.span,
                }
                .into());
            }
            _ => (default_value, None),
        };

        Ok(Some(ParsedBindingQualifiers {
            qualifiers: BindingQualifiers::new(binding, value),
            binding_token,
            value_token,
        }))
    }

    fn binding_type_annotation(&mut self) -> ParseResult<TypeSyntax> {
        let type_annotation = self.type_expression()?;
        if outer_mutable_type(&type_annotation) {
            return Err(ParseError {
                kind: ParseErrorKind::BindingValueCapabilityMustPrecedeName,
                span: type_annotation.span,
            }
            .into());
        }

        Ok(type_annotation)
    }

    fn value_capability_without_binding_error(&self, token: Token) -> ParseError {
        ParseError {
            kind: ParseErrorKind::ValueCapabilityWithoutBinding { found: token.kind },
            span: token.span,
        }
    }

    fn expression_statement(&mut self) -> ParseResult<Statement> {
        let expression = self.expression(LOWEST_BINDING_POWER, true)?;
        let span = if self.current()?.kind == TokenKind::Semicolon {
            let semicolon = self.advance()?;
            Span::new(self.module_id, expression.span.start, semicolon.span.end)
        } else if expression_may_omit_statement_semicolon(&expression)
            && self.current()?.kind == TokenKind::Eof
        {
            expression.span
        } else {
            let semicolon = self.expect(TokenKind::Semicolon)?;
            Span::new(self.module_id, expression.span.start, semicolon.span.end)
        };
        Ok(Statement::new(StatementKind::Expression(expression), span))
    }

    fn call_statement(&mut self, kind: CallStatementKind) -> ParseResult<Statement> {
        let keyword_kind = match kind {
            CallStatementKind::Defer => TokenKind::Defer,
            CallStatementKind::Coroutine => TokenKind::Co,
        };
        let keyword = self.expect(keyword_kind)?;
        let token = self.current()?;

        if matches!(
            token.kind,
            TokenKind::Semicolon | TokenKind::RightBrace | TokenKind::Eof
        ) {
            return Err(ParseError {
                kind: expected_call_error(kind),
                span: token.span,
            }
            .into());
        }

        let call = self.expression(LOWEST_BINDING_POWER, true)?;

        if !matches!(&call.kind, ExpressionKind::Call { .. }) {
            return Err(ParseError {
                kind: expected_call_error(kind),
                span: call.span,
            }
            .into());
        }

        let semicolon = self.expect(TokenKind::Semicolon)?;
        let statement_kind = match kind {
            CallStatementKind::Defer => StatementKind::Defer(call),
            CallStatementKind::Coroutine => StatementKind::Coroutine(call),
        };

        Ok(Statement::new(
            statement_kind,
            Span::new(self.module_id, keyword.span.start, semicolon.span.end),
        ))
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
            _ => Some(self.expression(LOWEST_BINDING_POWER, true)?),
        };
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Break(value),
            Span::new(self.module_id, keyword.span.start, semicolon.span.end),
        ))
    }

    fn continue_statement(&mut self) -> ParseResult<Statement> {
        let keyword = self.expect(TokenKind::Continue)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;
        Ok(Statement::new(
            StatementKind::Continue,
            Span::new(self.module_id, keyword.span.start, semicolon.span.end),
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
            _ => Some(self.expression(LOWEST_BINDING_POWER, true)?),
        };
        let semicolon = self.expect(TokenKind::Semicolon)?;

        Ok(Statement::new(
            StatementKind::Return(value),
            Span::new(self.module_id, keyword.span.start, semicolon.span.end),
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
            Span::new(self.module_id, start, end),
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
            Span::new(self.module_id, start, end),
        ))
    }

    fn prefix_type(&mut self) -> ParseResult<TypeSyntax> {
        let token = self.current()?;

        if token.kind != TokenKind::Mut {
            return self.primary_type();
        }

        self.advance()?;
        let inner = self.prefix_type()?;
        let span = Span::new(self.module_id, token.span.start, inner.span.end);
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
            TokenKind::Queue => self.builtin_type(BuiltinType::Queue),
            TokenKind::Vector => self.builtin_type(BuiltinType::Vector),
            TokenKind::Map => self.builtin_type(BuiltinType::Map),
            TokenKind::Error => self.builtin_type(BuiltinType::Error),
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
        let (arguments, end) = if self.current()?.kind == TokenKind::Less {
            let (arguments, close) = self.type_arguments()?;
            (arguments, close.span.end)
        } else {
            (Vec::new(), name.span.end)
        };

        Ok(TypeSyntax::new(
            TypeKind::Named {
                name: name.span,
                arguments,
            },
            Span::new(self.module_id, name.span.start, end),
        ))
    }

    fn builtin_type(&mut self, builtin: BuiltinType) -> ParseResult<TypeSyntax> {
        let name = self.advance()?;
        let token = self.current()?;

        if token.kind != TokenKind::Less {
            return Err(ParseError {
                kind: ParseErrorKind::ExpectedBuiltinTypeArguments {
                    builtin,
                    found: token.kind,
                },
                span: token.span,
            }
            .into());
        }

        let (arguments, close) = self.type_arguments()?;
        validate_builtin_type_argument_count(
            builtin,
            arguments.len(),
            Span::new(self.module_id, name.span.start, close.span.end),
        )?;

        Ok(TypeSyntax::new(
            TypeKind::Builtin { builtin, arguments },
            Span::new(self.module_id, name.span.start, close.span.end),
        ))
    }

    fn type_arguments(&mut self) -> ParseResult<(Vec<TypeSyntax>, Token)> {
        self.expect(TokenKind::Less)?;
        let mut arguments = vec![self.type_expression()?];

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

        let close = self.expect_type_argument_close()?;
        Ok((arguments, close))
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
        let span = Span::new(self.module_id, function.span.start, return_type.span.end);

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
                Span::new(
                    self.module_id,
                    left_parenthesis.span.start,
                    right_parenthesis.span.end,
                ),
            ));
        }

        let inner = self.type_expression()?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;
        Ok(TypeSyntax::new(
            TypeKind::Group(Box::new(inner)),
            Span::new(
                self.module_id,
                left_parenthesis.span.start,
                right_parenthesis.span.end,
            ),
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
                    Span::new(self.module_id, token.span.start, token.span.start + 1),
                );
                self.pending = Some(Token::new(
                    TokenKind::Greater,
                    Span::new(self.module_id, token.span.start + 1, token.span.end),
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

    fn expression(
        &mut self,
        minimum_binding_power: u8,
        allow_struct_construction: bool,
    ) -> ParseResult {
        let mut left = self.prefix(allow_struct_construction)?;

        loop {
            left = match self.current()?.kind {
                TokenKind::LeftParen => self.call(left)?,
                TokenKind::Dot => self.member_access(left)?,
                TokenKind::DoubleColon if matches!(&left.kind, ExpressionKind::Identifier) => {
                    self.named_associated_access(left)?
                }
                TokenKind::LeftBracket => self.index_or_slice(left)?,
                TokenKind::Question => self.try_expression(left)?,
                // A construction target is a nominal name, not an arbitrary
                // expression. Control-flow heads disable this branch so their
                // following brace remains the body delimiter.
                TokenKind::LeftBrace
                    if allow_struct_construction
                        && matches!(&left.kind, ExpressionKind::Identifier) =>
                {
                    self.struct_construction(left)?
                }
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
                    let right = self
                        .expression(binding_power.right_binding_power, allow_struct_construction)?;
                    let span = Span::new(self.module_id, left.span.start, right.span.end);
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
                    let right = self
                        .expression(binding_power.right_binding_power, allow_struct_construction)?;
                    let span = Span::new(self.module_id, left.span.start, right.span.end);
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
                    let span = Span::new(self.module_id, left.span.start, type_syntax.span.end);
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
        let (arguments, right_parenthesis) = self.call_arguments()?;
        let span = Span::new(
            self.module_id,
            callee.span.start,
            right_parenthesis.span.end,
        );

        Ok(Expression::new(
            ExpressionKind::Call {
                callee: Box::new(callee),
                arguments,
            },
            span,
        ))
    }

    fn call_arguments(&mut self) -> ParseResult<(Vec<Expression>, Token)> {
        self.expect(TokenKind::LeftParen)?;
        let mut arguments = Vec::new();

        if self.current()?.kind != TokenKind::RightParen {
            loop {
                arguments.push(self.expression(LOWEST_BINDING_POWER, true)?);

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
        Ok((arguments, right_parenthesis))
    }

    fn member_access(&mut self, object: Expression) -> ParseResult {
        self.expect(TokenKind::Dot)?;
        let member = self.expect(TokenKind::Identifier)?;
        let span = Span::new(self.module_id, object.span.start, member.span.end);

        Ok(Expression::new(
            ExpressionKind::MemberAccess {
                object: Box::new(object),
                member: member.span,
            },
            span,
        ))
    }

    fn named_associated_access(&mut self, owner: Expression) -> ParseResult {
        assert!(
            matches!(&owner.kind, ExpressionKind::Identifier),
            "associated-access owner must be a named type"
        );
        let owner = TypeSyntax::new(
            TypeKind::Named {
                name: owner.span,
                arguments: Vec::new(),
            },
            owner.span,
        );
        self.associated_access(owner)
    }

    fn associated_access(&mut self, owner: TypeSyntax) -> ParseResult {
        self.expect(TokenKind::DoubleColon)?;
        let member = self.expect(TokenKind::Identifier)?;
        let span = Span::new(self.module_id, owner.span.start, member.span.end);

        Ok(Expression::new(
            ExpressionKind::AssociatedAccess {
                owner,
                member: member.span,
            },
            span,
        ))
    }

    fn index_or_slice(&mut self, object: Expression) -> ParseResult {
        self.expect(TokenKind::LeftBracket)?;

        if self.current()?.kind == TokenKind::DotDotEqual {
            let delimiter = self.current()?;
            return Err(ParseError {
                kind: ParseErrorKind::InclusiveSliceNotSupported,
                span: delimiter.span,
            }
            .into());
        }

        if self.current()?.kind == TokenKind::DotDot {
            self.advance()?;
            return self.finish_slice(object, None);
        }

        let index = self.expression(LOWEST_BINDING_POWER, true)?;

        if self.current()?.kind == TokenKind::DotDotEqual {
            let delimiter = self.current()?;
            return Err(ParseError {
                kind: ParseErrorKind::InclusiveSliceNotSupported,
                span: delimiter.span,
            }
            .into());
        }

        if self.current()?.kind == TokenKind::DotDot {
            self.advance()?;
            return self.finish_slice(object, Some(index));
        }

        let right_bracket = self.expect(TokenKind::RightBracket)?;
        let span = Span::new(self.module_id, object.span.start, right_bracket.span.end);

        Ok(Expression::new(
            ExpressionKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            },
            span,
        ))
    }

    fn finish_slice(&mut self, object: Expression, start: Option<Expression>) -> ParseResult {
        let token = self.current()?;
        if token.kind == TokenKind::Eof {
            return Err(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBracket,
                    found: TokenKind::Eof,
                },
                span: token.span,
            }
            .into());
        }

        let end = if token.kind == TokenKind::RightBracket {
            None
        } else {
            Some(self.expression(LOWEST_BINDING_POWER, true)?)
        };
        let right_bracket = self.expect(TokenKind::RightBracket)?;
        let span = Span::new(self.module_id, object.span.start, right_bracket.span.end);

        Ok(Expression::new(
            ExpressionKind::Slice {
                object: Box::new(object),
                start: start.map(Box::new),
                end: end.map(Box::new),
            },
            span,
        ))
    }

    fn try_expression(&mut self, expression: Expression) -> ParseResult {
        let question = self.expect(TokenKind::Question)?;
        let span = Span::new(self.module_id, expression.span.start, question.span.end);

        Ok(Expression::new(
            ExpressionKind::Try {
                expression: Box::new(expression),
            },
            span,
        ))
    }

    fn prefix(&mut self, allow_struct_construction: bool) -> ParseResult {
        let token = self.current()?;

        match token.kind {
            TokenKind::Minus => {
                self.advance()?;
                let operand =
                    self.expression(prefix_binding_power(token.kind), allow_struct_construction)?;
                let span = Span::new(self.module_id, token.span.start, operand.span.end);

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
                let operand =
                    self.expression(prefix_binding_power(token.kind), allow_struct_construction)?;
                let span = Span::new(self.module_id, token.span.start, operand.span.end);
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
            TokenKind::Bool => self.primitive_associated_access(PrimitiveType::Bool),
            TokenKind::Char => self.primitive_conversion(PrimitiveType::Char),
            TokenKind::String => self.primitive_conversion(PrimitiveType::String),
            TokenKind::Bytes => self.primitive_associated_access(PrimitiveType::Bytes),
            TokenKind::Queue => self.builtin_associated_access(BuiltinType::Queue),
            TokenKind::Vector => self.builtin_associated_access(BuiltinType::Vector),
            TokenKind::Map => self.builtin_associated_access(BuiltinType::Map),
            TokenKind::Error => self.builtin_associated_access(BuiltinType::Error),
            TokenKind::Struct if allow_struct_construction => self.anonymous_struct_expression(),
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

    fn builtin_associated_access(&mut self, builtin: BuiltinType) -> ParseResult {
        let name = self.advance()?;
        let mut type_arguments = Vec::new();
        let mut end = name.span.end;

        if self.current()?.kind == TokenKind::Less {
            let (arguments, close) = self.type_arguments()?;
            validate_builtin_type_argument_count(
                builtin,
                arguments.len(),
                Span::new(self.module_id, name.span.start, close.span.end),
            )?;
            type_arguments = arguments;
            end = close.span.end;
        } else if builtin != BuiltinType::Error {
            let token = self.current()?;
            return Err(ParseError {
                kind: ParseErrorKind::ExpectedBuiltinTypeArguments {
                    builtin,
                    found: token.kind,
                },
                span: token.span,
            }
            .into());
        }

        let token = self.current()?;
        if token.kind != TokenKind::DoubleColon {
            return Err(ParseError {
                kind: ParseErrorKind::ExpectedBuiltinAssociatedAccess {
                    builtin,
                    found: token.kind,
                },
                span: token.span,
            }
            .into());
        }

        let owner = TypeSyntax::new(
            TypeKind::Builtin {
                builtin,
                arguments: type_arguments,
            },
            Span::new(self.module_id, name.span.start, end),
        );
        self.associated_access(owner)
    }

    fn lambda_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::Lambda)?;
        let parameters = self.function_parameters(false)?;
        let return_type = self.optional_return_type()?;
        let body = self.block()?;
        let span = Span::new(self.module_id, keyword.span.start, body.span.end);

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
        if self.current()?.kind == TokenKind::DoubleColon {
            let owner = TypeSyntax::new(TypeKind::Primitive(target), keyword.span);
            return self.associated_access(owner);
        }
        self.expect(TokenKind::LeftParen)?;
        let value = self.expression(LOWEST_BINDING_POWER, true)?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;

        Ok(Expression::new(
            ExpressionKind::PrimitiveConversion {
                target,
                value: Box::new(value),
            },
            Span::new(
                self.module_id,
                keyword.span.start,
                right_parenthesis.span.end,
            ),
        ))
    }

    fn primitive_associated_access(&mut self, primitive: PrimitiveType) -> ParseResult {
        let keyword = self.advance()?;
        if self.current()?.kind != TokenKind::DoubleColon {
            return Err(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: keyword.kind,
                },
                span: keyword.span,
            }
            .into());
        }
        let owner = TypeSyntax::new(TypeKind::Primitive(primitive), keyword.span);
        self.associated_access(owner)
    }

    fn struct_construction(&mut self, name_expression: Expression) -> ParseResult {
        assert!(
            matches!(&name_expression.kind, ExpressionKind::Identifier),
            "struct-construction target must be a nominal type name"
        );
        let name = name_expression.span;
        self.expect(TokenKind::LeftBrace)?;
        let mut fields = Vec::new();

        if self.current()?.kind != TokenKind::RightBrace {
            loop {
                let field_name = self.expect(TokenKind::Identifier)?;
                self.expect(TokenKind::Colon)?;
                let value = self.expression(LOWEST_BINDING_POWER, true)?;
                let span = Span::new(self.module_id, field_name.span.start, value.span.end);
                fields.push(StructFieldInitializer::new(field_name.span, value, span));

                if self.current()?.kind != TokenKind::Comma {
                    break;
                }

                self.advance()?;
                if self.current()?.kind == TokenKind::RightBrace {
                    break;
                }
            }
        }

        let right_brace = self.expect(TokenKind::RightBrace)?;
        Ok(Expression::new(
            ExpressionKind::StructConstruction { name, fields },
            Span::new(self.module_id, name.start, right_brace.span.end),
        ))
    }

    fn anonymous_struct_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::Struct)?;
        self.expect(TokenKind::LeftBrace)?;
        let mut members = Vec::new();

        let right_brace = loop {
            let token = self.current()?;
            match token.kind {
                TokenKind::RightBrace => break self.advance()?,
                TokenKind::Eof => break self.expect(TokenKind::RightBrace)?,
                TokenKind::Fn => {
                    members.push(AnonymousStructMember::Method(self.function()?));
                }
                TokenKind::Identifier => {
                    members.push(AnonymousStructMember::Field(self.anonymous_struct_field()?));
                }
                _ => {
                    return Err(ParseError {
                        kind: ParseErrorKind::ExpectedAnonymousStructMember { found: token.kind },
                        span: token.span,
                    }
                    .into());
                }
            }
        };

        Ok(Expression::new(
            ExpressionKind::AnonymousStruct { members },
            Span::new(self.module_id, keyword.span.start, right_brace.span.end),
        ))
    }

    fn anonymous_struct_field(&mut self) -> ParseResult<AnonymousStructField> {
        let name = self.expect(TokenKind::Identifier)?;
        let type_annotation = if self.current()?.kind == TokenKind::Colon {
            self.advance()?;
            Some(self.type_expression()?)
        } else {
            None
        };
        self.expect(TokenKind::Assign)?;
        let initializer = self.expression(LOWEST_BINDING_POWER, true)?;
        let semicolon = self.expect(TokenKind::Semicolon)?;
        let span = Span::new(self.module_id, name.span.start, semicolon.span.end);

        Ok(AnonymousStructField::new(
            name.span,
            type_annotation,
            initializer,
            span,
        ))
    }

    fn group(&mut self) -> ParseResult {
        let left_parenthesis = self.advance()?;

        if self.current()?.kind == TokenKind::RightParen {
            let right_parenthesis = self.advance()?;
            return Ok(Expression::new(
                ExpressionKind::Literal(LiteralKind::Unit),
                Span::new(
                    self.module_id,
                    left_parenthesis.span.start,
                    right_parenthesis.span.end,
                ),
            ));
        }

        // Parentheses provide an unambiguous boundary, so construction syntax
        // is available even when the surrounding condition or range head
        // disables it to reserve the following brace for a body.
        let expression = self.expression(LOWEST_BINDING_POWER, true)?;
        let right_parenthesis = self.expect(TokenKind::RightParen)?;

        Ok(Expression::new(
            ExpressionKind::Group(Box::new(expression)),
            Span::new(
                self.module_id,
                left_parenthesis.span.start,
                right_parenthesis.span.end,
            ),
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
                TokenKind::Const | TokenKind::Mut => {
                    statements.push(self.binding_statement()?);
                }
                TokenKind::VConst | TokenKind::VMut => {
                    return Err(self.value_capability_without_binding_error(token).into());
                }
                TokenKind::Defer => {
                    statements.push(self.call_statement(CallStatementKind::Defer)?);
                }
                TokenKind::Co => {
                    statements.push(self.call_statement(CallStatementKind::Coroutine)?);
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
                    let expression = self.expression(LOWEST_BINDING_POWER, true)?;
                    let following = self.current()?;

                    match following.kind {
                        TokenKind::Semicolon => {
                            let semicolon = self.advance()?;
                            let span = Span::new(
                                self.module_id,
                                expression.span.start,
                                semicolon.span.end,
                            );
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

        let span = Span::new(self.module_id, left_brace.span.start, right_brace.span.end);
        Ok(Block::new(statements, value, span))
    }

    fn conditional(&mut self) -> ParseResult {
        let if_keyword = self.expect(TokenKind::If)?;
        // An ungrouped `{` starts the `if` body, rather than a named or
        // anonymous struct expression in the condition.
        let condition = self.expression(LOWEST_BINDING_POWER, false)?;
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
            Span::new(self.module_id, if_keyword.span.start, end),
        ))
    }

    fn loop_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::Loop)?;
        let body = self.block()?;
        let span = Span::new(self.module_id, keyword.span.start, body.span.end);
        Ok(Expression::new(ExpressionKind::Loop { body }, span))
    }

    fn while_expression(&mut self) -> ParseResult {
        let keyword = self.expect(TokenKind::While)?;
        // Match `if` disambiguation: construction in the condition must be
        // parenthesized so this brace unambiguously starts the loop body.
        let condition = self.expression(LOWEST_BINDING_POWER, false)?;
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
            Span::new(self.module_id, keyword.span.start, end),
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
            Span::new(self.module_id, keyword.span.start, end_span),
        ))
    }

    fn range_bound_expression(&mut self) -> ParseResult {
        // A brace after a range bound belongs to the loop body. Grouping
        // re-enables struct construction through `group` above.
        let expression = self.expression(PREFIX_BINDING_POWER, false)?;

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
            Some(Ok(token)) => self.validate_token_module(token),
            Some(Err(error)) => {
                self.validate_span_module(error.span)?;
                Err(error.into())
            }
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
                let token = self.validate_token_module(token)?;
                self.last_end = token.span.end;
                Ok(token)
            }
            Some(Err(error)) => {
                self.validate_span_module(error.span)?;
                Err(error.into())
            }
            None => Ok(self.synthetic_eof()),
        }
    }

    fn synthetic_eof(&self) -> Token {
        Token::new(
            TokenKind::Eof,
            Span::new(self.module_id, self.last_end, self.last_end),
        )
    }

    fn validate_token_module(&self, token: Token) -> ParseResult<Token> {
        self.validate_span_module(token.span)?;
        Ok(token)
    }

    fn validate_span_module(&self, span: Span) -> ParseResult<()> {
        if span.module_id == self.module_id {
            Ok(())
        } else {
            Err(ParseError {
                kind: ParseErrorKind::TokenModuleMismatch {
                    expected: self.module_id,
                    found: span.module_id,
                },
                span,
            }
            .into())
        }
    }
}

fn assign_program_ids(program: &mut Program, context: &mut ParseContext) {
    program.id = context.next_node_id();
    for declaration in &mut program.declarations {
        assign_declaration_ids(declaration, context);
    }
}

fn assign_declaration_ids(declaration: &mut Declaration, context: &mut ParseContext) {
    match declaration {
        Declaration::Function(function) => assign_function_ids(function, context),
        Declaration::Struct(structure) => {
            structure.id = context.next_node_id();
            for member in &mut structure.members {
                match member {
                    StructMember::Field(field) => {
                        field.id = context.next_node_id();
                        assign_type_ids(&mut field.type_annotation, context);
                    }
                    StructMember::Function(function) => assign_function_ids(function, context),
                }
            }
        }
        Declaration::Interface(interface) => {
            interface.id = context.next_node_id();
            for requirement in &mut interface.requirements {
                requirement.id = context.next_node_id();
                for parameter in &mut requirement.parameters {
                    assign_parameter_ids(parameter, context);
                }
                if let Some(return_type) = &mut requirement.return_type {
                    assign_type_ids(return_type, context);
                }
            }
        }
    }
}

fn assign_function_ids(function: &mut Function, context: &mut ParseContext) {
    function.id = context.next_node_id();
    for parameter in &mut function.parameters {
        assign_parameter_ids(parameter, context);
    }
    if let Some(return_type) = &mut function.return_type {
        assign_type_ids(return_type, context);
    }
    assign_block_ids(&mut function.body, context);
}

fn assign_parameter_ids(parameter: &mut FunctionParameter, context: &mut ParseContext) {
    parameter.id = context.next_node_id();
    if let FunctionParameterKind::Named {
        type_annotation, ..
    } = &mut parameter.kind
    {
        assign_type_ids(type_annotation, context);
    }
}

fn assign_block_ids(block: &mut Block, context: &mut ParseContext) {
    block.id = context.next_node_id();
    for statement in &mut block.statements {
        assign_statement_ids(statement, context);
    }
    if let Some(value) = &mut block.value {
        assign_expression_ids(value, context);
    }
}

fn assign_statement_ids(statement: &mut Statement, context: &mut ParseContext) {
    statement.id = context.next_node_id();
    match &mut statement.kind {
        StatementKind::Binding {
            type_annotation,
            initializer,
            ..
        } => {
            if let Some(type_annotation) = type_annotation {
                assign_type_ids(type_annotation, context);
            }
            assign_expression_ids(initializer, context);
        }
        StatementKind::Expression(expression)
        | StatementKind::Defer(expression)
        | StatementKind::Coroutine(expression) => assign_expression_ids(expression, context),
        StatementKind::Function(function) => assign_function_ids(function, context),
        StatementKind::Break(value) | StatementKind::Return(value) => {
            if let Some(value) = value {
                assign_expression_ids(value, context);
            }
        }
        StatementKind::Continue => {}
    }
}

fn assign_expression_ids(expression: &mut Expression, context: &mut ParseContext) {
    expression.id = context.next_node_id();
    match &mut expression.kind {
        ExpressionKind::Identifier | ExpressionKind::SelfValue | ExpressionKind::Literal(_) => {}
        ExpressionKind::Group(inner) => assign_expression_ids(inner, context),
        ExpressionKind::Block(block) | ExpressionKind::Loop { body: block } => {
            assign_block_ids(block, context);
        }
        ExpressionKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            assign_expression_ids(condition, context);
            assign_block_ids(then_branch, context);
            if let Some(else_branch) = else_branch {
                match else_branch {
                    ConditionalElse::Block(block) => assign_block_ids(block, context),
                    ConditionalElse::If(expression) => assign_expression_ids(expression, context),
                }
            }
        }
        ExpressionKind::While {
            condition,
            body,
            else_branch,
        } => {
            assign_expression_ids(condition, context);
            assign_block_ids(body, context);
            if let Some(else_branch) = else_branch {
                assign_block_ids(else_branch, context);
            }
        }
        ExpressionKind::RangeFor {
            start,
            end,
            body,
            else_branch,
            ..
        } => {
            assign_expression_ids(start, context);
            assign_expression_ids(end, context);
            assign_block_ids(body, context);
            if let Some(else_branch) = else_branch {
                assign_block_ids(else_branch, context);
            }
        }
        ExpressionKind::Lambda {
            parameters,
            return_type,
            body,
        } => {
            for parameter in parameters {
                assign_parameter_ids(parameter, context);
            }
            if let Some(return_type) = return_type {
                assign_type_ids(return_type, context);
            }
            assign_block_ids(body, context);
        }
        ExpressionKind::PrimitiveConversion { value, .. } => {
            assign_expression_ids(value, context);
        }
        ExpressionKind::StructConstruction { fields, .. } => {
            for field in fields {
                field.id = context.next_node_id();
                assign_expression_ids(&mut field.value, context);
            }
        }
        ExpressionKind::AnonymousStruct { members } => {
            for member in members {
                match member {
                    AnonymousStructMember::Field(field) => {
                        field.id = context.next_node_id();
                        if let Some(type_annotation) = &mut field.type_annotation {
                            assign_type_ids(type_annotation, context);
                        }
                        assign_expression_ids(&mut field.initializer, context);
                    }
                    AnonymousStructMember::Method(method) => {
                        assign_function_ids(method, context);
                    }
                }
            }
        }
        ExpressionKind::Call { callee, arguments } => {
            assign_expression_ids(callee, context);
            for argument in arguments {
                assign_expression_ids(argument, context);
            }
        }
        ExpressionKind::MemberAccess { object, .. } => assign_expression_ids(object, context),
        ExpressionKind::AssociatedAccess { owner, .. } => assign_type_ids(owner, context),
        ExpressionKind::Index { object, index } => {
            assign_expression_ids(object, context);
            assign_expression_ids(index, context);
        }
        ExpressionKind::Slice { object, start, end } => {
            assign_expression_ids(object, context);
            if let Some(start) = start {
                assign_expression_ids(start, context);
            }
            if let Some(end) = end {
                assign_expression_ids(end, context);
            }
        }
        ExpressionKind::Try { expression } => assign_expression_ids(expression, context),
        ExpressionKind::TypeTest { value, type_syntax } => {
            assign_expression_ids(value, context);
            assign_type_ids(type_syntax, context);
        }
        ExpressionKind::Unary { operand, .. } => assign_expression_ids(operand, context),
        ExpressionKind::Binary { left, right, .. } => {
            assign_expression_ids(left, context);
            assign_expression_ids(right, context);
        }
        ExpressionKind::Assignment { target, value, .. } => {
            assign_expression_ids(target, context);
            assign_expression_ids(value, context);
        }
    }
}

fn assign_type_ids(type_syntax: &mut TypeSyntax, context: &mut ParseContext) {
    type_syntax.id = context.next_node_id();
    match &mut type_syntax.kind {
        TypeKind::Primitive(_) => {}
        TypeKind::Builtin { arguments, .. } | TypeKind::Named { arguments, .. } => {
            for argument in arguments {
                assign_type_ids(argument, context);
            }
        }
        TypeKind::Mutable(inner) | TypeKind::Group(inner) => assign_type_ids(inner, context),
        TypeKind::Callable {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                assign_type_ids(parameter, context);
            }
            assign_type_ids(return_type, context);
        }
        TypeKind::Intersection { members } | TypeKind::Union { members } => {
            for member in members {
                assign_type_ids(member, context);
            }
        }
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

const fn expected_call_error(kind: CallStatementKind) -> ParseErrorKind {
    match kind {
        CallStatementKind::Defer => ParseErrorKind::ExpectedDeferredCall,
        CallStatementKind::Coroutine => ParseErrorKind::ExpectedCoroutineCall,
    }
}

fn outer_mutable_type(type_syntax: &TypeSyntax) -> bool {
    match &type_syntax.kind {
        TypeKind::Mutable(_) => true,
        TypeKind::Group(inner) => outer_mutable_type(inner),
        _ => false,
    }
}

const fn builtin_type_argument_count(builtin: BuiltinType) -> usize {
    match builtin {
        BuiltinType::Queue | BuiltinType::Vector | BuiltinType::Error => 1,
        BuiltinType::Map => 2,
    }
}

fn validate_builtin_type_argument_count(
    builtin: BuiltinType,
    found: usize,
    span: Span,
) -> ParseResult<()> {
    let expected = builtin_type_argument_count(builtin);
    if found == expected {
        return Ok(());
    }

    Err(ParseError {
        kind: ParseErrorKind::InvalidBuiltinTypeArgumentCount {
            builtin,
            expected,
            found,
        },
        span,
    }
    .into())
}

fn range_bound_is_simple(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::Identifier
        | ExpressionKind::SelfValue
        | ExpressionKind::Literal(_)
        | ExpressionKind::Group(_) => true,
        ExpressionKind::Call { callee, .. } => range_bound_is_simple(callee),
        ExpressionKind::MemberAccess { object, .. }
        | ExpressionKind::Index { object, .. }
        | ExpressionKind::Slice { object, .. } => range_bound_is_simple(object),
        ExpressionKind::AssociatedAccess { .. } => true,
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
    use crate::source::{SourceModule, SourceModuleRegistry};

    fn module(source: &str) -> SourceModule {
        SourceModuleRegistry::new().add(source)
    }

    fn span(start: usize, end: usize) -> Span {
        Span::new(ModuleId::TEST_SOURCE, start, end)
    }

    fn parse_program_source(source: &str) -> ParseResult<Program> {
        let module = module(source);
        let mut context = ParseContext::new(module.module_id());
        let mut result = parse_program(&mut context, Lexer::new(&module));
        if let Ok(program) = &mut result {
            assign_program_ids(program, &mut ParseContext::unassigned());
        }
        result
    }

    fn parse(source: &str) -> ParseResult {
        let module = module(source);
        let mut context = ParseContext::new(module.module_id());
        let mut result = parse_expression(&mut context, Lexer::new(&module));
        if let Ok(expression) = &mut result {
            assign_expression_ids(expression, &mut ParseContext::unassigned());
        }
        result
    }

    fn parse_type_source(source: &str) -> ParseResult<TypeSyntax> {
        let module = module(source);
        let mut context = ParseContext::new(module.module_id());
        let mut result = parse_type(&mut context, Lexer::new(&module));
        if let Ok(type_syntax) = &mut result {
            assign_type_ids(type_syntax, &mut ParseContext::unassigned());
        }
        result
    }

    fn parse_statement_source(source: &str) -> ParseResult<Statement> {
        let module = module(source);
        let mut context = ParseContext::new(module.module_id());
        let mut result = parse_statement(&mut context, Lexer::new(&module));
        if let Ok(statement) = &mut result {
            assign_statement_ids(statement, &mut ParseContext::unassigned());
        }
        result
    }

    fn integer(span: Span) -> Expression {
        Expression::new(ExpressionKind::Literal(LiteralKind::Integer), span)
    }

    #[test]
    fn parses_empty_programs() {
        let source = " \n// no declarations\n";
        assert_eq!(
            parse_program_source(source),
            Ok(Program::new(Vec::new(), span(0, source.len())))
        );
    }

    #[test]
    fn parses_multiple_top_level_function_declarations() {
        let source = concat!(
            "fn helper(value: int) -> int { value }\n",
            "fn main() { helper(1); }",
        );
        let program = parse_program_source(source).expect("program should parse");

        assert_eq!(program.span, span(0, source.len()));
        assert_eq!(program.declarations.len(), 2);
        let Declaration::Function(helper) = &program.declarations[0] else {
            panic!("expected helper function");
        };
        let Declaration::Function(main) = &program.declarations[1] else {
            panic!("expected main function");
        };
        assert_eq!(helper.name, span(3, 9));
        assert_eq!(main.name, span(42, 46));
        assert_eq!(helper.parameters.len(), 1);
        assert_eq!(main.body.statements.len(), 1);
    }

    #[test]
    fn parses_named_struct_fields_methods_and_declaration_order() {
        let source = concat!(
            "struct Node {\n",
            "    value: int,\n",
            "    next: Node | none,\n",
            "    fn value_or(self, fallback: int) -> int { self.value }\n",
            "}\n",
            "fn main() {}",
        );
        let program = parse_program_source(source).expect("program should parse");

        assert_eq!(program.declarations.len(), 2);
        let Declaration::Struct(structure) = &program.declarations[0] else {
            panic!("expected a struct declaration");
        };
        let Declaration::Function(main) = &program.declarations[1] else {
            panic!("expected main function");
        };

        let struct_end = source.find("\nfn main").expect("main follows struct");
        assert_eq!(structure.name, span(7, 11));
        assert_eq!(structure.span, span(0, struct_end));
        assert_eq!(structure.members.len(), 3);
        let StructMember::Field(value) = &structure.members[0] else {
            panic!("expected value field");
        };
        let StructMember::Field(next) = &structure.members[1] else {
            panic!("expected next field");
        };
        let StructMember::Function(method) = &structure.members[2] else {
            panic!("expected method");
        };
        assert_eq!(&source[value.name.start..value.name.end], "value");
        assert!(matches!(
            &value.type_annotation.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(&source[next.name.start..next.name.end], "next");
        assert!(matches!(&next.type_annotation.kind, TypeKind::Union { .. }));
        assert_eq!(&source[method.name.start..method.name.end], "value_or");
        assert!(matches!(
            method.parameters[0].kind,
            FunctionParameterKind::Receiver { .. }
        ));
        assert_eq!(&source[main.name.start..main.name.end], "main");
    }

    #[test]
    fn parses_empty_and_recursive_struct_declarations() {
        let source = "struct Empty {} struct Node { next: Node | none, }";
        let program = parse_program_source(source).expect("struct declarations should parse");

        let Declaration::Struct(empty) = &program.declarations[0] else {
            panic!("expected Empty");
        };
        let Declaration::Struct(node) = &program.declarations[1] else {
            panic!("expected Node");
        };
        assert!(empty.members.is_empty());
        let StructMember::Field(next) = &node.members[0] else {
            panic!("expected recursive field");
        };
        assert!(matches!(&next.type_annotation.kind, TypeKind::Union { .. }));
    }

    #[test]
    fn parses_receiverless_named_struct_functions() {
        let source = "struct Point { fn origin() -> Point { Point {} } }";
        let program = parse_program_source(source).expect("associated function should parse");
        let Declaration::Struct(point) = &program.declarations[0] else {
            panic!("expected Point struct");
        };
        let StructMember::Function(origin) = &point.members[0] else {
            panic!("expected origin function");
        };

        assert_eq!(&source[origin.name.start..origin.name.end], "origin");
        assert!(origin.parameters.is_empty());
    }

    #[test]
    fn reports_malformed_named_struct_declarations() {
        for (source, expected_kind, expected_span) in [
            (
                "struct",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span(6, 6),
            ),
            (
                "struct Thing",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(12, 12),
            ),
            (
                "struct Thing { field int, }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::Int,
                },
                span(21, 24),
            ),
            (
                "struct Thing { field: , }",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Comma,
                },
                span(22, 23),
            ),
            (
                "struct Thing { field: int }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Comma,
                    found: TokenKind::RightBrace,
                },
                span(26, 27),
            ),
            (
                "struct Thing { const }",
                ParseErrorKind::ExpectedStructMember {
                    found: TokenKind::Const,
                },
                span(15, 20),
            ),
            (
                "struct Thing {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(14, 14),
            ),
        ] {
            assert_eq!(
                parse_program_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn parses_empty_and_multi_method_interface_declarations() {
        let source = concat!(
            "interface Empty {}\n",
            "interface Stream {\n",
            "    fn read(mut self, count: int,) -> Queue<Result<bytes>>;\n",
            "    fn copy(self, source: Reader & Seekable) -> bytes | none;\n",
            "    fn close(self);\n",
            "}\n",
            "struct Buffer {}\n",
            "fn main() {}",
        );
        let program = parse_program_source(source).expect("interfaces should parse");

        assert_eq!(program.declarations.len(), 4);
        let Declaration::Interface(empty) = &program.declarations[0] else {
            panic!("expected Empty interface");
        };
        let Declaration::Interface(stream) = &program.declarations[1] else {
            panic!("expected Stream interface");
        };
        let Declaration::Struct(_) = &program.declarations[2] else {
            panic!("expected Buffer struct");
        };
        let Declaration::Function(_) = &program.declarations[3] else {
            panic!("expected main function");
        };

        assert!(empty.requirements.is_empty());
        assert_eq!(&source[empty.name.start..empty.name.end], "Empty");
        assert_eq!(stream.requirements.len(), 3);
        assert_eq!(&source[stream.name.start..stream.name.end], "Stream");

        let read = &stream.requirements[0];
        assert_eq!(&source[read.name.start..read.name.end], "read");
        assert_eq!(read.parameters.len(), 2);
        assert_eq!(
            read.parameters[0].qualifiers,
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut)
        );
        assert!(matches!(
            &read.parameters[0].kind,
            FunctionParameterKind::Receiver { .. }
        ));
        assert!(matches!(
            read.return_type.as_ref(),
            Some(TypeSyntax {
                kind: TypeKind::Builtin {
                    builtin: BuiltinType::Queue,
                    arguments, .. },
                ..
            }) if arguments.len() == 1
        ));
        assert_eq!(&source[read.span.end - 1..read.span.end], ";");

        let copy = &stream.requirements[1];
        assert!(matches!(
            &copy.parameters[1].kind,
            FunctionParameterKind::Named {
                type_annotation: TypeSyntax {
                    kind: TypeKind::Intersection { .. },
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            copy.return_type.as_ref(),
            Some(TypeSyntax {
                kind: TypeKind::Union { .. },
                ..
            })
        ));

        let close = &stream.requirements[2];
        assert_eq!(&source[close.name.start..close.name.end], "close");
        assert!(close.return_type.is_none());
        assert_eq!(stream.span.end, source.find("\nstruct Buffer").unwrap());
    }

    #[test]
    fn reports_malformed_interface_declarations() {
        for (source, expected_kind, expected_span) in [
            (
                "interface",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span(9, 9),
            ),
            (
                "interface Reader",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(16, 16),
            ),
            (
                "interface Reader { value: int, }",
                ParseErrorKind::ExpectedInterfaceMember {
                    found: TokenKind::Identifier,
                },
                span(19, 24),
            ),
            (
                "interface Reader { fn (self); }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::LeftParen,
                },
                span(22, 23),
            ),
            (
                "interface Reader { fn read(value); }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightParen,
                },
                span(32, 33),
            ),
            (
                "interface Reader { fn read(self) {} }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::LeftBrace,
                },
                span(33, 34),
            ),
            (
                "interface Reader { fn read(self) }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span(33, 34),
            ),
            (
                "interface Reader {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(18, 18),
            ),
        ] {
            assert_eq!(
                parse_program_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn interface_declarations_are_file_level_only() {
        assert_eq!(
            parse_statement_source("interface Local {}"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Interface,
                },
                span: span(0, 9),
            }))
        );
    }

    #[test]
    fn parses_contextually_typed_anonymous_struct_examples() {
        let statement = parse_statement_source(concat!(
            "const writer: Writer = struct { ",
            "fn write(mut self, data: bytes) -> int { 1 }",
            " };",
        ))
        .expect("annotated anonymous struct should parse");
        let StatementKind::Binding {
            type_annotation,
            initializer,
            ..
        } = statement.kind
        else {
            panic!("expected binding");
        };
        assert!(matches!(
            type_annotation,
            Some(TypeSyntax {
                kind: TypeKind::Named { .. },
                ..
            })
        ));
        assert!(matches!(
            initializer.kind,
            ExpressionKind::AnonymousStruct { .. }
        ));

        let source = concat!(
            "fn make_writer() -> Writer { ",
            "struct { fn write(mut self, data: bytes) -> int { 1 } }",
            " }",
        );
        let program = parse_program_source(source).expect("contextual return should parse");
        let Declaration::Function(function) = &program.declarations[0] else {
            panic!("expected function");
        };
        assert!(matches!(
            function.body.value.as_deref(),
            Some(Expression {
                kind: ExpressionKind::AnonymousStruct { .. },
                ..
            })
        ));

        let statement = parse_statement_source(concat!(
            "consume_writer(struct { ",
            "fn write(mut self, data: bytes) -> int { 1 }",
            " });",
        ))
        .expect("anonymous struct argument should parse");
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::Call { arguments, .. },
            ..
        }) = statement.kind
        else {
            panic!("expected call statement");
        };
        assert!(matches!(
            arguments[0].kind,
            ExpressionKind::AnonymousStruct { .. }
        ));
    }

    #[test]
    fn rejects_removed_interface_construction_syntax() {
        let source = "Writer { fn write(self) {} }";
        assert_eq!(
            parse(source),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Fn,
                },
                span: span(9, 11),
            }))
        );

        assert!(matches!(
            parse("Position { x: 1 }")
                .expect("named struct construction should remain valid")
                .kind,
            ExpressionKind::StructConstruction { .. }
        ));
    }

    #[test]
    fn rejects_non_declarations_at_top_level() {
        for (source, found, span) in [
            ("const value = 1;", TokenKind::Const, span(0, 5)),
            ("defer run();", TokenKind::Defer, span(0, 5)),
            ("co run();", TokenKind::Co, span(0, 2)),
            ("run();", TokenKind::Identifier, span(0, 3)),
            ("return;", TokenKind::Return, span(0, 6)),
            ("fn first() {} 42", TokenKind::IntegerLiteral, span(14, 16)),
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
    fn parameterized_builtin_names_are_reserved() {
        assert_eq!(
            parse_program_source("struct Queue {}"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Queue,
                },
                span: span(7, 12),
            }))
        );
        assert_eq!(
            parse_statement_source("const Map = 1;"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Map,
                },
                span: span(6, 9),
            }))
        );
    }

    #[test]
    fn whole_program_parser_propagates_lexical_errors() {
        assert_eq!(
            parse_program_source("fn main() {} @"),
            Err(FrontendError::Lexical(LexError {
                kind: LexErrorKind::UnexpectedCharacter,
                span: span(13, 14),
            }))
        );
    }

    #[test]
    fn parses_empty_main_function_declarations() {
        let statement = parse_statement_source("fn main() {}").expect("main function should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function declaration");
        };

        assert_eq!(statement.span, span(0, 12));
        assert_eq!(function.name, span(3, 7));
        assert!(function.parameters.is_empty());
        assert_eq!(function.return_type, None);
        assert_eq!(function.body, Block::new(Vec::new(), None, span(10, 12)));
    }

    #[test]
    fn parses_named_functions_with_typed_parameters() {
        let source = "fn add(left: int, mut right: int,) -> int { left + right }";
        let statement = parse_statement_source(source).expect("function should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function declaration");
        };

        assert_eq!(statement.span, span(0, 58));
        assert_eq!(function.span, statement.span);
        assert_eq!(function.name, span(3, 6));
        assert_eq!(function.parameters.len(), 2);
        assert_eq!(function.parameters[0].span, span(7, 16));
        assert_eq!(
            function.parameters[0].qualifiers,
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const)
        );
        let FunctionParameterKind::Named {
            name,
            type_annotation,
        } = &function.parameters[0].kind
        else {
            panic!("expected a named parameter");
        };
        assert_eq!(*name, span(7, 11));
        assert!(matches!(
            &type_annotation.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(type_annotation.span, span(13, 16));
        assert_eq!(function.parameters[1].span, span(18, 32));
        assert_eq!(
            function.parameters[1].qualifiers,
            BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Mut)
        );
        let return_type = function
            .return_type
            .as_ref()
            .expect("return type should be explicit");
        assert!(matches!(
            &return_type.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(return_type.span, span(38, 41));
        assert_eq!(function.body.span, span(42, 58));
        assert!(function.body.statements.is_empty());
        let value = function
            .body
            .value
            .as_deref()
            .expect("function body should have a value");
        assert!(matches!(
            &value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(value.span, span(44, 56));
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
        assert_eq!(function.parameters[0].span, span(10, 18));
        assert_eq!(
            function.parameters[0].qualifiers,
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut)
        );
        let FunctionParameterKind::Receiver { name } = &function.parameters[0].kind else {
            panic!("expected a receiver parameter");
        };
        assert_eq!(*name, span(14, 18));
        assert_eq!(
            function
                .return_type
                .as_ref()
                .expect("return type should be explicit")
                .span,
            span(37, 39)
        );
        assert_eq!(function.body.statements.len(), 1);
        assert_eq!(
            function.body.statements[0],
            Statement::new(StatementKind::Return(None), span(42, 49))
        );
        assert!(function.body.value.is_none());
    }

    #[test]
    fn parses_value_bearing_return_statements() {
        assert_eq!(
            parse_statement_source("return;"),
            Ok(Statement::new(StatementKind::Return(None), span(0, 7),))
        );

        let statement =
            parse_statement_source("return value + 1;").expect("value return should parse");
        assert_eq!(statement.span, span(0, 17));
        let StatementKind::Return(Some(value)) = statement.kind else {
            panic!("expected a value-bearing return");
        };
        assert_eq!(value.span, span(7, 16));
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
        assert_eq!(block.statements[0].span, span(2, 44));
        let StatementKind::Function(function) = &block.statements[0].kind else {
            panic!("expected a nested function");
        };
        assert_eq!(function.name, span(5, 11));
        let value = block
            .value
            .as_deref()
            .expect("block should have a final value");
        assert!(matches!(&value.kind, ExpressionKind::Call { .. }));
        assert_eq!(value.span, span(45, 58));
    }

    #[test]
    fn parses_empty_lambdas_with_default_unit_returns() {
        let expression = parse("lambda() {}").expect("lambda should parse");

        assert_eq!(expression.span, span(0, 11));
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
        assert_eq!(body, Block::new(Vec::new(), None, span(9, 11)));
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

        assert_eq!(expression.span, span(0, 64));
        assert_eq!(parameters.len(), 2);
        assert_eq!(
            parameters[0].qualifiers,
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const)
        );
        assert_eq!(parameters[0].span, span(7, 17));
        let FunctionParameterKind::Named {
            name,
            type_annotation,
        } = &parameters[0].kind
        else {
            panic!("expected a named parameter");
        };
        assert_eq!(*name, span(7, 12));
        assert!(matches!(
            &type_annotation.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(type_annotation.span, span(14, 17));
        assert_eq!(
            parameters[1].qualifiers,
            BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Mut)
        );
        assert_eq!(parameters[1].span, span(19, 37));
        let FunctionParameterKind::Named {
            name,
            type_annotation,
        } = &parameters[1].kind
        else {
            panic!("expected a named parameter");
        };
        assert_eq!(*name, span(23, 29));
        let TypeKind::Named { name, .. } = &type_annotation.kind else {
            panic!("expected a named parameter type");
        };
        assert_eq!(*name, span(31, 37));
        assert_eq!(type_annotation.span, span(31, 37));
        let return_type = return_type.expect("return type should be explicit");
        assert!(matches!(
            return_type.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(return_type.span, span(43, 46));
        assert_eq!(body.span, span(47, 64));
        assert_eq!(body.statements.len(), 1);
        assert_eq!(body.statements[0].span, span(49, 62));
        let StatementKind::Return(Some(value)) = &body.statements[0].kind else {
            panic!("expected a value-bearing return");
        };
        assert!(matches!(&value.kind, ExpressionKind::Identifier));
        assert_eq!(value.span, span(56, 61));
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
        assert_eq!(initializer.span, span(18, 57));
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
        assert_eq!(callee.span, span(0, 35));

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
        assert_eq!(expression.span, span(0, 11));
        assert_eq!(statement.span, span(0, 12));

        assert_eq!(
            parse_statement_source("lambda() {}"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                span: span(11, 11),
            }))
        );

        assert_eq!(
            parse("{ lambda() {} value }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Identifier,
                },
                span: span(14, 19),
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
                span(6, 6),
            ),
            (
                "lambda(value) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightParen,
                },
                span(12, 13),
            ),
            (
                "lambda(: int) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Colon,
                },
                span(7, 8),
            ),
            (
                "lambda(value:) {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::RightParen,
                },
                span(13, 14),
            ),
            (
                "lambda(value: int {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::LeftBrace,
                },
                span(18, 19),
            ),
            (
                "lambda() -> {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::LeftBrace,
                },
                span(12, 13),
            ),
            (
                "lambda() -> int",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(15, 15),
            ),
            (
                "lambda() {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(10, 10),
            ),
            (
                "lambda(self) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::SelfValue,
                },
                span(7, 11),
            ),
            (
                "lambda(mut self) {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::SelfValue,
                },
                span(11, 15),
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
                span(2, 2),
            ),
            (
                "fn name",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftParen,
                    found: TokenKind::Eof,
                },
                span(7, 7),
            ),
            (
                "fn f(value) -> () {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightParen,
                },
                span(10, 11),
            ),
            (
                "fn f(value:) -> () {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::RightParen,
                },
                span(11, 12),
            ),
            (
                "fn f() () {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::LeftParen,
                },
                span(7, 8),
            ),
            (
                "fn f() -> {}",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::LeftBrace,
                },
                span(10, 11),
            ),
            (
                "fn f() -> ()",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(12, 12),
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
            ("return", TokenKind::Eof, span(6, 6)),
            ("return value", TokenKind::Eof, span(12, 12)),
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
                span: span(9, 10),
            }))
        );
    }

    #[test]
    fn parses_deferred_and_coroutine_calls() {
        let statement =
            parse_statement_source("defer cleanup();").expect("defer statement should parse");
        let StatementKind::Defer(call) = statement.kind else {
            panic!("expected a defer statement");
        };
        assert_eq!(statement.span, span(0, 16));
        assert_eq!(call.span, span(6, 15));
        assert!(matches!(
            call.kind,
            ExpressionKind::Call {
                callee,
                arguments,
            } if matches!(callee.kind, ExpressionKind::Identifier) && arguments.is_empty()
        ));

        let statement = parse_statement_source("co service.process(request);")
            .expect("coroutine statement should parse");
        let StatementKind::Coroutine(call) = statement.kind else {
            panic!("expected a coroutine statement");
        };
        assert_eq!(statement.span, span(0, 28));
        assert_eq!(call.span, span(3, 27));
        assert!(matches!(
            call.kind,
            ExpressionKind::Call {
                callee,
                arguments,
            } if matches!(callee.kind, ExpressionKind::MemberAccess { .. })
                && arguments.len() == 1
        ));
    }

    #[test]
    fn call_only_statements_accept_composed_callees_and_arguments() {
        for source in [
            "defer factory()();",
            "co lambda(value: int) { consume(value); }(item);",
        ] {
            let statement = parse_statement_source(source)
                .expect("an outermost call expression should be accepted");
            let call = match statement.kind {
                StatementKind::Defer(call) | StatementKind::Coroutine(call) => call,
                _ => panic!("expected a call-only statement"),
            };
            assert!(matches!(call.kind, ExpressionKind::Call { .. }));
        }

        let block = parse("{ defer cleanup(first + second); co service.process(load()?); }")
            .expect("call-only statements should parse in executable blocks");
        let ExpressionKind::Block(block) = block.kind else {
            panic!("expected a block");
        };
        assert_eq!(block.statements.len(), 2);
        assert!(matches!(&block.statements[0].kind, StatementKind::Defer(_)));
        assert!(matches!(
            &block.statements[1].kind,
            StatementKind::Coroutine(_)
        ));
        assert!(block.value.is_none());
    }

    #[test]
    fn call_only_statements_reject_non_call_operands() {
        for (source, expected_kind) in [
            ("defer worker;", ParseErrorKind::ExpectedDeferredCall),
            ("co service.run;", ParseErrorKind::ExpectedCoroutineCall),
            ("defer 1 + 2;", ParseErrorKind::ExpectedDeferredCall),
            ("co target = value;", ParseErrorKind::ExpectedCoroutineCall),
            ("defer {};", ParseErrorKind::ExpectedDeferredCall),
            ("co cleanup()?;", ParseErrorKind::ExpectedCoroutineCall),
        ] {
            let operand_start = source.find(' ').expect("test source has an operand") + 1;
            let operand_end = source.len() - 1;
            assert_eq!(
                parse_statement_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: span(operand_start, operand_end),
                })),
                "incorrect diagnostic for {source}",
            );
        }
    }

    #[test]
    fn call_only_statements_require_an_operand_and_semicolon() {
        for (source, expected_kind, expected_span) in [
            ("defer;", ParseErrorKind::ExpectedDeferredCall, span(5, 6)),
            ("co;", ParseErrorKind::ExpectedCoroutineCall, span(2, 3)),
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
            parse_statement_source("defer cleanup()"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                span: span(15, 15),
            }))
        );
        assert_eq!(
            parse("{ co run() }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span: span(11, 12),
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
                    qualifiers: BindingQualifiers::new(
                        BindingMutability::Const,
                        ValueCapability::Const,
                    ),
                    name: span(6, 11),
                    type_annotation: None,
                    initializer: integer(span(14, 16)),
                },
                span(0, 17),
            ))
        );
    }

    #[test]
    fn parses_mut_binding_with_an_explicit_type() {
        let statement = parse_statement_source("mut value: int = 1 + 2;")
            .expect("annotated mutable binding should parse");
        let StatementKind::Binding {
            qualifiers,
            name,
            type_annotation: Some(type_annotation),
            initializer,
        } = statement.kind
        else {
            panic!("expected an annotated binding statement");
        };

        assert_eq!(
            qualifiers,
            BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Mut)
        );
        assert_eq!(name, span(4, 9));
        assert_eq!(
            type_annotation,
            TypeSyntax::new(TypeKind::Primitive(PrimitiveType::Int), span(11, 14),)
        );
        assert!(matches!(
            initializer.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(initializer.span, span(17, 22));
        assert_eq!(statement.span, span(0, 23));
    }

    #[test]
    fn parses_all_binding_and_value_qualifier_combinations() {
        for (source, expected) in [
            (
                "const value = 1;",
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const),
            ),
            (
                "mut value = 1;",
                BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Mut),
            ),
            (
                "const vmut value = 1;",
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut),
            ),
            (
                "mut vconst value = 1;",
                BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Const),
            ),
        ] {
            let statement = parse_statement_source(source).expect("binding should parse");
            let StatementKind::Binding { qualifiers, .. } = statement.kind else {
                panic!("expected a binding statement");
            };

            assert_eq!(qualifiers, expected, "incorrect qualifiers for {source}");
        }
    }

    #[test]
    fn value_capability_precedes_an_annotated_binding_name() {
        for (source, expected) in [
            (
                "const vmut user: User = other;",
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut),
            ),
            (
                "mut vconst user: User = other;",
                BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Const),
            ),
        ] {
            let statement = parse_statement_source(source).expect("binding should parse");
            let StatementKind::Binding {
                qualifiers,
                type_annotation: Some(type_annotation),
                ..
            } = statement.kind
            else {
                panic!("expected an annotated binding statement");
            };

            assert_eq!(qualifiers, expected, "incorrect qualifiers for {source}");
            assert!(matches!(type_annotation.kind, TypeKind::Named { .. }));
        }

        parse_statement_source("const value: Queue<mut User> = other;")
            .expect("nested mutable type arguments should remain valid");
    }

    #[test]
    fn value_capability_applies_to_an_annotated_callable_binding() {
        let statement = parse_statement_source(
            "const vmut callback: fn(mut User) -> () = make_callback();",
        )
        .expect("mutable callable binding should parse");
        let StatementKind::Binding {
            qualifiers,
            type_annotation: Some(type_annotation),
            ..
        } = statement.kind
        else {
            panic!("expected an annotated binding statement");
        };

        assert_eq!(
            qualifiers,
            BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut)
        );
        let TypeKind::Callable {
            parameters,
            return_type,
        } = type_annotation.kind
        else {
            panic!("expected a callable type annotation");
        };
        assert_eq!(parameters.len(), 1);
        assert!(matches!(
            &parameters[0].kind,
            TypeKind::Mutable(inner)
                if matches!(&inner.kind, TypeKind::Named { .. })
        ));
        assert!(matches!(
            return_type.kind,
            TypeKind::Primitive(PrimitiveType::Unit)
        ));
    }

    #[test]
    fn parses_binding_qualifiers_on_named_parameters() {
        let source = concat!(
            "fn use(",
            "plain: User, ",
            "const explicit: User, ",
            "mut both: User, ",
            "const vmut fixed_mut: User, ",
            "mut vconst moving_const: User",
            ") {}",
        );
        let statement = parse_statement_source(source).expect("parameters should parse");
        let StatementKind::Function(function) = statement.kind else {
            panic!("expected a function");
        };

        assert_eq!(
            function
                .parameters
                .iter()
                .map(|parameter| parameter.qualifiers)
                .collect::<Vec<_>>(),
            vec![
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const),
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Const),
                BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Mut),
                BindingQualifiers::new(BindingMutability::Const, ValueCapability::Mut),
                BindingQualifiers::new(BindingMutability::Mut, ValueCapability::Const),
            ]
        );
    }

    #[test]
    fn rejects_invalid_or_misplaced_value_capabilities() {
        for (source, expected_kind, needle) in [
            (
                "const vconst value = 1;",
                ParseErrorKind::InvalidBindingQualifiers {
                    binding: TokenKind::Const,
                    value: TokenKind::VConst,
                },
                "vconst",
            ),
            (
                "mut vmut value = 1;",
                ParseErrorKind::InvalidBindingQualifiers {
                    binding: TokenKind::Mut,
                    value: TokenKind::VMut,
                },
                "vmut",
            ),
            (
                "vmut value = 1;",
                ParseErrorKind::ValueCapabilityWithoutBinding {
                    found: TokenKind::VMut,
                },
                "vmut",
            ),
            (
                "const value: mut User = other;",
                ParseErrorKind::BindingValueCapabilityMustPrecedeName,
                "mut User",
            ),
            (
                "fn use(value: mut User) {}",
                ParseErrorKind::BindingValueCapabilityMustPrecedeName,
                "mut User",
            ),
            (
                "fn use(const vmut self) {}",
                ParseErrorKind::InvalidReceiverQualifiers,
                "vmut",
            ),
            (
                "fn use(mut vconst self) {}",
                ParseErrorKind::InvalidReceiverQualifiers,
                "vconst",
            ),
            (
                "fn use(const self) {}",
                ParseErrorKind::InvalidReceiverQualifiers,
                "const",
            ),
            (
                "fn use(vmut self) {}",
                ParseErrorKind::ValueCapabilityWithoutBinding {
                    found: TokenKind::VMut,
                },
                "vmut",
            ),
        ] {
            let start = source.find(needle).expect("diagnostic text should exist");
            assert_eq!(
                parse_statement_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: span(start, start + needle.len()),
                })),
                "incorrect diagnostic for {source}",
            );
        }
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
        assert_eq!(expression.span, span(0, 19));
        assert_eq!(statement.span, span(0, 20));
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
                span(6, 7),
            ),
            (
                "const value;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Assign,
                    found: TokenKind::Semicolon,
                },
                span(11, 12),
            ),
            (
                "const value = ;",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Semicolon,
                },
                span(14, 15),
            ),
            (
                "mut value: = 1;",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Assign,
                },
                span(11, 12),
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
                span: span(7, 13),
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
        assert_eq!(condition.span, span(3, 8));
        assert_eq!(then_branch.span, span(9, 14));
        assert_eq!(then_branch.value.as_deref(), Some(&integer(span(11, 12))));
        assert_eq!(else_branch, None);
        assert_eq!(expression.span, span(0, 14));
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

        assert_eq!(then_branch.span, span(9, 14));
        assert_eq!(else_branch.span, span(20, 25));
        assert_eq!(expression.span, span(0, 25));
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

        assert_eq!(expression.span, span(0, source.len()));
        assert_eq!(nested.span, span(20, source.len()));
        assert_eq!(condition.span, span(23, 29));
        assert_eq!(final_branch.span, span(41, 46));
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
        assert_eq!(block.statements[0].span, span(2, 13));
    }

    #[test]
    fn reports_malformed_conditionals() {
        for (source, expected_kind, expected_span) in [
            (
                "if",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                span(2, 2),
            ),
            (
                "if true",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(7, 7),
            ),
            (
                "if true value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Identifier,
                },
                span(8, 13),
            ),
            (
                "if true {} else value",
                ParseErrorKind::ExpectedElseBranch {
                    found: TokenKind::Identifier,
                },
                span(16, 21),
            ),
            (
                "if true {} else",
                ParseErrorKind::ExpectedElseBranch {
                    found: TokenKind::Eof,
                },
                span(15, 15),
            ),
            (
                "else {}",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Else,
                },
                span(0, 4),
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
            Ok(Statement::new(StatementKind::Break(None), span(0, 6),))
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
        assert_eq!(value.span, span(6, 15));
        assert_eq!(statement.span, span(0, 16));
    }

    #[test]
    fn parses_continue_statements() {
        assert_eq!(
            parse_statement_source("continue;"),
            Ok(Statement::new(StatementKind::Continue, span(0, 9)))
        );
    }

    #[test]
    fn parses_infinite_loops() {
        let expression = parse("loop {}").expect("infinite loop should parse");
        let ExpressionKind::Loop { body } = expression.kind else {
            panic!("expected an infinite loop");
        };

        assert_eq!(body, Block::new(Vec::new(), None, span(5, 7)));
        assert_eq!(expression.span, span(0, 7));

        let expression = parse("loop { break 42; }").expect("loop with break should parse");
        let ExpressionKind::Loop { body } = expression.kind else {
            panic!("expected an infinite loop");
        };
        assert_eq!(body.statements.len(), 1);
        assert_eq!(body.statements[0].span, span(7, 16));
        assert!(matches!(
            &body.statements[0].kind,
            StatementKind::Break(Some(Expression {
                kind: ExpressionKind::Literal(LiteralKind::Integer),
                ..
            }))
        ));
        assert!(body.value.is_none());
        assert_eq!(expression.span, span(0, 18));
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
        assert_eq!(condition.span, span(6, 11));
        assert_eq!(body.span, span(12, 25));
        assert!(matches!(&body.statements[0].kind, StatementKind::Continue));
        assert_eq!(else_branch, None);
        assert_eq!(expression.span, span(0, 25));

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
        assert_eq!(body.span, span(12, 14));
        assert_eq!(else_branch.span, span(20, 25));
        assert_eq!(expression.span, span(0, source.len()));
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

        assert_eq!(expression.span, span(0, 17));
        assert_eq!(binding, span(4, 5));
        assert_eq!(start.as_ref(), &integer(span(9, 10)));
        assert_eq!(end.as_ref(), &integer(span(12, 14)));
        assert_eq!(inclusivity, RangeInclusivity::Exclusive);
        assert_eq!(body, Block::new(Vec::new(), None, span(15, 17)));
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

        assert_eq!(expression.span, span(0, 52));
        assert_eq!(binding, span(4, 9));
        assert_eq!(start.span, span(13, 18));
        assert_eq!(end.span, span(21, 26));
        assert_eq!(inclusivity, RangeInclusivity::Inclusive);
        assert_eq!(body.span, span(27, 40));
        assert!(matches!(&body.statements[0].kind, StatementKind::Continue));
        assert_eq!(else_branch.span, span(46, 52));
        assert_eq!(else_branch.value.as_deref(), Some(&integer(span(48, 50))));
    }

    #[test]
    fn range_bounds_accept_unary_and_postfix_expressions() {
        let expression = parse("for i in -start()..limit.value {}")
            .expect("simple computed bounds should parse");
        let ExpressionKind::RangeFor { start, end, .. } = expression.kind else {
            panic!("expected a range loop");
        };

        assert_eq!(start.span, span(9, 17));
        assert!(matches!(
            start.kind,
            ExpressionKind::Unary {
                operator: UnaryOperator::Negate,
                operand,
            } if matches!(operand.kind, ExpressionKind::Call { .. })
        ));
        assert_eq!(end.span, span(19, 30));
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
        assert_eq!(loop_else.value.as_deref(), Some(&integer(span(50, 51))));
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
        assert_eq!(callee.span, span(0, 16));

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
        assert_eq!(block.statements[0].span, span(2, 19));
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
                span(3, 3),
            ),
            (
                "for mut i in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Mut,
                },
                span(4, 7),
            ),
            (
                "for const i in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Const,
                },
                span(4, 9),
            ),
            (
                "for in 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::In,
                },
                span(4, 6),
            ),
            (
                "for i 0..1 {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::In,
                    found: TokenKind::IntegerLiteral,
                },
                span(6, 7),
            ),
            (
                "for i in ..1 {}",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::DotDot,
                },
                span(9, 11),
            ),
            (
                "for i in {}..1 {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                span(9, 11),
            ),
            (
                "for i in start + 1..limit {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                span(15, 16),
            ),
            (
                "for i in 0 {}",
                ParseErrorKind::ExpectedRangeOperator {
                    found: TokenKind::LeftBrace,
                },
                span(11, 12),
            ),
            (
                "for i in 0",
                ParseErrorKind::ExpectedRangeOperator {
                    found: TokenKind::Eof,
                },
                span(10, 10),
            ),
            (
                "for i in 0.. {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                span(13, 15),
            ),
            (
                "for i in 0..limit + 1 {}",
                ParseErrorKind::RangeBoundRequiresGrouping,
                span(18, 19),
            ),
            (
                "for i in 0..if ready { 1 } else { 2 } {} else { 3 }",
                ParseErrorKind::RangeBoundRequiresGrouping,
                span(12, 37),
            ),
            (
                "for i in 0..",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                span(12, 12),
            ),
            (
                "for i in 0..1",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(13, 13),
            ),
            (
                "for i in 0..1 {} else if true {}",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::If,
                },
                span(22, 24),
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
                span: span(17, 25),
            }))
        );

        assert_eq!(
            parse("0..10"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::UnexpectedToken {
                    found: TokenKind::DotDot,
                },
                span: span(1, 3),
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
        assert_eq!(block.statements[0].span, span(2, 10));
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
                span(4, 4),
            ),
            (
                "while",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Eof,
                },
                span(5, 5),
            ),
            (
                "while true",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Eof,
                },
                span(10, 10),
            ),
            (
                "while true {} else value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::LeftBrace,
                    found: TokenKind::Identifier,
                },
                span(19, 24),
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
                span(5, 5),
            ),
            (
                "continue",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Eof,
                },
                span(8, 8),
            ),
            (
                "continue value;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::Identifier,
                },
                span(9, 14),
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
                span: span(8, 12),
            }))
        );

        assert_eq!(
            parse("{ break }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span: span(8, 9),
            }))
        );
    }

    #[test]
    fn parses_empty_and_value_producing_blocks() {
        let empty_span = span(0, 2);
        assert_eq!(
            parse("{}"),
            Ok(Expression::new(
                ExpressionKind::Block(Block::new(Vec::new(), None, empty_span)),
                empty_span,
            ))
        );

        let value_span = span(0, 6);
        assert_eq!(
            parse("{ 42 }"),
            Ok(Expression::new(
                ExpressionKind::Block(Block::new(
                    Vec::new(),
                    Some(Box::new(integer(span(2, 4)))),
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
            Statement::new(StatementKind::Expression(integer(span(2, 4))), span(2, 5),)
        );
        assert_eq!(block.value, None);
        assert_eq!(block.span, span(0, 7));
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
        assert_eq!(block.statements[0].span, span(2, 14));
        assert_eq!(block.statements[1].span, span(15, 22));
        assert!(matches!(
            &block.statements[0].kind,
            StatementKind::Binding {
                qualifiers: BindingQualifiers {
                    binding: BindingMutability::Const,
                    value: ValueCapability::Const,
                },
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
            span(23, 28),
        );
        assert_eq!(block.span, span(0, source.len()));
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
                span: span(8, 14),
            }))
        );

        assert_eq!(
            parse("{ ; }"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Semicolon,
                },
                span: span(2, 3),
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
                    span: span(source.len(), source.len()),
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
                span: span(3, 8),
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
    fn parses_named_struct_construction_with_full_field_expressions() {
        let source = "Position { y: calculate_y(), x: 1 + 2, }";
        let expression = parse(source).expect("struct construction should parse");
        let ExpressionKind::StructConstruction { name, fields } = expression.kind else {
            panic!("expected named struct construction");
        };

        assert_eq!(expression.span, span(0, source.len()));
        assert_eq!(&source[name.start..name.end], "Position");
        assert_eq!(fields.len(), 2);
        assert_eq!(&source[fields[0].name.start..fields[0].name.end], "y");
        assert!(matches!(fields[0].value.kind, ExpressionKind::Call { .. }));
        assert_eq!(&source[fields[1].name.start..fields[1].name.end], "x");
        assert!(matches!(
            fields[1].value.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert_eq!(
            fields[1].span,
            span(fields[1].name.start, fields[1].value.span.end)
        );

        let empty = parse("Marker {}").expect("empty construction should parse");
        assert!(matches!(
            empty.kind,
            ExpressionKind::StructConstruction { fields, .. } if fields.is_empty()
        ));
    }

    #[test]
    fn named_struct_construction_composes_with_postfix_and_infix_expressions() {
        let expression =
            parse("Position { x: 1 }.magnitude() + 2").expect("struct construction should compose");
        let ExpressionKind::Binary {
            operator: BinaryOperator::Add,
            left,
            ..
        } = expression.kind
        else {
            panic!("expected addition");
        };
        let ExpressionKind::Call { callee, .. } = left.kind else {
            panic!("expected method call");
        };
        let ExpressionKind::MemberAccess { object, .. } = callee.kind else {
            panic!("expected member access");
        };
        assert!(matches!(
            object.kind,
            ExpressionKind::StructConstruction { .. }
        ));
    }

    #[test]
    fn grouped_struct_construction_is_allowed_in_conditions_and_range_bounds() {
        let expression =
            parse("if (Position { x: 1 }) {}").expect("grouped condition should parse");
        let ExpressionKind::If { condition, .. } = expression.kind else {
            panic!("expected conditional");
        };
        assert!(matches!(
            condition.kind,
            ExpressionKind::Group(ref inner)
                if matches!(inner.kind, ExpressionKind::StructConstruction { .. })
        ));

        let expression = parse("while (struct { value = 1; }) {}")
            .expect("grouped anonymous struct condition should parse");
        let ExpressionKind::While { condition, .. } = expression.kind else {
            panic!("expected while loop");
        };
        assert!(matches!(
            condition.kind,
            ExpressionKind::Group(ref inner)
                if matches!(inner.kind, ExpressionKind::AnonymousStruct { .. })
        ));

        let expression = parse("for i in 0..(Position { x: 1 }) {}")
            .expect("grouped construction range bound should parse");
        let ExpressionKind::RangeFor { end, .. } = expression.kind else {
            panic!("expected range loop");
        };
        assert!(matches!(end.kind, ExpressionKind::Group(_)));
    }

    #[test]
    fn parses_anonymous_struct_fields_and_methods() {
        let source = concat!(
            "struct { ",
            "x: float = 10.0; ",
            "label = \"point\"; ",
            "fn magnitude(self) -> float { self.x }",
            " }",
        );
        let expression = parse(source).expect("anonymous struct should parse");
        let ExpressionKind::AnonymousStruct { members } = expression.kind else {
            panic!("expected anonymous struct");
        };

        assert_eq!(expression.span, span(0, source.len()));
        assert_eq!(members.len(), 3);
        let AnonymousStructMember::Field(x) = &members[0] else {
            panic!("expected x field");
        };
        let AnonymousStructMember::Field(label) = &members[1] else {
            panic!("expected label field");
        };
        let AnonymousStructMember::Method(method) = &members[2] else {
            panic!("expected magnitude method");
        };
        assert_eq!(&source[x.name.start..x.name.end], "x");
        assert!(matches!(
            &x.type_annotation,
            Some(TypeSyntax {
                kind: TypeKind::Primitive(PrimitiveType::Float),
                ..
            })
        ));
        assert_eq!(&source[label.name.start..label.name.end], "label");
        assert!(label.type_annotation.is_none());
        assert_eq!(&source[method.name.start..method.name.end], "magnitude");
        assert!(matches!(
            method.parameters[0].kind,
            FunctionParameterKind::Receiver { .. }
        ));

        let expression = parse("struct {}.method() + other")
            .expect("empty anonymous struct should compose with postfix and infix syntax");
        assert!(matches!(
            expression.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
    }

    #[test]
    fn reports_malformed_named_struct_construction() {
        for (source, expected_kind, expected_span) in [
            (
                "Position {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span(10, 10),
            ),
            (
                "Position { x }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Colon,
                    found: TokenKind::RightBrace,
                },
                span(13, 14),
            ),
            (
                "Position { x: }",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::RightBrace,
                },
                span(14, 15),
            ),
            (
                "Position { x: 1 y: 2 }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Identifier,
                },
                span(16, 17),
            ),
            (
                "Position { x: 1,",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span(16, 16),
            ),
            (
                "Position { x: 1",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(15, 15),
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
    fn reports_malformed_anonymous_structs() {
        for (source, expected_kind, expected_span) in [
            (
                "struct {",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(8, 8),
            ),
            (
                "struct { const value = 1; }",
                ParseErrorKind::ExpectedAnonymousStructMember {
                    found: TokenKind::Const,
                },
                span(9, 14),
            ),
            (
                "struct { value: = 1; }",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Assign,
                },
                span(16, 17),
            ),
            (
                "struct { value: int 1; }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Assign,
                    found: TokenKind::IntegerLiteral,
                },
                span(20, 21),
            ),
            (
                "struct { value = ; }",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Semicolon,
                },
                span(17, 18),
            ),
            (
                "struct { value = 1 }",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Semicolon,
                    found: TokenKind::RightBrace,
                },
                span(19, 20),
            ),
            (
                "struct { value = 1;",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBrace,
                    found: TokenKind::Eof,
                },
                span(19, 19),
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
    fn parses_parameterized_builtin_associated_calls() {
        for (source, expected_builtin, type_count, argument_count) in [
            ("Queue<int>::new()", BuiltinType::Queue, 1, 0),
            ("Vector<string>::new()", BuiltinType::Vector, 1, 0),
            ("Map<string, Vector<int>>::new()", BuiltinType::Map, 2, 0),
            ("Error::new(value)", BuiltinType::Error, 0, 1),
            ("Error<string>::new(value)", BuiltinType::Error, 1, 1),
        ] {
            let expression = parse(source).expect("built-in associated call should parse");
            let ExpressionKind::Call { callee, arguments } = expression.kind else {
                panic!("expected an ordinary call for {source}");
            };
            let ExpressionKind::AssociatedAccess { owner, member } = callee.kind else {
                panic!("expected built-in associated access for {source}");
            };
            let TypeKind::Builtin {
                builtin,
                arguments: type_arguments,
            } = owner.kind
            else {
                panic!("expected a built-in owner for {source}");
            };
            assert_eq!(builtin, expected_builtin);
            assert_eq!(type_arguments.len(), type_count);
            assert_eq!(&source[member.start..member.end], "new");
            assert_eq!(arguments.len(), argument_count);
            assert_eq!(expression.span, span(0, source.len()));
        }

        let expression =
            parse("Queue<int>::new").expect("a built-in associated function value should parse");
        assert!(matches!(
            expression.kind,
            ExpressionKind::AssociatedAccess {
                owner: TypeSyntax {
                    kind: TypeKind::Builtin {
                        builtin: BuiltinType::Queue,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn builtin_associated_calls_compose_with_expressions() {
        let expression = parse("Vector<int>::new().length() + 1")
            .expect("built-in associated call should compose with postfix and infix syntax");
        assert!(matches!(
            expression.kind,
            ExpressionKind::Binary {
                left,
                operator: BinaryOperator::Add,
                ..
            } if matches!(left.kind, ExpressionKind::Call { .. })
        ));

        let expression = parse("Map<string, int>::new()[key]")
            .expect("built-in associated call should compose with indexing");
        assert!(matches!(expression.kind, ExpressionKind::Index { .. }));

        let expression = parse("Error<int>::new(value + 1)")
            .expect("Error should accept a complete payload expression");
        let ExpressionKind::Call { arguments, .. } = expression.kind else {
            panic!("expected Error associated call");
        };
        assert!(matches!(&arguments[0].kind, ExpressionKind::Binary { .. }));
    }

    #[test]
    fn builtin_new_uses_ordinary_call_syntax() {
        for source in ["defer Queue<int>::new();", "co Error::new(value);"] {
            let statement = parse_statement_source(source)
                .expect("built-in associated calls should be valid call-only statements");
            assert!(matches!(
                statement.kind,
                StatementKind::Defer(Expression {
                    kind: ExpressionKind::Call { .. },
                    ..
                }) | StatementKind::Coroutine(Expression {
                    kind: ExpressionKind::Call { .. },
                    ..
                })
            ));
        }
    }

    #[test]
    fn builtin_associated_call_arities_are_left_to_type_checking() {
        for source in ["Error::new()", "Queue<int>::new(value)"] {
            assert!(matches!(
                parse(source)
                    .expect("ordinary call syntax should parse")
                    .kind,
                ExpressionKind::Call { .. }
            ));
        }
    }

    #[test]
    fn reports_malformed_builtin_associated_access() {
        for (source, expected_kind, expected_span) in [
            (
                "Queue::new()",
                ParseErrorKind::ExpectedBuiltinTypeArguments {
                    builtin: BuiltinType::Queue,
                    found: TokenKind::DoubleColon,
                },
                span(5, 7),
            ),
            (
                "Vector<int>",
                ParseErrorKind::ExpectedBuiltinAssociatedAccess {
                    builtin: BuiltinType::Vector,
                    found: TokenKind::Eof,
                },
                span(11, 11),
            ),
            (
                "Map<int>::new()",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Map,
                    expected: 2,
                    found: 1,
                },
                span(0, 8),
            ),
            (
                "Queue<int, string>::new()",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Queue,
                    expected: 1,
                    found: 2,
                },
                span(0, 18),
            ),
            (
                "Error<int, string>::new(value)",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Error,
                    expected: 1,
                    found: 2,
                },
                span(0, 18),
            ),
            (
                "Error<>::new(value)",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Greater,
                },
                span(6, 7),
            ),
            (
                "Error value",
                ParseErrorKind::ExpectedBuiltinAssociatedAccess {
                    builtin: BuiltinType::Error,
                    found: TokenKind::Identifier,
                },
                span(6, 11),
            ),
            (
                "Queue<int>()",
                ParseErrorKind::ExpectedBuiltinAssociatedAccess {
                    builtin: BuiltinType::Queue,
                    found: TokenKind::LeftParen,
                },
                span(10, 11),
            ),
            (
                "Error(value)",
                ParseErrorKind::ExpectedBuiltinAssociatedAccess {
                    builtin: BuiltinType::Error,
                    found: TokenKind::LeftParen,
                },
                span(5, 6),
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
    fn parses_primitive_conversions() {
        for (source, expected_target) in [
            ("int(value)", PrimitiveType::Int),
            ("float(value)", PrimitiveType::Float),
            ("char(value)", PrimitiveType::Char),
            ("string(value)", PrimitiveType::String),
        ] {
            let expression = parse(source).expect("primitive conversion should parse");
            let ExpressionKind::PrimitiveConversion { target, value } = expression.kind else {
                panic!("expected a primitive conversion for {source}");
            };
            let value_start = source.find("value").expect("source contains value");

            assert_eq!(target, expected_target);
            assert_eq!(expression.span, span(0, source.len()));
            assert_eq!(value.span, span(value_start, value_start + 5));
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
                span(3, 3),
            ),
            (
                "float()",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::RightParen,
                },
                span(6, 7),
            ),
            (
                "char(value,)",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Comma,
                },
                span(10, 11),
            ),
            (
                "string(value",
                ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Eof,
                },
                span(12, 12),
            ),
            (
                "bool(value)",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Bool,
                },
                span(0, 4),
            ),
            (
                "bytes(value)",
                ParseErrorKind::ExpectedExpression {
                    found: TokenKind::Bytes,
                },
                span(0, 5),
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
                integer(span(0, 1)),
                BinaryOperator::Add,
                binary(
                    integer(span(4, 5)),
                    BinaryOperator::Multiply,
                    integer(span(8, 9)),
                    span(4, 9),
                ),
                span(0, 9),
            ))
        );
    }

    #[test]
    fn binary_operators_associate_to_the_left() {
        assert_eq!(
            parse("8 - 3 - 1"),
            Ok(binary(
                binary(
                    integer(span(0, 1)),
                    BinaryOperator::Subtract,
                    integer(span(4, 5)),
                    span(0, 5),
                ),
                BinaryOperator::Subtract,
                integer(span(8, 9)),
                span(0, 9),
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
        assert_eq!(left.span, span(0, 7));
        assert_eq!(expression.span, span(0, 11));
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
        assert_eq!(expression.span, span(0, 5));

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
        assert_eq!(member, span(13, 19));

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
    fn parses_associated_access_and_calls() {
        let expression = parse("Point::origin().x").expect("associated call should parse");
        let ExpressionKind::MemberAccess { object, member } = expression.kind else {
            panic!("expected instance member access after the associated call");
        };
        assert_eq!(member, span(16, 17));

        let ExpressionKind::Call { callee, arguments } = object.kind else {
            panic!("expected associated function call");
        };
        assert!(arguments.is_empty());
        let ExpressionKind::AssociatedAccess { owner, member } = callee.kind else {
            panic!("expected associated access as the callee");
        };
        assert_eq!(member, span(7, 13));
        assert!(matches!(
            owner.kind,
            TypeKind::Named { name, ref arguments }
                if name == span(0, 5) && arguments.is_empty()
        ));

        let expression = parse("bytes::concat").expect("primitive associated access should parse");
        assert!(matches!(
            expression.kind,
            ExpressionKind::AssociatedAccess {
                owner: TypeSyntax {
                    kind: TypeKind::Primitive(PrimitiveType::Bytes),
                    ..
                },
                member,
            } if member == span(7, 13)
        ));
    }

    #[test]
    fn associated_access_requires_a_member_name() {
        assert_eq!(
            parse("Point::"),
            Err(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Identifier,
                    found: TokenKind::Eof,
                },
                span: span(7, 7),
            }
            .into())
        );
    }

    #[test]
    fn parses_open_and_bounded_slices() {
        for (source, has_start, has_end) in [
            ("items[1..4]", true, true),
            ("items[..4]", false, true),
            ("items[1..]", true, false),
            ("items[..]", false, false),
        ] {
            let expression = parse(source).expect("slice should parse");
            let ExpressionKind::Slice { start, end, object } = expression.kind else {
                panic!("expected a slice for {source}");
            };
            assert_eq!(start.is_some(), has_start);
            assert_eq!(end.is_some(), has_end);
            assert!(matches!(object.kind, ExpressionKind::Identifier));
            assert_eq!(expression.span, span(0, source.len()));
        }
    }

    #[test]
    fn slice_bounds_accept_negative_and_full_expressions() {
        let expression = parse("items[-2..-1]").expect("negative slice bounds should parse");
        let ExpressionKind::Slice {
            start: Some(start),
            end: Some(end),
            ..
        } = expression.kind
        else {
            panic!("expected a bounded slice");
        };
        assert!(matches!(
            start.kind,
            ExpressionKind::Unary {
                operator: UnaryOperator::Negate,
                ..
            }
        ));
        assert!(matches!(
            end.kind,
            ExpressionKind::Unary {
                operator: UnaryOperator::Negate,
                ..
            }
        ));

        let expression = parse("items[start + offset..end - trim]")
            .expect("complete bound expressions should parse");
        let ExpressionKind::Slice {
            start: Some(start),
            end: Some(end),
            ..
        } = expression.kind
        else {
            panic!("expected a bounded slice");
        };
        assert!(matches!(
            start.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert!(matches!(
            end.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Subtract,
                ..
            }
        ));
    }

    #[test]
    fn slices_compose_with_postfix_and_infix_expressions() {
        let expression = parse("items[1..-1].length() + 1")
            .expect("slice should compose with postfix and infix syntax");
        let ExpressionKind::Binary {
            left,
            operator: BinaryOperator::Add,
            ..
        } = expression.kind
        else {
            panic!("expected addition at the root");
        };
        let ExpressionKind::Call { callee, .. } = left.kind else {
            panic!("expected a call before addition");
        };
        let ExpressionKind::MemberAccess { object, .. } = callee.kind else {
            panic!("expected member access before the call");
        };
        assert!(matches!(object.kind, ExpressionKind::Slice { .. }));
    }

    #[test]
    fn slice_assignment_remains_an_assignment_for_later_validation() {
        let expression = parse("items[1..2] = replacement")
            .expect("slice assignment should remain syntactically representable");
        let ExpressionKind::Assignment { target, .. } = expression.kind else {
            panic!("expected an assignment at the root");
        };
        assert!(matches!(target.kind, ExpressionKind::Slice { .. }));
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
                span: span(6, 6),
            }))
        );

        assert_eq!(
            parse("items[]"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedExpression {
                    found: TokenKind::RightBracket,
                },
                span: span(6, 7),
            }))
        );

        for source in ["items[..=end]", "items[start..=end]"] {
            let delimiter = source
                .find("..=")
                .expect("source has an inclusive delimiter");
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::InclusiveSliceNotSupported,
                    span: span(delimiter, delimiter + 3),
                }))
            );
        }

        for source in ["items[1..", "items[.."] {
            assert_eq!(
                parse(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedToken {
                        expected: TokenKind::RightBracket,
                        found: TokenKind::Eof,
                    },
                    span: span(source.len(), source.len()),
                }))
            );
        }

        let source = "items[1..2..3]";
        let delimiter = source.rfind("..").expect("source has a second delimiter");
        assert_eq!(
            parse(source),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightBracket,
                    found: TokenKind::DotDot,
                },
                span: span(delimiter, delimiter + 2),
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

        assert_eq!(expression.span, span(0, 12));
        let ExpressionKind::TypeTest { value, type_syntax } = expression.kind else {
            panic!("expected a type test");
        };
        assert_eq!(value.span, span(0, 5));
        assert!(matches!(value.kind, ExpressionKind::Identifier));
        assert_eq!(
            type_syntax,
            TypeSyntax::new(TypeKind::Primitive(PrimitiveType::Int), span(9, 12),)
        );

        let expression =
            parse("result is Error<string> | none").expect("union type test should parse");
        let ExpressionKind::TypeTest { type_syntax, .. } = expression.kind else {
            panic!("expected a type test");
        };
        assert_eq!(type_syntax.span, span(10, 30));
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
        assert_eq!(type_syntax.span, span(13, 16));

        let expression = parse("service.read() is bytes").expect("postfix operand should parse");
        let ExpressionKind::TypeTest { value, .. } = expression.kind else {
            panic!("expected a type test");
        };
        assert!(matches!(value.kind, ExpressionKind::Call { .. }));
        assert_eq!(value.span, span(0, 14));
    }

    #[test]
    fn type_tests_parse_in_conditional_conditions() {
        let expression = parse("if value is int { value }").expect("conditional should parse");
        let ExpressionKind::If { condition, .. } = expression.kind else {
            panic!("expected a conditional");
        };
        assert!(matches!(condition.kind, ExpressionKind::TypeTest { .. }));
        assert_eq!(condition.span, span(3, 15));
    }

    #[test]
    fn reports_missing_and_trailing_type_test_syntax() {
        for (source, expected_kind, expected_span) in [
            (
                "value is",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Eof,
                },
                span(8, 8),
            ),
            (
                "value is + 1",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Plus,
                },
                span(9, 10),
            ),
            (
                "value is int |",
                ParseErrorKind::ExpectedType {
                    found: TokenKind::Eof,
                },
                span(14, 14),
            ),
            (
                "value is int trailing",
                ParseErrorKind::UnexpectedToken {
                    found: TokenKind::Identifier,
                },
                span(13, 21),
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
                span: span(3, 3),
            }))
        );

        assert_eq!(
            parse("(1 + 2"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::RightParen,
                    found: TokenKind::Eof,
                },
                span: span(6, 6),
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
                span: span(2, 3),
            }))
        );
    }

    #[test]
    fn returns_lexical_errors_from_the_iterator() {
        assert_eq!(
            parse("\"bad\\q\""),
            Err(FrontendError::Lexical(LexError {
                kind: LexErrorKind::InvalidEscape,
                span: span(4, 6),
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
                    name: span(0, 4),
                    arguments: Vec::new(),
                },
                span(0, 4),
            ))
        );
    }

    #[test]
    fn parses_unit_and_grouped_types() {
        assert_eq!(
            parse_type_source("()"),
            Ok(TypeSyntax::new(
                TypeKind::Primitive(PrimitiveType::Unit),
                span(0, 2),
            ))
        );

        let type_syntax = parse_type_source("(int | none)").expect("grouped type should parse");
        let TypeKind::Group(inner) = type_syntax.kind else {
            panic!("expected a grouped type");
        };
        assert!(matches!(inner.kind, TypeKind::Union { .. }));
        assert_eq!(type_syntax.span, span(0, 12));
    }

    #[test]
    fn parses_parameterized_and_nested_parameterized_types() {
        let type_syntax = parse_type_source("Map<string, Error<int | none>>")
            .expect("built-in type should parse");
        let TypeKind::Builtin {
            builtin: BuiltinType::Map,
            arguments,
        } = type_syntax.kind
        else {
            panic!("expected Map type");
        };

        assert_eq!(arguments.len(), 2);
        let TypeKind::Builtin {
            builtin: BuiltinType::Error,
            arguments: error_arguments,
        } = &arguments[1].kind
        else {
            panic!("expected a nested Error type");
        };
        assert_eq!(error_arguments.len(), 1);
        assert!(matches!(&error_arguments[0].kind, TypeKind::Union { .. }));

        let type_syntax = parse_type_source("Container<string, Result<int>>")
            .expect("ordinary parameterized named types should remain valid");
        let TypeKind::Named { arguments, .. } = type_syntax.kind else {
            panic!("expected an ordinary named type");
        };
        assert_eq!(arguments.len(), 2);
        assert!(matches!(&arguments[1].kind, TypeKind::Named { .. }));
    }

    #[test]
    fn reports_invalid_builtin_type_arity() {
        for (source, expected_kind, expected_span) in [
            (
                "Queue",
                ParseErrorKind::ExpectedBuiltinTypeArguments {
                    builtin: BuiltinType::Queue,
                    found: TokenKind::Eof,
                },
                span(5, 5),
            ),
            (
                "Vector<int, string>",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Vector,
                    expected: 1,
                    found: 2,
                },
                span(0, 19),
            ),
            (
                "Map<int>",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Map,
                    expected: 2,
                    found: 1,
                },
                span(0, 8),
            ),
            (
                "Error<int, string>",
                ParseErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Error,
                    expected: 1,
                    found: 2,
                },
                span(0, 18),
            ),
        ] {
            assert_eq!(
                parse_type_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: expected_kind,
                    span: expected_span,
                })),
                "incorrect diagnostic for {source}",
            );
        }
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
    fn value_capability_keywords_are_not_type_qualifiers() {
        for (source, found) in [
            ("vconst User", TokenKind::VConst),
            ("vmut User", TokenKind::VMut),
        ] {
            assert_eq!(
                parse_type_source(source),
                Err(FrontendError::Syntax(ParseError {
                    kind: ParseErrorKind::ExpectedType { found },
                    span: span(0, source.find(' ').expect("type has a space")),
                }))
            );
        }
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
                span: span(5, 5),
            }))
        );

        assert_eq!(
            parse_type_source("Error<int"),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::ExpectedToken {
                    expected: TokenKind::Greater,
                    found: TokenKind::Eof,
                },
                span: span(9, 9),
            }))
        );
    }

    #[test]
    fn assigns_unique_module_qualified_ids_to_every_parsed_ast_node() {
        let mut registry = SourceModuleRegistry::new();
        let module = registry.add(concat!(
            "struct Item { value: int, }\n",
            "interface Reader { fn read(self, fallback: Item) -> Item; }\n",
            "fn main(item: Item) -> Item {\n",
            "    const wrapped = struct { value: Item = item; };\n",
            "    Item { value: wrapped.value }\n",
            "}",
        ));
        let mut context = ParseContext::new(module.module_id());
        let program = parse_program(&mut context, Lexer::new(&module))
            .expect("representative program should parse");
        let debug = format!("{program:?}");

        assert!(!debug.contains(&format!("node_id: {}", u32::MAX)));
        for node_id in 0..context.next_node_id {
            assert!(
                debug.contains(&format!(
                    "NodeId {{ module_id: {:?}, node_id: {node_id} }}",
                    module.module_id()
                )),
                "missing allocated node ID {node_id} from parsed AST",
            );
        }
    }

    #[test]
    fn equal_numeric_node_ids_in_different_modules_are_distinct() {
        let mut registry = SourceModuleRegistry::new();
        let first = registry.add("value");
        let second = registry.add("value");
        let mut first_context = ParseContext::new(first.module_id());
        let mut second_context = ParseContext::new(second.module_id());

        let first_expression = parse_expression(&mut first_context, Lexer::new(&first))
            .expect("first expression should parse");
        let second_expression = parse_expression(&mut second_context, Lexer::new(&second))
            .expect("second expression should parse");

        assert_eq!(first_expression.id.node_id, second_expression.id.node_id);
        assert_ne!(first_expression.id, second_expression.id);
    }

    #[test]
    fn reusing_a_parse_context_keeps_fragment_node_ids_unique() {
        let module = SourceModuleRegistry::new().add("value");
        let mut context = ParseContext::new(module.module_id());

        let first = parse_expression(&mut context, Lexer::new(&module))
            .expect("first fragment should parse");
        let second = parse_expression(&mut context, Lexer::new(&module))
            .expect("second fragment should parse");

        assert_eq!(first.id.node_id, 0);
        assert_eq!(second.id.node_id, 1);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn rejects_tokens_from_a_different_module() {
        let mut registry = SourceModuleRegistry::new();
        let token_module = registry.add("value");
        let context_module = registry.add("value");
        let mut context = ParseContext::new(context_module.module_id());

        assert_eq!(
            parse_expression(&mut context, Lexer::new(&token_module)),
            Err(FrontendError::Syntax(ParseError {
                kind: ParseErrorKind::TokenModuleMismatch {
                    expected: context_module.module_id(),
                    found: token_module.module_id(),
                },
                span: Span::new(token_module.module_id(), 0, 5),
            }))
        );
    }

    #[test]
    #[should_panic(expected = "node ID space exhausted")]
    fn node_id_overflow_panics_clearly() {
        let module = SourceModuleRegistry::new().add("value");
        let mut context = ParseContext {
            module_id: module.module_id(),
            next_node_id: u32::MAX,
            leave_ids_unassigned: false,
        };

        let _ = parse_expression(&mut context, Lexer::new(&module));
    }
}
