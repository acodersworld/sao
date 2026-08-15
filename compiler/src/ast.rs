use crate::source::{ModuleId, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub module_id: ModuleId,
    pub node_id: u32,
}

impl NodeId {
    pub(crate) const UNASSIGNED: Self = Self {
        module_id: ModuleId::PRELUDE,
        node_id: u32::MAX,
    };
}

/// One complete source program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub id: NodeId,
    pub declarations: Vec<Declaration>,
    pub span: Span,
}

impl Program {
    #[must_use]
    pub const fn new(declarations: Vec<Declaration>, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            declarations,
            span,
        }
    }
}

/// A declaration in a program's file-level namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    Function(Function),
    Struct(StructDeclaration),
    Interface(InterfaceDeclaration),
}

/// A named structural interface declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDeclaration {
    pub id: NodeId,
    pub name: Span,
    pub requirements: Vec<InterfaceMethodRequirement>,
    pub span: Span,
}

impl InterfaceDeclaration {
    #[must_use]
    pub const fn new(
        name: Span,
        requirements: Vec<InterfaceMethodRequirement>,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            requirements,
            span,
        }
    }
}

/// One bodyless method signature required by an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceMethodRequirement {
    pub id: NodeId,
    pub name: Span,
    pub parameters: Vec<FunctionParameter>,
    /// An explicit return annotation. Its absence defaults to unit.
    pub return_type: Option<TypeSyntax>,
    pub span: Span,
}

impl InterfaceMethodRequirement {
    #[must_use]
    pub const fn new(
        name: Span,
        parameters: Vec<FunctionParameter>,
        return_type: Option<TypeSyntax>,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            parameters,
            return_type,
            span,
        }
    }
}

/// A named nominal struct declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDeclaration {
    pub id: NodeId,
    pub name: Span,
    pub members: Vec<StructMember>,
    pub span: Span,
}

impl StructDeclaration {
    #[must_use]
    pub const fn new(name: Span, members: Vec<StructMember>, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            members,
            span,
        }
    }
}

/// A field or function declared by a named struct. A function with a receiver
/// is an instance method; a receiverless function is associated with the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructMember {
    Field(StructField),
    Function(Function),
}

/// A field declared by a named struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructField {
    pub id: NodeId,
    pub name: Span,
    pub type_annotation: TypeSyntax,
    pub span: Span,
}

impl StructField {
    #[must_use]
    pub const fn new(name: Span, type_annotation: TypeSyntax, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            type_annotation,
            span,
        }
    }
}

/// One field initializer in a named struct construction expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructFieldInitializer {
    pub id: NodeId,
    pub name: Span,
    pub value: Expression,
    pub span: Span,
}

impl StructFieldInitializer {
    #[must_use]
    pub const fn new(name: Span, value: Expression, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            value,
            span,
        }
    }
}

/// A field or method in an anonymous struct expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnonymousStructMember {
    Field(AnonymousStructField),
    Method(Function),
}

/// An initialized field in an anonymous struct expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonymousStructField {
    pub id: NodeId,
    pub name: Span,
    pub type_annotation: Option<TypeSyntax>,
    pub initializer: Expression,
    pub span: Span,
}

impl AnonymousStructField {
    #[must_use]
    pub const fn new(
        name: Span,
        type_annotation: Option<TypeSyntax>,
        initializer: Expression,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            type_annotation,
            initializer,
            span,
        }
    }
}

/// An expression in the source-level syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub id: NodeId,
    pub kind: ExpressionKind,
    pub span: Span,
}

impl Expression {
    #[must_use]
    pub const fn new(kind: ExpressionKind, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            kind,
            span,
        }
    }
}

/// A type expression in the source-level syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSyntax {
    pub id: NodeId,
    pub kind: TypeKind,
    pub span: Span,
}

impl TypeSyntax {
    #[must_use]
    pub const fn new(kind: TypeKind, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            kind,
            span,
        }
    }
}

/// A statement in the source-level syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub id: NodeId,
    pub kind: StatementKind,
    pub span: Span,
}

impl Statement {
    #[must_use]
    pub const fn new(kind: StatementKind, span: Span) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            kind,
            span,
        }
    }
}

/// A braced executable block containing statements and an optional final value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: NodeId,
    pub statements: Vec<Statement>,
    pub value: Option<Box<Expression>>,
    pub span: Span,
}

impl Block {
    #[must_use]
    pub const fn new(
        statements: Vec<Statement>,
        value: Option<Box<Expression>>,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            statements,
            value,
            span,
        }
    }
}

/// A named function declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: NodeId,
    pub name: Span,
    pub parameters: Vec<FunctionParameter>,
    /// An explicit return annotation. Its absence defaults to unit.
    pub return_type: Option<TypeSyntax>,
    pub body: Block,
    pub span: Span,
}

