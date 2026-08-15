use std::fmt;
use std::iter::FusedIterator;

use logos::{Lexer as LogosLexer, Logos, SpannedIter};

use crate::source::{ModuleId, SourceModule};

pub use crate::source::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LexErrorKind {
    UnexpectedCharacter,
    UnterminatedBlockComment,
    UnterminatedStringLiteral,
    UnterminatedCharacterLiteral,
    InvalidEscape,
    NonAsciiLiteral,
    InvalidLiteralCharacter,
    EmptyCharacterLiteral,
    MultipleCharacterLiteral,
}

impl fmt::Display for LexErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnexpectedCharacter => "unexpected character",
            Self::UnterminatedBlockComment => "unterminated block comment",
            Self::UnterminatedStringLiteral => "unterminated string literal",
            Self::UnterminatedCharacterLiteral => "unterminated character literal",
            Self::InvalidEscape => "invalid escape sequence",
            Self::NonAsciiLiteral => "literal contains a non-ASCII character",
            Self::InvalidLiteralCharacter => "literal contains an invalid control character",
            Self::EmptyCharacterLiteral => "character literal is empty",
            Self::MultipleCharacterLiteral => "character literal contains more than one character",
        };

        formatter.write_str(message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawLexError {
    kind: LexErrorKind,
    relative_span: Option<RelativeSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RelativeSpan {
    start: usize,
    end: usize,
}

impl RawLexError {
    const fn whole_token(kind: LexErrorKind) -> Self {
        Self {
            kind,
            relative_span: None,
        }
    }

    const fn at(kind: LexErrorKind, start: usize, end: usize) -> Self {
        Self {
            kind,
            relative_span: Some(RelativeSpan { start, end }),
        }
    }
}

impl Default for RawLexError {
    fn default() -> Self {
        Self::whole_token(LexErrorKind::UnexpectedCharacter)
    }
}

macro_rules! define_token_kinds {
    ($( $(#[$attribute:meta])* $variant:ident ),+ $(,)?) => {
        #[derive(Debug, Logos, Clone, Copy, PartialEq, Eq, Hash)]
        #[logos(error = RawLexError)]
        // Ignore insignificant horizontal and vertical ASCII whitespace.
        #[logos(skip r"[ \t\r\n\x0C]+")]
        // Ignore a line comment through the next line ending or end of input.
        #[logos(skip(r"//[^\r\n]*", allow_greedy = true))]
        // Ignore nested block comments using the callback below.
        #[logos(skip(r"/\*", lex_block_comment))]
        enum RawTokenKind {
            $(
                $(#[$attribute])*
                $variant,
            )+
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum TokenKind {
            $($variant,)+
            Eof,
        }

        impl From<RawTokenKind> for TokenKind {
            fn from(token: RawTokenKind) -> Self {
                match token {
                    $(RawTokenKind::$variant => Self::$variant,)+
                }
            }
        }
    };
}

define_token_kinds! {
    // Begins a named function declaration or a callable type.
    #[token("fn")] Fn,
    // Begins an anonymous function expression that may capture lexical bindings.
    #[token("lambda")] Lambda,
    // Begins a named or anonymous structure declaration.
    #[token("struct")] Struct,
    // Begins a named structural interface declaration.
    #[token("interface")] Interface,
    // Declares a fixed binding with const value access by default.
    #[token("const")] Const,
    // Declares a reassignable binding with mutable value access by default.
    #[token("mut")] Mut,
    // Selects const value access when it differs from binding mutability.
    #[token("vconst")] VConst,
    // Selects mutable value access when it differs from binding mutability.
    #[token("vmut")] VMut,
    // Refers to the receiver of the current method.
    #[token("self")] SelfValue,
    // Begins a conditional expression.
    #[token("if")] If,
    // Begins the alternative branch of a conditional or loop expression.
    #[token("else")] Else,
    // Begins an unconditional loop expression.
    #[token("loop")] Loop,
    // Begins a condition-controlled loop expression.
    #[token("while")] While,
    // Begins an ascending integer range loop expression.
    #[token("for")] For,
    // Separates a range loop's induction binding from its bounds.
    #[token("in")] In,
    // Exits the innermost loop, optionally carrying a value.
    #[token("break")] Break,
    // Continues with the next iteration of the innermost loop.
    #[token("continue")] Continue,
    // Returns from the current function, optionally carrying a value.
    #[token("return")] Return,
    // Tests whether a value has a specified type.
    #[token("is")] Is,
    // Starts a function or method call as a coroutine.
    #[token("co")] Co,
    // Registers a call to run when the current lexical scope exits.
    #[token("defer")] Defer,
    // Represents the Boolean true literal.
    #[token("true")] True,
    // Represents the Boolean false literal.
    #[token("false")] False,
    // Represents the singleton absence type and value.
    #[token("none")] None,

    // Names the signed 64-bit integer primitive type.
    #[token("int")] Int,
    // Names the binary64 floating-point primitive type.
    #[token("float")] Float,
    // Names the Boolean primitive type.
    #[token("bool")] Bool,
    // Names the one-byte ASCII character primitive type.
    #[token("char")] Char,
    // Names the mutable ASCII string primitive type.
    #[token("string")] String,
    // Names the mutable byte-sequence built-in type.
    #[token("bytes")] Bytes,

    // Names the compiler-known FIFO queue type constructor.
    #[token("Queue")] Queue,
    // Names the compiler-known vector type constructor.
    #[token("Vector")] Vector,
    // Names the compiler-known map type constructor.
    #[token("Map")] Map,
    // Names the compiler-known recoverable-error type constructor.
    #[token("Error")] Error,

    // Opens a parenthesized expression, call argument list, or unit value.
    #[token("(")] LeftParen,
    // Closes a parenthesized expression, call argument list, or unit value.
    #[token(")")] RightParen,
    // Opens a block, structure body, or object expression.
    #[token("{")] LeftBrace,
    // Closes a block, structure body, or object expression.
    #[token("}")] RightBrace,
    // Opens an indexing or slicing expression.
    #[token("[")] LeftBracket,
    // Closes an indexing or slicing expression.
    #[token("]")] RightBracket,
    // Separates parameters, arguments, fields, and related list elements.
    #[token(",")] Comma,
    // Terminates a statement and discards an expression's value.
    #[token(";")] Semicolon,
    // Selects an associated member from a type.
    #[token("::")] DoubleColon,
    // Separates a name from its type or field initializer context.
    #[token(":")] Colon,
    // Separates the bounds of an exclusive range or sequence slice.
    #[token("..")] DotDot,
    // Separates inclusive range bounds; inclusive slices reject this token.
    #[token("..=")] DotDotEqual,
    // Selects a member from a value.
    #[token(".")] Dot,
    // Introduces a function's return type.
    #[token("->")] Arrow,
    // Spells the postfix Try operator that propagates an Error value.
    #[token("?")] Question,

    // Assigns a value to a mutable destination.
    #[token("=")] Assign,
    // Adds numbers or concatenates strings.
    #[token("+")] Plus,
    // Subtracts values or negates a numeric operand.
    #[token("-")] Minus,
    // Multiplies numeric operands.
    #[token("*")] Star,
    // Divides numeric operands.
    #[token("/")] Slash,
    // Computes the integer remainder.
    #[token("%")] Percent,
    // Negates a Boolean operand.
    #[token("!")] Bang,
    // Computes bitwise AND or joins types by intersection.
    #[token("&")] Ampersand,
    // Computes bitwise OR or joins types by union.
    #[token("|")] Pipe,
    // Computes bitwise exclusive OR.
    #[token("^")] Caret,
    // Computes bitwise complement.
    #[token("~")] Tilde,
    // Computes short-circuiting logical AND.
    #[token("&&")] LogicalAnd,
    // Computes short-circuiting logical OR.
    #[token("||")] LogicalOr,
    // Shifts an integer left.
    #[token("<<")] ShiftLeft,
    // Shifts an integer right.
    #[token(">>")] ShiftRight,
    // Compares two values for equality.
    #[token("==")] Equal,
    // Compares two values for inequality.
    #[token("!=")] NotEqual,
    // Compares values or opens a parameterized type's argument list.
    #[token("<")] Less,
    // Compares whether the left value is less than or equal to the right value.
    #[token("<=")] LessEqual,
    // Compares values or closes a parameterized type's argument list.
    #[token(">")] Greater,
    // Compares whether the left value is greater than or equal to the right value.
    #[token(">=")] GreaterEqual,
    // Adds to a mutable destination and assigns the result.
    #[token("+=")] PlusAssign,
    // Subtracts from a mutable destination and assigns the result.
    #[token("-=")] MinusAssign,
    // Multiplies a mutable destination and assigns the result.
    #[token("*=")] StarAssign,
    // Divides a mutable destination and assigns the result.
    #[token("/=")] SlashAssign,
    // Computes remainder on a mutable destination and assigns the result.
    #[token("%=")] PercentAssign,
    // Applies bitwise AND to a mutable destination and assigns the result.
    #[token("&=")] AmpersandAssign,
    // Applies bitwise OR to a mutable destination and assigns the result.
    #[token("|=")] PipeAssign,
    // Applies bitwise exclusive OR to a mutable destination and assigns the result.
    #[token("^=")] CaretAssign,
    // Shifts a mutable integer destination left and assigns the result.
    #[token("<<=")] ShiftLeftAssign,
    // Shifts a mutable integer destination right and assigns the result.
    #[token(">>=")] ShiftRightAssign,

    // Recognizes a case-sensitive ASCII user-defined name.
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")] Identifier,
    // Recognizes a decimal float containing a fractional part and optional exponent.
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?")]
    // Also recognizes a decimal float written with an exponent but no decimal point.
    #[regex(r"[0-9]+[eE][+-]?[0-9]+")] FloatLiteral,
    // Recognizes an unsigned spelling of a decimal integer; signs are separate tokens.
    #[regex(r"[0-9]+")] IntegerLiteral,
    // Consumes and validates an ASCII string literal using the callback below.
    #[token("\"", lex_string_literal)] StringLiteral,
    // Consumes and validates a single ASCII character literal using the callback below.
    #[token("'", lex_character_literal)] CharacterLiteral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    #[must_use]
    pub const fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }

    #[must_use]
    pub fn text(self, module: &SourceModule) -> &str {
        module
            .text(self.span)
            .expect("token span must belong to its source module")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LexError {
    pub kind: LexErrorKind,
    pub span: Span,
}

pub type LexResult = Result<Vec<Token>, Vec<LexError>>;

/// A lazy token iterator over one registered SAO source module.
///
/// The iterator always yields one explicit `Eof` token before it is exhausted.
/// Malformed source produces an error item, after which iteration continues at
/// the next token boundary.
#[must_use = "a lexer does nothing unless it is iterated"]
pub struct Lexer<'source> {
    inner: SpannedIter<'source, RawTokenKind>,
    module_id: ModuleId,
    source_len: usize,
    emitted_eof: bool,
}

impl<'source> Lexer<'source> {
    pub fn new(module: &'source SourceModule) -> Self {
        Self {
            inner: RawTokenKind::lexer(module.source()).spanned(),
            module_id: module.module_id(),
            source_len: module.source().len(),
            emitted_eof: false,
        }
    }
}

impl Iterator for Lexer<'_> {
    type Item = Result<Token, LexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((result, logos_span)) = self.inner.next() {
            let token_span = Span::new(self.module_id, logos_span.start, logos_span.end);

            return Some(match result {
                Ok(kind) => Ok(Token::new(kind.into(), token_span)),
                Err(error) => {
                    let error_span = error.relative_span.map_or(token_span, |relative| {
                        Span::new(
                            self.module_id,
                            token_span.start + relative.start,
                            token_span.start + relative.end,
                        )
                    });

                    Err(LexError {
                        kind: error.kind,
                        span: error_span,
                    })
                }
            });
        }

        if self.emitted_eof {
            return None;
        }

        self.emitted_eof = true;
        Some(Ok(Token::new(
            TokenKind::Eof,
            Span::new(self.module_id, self.source_len, self.source_len),
        )))
    }
}

impl FusedIterator for Lexer<'_> {}

/// Lexes an entire source module.
///
/// Returns the complete token stream when the source is lexically valid. If
/// any malformed regions are found, returns every lexical error instead. Use
/// [`Lexer`] directly to handle tokens and errors incrementally.
pub fn lex(module: &SourceModule) -> LexResult {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();

    for result in Lexer::new(module) {
        match result {
            Ok(token) => tokens.push(token),
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(errors)
    }
}

fn lex_block_comment(lexer: &mut LogosLexer<'_, RawTokenKind>) -> Result<(), RawLexError> {
    let remainder = lexer.remainder().as_bytes();
    let mut depth = 1_usize;
    let mut offset = 0_usize;

    while offset + 1 < remainder.len() {
        match (remainder[offset], remainder[offset + 1]) {
            (b'/', b'*') => {
                depth += 1;
                offset += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                offset += 2;

                if depth == 0 {
                    lexer.bump(offset);
                    return Ok(());
                }
            }
            _ => offset += 1,
        }
    }

    lexer.bump(remainder.len());
    Err(RawLexError::whole_token(
        LexErrorKind::UnterminatedBlockComment,
    ))
}

fn lex_string_literal(lexer: &mut LogosLexer<'_, RawTokenKind>) -> Result<(), RawLexError> {
    lex_quoted_literal(lexer, b'"', QuotedLiteralKind::String)
}

fn lex_character_literal(lexer: &mut LogosLexer<'_, RawTokenKind>) -> Result<(), RawLexError> {
    lex_quoted_literal(lexer, b'\'', QuotedLiteralKind::Character)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotedLiteralKind {
    String,
    Character,
}

impl QuotedLiteralKind {
    const fn unterminated_error(self) -> LexErrorKind {
        match self {
            Self::String => LexErrorKind::UnterminatedStringLiteral,
            Self::Character => LexErrorKind::UnterminatedCharacterLiteral,
        }
    }
}

fn lex_quoted_literal(
    lexer: &mut LogosLexer<'_, RawTokenKind>,
    quote: u8,
    literal_kind: QuotedLiteralKind,
) -> Result<(), RawLexError> {
    let remainder = lexer.remainder();
    let bytes = remainder.as_bytes();
    let mut offset = 0_usize;
    let mut character_count = 0_usize;
    let mut first_error = None;

    while offset < bytes.len() {
        let byte = bytes[offset];

        if byte == quote {
            lexer.bump(offset + 1);

            if let Some(error) = first_error {
                return Err(error);
            }

            if literal_kind == QuotedLiteralKind::Character {
                return match character_count {
                    0 => Err(RawLexError::whole_token(
                        LexErrorKind::EmptyCharacterLiteral,
                    )),
                    1 => Ok(()),
                    _ => Err(RawLexError::whole_token(
                        LexErrorKind::MultipleCharacterLiteral,
                    )),
                };
            }

            return Ok(());
        }

        if matches!(byte, b'\n' | b'\r') {
            lexer.bump(offset);
            return Err(RawLexError::whole_token(literal_kind.unterminated_error()));
        }

        if byte == b'\\' {
            if offset + 1 >= bytes.len() {
                lexer.bump(bytes.len());
                return Err(RawLexError::whole_token(literal_kind.unterminated_error()));
            }

            let escaped = bytes[offset + 1];
            if escaped == b'x' {
                let escape_end = (offset + 4).min(bytes.len());
                if offset + 3 >= bytes.len()
                    || !bytes[offset + 2].is_ascii_hexdigit()
                    || !bytes[offset + 3].is_ascii_hexdigit()
                {
                    first_error.get_or_insert(RawLexError::at(
                        LexErrorKind::InvalidEscape,
                        offset + 1,
                        escape_end + 1,
                    ));
                    offset += 2;
                    character_count += 1;
                    continue;
                }

                let value = hex_value(bytes[offset + 2]) * 16 + hex_value(bytes[offset + 3]);
                if value > 0x7f {
                    first_error.get_or_insert(RawLexError::at(
                        LexErrorKind::NonAsciiLiteral,
                        offset + 1,
                        offset + 5,
                    ));
                }

                offset += 4;
                character_count += 1;
                continue;
            }

            if !matches!(escaped, b'\\' | b'"' | b'\'' | b'n' | b'r' | b't' | b'0') {
                first_error.get_or_insert(RawLexError::at(
                    LexErrorKind::InvalidEscape,
                    offset + 1,
                    offset + 3,
                ));
            }

            offset += 2;
            character_count += 1;
            continue;
        }

        if !byte.is_ascii() {
            let character_length = remainder[offset..]
                .chars()
                .next()
                .expect("offset is within the source")
                .len_utf8();
            first_error.get_or_insert(RawLexError::at(
                LexErrorKind::NonAsciiLiteral,
                offset + 1,
                offset + 1 + character_length,
            ));
            offset += character_length;
            character_count += 1;
            continue;
        }

        if byte < 0x20 || byte == 0x7f {
            first_error.get_or_insert(RawLexError::at(
                LexErrorKind::InvalidLiteralCharacter,
                offset + 1,
                offset + 2,
            ));
        }

        offset += 1;
        character_count += 1;
    }

    lexer.bump(bytes.len());
    Err(RawLexError::whole_token(literal_kind.unterminated_error()))
}

const fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceModuleRegistry;

    fn module(source: &str) -> SourceModule {
        SourceModuleRegistry::new().add(source)
    }

    const fn span(start: usize, end: usize) -> Span {
        Span::new(ModuleId::TEST_SOURCE, start, end)
    }

    fn lex_source(source: &str) -> LexResult {
        lex(&module(source))
    }

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex_source(source)
            .expect("source should lex successfully")
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    fn streamed(source: &str) -> (Vec<Token>, Vec<LexError>) {
        let module = module(source);
        let mut tokens = Vec::new();
        let mut errors = Vec::new();

        for result in Lexer::new(&module) {
            match result {
                Ok(token) => tokens.push(token),
                Err(error) => errors.push(error),
            }
        }

        (tokens, errors)
    }

    #[test]
    fn streams_tokens_lazily_and_emits_eof_once() {
        let module = module("const value");
        let mut lexer = Lexer::new(&module);

        assert_eq!(
            lexer.next(),
            Some(Ok(Token::new(TokenKind::Const, span(0, 5))))
        );
        assert_eq!(
            lexer.next(),
            Some(Ok(Token::new(TokenKind::Identifier, span(6, 11))))
        );
        assert_eq!(
            lexer.next(),
            Some(Ok(Token::new(TokenKind::Eof, span(11, 11))))
        );
        assert_eq!(lexer.next(), None);
        assert_eq!(lexer.next(), None);
    }

    #[test]
    fn streaming_error_is_returned_as_err() {
        let module = module("\"bad\\q\"");
        let mut lexer = Lexer::new(&module);

        assert_eq!(
            lexer.next(),
            Some(Err(LexError {
                kind: LexErrorKind::InvalidEscape,
                span: span(4, 6),
            }))
        );
    }

    #[test]
    fn lexes_keywords_and_identifiers() {
        let source = concat!(
            "fn lambda struct interface const mut vconst vmut self if else loop while for in break continue ",
            "return is co defer true false none int float bool char string bytes ",
            "Queue Vector Map Error ",
            "name _private fnord vconstant vmutable",
        );

        assert_eq!(
            kinds(source),
            vec![
                TokenKind::Fn,
                TokenKind::Lambda,
                TokenKind::Struct,
                TokenKind::Interface,
                TokenKind::Const,
                TokenKind::Mut,
                TokenKind::VConst,
                TokenKind::VMut,
                TokenKind::SelfValue,
                TokenKind::If,
                TokenKind::Else,
                TokenKind::Loop,
                TokenKind::While,
                TokenKind::For,
                TokenKind::In,
                TokenKind::Break,
                TokenKind::Continue,
                TokenKind::Return,
                TokenKind::Is,
                TokenKind::Co,
                TokenKind::Defer,
                TokenKind::True,
                TokenKind::False,
                TokenKind::None,
                TokenKind::Int,
                TokenKind::Float,
                TokenKind::Bool,
                TokenKind::Char,
                TokenKind::String,
                TokenKind::Bytes,
                TokenKind::Queue,
                TokenKind::Vector,
                TokenKind::Map,
                TokenKind::Error,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_punctuation_and_operators_with_longest_match() {
        let source = concat!(
            "( ) { } [ ] , ; :: : . .. ..= -> ? ",
            "= + - * / % ! & | ^ ~ && || << >> == != < <= > >= ",
            "+= -= *= /= %= &= |= ^= <<= >>=",
        );

        assert_eq!(
            kinds(source),
            vec![
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::LeftBracket,
                TokenKind::RightBracket,
                TokenKind::Comma,
                TokenKind::Semicolon,
                TokenKind::DoubleColon,
                TokenKind::Colon,
                TokenKind::Dot,
                TokenKind::DotDot,
                TokenKind::DotDotEqual,
                TokenKind::Arrow,
                TokenKind::Question,
                TokenKind::Assign,
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::Bang,
                TokenKind::Ampersand,
                TokenKind::Pipe,
                TokenKind::Caret,
                TokenKind::Tilde,
                TokenKind::LogicalAnd,
                TokenKind::LogicalOr,
                TokenKind::ShiftLeft,
                TokenKind::ShiftRight,
                TokenKind::Equal,
                TokenKind::NotEqual,
                TokenKind::Less,
                TokenKind::LessEqual,
                TokenKind::Greater,
                TokenKind::GreaterEqual,
                TokenKind::PlusAssign,
                TokenKind::MinusAssign,
                TokenKind::StarAssign,
                TokenKind::SlashAssign,
                TokenKind::PercentAssign,
                TokenKind::AmpersandAssign,
                TokenKind::PipeAssign,
                TokenKind::CaretAssign,
                TokenKind::ShiftLeftAssign,
                TokenKind::ShiftRightAssign,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_conservative_decimal_numbers() {
        assert_eq!(
            kinds("0 001 42 1.0 12.34e-2 9E+7 -5 .5 1."),
            vec![
                TokenKind::IntegerLiteral,
                TokenKind::IntegerLiteral,
                TokenKind::IntegerLiteral,
                TokenKind::FloatLiteral,
                TokenKind::FloatLiteral,
                TokenKind::FloatLiteral,
                TokenKind::Minus,
                TokenKind::IntegerLiteral,
                TokenKind::Dot,
                TokenKind::IntegerLiteral,
                TokenKind::IntegerLiteral,
                TokenKind::Dot,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn distinguishes_ranges_from_members_and_decimal_numbers() {
        assert_eq!(
            kinds("0..10 0..=10 value.member 1.0 ..."),
            vec![
                TokenKind::IntegerLiteral,
                TokenKind::DotDot,
                TokenKind::IntegerLiteral,
                TokenKind::IntegerLiteral,
                TokenKind::DotDotEqual,
                TokenKind::IntegerLiteral,
                TokenKind::Identifier,
                TokenKind::Dot,
                TokenKind::Identifier,
                TokenKind::FloatLiteral,
                TokenKind::DotDot,
                TokenKind::Dot,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn preserves_literal_text_and_accepts_ascii_escapes() {
        let source = "\"hello\\n\\x00\" '\\x7f' '\\''";
        let module = module(source);
        let tokens = lex(&module).expect("valid literals should lex successfully");

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![
                TokenKind::StringLiteral,
                TokenKind::CharacterLiteral,
                TokenKind::CharacterLiteral,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[0].text(&module), "\"hello\\n\\x00\"");
        assert_eq!(tokens[1].text(&module), "'\\x7f'");
        assert_eq!(tokens[2].text(&module), "'\\''");
    }

    #[test]
    fn skips_line_and_nested_block_comments() {
        let source = concat!(
            "const /* outer /* nested */ still outer */ value ",
            "// Unicode is permitted in comments: \u{03c0}\r\n",
            "= 1;",
        );
        let tokens = lex_source(source).expect("comments should be skipped successfully");

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![
                TokenKind::Const,
                TokenKind::Identifier,
                TokenKind::Assign,
                TokenKind::IntegerLiteral,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn reports_unterminated_nested_block_comment_as_one_error() {
        let source = "const /* outer /* nested */";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::Const, TokenKind::Eof]
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, LexErrorKind::UnterminatedBlockComment);
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn reports_invalid_escape_and_resumes_after_literal() {
        let source = "\"bad\\q\" + 1";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::Plus, TokenKind::IntegerLiteral, TokenKind::Eof,]
        );
        assert_eq!(
            errors,
            vec![LexError {
                kind: LexErrorKind::InvalidEscape,
                span: span(4, 6),
            }]
        );
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn rejects_non_ascii_string_and_character_contents() {
        let source = "\"\u{00e9}\" '\u{00e9}'";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::Eof]
        );
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.kind == LexErrorKind::NonAsciiLiteral)
        );
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn validates_character_literal_length() {
        let source = "'' 'ab' 'a'";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::CharacterLiteral, TokenKind::Eof]
        );
        assert_eq!(errors[0].kind, LexErrorKind::EmptyCharacterLiteral);
        assert_eq!(errors[1].kind, LexErrorKind::MultipleCharacterLiteral);
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn unterminated_literal_stops_before_newline_and_lexing_continues() {
        let source = "\"abc\nconst";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::Const, TokenKind::Eof]
        );
        assert_eq!(errors[0].kind, LexErrorKind::UnterminatedStringLiteral);
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn reports_each_unrecognized_source_character_and_continues() {
        let source = "@ $";
        let (tokens, errors) = streamed(source);

        assert_eq!(
            tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
            vec![TokenKind::Eof]
        );
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.kind == LexErrorKind::UnexpectedCharacter)
        );
        assert_eq!(lex_source(source), Err(errors));
    }

    #[test]
    fn returns_byte_spans_and_an_explicit_eof_token() {
        let source = "const value = 10;";
        let tokens = lex_source(source).expect("valid source should lex successfully");

        assert_eq!(tokens[0].span, span(0, 5));
        assert_eq!(tokens[1].span, span(6, 11));
        assert_eq!(tokens[2].span, span(12, 13));
        assert_eq!(tokens[3].span, span(14, 16));
        assert_eq!(tokens[4].span, span(16, 17));
        assert_eq!(tokens[5].span, span(17, 17));
        assert!(tokens[5].span.is_empty());
    }
}
