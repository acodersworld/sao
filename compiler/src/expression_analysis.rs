//! Internal foundation for core expression type checking.
//!
//! This module intentionally has no public whole-program entry point yet. It
//! records expression, binding, call, callable-result, control-flow, and
//! contextual union facts needed by later increments without treating
//! not-yet-implemented expression forms as source errors.

// The whole-program entry point remains intentionally private and unconnected
// until the core-expression phase covers every expression form in its scope.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use crate::{
    ast::{
        AnonymousStructMember, AssignmentOperator, BinaryOperator, BindingMutability,
        BindingQualifiers, Block, BuiltinType, ConditionalElse, Declaration, Expression,
        ExpressionKind, Function, FunctionParameter, FunctionParameterKind, LiteralKind, NodeId,
        PrimitiveType, Program, ReceiverStorage, Statement, StatementKind, StructFieldInitializer,
        StructMember, TypeSyntax, UnaryOperator, ValueCapability,
    },
    context_resolution::ContextResolution,
    name_resolution::NameResolution,
    semantic_types::{
        AccessCapability, CopySemantics, SemanticType, StorageSemantics, TypeId, ValueCategory,
        ValueTransfer,
    },
    signature_collection::{
        InterfaceRequirementSignature, MethodId, ReceiverSignature, SignatureCollection,
        StructMemberSignatureKind, StructSignature,
    },
    source::{SourceModule, Span},
    symbol_table::{SymbolId, SymbolKind},
    type_resolution::TypeResolution,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypedExpression {
    type_id: TypeId,
    category: ValueCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpressionOutcome {
    typed: TypedExpression,
    explicitly_produces_value: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockOutcome {
    typed: TypedExpression,
    explicit_value: Option<NodeId>,
}

/// Records that an expression of one member type must be materialized as an
/// explicitly expected union.
///
/// For example, `const value: int | float = 10;` injects the `int` expression
/// into `int | float`. Lowering uses this fact to construct the union with the
/// `int` tag and `10` as its payload. Each branch in
/// `if ready { 10 } else { 3.142 }` is injected separately when the conditional
/// is expected to have type `int | float`.
///
/// An expression that already has the expected union type needs no injection;
/// for example, passing an existing `int | float` binding to an `int | float`
/// parameter preserves the existing tag and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnionInjection {
    member_type: TypeId,
    union_type: TypeId,
}

/// Describes a structural conversion from a concrete struct to an erased
/// interface view. Lowering uses the matched method identities to select the
/// concrete vtable and the backing transfer to keep the viewed object alive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterfaceConversion {
    source_type: TypeId,
    destination_type: TypeId,
    methods: Vec<MethodId>,
    backing_transfer: ValueTransfer,
}

