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
        BindingQualifiers, Declaration, Expression, ExpressionKind, Function,
        FunctionParameterKind, LiteralKind, NodeId, PrimitiveType, Program, ReceiverStorage,
        Statement, StatementKind, StructMember, ValueCapability,
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
            _ => return None,
        };
        self.checking.expressions.insert(expression.id, typed);
        Some(typed)
    }

    fn check(&mut self, expression: &Expression, expected: TypeId) -> Option<TypedExpression> {
        let found = self.synthesize(expression)?;
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
}