impl Function {
    #[must_use]
    pub const fn new(
        name: Span,
        parameters: Vec<FunctionParameter>,
        return_type: Option<TypeSyntax>,
        body: Block,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            name,
            parameters,
            return_type,
            body,
            span,
        }
    }
}

/// A parameter in a named function declaration or lambda expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionParameter {
    pub id: NodeId,
    /// The independent mutability of the parameter binding and access through
    /// the value held by that binding.
    pub qualifiers: BindingQualifiers,
    pub kind: FunctionParameterKind,
    pub span: Span,
}

impl FunctionParameter {
    #[must_use]
    pub const fn new(
        qualifiers: BindingQualifiers,
        kind: FunctionParameterKind,
        span: Span,
    ) -> Self {
        Self {
            id: NodeId::UNASSIGNED,
            qualifiers,
            kind,
            span,
        }
    }
}

/// The syntactic form of a function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionParameterKind {
    Named {
        name: Span,
        type_annotation: TypeSyntax,
    },
    /// A method receiver written as `self` or `mut self`.
    Receiver { name: Span },
}

/// The syntax accepted after an `else` keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalElse {
    /// A braced `else` body.
    Block(Block),
    /// An `else if` expression. The parser guarantees an [`ExpressionKind::If`].
    If(Box<Expression>),
}

/// The syntax represented by a [`Statement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementKind {
    Binding {
        qualifiers: BindingQualifiers,
        name: Span,
        type_annotation: Option<TypeSyntax>,
        initializer: Expression,
    },
    Expression(Expression),
    Function(Function),
    /// Registers a call to run when the current lexical block exits. The
    /// parser guarantees that the expression is an [`ExpressionKind::Call`].
    Defer(Expression),
    /// Starts a call in a new coroutine. The parser guarantees that the
    /// expression is an [`ExpressionKind::Call`].
    Coroutine(Expression),
    Break(Option<Expression>),
    Continue,
    Return(Option<Expression>),
}

/// Whether a binding's storage is fixed or may be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingMutability {
    Const,
    Mut,
}

/// Whether access through a value is const or mutable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueCapability {
    Const,
    Mut,
}

/// The two independent qualifiers carried by a local or parameter binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingQualifiers {
    pub binding: BindingMutability,
    pub value: ValueCapability,
}

impl BindingQualifiers {
    #[must_use]
    pub const fn new(binding: BindingMutability, value: ValueCapability) -> Self {
        Self { binding, value }
    }
}

/// Whether a range loop includes its end bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeInclusivity {
    Exclusive,
    Inclusive,
}

/// The syntax represented by a [`TypeSyntax`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Primitive(PrimitiveType),
    Builtin {
        builtin: BuiltinType,
        arguments: Vec<TypeSyntax>,
    },
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

/// A reserved compiler-known parameterized type constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinType {
    Queue,
    Vector,
    Map,
    Error,
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
    Block(Block),
    If {
        condition: Box<Expression>,
        then_branch: Block,
        else_branch: Option<ConditionalElse>,
    },
    Loop {
        body: Block,
    },
    While {
        condition: Box<Expression>,
        body: Block,
        else_branch: Option<Block>,
    },
    RangeFor {
        binding: Span,
        start: Box<Expression>,
        end: Box<Expression>,
        inclusivity: RangeInclusivity,
        body: Block,
        else_branch: Option<Block>,
    },
    Lambda {
        /// Lambda parameters are always [`FunctionParameterKind::Named`].
        parameters: Vec<FunctionParameter>,
        /// An explicit return annotation. Its absence defaults to unit.
        return_type: Option<TypeSyntax>,
        body: Block,
    },
    PrimitiveConversion {
        target: PrimitiveType,
        value: Box<Expression>,
    },
    StructConstruction {
        name: Span,
        fields: Vec<StructFieldInitializer>,
    },
    AnonymousStruct {
        members: Vec<AnonymousStructMember>,
    },
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    MemberAccess {
        object: Box<Expression>,
        member: Span,
    },
    /// Selects a receiverless function from a type with `Type::function`.
    AssociatedAccess {
        owner: TypeSyntax,
        member: Span,
    },
    Index {
        object: Box<Expression>,
        index: Box<Expression>,
    },
    Slice {
        object: Box<Expression>,
        /// Omitted by `value[..end]` and `value[..]`.
        start: Option<Box<Expression>>,
        /// Omitted by `value[start..]` and `value[..]`.
        end: Option<Box<Expression>>,
    },
    Try {
        expression: Box<Expression>,
    },
    TypeTest {
        value: Box<Expression>,
        type_syntax: TypeSyntax,
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