#[derive(Debug, Clone)]
struct RequiredInterfaceMethod {
    name: String,
    requirement: InterfaceRequirementSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Describes how the surrounding syntax consumes a conditional expression.
///
/// This is separate from the conditional's type because explicit `()` and
/// semicolon-ended completion both have unit type, but only the former
/// explicitly generates a branch value and therefore requires a complete
/// `else` chain.
enum ConditionalUse {
    /// The conditional must produce a value for a binding, argument, return,
    /// operand, or another value position.
    ///
    /// For example, `const value = if ready { 1 } else { 2 };` requires the
    /// final `else` because the initializer consumes the conditional's value.
    Value,
    /// The conditional is an expression statement whose result is ignored.
    ///
    /// For example, `if ready { notify(); }` may omit `else` because the call
    /// is semicolon-ended and the conditional does not explicitly produce a
    /// value. Writing `{ () }` instead would explicitly produce unit and would
    /// require `else` even though the result is discarded.
    Discarded,
    /// The conditional supplies the completion of another conditional branch.
    ///
    /// In `if outer { if inner { notify(); } } else { wait(); }`, the inner
    /// conditional may complete the outer branch implicitly without needing
    /// its own `else`; the outer chain decides whether branch values are
    /// required.
    BranchCompletion,
    /// The conditional is the syntactic final expression of a callable body.
    ///
    /// For example, `fn run() { if ready { notify(); } }` may complete a unit
    /// callable implicitly. A callable returning `int` cannot use the same
    /// missing-`else` form because its false path would produce no result.
    CallableCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindingSemantics {
    type_id: TypeId,
    qualifiers: BindingQualifiers,
    category: ValueCategory,
}

/// An assignable location denoted by an identifier, `self`, or a field access.
///
/// A plain object root is semantically a reference to frame-owned or borrowed
/// storage. Root binding mutability controls whether that reference can be
/// redirected. A field has no independently reassignable binding; mutation of
/// either kind of place is governed by its effective value capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    symbol: Option<SymbolId>,
    type_id: TypeId,
    category: ValueCategory,
    binding_mutability: Option<BindingMutability>,
    value_capability: ValueCapability,
}

/// The declaration selected by a named-struct member expression. Typed IR
/// consumes this identity directly instead of repeating source-name lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedMember {
    /// A source or construction field mapped to its declared field identity.
    Field { declaration: NodeId },
    /// A receiverless function selected through `Type::function`.
    AssociatedFunction { declaration: NodeId },
    /// A method invoked directly through a value. Methods are never emitted as
    /// first-class bound callable values.
    Method {
        declaration: NodeId,
        method_id: MethodId,
    },
    /// A structurally selected requirement invoked through interface dispatch.
    InterfaceMethod {
        declaration: NodeId,
        method_id: MethodId,
    },
    /// The compiler-provided recursive copy operation and its source type.
    Copy { source_type: TypeId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LambdaCaptureSource {
    Symbol(SymbolId),
    SelfValue { method: NodeId },
}

/// The type-facing portion of a lambda capture.
///
/// This deliberately records only the source and its two capabilities. The
/// later capture-analysis pass still decides environment layout, recursive
/// copies, shared mutable cells, tracing, and escape validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LambdaCapture {
    source: LambdaCaptureSource,
    qualifiers: BindingQualifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpressionCheckingError {
    kind: ExpressionCheckingErrorKind,
    span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionCheckingErrorKind {
    IntegerLiteralOutOfRange,
    TypeMismatch {
        expected: TypeId,
        found: TypeId,
    },
    InvalidUnaryOperand {
        operator: UnaryOperator,
        found: TypeId,
    },
    InvalidBinaryOperand {
        operator: BinaryOperator,
        found: TypeId,
    },
    InvalidGarbageCollectionSource {
        found: TypeId,
        category: ValueCategory,
    },
    InvalidReturnSource {
        found: TypeId,
        category: ValueCategory,
    },
    NotCallable {
        found: TypeId,
    },
    ArgumentCountMismatch {
        expected: usize,
        found: usize,
    },
    ConditionalElseRequired,
    ConditionalBranchValueRequired,
    InvalidAssignmentTarget,
    ImmutableBinding,
    ImmutableValue,
    InvalidAssignmentOperand {
        operator: AssignmentOperator,
        found: TypeId,
    },
    InvalidConstructionOwner,
    UnknownConstructionField,
    DuplicateConstructionField,
    MissingConstructionField {
        declaration: NodeId,
    },
    InvalidOwningSource {
        found: TypeId,
        category: ValueCategory,
    },
    UnknownMember,
    InvalidMemberOwner {
        found: TypeId,
    },
    FieldRequiresValue,
    AssociatedFunctionRequiresType,
    MethodRequiresValue,
    MethodRequiresCall,
    CopyRequiresCall,
    CopyRequiresValue,
    ReceiverStorageMismatch,
    ReceiverCapabilityMismatch,
    MissingInterfaceMethod {
        declaration: NodeId,
    },
    IncompatibleInterfaceMethod {
        requirement: NodeId,
        implementation: NodeId,
    },
    ConflictingInterfaceRequirement {
        first: NodeId,
        second: NodeId,
    },
    InterfaceRequiresGarbageCollectedSource,
    InfiniteInlineLayout {
        owner: TypeId,
    },
}

#[derive(Debug, Default)]
struct ExpressionChecking {
    expressions: HashMap<NodeId, TypedExpression>,
    explicit_values: HashMap<NodeId, bool>,
    bindings: HashMap<SymbolId, BindingSemantics>,
    transfers: HashMap<NodeId, ValueTransfer>,
    union_injections: HashMap<NodeId, UnionInjection>,
    interface_conversions: HashMap<NodeId, InterfaceConversion>,
    /// Final semantic types of anonymous fields, including types inferred from
    /// their initializers after signature collection.
    anonymous_field_types: HashMap<NodeId, TypeId>,
    lambda_captures: HashMap<NodeId, Vec<LambdaCapture>>,
    /// Assignable roots and fields, including the access capability that
    /// controls rebinding or mutation through each place.
    places: HashMap<NodeId, Place>,
    /// Final declaration identities selected by member lookup. Later typed IR
    /// can consume these without repeating lookup from source names.
    resolved_members: HashMap<NodeId, ResolvedMember>,
    /// Bindings written by assignment. For object-like locals, lowering uses
    /// this to decide when the source-level reference needs indirection in
    /// addition to any frame-owned backing storage.
    reassigned_bindings: HashSet<SymbolId>,
    errors: Vec<ExpressionCheckingError>,
}

#[derive(Debug, Default)]
struct LexicalIndex {
    callable_parents: HashMap<NodeId, Option<NodeId>>,
    symbol_owners: HashMap<SymbolId, NodeId>,
    receiver_qualifiers: HashMap<NodeId, BindingQualifiers>,
}

impl LexicalIndex {
    fn build(program: &Program, names: &NameResolution) -> Self {
        let mut index = Self::default();
        for declaration in &program.declarations {
            match declaration {
                Declaration::Function(function) => index.visit_function(function, None, names),
                Declaration::Struct(structure) => {
                    for member in &structure.members {
                        if let StructMember::Function(function) = member {
                            index.visit_function(function, None, names);
                        }
                    }
                }
                Declaration::Interface(_) => {}
            }
        }
        index
    }

    fn visit_function(
        &mut self,
        function: &Function,
        parent: Option<NodeId>,
        names: &NameResolution,
    ) {
        self.callable_parents.insert(function.id, parent);
        self.record_parameters(function.id, &function.parameters, names);
        if let Some(receiver) = function
            .parameters
            .iter()
            .find(|parameter| matches!(&parameter.kind, FunctionParameterKind::Receiver { .. }))
        {
            self.receiver_qualifiers
                .insert(function.id, receiver.qualifiers);
        }
        self.visit_block(&function.body, function.id, names);
    }

    fn record_parameters(
        &mut self,
        callable: NodeId,
        parameters: &[FunctionParameter],
        names: &NameResolution,
    ) {
        for parameter in parameters {
            if matches!(&parameter.kind, FunctionParameterKind::Named { .. }) {
                let symbol = names
                    .symbol_for_declaration(parameter.id)
                    .expect("named parameter must have a semantic symbol");
                self.symbol_owners.insert(symbol, callable);
            }
        }
    }

    fn visit_block(&mut self, block: &Block, callable: NodeId, names: &NameResolution) {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Binding { initializer, .. } => {
                    self.visit_expression(initializer, callable, names);
                    let symbol = names
                        .symbol_for_declaration(statement.id)
                        .expect("ordinary binding must have a semantic symbol");
                    self.symbol_owners.insert(symbol, callable);
                }
                StatementKind::Expression(expression)
                | StatementKind::Defer(expression)
                | StatementKind::Coroutine(expression) => {
                    self.visit_expression(expression, callable, names);
                }
                StatementKind::Function(function) => {
                    self.visit_function(function, Some(callable), names);
                }
                StatementKind::Break(value) | StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.visit_expression(value, callable, names);
                    }
                }
                StatementKind::Continue => {}
            }
        }
        if let Some(value) = &block.value {
            self.visit_expression(value, callable, names);
        }
    }

    fn visit_expression(
        &mut self,
        expression: &Expression,
        callable: NodeId,
        names: &NameResolution,
    ) {
        match &expression.kind {
            ExpressionKind::Identifier
            | ExpressionKind::SelfValue
            | ExpressionKind::Literal(_)
            | ExpressionKind::AssociatedAccess { .. } => {}
            ExpressionKind::Group(inner)
            | ExpressionKind::PrimitiveConversion { value: inner, .. }
            | ExpressionKind::GarbageCollect(inner)
            | ExpressionKind::MemberAccess { object: inner, .. }
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::TypeTest { value: inner, .. }
            | ExpressionKind::Unary { operand: inner, .. } => {
                self.visit_expression(inner, callable, names);
            }
            ExpressionKind::Block(block) | ExpressionKind::Loop { body: block } => {
                self.visit_block(block, callable, names);
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expression(condition, callable, names);
                self.visit_block(then_branch, callable, names);
                if let Some(else_branch) = else_branch {
                    match else_branch {
                        ConditionalElse::Block(block) => self.visit_block(block, callable, names),
                        ConditionalElse::If(expression) => {
                            self.visit_expression(expression, callable, names);
                        }
                    }
                }
            }
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.visit_expression(condition, callable, names);
                self.visit_block(body, callable, names);
                if let Some(block) = else_branch {
                    self.visit_block(block, callable, names);
                }
            }
            ExpressionKind::RangeFor {
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                self.visit_expression(start, callable, names);
                self.visit_expression(end, callable, names);
                let symbol = names
                    .symbol_for_declaration(expression.id)
                    .expect("range binding must have a semantic symbol");
                self.symbol_owners.insert(symbol, callable);
                self.visit_block(body, callable, names);
                if let Some(block) = else_branch {
                    self.visit_block(block, callable, names);
                }
            }
            ExpressionKind::Lambda {
                parameters, body, ..
            } => {
                self.callable_parents.insert(expression.id, Some(callable));
                self.record_parameters(expression.id, parameters, names);
                self.visit_block(body, expression.id, names);
            }
            ExpressionKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.visit_expression(&field.value, callable, names);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                for member in members {
                    match member {
                        AnonymousStructMember::Field(field) => {
                            self.visit_expression(&field.initializer, callable, names);
                        }
                        AnonymousStructMember::Method(method) => {
                            self.visit_function(method, Some(callable), names);
                        }
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.visit_expression(callee, callable, names);
                for argument in arguments {
                    self.visit_expression(argument, callable, names);
                }
            }
            ExpressionKind::Index { object, index }
            | ExpressionKind::Binary {
                left: object,
                right: index,
                ..
            }
            | ExpressionKind::Assignment {
                target: object,
                value: index,
                ..
            } => {
                self.visit_expression(object, callable, names);
                self.visit_expression(index, callable, names);
            }
            ExpressionKind::Slice { object, start, end } => {
                self.visit_expression(object, callable, names);
                if let Some(start) = start {
                    self.visit_expression(start, callable, names);
                }
                if let Some(end) = end {
                    self.visit_expression(end, callable, names);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LayoutField {
    declaration: NodeId,
    span: Span,
    type_id: TypeId,
}

#[derive(Debug, Clone)]
struct AggregateLayout {
    type_id: TypeId,
    fields: Vec<LayoutField>,
}

struct Analyzer<'semantic> {
    module: &'semantic SourceModule,
    names: &'semantic NameResolution,
    context: &'semantic ContextResolution,
    signatures: &'semantic SignatureCollection,
    types: &'semantic mut TypeResolution,
    method_owners: HashMap<NodeId, TypeId>,
    /// Connects the type symbol resolved on `Name { ... }` to the declaration
    /// whose collected field signature must be checked.
    named_struct_symbols: HashMap<SymbolId, NodeId>,
    callable_parents: HashMap<NodeId, Option<NodeId>>,
    symbol_owners: HashMap<SymbolId, NodeId>,
    receiver_qualifiers: HashMap<NodeId, BindingQualifiers>,
    /// Aggregate declarations and expressions in source discovery order. The
    /// order is retained so recursive-layout diagnostics are deterministic.
    aggregate_order: Vec<NodeId>,
    aggregate_layouts: HashMap<NodeId, AggregateLayout>,
    /// Flow-sensitive provenance of the storage currently denoted by each
    /// binding. Declared type and qualifiers remain in `checking.bindings`.
    current_binding_categories: HashMap<SymbolId, ValueCategory>,
    checking: ExpressionChecking,
}

#[cfg(test)]
pub(super) fn assert_program_checks(
    module: &SourceModule,
    program: &Program,
    names: &NameResolution,
    context: &ContextResolution,
    signatures: &SignatureCollection,
    types: &mut TypeResolution,
) {
    let checking =
        Analyzer::new(module, names, context, signatures, types, program).check_program(program);
    assert!(
        checking.errors.is_empty(),
        "the complex program should pass implemented expression checking: {:#?}",
        checking.errors
    );
}

impl<'semantic> Analyzer<'semantic> {
    fn new(
        module: &'semantic SourceModule,
        names: &'semantic NameResolution,
        context: &'semantic ContextResolution,
        signatures: &'semantic SignatureCollection,
        types: &'semantic mut TypeResolution,
        program: &Program,
    ) -> Self {
        let LexicalIndex {
            callable_parents,
            symbol_owners,
            receiver_qualifiers,
        } = LexicalIndex::build(program, names);
        let mut method_owners = HashMap::new();
        let mut named_struct_symbols = HashMap::new();
        let mut aggregate_order = Vec::new();
        let mut aggregate_layouts = HashMap::new();
        for declaration in &program.declarations {
            let Declaration::Struct(structure) = declaration else {
                continue;
            };
            let symbol = names
                .symbol_for_declaration(structure.id)
                .expect("named struct must have a semantic symbol");
            named_struct_symbols.insert(symbol, structure.id);
            let signature = signatures
                .named_struct(structure.id)
                .expect("named struct signature must have been collected");
            let owner = signature.type_id;
            let fields = structure
                .members
                .iter()
                .filter_map(|member| {
                    let StructMember::Field(field) = member else {
                        return None;
                    };
                    let StructMemberSignatureKind::Field(field_signature) = signature
                        .member(
                            module
                                .text(field.name)
                                .expect("field span belongs to the source module"),
                        )
                        .expect("named field must have a collected signature")
                        .kind
                    else {
                        unreachable!("named field must select a field signature")
                    };
                    Some(LayoutField {
                        declaration: field.id,
                        span: field.span,
                        type_id: field_signature
                            .type_id
                            .expect("named fields always have declared types"),
                    })
                })
                .collect();
            aggregate_order.push(structure.id);
            aggregate_layouts.insert(
                structure.id,
                AggregateLayout {
                    type_id: owner,
                    fields,
                },
            );
            for member in &structure.members {
                if let StructMember::Function(function) = member
                    && signatures
                        .callable(function.id)
                        .is_some_and(|signature| signature.receiver.is_some())
                {
                    method_owners.insert(function.id, owner);
                }
            }
        }

        Self {
            module,
            names,
            context,
            signatures,
            types,
            method_owners,
            named_struct_symbols,
            callable_parents,
            symbol_owners,
            receiver_qualifiers,
            aggregate_order,
            aggregate_layouts,
            current_binding_categories: HashMap::new(),
            checking: ExpressionChecking::default(),
        }
    }

    fn check_program(mut self, program: &Program) -> ExpressionChecking {
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }
        self.validate_finite_inline_layouts();
        self.checking
    }

    fn visit_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function) => self.visit_function(function),
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    if let StructMember::Function(function) = member {
                        self.visit_function(function);
                    }
                }
            }
            Declaration::Interface(_) => {}
        }
    }

    fn visit_function(&mut self, function: &Function) {
        let enclosing_categories = self.current_binding_categories.clone();
        self.seed_callable_parameters(function.id, &function.parameters);
        let return_type = self
            .signatures
            .callable(function.id)
            .expect("function signature must have been collected")
            .return_type;
        self.visit_callable_body(&function.body, return_type);
        self.current_binding_categories = enclosing_categories;
    }

    /// Makes a named function's or lambda's parameters available while checking
    /// its body.
    ///
    /// Each parameter's collected semantic type, source qualifiers, and value
    /// category are recorded against its resolved symbol. Receivers are not
    /// included because `self` is typed separately from receiver metadata.
    fn seed_callable_parameters(&mut self, callable: NodeId, parameters: &[FunctionParameter]) {
        let signature = self
            .signatures
            .callable(callable)
            .expect("callable signature must have been collected");
        let mut semantic_parameters = signature.parameters.clone().into_iter();

        for parameter in parameters {
            let FunctionParameterKind::Named { .. } = &parameter.kind else {
                continue;
            };
            let type_id = semantic_parameters
                .next()
                .expect("collected signature must contain every named parameter");
            let symbol = self
                .names
                .symbol_for_declaration(parameter.id)
                .expect("named parameter must have a semantic symbol");
            let category = self.parameter_category(type_id);
            self.checking.bindings.insert(
                symbol,
                BindingSemantics {
                    type_id,
                    qualifiers: parameter.qualifiers,
                    category,
                },
            );
            self.current_binding_categories.insert(symbol, category);
        }
        assert!(
            semantic_parameters.next().is_none(),
            "collected signature has more semantic parameters than the AST"
        );
    }

    /// Checks a named callable body and whether its sequential execution can
    /// reach the body's implicit result.
    ///
    /// Every statement is analyzed even after control flow has diverged. A
    /// reachable final expression supplies the callable result, while reachable
    /// completion without one supplies unit.
    fn visit_callable_body(&mut self, block: &crate::ast::Block, expected: TypeId) {
        // A return or diverging statement prevents execution from reaching the
        // body's final value or implicit unit result.
        let can_reach_body_end = self.visit_block_statements(block);
        match (&block.value, can_reach_body_end) {
            (Some(value), true) if matches!(&value.kind, ExpressionKind::If { .. }) => {
                let outcome = self.analyze_conditional_expression(
                    value,
                    Some(expected),
                    ConditionalUse::CallableCompletion,
                    true,
                );
                if let Some(outcome) = outcome
                    && outcome.explicitly_produces_value
                {
                    self.record_return_transfer(value, outcome.typed);
                }
            }
            (Some(value), true) => self.analyze_return_value(value, expected),
            (Some(value), false) => {
                let _ = self.synthesize_discarded(value);
            }
            (None, true) => self.check_absent_value(expected, block.span),
            (None, false) => {}
        }
    }

    fn visit_statement(&mut self, statement: &Statement) -> bool {
        match &statement.kind {
            StatementKind::Binding {
                qualifiers,
                type_annotation,
                initializer,
                ..
            } => {
                let source = self.analyze_binding(
                    statement,
                    *qualifiers,
                    type_annotation.as_ref().map(|syntax| syntax.id),
                    initializer,
                );
                source.map_or(true, |typed| !self.is_divergence(typed.type_id))
            }
            StatementKind::Expression(expression) => self
                .synthesize_discarded(expression)
                .map_or(true, |typed| !self.is_divergence(typed.type_id)),
            StatementKind::Function(function) => {
                self.visit_function(function);
                true
            }
            StatementKind::Return(value) => {
                let callable = self
                    .context
                    .callable_for_return(statement.id)
                    .expect("return statement must have a resolved callable target");
                let expected = self
                    .signatures
                    .callable(callable)
                    .expect("return target must have a collected signature")
                    .return_type;
                if let Some(value) = value {
                    self.analyze_return_value(value, expected);
                } else {
                    self.check_absent_value(expected, statement.span);
                }
                false
            }
            StatementKind::Defer(_)
            | StatementKind::Coroutine(_)
            | StatementKind::Break(_)
            | StatementKind::Continue => true,
        }
    }

    /// Analyzes every statement in source order and reports whether sequential
    /// execution can reach the block's final expression or closing brace.
    fn visit_block_statements(&mut self, block: &Block) -> bool {
        let mut can_reach_block_end = true;
        for statement in &block.statements {
            let statement_can_complete = self.visit_statement(statement);
            can_reach_block_end &= statement_can_complete;
        }
        can_reach_block_end
    }

    /// Types an ordinary binding initializer and records the resulting binding.
    ///
    /// An annotated binding checks its initializer against the declared type,
    /// while an unannotated binding synthesizes its type from the initializer.
    /// Both shapes then select the value transfer and record the binding's
    /// semantic type, qualifiers, and value category against its symbol.
    fn analyze_binding(
        &mut self,
        statement: &Statement,
        qualifiers: BindingQualifiers,
        annotation: Option<NodeId>,
        initializer: &Expression,
    ) -> Option<TypedExpression> {
        let expected = annotation.map(|id| {
            let resolved = self
                .types
                .type_for_syntax(id)
                .expect("binding annotation must have a resolved type");
            self.with_value_capability(resolved, qualifiers.value)
        });
        let source = match expected {
            Some(expected) => self.check(initializer, expected),
            None => self.synthesize(initializer),
        };
        let Some(mut source) = source else {
            return None;
        };

        let mut stored_type = if self.is_recovery(source.type_id) {
            source.type_id
        } else {
            expected.unwrap_or_else(|| self.with_value_capability(source.type_id, qualifiers.value))
        };
        if expected.is_none()
            && !self.is_recovery(source.type_id)
            && !self.value_capability_is_compatible(source, stored_type, false)
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TypeMismatch {
                    expected: stored_type,
                    found: source.type_id,
                },
                span: initializer.span,
            });
            source = TypedExpression {
                type_id: self.types.types().recovery(),
                category: source.category,
            };
            stored_type = source.type_id;
            self.checking.expressions.insert(initializer.id, source);
        }
        let (category, transfer) = self.binding_transfer(source);
        if let Some(transfer) = transfer {
            self.checking.transfers.insert(initializer.id, transfer);
        }
        let symbol = self
            .names
            .symbol_for_declaration(statement.id)
            .expect("ordinary binding must have a semantic symbol");
        self.checking.bindings.insert(
            symbol,
            BindingSemantics {
                type_id: stored_type,
                qualifiers,
                category,
            },
        );
        self.current_binding_categories.insert(symbol, category);
        Some(source)
    }

    /// Checks a returned expression and records how its value enters the
    /// caller-owned result location.
    fn analyze_return_value(&mut self, value: &Expression, expected: TypeId) {
        let Some(source) = self.check_with_capability(value, expected, true) else {
            return;
        };
        self.record_return_transfer(value, source);
    }

    fn record_return_transfer(&mut self, value: &Expression, source: TypedExpression) {
        if self.is_recovery(source.type_id) || self.is_divergence(source.type_id) {
            return;
        }

        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("return type belongs to the program type store");
        let transfer = if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            Some(ValueTransfer::ReuseGarbageCollected)
        } else {
            match semantic.copy_semantics() {
                Some(CopySemantics::Trivial) => Some(ValueTransfer::TrivialCopy),
                Some(CopySemantics::Recursive) => {
                    Some(if source.category == ValueCategory::FreshTemporary {
                        ValueTransfer::MoveTemporary
                    } else {
                        ValueTransfer::RecursiveCopy
                    })
                }
                Some(CopySemantics::NonEscapingErasedView)
                    if matches!(semantic, SemanticType::Callable { .. })
                        && source.category != ValueCategory::BorrowedPlace =>
                {
                    Some(ValueTransfer::MoveTemporary)
                }
                Some(CopySemantics::NonEscapingErasedView) | None => None,
                Some(CopySemantics::GarbageCollectedPayload) => {
                    unreachable!("GC return storage was handled above")
                }
            }
        };
        if let Some(transfer) = transfer {
            self.checking.transfers.insert(value.id, transfer);
            return;
        }

        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::InvalidReturnSource {
                found: source.type_id,
                category: source.category,
            },
            span: value.span,
        });
        self.checking.expressions.insert(
            value.id,
            TypedExpression {
                type_id: self.types.types().recovery(),
                category: source.category,
            },
        );
    }

    /// Checks an implicit unit result, such as a bare return or a callable body
    /// that reaches its closing brace without a final expression.
    fn check_absent_value(&mut self, expected: TypeId, span: Span) {
        if self.is_recovery(expected) {
            return;
        }
        let unit = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Unit, AccessCapability::Const);
        if self
            .types
            .types()
            .has_same_shape(unit, expected)
            .expect("return types belong to the program type store")
        {
            return;
        }
        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: unit,
            },
            span,
        });
    }

    /// Analyzes one executable block while preserving the distinction between
    /// an explicit final value and implicit unit completion.
    fn analyze_block(
        &mut self,
        block: &Block,
        expected: Option<TypeId>,
        tail_use: ConditionalUse,
        allow_recursive_copy: bool,
    ) -> Option<BlockOutcome> {
        let can_reach_block_end = self.visit_block_statements(block);

        if !can_reach_block_end {
            if let Some(value) = &block.value {
                let _ = self.synthesize_discarded(value);
            }
            return Some(BlockOutcome {
                typed: TypedExpression {
                    type_id: self.types.types().divergence(),
                    category: ValueCategory::FreshTemporary,
                },
                explicit_value: None,
            });
        }

        let Some(value) = &block.value else {
            return Some(BlockOutcome {
                typed: self.fresh_primitive(PrimitiveType::Unit),
                explicit_value: None,
            });
        };
        let outcome = if let ExpressionKind::If { .. } = &value.kind {
            match expected {
                Some(expected) => self.analyze_conditional_expression(
                    value,
                    Some(expected),
                    tail_use,
                    allow_recursive_copy,
                )?,
                None => self.synthesize_conditional_expression(value, tail_use)?,
            }
        } else {
            let typed = match expected {
                Some(expected) => {
                    self.check_with_capability(value, expected, allow_recursive_copy)?
                }
                None => self.synthesize(value)?,
            };
            ExpressionOutcome {
                typed,
                explicitly_produces_value: self
                    .checking
                    .explicit_values
                    .get(&value.id)
                    .copied()
                    .unwrap_or(true),
            }
        };
        Some(BlockOutcome {
            typed: outcome.typed,
            explicit_value: outcome.explicitly_produces_value.then_some(value.id),
        })
    }

    /// Analyzes an expression whose result is discarded by its containing
    /// statement, allowing a non-value-producing conditional to omit `else`.
    fn synthesize_discarded(&mut self, expression: &Expression) -> Option<TypedExpression> {
        let outcome = match &expression.kind {
            ExpressionKind::Group(inner) => {
                let typed = self.synthesize_discarded(inner)?;
                let explicitly_produces_value = self
                    .checking
                    .explicit_values
                    .get(&inner.id)
                    .copied()
                    .unwrap_or(true);
                ExpressionOutcome {
                    typed,
                    explicitly_produces_value,
                }
            }
            ExpressionKind::Block(block) => {
                let block = self.analyze_block(block, None, ConditionalUse::Discarded, false)?;
                ExpressionOutcome {
                    typed: block.typed,
                    explicitly_produces_value: block.explicit_value.is_some(),
                }
            }
            ExpressionKind::If { .. } => {
                self.synthesize_conditional_expression(expression, ConditionalUse::Discarded)?
            }
            _ => ExpressionOutcome {
                typed: self.synthesize(expression)?,
                explicitly_produces_value: true,
            },
        };
        self.checking
            .expressions
            .insert(expression.id, outcome.typed);
        self.checking
            .explicit_values
            .insert(expression.id, outcome.explicitly_produces_value);
        Some(outcome.typed)
    }

    fn synthesize_conditional_expression(
        &mut self,
        expression: &Expression,
        usage: ConditionalUse,
    ) -> Option<ExpressionOutcome> {
        self.analyze_conditional_expression(expression, None, usage, false)
    }

    fn check_conditional_expression(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        usage: ConditionalUse,
    ) -> Option<ExpressionOutcome> {
        self.analyze_conditional_expression(expression, Some(expected), usage, false)
    }

    /// Checks a complete `if`/`else if`/`else` chain as one expression.
    fn analyze_conditional_expression(
        &mut self,
        expression: &Expression,
        expected: Option<TypeId>,
        usage: ConditionalUse,
        allow_recursive_copy: bool,
    ) -> Option<ExpressionOutcome> {
        let first_error = self.checking.errors.len();
        let mut arms = Vec::new();
        let final_else = collect_conditional_arms(expression, &mut arms);
        let bool_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Bool, AccessCapability::Const);
        let unit_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Unit, AccessCapability::Const);
        let mut condition_invalid = false;
        let incoming_categories = self.current_binding_categories.clone();
        let mut fallthrough_categories = incoming_categories.clone();
        let mut branches =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let mut branch_categories =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let conditional_nodes: Vec<_> = arms
            .iter()
            .map(|(conditional, _, _)| *conditional)
            .collect();
        for (_, condition, branch) in arms {
            self.current_binding_categories = fallthrough_categories;
            let checked_condition = self.check(condition, bool_type)?;
            condition_invalid |= self.is_recovery(checked_condition.type_id);
            fallthrough_categories = self.current_binding_categories.clone();
            self.current_binding_categories = fallthrough_categories.clone();
            branches.push((
                branch,
                self.analyze_block(
                    branch,
                    expected,
                    ConditionalUse::BranchCompletion,
                    allow_recursive_copy,
                )?,
            ));
            branch_categories.push(self.current_binding_categories.clone());
        }
        if let Some(branch) = final_else {
            self.current_binding_categories = fallthrough_categories.clone();
            branches.push((
                branch,
                self.analyze_block(
                    branch,
                    expected,
                    ConditionalUse::BranchCompletion,
                    allow_recursive_copy,
                )?,
            ));
            branch_categories.push(self.current_binding_categories.clone());
        }

        let has_else = final_else.is_some();
        let mut completing_categories: Vec<_> = branches
            .iter()
            .zip(&branch_categories)
            .filter(|((_, outcome), _)| !self.is_divergence(outcome.typed.type_id))
            .map(|(_, categories)| categories)
            .collect();
        if !has_else {
            completing_categories.push(&fallthrough_categories);
        }
        self.current_binding_categories =
            self.merge_binding_categories(&incoming_categories, &completing_categories);
        let branch_invalid = branches
            .iter()
            .any(|(_, branch)| self.is_recovery(branch.typed.type_id));
        let any_explicit = branches.iter().any(|(_, branch)| {
            !self.is_divergence(branch.typed.type_id) && branch.explicit_value.is_some()
        });
        let mut invalid = condition_invalid || branch_invalid;
        let missing_else_allowed = match usage {
            ConditionalUse::Discarded | ConditionalUse::BranchCompletion => true,
            ConditionalUse::CallableCompletion => expected.is_some_and(|expected| {
                self.is_recovery(expected)
                    || self
                        .types
                        .types()
                        .has_same_shape(expected, unit_type)
                        .expect("callable result types belong to the program type store")
            }),
            ConditionalUse::Value => false,
        };
        if (any_explicit || !missing_else_allowed) && !has_else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ConditionalElseRequired,
                span: expression.span,
            });
            invalid = true;
        }
        if any_explicit {
            for (block, branch) in &branches {
                if !self.is_divergence(branch.typed.type_id) && branch.explicit_value.is_none() {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::ConditionalBranchValueRequired,
                        span: block.span,
                    });
                    invalid = true;
                }
            }
        }

        let normally_completing: Vec<_> = branches
            .iter()
            .filter(|(_, branch)| !self.is_divergence(branch.typed.type_id))
            .copied()
            .collect();
        if has_else && normally_completing.is_empty() {
            let outcome = ExpressionOutcome {
                typed: TypedExpression {
                    type_id: if invalid {
                        self.types.types().recovery()
                    } else {
                        self.types.types().divergence()
                    },
                    category: ValueCategory::FreshTemporary,
                },
                explicitly_produces_value: false,
            };
            for conditional in conditional_nodes {
                self.record_expression_outcome(conditional, outcome);
            }
            self.checking.errors[first_error..]
                .sort_by_key(|error| (error.span.start, error.span.end));
            return Some(outcome);
        }

        let mut typed = if any_explicit {
            let mut values = normally_completing.iter().filter_map(|(block, branch)| {
                branch.explicit_value.map(|id| (*block, id, branch.typed))
            });
            let (_, _, first) = values
                .next()
                .expect("an explicit conditional has a normally completing value path");
            let result_type = expected.unwrap_or(first.type_id);
            if expected.is_none() && !self.is_recovery(first.type_id) {
                for (block, _, value) in values {
                    if self.is_recovery(value.type_id) {
                        invalid = true;
                        continue;
                    }
                    let matches = self
                        .types
                        .types()
                        .has_same_shape(value.type_id, result_type)
                        .expect("conditional result types belong to the program type store");
                    if !matches {
                        self.checking.errors.push(ExpressionCheckingError {
                            kind: ExpressionCheckingErrorKind::TypeMismatch {
                                expected: result_type,
                                found: value.type_id,
                            },
                            span: block
                                .value
                                .as_deref()
                                .map_or(block.span, |value| value.span),
                        });
                        invalid = true;
                    }
                }
            }
            if invalid {
                self.recovery_temporary()
            } else {
                self.merge_conditional_values(result_type, &normally_completing)
            }
        } else {
            self.fresh_primitive(PrimitiveType::Unit)
        };

        if !has_else {
            typed = self.fresh_primitive(PrimitiveType::Unit);
        }
        if let Some(expected) = expected
            && !any_explicit
            && !invalid
            && usage != ConditionalUse::BranchCompletion
        {
            typed = self.check_typed(expression, expected, typed, allow_recursive_copy)?;
            invalid |= self.is_recovery(typed.type_id);
        }
        if invalid {
            typed = self.recovery_temporary();
        }
        let outcome = ExpressionOutcome {
            typed,
            explicitly_produces_value: any_explicit,
        };
        self.record_conditional_suffix_outcomes(
            &conditional_nodes,
            &branches,
            outcome,
            invalid,
            has_else,
        );
        self.checking.errors[first_error..].sort_by_key(|error| (error.span.start, error.span.end));
        Some(outcome)
    }

    /// Selects one category for the conditional result and records how each
    /// explicit branch value reaches that merged result.
    fn merge_conditional_values(
        &mut self,
        result_type: TypeId,
        branches: &[(&Block, BlockOutcome)],
    ) -> TypedExpression {
        let values: Vec<_> = branches
            .iter()
            .filter_map(|(_, branch)| branch.explicit_value.map(|id| (id, branch.typed)))
            .collect();
        if values.len() == 1 {
            return values[0].1;
        }
        let semantic = self
            .types
            .types()
            .get(result_type)
            .expect("conditional result type belongs to the program type store");
        let (category, transfers): (ValueCategory, Vec<_>) =
            if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
                (
                    ValueCategory::GarbageCollectedReference,
                    values
                        .iter()
                        .map(|(id, _)| (*id, ValueTransfer::ReuseGarbageCollected))
                        .collect(),
                )
            } else if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
                (
                    ValueCategory::FreshTemporary,
                    values
                        .iter()
                        .map(|(id, _)| (*id, ValueTransfer::TrivialCopy))
                        .collect(),
                )
            } else {
                let all_fresh = values
                    .iter()
                    .all(|(_, value)| value.category == ValueCategory::FreshTemporary);
                let category = if all_fresh {
                    ValueCategory::FreshTemporary
                } else {
                    ValueCategory::BorrowedPlace
                };
                let transfers = values
                    .iter()
                    .map(|(id, value)| {
                        let transfer = if value.category == ValueCategory::FreshTemporary {
                            ValueTransfer::MoveTemporary
                        } else {
                            ValueTransfer::Borrow
                        };
                        (*id, transfer)
                    })
                    .collect();
                (category, transfers)
            };
        for (id, transfer) in transfers {
            self.checking.transfers.insert(id, transfer);
        }
        TypedExpression {
            type_id: result_type,
            category,
        }
    }

    /// Merges the provenance of bindings that existed before a conditional.
    /// Branch-local declarations do not escape their block, and a possible
    /// borrow wins over frame-owned provenance on mixed paths.
    fn merge_binding_categories(
        &self,
        incoming: &HashMap<SymbolId, ValueCategory>,
        completing: &[&HashMap<SymbolId, ValueCategory>],
    ) -> HashMap<SymbolId, ValueCategory> {
        if completing.is_empty() {
            return incoming.clone();
        }
        incoming
            .iter()
            .map(|(symbol, incoming_category)| {
                let mut merged = *incoming_category;
                for categories in completing {
                    let category = categories
                        .get(symbol)
                        .copied()
                        .unwrap_or(*incoming_category);
                    if merged != category {
                        merged = match (merged, category) {
                            (ValueCategory::GarbageCollectedReference, _)
                            | (_, ValueCategory::GarbageCollectedReference) => {
                                ValueCategory::GarbageCollectedReference
                            }
                            (ValueCategory::OwnedInlinePlace, ValueCategory::OwnedInlinePlace) => {
                                ValueCategory::OwnedInlinePlace
                            }
                            _ => ValueCategory::BorrowedPlace,
                        };
                    }
                }
                (*symbol, merged)
            })
            .collect()
    }

    /// Records each `else if` node from the result paths belonging to that
    /// suffix rather than copying the outer conditional's category blindly.
    fn record_conditional_suffix_outcomes(
        &mut self,
        conditionals: &[&Expression],
        branches: &[(&Block, BlockOutcome)],
        outer: ExpressionOutcome,
        invalid: bool,
        has_else: bool,
    ) {
        for (index, conditional) in conditionals.iter().enumerate() {
            if invalid {
                self.record_expression_outcome(conditional, outer);
                continue;
            }
            let normally_completing: Vec<_> = branches[index..]
                .iter()
                .filter(|(_, branch)| !self.is_divergence(branch.typed.type_id))
                .copied()
                .collect();
            let outcome = if normally_completing.is_empty() && has_else {
                ExpressionOutcome {
                    typed: TypedExpression {
                        type_id: self.types.types().divergence(),
                        category: ValueCategory::FreshTemporary,
                    },
                    explicitly_produces_value: false,
                }
            } else if normally_completing
                .iter()
                .any(|(_, branch)| branch.explicit_value.is_some())
            {
                ExpressionOutcome {
                    typed: self.merge_conditional_values(outer.typed.type_id, &normally_completing),
                    explicitly_produces_value: true,
                }
            } else {
                ExpressionOutcome {
                    typed: self.fresh_primitive(PrimitiveType::Unit),
                    explicitly_produces_value: false,
                }
            };
            self.record_expression_outcome(conditional, outcome);
        }
    }

    fn record_expression_outcome(&mut self, expression: &Expression, outcome: ExpressionOutcome) {
        self.checking
            .expressions
            .insert(expression.id, outcome.typed);
        self.checking
            .explicit_values
            .insert(expression.id, outcome.explicitly_produces_value);
    }

    fn synthesize(&mut self, expression: &Expression) -> Option<TypedExpression> {
        if let Some(typed) = self.checking.expressions.get(&expression.id).copied() {
            return Some(typed);
        }

        let typed = match &expression.kind {
            ExpressionKind::Literal(literal) => self.synthesize_literal(expression, *literal),
            ExpressionKind::Identifier => self.synthesize_identifier(expression)?,
            ExpressionKind::SelfValue => self.synthesize_self(expression),
            ExpressionKind::Group(inner) => {
                let typed = self.synthesize(inner)?;
                let explicitly_produces_value = self
                    .checking
                    .explicit_values
                    .get(&inner.id)
                    .copied()
                    .unwrap_or(true);
                self.checking
                    .explicit_values
                    .insert(expression.id, explicitly_produces_value);
                typed
            }
            ExpressionKind::Block(block) => {
                let outcome = self.analyze_block(block, None, ConditionalUse::Value, false)?;
                self.checking
                    .explicit_values
                    .insert(expression.id, outcome.explicit_value.is_some());
                outcome.typed
            }
            ExpressionKind::If { .. } => {
                self.synthesize_conditional_expression(expression, ConditionalUse::Value)?
                    .typed
            }
            ExpressionKind::Lambda {
                parameters, body, ..
            } => self.synthesize_lambda(expression, parameters, body)?,
            ExpressionKind::PrimitiveConversion { target, value } => {
                self.synthesize_primitive_conversion(*target, value)?
            }
            ExpressionKind::GarbageCollect(value) => self.synthesize_garbage_collection(value)?,
            ExpressionKind::StructConstruction { fields, .. } => {
                self.synthesize_named_struct_construction(expression, fields)?
            }
            ExpressionKind::AnonymousStruct { members } => {
                self.synthesize_anonymous_struct(expression, members)?
            }
            ExpressionKind::MemberAccess { object, member } => {
                self.synthesize_member_access(expression, object, *member)?
            }
            ExpressionKind::AssociatedAccess { owner, member } => {
                self.synthesize_named_associated_access(expression, owner, *member)?
            }
            ExpressionKind::Unary { operator, operand } => {
                self.synthesize_unary(*operator, operand)?
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => self.synthesize_binary(left, *operator, right)?,
            ExpressionKind::Call { callee, arguments } => {
                self.synthesize_call(expression, callee, arguments)?
            }
            ExpressionKind::Assignment {
                target,
                operator,
                value,
            } if matches!(
                &target.kind,
                ExpressionKind::Identifier | ExpressionKind::MemberAccess { .. }
            ) =>
            {
                self.synthesize_place_assignment(target, *operator, value)?
            }
            _ => return None,
        };
        self.checking.expressions.insert(expression.id, typed);
        self.checking
            .explicit_values
            .entry(expression.id)
            .or_insert(true);
        Some(typed)
    }

    fn synthesize_lambda(
        &mut self,
        expression: &Expression,
        parameters: &[FunctionParameter],
        body: &Block,
    ) -> Option<TypedExpression> {
        let signature = self
            .signatures
            .callable(expression.id)
            .expect("lambda signature must have been collected")
            .clone();
        let captures = self.lambda_captures(expression.id, body);
        let capability = if captures.iter().any(|capture| {
            capture.qualifiers.binding == BindingMutability::Mut
                || capture.qualifiers.value == ValueCapability::Mut
        }) {
            AccessCapability::Mut
        } else {
            AccessCapability::Const
        };
        self.checking
            .lambda_captures
            .insert(expression.id, captures);

        let first_body_error = self.checking.errors.len();
        let enclosing_categories = self.current_binding_categories.clone();
        self.seed_callable_parameters(expression.id, parameters);
        self.visit_callable_body(body, signature.return_type);
        self.current_binding_categories = enclosing_categories;
        if self.checking.errors.len() != first_body_error {
            return Some(self.recovery_temporary());
        }

        Some(TypedExpression {
            type_id: self.types.types_mut().callable(
                signature.parameters,
                signature.return_type,
                capability,
            ),
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Discovers only the free sources needed to infer a lambda's callable
    /// capability. The post-type capture pass remains responsible for deciding
    /// how these sources are represented and whether they may escape.
    fn lambda_captures(&self, lambda: NodeId, body: &Block) -> Vec<LambdaCapture> {
        let mut sources = Vec::new();
        let mut seen = HashSet::new();
        self.collect_captures_from_block(lambda, body, &mut sources, &mut seen);
        sources
            .into_iter()
            .map(|source| {
                let qualifiers = match source {
                    LambdaCaptureSource::Symbol(symbol) => {
                        self.checking
                            .bindings
                            .get(&symbol)
                            .expect("captured binding must be available before the lambda")
                            .qualifiers
                    }
                    LambdaCaptureSource::SelfValue { method } => *self
                        .receiver_qualifiers
                        .get(&method)
                        .expect("captured self must have receiver qualifiers"),
                };
                LambdaCapture { source, qualifiers }
            })
            .collect()
    }

    fn collect_captures_from_block(
        &self,
        lambda: NodeId,
        block: &Block,
        captures: &mut Vec<LambdaCaptureSource>,
        seen: &mut HashSet<LambdaCaptureSource>,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Binding { initializer, .. }
                | StatementKind::Expression(initializer)
                | StatementKind::Defer(initializer)
                | StatementKind::Coroutine(initializer) => {
                    self.collect_captures_from_expression(lambda, initializer, captures, seen);
                }
                // Named functions never capture, and illegal references from
                // their bodies must not make an enclosing lambda capturing.
                StatementKind::Function(_) | StatementKind::Continue => {}
                StatementKind::Break(value) | StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.collect_captures_from_expression(lambda, value, captures, seen);
                    }
                }
            }
        }
        if let Some(value) = &block.value {
            self.collect_captures_from_expression(lambda, value, captures, seen);
        }
    }

    fn collect_captures_from_expression(
        &self,
        lambda: NodeId,
        expression: &Expression,
        captures: &mut Vec<LambdaCaptureSource>,
        seen: &mut HashSet<LambdaCaptureSource>,
    ) {
        match &expression.kind {
            ExpressionKind::Identifier => {
                let symbol = self
                    .names
                    .symbol_for_reference(expression.id)
                    .expect("identifier must have a resolved semantic symbol");
                let Some(symbol_data) = self.names.symbols().symbol(symbol) else {
                    return;
                };
                if !matches!(
                    symbol_data.kind,
                    SymbolKind::Binding | SymbolKind::Parameter | SymbolKind::RangeBinding
                ) {
                    return;
                }
                let Some(owner) = self.symbol_owners.get(&symbol).copied() else {
                    return;
                };
                if !self.callable_is_within(owner, lambda) {
                    push_unique_capture(LambdaCaptureSource::Symbol(symbol), captures, seen);
                }
            }
            ExpressionKind::SelfValue => {
                let method = self
                    .context
                    .method_for_self(expression.id)
                    .expect("self expression must have a resolved method target");
                if !self.callable_is_within(method, lambda) {
                    push_unique_capture(LambdaCaptureSource::SelfValue { method }, captures, seen);
                }
            }
            ExpressionKind::Literal(_) | ExpressionKind::AssociatedAccess { .. } => {}
            ExpressionKind::Group(inner)
            | ExpressionKind::PrimitiveConversion { value: inner, .. }
            | ExpressionKind::GarbageCollect(inner)
            | ExpressionKind::MemberAccess { object: inner, .. }
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::TypeTest { value: inner, .. }
            | ExpressionKind::Unary { operand: inner, .. } => {
                self.collect_captures_from_expression(lambda, inner, captures, seen);
            }
            ExpressionKind::Block(block) | ExpressionKind::Loop { body: block } => {
                self.collect_captures_from_block(lambda, block, captures, seen);
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_captures_from_expression(lambda, condition, captures, seen);
                self.collect_captures_from_block(lambda, then_branch, captures, seen);
                if let Some(else_branch) = else_branch {
                    match else_branch {
                        ConditionalElse::Block(block) => {
                            self.collect_captures_from_block(lambda, block, captures, seen);
                        }
                        ConditionalElse::If(expression) => {
                            self.collect_captures_from_expression(
                                lambda, expression, captures, seen,
                            );
                        }
                    }
                }
            }
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.collect_captures_from_expression(lambda, condition, captures, seen);
                self.collect_captures_from_block(lambda, body, captures, seen);
                if let Some(block) = else_branch {
                    self.collect_captures_from_block(lambda, block, captures, seen);
                }
            }
            ExpressionKind::RangeFor {
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                self.collect_captures_from_expression(lambda, start, captures, seen);
                self.collect_captures_from_expression(lambda, end, captures, seen);
                self.collect_captures_from_block(lambda, body, captures, seen);
                if let Some(block) = else_branch {
                    self.collect_captures_from_block(lambda, block, captures, seen);
                }
            }
            ExpressionKind::Lambda { body, .. } => {
                self.collect_captures_from_block(lambda, body, captures, seen);
            }
            ExpressionKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.collect_captures_from_expression(lambda, &field.value, captures, seen);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                for member in members {
                    match member {
                        AnonymousStructMember::Field(field) => {
                            self.collect_captures_from_expression(
                                lambda,
                                &field.initializer,
                                captures,
                                seen,
                            );
                        }
                        AnonymousStructMember::Method(method) => {
                            self.collect_captures_from_block(lambda, &method.body, captures, seen);
                        }
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.collect_captures_from_expression(lambda, callee, captures, seen);
                for argument in arguments {
                    self.collect_captures_from_expression(lambda, argument, captures, seen);
                }
            }
            ExpressionKind::Index { object, index }
            | ExpressionKind::Binary {
                left: object,
                right: index,
                ..
            }
            | ExpressionKind::Assignment {
                target: object,
                value: index,
                ..
            } => {
                self.collect_captures_from_expression(lambda, object, captures, seen);
                self.collect_captures_from_expression(lambda, index, captures, seen);
            }
            ExpressionKind::Slice { object, start, end } => {
                self.collect_captures_from_expression(lambda, object, captures, seen);
                if let Some(start) = start {
                    self.collect_captures_from_expression(lambda, start, captures, seen);
                }
                if let Some(end) = end {
                    self.collect_captures_from_expression(lambda, end, captures, seen);
                }
            }
        }
    }

    fn callable_is_within(&self, mut callable: NodeId, outer: NodeId) -> bool {
        loop {
            if callable == outer {
                return true;
            }
            let Some(Some(parent)) = self.callable_parents.get(&callable) else {
                return false;
            };
            callable = *parent;
        }
    }

    fn check(&mut self, expression: &Expression, expected: TypeId) -> Option<TypedExpression> {
        self.check_with_capability(expression, expected, false)
    }

    fn check_with_capability(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        allow_recursive_copy: bool,
    ) -> Option<TypedExpression> {
        if let ExpressionKind::Block(block) = &expression.kind
            && !self.checking.expressions.contains_key(&expression.id)
        {
            let outcome = self.analyze_block(
                block,
                Some(expected),
                ConditionalUse::Value,
                allow_recursive_copy,
            )?;
            let typed =
                if outcome.explicit_value.is_none() && !self.is_divergence(outcome.typed.type_id) {
                    self.check_typed(expression, expected, outcome.typed, allow_recursive_copy)?
                } else {
                    outcome.typed
                };
            self.checking.expressions.insert(expression.id, typed);
            self.checking
                .explicit_values
                .insert(expression.id, outcome.explicit_value.is_some());
            return Some(typed);
        }
        if matches!(&expression.kind, ExpressionKind::If { .. })
            && !self.checking.expressions.contains_key(&expression.id)
        {
            return self
                .analyze_conditional_expression(
                    expression,
                    Some(expected),
                    ConditionalUse::Value,
                    allow_recursive_copy,
                )
                .map(|outcome| outcome.typed);
        }
        if let ExpressionKind::Group(inner) = &expression.kind
            && !self.checking.expressions.contains_key(&expression.id)
        {
            let typed = self.check_with_capability(inner, expected, allow_recursive_copy)?;
            self.checking.expressions.insert(expression.id, typed);
            let explicitly_produces_value = self
                .checking
                .explicit_values
                .get(&inner.id)
                .copied()
                .unwrap_or(true);
            self.checking
                .explicit_values
                .insert(expression.id, explicitly_produces_value);
            return Some(typed);
        }

        let found = self.synthesize(expression)?;
        self.check_typed(expression, expected, found, allow_recursive_copy)
    }

    fn check_typed(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        found: TypedExpression,
        allow_recursive_copy: bool,
    ) -> Option<TypedExpression> {
        if self.is_recovery(expected)
            || self.is_recovery(found.type_id)
            || self.is_divergence(found.type_id)
        {
            return Some(found);
        }
        if self
            .types
            .types()
            .has_same_shape(found.type_id, expected)
            .expect("checked types must belong to the program type store")
        {
            if self.value_capability_is_compatible(found, expected, allow_recursive_copy) {
                return Some(found);
            }
            return Some(self.report_type_mismatch(expression, expected, found));
        }
        if let Some(converted) = self.check_structural_interface_conversion(
            expression,
            expected,
            found,
            allow_recursive_copy,
        ) {
            return Some(converted);
        }
        let union_member = match self.types.types().get(expected) {
            Some(SemanticType::Union { members, .. }) => members.iter().copied().find(|member| {
                self.types
                    .types()
                    .has_same_shape(found.type_id, *member)
                    .expect("union members belong to the program type store")
                    && self.value_capability_is_compatible(found, *member, allow_recursive_copy)
            }),
            _ => None,
        };
        if let Some(member_type) = union_member {
            self.checking.union_injections.insert(
                expression.id,
                UnionInjection {
                    member_type,
                    union_type: expected,
                },
            );
            let borrowed_erased_view = found.category == ValueCategory::BorrowedPlace
                && self
                    .types
                    .types()
                    .get(found.type_id)
                    .is_some_and(|semantic| {
                        semantic.copy_semantics() == Some(CopySemantics::NonEscapingErasedView)
                    });
            let injected = TypedExpression {
                type_id: expected,
                category: if borrowed_erased_view {
                    ValueCategory::BorrowedPlace
                } else {
                    ValueCategory::FreshTemporary
                },
            };
            self.checking.expressions.insert(expression.id, injected);
            return Some(injected);
        }

        Some(self.report_type_mismatch(expression, expected, found))
    }

    fn report_type_mismatch(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        found: TypedExpression,
    ) -> TypedExpression {
        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            },
            span: expression.span,
        });
        let recovered = TypedExpression {
            type_id: self.types.types().recovery(),
            category: found.category,
        };
        self.checking.expressions.insert(expression.id, recovered);
        recovered
    }

    /// Converts a concrete named or anonymous struct into an explicitly
    /// expected structural interface view. Returning `None` means ordinary
    /// exact-shape or union checking should handle the pair instead.
    fn check_structural_interface_conversion(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        found: TypedExpression,
        _allow_recursive_copy: bool,
    ) -> Option<TypedExpression> {
        let (interface_type, destination_capability, destination_is_gc) =
            self.interface_destination(expected)?;
        let Some((owner, source_capability, source_is_gc)) = self.aggregate_parts(found.type_id)
        else {
            return None;
        };
        let requirements = match self.interface_requirements(interface_type) {
            Ok(requirements) => requirements,
            Err((first, second)) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::ConflictingInterfaceRequirement {
                        first,
                        second,
                    },
                    span: expression.span,
                });
                return Some(self.recover_expression(expression, found.category));
            }
        };

        let requires_gc_receiver = requirements.iter().any(|required| {
            self.signatures
                .method_signature(required.requirement.method_id)
                .is_some_and(|signature| {
                    signature.receiver.storage == ReceiverStorage::GarbageCollected
                })
        });
        if (destination_is_gc || requires_gc_receiver) && !source_is_gc {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InterfaceRequiresGarbageCollectedSource,
                span: expression.span,
            });
            return Some(self.recover_expression(expression, found.category));
        }

        if source_capability == AccessCapability::Const
            && destination_capability == AccessCapability::Mut
            && found.category != ValueCategory::FreshTemporary
        {
            return Some(self.report_type_mismatch(expression, expected, found));
        }

        let signature = self
            .aggregate_signature(owner)
            .expect("concrete interface source must have a struct signature")
            .clone();
        let mut matched = Vec::with_capacity(requirements.len());
        let mut valid = true;
        for required in &requirements {
            let Some(implementation) = signature.member(&required.name).copied() else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MissingInterfaceMethod {
                        declaration: required.requirement.declaration,
                    },
                    span: expression.span,
                });
                valid = false;
                continue;
            };
            let (implementation_declaration, implementation_method) = match implementation.kind {
                StructMemberSignatureKind::Method {
                    declaration,
                    method_id,
                } => (declaration, Some(method_id)),
                StructMemberSignatureKind::Field(field) => (field.declaration, None),
                StructMemberSignatureKind::AssociatedFunction { declaration } => {
                    (declaration, None)
                }
            };
            if implementation_method != Some(required.requirement.method_id) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::IncompatibleInterfaceMethod {
                        requirement: required.requirement.declaration,
                        implementation: implementation_declaration,
                    },
                    span: implementation.span,
                });
                valid = false;
                continue;
            }
            matched.push(required.requirement.method_id);
        }
        if !valid {
            return Some(self.recover_expression(expression, found.category));
        }

        let (category, backing_transfer) = if destination_is_gc {
            (
                ValueCategory::GarbageCollectedReference,
                ValueTransfer::ReuseGarbageCollected,
            )
        } else if source_is_gc {
            (ValueCategory::BorrowedPlace, ValueTransfer::Borrow)
        } else if found.category == ValueCategory::FreshTemporary {
            (ValueCategory::BorrowedPlace, ValueTransfer::MoveTemporary)
        } else {
            (ValueCategory::BorrowedPlace, ValueTransfer::Borrow)
        };
        self.checking.interface_conversions.insert(
            expression.id,
            InterfaceConversion {
                source_type: found.type_id,
                destination_type: expected,
                methods: matched,
                backing_transfer,
            },
        );
        let converted = TypedExpression {
            type_id: expected,
            category,
        };
        self.checking.expressions.insert(expression.id, converted);
        Some(converted)
    }

    /// Peels plain or GC-qualified interface destinations while preserving
    /// the access capability enforced at the conversion boundary.
    fn interface_destination(&self, type_id: TypeId) -> Option<(TypeId, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Interface { capability, .. }
            | SemanticType::Intersection { capability, .. } => Some((type_id, *capability, false)),
            SemanticType::GarbageCollected { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::Interface { .. } | SemanticType::Intersection { .. })
                ) =>
            {
                Some((*target, *capability, true))
            }
            _ => None,
        }
    }

    /// Flattens one interface or intersection into source-ordered requirements.
    /// Identical repeated requirements are deduplicated; a repeated name with
    /// a different method identity makes the intersection uncallable.
    fn interface_requirements(
        &self,
        type_id: TypeId,
    ) -> Result<Vec<RequiredInterfaceMethod>, (NodeId, NodeId)> {
        let mut requirements = Vec::new();
        let mut by_name: HashMap<String, InterfaceRequirementSignature> = HashMap::new();
        self.collect_interface_requirements(type_id, &mut requirements, &mut by_name)?;
        Ok(requirements)
    }

    fn collect_interface_requirements(
        &self,
        type_id: TypeId,
        requirements: &mut Vec<RequiredInterfaceMethod>,
        by_name: &mut HashMap<String, InterfaceRequirementSignature>,
    ) -> Result<(), (NodeId, NodeId)> {
        match self.types.types().get(type_id) {
            Some(SemanticType::Interface { declaration, .. }) => {
                let signature = self
                    .signatures
                    .interface(*declaration)
                    .expect("interface signature must have been collected");
                for name in signature.requirement_order() {
                    let requirement = *signature
                        .requirement(name)
                        .expect("ordered interface requirement remains available");
                    if let Some(previous) = by_name.get(name) {
                        if previous.method_id != requirement.method_id {
                            return Err((previous.declaration, requirement.declaration));
                        }
                        continue;
                    }
                    by_name.insert(name.clone(), requirement);
                    requirements.push(RequiredInterfaceMethod {
                        name: name.clone(),
                        requirement,
                    });
                }
                Ok(())
            }
            Some(SemanticType::Intersection { members, .. }) => {
                for member in members {
                    self.collect_interface_requirements(*member, requirements, by_name)?;
                }
                Ok(())
            }
            _ => unreachable!("interface destination contains only interface members"),
        }
    }

    fn recover_expression(
        &mut self,
        expression: &Expression,
        category: ValueCategory,
    ) -> TypedExpression {
        let recovered = TypedExpression {
            type_id: self.types.types().recovery(),
            category,
        };
        self.checking.expressions.insert(expression.id, recovered);
        recovered
    }

    /// Callable capability is behavioral: a const callable can satisfy a
    /// mutable-capability destination, but a callable that may mutate captures
    /// cannot satisfy a const-callable guarantee.
    fn callable_capability_is_compatible(&self, found: TypeId, expected: TypeId) -> bool {
        let found = self.callable_capability(found);
        let expected = self.callable_capability(expected);
        match (found, expected) {
            (Some(AccessCapability::Mut), Some(AccessCapability::Const)) => false,
            _ => true,
        }
    }

    /// Checks whether a value may acquire the access capability required by a
    /// destination. Copies and fresh storage choose capability independently;
    /// borrowed and GC references may only preserve or reduce access.
    fn value_capability_is_compatible(
        &self,
        found: TypedExpression,
        expected: TypeId,
        allow_recursive_copy: bool,
    ) -> bool {
        if self.callable_capability(found.type_id).is_some()
            && self.callable_capability(expected).is_some()
        {
            return self.callable_capability_is_compatible(found.type_id, expected);
        }
        let Some(found_semantic) = self.types.types().get(found.type_id) else {
            return false;
        };
        let Some(expected_semantic) = self.types.types().get(expected) else {
            return false;
        };
        match (found_semantic.capability(), expected_semantic.capability()) {
            (Some(AccessCapability::Const), Some(AccessCapability::Mut)) => {
                found.category == ValueCategory::FreshTemporary
                    || found_semantic.copy_semantics() == Some(CopySemantics::Trivial)
                    || (allow_recursive_copy
                        && found_semantic.copy_semantics() == Some(CopySemantics::Recursive))
            }
            _ => true,
        }
    }

    fn callable_capability(&self, type_id: TypeId) -> Option<AccessCapability> {
        match self.types.types().get(type_id)? {
            SemanticType::Callable { capability, .. } => Some(*capability),
            SemanticType::GarbageCollected { target, .. } => {
                match self.types.types().get(*target)? {
                    SemanticType::Callable { capability, .. } => Some(*capability),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn synthesize_primitive_conversion(
        &mut self,
        target: PrimitiveType,
        value: &Expression,
    ) -> Option<TypedExpression> {
        let source = match target {
            PrimitiveType::Int => PrimitiveType::Float,
            PrimitiveType::Float => PrimitiveType::Int,
            PrimitiveType::Char => PrimitiveType::Int,
            PrimitiveType::Unit
            | PrimitiveType::None
            | PrimitiveType::Bool
            | PrimitiveType::String
            | PrimitiveType::Bytes => return None,
        };
        let expected = self
            .types
            .types_mut()
            .primitive(source, AccessCapability::Const);
        let checked = self.check(value, expected)?;
        if self.is_recovery(checked.type_id) {
            return Some(self.recovery_temporary());
        }
        Some(self.fresh_primitive(target))
    }

    /// Types prefix GC allocation and records how its operand enters GC storage.
    ///
    /// Fresh temporaries are moved into a new allocation, existing GC
    /// references are reused, and plain places are rejected because allocation
    /// cannot change the storage identity of an existing value.
    fn synthesize_garbage_collection(&mut self, value: &Expression) -> Option<TypedExpression> {
        let source = self.synthesize(value)?;
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("allocated value type belongs to the program type store");
        if matches!(semantic, SemanticType::Recovery | SemanticType::Divergence) {
            return Some(source);
        }
        if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            self.checking
                .transfers
                .insert(value.id, ValueTransfer::ReuseGarbageCollected);
            return Some(TypedExpression {
                type_id: source.type_id,
                category: ValueCategory::GarbageCollectedReference,
            });
        }
        if source.category != ValueCategory::FreshTemporary {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidGarbageCollectionSource {
                    found: source.type_id,
                    category: source.category,
                },
                span: value.span,
            });
            return Some(self.recovery_temporary());
        }

        let type_id = self
            .types
            .types_mut()
            .garbage_collected(source.type_id)
            .expect("fresh value must have GC-qualifiable storage");
        self.checking
            .transfers
            .insert(value.id, ValueTransfer::AllocateGarbageCollected);
        Some(TypedExpression {
            type_id,
            category: ValueCategory::GarbageCollectedReference,
        })
    }

    fn synthesize_unary(
        &mut self,
        operator: UnaryOperator,
        operand: &Expression,
    ) -> Option<TypedExpression> {
        let typed_operand = self.synthesize(operand)?;
        if self.is_recovery(typed_operand.type_id) {
            return Some(self.recovery_temporary());
        }
        let primitive = self.primitive_kind(typed_operand.type_id);
        let valid = matches!(
            (operator, primitive),
            (
                UnaryOperator::Negate,
                Some(PrimitiveType::Int | PrimitiveType::Float)
            ) | (
                UnaryOperator::Not,
                Some(PrimitiveType::Bool | PrimitiveType::Int)
            )
        );
        if valid {
            return Some(
                self.fresh_primitive(primitive.expect("valid unary operand must be primitive")),
            );
        }

        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::InvalidUnaryOperand {
                operator,
                found: typed_operand.type_id,
            },
            span: operand.span,
        });
        Some(self.recovery_temporary())
    }

    fn synthesize_binary(
        &mut self,
        left: &Expression,
        operator: BinaryOperator,
        right: &Expression,
    ) -> Option<TypedExpression> {
        let typed_left = self.synthesize(left)?;
        if self.is_recovery(typed_left.type_id) {
            let _ = self.synthesize(right);
            return Some(self.recovery_temporary());
        }

        let left_primitive = self.primitive_kind(typed_left.type_id);
        let result = match operator {
            BinaryOperator::Add => match left_primitive {
                Some(PrimitiveType::Int | PrimitiveType::Float | PrimitiveType::String) => {
                    left_primitive
                }
                _ => None,
            },
            BinaryOperator::Subtract | BinaryOperator::Multiply | BinaryOperator::Divide => {
                match left_primitive {
                    Some(PrimitiveType::Int | PrimitiveType::Float) => left_primitive,
                    _ => None,
                }
            }
            BinaryOperator::Remainder
            | BinaryOperator::ShiftLeft
            | BinaryOperator::ShiftRight
            | BinaryOperator::BitwiseAnd
            | BinaryOperator::BitwiseXor
            | BinaryOperator::BitwiseOr => match left_primitive {
                Some(PrimitiveType::Int) => Some(PrimitiveType::Int),
                _ => None,
            },
            BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual => match left_primitive {
                Some(PrimitiveType::Int | PrimitiveType::Float | PrimitiveType::Char) => {
                    Some(PrimitiveType::Bool)
                }
                _ => None,
            },
            BinaryOperator::Equal | BinaryOperator::NotEqual => match left_primitive {
                Some(
                    PrimitiveType::Unit
                    | PrimitiveType::None
                    | PrimitiveType::Int
                    | PrimitiveType::Float
                    | PrimitiveType::Bool
                    | PrimitiveType::Char
                    | PrimitiveType::String,
                ) => Some(PrimitiveType::Bool),
                _ => None,
            },
            BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr => match left_primitive {
                Some(PrimitiveType::Bool) => Some(PrimitiveType::Bool),
                _ => None,
            },
        };

        let Some(result) = result else {
            let _ = self.synthesize(right);
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidBinaryOperand {
                    operator,
                    found: typed_left.type_id,
                },
                span: left.span,
            });
            return Some(self.recovery_temporary());
        };

        let operand_type = self
            .types
            .types_mut()
            .with_capability(typed_left.type_id, AccessCapability::Const)
            .expect("primitive operand type belongs to the program type store");
        let typed_right = self.check(right, operand_type)?;
        if self.is_recovery(typed_right.type_id) {
            return Some(self.recovery_temporary());
        }
        if operator == BinaryOperator::Add && left_primitive == Some(PrimitiveType::String) {
            return Some(TypedExpression {
                type_id: self
                    .types
                    .types_mut()
                    .primitive(PrimitiveType::String, AccessCapability::Mut),
                category: ValueCategory::FreshTemporary,
            });
        }
        Some(self.fresh_primitive(result))
    }

    /// Checks a complete named-struct construction as an owning boundary.
    ///
    /// Labels are resolved against collected fields while initializers are
    /// analyzed in source order. Successful values must be independently
    /// storable: primitives copy, fresh plain values move, and GC references
    /// are reused. Named plain values therefore require an explicit `.copy()`.
    fn synthesize_named_struct_construction(
        &mut self,
        expression: &Expression,
        fields: &[StructFieldInitializer],
    ) -> Option<TypedExpression> {
        let symbol = self
            .names
            .symbol_for_reference(expression.id)
            .expect("named construction must have a resolved type symbol");
        let Some(declaration) = self.named_struct_symbols.get(&symbol).copied() else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidConstructionOwner,
                span: expression.span,
            });
            for field in fields {
                let _ = self.synthesize(&field.value);
            }
            return Some(self.recovery_temporary());
        };
        let signature = self
            .signatures
            .named_struct(declaration)
            .expect("named struct signature must have been collected")
            .clone();

        let mut seen = HashSet::new();
        let mut valid = true;
        let mut all_supported = true;
        for field in fields {
            let name = self
                .module
                .text(field.name)
                .expect("field label belongs to the source module")
                .to_string();
            let Some(member) = signature.member(&name).copied() else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownConstructionField,
                    span: field.name,
                });
                valid = false;
                all_supported &= self.synthesize(&field.value).is_some();
                continue;
            };
            if !seen.insert(name) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::DuplicateConstructionField,
                    span: field.name,
                });
                valid = false;
                all_supported &= self.synthesize(&field.value).is_some();
                continue;
            }
            let StructMemberSignatureKind::Field(field_signature) = member.kind else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownConstructionField,
                    span: field.name,
                });
                valid = false;
                all_supported &= self.synthesize(&field.value).is_some();
                continue;
            };
            self.checking.resolved_members.insert(
                field.id,
                ResolvedMember::Field {
                    declaration: field_signature.declaration,
                },
            );
            let expected = field_signature
                .type_id
                .expect("named struct fields always have declared types");
            let Some(checked) = self.check(&field.value, expected) else {
                all_supported = false;
                continue;
            };
            if self.is_recovery(checked.type_id) {
                valid = false;
                continue;
            }
            valid &= self.validate_owning_transfer(&field.value, checked, true);
        }

        for name in signature.field_order() {
            if seen.contains(name) {
                continue;
            }
            let member = signature
                .member(name)
                .expect("ordered field must remain in the member table");
            let StructMemberSignatureKind::Field(field) = member.kind else {
                unreachable!("field order contains only fields")
            };
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::MissingConstructionField {
                    declaration: field.declaration,
                },
                span: expression.span,
            });
            valid = false;
        }

        if !all_supported {
            return None;
        }
        if !valid {
            return Some(self.recovery_temporary());
        }
        let type_id = self
            .types
            .types_mut()
            .with_capability(signature.type_id, AccessCapability::Mut)
            .expect("named struct type belongs to the program type store");
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Checks the fields and methods declared by one anonymous struct and
    /// materializes its compiler-generated nominal type.
    ///
    /// Fields execute at construction time and are therefore analyzed in
    /// source order. Methods are checked only after every inferred field type
    /// is known, so a method may refer to a field declared later in the source.
    fn synthesize_anonymous_struct(
        &mut self,
        expression: &Expression,
        members: &[AnonymousStructMember],
    ) -> Option<TypedExpression> {
        let signature = self
            .signatures
            .anonymous_struct(expression.id)
            .expect("anonymous struct signature must have been collected")
            .clone();
        let first_error = self.checking.errors.len();
        let mut layout_fields = Vec::new();
        let mut all_supported = true;

        for member in members {
            let AnonymousStructMember::Field(field) = member else {
                continue;
            };
            let name = self
                .module
                .text(field.name)
                .expect("anonymous field name belongs to the source module");
            let field_signature = signature
                .member(name)
                .expect("anonymous field must have a collected signature");
            let StructMemberSignatureKind::Field(field_signature) = field_signature.kind else {
                unreachable!("anonymous field must select a field signature")
            };
            self.checking.resolved_members.insert(
                field.id,
                ResolvedMember::Field {
                    declaration: field.id,
                },
            );

            let checked = match field_signature.type_id {
                Some(expected) => self.check(&field.initializer, expected),
                None => self.synthesize(&field.initializer),
            };
            let Some(checked) = checked else {
                all_supported = false;
                let field_type = field_signature
                    .type_id
                    .unwrap_or_else(|| self.types.types().recovery());
                self.checking
                    .anonymous_field_types
                    .insert(field.id, field_type);
                layout_fields.push(LayoutField {
                    declaration: field.id,
                    span: field.span,
                    type_id: field_type,
                });
                continue;
            };
            let field_type = if self.is_recovery(checked.type_id) {
                checked.type_id
            } else {
                field_signature.type_id.unwrap_or(checked.type_id)
            };
            self.checking
                .anonymous_field_types
                .insert(field.id, field_type);
            layout_fields.push(LayoutField {
                declaration: field.id,
                span: field.span,
                type_id: field_type,
            });
            if !self.is_recovery(checked.type_id) {
                self.validate_owning_transfer(&field.initializer, checked, true);
            }
        }

        if !self.aggregate_layouts.contains_key(&expression.id) {
            self.aggregate_order.push(expression.id);
        }
        self.aggregate_layouts.insert(
            expression.id,
            AggregateLayout {
                type_id: signature.type_id,
                fields: layout_fields,
            },
        );

        for member in members {
            let AnonymousStructMember::Method(method) = member else {
                continue;
            };
            self.method_owners.insert(method.id, signature.type_id);
            self.visit_function(method);
        }

        if !all_supported {
            return None;
        }
        if self.checking.errors.len() != first_error {
            return Some(self.recovery_temporary());
        }
        let type_id = self
            .types
            .types_mut()
            .with_capability(signature.type_id, AccessCapability::Mut)
            .expect("anonymous struct type belongs to the program type store");
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Synthesizes field access on a named or anonymous struct and records the
    /// resulting place. Concrete and interface methods, plus `.copy`, fail
    /// here because they are meaningful only as an immediate call callee.
    fn synthesize_member_access(
        &mut self,
        expression: &Expression,
        object: &Expression,
        member: Span,
    ) -> Option<TypedExpression> {
        let typed_object = self.synthesize(object)?;
        if self.is_recovery(typed_object.type_id) {
            return Some(self.recovery_temporary());
        }
        let Some((declaration, object_capability, is_gc)) =
            self.aggregate_parts(typed_object.type_id)
        else {
            if self.interface_destination(typed_object.type_id).is_some() {
                let name = self
                    .module
                    .text(member)
                    .expect("interface member name belongs to the source module");
                match self.interface_requirement_named(typed_object.type_id, name) {
                    Ok(Some(_)) => self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::MethodRequiresCall,
                        span: member,
                    }),
                    Ok(None) => self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::UnknownMember,
                        span: member,
                    }),
                    Err((first, second)) => {
                        self.checking.errors.push(ExpressionCheckingError {
                            kind: ExpressionCheckingErrorKind::ConflictingInterfaceRequirement {
                                first,
                                second,
                            },
                            span: member,
                        });
                    }
                }
                return Some(self.recovery_temporary());
            }
            if self.member_owner_is_definitively_invalid(typed_object.type_id) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::InvalidMemberOwner {
                        found: typed_object.type_id,
                    },
                    span: object.span,
                });
                return Some(self.recovery_temporary());
            }
            return None;
        };
        let name = self
            .module
            .text(member)
            .expect("member name belongs to the source module");
        if name == "copy" {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::CopyRequiresCall,
                span: member,
            });
            return Some(self.recovery_temporary());
        }
        let Some(selected) = self
            .aggregate_signature(declaration)
            .and_then(|signature| signature.member(name))
            .copied()
        else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::UnknownMember,
                span: member,
            });
            return Some(self.recovery_temporary());
        };
        match selected.kind {
            StructMemberSignatureKind::Field(field) => {
                let declared = self.field_type(field.declaration, field.type_id);
                let object_capability =
                    if !is_gc && typed_object.category == ValueCategory::FreshTemporary {
                        AccessCapability::Mut
                    } else {
                        object_capability
                    };
                let type_id = self.field_access_type(declared, object_capability);
                let category = self.field_category(typed_object, type_id);
                let capability = self
                    .types
                    .types()
                    .get(type_id)
                    .and_then(SemanticType::capability)
                    .expect("field type has a value capability");
                self.checking.places.insert(
                    expression.id,
                    Place {
                        symbol: None,
                        type_id,
                        category,
                        binding_mutability: None,
                        value_capability: match capability {
                            AccessCapability::Const => ValueCapability::Const,
                            AccessCapability::Mut => ValueCapability::Mut,
                        },
                    },
                );
                self.checking.resolved_members.insert(
                    expression.id,
                    ResolvedMember::Field {
                        declaration: field.declaration,
                    },
                );
                Some(TypedExpression { type_id, category })
            }
            StructMemberSignatureKind::Method { .. } => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MethodRequiresCall,
                    span: member,
                });
                Some(self.recovery_temporary())
            }
            StructMemberSignatureKind::AssociatedFunction { .. } => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                    span: member,
                });
                Some(self.recovery_temporary())
            }
        }
    }

    /// Selects one requirement by source member name after flattening and
    /// validating an interface intersection.
    fn interface_requirement_named(
        &self,
        type_id: TypeId,
        name: &str,
    ) -> Result<Option<RequiredInterfaceMethod>, (NodeId, NodeId)> {
        let Some((interface, _, _)) = self.interface_destination(type_id) else {
            return Ok(None);
        };
        Ok(self
            .interface_requirements(interface)?
            .into_iter()
            .find(|required| required.name == name))
    }

    /// Selects a receiverless named-struct function through `Type::function`.
    ///
    /// Unlike instance methods, associated functions are ordinary first-class
    /// callable values because they carry no hidden receiver.
    fn synthesize_named_associated_access(
        &mut self,
        expression: &Expression,
        owner: &TypeSyntax,
        member: Span,
    ) -> Option<TypedExpression> {
        let owner_type = self.types.type_for_syntax(owner.id)?;
        let Some(SemanticType::NamedStruct { declaration, .. }) =
            self.types.types().get(owner_type).cloned()
        else {
            return None;
        };
        let name = self
            .module
            .text(member)
            .expect("associated member name belongs to the source module");
        if name == "copy" {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::CopyRequiresValue,
                span: member,
            });
            return Some(self.recovery_temporary());
        }
        let Some(selected) = self
            .signatures
            .named_struct(declaration)
            .and_then(|signature| signature.member(name))
            .copied()
        else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::UnknownMember,
                span: member,
            });
            return Some(self.recovery_temporary());
        };
        let declaration = match selected.kind {
            StructMemberSignatureKind::AssociatedFunction { declaration } => declaration,
            StructMemberSignatureKind::Field(_) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::FieldRequiresValue,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            StructMemberSignatureKind::Method { .. } => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MethodRequiresValue,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
        };
        let signature = self
            .signatures
            .callable(declaration)
            .expect("associated function signature must have been collected");
        let type_id = self.types.types_mut().callable(
            signature.parameters.clone(),
            signature.return_type,
            AccessCapability::Const,
        );
        self.checking.resolved_members.insert(
            expression.id,
            ResolvedMember::AssociatedFunction { declaration },
        );
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Peels a plain or GC-qualified concrete struct into the information
    /// shared by field, method, copy, and structural-interface checking. The
    /// boolean distinguishes GC storage for receiver validation.
    fn aggregate_parts(&self, type_id: TypeId) -> Option<(NodeId, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::NamedStruct {
                declaration,
                capability,
            }
            | SemanticType::AnonymousStruct {
                expression: declaration,
                capability,
            } => Some((*declaration, *capability, false)),
            SemanticType::GarbageCollected { target, capability } => {
                match self.types.types().get(*target)? {
                    SemanticType::NamedStruct { declaration, .. }
                    | SemanticType::AnonymousStruct {
                        expression: declaration,
                        ..
                    } => Some((*declaration, *capability, true)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Finds the collected member table for either a source-named struct or a
    /// compiler-named anonymous struct.
    fn aggregate_signature(&self, owner: NodeId) -> Option<&StructSignature> {
        self.signatures
            .named_struct(owner)
            .or_else(|| self.signatures.anonymous_struct(owner))
    }

    /// Completes a field signature by consulting expression-time inference for
    /// anonymous fields that had no source annotation.
    fn field_type(&self, declaration: NodeId, collected: Option<TypeId>) -> TypeId {
        collected
            .or_else(|| {
                self.checking
                    .anonymous_field_types
                    .get(&declaration)
                    .copied()
            })
            .expect("checked anonymous field must have an inferred type")
    }

    /// Identifies owners that cannot gain a member family in a later increment.
    /// Strings, bytes, built-ins, and interfaces are omitted
    /// because their member checking is intentionally still deferred.
    fn member_owner_is_definitively_invalid(&self, type_id: TypeId) -> bool {
        matches!(
            self.types.types().get(type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Unit
                    | PrimitiveType::None
                    | PrimitiveType::Int
                    | PrimitiveType::Float
                    | PrimitiveType::Bool
                    | PrimitiveType::Char,
                ..
            })
        )
    }

    /// Applies transitive access capability to a field reached through an
    /// object. Inline fields inherit the object's access. GC-valued fields also
    /// retain the capability stored in their reference, so neither route can
    /// turn const access back into mutable access.
    fn field_access_type(
        &mut self,
        declared: TypeId,
        object_capability: AccessCapability,
    ) -> TypeId {
        let declared_semantic = self
            .types
            .types()
            .get(declared)
            .expect("field type belongs to the program type store");
        let capability =
            if declared_semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
                match (object_capability, declared_semantic.capability()) {
                    (AccessCapability::Const, _) | (_, Some(AccessCapability::Const)) => {
                        AccessCapability::Const
                    }
                    _ => AccessCapability::Mut,
                }
            } else {
                object_capability
            };
        self.types
            .types_mut()
            .with_capability(declared, capability)
            .expect("field type belongs to the program type store")
    }

    /// Determines the storage provenance observed through a field access.
    ///
    /// Inline ownership is preserved through owned or fresh objects. Access
    /// through a borrowed or GC-backed object is borrowed, while a GC-valued
    /// field remains a GC reference regardless of its containing object.
    fn field_category(&self, object: TypedExpression, field_type: TypeId) -> ValueCategory {
        if self.types.types().get(field_type).is_some_and(|semantic| {
            semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected)
        }) {
            return ValueCategory::GarbageCollectedReference;
        }
        match object.category {
            ValueCategory::FreshTemporary => ValueCategory::FreshTemporary,
            ValueCategory::OwnedInlinePlace => ValueCategory::OwnedInlinePlace,
            ValueCategory::BorrowedPlace | ValueCategory::GarbageCollectedReference => {
                ValueCategory::BorrowedPlace
            }
        }
    }

    /// Validates a value entering owned aggregate storage and optionally
    /// records the selected transfer once the destination itself is valid.
    ///
    /// The `record` switch lets an immutable field still report an independent
    /// invalid-source diagnostic without claiming that assignment occurred.
    fn validate_owning_transfer(
        &mut self,
        source_expression: &Expression,
        source: TypedExpression,
        record: bool,
    ) -> bool {
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("owning source type belongs to the program type store");
        let transfer = if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            Some(ValueTransfer::ReuseGarbageCollected)
        } else if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
            Some(ValueTransfer::TrivialCopy)
        } else if source.category == ValueCategory::FreshTemporary {
            Some(ValueTransfer::MoveTemporary)
        } else {
            None
        };
        if let Some(transfer) = transfer {
            if record {
                self.checking
                    .transfers
                    .insert(source_expression.id, transfer);
            }
            return true;
        }
        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::InvalidOwningSource {
                found: source.type_id,
                category: source.category,
            },
            span: source_expression.span,
        });
        self.checking.expressions.insert(
            source_expression.id,
            TypedExpression {
                type_id: self.types.types().recovery(),
                category: source.category,
            },
        );
        false
    }

    /// Dispatches assignment according to the semantic kind of place. Root
    /// identifiers redirect bindings, whereas fields replace or mutate storage
    /// owned by their containing aggregate.
    fn synthesize_place_assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
    ) -> Option<TypedExpression> {
        match &target.kind {
            ExpressionKind::Identifier => self.synthesize_root_assignment(target, operator, value),
            ExpressionKind::MemberAccess { .. } => {
                self.synthesize_field_assignment(target, operator, value)
            }
            _ => unreachable!("place assignment dispatch accepts only implemented places"),
        }
    }

    /// Checks assignment through a direct field place.
    ///
    /// Field replacement is controlled by value access rather than the root
    /// binding's mutability. Simple assignment is an owning boundary; compound
    /// assignment mutates the existing primitive or string value in place.
    fn synthesize_field_assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
    ) -> Option<TypedExpression> {
        let typed_target = self.synthesize(target)?;
        let Some(place) = self.checking.places.get(&target.id).copied() else {
            let _ = self.synthesize(value);
            return Some(self.recovery_temporary());
        };
        let mutable = place.value_capability == ValueCapability::Mut;
        if !mutable {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ImmutableValue,
                span: target.span,
            });
        }

        if operator == AssignmentOperator::Assign {
            let checked = self.check(value, place.type_id)?;
            if self.is_recovery(checked.type_id) {
                return Some(self.recovery_temporary());
            }
            if !self.validate_owning_transfer(value, checked, mutable) || !mutable {
                return Some(self.recovery_temporary());
            }
            return Some(self.fresh_primitive(PrimitiveType::Unit));
        }

        let primitive = self.primitive_kind(typed_target.type_id);
        let string_append =
            operator == AssignmentOperator::Add && primitive == Some(PrimitiveType::String);
        let valid_operator = matches!(
            (operator, primitive),
            (
                AssignmentOperator::Add
                    | AssignmentOperator::Subtract
                    | AssignmentOperator::Multiply
                    | AssignmentOperator::Divide,
                Some(PrimitiveType::Int | PrimitiveType::Float)
            ) | (AssignmentOperator::Add, Some(PrimitiveType::String))
                | (
                    AssignmentOperator::Remainder
                        | AssignmentOperator::BitwiseAnd
                        | AssignmentOperator::BitwiseXor
                        | AssignmentOperator::BitwiseOr
                        | AssignmentOperator::ShiftLeft
                        | AssignmentOperator::ShiftRight,
                    Some(PrimitiveType::Int)
                )
        );
        if !valid_operator {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidAssignmentOperand {
                    operator,
                    found: typed_target.type_id,
                },
                span: target.span,
            });
            let _ = self.synthesize(value);
            return Some(self.recovery_temporary());
        }
        let expected = self
            .types
            .types_mut()
            .with_capability(typed_target.type_id, AccessCapability::Const)
            .expect("compound-assignment type belongs to the program type store");
        let checked = self.check(value, expected)?;
        if !mutable || self.is_recovery(checked.type_id) {
            return Some(self.recovery_temporary());
        }
        self.checking.transfers.insert(
            value.id,
            if string_append {
                ValueTransfer::Borrow
            } else {
                ValueTransfer::TrivialCopy
            },
        );
        Some(self.fresh_primitive(PrimitiveType::Unit))
    }

    /// Checks assignment to an identifier root. Plain assignment redirects the
    /// root's reference slot; it never overwrites or recursively copies the
    /// object denoted by that slot.
    fn synthesize_root_assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
    ) -> Option<TypedExpression> {
        let typed_target = self.synthesize(target)?;
        let Some(place) = self.checking.places.get(&target.id).copied() else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidAssignmentTarget,
                span: target.span,
            });
            let _ = self.synthesize(value);
            return Some(self.recovery_temporary());
        };
        let symbol = place
            .symbol
            .expect("an identifier root place must have a symbol");

        if operator == AssignmentOperator::Assign {
            let mutable = place.binding_mutability == Some(BindingMutability::Mut);
            if !mutable {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::ImmutableBinding,
                    span: target.span,
                });
            }
            let checked = self.check(value, place.type_id)?;
            let valid_value = !self.is_recovery(checked.type_id);
            if mutable && valid_value {
                let (category, transfer) = self.assignment_transfer(checked);
                self.current_binding_categories.insert(symbol, category);
                self.checking.reassigned_bindings.insert(symbol);
                self.checking.transfers.insert(value.id, transfer);
                return Some(self.fresh_primitive(PrimitiveType::Unit));
            }
            return Some(self.recovery_temporary());
        }

        let primitive = self.primitive_kind(typed_target.type_id);
        let string_append =
            operator == AssignmentOperator::Add && primitive == Some(PrimitiveType::String);
        let mutable_destination = if string_append {
            place.value_capability == ValueCapability::Mut
        } else {
            place.binding_mutability == Some(BindingMutability::Mut)
        };
        if !mutable_destination {
            self.checking.errors.push(ExpressionCheckingError {
                kind: if string_append {
                    ExpressionCheckingErrorKind::ImmutableValue
                } else {
                    ExpressionCheckingErrorKind::ImmutableBinding
                },
                span: target.span,
            });
        }

        let valid_operator = matches!(
            (operator, primitive),
            (
                AssignmentOperator::Add
                    | AssignmentOperator::Subtract
                    | AssignmentOperator::Multiply
                    | AssignmentOperator::Divide,
                Some(PrimitiveType::Int | PrimitiveType::Float)
            ) | (AssignmentOperator::Add, Some(PrimitiveType::String))
                | (
                    AssignmentOperator::Remainder
                        | AssignmentOperator::BitwiseAnd
                        | AssignmentOperator::BitwiseXor
                        | AssignmentOperator::BitwiseOr
                        | AssignmentOperator::ShiftLeft
                        | AssignmentOperator::ShiftRight,
                    Some(PrimitiveType::Int)
                )
        );
        if !valid_operator {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidAssignmentOperand {
                    operator,
                    found: typed_target.type_id,
                },
                span: target.span,
            });
            let _ = self.synthesize(value);
            return Some(self.recovery_temporary());
        }

        let expected_value = if string_append {
            self.types
                .types_mut()
                .with_capability(typed_target.type_id, AccessCapability::Const)
                .expect("string assignment type belongs to the program type store")
        } else {
            typed_target.type_id
        };
        let checked = self.check(value, expected_value)?;
        if !mutable_destination || self.is_recovery(checked.type_id) {
            return Some(self.recovery_temporary());
        }
        if string_append {
            self.checking
                .transfers
                .insert(value.id, ValueTransfer::Borrow);
        } else {
            self.checking
                .transfers
                .insert(value.id, ValueTransfer::TrivialCopy);
            self.current_binding_categories
                .insert(symbol, ValueCategory::OwnedInlinePlace);
            self.checking.reassigned_bindings.insert(symbol);
        }
        Some(self.fresh_primitive(PrimitiveType::Unit))
    }

    fn synthesize_call(
        &mut self,
        expression: &Expression,
        callee: &Expression,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        if let Some(result) = self.synthesize_member_call(expression, callee, arguments) {
            return result;
        }
        let typed_callee = self.synthesize(callee)?;
        if self.is_recovery(typed_callee.type_id) {
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        }

        let callable = match self.types.types().get(typed_callee.type_id).cloned() {
            Some(SemanticType::Callable {
                parameters,
                return_type,
                ..
            }) => Some((parameters, return_type)),
            Some(SemanticType::GarbageCollected { target, .. }) => {
                match self.types.types().get(target).cloned() {
                    Some(SemanticType::Callable {
                        parameters,
                        return_type,
                        ..
                    }) => Some((parameters, return_type)),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((parameters, return_type)) = callable else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::NotCallable {
                    found: typed_callee.type_id,
                },
                span: callee.span,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        };

        let arguments_valid = self.analyze_call_arguments(expression, arguments, &parameters)?;
        if !arguments_valid || self.is_recovery(return_type) {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(return_type))
    }

    /// Handles compiler-provided copies and concrete or interface methods before an
    /// ordinary call attempts to synthesize its callee as a first-class value.
    /// Returning `None` means the member is a field (possibly callable) or
    /// belongs to a member family deferred to a later increment.
    fn synthesize_member_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        arguments: &[Expression],
    ) -> Option<Option<TypedExpression>> {
        let ExpressionKind::MemberAccess { object, member } = &callee.kind else {
            return None;
        };
        let typed_object = match self.synthesize(object) {
            Some(typed) => typed,
            None => return None,
        };
        if self.is_recovery(typed_object.type_id) {
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(Some(self.recovery_temporary()));
        }
        let aggregate = self.aggregate_parts(typed_object.type_id);
        let name = self
            .module
            .text(*member)
            .expect("member name belongs to the source module")
            .to_string();
        if aggregate.is_none() {
            if self.interface_destination(typed_object.type_id).is_some() {
                return Some(self.synthesize_interface_method_call(
                    call,
                    callee,
                    object,
                    typed_object,
                    *member,
                    &name,
                    arguments,
                ));
            }
            return None;
        }
        let (owner, object_capability, is_gc) = aggregate.expect("aggregate presence was checked");
        if name == "copy" {
            let valid_arity = arguments.is_empty();
            if !valid_arity {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                        expected: 0,
                        found: arguments.len(),
                    },
                    span: call.span,
                });
            }
            let mut all_supported = true;
            for argument in arguments {
                all_supported &= self.synthesize(argument).is_some();
            }
            if !all_supported {
                return Some(None);
            }
            self.checking.resolved_members.insert(
                callee.id,
                ResolvedMember::Copy {
                    source_type: typed_object.type_id,
                },
            );
            if !valid_arity {
                return Some(Some(self.recovery_temporary()));
            }
            self.checking
                .transfers
                .insert(object.id, ValueTransfer::RecursiveCopy);
            let plain = self
                .aggregate_signature(owner)
                .expect("copy owner has a struct signature")
                .type_id;
            let type_id = self
                .types
                .types_mut()
                .with_capability(plain, AccessCapability::Mut)
                .expect("copied struct type belongs to the program type store");
            return Some(Some(TypedExpression {
                type_id,
                category: ValueCategory::FreshTemporary,
            }));
        }

        let Some(selected) = self
            .aggregate_signature(owner)
            .and_then(|signature| signature.member(&name))
            .copied()
        else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::UnknownMember,
                span: *member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(Some(self.recovery_temporary()));
        };
        let StructMemberSignatureKind::Method {
            declaration,
            method_id,
        } = selected.kind
        else {
            if matches!(
                selected.kind,
                StructMemberSignatureKind::AssociatedFunction { .. }
            ) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                    span: *member,
                });
                for argument in arguments {
                    let _ = self.synthesize(argument);
                }
                return Some(Some(self.recovery_temporary()));
            }
            return None;
        };
        let signature = self
            .signatures
            .callable(declaration)
            .expect("method signature must have been collected")
            .clone();
        let receiver = signature
            .receiver
            .expect("instance method must have a receiver signature");
        let receiver_valid =
            self.check_method_receiver(object, typed_object, receiver, object_capability, is_gc);
        self.checking.resolved_members.insert(
            callee.id,
            ResolvedMember::Method {
                declaration,
                method_id,
            },
        );
        let arguments_valid =
            match self.analyze_call_arguments(call, arguments, &signature.parameters) {
                Some(valid) => valid,
                None => return Some(None),
            };
        if !receiver_valid || !arguments_valid || self.is_recovery(signature.return_type) {
            return Some(Some(self.recovery_temporary()));
        }
        Some(Some(self.call_result(signature.return_type)))
    }

    /// Invokes one structurally selected interface requirement. The interface
    /// type fixes the receiver shape; the runtime vtable supplies the concrete
    /// receiver adapter recorded by the conversion that created the view.
    fn synthesize_interface_method_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        member: Span,
        name: &str,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let required = match self.interface_requirement_named(typed_object.type_id, name) {
            Ok(Some(required)) => required,
            Ok(None) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownMember,
                    span: member,
                });
                for argument in arguments {
                    let _ = self.synthesize(argument);
                }
                return Some(self.recovery_temporary());
            }
            Err((first, second)) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::ConflictingInterfaceRequirement {
                        first,
                        second,
                    },
                    span: member,
                });
                for argument in arguments {
                    let _ = self.synthesize(argument);
                }
                return Some(self.recovery_temporary());
            }
        };
        let signature = self
            .signatures
            .callable(required.requirement.declaration)
            .expect("interface requirement signature must have been collected")
            .clone();
        let receiver = signature
            .receiver
            .expect("interface requirement must have a receiver");
        let (_, object_capability, _) = self
            .interface_destination(typed_object.type_id)
            .expect("interface method receiver has an interface type");
        let receiver_valid = if receiver.capability == AccessCapability::Mut
            && object_capability == AccessCapability::Const
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ReceiverCapabilityMismatch,
                span: object.span,
            });
            false
        } else {
            true
        };
        let transfer = match receiver.storage {
            ReceiverStorage::GarbageCollected => ValueTransfer::ReuseGarbageCollected,
            ReceiverStorage::Plain => ValueTransfer::Borrow,
        };
        self.checking.transfers.insert(object.id, transfer);
        self.checking.resolved_members.insert(
            callee.id,
            ResolvedMember::InterfaceMethod {
                declaration: required.requirement.declaration,
                method_id: required.requirement.method_id,
            },
        );
        let arguments_valid =
            self.analyze_call_arguments(call, arguments, &signature.parameters)?;
        if !receiver_valid || !arguments_valid || self.is_recovery(signature.return_type) {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(signature.return_type))
    }

    /// Validates the hidden receiver supplied by a direct method call and
    /// records how it is passed. Plain methods use `Borrow` regardless of the
    /// object's storage class; `&self` methods reuse a GC reference. A fresh
    /// plain temporary may independently select mut access.
    fn check_method_receiver(
        &mut self,
        object: &Expression,
        typed_object: TypedExpression,
        receiver: ReceiverSignature,
        object_capability: AccessCapability,
        is_gc: bool,
    ) -> bool {
        let storage_valid = receiver.storage == ReceiverStorage::Plain || is_gc;
        if !storage_valid {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ReceiverStorageMismatch,
                span: object.span,
            });
        }
        let capability_valid = !matches!(
            (object_capability, receiver.capability),
            (AccessCapability::Const, AccessCapability::Mut)
        ) || (!is_gc
            && typed_object.category == ValueCategory::FreshTemporary);
        if !capability_valid {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ReceiverCapabilityMismatch,
                span: object.span,
            });
        }
        if storage_valid && capability_valid {
            let transfer = match receiver.storage {
                ReceiverStorage::GarbageCollected => ValueTransfer::ReuseGarbageCollected,
                ReceiverStorage::Plain => ValueTransfer::Borrow,
            };
            self.checking.transfers.insert(object.id, transfer);
        }
        storage_valid && capability_valid
    }

    /// Checks call arguments from left to right after callee/receiver lookup.
    ///
    /// Arity failure does not stop argument analysis. Transfers are recorded
    /// only for arguments that correspond to parameters and check successfully;
    /// surplus arguments are analyzed without receiving transfer metadata.
    fn analyze_call_arguments(
        &mut self,
        call: &Expression,
        arguments: &[Expression],
        parameters: &[TypeId],
    ) -> Option<bool> {
        let arity_matches = parameters.len() == arguments.len();
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: parameters.len(),
                    found: arguments.len(),
                },
                span: call.span,
            });
        }
        let mut valid = arity_matches;
        let mut all_supported = true;
        for (index, argument) in arguments.iter().enumerate() {
            let Some(expected) = parameters.get(index).copied() else {
                all_supported &= self.synthesize(argument).is_some();
                continue;
            };
            let Some(checked) = self.check(argument, expected) else {
                all_supported = false;
                continue;
            };
            if self.is_recovery(checked.type_id) {
                valid = false;
                continue;
            }
            if let Some(transfer) = self.argument_transfer(checked) {
                self.checking.transfers.insert(argument.id, transfer);
            }
        }
        all_supported.then_some(valid)
    }

    /// Gives all successful ordinary calls their declared result type. GC
    /// results preserve reference provenance; other results are fresh values
    /// supplied by the callee's result storage.
    fn call_result(&self, return_type: TypeId) -> TypedExpression {
        let category = if self.types.types().get(return_type).is_some_and(|semantic| {
            semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected)
        }) {
            ValueCategory::GarbageCollectedReference
        } else {
            ValueCategory::FreshTemporary
        };
        TypedExpression {
            type_id: return_type,
            category,
        }
    }

    fn primitive_kind(&self, type_id: TypeId) -> Option<PrimitiveType> {
        match self.types.types().get(type_id) {
            Some(SemanticType::Primitive { primitive, .. }) => Some(*primitive),
            _ => None,
        }
    }

    fn fresh_primitive(&mut self, primitive: PrimitiveType) -> TypedExpression {
        TypedExpression {
            type_id: self
                .types
                .types_mut()
                .primitive(primitive, AccessCapability::Const),
            category: ValueCategory::FreshTemporary,
        }
    }

    fn recovery_temporary(&self) -> TypedExpression {
        TypedExpression {
            type_id: self.types.types().recovery(),
            category: ValueCategory::FreshTemporary,
        }
    }

    fn synthesize_literal(
        &mut self,
        expression: &Expression,
        literal: LiteralKind,
    ) -> TypedExpression {
        let (primitive, capability) = match literal {
            LiteralKind::Unit => (PrimitiveType::Unit, AccessCapability::Const),
            LiteralKind::Integer => {
                let spelling = self
                    .module
                    .text(expression.span)
                    .expect("literal span must belong to its source module");
                if spelling.parse::<i64>().is_err() {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::IntegerLiteralOutOfRange,
                        span: expression.span,
                    });
                    return TypedExpression {
                        type_id: self.types.types().recovery(),
                        category: ValueCategory::FreshTemporary,
                    };
                }
                (PrimitiveType::Int, AccessCapability::Const)
            }
            LiteralKind::Float => (PrimitiveType::Float, AccessCapability::Const),
            LiteralKind::Boolean(_) => (PrimitiveType::Bool, AccessCapability::Const),
            LiteralKind::Character => (PrimitiveType::Char, AccessCapability::Const),
            LiteralKind::String => (PrimitiveType::String, AccessCapability::Mut),
            LiteralKind::None => (PrimitiveType::None, AccessCapability::Const),
        };
        TypedExpression {
            type_id: self.types.types_mut().primitive(primitive, capability),
            category: ValueCategory::FreshTemporary,
        }
    }

    fn synthesize_identifier(&mut self, expression: &Expression) -> Option<TypedExpression> {
        let symbol = self
            .names
            .symbol_for_reference(expression.id)
            .expect("identifier must have a resolved semantic symbol");
        if let Some(binding) = self.checking.bindings.get(&symbol).copied() {
            let category = self
                .current_binding_categories
                .get(&symbol)
                .copied()
                .unwrap_or(binding.category);
            let typed = TypedExpression {
                type_id: binding.type_id,
                category,
            };
            self.checking.places.insert(
                expression.id,
                Place {
                    symbol: Some(symbol),
                    type_id: typed.type_id,
                    category,
                    binding_mutability: Some(binding.qualifiers.binding),
                    value_capability: binding.qualifiers.value,
                },
            );
            return Some(typed);
        }
        let type_id = self.signatures.callable_value_type(symbol)?;
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    fn synthesize_self(&mut self, expression: &Expression) -> TypedExpression {
        let method = self
            .context
            .method_for_self(expression.id)
            .expect("self expression must have a resolved method target");
        let owner = *self
            .method_owners
            .get(&method)
            .expect("named method must have a recorded owner type");
        let receiver = self
            .signatures
            .callable(method)
            .and_then(|signature| signature.receiver)
            .expect("self target must have a receiver signature");
        let owner = self
            .types
            .types_mut()
            .with_capability(owner, receiver.capability)
            .expect("method owner type belongs to the program type store");
        let typed = match receiver.storage {
            ReceiverStorage::Plain => TypedExpression {
                type_id: owner,
                category: ValueCategory::BorrowedPlace,
            },
            ReceiverStorage::GarbageCollected => TypedExpression {
                type_id: self
                    .types
                    .types_mut()
                    .garbage_collected(owner)
                    .expect("method owner is a value type"),
                category: ValueCategory::GarbageCollectedReference,
            },
        };
        self.checking.places.insert(
            expression.id,
            Place {
                symbol: None,
                type_id: typed.type_id,
                category: typed.category,
                binding_mutability: None,
                value_capability: match receiver.capability {
                    AccessCapability::Const => ValueCapability::Const,
                    AccessCapability::Mut => ValueCapability::Mut,
                },
            },
        );
        typed
    }

    fn parameter_category(&self, type_id: TypeId) -> ValueCategory {
        let semantic = self
            .types
            .types()
            .get(type_id)
            .expect("parameter type belongs to the program type store");
        match semantic.storage_semantics() {
            Some(StorageSemantics::GarbageCollected) => ValueCategory::GarbageCollectedReference,
            _ if semantic.copy_semantics() == Some(CopySemantics::Trivial) => {
                ValueCategory::OwnedInlinePlace
            }
            _ => ValueCategory::BorrowedPlace,
        }
    }

    fn binding_transfer(&self, source: TypedExpression) -> (ValueCategory, Option<ValueTransfer>) {
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("initializer type belongs to the program type store");
        if matches!(semantic, SemanticType::Recovery | SemanticType::Divergence) {
            return (source.category, None);
        }
        if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            return (
                ValueCategory::GarbageCollectedReference,
                Some(ValueTransfer::ReuseGarbageCollected),
            );
        }
        if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
            return (
                ValueCategory::OwnedInlinePlace,
                Some(ValueTransfer::TrivialCopy),
            );
        }
        if source.category == ValueCategory::FreshTemporary {
            return (
                ValueCategory::OwnedInlinePlace,
                Some(ValueTransfer::MoveTemporary),
            );
        }
        (ValueCategory::BorrowedPlace, Some(ValueTransfer::Borrow))
    }

    fn assignment_transfer(&self, source: TypedExpression) -> (ValueCategory, ValueTransfer) {
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("assigned type belongs to the program type store");
        if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            return (
                ValueCategory::GarbageCollectedReference,
                ValueTransfer::ReuseGarbageCollected,
            );
        }
        if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
            return (ValueCategory::OwnedInlinePlace, ValueTransfer::TrivialCopy);
        }
        if source.category == ValueCategory::FreshTemporary {
            return (
                ValueCategory::OwnedInlinePlace,
                ValueTransfer::MoveTemporary,
            );
        }
        (source.category, ValueTransfer::Borrow)
    }

    /// Rejects aggregates whose inline fields recursively require storage for
    /// the aggregate itself. GC references and external-buffer built-ins stop
    /// traversal because their payloads are not embedded inline.
    fn validate_finite_inline_layouts(&mut self) {
        let mut edges: HashMap<NodeId, Vec<(NodeId, LayoutField)>> = HashMap::new();
        for owner in &self.aggregate_order {
            let Some(layout) = self.aggregate_layouts.get(owner) else {
                continue;
            };
            let mut owner_edges = Vec::new();
            for field in &layout.fields {
                let mut dependencies = Vec::new();
                self.inline_aggregate_dependencies(
                    field.type_id,
                    &mut dependencies,
                    &mut HashSet::new(),
                );
                for dependency in dependencies {
                    owner_edges.push((dependency, *field));
                }
            }
            edges.insert(*owner, owner_edges);
        }

        for component in strongly_connected_components(&self.aggregate_order, &edges) {
            let members: HashSet<NodeId> = component.iter().copied().collect();
            let cyclic = component.len() > 1
                || component.first().is_some_and(|owner| {
                    edges
                        .get(owner)
                        .is_some_and(|outgoing| outgoing.iter().any(|(target, _)| target == owner))
                });
            if !cyclic {
                continue;
            }
            let offending = self.aggregate_order.iter().find_map(|owner| {
                if !members.contains(owner) {
                    return None;
                }
                edges.get(owner).and_then(|outgoing| {
                    outgoing
                        .iter()
                        .find(|(target, _)| members.contains(target))
                        .map(|(_, field)| (*owner, *field))
                })
            });
            let Some((owner, field)) = offending else {
                continue;
            };
            let owner_type = self
                .aggregate_layouts
                .get(&owner)
                .expect("cyclic aggregate remains in the layout table")
                .type_id;
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InfiniteInlineLayout { owner: owner_type },
                span: field.span,
            });
        }
    }

    fn inline_aggregate_dependencies(
        &self,
        type_id: TypeId,
        dependencies: &mut Vec<NodeId>,
        visited: &mut HashSet<TypeId>,
    ) {
        if !visited.insert(type_id) {
            return;
        }
        match self.types.types().get(type_id) {
            Some(SemanticType::NamedStruct { declaration, .. }) => {
                if !dependencies.contains(declaration) {
                    dependencies.push(*declaration);
                }
            }
            Some(SemanticType::AnonymousStruct { expression, .. }) => {
                if !dependencies.contains(expression) {
                    dependencies.push(*expression);
                }
            }
            Some(SemanticType::Union { members, .. }) => {
                for member in members {
                    self.inline_aggregate_dependencies(*member, dependencies, visited);
                }
            }
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Error,
                arguments,
                ..
            }) => {
                for argument in arguments {
                    self.inline_aggregate_dependencies(*argument, dependencies, visited);
                }
            }
            Some(
                SemanticType::GarbageCollected { .. }
                | SemanticType::Primitive { .. }
                | SemanticType::Callable { .. }
                | SemanticType::Interface { .. }
                | SemanticType::Intersection { .. }
                | SemanticType::Builtin { .. }
                | SemanticType::Recovery
                | SemanticType::Divergence,
            )
            | None => {}
        }
    }

    fn argument_transfer(&self, source: TypedExpression) -> Option<ValueTransfer> {
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("argument type belongs to the program type store");
        if matches!(semantic, SemanticType::Recovery | SemanticType::Divergence) {
            return None;
        }
        if semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected) {
            return Some(ValueTransfer::ReuseGarbageCollected);
        }
        if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
            return Some(ValueTransfer::TrivialCopy);
        }
        Some(ValueTransfer::Borrow)
    }

    fn with_value_capability(&mut self, type_id: TypeId, capability: ValueCapability) -> TypeId {
        self.types
            .types_mut()
            .with_capability(type_id, capability.into())
            .expect("semantic type belongs to the program type store")
    }

    fn is_recovery(&self, type_id: TypeId) -> bool {
        type_id == self.types.types().recovery()
    }

    fn is_divergence(&self, type_id: TypeId) -> bool {
        type_id == self.types.types().divergence()
    }
}

