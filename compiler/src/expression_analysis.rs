//! Internal foundation for core expression type checking.
//!
//! This module intentionally has no public whole-program entry point yet. It
//! records the leaf-expression and ordinary-binding facts needed by later
//! increments without treating not-yet-implemented expression forms as source
//! errors.

// The whole-program entry point remains intentionally private and unconnected
// until the core-expression phase covers every expression form in its scope.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::{
    ast::{
        BinaryOperator, BindingQualifiers, Declaration, Expression, ExpressionKind, Function,
        FunctionParameterKind, LiteralKind, NodeId, PrimitiveType, Program, ReceiverStorage,
        Statement, StatementKind, StructMember, UnaryOperator, ValueCapability,
    },
    context_resolution::ContextResolution,
    name_resolution::NameResolution,
    semantic_types::{
        AccessCapability, CopySemantics, SemanticType, StorageSemantics, TypeId, ValueCategory,
        ValueTransfer,
    },
    signature_collection::SignatureCollection,
    source::{SourceModule, Span},
    symbol_table::SymbolId,
    type_resolution::TypeResolution,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypedExpression {
    type_id: TypeId,
    category: ValueCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindingSemantics {
    type_id: TypeId,
    qualifiers: BindingQualifiers,
    category: ValueCategory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpressionCheckingError {
    kind: ExpressionCheckingErrorKind,
    span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionCheckingErrorKind {
    IntegerLiteralOutOfRange,
    TypeMismatch { expected: TypeId, found: TypeId },
    InvalidUnaryOperand {
        operator: UnaryOperator,
        found: TypeId,
    },
    InvalidBinaryOperand {
        operator: BinaryOperator,
        found: TypeId,
    },
    NotCallable { found: TypeId },
    ArgumentCountMismatch { expected: usize, found: usize },
}

#[derive(Debug, Default)]
struct ExpressionChecking {
    expressions: HashMap<NodeId, TypedExpression>,
    bindings: HashMap<SymbolId, BindingSemantics>,
    transfers: HashMap<NodeId, ValueTransfer>,
    errors: Vec<ExpressionCheckingError>,
}

struct Analyzer<'semantic> {
    module: &'semantic SourceModule,
    names: &'semantic NameResolution,
    context: &'semantic ContextResolution,
    signatures: &'semantic SignatureCollection,
    types: &'semantic mut TypeResolution,
    method_owners: HashMap<NodeId, TypeId>,
    checking: ExpressionChecking,
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
        let mut method_owners = HashMap::new();
        for declaration in &program.declarations {
            let Declaration::Struct(structure) = declaration else {
                continue;
            };
            let owner = signatures
                .named_struct(structure.id)
                .expect("named struct signature must have been collected")
                .type_id;
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
            checking: ExpressionChecking::default(),
        }
    }

    fn check_program(mut self, program: &Program) -> ExpressionChecking {
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }
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
        self.seed_parameters(function);
        self.visit_block(&function.body);
    }

    /// Makes a function's named parameters available while checking its body.
    ///
    /// Each parameter's collected semantic type, source qualifiers, and value
    /// category are recorded against its resolved symbol. Receivers are not
    /// included because `self` is typed separately from receiver metadata.
    fn seed_parameters(&mut self, function: &Function) {
        let signature = self
            .signatures
            .callable(function.id)
            .expect("function signature must have been collected");
        let mut semantic_parameters = signature.parameters.clone().into_iter();

        for parameter in &function.parameters {
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
        }
        assert!(
            semantic_parameters.next().is_none(),
            "collected signature has more semantic parameters than the AST"
        );
    }

    fn visit_block(&mut self, block: &crate::ast::Block) {
        for statement in &block.statements {
            self.visit_statement(statement);
        }
        if let Some(value) = &block.value {
            self.synthesize(value);
        }
    }

    fn visit_statement(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::Binding {
                qualifiers,
                type_annotation,
                initializer,
                ..
            } => self.analyze_binding(
                statement,
                *qualifiers,
                type_annotation.as_ref().map(|syntax| syntax.id),
                initializer,
            ),
            StatementKind::Expression(expression) => {
                self.synthesize(expression);
            }
            StatementKind::Function(function) => self.visit_function(function),
            StatementKind::Defer(_)
            | StatementKind::Coroutine(_)
            | StatementKind::Break(_)
            | StatementKind::Continue
            | StatementKind::Return(_) => {}
        }
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
    ) {
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
        let Some(source) = source else {
            return;
        };

        let stored_type = if self.is_recovery(source.type_id) {
            source.type_id
        } else {
            expected.unwrap_or_else(|| {
                self.with_value_capability(source.type_id, qualifiers.value)
            })
        };
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
    }

    fn synthesize(&mut self, expression: &Expression) -> Option<TypedExpression> {
        if let Some(typed) = self.checking.expressions.get(&expression.id).copied() {
            return Some(typed);
        }

        let typed = match &expression.kind {
            ExpressionKind::Literal(literal) => self.synthesize_literal(expression, *literal),
            ExpressionKind::Identifier => self.synthesize_identifier(expression)?,
            ExpressionKind::SelfValue => self.synthesize_self(expression),
            ExpressionKind::Group(inner) => self.synthesize(inner)?,
            ExpressionKind::PrimitiveConversion { target, value } => {
                self.synthesize_primitive_conversion(*target, value)?
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
            _ => return None,
        };
        self.checking.expressions.insert(expression.id, typed);
        Some(typed)
    }

    fn check(&mut self, expression: &Expression, expected: TypeId) -> Option<TypedExpression> {
        if let ExpressionKind::Group(inner) = &expression.kind
            && !self.checking.expressions.contains_key(&expression.id)
        {
            let typed = self.check(inner, expected)?;
            self.checking.expressions.insert(expression.id, typed);
            return Some(typed);
        }

        let found = self.synthesize(expression)?;
        self.check_typed(expression, expected, found)
    }

    fn check_typed(
        &mut self,
        expression: &Expression,
        expected: TypeId,
        found: TypedExpression,
    ) -> Option<TypedExpression> {
        if self.is_recovery(expected) || self.is_recovery(found.type_id) {
            return Some(found);
        }
        if self
            .types
            .types()
            .has_same_shape(found.type_id, expected)
            .expect("checked types must belong to the program type store")
        {
            return Some(found);
        }

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
        Some(recovered)
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
            (UnaryOperator::Negate, Some(PrimitiveType::Int | PrimitiveType::Float))
                | (UnaryOperator::Not, Some(PrimitiveType::Bool | PrimitiveType::Int))
        );
        if valid {
            return Some(self.fresh_primitive(
                primitive.expect("valid unary operand must be primitive"),
            ));
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
            BinaryOperator::Subtract
            | BinaryOperator::Multiply
            | BinaryOperator::Divide => match left_primitive {
                Some(PrimitiveType::Int | PrimitiveType::Float) => left_primitive,
                _ => None,
            },
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

        let typed_right = self.check(right, typed_left.type_id)?;
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

    fn synthesize_call(
        &mut self,
        expression: &Expression,
        callee: &Expression,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let typed_callee = self.synthesize(callee)?;
        if self.is_recovery(typed_callee.type_id) {
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        }

        let Some(SemanticType::Callable {
            parameters,
            return_type,
            ..
        }) = self.types.types().get(typed_callee.type_id).cloned()
        else {
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

        let arity_matches = parameters.len() == arguments.len();
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: parameters.len(),
                    found: arguments.len(),
                },
                span: expression.span,
            });
        }

        let mut arguments_valid = true;
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
                arguments_valid = false;
                continue;
            }
            if let Some(transfer) = self.argument_transfer(checked) {
                self.checking.transfers.insert(argument.id, transfer);
            }
        }

        if !all_supported {
            return None;
        }
        if !arity_matches || !arguments_valid || self.is_recovery(return_type) {
            return Some(self.recovery_temporary());
        }

        let category = if self
            .types
            .types()
            .get(return_type)
            .is_some_and(|semantic| {
                semantic.storage_semantics() == Some(StorageSemantics::GarbageCollected)
            })
        {
            ValueCategory::GarbageCollectedReference
        } else {
            ValueCategory::FreshTemporary
        };
        Some(TypedExpression {
            type_id: return_type,
            category,
        })
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

    fn synthesize_identifier(&self, expression: &Expression) -> Option<TypedExpression> {
        let symbol = self
            .names
            .symbol_for_reference(expression.id)
            .expect("identifier must have a resolved semantic symbol");
        if let Some(binding) = self.checking.bindings.get(&symbol) {
            return Some(TypedExpression {
                type_id: binding.type_id,
                category: binding.category,
            });
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
        match receiver.storage {
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
        }
    }

    fn parameter_category(&self, type_id: TypeId) -> ValueCategory {
        let semantic = self
            .types
            .types()
            .get(type_id)
            .expect("parameter type belongs to the program type store");
        match semantic.storage_semantics() {
            Some(StorageSemantics::GarbageCollected) => {
                ValueCategory::GarbageCollectedReference
            }
            _ if semantic.copy_semantics() == Some(CopySemantics::Trivial) => {
                ValueCategory::OwnedInlinePlace
            }
            _ => ValueCategory::BorrowedPlace,
        }
    }

    fn binding_transfer(
        &self,
        source: TypedExpression,
    ) -> (ValueCategory, Option<ValueTransfer>) {
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
        let context =
            resolve_program_context(&program).expect("test context should resolve");
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

    fn call(expression: &Expression) -> (&Expression, &[Expression]) {
        let ExpressionKind::Call { callee, arguments } = &expression.kind else {
            panic!("expected call expression")
        };
        (callee, arguments)
    }

    fn named_parameter(function: &Function, index: usize) -> &FunctionParameter {
        function
            .parameters
            .iter()
            .filter(|parameter| {
                matches!(&parameter.kind, FunctionParameterKind::Named { .. })
            })
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
    fn synthesizes_literal_types_and_categories() {
        let (module, program, names, context, mut types, signatures) = prepare(
            "fn main() { (); 1; 1.0; true; 'a'; \"text\"; none; }",
        );
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        for (statement, (primitive, capability)) in
            main.body.statements.iter().zip(expected)
        {
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
            assert_eq!(checking.expressions[&reference.id].category, expected_category);
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
        let main = function(&program.declarations[0]);
        let expected = [
            (ValueCategory::OwnedInlinePlace, ValueTransfer::MoveTemporary),
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
        let main = function(&program.declarations[0]);
        let grouped = expression(&main.body.statements[1]);
        let ExpressionKind::Group(inner) = &grouped.kind else {
            panic!("expected grouped expression")
        };
        assert_eq!(checking.expressions[&grouped.id], checking.expressions[&inner.id]);
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
        let (module, program, names, context, mut types, signatures) = prepare(
            "fn main() { -1; -1.0; !true; !1; -\"text\"; !1.0; }",
        );
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        for (statement, (primitive, capability)) in
            main.body.statements.iter().zip(expected)
        {
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
        assert_eq!(checking.errors.len(), 3);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand {
                operator: BinaryOperator::Add,
                ..
            }
        ));
        assert!(checking.errors[1..].iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        )));
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
        let main = function(&program.declarations[0]);
        for (statement, primitive) in main.body.statements[..3]
            .iter()
            .zip([PrimitiveType::Int, PrimitiveType::Float, PrimitiveType::Char])
        {
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
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        )));
        assert_eq!(
            checking.expressions[&expression(&main.body.statements[3]).id].type_id,
            types.types().recovery()
        );
        let unsupported = expression(&main.body.statements[5]);
        assert!(!checking.expressions.contains_key(&unsupported.id));
    }

    #[test]
    fn records_binding_transfers_from_primitive_expressions() {
        let (module, program, names, context, mut types, signatures) = prepare(
            concat!(
                "fn main() { ",
                "const prefix = \"a\"; ",
                "const sum = 1 + 2; ",
                "const text = prefix + \"b\"; ",
                "}",
            ),
        );
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
            "fn count() -> int {}\n",
            "fn user() -> User {}\n",
            "fn shared() -> &User {}\n",
            "fn main() { count(); user(); shared(); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
            assert_eq!(checking.expressions[&called.id].type_id, signature.return_type);
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
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
    fn diagnoses_invalid_calls_and_recovers_without_parent_errors() {
        let source = concat!(
            "fn target(left: int, right: float) -> int {}\n",
            "fn main() {\n",
            "    const recovered: bool = target(true, 9223372036854775808);\n",
            "    1(9223372036854775808);\n",
            "    target(1);\n",
            "    target(1, 2.0, 3);\n",
            "    9223372036854775808(1, false);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(
            &module,
            &program,
            &names,
            &context,
            &mut types,
            &signatures,
        );
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
        assert!(recovered_callee_arguments.iter().all(|argument| {
            checking.expressions.contains_key(&argument.id)
        }));
        assert!(!checking
            .transfers
            .contains_key(&mismatched_arguments[0].id));
        let (_, surplus_arguments) = call(expression(&main.body.statements[3]));
        assert!(!checking.transfers.contains_key(&surplus_arguments[2].id));
    }
}
