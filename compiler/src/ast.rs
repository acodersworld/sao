use crate::lexer::Span;

/// An expression in the source-level syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

impl Expression {
    #[must_use]
    pub const fn new(kind: ExpressionKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// A type expression in the source-level syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSyntax {
    pub kind: TypeKind,
    pub span: Span,
}

impl TypeSyntax {
    #[must_use]
    pub const fn new(kind: TypeKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The syntax represented by a [`TypeSyntax`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Primitive(PrimitiveType),
    Named {
        name: Span,
        arguments: Vec<TypeSyntax>,
    },
    Mutable(Box<TypeSyntax>),
    Group(Box<TypeSyntax>),
    Callable {
        parameters: Vec<TypeSyntax>,
        return_type: Box<TypeSyntax>,
    },
    /// An unparenthesized `&` chain in source order, with at least two members.
    Intersection {
        members: Vec<TypeSyntax>,
    },
    /// An unparenthesized `|` chain in source order, with at least two members.
    Union {
        members: Vec<TypeSyntax>,
    },
}

/// A built-in type with a reserved source spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    Unit,
    None,
    Int,
    Float,
    Bool,
    Char,
    String,
    Bytes,
}

/// The syntax represented by an [`Expression`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    Identifier,
    SelfValue,
    Literal(LiteralKind),
    Group(Box<Expression>),
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    MemberAccess {
        object: Box<Expression>,
        member: Span,
    },
    Index {
        object: Box<Expression>,
        index: Box<Expression>,
    },
    Try {
        expression: Box<Expression>,
    },
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    Binary {
        left: Box<Expression>,
        operator: BinaryOperator,
        right: Box<Expression>,
    },
    Assignment {
        target: Box<Expression>,
        operator: AssignmentOperator,
        value: Box<Expression>,
    },
}

/// A literal's source-level category. Its original spelling is retained by the
/// containing expression's span and is decoded during semantic analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiteralKind {
    Unit,
    Integer,
    Float,
    Boolean(bool),
    Character,
    String,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOperator {
    Negate,
    LogicalNot,
    BitwiseNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    ShiftLeft,
    ShiftRight,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    BitwiseAnd,
    BitwiseXor,
    BitwiseOr,
    LogicalAnd,
    LogicalOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignmentOperator {
    Assign,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    BitwiseAnd,
    BitwiseXor,
    BitwiseOr,
    ShiftLeft,
    ShiftRight,
}