fn push_unique_capture(
    source: LambdaCaptureSource,
    captures: &mut Vec<LambdaCaptureSource>,
    seen: &mut HashSet<LambdaCaptureSource>,
) {
    if seen.insert(source) {
        captures.push(source);
    }
}

/// Partitions the inline aggregate-containment graph using Robert Tarjan's
/// strongly connected components algorithm.
///
/// Robert Tarjan introduced this depth-first-search algorithm in 1972. A
/// strongly connected component is a maximal group of graph nodes in which
/// every node can reach every other node. Here, nodes are named or anonymous
/// structs and an edge `A -> B` means that `A` contains `B` inline. A component
/// containing multiple structs therefore describes mutually recursive inline
/// storage; a one-node component is recursive only when it has a self-edge.
/// Both shapes have infinite size and are rejected by layout validation.
/// Edges do not cross GC references, so `next: &Node | none` does not make
/// `Node` part of an inline cycle.
///
/// During one depth-first traversal, Tarjan's algorithm assigns each node a
/// monotonically increasing discovery index and a `low_link`: the earliest
/// discovery index reachable while remaining in the active search. Active
/// nodes stay on `stack`, with `on_stack` providing constant-time membership
/// checks. When a node's low-link equals its own discovery index, that node is
/// the root of a complete component, so nodes are popped through that root and
/// emitted together. This finds every component in linear time relative to the
/// number of aggregate nodes and inline-containment edges.
fn strongly_connected_components(
    nodes: &[NodeId],
    edges: &HashMap<NodeId, Vec<(NodeId, LayoutField)>>,
) -> Vec<Vec<NodeId>> {
    struct Tarjan {
        next_index: usize,
        indices: HashMap<NodeId, usize>,
        low_links: HashMap<NodeId, usize>,
        stack: Vec<NodeId>,
        on_stack: HashSet<NodeId>,
        components: Vec<Vec<NodeId>>,
    }

    fn visit(
        node: NodeId,
        node_set: &HashSet<NodeId>,
        edges: &HashMap<NodeId, Vec<(NodeId, LayoutField)>>,
        state: &mut Tarjan,
    ) {
        let index = state.next_index;
        state.next_index += 1;
        state.indices.insert(node, index);
        state.low_links.insert(node, index);
        state.stack.push(node);
        state.on_stack.insert(node);

        if let Some(outgoing) = edges.get(&node) {
            for (target, _) in outgoing {
                if !node_set.contains(target) {
                    continue;
                }
                if !state.indices.contains_key(target) {
                    visit(*target, node_set, edges, state);
                    let target_low = state.low_links[target];
                    let node_low = state.low_links[&node].min(target_low);
                    state.low_links.insert(node, node_low);
                } else if state.on_stack.contains(target) {
                    let target_index = state.indices[target];
                    let node_low = state.low_links[&node].min(target_index);
                    state.low_links.insert(node, node_low);
                }
            }
        }

        if state.low_links[&node] != state.indices[&node] {
            return;
        }
        let mut component = Vec::new();
        loop {
            let member = state
                .stack
                .pop()
                .expect("strongly connected component root remains on the stack");
            state.on_stack.remove(&member);
            component.push(member);
            if member == node {
                break;
            }
        }
        state.components.push(component);
    }

    let node_set: HashSet<NodeId> = nodes.iter().copied().collect();
    let mut state = Tarjan {
        next_index: 0,
        indices: HashMap::new(),
        low_links: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        components: Vec::new(),
    };
    for node in nodes {
        if !state.indices.contains_key(node) {
            visit(*node, &node_set, edges, &mut state);
        }
    }
    state.components
}

fn collect_conditional_arms<'expression>(
    expression: &'expression Expression,
    arms: &mut Vec<(
        &'expression Expression,
        &'expression Expression,
        &'expression Block,
    )>,
) -> Option<&'expression Block> {
    let ExpressionKind::If {
        condition,
        then_branch,
        else_branch,
    } = &expression.kind
    else {
        unreachable!("conditional chains contain only if expressions")
    };
    arms.push((expression, condition, then_branch));
    match else_branch {
        Some(ConditionalElse::Block(block)) => Some(block),
        Some(ConditionalElse::If(conditional)) => collect_conditional_arms(conditional, arms),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ast::{FunctionParameter, StructDeclaration},
        context_resolution::resolve_program_context,
        lexer::Lexer,
        name_resolution::resolve_program,
        parser::{ParseContext, parse_program},
        signature_collection::collect_signatures,
        source::SourceModuleRegistry,
        type_resolution::resolve_types,
    };

    fn prepare(
        source: &str,
    ) -> (
        SourceModule,
        Program,
        NameResolution,
        ContextResolution,
        TypeResolution,
        SignatureCollection,
    ) {
        let mut registry = SourceModuleRegistry::new();
        let module = registry.add(source);
        let mut parse_context = ParseContext::new(module.module_id());
        let program = parse_program(&mut parse_context, Lexer::new(&module))
            .expect("test source should parse");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let context = resolve_program_context(&program).expect("test context should resolve");
        let mut types =
            resolve_types(&module, &program, &names).expect("test types should resolve");
        let signatures = collect_signatures(&module, &program, &names, &context, &mut types)
            .expect("test signatures should collect");
        (module, program, names, context, types, signatures)
    }

    fn check(
        module: &SourceModule,
        program: &Program,
        names: &NameResolution,
        context: &ContextResolution,
        types: &mut TypeResolution,
        signatures: &SignatureCollection,
    ) -> ExpressionChecking {
        Analyzer::new(module, names, context, signatures, types, program).check_program(program)
    }

    fn function(declaration: &Declaration) -> &Function {
        let Declaration::Function(function) = declaration else {
            panic!("expected function declaration")
        };
        function
    }

    fn structure(declaration: &Declaration) -> &StructDeclaration {
        let Declaration::Struct(structure) = declaration else {
            panic!("expected struct declaration")
        };
        structure
    }

    fn expression(statement: &Statement) -> &Expression {
        let StatementKind::Expression(expression) = &statement.kind else {
            panic!("expected expression statement")
        };
        expression
    }

    fn binding_initializer(statement: &Statement) -> &Expression {
        let StatementKind::Binding { initializer, .. } = &statement.kind else {
            panic!("expected binding statement")
        };
        initializer
    }

    fn conditional_branches(expression: &Expression) -> (&Block, &Block) {
        let ExpressionKind::If {
            then_branch,
            else_branch: Some(ConditionalElse::Block(else_branch)),
            ..
        } = &expression.kind
        else {
            panic!("expected conditional with a final else block")
        };
        (then_branch, else_branch)
    }

    fn call(expression: &Expression) -> (&Expression, &[Expression]) {
        let ExpressionKind::Call { callee, arguments } = &expression.kind else {
            panic!("expected call expression")
        };
        (callee, arguments)
    }

    fn garbage_collected(expression: &Expression) -> &Expression {
        let ExpressionKind::GarbageCollect(value) = &expression.kind else {
            panic!("expected garbage-collection expression")
        };
        value
    }

    fn lambda(expression: &Expression) -> (&[FunctionParameter], &Block) {
        let ExpressionKind::Lambda {
            parameters, body, ..
        } = &expression.kind
        else {
            panic!("expected lambda expression")
        };
        (parameters, body)
    }

    fn body_value(function: &Function) -> &Expression {
        function
            .body
            .value
            .as_deref()
            .expect("function should have a final value")
    }

    fn return_value(statement: &Statement) -> &Expression {
        let StatementKind::Return(Some(value)) = &statement.kind else {
            panic!("expected return statement with a value")
        };
        value
    }

    fn named_parameter(function: &Function, index: usize) -> &FunctionParameter {
        function
            .parameters
            .iter()
            .filter(|parameter| matches!(&parameter.kind, FunctionParameterKind::Named { .. }))
            .nth(index)
            .expect("named parameter should exist")
    }

    fn assert_primitive_expression(
        types: &TypeResolution,
        checking: &ExpressionChecking,
        expression: &Expression,
        primitive: PrimitiveType,
        capability: AccessCapability,
    ) {
        let typed = checking
            .expressions
            .get(&expression.id)
            .expect("expression should have semantic information");
        assert_eq!(typed.category, ValueCategory::FreshTemporary);
        assert!(matches!(
            types.types().get(typed.type_id),
            Some(SemanticType::Primitive {
                primitive: found_primitive,
                capability: found_capability,
            }) if *found_primitive == primitive && *found_capability == capability
        ));
    }

    #[test]
    fn checks_block_values_and_preserves_explicit_value_information() {
        let source = concat!(
            "struct Item {}\n",
            "fn inspect(item: Item) {\n",
            "    const borrowed = { item };\n",
            "    const explicit_unit = { () };\n",
            "    const implicit_unit = { (); };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let inspect = function(&program.declarations[1]);
        let borrowed = binding_initializer(&inspect.body.statements[0]);
        let explicit_unit = binding_initializer(&inspect.body.statements[1]);
        let implicit_unit = binding_initializer(&inspect.body.statements[2]);
        assert_eq!(
            checking.expressions[&borrowed.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(checking.transfers[&borrowed.id], ValueTransfer::Borrow);
        assert_eq!(checking.explicit_values[&borrowed.id], true);
        assert_eq!(checking.explicit_values[&explicit_unit.id], true);
        assert_eq!(checking.explicit_values[&implicit_unit.id], false);
        assert_primitive_expression(
            &types,
            &checking,
            explicit_unit,
            PrimitiveType::Unit,
            AccessCapability::Const,
        );
        assert_primitive_expression(
            &types,
            &checking,
            implicit_unit,
            PrimitiveType::Unit,
            AccessCapability::Const,
        );
    }

    #[test]
    fn checks_implicit_block_unit_against_its_expected_type() {
        let source = concat!(
            "fn wrong() -> int {\n",
            "    { (); }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let value = body_value(function(&program.declarations[0]));
        assert_eq!(checking.errors[0].span, value.span);
        assert_eq!(
            checking.expressions[&value.id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn distinguishes_statement_and_value_conditionals() {
        let source = concat!(
            "fn action() {}\n",
            "fn run(condition: bool) {\n",
            "    if condition { action(); }\n",
            "    const implicit = if condition { action(); } else { action(); };\n",
            "    const explicit = if condition { () } else { () };\n",
            "    const nested = if condition { if condition { action(); } } else { action(); };\n",
            "}\n",
            "fn final_statement(condition: bool) {\n",
            "    if condition { action(); }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let run = function(&program.declarations[1]);
        let statement = expression(&run.body.statements[0]);
        let implicit = binding_initializer(&run.body.statements[1]);
        let explicit = binding_initializer(&run.body.statements[2]);
        let nested = binding_initializer(&run.body.statements[3]);
        assert_eq!(checking.explicit_values[&statement.id], false);
        assert_eq!(checking.explicit_values[&implicit.id], false);
        assert_eq!(checking.explicit_values[&explicit.id], true);
        assert_eq!(checking.explicit_values[&nested.id], false);
        let final_statement = body_value(function(&program.declarations[2]));
        assert_eq!(checking.explicit_values[&final_statement.id], false);
        assert_primitive_expression(
            &types,
            &checking,
            final_statement,
            PrimitiveType::Unit,
            AccessCapability::Const,
        );
    }

    #[test]
    fn diagnoses_non_exhaustive_and_mixed_value_conditionals() {
        let source = concat!(
            "fn action() {}\n",
            "fn consume(value: ()) {}\n",
            "fn inspect(condition: bool) {\n",
            "    const missing = if condition { action(); };\n",
            "    if condition { () };\n",
            "    if condition { () } else { action(); };\n",
            "    consume(if condition { action(); });\n",
            "}\n",
            "fn missing_result(condition: bool) -> int {\n",
            "    if condition { return 1; }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let expected = [
            ExpressionCheckingErrorKind::ConditionalElseRequired,
            ExpressionCheckingErrorKind::ConditionalElseRequired,
            ExpressionCheckingErrorKind::ConditionalBranchValueRequired,
            ExpressionCheckingErrorKind::ConditionalElseRequired,
            ExpressionCheckingErrorKind::ConditionalElseRequired,
        ];
        assert_eq!(checking.errors.len(), expected.len());
        for (error, expected) in checking.errors.iter().zip(expected) {
            assert_eq!(error.kind, expected);
        }
    }

    #[test]
    fn checks_expected_union_conditionals_without_inferring_unions() {
        let source = concat!(
            "fn choose(condition: bool) {\n",
            "    const exact: int | float = if condition { 10 } else { 3.142 };\n",
            "    const inferred = if condition { 10 } else { 3.142 };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let choose = function(&program.declarations[0]);
        let exact = binding_initializer(&choose.body.statements[0]);
        let (then_branch, else_branch) = conditional_branches(exact);
        let then_value = then_branch
            .value
            .as_deref()
            .expect("then value should exist");
        let else_value = else_branch
            .value
            .as_deref()
            .expect("else value should exist");
        assert_eq!(checking.union_injections.len(), 2);
        assert_eq!(
            checking.union_injections[&then_value.id].union_type,
            checking.expressions[&exact.id].type_id
        );
        assert_eq!(
            checking.union_injections[&else_value.id].union_type,
            checking.expressions[&exact.id].type_id
        );
        assert_ne!(
            checking.union_injections[&then_value.id].member_type,
            checking.union_injections[&else_value.id].member_type
        );
        assert_eq!(checking.errors.len(), 1);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let inferred = binding_initializer(&choose.body.statements[1]);
        let (_, inferred_else) = conditional_branches(inferred);
        assert_eq!(
            checking.errors[0].span,
            inferred_else
                .value
                .as_deref()
                .expect("else value should exist")
                .span
        );
        assert_eq!(
            checking.expressions[&inferred.id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn injects_exact_union_members_without_reinjecting_union_values() {
        let source = concat!(
            "fn consume(value: int | float) {}\n",
            "fn main() {\n",
            "    const value: int | float = 1;\n",
            "    consume(value);\n",
            "    const invalid: int | float = true;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[1]);
        let injected = binding_initializer(&main.body.statements[0]);
        let called = expression(&main.body.statements[1]);
        let (_, arguments) = call(called);
        let invalid = binding_initializer(&main.body.statements[2]);
        assert_eq!(checking.union_injections.len(), 1);
        assert!(checking.union_injections.contains_key(&injected.id));
        assert!(!checking.union_injections.contains_key(&arguments[0].id));
        assert_eq!(checking.errors.len(), 1);
        assert_eq!(checking.errors[0].span, invalid.span);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
    }

    #[test]
    fn merges_conditional_categories_and_records_path_transfers() {
        let source = concat!(
            "fn inspect(condition: bool) {\n",
            "    const original = \"original\";\n",
            "    const mixed = if condition { original } else { \"fresh\" };\n",
            "    const fresh = if condition { \"left\" } else { \"right\" };\n",
            "    const left = &\"left\";\n",
            "    const right = &\"right\";\n",
            "    const shared = if condition { left } else { right };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let inspect = function(&program.declarations[0]);
        let mixed = binding_initializer(&inspect.body.statements[1]);
        let fresh = binding_initializer(&inspect.body.statements[2]);
        let shared = binding_initializer(&inspect.body.statements[5]);
        assert_eq!(
            checking.expressions[&mixed.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.expressions[&fresh.id].category,
            ValueCategory::FreshTemporary
        );
        assert_eq!(
            checking.expressions[&shared.id].category,
            ValueCategory::GarbageCollectedReference
        );
        let (mixed_then, mixed_else) = conditional_branches(mixed);
        let mixed_values = [
            mixed_then
                .value
                .as_deref()
                .expect("then value should exist"),
            mixed_else
                .value
                .as_deref()
                .expect("else value should exist"),
        ];
        assert_eq!(
            checking.transfers[&mixed_values[0].id],
            ValueTransfer::Borrow
        );
        assert_eq!(
            checking.transfers[&mixed_values[1].id],
            ValueTransfer::MoveTemporary
        );
        let (shared_then, shared_else) = conditional_branches(shared);
        let shared_values = [
            shared_then
                .value
                .as_deref()
                .expect("then value should exist"),
            shared_else
                .value
                .as_deref()
                .expect("else value should exist"),
        ];
        assert_eq!(shared_values.len(), 2);
        for value in shared_values {
            assert_eq!(
                checking.transfers[&value.id],
                ValueTransfer::ReuseGarbageCollected
            );
        }
    }

    #[test]
    fn propagates_conditional_divergence_through_else_if_chains() {
        let source = concat!(
            "fn choose(first: bool, second: bool) -> int {\n",
            "    if first { 1 } else if second { return 2; } else { 3 }\n",
            "}\n",
            "fn finish(condition: bool) -> int {\n",
            "    if condition { return 1; } else { panic(\"stop\"); }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let choose = body_value(function(&program.declarations[0]));
        let ExpressionKind::If {
            else_branch: Some(ConditionalElse::If(nested)),
            ..
        } = &choose.kind
        else {
            panic!("expected else-if chain")
        };
        assert_eq!(checking.explicit_values[&choose.id], true);
        assert_eq!(
            checking.expressions[&nested.id],
            checking.expressions[&choose.id]
        );
        assert_eq!(checking.transfers[&choose.id], ValueTransfer::TrivialCopy);
        let finish = body_value(function(&program.declarations[1]));
        assert_eq!(
            checking.expressions[&finish.id].type_id,
            types.types().divergence()
        );
        assert!(!checking.transfers.contains_key(&finish.id));
    }

    #[test]
    fn recovers_from_invalid_conditions_without_parent_diagnostics() {
        let source = concat!(
            "fn action() {}\n",
            "fn inspect() {\n",
            "    const invalid = 1 + if 1 { 2 } else { 3 };\n",
            "    if 9223372036854775808 { action(); }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        let inspect = function(&program.declarations[1]);
        let invalid = binding_initializer(&inspect.body.statements[0]);
        assert_eq!(
            checking.expressions[&invalid.id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn synthesizes_literal_types_and_categories() {
        let (module, program, names, context, mut types, signatures) =
            prepare("fn main() { (); 1; 1.0; true; 'a'; \"text\"; none; }");
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let expected = [
            (PrimitiveType::Unit, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Float, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Char, AccessCapability::Const),
            (PrimitiveType::String, AccessCapability::Mut),
            (PrimitiveType::None, AccessCapability::Const),
        ];
        for (statement, (primitive, capability)) in main.body.statements.iter().zip(expected) {
            let expression = expression(statement);
            assert_eq!(
                checking.expressions.get(&expression.id),
                Some(&TypedExpression {
                    type_id: types.types_mut().primitive(primitive, capability),
                    category: ValueCategory::FreshTemporary,
                })
            );
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn reports_an_out_of_range_integer_literal() {
        let (module, program, names, context, mut types, signatures) =
            prepare("fn main() { 9223372036854775808; }");
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        let main = function(&program.declarations[0]);
        assert_eq!(
            checking
                .expressions
                .get(&expression(&main.body.statements[0]).id)
                .map(|typed| typed.type_id),
            Some(types.types().recovery())
        );
    }

    #[test]
    fn resolves_forward_and_recursive_function_identifiers() {
        let source = concat!(
            "fn first() { second; first; } ",
            "fn second() {} ",
            "fn main() { first; }",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let first = function(&program.declarations[0]);
        for statement in &first.body.statements {
            let expression = expression(statement);
            let symbol = names
                .symbol_for_reference(expression.id)
                .expect("identifier should resolve");
            assert_eq!(
                checking.expressions[&expression.id].type_id,
                signatures
                    .callable_value_type(symbol)
                    .expect("function should have a callable value type")
            );
            assert_eq!(
                checking.expressions[&expression.id].category,
                ValueCategory::FreshTemporary
            );
        }
    }

    #[test]
    fn seeds_parameter_types_qualifiers_and_categories() {
        let source = concat!(
            "struct Item {} ",
            "fn inspect(value: int, item: Item, shared: &Item) { ",
            "value; item; shared; const alias = shared; ",
            "} ",
            "fn main() {}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let inspect = function(&program.declarations[1]);
        let expected_categories = [
            ValueCategory::OwnedInlinePlace,
            ValueCategory::BorrowedPlace,
            ValueCategory::GarbageCollectedReference,
        ];
        for (index, expected_category) in expected_categories.into_iter().enumerate() {
            let parameter = named_parameter(inspect, index);
            let symbol = names
                .symbol_for_declaration(parameter.id)
                .expect("parameter should have a symbol");
            let binding = checking.bindings[&symbol];
            assert_eq!(binding.qualifiers, parameter.qualifiers);
            assert_eq!(binding.category, expected_category);
            let reference = expression(&inspect.body.statements[index]);
            assert_eq!(checking.expressions[&reference.id].type_id, binding.type_id);
            assert_eq!(
                checking.expressions[&reference.id].category,
                expected_category
            );
        }
        let StatementKind::Binding {
            initializer: alias_initializer,
            ..
        } = &inspect.body.statements[3].kind
        else {
            panic!("expected GC alias binding")
        };
        assert_eq!(
            checking.transfers.get(&alias_initializer.id),
            Some(&ValueTransfer::ReuseGarbageCollected)
        );
    }

    #[test]
    fn types_plain_and_garbage_collected_self() {
        let source = concat!(
            "struct Item { ",
            "fn plain(mut self) { self; } ",
            "fn shared(&mut self) { self; } ",
            "} fn main() {}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let item = structure(&program.declarations[0]);
        let methods: Vec<_> = item
            .members
            .iter()
            .filter_map(|member| match member {
                StructMember::Function(function) => Some(function),
                StructMember::Field(_) => None,
            })
            .collect();
        let plain = expression(&methods[0].body.statements[0]);
        let shared = expression(&methods[1].body.statements[0]);
        assert_eq!(
            checking.expressions[&plain.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.expressions[&shared.id].category,
            ValueCategory::GarbageCollectedReference
        );
        assert!(matches!(
            types.types().get(checking.expressions[&shared.id].type_id),
            Some(SemanticType::GarbageCollected {
                capability: AccessCapability::Mut,
                ..
            })
        ));
    }

    #[test]
    fn records_binding_types_categories_and_transfers() {
        let source = concat!(
            "fn main() { ",
            "const first = \"first\"; ",
            "const second = first; ",
            "const number = 1; ",
            "mut copy = number; ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let expected = [
            (
                ValueCategory::OwnedInlinePlace,
                ValueTransfer::MoveTemporary,
            ),
            (ValueCategory::BorrowedPlace, ValueTransfer::Borrow),
            (ValueCategory::OwnedInlinePlace, ValueTransfer::TrivialCopy),
            (ValueCategory::OwnedInlinePlace, ValueTransfer::TrivialCopy),
        ];
        for (statement, (category, transfer)) in main.body.statements.iter().zip(expected) {
            let StatementKind::Binding { initializer, .. } = &statement.kind else {
                panic!("expected binding")
            };
            let symbol = names
                .symbol_for_declaration(statement.id)
                .expect("binding should have a symbol");
            assert_eq!(checking.bindings[&symbol].category, category);
            assert_eq!(checking.transfers.get(&initializer.id), Some(&transfer));
        }
        let copy_symbol = names
            .symbol_for_declaration(main.body.statements[3].id)
            .expect("copy should have a symbol");
        assert!(matches!(
            types.types().get(checking.bindings[&copy_symbol].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Mut,
            })
        ));
    }

    #[test]
    fn preserves_shadowing_order_for_binding_references() {
        let source = concat!(
            "fn main() { ",
            "const value = 1; ",
            "const value = value; ",
            "value; ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let StatementKind::Binding {
            initializer: shadowing_initializer,
            ..
        } = &main.body.statements[1].kind
        else {
            panic!("expected shadowing binding")
        };
        let final_reference = expression(&main.body.statements[2]);
        let first_symbol = names
            .symbol_for_declaration(main.body.statements[0].id)
            .expect("first binding should resolve");
        let second_symbol = names
            .symbol_for_declaration(main.body.statements[1].id)
            .expect("second binding should resolve");
        assert_eq!(
            names.symbol_for_reference(shadowing_initializer.id),
            Some(first_symbol)
        );
        assert_eq!(
            names.symbol_for_reference(final_reference.id),
            Some(second_symbol)
        );
        assert_eq!(
            checking.expressions[&shadowing_initializer.id].type_id,
            checking.bindings[&first_symbol].type_id
        );
        assert_eq!(
            checking.expressions[&final_reference.id].type_id,
            checking.bindings[&second_symbol].type_id
        );
    }

    #[test]
    fn reports_one_mismatch_and_recovers_without_cascading() {
        let source = "fn main() { const bad: float = 1; const next: int = bad; }";
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let main = function(&program.declarations[0]);
        for statement in &main.body.statements {
            let symbol = names
                .symbol_for_declaration(statement.id)
                .expect("binding should resolve");
            assert_eq!(checking.bindings[&symbol].type_id, types.types().recovery());
        }
    }

    #[test]
    fn accepts_an_exact_annotated_binding_type() {
        let source = "fn main() { const value: int = 1; }";
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let main = function(&program.declarations[0]);
        let symbol = names
            .symbol_for_declaration(main.body.statements[0].id)
            .expect("binding should resolve");
        assert!(matches!(
            types.types().get(checking.bindings[&symbol].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Const,
            })
        ));
    }

    #[test]
    fn groups_preserve_semantics_and_forward_expected_types() {
        let source = concat!(
            "fn main() { ",
            "const value = 1; ",
            "(value); ",
            "const bad: float = (1); ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let grouped = expression(&main.body.statements[1]);
        let ExpressionKind::Group(inner) = &grouped.kind else {
            panic!("expected grouped expression")
        };
        assert_eq!(
            checking.expressions[&grouped.id],
            checking.expressions[&inner.id]
        );
        assert_eq!(
            checking.expressions[&grouped.id].category,
            ValueCategory::OwnedInlinePlace
        );

        let StatementKind::Binding {
            initializer: bad_group,
            ..
        } = &main.body.statements[2].kind
        else {
            panic!("expected annotated binding")
        };
        let ExpressionKind::Group(bad_inner) = &bad_group.kind else {
            panic!("expected grouped initializer")
        };
        assert_eq!(checking.errors.len(), 1);
        assert_eq!(checking.errors[0].span, bad_inner.span);
        assert_eq!(
            checking.expressions[&bad_group.id].type_id,
            types.types().recovery()
        );
        assert_eq!(
            checking.expressions[&bad_inner.id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn checks_primitive_unary_operators() {
        let (module, program, names, context, mut types, signatures) =
            prepare("fn main() { -1; -1.0; !true; !1; -\"text\"; !1.0; }");
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let expected = [
            PrimitiveType::Int,
            PrimitiveType::Float,
            PrimitiveType::Bool,
            PrimitiveType::Int,
        ];
        for (statement, primitive) in main.body.statements.iter().zip(expected) {
            assert_primitive_expression(
                &types,
                &checking,
                expression(statement),
                primitive,
                AccessCapability::Const,
            );
        }
        assert_eq!(checking.errors.len(), 2);
        assert!(checking.errors.iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InvalidUnaryOperand { .. }
        )));
        for statement in &main.body.statements[4..] {
            assert_eq!(
                checking.expressions[&expression(statement).id].type_id,
                types.types().recovery()
            );
        }
    }

    #[test]
    fn checks_all_primitive_binary_operator_families() {
        let source = concat!(
            "fn main() { ",
            "1 + 2; 1.0 + 2.0; \"a\" + \"b\"; 1 - 2; 1.0 * 2.0; 1 / 2; ",
            "1 % 2; 1 << 2; 1 >> 2; 1 & 2; 1 ^ 2; 1 | 2; ",
            "1 < 2; 1.0 <= 2.0; 'a' > 'b'; 1 >= 2; ",
            "() == (); none != none; true == false; 'a' == 'b'; \"a\" != \"b\"; ",
            "true && false; false || true; ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let expected = [
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Float, AccessCapability::Const),
            (PrimitiveType::String, AccessCapability::Mut),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Float, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Int, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
            (PrimitiveType::Bool, AccessCapability::Const),
        ];
        assert_eq!(main.body.statements.len(), expected.len());
        for (statement, (primitive, capability)) in main.body.statements.iter().zip(expected) {
            assert_primitive_expression(
                &types,
                &checking,
                expression(statement),
                primitive,
                capability,
            );
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn diagnoses_binary_operands_without_cascading() {
        let source = "fn main() { true + false; 1 + 1.0; (1 + 1.0) + 2; }";
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert!(
            checking.errors[1..].iter().all(|error| matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ))
        );
        let main = function(&program.declarations[0]);
        assert_eq!(
            checking.expressions[&expression(&main.body.statements[2]).id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn checks_supported_primitive_conversions() {
        let source = concat!(
            "fn main() { ",
            "int(1.0); float(1); char(65); int(1); ",
            "const bad: float = int(1.0); string(1); ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        for (statement, primitive) in main.body.statements[..3].iter().zip([
            PrimitiveType::Int,
            PrimitiveType::Float,
            PrimitiveType::Char,
        ]) {
            assert_primitive_expression(
                &types,
                &checking,
                expression(statement),
                primitive,
                AccessCapability::Const,
            );
        }
        assert_eq!(checking.errors.len(), 2);
        assert!(
            checking.errors.iter().all(|error| matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ))
        );
        assert_eq!(
            checking.expressions[&expression(&main.body.statements[3]).id].type_id,
            types.types().recovery()
        );
        let unsupported = expression(&main.body.statements[5]);
        assert!(!checking.expressions.contains_key(&unsupported.id));
    }

    #[test]
    fn records_binding_transfers_from_primitive_expressions() {
        let (module, program, names, context, mut types, signatures) = prepare(concat!(
            "fn main() { ",
            "const prefix = \"a\"; ",
            "const sum = 1 + 2; ",
            "const text = prefix + \"b\"; ",
            "}",
        ));
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        for (statement, transfer) in main.body.statements.iter().zip([
            ValueTransfer::MoveTemporary,
            ValueTransfer::TrivialCopy,
            ValueTransfer::MoveTemporary,
        ]) {
            let StatementKind::Binding { initializer, .. } = &statement.kind else {
                panic!("expected binding")
            };
            assert_eq!(checking.transfers.get(&initializer.id), Some(&transfer));
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn checks_callable_completion_returns_and_sequential_fallthrough() {
        let source = concat!(
            "fn tail() -> int { 1 }\n",
            "fn explicit() -> int { return 1; }\n",
            "fn unit() {}\n",
            "fn bare() { return; }\n",
            "fn missing() -> int {}\n",
            "fn wrong_tail() -> int { false }\n",
            "fn wrong_return() -> int { return false; }\n",
            "fn unexpected() { return 1; }\n",
            "fn recovered() -> int { return 9223372036854775808; }\n",
            "fn unreachable() -> int { return 1; false + true; false }\n",
            "fn divergent() -> int { panic(\"stop\") }\n",
            "fn missing_bare() -> int { return; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let missing = function(&program.declarations[4]);
        let wrong_tail = body_value(function(&program.declarations[5]));
        let wrong_return = return_value(&function(&program.declarations[6]).body.statements[0]);
        let unexpected = return_value(&function(&program.declarations[7]).body.statements[0]);
        let recovered = return_value(&function(&program.declarations[8]).body.statements[0]);
        let unreachable = function(&program.declarations[9]);
        let unreachable_error = expression(&unreachable.body.statements[1]);
        let ExpressionKind::Binary {
            left: unreachable_error_left,
            ..
        } = &unreachable_error.kind
        else {
            panic!("expected invalid binary expression")
        };

        let missing_bare = function(&program.declarations[11]);
        assert_eq!(checking.errors.len(), 7);
        assert_eq!(checking.errors[0].span, missing.body.span);
        for (error, value) in [
            (&checking.errors[1], wrong_tail),
            (&checking.errors[2], wrong_return),
            (&checking.errors[3], unexpected),
        ] {
            assert_eq!(error.span, value.span);
            assert!(matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ));
        }
        assert_eq!(checking.errors[4].span, recovered.span);
        assert_eq!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        assert_eq!(checking.errors[5].span, unreachable_error_left.span);
        assert!(matches!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand { .. }
        ));
        assert_eq!(
            checking.errors[6].span,
            missing_bare.body.statements[0].span
        );
        assert!(matches!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));

        let tail = body_value(function(&program.declarations[0]));
        let explicit = return_value(&function(&program.declarations[1]).body.statements[0]);
        let unreachable_return = return_value(&unreachable.body.statements[0]);
        for value in [tail, explicit, unreachable_return] {
            assert_eq!(
                checking.transfers.get(&value.id),
                Some(&ValueTransfer::TrivialCopy)
            );
        }
        let unreachable_tail = body_value(unreachable);
        assert!(checking.expressions.contains_key(&unreachable_tail.id));
        assert!(!checking.transfers.contains_key(&unreachable_tail.id));
        let divergent = body_value(function(&program.declarations[10]));
        assert_eq!(
            checking.expressions[&divergent.id].type_id,
            types.types().divergence()
        );
        assert!(!checking.transfers.contains_key(&divergent.id));
    }

    #[test]
    fn records_value_semantic_return_transfers() {
        let source = concat!(
            "struct Item {}\n",
            "fn make() -> Item { Item {} }\n",
            "fn primitive(value: int) -> int { value }\n",
            "fn fresh() -> string { \"fresh\" }\n",
            "fn copied(value: Item) -> Item { value }\n",
            "fn copied_local() -> Item { const local = make(); local }\n",
            "fn allocated() -> &Item { &make() }\n",
            "fn helper() {}\n",
            "fn callable() -> fn() -> () { helper }\n",
            "fn callable_local() -> fn() -> () { const value = helper; value }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let returned = [
            (
                body_value(function(&program.declarations[2])),
                ValueTransfer::TrivialCopy,
            ),
            (
                body_value(function(&program.declarations[3])),
                ValueTransfer::MoveTemporary,
            ),
            (
                body_value(function(&program.declarations[4])),
                ValueTransfer::RecursiveCopy,
            ),
            (
                body_value(function(&program.declarations[5])),
                ValueTransfer::RecursiveCopy,
            ),
            (
                body_value(function(&program.declarations[6])),
                ValueTransfer::ReuseGarbageCollected,
            ),
            (
                body_value(function(&program.declarations[8])),
                ValueTransfer::MoveTemporary,
            ),
            (
                body_value(function(&program.declarations[9])),
                ValueTransfer::MoveTemporary,
            ),
        ];
        assert_eq!(returned.len(), 7);
        for (value, transfer) in returned {
            assert_eq!(checking.transfers.get(&value.id), Some(&transfer));
        }
        let allocated = body_value(function(&program.declarations[6]));
        let allocation_source = garbage_collected(allocated);
        assert_eq!(
            checking.transfers.get(&allocation_source.id),
            Some(&ValueTransfer::AllocateGarbageCollected)
        );
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn rejects_non_escaping_erased_return_sources() {
        let source = concat!(
            "interface Reader { fn read(self); }\n",
            "interface Writer { fn write(self); }\n",
            "fn return_interface(value: Reader) -> Reader { value }\n",
            "fn return_intersection(value: Reader & Writer) -> Reader & Writer { value }\n",
            "fn return_callable(value: fn() -> ()) -> fn() -> () { value }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let returned = [
            body_value(function(&program.declarations[2])),
            body_value(function(&program.declarations[3])),
            body_value(function(&program.declarations[4])),
        ];
        assert_eq!(checking.errors.len(), returned.len());
        for (error, value) in checking.errors.iter().zip(returned) {
            assert_eq!(error.span, value.span);
            assert!(matches!(
                error.kind,
                ExpressionCheckingErrorKind::InvalidReturnSource {
                    category: ValueCategory::BorrowedPlace,
                    ..
                }
            ));
            assert_eq!(
                checking.expressions[&value.id].type_id,
                types.types().recovery()
            );
            assert!(!checking.transfers.contains_key(&value.id));
        }
    }

    #[test]
    fn checks_nested_function_and_named_method_results() {
        let source = concat!(
            "struct Item {\n",
            "    fn duplicate(self) -> Item { return self; }\n",
            "}\n",
            "fn outer() -> int {\n",
            "    fn nested(value: int) -> int { value }\n",
            "    nested(1)\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let StructMember::Function(method) = &structure(&program.declarations[0]).members[0] else {
            panic!("expected method")
        };
        let outer = function(&program.declarations[1]);
        let StatementKind::Function(nested) = &outer.body.statements[0].kind else {
            panic!("expected nested function")
        };
        let returned = [
            (
                return_value(&method.body.statements[0]),
                ValueTransfer::RecursiveCopy,
            ),
            (body_value(nested), ValueTransfer::TrivialCopy),
            (body_value(outer), ValueTransfer::TrivialCopy),
        ];
        assert_eq!(returned.len(), 3);
        for (value, transfer) in returned {
            assert_eq!(checking.transfers.get(&value.id), Some(&transfer));
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn synthesizes_calls_through_ordinary_callable_values() {
        let source = concat!(
            "fn first(value: int) -> bool { second(value); true }\n",
            "fn second(value: int) -> bool { second(value); true }\n",
            "fn invoke(operation: fn(int) -> bool, value: int) {\n",
            "    operation(value);\n",
            "    const alias = first;\n",
            "    alias(value);\n",
            "    println(\"ok\");\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let first = function(&program.declarations[0]);
        let second = function(&program.declarations[1]);
        let invoke = function(&program.declarations[2]);
        for called in [
            expression(&first.body.statements[0]),
            expression(&second.body.statements[0]),
            expression(&invoke.body.statements[0]),
            expression(&invoke.body.statements[2]),
        ] {
            assert_primitive_expression(
                &types,
                &checking,
                called,
                PrimitiveType::Bool,
                AccessCapability::Const,
            );
        }
        assert_primitive_expression(
            &types,
            &checking,
            expression(&invoke.body.statements[3]),
            PrimitiveType::Unit,
            AccessCapability::Const,
        );
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn assigns_call_result_categories_from_return_storage() {
        let source = concat!(
            "struct User {}\n",
            "fn count() -> int { 0 }\n",
            "fn user() -> User { User {} }\n",
            "fn shared() -> &User { &User {} }\n",
            "fn main() { count(); user(); shared(); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[4]);
        for (statement, declaration, category) in [
            (&main.body.statements[0], 1, ValueCategory::FreshTemporary),
            (&main.body.statements[1], 2, ValueCategory::FreshTemporary),
            (
                &main.body.statements[2],
                3,
                ValueCategory::GarbageCollectedReference,
            ),
        ] {
            let called = expression(statement);
            let signature = signatures
                .callable(function(&program.declarations[declaration]).id)
                .expect("called function should have a signature");
            assert_eq!(
                checking.expressions[&called.id].type_id,
                signature.return_type
            );
            assert_eq!(checking.expressions[&called.id].category, category);
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn records_parameter_transfers_for_successful_arguments() {
        let source = concat!(
            "struct User {}\n",
            "fn consume(count: int, user: User, text: string, shared: &User) {}\n",
            "fn inspect(count: int, user: User, shared: &User) {\n",
            "    consume(count, user, \"a\" + \"b\", shared);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let inspect = function(&program.declarations[2]);
        let (_, arguments) = call(expression(&inspect.body.statements[0]));
        assert_eq!(arguments.len(), 4);
        for (argument, transfer) in arguments.iter().zip([
            ValueTransfer::TrivialCopy,
            ValueTransfer::Borrow,
            ValueTransfer::Borrow,
            ValueTransfer::ReuseGarbageCollected,
        ]) {
            assert_eq!(checking.transfers.get(&argument.id), Some(&transfer));
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn allocates_fresh_values_and_reuses_gc_references() {
        let source = concat!(
            "struct Item {}\n",
            "fn make() -> Item { Item {} }\n",
            "fn shared() -> &Item { &Item {} }\n",
            "fn main() {\n",
            "    const number = &1;\n",
            "    const text = &\"text\";\n",
            "    const item = &make();\n",
            "    const first = shared();\n",
            "    const again = &first;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[3]);
        assert_eq!(main.body.statements.len(), 5);

        let mut initializers = Vec::new();
        for statement in &main.body.statements {
            let StatementKind::Binding { initializer, .. } = &statement.kind else {
                panic!("expected binding")
            };
            let symbol = names
                .symbol_for_declaration(statement.id)
                .expect("binding should have a symbol");
            assert_eq!(
                checking.bindings[&symbol].category,
                ValueCategory::GarbageCollectedReference
            );
            initializers.push(initializer);
        }
        for initializer in &initializers {
            assert_eq!(
                checking.expressions[&initializer.id].category,
                ValueCategory::GarbageCollectedReference
            );
        }

        let number_value = garbage_collected(initializers[0]);
        let number_type = checking.expressions[&initializers[0].id].type_id;
        let number_target = types
            .types()
            .garbage_collected_target(number_type)
            .expect("allocated integer should have a GC target");
        assert!(matches!(
            types.types().get(number_target),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Const,
            })
        ));

        let text_value = garbage_collected(initializers[1]);
        let text_type = checking.expressions[&initializers[1].id].type_id;
        let text_target = types
            .types()
            .garbage_collected_target(text_type)
            .expect("allocated string should have a GC target");
        assert!(matches!(
            types.types().get(text_target),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                capability: AccessCapability::Mut,
            })
        ));

        let item_value = garbage_collected(initializers[2]);
        let item_type = checking.expressions[&initializers[2].id].type_id;
        assert_eq!(
            types.types().garbage_collected_target(item_type),
            Some(
                signatures
                    .callable(function(&program.declarations[1]).id)
                    .expect("make should have a signature")
                    .return_type
            )
        );

        let again_value = garbage_collected(initializers[4]);
        assert_eq!(
            checking.expressions[&initializers[4].id].type_id,
            checking.expressions[&initializers[3].id].type_id
        );
        for value in [number_value, text_value, item_value] {
            assert_eq!(
                checking.transfers.get(&value.id),
                Some(&ValueTransfer::AllocateGarbageCollected)
            );
        }
        assert_eq!(
            checking.transfers.get(&again_value.id),
            Some(&ValueTransfer::ReuseGarbageCollected)
        );
        for initializer in initializers {
            assert_eq!(
                checking.transfers.get(&initializer.id),
                Some(&ValueTransfer::ReuseGarbageCollected)
            );
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn rejects_plain_places_as_gc_allocation_sources_without_cascades() {
        let source = concat!(
            "struct Item {}\n",
            "fn make() -> Item { Item {} }\n",
            "fn inspect(parameter: Item) {\n",
            "    const local = make();\n",
            "    const recovered: bool = &local;\n",
            "    &parameter;\n",
            "    &9223372036854775808;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let inspect = function(&program.declarations[2]);
        assert_eq!(inspect.body.statements.len(), 4);
        let StatementKind::Binding {
            initializer: recovered,
            ..
        } = &inspect.body.statements[1].kind
        else {
            panic!("expected recovered binding")
        };
        let local = garbage_collected(recovered);
        let borrowed = expression(&inspect.body.statements[2]);
        let parameter = garbage_collected(borrowed);
        let overflow = expression(&inspect.body.statements[3]);
        let overflow_value = garbage_collected(overflow);

        assert_eq!(checking.errors.len(), 3);
        assert_eq!(checking.errors[0].span, local.span);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidGarbageCollectionSource {
                category: ValueCategory::OwnedInlinePlace,
                ..
            }
        ));
        assert_eq!(checking.errors[1].span, parameter.span);
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::InvalidGarbageCollectionSource {
                category: ValueCategory::BorrowedPlace,
                ..
            }
        ));
        assert_eq!(checking.errors[2].span, overflow_value.span);
        assert_eq!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        for allocated in [recovered, borrowed, overflow] {
            assert_eq!(
                checking.expressions[&allocated.id].type_id,
                types.types().recovery()
            );
        }
        assert!(checking.transfers.get(&local.id).is_none());
        assert!(checking.transfers.get(&parameter.id).is_none());
        assert!(checking.transfers.get(&overflow_value.id).is_none());
    }

    #[test]
    fn diagnoses_invalid_calls_and_recovers_without_parent_errors() {
        let source = concat!(
            "fn target(left: int, right: float) -> int { 0 }\n",
            "fn main() {\n",
            "    const recovered: bool = target(true, 9223372036854775808);\n",
            "    1(9223372036854775808);\n",
            "    target(1);\n",
            "    target(1, 2.0, 3);\n",
            "    9223372036854775808(1, false);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[1]);
        let StatementKind::Binding { initializer, .. } = &main.body.statements[0].kind else {
            panic!("expected recovered binding")
        };
        let (_, mismatched_arguments) = call(initializer);
        assert_eq!(checking.errors[0].span, mismatched_arguments[0].span);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(checking.errors[1].span, mismatched_arguments[1].span);
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );

        let non_callable = expression(&main.body.statements[1]);
        let (non_callable_callee, non_callable_arguments) = call(non_callable);
        assert_eq!(checking.errors[2].span, non_callable_callee.span);
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::NotCallable { .. }
        ));
        assert_eq!(checking.errors[3].span, non_callable_arguments[0].span);
        assert_eq!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );

        for (error, expected, found) in [(&checking.errors[4], 2, 1), (&checking.errors[5], 2, 3)] {
            assert_eq!(
                error.kind,
                ExpressionCheckingErrorKind::ArgumentCountMismatch { expected, found }
            );
        }

        let recovered_callee_call = expression(&main.body.statements[4]);
        let (_, recovered_callee_arguments) = call(recovered_callee_call);
        assert_eq!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        assert_eq!(checking.errors.len(), 7);
        for called in [
            initializer,
            non_callable,
            expression(&main.body.statements[2]),
            expression(&main.body.statements[3]),
            recovered_callee_call,
        ] {
            assert_eq!(
                checking.expressions[&called.id].type_id,
                types.types().recovery()
            );
        }
        assert!(
            recovered_callee_arguments
                .iter()
                .all(|argument| { checking.expressions.contains_key(&argument.id) })
        );
        assert!(!checking.transfers.contains_key(&mismatched_arguments[0].id));
        let (_, surplus_arguments) = call(expression(&main.body.statements[3]));
        assert!(!checking.transfers.contains_key(&surplus_arguments[2].id));
    }

    #[test]
    fn synthesizes_lambda_signatures_parameters_calls_and_transfers() {
        let source = concat!(
            "fn main() {\n",
            "    const offset = 1;\n",
            "    const add = lambda(value: int) -> int { value + offset };\n",
            "    const called = add(2);\n",
            "    const immediate = lambda(value: int) -> int { value }(3);\n",
            "    const heap = &lambda(value: int) -> int { value };\n",
            "    const heap_called = heap(4);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let main = function(&program.declarations[0]);
        let add = binding_initializer(&main.body.statements[1]);
        let (parameters, add_body) = lambda(add);
        let add_type = checking.expressions[&add.id];
        assert_eq!(add_type.category, ValueCategory::FreshTemporary);
        assert!(matches!(
            types.types().get(add_type.type_id),
            Some(SemanticType::Callable {
                parameters,
                capability: AccessCapability::Const,
                ..
            }) if parameters.len() == 1
        ));
        assert_eq!(checking.lambda_captures[&add.id].len(), 1);
        let offset_symbol = names
            .symbol_for_declaration(main.body.statements[0].id)
            .expect("offset should have a symbol");
        assert_eq!(
            checking.lambda_captures[&add.id][0].source,
            LambdaCaptureSource::Symbol(offset_symbol)
        );
        let parameter_symbol = names
            .symbol_for_declaration(parameters[0].id)
            .expect("lambda parameter should have a symbol");
        assert_eq!(
            checking.bindings[&parameter_symbol].category,
            ValueCategory::OwnedInlinePlace
        );
        let ExpressionKind::Binary { left, .. } = &add_body
            .value
            .as_deref()
            .expect("lambda should have a result")
            .kind
        else {
            panic!("expected binary lambda result")
        };
        assert_eq!(
            checking.expressions[&left.id].type_id,
            checking.bindings[&parameter_symbol].type_id
        );
        assert_eq!(checking.transfers[&add.id], ValueTransfer::MoveTemporary);

        let immediate = binding_initializer(&main.body.statements[3]);
        let (immediate_lambda, _) = call(immediate);
        assert!(checking.lambda_captures[&immediate_lambda.id].is_empty());
        let heap = binding_initializer(&main.body.statements[4]);
        let heap_lambda = garbage_collected(heap);
        assert_eq!(
            checking.transfers[&heap_lambda.id],
            ValueTransfer::AllocateGarbageCollected
        );
        let heap_called = binding_initializer(&main.body.statements[5]);
        let (_, arguments) = call(heap_called);
        assert_primitive_expression(
            &types,
            &checking,
            heap_called,
            PrimitiveType::Int,
            AccessCapability::Const,
        );
        assert_eq!(
            checking.transfers[&arguments[0].id],
            ValueTransfer::TrivialCopy
        );
    }

    #[test]
    fn infers_mutable_lambda_capability_and_enforces_its_direction() {
        let source = concat!(
            "fn accepts_const(callback: fn() -> int) {}\n",
            "fn accepts_mut(const vmut callback: fn() -> int) {}\n",
            "fn invalid_return(mut value: int) -> fn() -> int {\n",
            "    lambda() -> int { value }\n",
            "}\n",
            "fn main() {\n",
            "    mut vconst count = 0;\n",
            "    const vmut shared = 0;\n",
            "    const invalid = lambda() -> int { count };\n",
            "    const vmut valid = lambda() -> int { count };\n",
            "    const vmut valid_shared = lambda() -> int { shared };\n",
            "    accepts_mut(lambda() -> int { 1 });\n",
            "    accepts_const(lambda() -> int { count });\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3);
        assert!(
            checking.errors.iter().all(|error| matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ))
        );
        let invalid_return = body_value(function(&program.declarations[2]));
        assert_eq!(checking.errors[0].span, invalid_return.span);
        let main = function(&program.declarations[3]);
        let invalid = binding_initializer(&main.body.statements[2]);
        let valid = binding_initializer(&main.body.statements[3]);
        let valid_shared = binding_initializer(&main.body.statements[4]);
        assert_eq!(checking.errors[1].span, invalid.span);
        assert_eq!(
            checking.expressions[&invalid.id].type_id,
            types.types().recovery()
        );
        for closure in [valid, valid_shared] {
            assert!(matches!(
                types.types().get(checking.expressions[&closure.id].type_id),
                Some(SemanticType::Callable {
                    capability: AccessCapability::Mut,
                    ..
                })
            ));
        }
        let rejected_call = expression(&main.body.statements[6]);
        let (_, rejected_arguments) = call(rejected_call);
        assert_eq!(checking.errors[2].span, rejected_arguments[0].span);
        assert_eq!(
            checking.expressions[&rejected_call.id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn discovers_deduplicated_shadowed_and_transitive_lambda_captures() {
        let source = concat!(
            "fn inspect(value: int) {\n",
            "    mut changing = 1;\n",
            "    const duplicate = lambda() -> int { value; value };\n",
            "    const shadowed = lambda(value: int) -> int { value };\n",
            "    const vmut outer = lambda() {\n",
            "        const vmut inner = lambda() -> int { changing };\n",
            "    };\n",
            "    const boundary = lambda() {\n",
            "        fn nested() { value; }\n",
            "    };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let inspect = function(&program.declarations[0]);
        let duplicate = binding_initializer(&inspect.body.statements[1]);
        let shadowed = binding_initializer(&inspect.body.statements[2]);
        let outer = binding_initializer(&inspect.body.statements[3]);
        let (_, outer_body) = lambda(outer);
        let inner = binding_initializer(&outer_body.statements[0]);
        let boundary = binding_initializer(&inspect.body.statements[4]);
        assert_eq!(checking.lambda_captures[&duplicate.id].len(), 1);
        assert!(checking.lambda_captures[&shadowed.id].is_empty());
        assert_eq!(checking.lambda_captures[&outer.id].len(), 1);
        assert_eq!(checking.lambda_captures[&inner.id].len(), 1);
        assert!(checking.lambda_captures[&boundary.id].is_empty());
        for closure in [outer, inner] {
            assert!(matches!(
                types.types().get(checking.expressions[&closure.id].type_id),
                Some(SemanticType::Callable {
                    capability: AccessCapability::Mut,
                    ..
                })
            ));
        }
    }

    #[test]
    fn derives_lambda_capability_from_captured_self_qualifiers() {
        let source = concat!(
            "struct Item {\n",
            "    fn readonly(self) { const closure = lambda() { self; }; }\n",
            "    fn writable(mut self) { const vmut closure = lambda() { self; }; }\n",
            "    fn shared(&mut self) { const vmut closure = lambda() { self; }; }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let item = structure(&program.declarations[0]);
        let methods: Vec<_> = item
            .members
            .iter()
            .filter_map(|member| match member {
                StructMember::Function(function) => Some(function),
                StructMember::Field(_) => None,
            })
            .collect();
        for (method, expected) in methods.into_iter().zip([
            AccessCapability::Const,
            AccessCapability::Mut,
            AccessCapability::Mut,
        ]) {
            let closure = binding_initializer(&method.body.statements[0]);
            assert_eq!(checking.lambda_captures[&closure.id].len(), 1);
            assert_eq!(
                checking.lambda_captures[&closure.id][0].source,
                LambdaCaptureSource::SelfValue { method: method.id }
            );
            assert!(matches!(
                types.types().get(checking.expressions[&closure.id].type_id),
                Some(SemanticType::Callable { capability, .. }) if *capability == expected
            ));
        }
    }

    #[test]
    fn recovers_lambda_body_errors_without_parent_diagnostics_or_transfers() {
        let source = concat!(
            "fn main() {\n",
            "    const returned = lambda(value: int) -> int { return value; };\n",
            "    const missing = lambda() -> int {};\n",
            "    const invalid: fn() -> int = lambda() -> int { true };\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let main = function(&program.declarations[0]);
        let returned = binding_initializer(&main.body.statements[0]);
        let missing = binding_initializer(&main.body.statements[1]);
        let invalid = binding_initializer(&main.body.statements[2]);
        assert_eq!(
            checking.transfers[&returned.id],
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.expressions[&missing.id].type_id,
            types.types().recovery()
        );
        assert_eq!(
            checking.expressions[&invalid.id].type_id,
            types.types().recovery()
        );
        assert!(!checking.transfers.contains_key(&missing.id));
        assert!(!checking.transfers.contains_key(&invalid.id));
    }

    #[test]
    fn records_binding_parameter_and_self_places() {
        let source = concat!(
            "struct Item { fn inspect(mut self) { self; } }\n",
            "fn named() {}\n",
            "fn roots(mut vconst item: Item) { const local = item; item; local; named; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let item = structure(&program.declarations[0]);
        let StructMember::Function(method) = &item.members[0] else {
            panic!("expected method")
        };
        let self_value = expression(&method.body.statements[0]);
        let roots = function(&program.declarations[2]);
        let parameter = expression(&roots.body.statements[1]);
        let local = expression(&roots.body.statements[2]);
        let named = expression(&roots.body.statements[3]);
        let parameter_place = checking.places[&parameter.id];
        assert_eq!(
            parameter_place.binding_mutability,
            Some(BindingMutability::Mut)
        );
        assert_eq!(parameter_place.value_capability, ValueCapability::Const);
        assert_eq!(parameter_place.category, ValueCategory::BorrowedPlace);
        let local_place = checking.places[&local.id];
        assert_eq!(
            local_place.binding_mutability,
            Some(BindingMutability::Const)
        );
        assert_eq!(local_place.value_capability, ValueCapability::Const);
        assert_eq!(local_place.category, ValueCategory::BorrowedPlace);
        let self_place = checking.places[&self_value.id];
        assert_eq!(self_place.symbol, None);
        assert_eq!(self_place.binding_mutability, None);
        assert_eq!(self_place.value_capability, ValueCapability::Mut);
        assert!(!checking.places.contains_key(&named.id));
    }

    #[test]
    fn rebinds_plain_roots_and_moves_fresh_call_results() {
        let source = concat!(
            "struct Item {}\n",
            "fn produce(item: Item) -> Item { item }\n",
            "fn inspect(mut vconst current: Item, other: Item) {\n",
            "    current = other;\n",
            "    current;\n",
            "    current = produce(other);\n",
            "    current;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let inspect = function(&program.declarations[2]);
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::Assignment {
                value: borrowed, ..
            },
            ..
        }) = &inspect.body.statements[0].kind
        else {
            panic!("expected plain assignment")
        };
        let after_borrow = expression(&inspect.body.statements[1]);
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::Assignment { value: fresh, .. },
            ..
        }) = &inspect.body.statements[2].kind
        else {
            panic!("expected fresh assignment")
        };
        let after_fresh = expression(&inspect.body.statements[3]);
        assert_eq!(checking.transfers[&borrowed.id], ValueTransfer::Borrow);
        assert_eq!(checking.transfers[&fresh.id], ValueTransfer::MoveTemporary);
        assert_eq!(
            checking.places[&after_borrow.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.places[&after_fresh.id].category,
            ValueCategory::OwnedInlinePlace
        );
        let symbol = names
            .symbol_for_declaration(named_parameter(inspect, 0).id)
            .expect("parameter should have a symbol");
        assert!(checking.reassigned_bindings.contains(&symbol));
    }

    #[test]
    fn merges_plain_root_provenance_after_conditional_rebinding() {
        let source = concat!(
            "struct Item {}\n",
            "fn produce(item: Item) -> Item { item }\n",
            "fn choose(mut vconst current: Item, other: Item, condition: bool) {\n",
            "    if condition { current = produce(other); } else { current = other; };\n",
            "    current;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let choose = function(&program.declarations[2]);
        let current = expression(&choose.body.statements[1]);
        assert_eq!(
            checking.places[&current.id].category,
            ValueCategory::BorrowedPlace
        );
    }

    #[test]
    fn checks_root_compound_assignment_mutability_and_operands() {
        let source = concat!(
            "fn inspect() {\n",
            "    mut vconst number = 1; number += 2; number <<= 1;\n",
            "    const vmut text = \"a\"; text += \"b\";\n",
            "    mut vconst readonly_text = \"a\"; readonly_text += \"b\";\n",
            "    const fixed = 1; fixed += 2;\n",
            "    mut flag = true; flag += false;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ImmutableValue
        );
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::ImmutableBinding
        );
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::InvalidAssignmentOperand {
                operator: AssignmentOperator::Add,
                ..
            }
        ));
        let inspect = function(&program.declarations[0]);
        for index in [1, 2, 4] {
            let assignment = expression(&inspect.body.statements[index]);
            assert_primitive_expression(
                &types,
                &checking,
                assignment,
                PrimitiveType::Unit,
                AccessCapability::Const,
            );
        }
    }

    #[test]
    fn rejects_fixed_and_non_place_identifier_assignment_targets() {
        let source = concat!(
            "struct Item {}\n",
            "fn named() {}\n",
            "fn inspect(item: Item, other: Item) { item = other; named = named; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ImmutableBinding
        );
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::InvalidAssignmentTarget
        );
    }

    #[test]
    fn enforces_view_capabilities_but_allows_recursive_return_copies() {
        let source = concat!(
            "struct Item {}\n",
            "fn mutate(const vmut item: Item) {}\n",
            "fn copied(item: Item) -> mut Item { item }\n",
            "fn inspect(item: Item) { mutate(item); }\n",
            "fn redirect(mut vconst current: &Item, other: &Item) { current = other; }\n",
            "fn reject_redirect(mut current: &mut Item, other: &Item) { current = other; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2);
        assert!(
            checking.errors.iter().all(|error| matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ))
        );
        let copied = function(&program.declarations[2]);
        let returned = body_value(copied);
        assert_eq!(
            checking.transfers[&returned.id],
            ValueTransfer::RecursiveCopy
        );
        let redirect = function(&program.declarations[4]);
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::Assignment { value, .. },
            ..
        }) = &redirect.body.statements[0].kind
        else {
            panic!("expected GC assignment")
        };
        assert_eq!(
            checking.transfers[&value.id],
            ValueTransfer::ReuseGarbageCollected
        );
    }

    #[test]
    fn checks_named_construction_fields_associated_functions_and_methods() {
        let source = concat!(
            "fn forward() -> Container { Container::new(0) }\n",
            "struct Leaf { value: int, }\n",
            "struct Container {\n",
            "    total: int,\n",
            "    leaf: Leaf,\n",
            "    fn new(value: int) -> Container {\n",
            "        Container { leaf: Leaf { value: value }, total: value }\n",
            "    }\n",
            "    fn read(self) -> int { self.total }\n",
            "    fn add(mut self, amount: int) { self.total += amount; }\n",
            "    fn heap_read(&self) -> int { self.total }\n",
            "}\n",
            "fn inspect(const vmut container: Container, other: Leaf, shared: &Container) {\n",
            "    const constructed = Container { total: 1, leaf: Leaf { value: 2 } };\n",
            "    const associated = Container::new;\n",
            "    const from_associated = associated(3);\n",
            "    const total = constructed.total;\n",
            "    container.total = 4;\n",
            "    container.leaf = Leaf { value: 5 };\n",
            "    container.add(6);\n",
            "    const borrowed_read = shared.read();\n",
            "    const gc_read = shared.heap_read();\n",
            "    const copied = other.copy();\n",
            "    container.leaf = other.copy();\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[3]);
        let constructed = binding_initializer(&inspect.body.statements[0]);
        let ExpressionKind::StructConstruction { fields, .. } = &constructed.kind else {
            panic!("expected named construction")
        };
        assert_eq!(
            checking.transfers[&fields[0].value.id],
            ValueTransfer::TrivialCopy
        );
        assert_eq!(
            checking.transfers[&fields[1].value.id],
            ValueTransfer::MoveTemporary
        );

        let associated = binding_initializer(&inspect.body.statements[1]);
        assert!(matches!(
            checking.resolved_members[&associated.id],
            ResolvedMember::AssociatedFunction { .. }
        ));

        let total = binding_initializer(&inspect.body.statements[3]);
        assert!(matches!(
            checking.resolved_members[&total.id],
            ResolvedMember::Field { .. }
        ));
        assert_eq!(
            checking.places[&total.id].category,
            ValueCategory::OwnedInlinePlace
        );

        let add = expression(&inspect.body.statements[6]);
        let (add_callee, _) = call(add);
        let ExpressionKind::MemberAccess {
            object: add_object, ..
        } = &add_callee.kind
        else {
            panic!("expected method member")
        };
        assert!(matches!(
            checking.resolved_members[&add_callee.id],
            ResolvedMember::Method { .. }
        ));
        assert_eq!(checking.transfers[&add_object.id], ValueTransfer::Borrow);

        let borrowed_read = binding_initializer(&inspect.body.statements[7]);
        let (borrowed_callee, _) = call(borrowed_read);
        let ExpressionKind::MemberAccess {
            object: borrowed_object,
            ..
        } = &borrowed_callee.kind
        else {
            panic!("expected method member")
        };
        assert_eq!(
            checking.transfers[&borrowed_object.id],
            ValueTransfer::Borrow
        );

        let gc_read = binding_initializer(&inspect.body.statements[8]);
        let (gc_callee, _) = call(gc_read);
        let ExpressionKind::MemberAccess {
            object: gc_object, ..
        } = &gc_callee.kind
        else {
            panic!("expected method member")
        };
        assert_eq!(
            checking.transfers[&gc_object.id],
            ValueTransfer::ReuseGarbageCollected
        );

        let copied = binding_initializer(&inspect.body.statements[9]);
        let (copy_callee, _) = call(copied);
        let ExpressionKind::MemberAccess {
            object: copy_source,
            ..
        } = &copy_callee.kind
        else {
            panic!("expected copy member")
        };
        assert!(matches!(
            checking.resolved_members[&copy_callee.id],
            ResolvedMember::Copy { .. }
        ));
        assert_eq!(
            checking.transfers[&copy_source.id],
            ValueTransfer::RecursiveCopy
        );

        let fresh_assignment = expression(&inspect.body.statements[5]);
        let ExpressionKind::Assignment { value, .. } = &fresh_assignment.kind else {
            panic!("expected field assignment")
        };
        assert_eq!(checking.transfers[&value.id], ValueTransfer::MoveTemporary);
    }

    #[test]
    fn reports_named_member_and_owning_field_errors_without_cascades() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Container {\n",
            "    leaf: Leaf,\n",
            "    count: int,\n",
            "    fn make() -> Leaf { Leaf {} }\n",
            "    fn read(self) -> int { self.count }\n",
            "}\n",
            "fn bad(mut container: Container, const vmut leaf: Leaf) {\n",
            "    const broken = Container { leaf: Leaf {}, leaf: Leaf {}, unknown: 1 };\n",
            "    const borrowed = Container { leaf: leaf, count: 0 };\n",
            "    container.leaf = leaf;\n",
            "    const selected = container.read;\n",
            "    leaf.copy;\n",
            "    leaf.copy(1);\n",
            "    container.make();\n",
            "    Container::leaf;\n",
            "    Container::read;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 11, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::DuplicateConstructionField
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::UnknownConstructionField
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::MissingConstructionField { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::InvalidOwningSource { .. }
        ));
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::InvalidOwningSource { .. }
        ));
        assert_eq!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::MethodRequiresCall
        );
        assert_eq!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::CopyRequiresCall
        );
        assert_eq!(
            checking.errors[7].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 0,
                found: 1,
            }
        );
        assert_eq!(
            checking.errors[8].kind,
            ExpressionCheckingErrorKind::AssociatedFunctionRequiresType
        );
        assert_eq!(
            checking.errors[9].kind,
            ExpressionCheckingErrorKind::FieldRequiresValue
        );
        assert_eq!(
            checking.errors[10].kind,
            ExpressionCheckingErrorKind::MethodRequiresValue
        );
    }

    #[test]
    fn reuses_gc_references_stored_in_named_fields() {
        let source = concat!(
            "struct Item {}\n",
            "struct Holder { item: &Item, }\n",
            "fn inspect(item: &Item) {\n",
            "    const holder = Holder { item: item };\n",
            "    const read = holder.item;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty());
        let inspect = function(&program.declarations[2]);
        let holder = binding_initializer(&inspect.body.statements[0]);
        let ExpressionKind::StructConstruction { fields, .. } = &holder.kind else {
            panic!("expected holder construction")
        };
        assert_eq!(
            checking.transfers[&fields[0].value.id],
            ValueTransfer::ReuseGarbageCollected
        );
        let read = binding_initializer(&inspect.body.statements[1]);
        assert_eq!(
            checking.expressions[&read.id].category,
            ValueCategory::GarbageCollectedReference
        );
    }

    #[test]
    fn checks_method_receiver_storage_and_capability() {
        let source = concat!(
            "struct Item {\n",
            "    value: int,\n",
            "    fn mutate(mut self) {}\n",
            "    fn retained(&self) {}\n",
            "}\n",
            "fn bad(value: Item) {\n",
            "    value.value = 1;\n",
            "    value.mutate();\n",
            "    value.retained();\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ImmutableValue
        );
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::ReceiverCapabilityMismatch
        );
        assert_eq!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::ReceiverStorageMismatch
        );
    }

    #[test]
    fn checks_method_arguments_after_receiver_selection() {
        let source = concat!(
            "struct Item { fn take(self, value: int) {} }\n",
            "fn bad(item: Item) {\n",
            "    item.take();\n",
            "    item.take(1.0);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        );
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
    }

    #[test]
    fn checks_anonymous_fields_methods_and_copy() {
        let source = concat!(
            "fn main() {\n",
            "    const seed = 1;\n",
            "    const vmut object = struct {\n",
            "        count = seed;\n",
            "        label: string = \"item\";\n",
            "        fn read(self) -> int { self.count }\n",
            "        fn captured(self) -> int { seed }\n",
            "        fn shadow(self, seed: int) -> int { seed }\n",
            "        fn add(mut self, amount: int) -> int {\n",
            "            self.count += amount;\n",
            "            self.count\n",
            "        }\n",
            "    };\n",
            "    object.count = 2;\n",
            "    object.add(3);\n",
            "    const copied = object.copy();\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[0]);
        let anonymous = binding_initializer(&main.body.statements[1]);
        let ExpressionKind::AnonymousStruct { members } = &anonymous.kind else {
            panic!("expected anonymous struct initializer")
        };
        let AnonymousStructMember::Field(count) = &members[0] else {
            panic!("expected inferred anonymous field")
        };
        let AnonymousStructMember::Field(label) = &members[1] else {
            panic!("expected annotated anonymous field")
        };
        assert!(matches!(
            types.types().get(checking.anonymous_field_types[&count.id]),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));
        assert!(matches!(
            types.types().get(checking.anonymous_field_types[&label.id]),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                ..
            })
        ));
        assert_eq!(
            checking.transfers[&count.initializer.id],
            ValueTransfer::TrivialCopy
        );
        assert_eq!(
            checking.transfers[&label.initializer.id],
            ValueTransfer::MoveTemporary
        );
        let copied = binding_initializer(&main.body.statements[4]);
        let (copy_callee, _) = call(copied);
        let ExpressionKind::MemberAccess { object, .. } = &copy_callee.kind else {
            panic!("expected anonymous copy member")
        };
        assert_eq!(checking.transfers[&object.id], ValueTransfer::RecursiveCopy);
    }

    #[test]
    fn converts_named_and_anonymous_structs_and_dispatches_interfaces() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface Accumulator { fn add(mut self, amount: int) -> int; }\n",
            "interface Empty {}\n",
            "struct Named { value: int, fn read(self) -> int { self.value } }\n",
            "fn consume(value: Reader) -> int { value.read() }\n",
            "fn main() {\n",
            "    const named = Named { value: 1 };\n",
            "    const named_reader: Reader = named;\n",
            "    const empty: Empty = named;\n",
            "    const fresh_reader: Reader = Named { value: 4 };\n",
            "    const vmut implementation = struct {\n",
            "        value = 2;\n",
            "        fn read(self) -> int { self.value }\n",
            "        fn add(mut self, amount: int) -> int { self.value + amount }\n",
            "    };\n",
            "    const reader: Reader = implementation;\n",
            "    const vmut both: Reader & Accumulator = implementation;\n",
            "    consume(implementation);\n",
            "    reader.read();\n",
            "    both.add(3);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[5]);
        let named_conversion = binding_initializer(&main.body.statements[1]);
        let empty_conversion = binding_initializer(&main.body.statements[2]);
        let fresh_conversion = binding_initializer(&main.body.statements[3]);
        let reader_conversion = binding_initializer(&main.body.statements[5]);
        let intersection_conversion = binding_initializer(&main.body.statements[6]);
        for converted in [
            named_conversion,
            empty_conversion,
            reader_conversion,
            intersection_conversion,
        ] {
            assert_eq!(
                checking.expressions[&converted.id].category,
                ValueCategory::BorrowedPlace
            );
            assert_eq!(
                checking.interface_conversions[&converted.id].backing_transfer,
                ValueTransfer::Borrow
            );
        }
        assert_eq!(
            checking.interface_conversions[&fresh_conversion.id].backing_transfer,
            ValueTransfer::MoveTemporary
        );
        assert!(
            checking.interface_conversions[&empty_conversion.id]
                .methods
                .is_empty()
        );
        let read_call = expression(&main.body.statements[8]);
        let (read_callee, _) = call(read_call);
        assert!(matches!(
            checking.resolved_members[&read_callee.id],
            ResolvedMember::InterfaceMethod { .. }
        ));
        let add_call = expression(&main.body.statements[9]);
        let (add_callee, _) = call(add_call);
        assert!(matches!(
            checking.resolved_members[&add_callee.id],
            ResolvedMember::InterfaceMethod { .. }
        ));
    }

    #[test]
    fn checks_gc_interface_backing_and_structural_failures() {
        let source = concat!(
            "interface Need { fn run(self, value: int) -> int; }\n",
            "interface Keep { fn get(&self) -> int; }\n",
            "interface First { fn same(self) -> int; }\n",
            "interface Second { fn same(mut self) -> int; }\n",
            "struct Wrong { fn run(self, value: float) -> int { 0 } }\n",
            "fn main() {\n",
            "    const wrong = Wrong {};\n",
            "    const incompatible: Need = wrong;\n",
            "    const missing: Need = struct {};\n",
            "    const conflict: First & Second = struct {};\n",
            "    const inline_keep: Keep = struct { fn get(&self) -> int { 1 } };\n",
            "    const correct = struct { fn run(self, value: int) -> int { value } };\n",
            "    const vmut escalation: Need = correct;\n",
            "    const vmut fresh_need: Need = struct {\n",
            "        fn run(self, value: int) -> int { value }\n",
            "    };\n",
            "    const heap_keep: &Keep = &struct { fn get(&self) -> int { 2 } };\n",
            "    const borrowed_keep: Keep = &struct { fn get(&self) -> int { 3 } };\n",
            "    heap_keep.get();\n",
            "    borrowed_keep.get();\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::IncompatibleInterfaceMethod { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::MissingInterfaceMethod { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::ConflictingInterfaceRequirement { .. }
        ));
        assert_eq!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::InterfaceRequiresGarbageCollectedSource
        );
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let main = function(&program.declarations[5]);
        let heap = binding_initializer(&main.body.statements[8]);
        let borrowed = binding_initializer(&main.body.statements[9]);
        assert_eq!(
            checking.expressions[&heap.id].category,
            ValueCategory::GarbageCollectedReference
        );
        assert_eq!(
            checking.expressions[&borrowed.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.interface_conversions[&borrowed.id].backing_transfer,
            ValueTransfer::Borrow
        );
    }

    #[test]
    fn rejects_only_unbounded_inline_aggregate_cycles() {
        let source = concat!(
            "struct Direct { next: Direct, }\n",
            "struct Left { right: Right | none, }\n",
            "struct Right { left: Left, }\n",
            "struct Safe { next: &Safe | none, items: Vector<Safe>, }\n",
            "struct Wrapped { failure: Error<Wrapped>, }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InfiniteInlineLayout { .. }
        )));
    }
}
