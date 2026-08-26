//! Resolves source names to stable semantic symbol identities.
//!
//! This pass builds the program's value and type scopes, collects declarations
//! needed for forward references, and connects lexical name uses to their
//! declarations while reporting unknown or duplicate names. Member lookup,
//! `self` and control-flow context, type resolution, and capture restrictions
//! are handled by later semantic passes.

use std::{collections::HashMap, fmt};

use crate::{
    ast::{
        AnonymousStructMember, Block, ConditionalElse, Declaration, Expression, ExpressionKind,
        Function, FunctionParameter, FunctionParameterKind, InterfaceMethodRequirement, NodeId,
        Program, Statement, StatementKind, StructMember, TypeKind, TypeSyntax,
    },
    source::{ModuleId, SourceModule, Span},
    symbol_table::{
        DeclareError, Namespace, ScopeCreationError, ScopeId, SymbolId, SymbolKind,
        SymbolLookupError, SymbolTable,
    },
};

const BUILTIN_VALUES: &[&str] = &["ascii", "panic", "print", "println", "yield"];

#[derive(Debug)]
pub struct NameResolution {
    symbols: SymbolTable,
    program_scope: ScopeId,
    declarations: HashMap<NodeId, SymbolId>,
    references: HashMap<NodeId, SymbolId>,
}

impl NameResolution {
    #[must_use]
    pub const fn program_scope(&self) -> ScopeId {
        self.program_scope
    }

    #[must_use]
    pub const fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    #[must_use]
    pub fn symbol_for_declaration(&self, id: NodeId) -> Option<SymbolId> {
        self.declarations.get(&id).copied()
    }

    #[must_use]
    pub fn symbol_for_reference(&self, id: NodeId) -> Option<SymbolId> {
        self.references.get(&id).copied()
    }

    #[must_use]
    pub const fn declarations(&self) -> &HashMap<NodeId, SymbolId> {
        &self.declarations
    }

    #[must_use]
    pub const fn references(&self) -> &HashMap<NodeId, SymbolId> {
        &self.references
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameResolutionError {
    pub kind: NameResolutionErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameResolutionErrorKind {
    UnknownName { namespace: Namespace, name: String },
    DuplicateDeclaration { name: String, other: Span },
    MissingMain,
}

impl fmt::Display for NameResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            NameResolutionErrorKind::UnknownName { namespace, name } => write!(
                formatter,
                "unknown {namespace:?} name `{name}` at {}..{}",
                self.span.start, self.span.end
            ),
            NameResolutionErrorKind::DuplicateDeclaration { name, other } => write!(
                formatter,
                "duplicate declaration of `{name}` at {}..{}; conflicting declaration at {}..{}",
                self.span.start, self.span.end, other.start, other.end
            ),
            NameResolutionErrorKind::MissingMain => {
                formatter.write_str("program does not declare a top-level `main` function")
            }
        }
    }
}

impl std::error::Error for NameResolutionError {}

pub type NameResolutionResult = Result<NameResolution, Vec<NameResolutionError>>;

/// Resolves every lexical value and type name in one parsed source program.
///
/// Member names and `self` are deliberately excluded: their validity depends
/// on contextual or type information. Named-function capture restrictions are
/// likewise checked by the later capture-analysis pass after ordinary lexical
/// resolution has identified the nearest declaration.
pub fn resolve_program(module: &SourceModule, program: &Program) -> NameResolutionResult {
    assert_eq!(
        module.module_id(),
        program.id.module_id,
        "program must be resolved with its source module"
    );
    Resolver::new(module).resolve(program)
}

struct Resolver<'source> {
    module: &'source SourceModule,
    symbols: SymbolTable,
    program_scope: ScopeId,
    declarations: HashMap<NodeId, SymbolId>,
    references: HashMap<NodeId, SymbolId>,
    errors: Vec<NameResolutionError>,
    top_level_main_count: usize,
}

impl<'source> Resolver<'source> {
    fn new(module: &'source SourceModule) -> Self {
        let mut symbols = SymbolTable::new();
        let prelude_scope = symbols.root_scope();

        for name in BUILTIN_VALUES {
            symbols
                .declare(
                    prelude_scope,
                    name,
                    SymbolKind::BuiltinValue,
                    Span::new(ModuleId::PRELUDE, 0, 0),
                )
                .expect("built-in names are unique and the prelude scope exists");
        }

        let program_scope = symbols
            .new_child_scope(prelude_scope)
            .expect("the prelude scope exists");

        Self {
            module,
            symbols,
            program_scope,
            declarations: HashMap::new(),
            references: HashMap::new(),
            errors: Vec::new(),
            top_level_main_count: 0,
        }
    }

    fn resolve(mut self, program: &Program) -> NameResolutionResult {
        self.collect_top_level_declarations(program);

        if self.top_level_main_count == 0 {
            self.errors.push(NameResolutionError {
                kind: NameResolutionErrorKind::MissingMain,
                span: Span::new(program.span.module_id, program.span.end, program.span.end),
            });
        }

        for declaration in &program.declarations {
            self.resolve_declaration(declaration);
        }

        if self.errors.is_empty() {
            Ok(NameResolution {
                symbols: self.symbols,
                program_scope: self.program_scope,
                declarations: self.declarations,
                references: self.references,
            })
        } else {
            Err(self.errors)
        }
    }

    fn collect_top_level_declarations(&mut self, program: &Program) {
        for declaration in &program.declarations {
            match declaration {
                Declaration::Function(function) => {
                    if self.text(function.name) == "main" {
                        self.top_level_main_count += 1;
                    }
                    self.declare(
                        self.program_scope,
                        function.id,
                        function.name,
                        SymbolKind::Function,
                    );
                }
                Declaration::Struct(structure) => {
                    self.declare(
                        self.program_scope,
                        structure.id,
                        structure.name,
                        SymbolKind::Struct,
                    );
                }
                Declaration::Interface(interface) => {
                    self.declare(
                        self.program_scope,
                        interface.id,
                        interface.name,
                        SymbolKind::Interface,
                    );
                }
            }
        }
    }

    fn resolve_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function) => {
                self.resolve_function(function, self.program_scope);
            }
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    match member {
                        StructMember::Field(field) => {
                            self.resolve_type(self.program_scope, &field.type_annotation);
                        }
                        StructMember::Function(function) => {
                            self.resolve_function(function, self.program_scope);
                        }
                    }
                }
            }
            Declaration::Interface(interface) => {
                for requirement in &interface.requirements {
                    self.resolve_interface_requirement(requirement, self.program_scope);
                }
            }
        }
    }

    fn resolve_interface_requirement(
        &mut self,
        requirement: &InterfaceMethodRequirement,
        enclosing_scope: ScopeId,
    ) {
        self.resolve_parameter_types(enclosing_scope, &requirement.parameters);
        if let Some(return_type) = &requirement.return_type {
            self.resolve_type(enclosing_scope, return_type);
        }

        let parameter_scope = self.new_child_scope(enclosing_scope);
        self.declare_parameters(parameter_scope, &requirement.parameters);
    }

    fn resolve_function(&mut self, function: &Function, enclosing_scope: ScopeId) {
        self.resolve_parameter_types(enclosing_scope, &function.parameters);
        if let Some(return_type) = &function.return_type {
            self.resolve_type(enclosing_scope, return_type);
        }

        let body_scope = self.new_child_scope(enclosing_scope);
        self.declare_parameters(body_scope, &function.parameters);
        self.resolve_block_contents(body_scope, &function.body);
    }

    fn resolve_parameter_types(&mut self, scope: ScopeId, parameters: &[FunctionParameter]) {
        for parameter in parameters {
            if let FunctionParameterKind::Named {
                type_annotation, ..
            } = &parameter.kind
            {
                self.resolve_type(scope, type_annotation);
            }
        }
    }

    fn declare_parameters(&mut self, scope: ScopeId, parameters: &[FunctionParameter]) {
        for parameter in parameters {
            if let FunctionParameterKind::Named { name, .. } = &parameter.kind {
                self.declare(scope, parameter.id, *name, SymbolKind::Parameter);
            }
        }
    }

    fn resolve_block(&mut self, enclosing_scope: ScopeId, block: &Block) {
        let scope = self.new_child_scope(enclosing_scope);
        self.resolve_block_contents(scope, block);
    }

    fn resolve_block_contents(&mut self, scope: ScopeId, block: &Block) {
        for statement in &block.statements {
            if let StatementKind::Function(function) = &statement.kind {
                self.declare(scope, function.id, function.name, SymbolKind::Function);
            }
        }

        for statement in &block.statements {
            self.resolve_statement(scope, statement);
        }

        if let Some(value) = &block.value {
            self.resolve_expression(scope, value);
        }
    }

    fn resolve_statement(&mut self, scope: ScopeId, statement: &Statement) {
        match &statement.kind {
            StatementKind::Binding {
                name,
                type_annotation,
                initializer,
                ..
            } => {
                if let Some(type_annotation) = type_annotation {
                    self.resolve_type(scope, type_annotation);
                }
                self.resolve_expression(scope, initializer);
                self.declare(scope, statement.id, *name, SymbolKind::Binding);
            }
            StatementKind::Expression(expression)
            | StatementKind::Defer(expression)
            | StatementKind::Coroutine(expression) => {
                self.resolve_expression(scope, expression);
            }
            StatementKind::Function(function) => self.resolve_function(function, scope),
            StatementKind::Break(value) | StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.resolve_expression(scope, value);
                }
            }
            StatementKind::Continue => {}
        }
    }

    fn resolve_expression(&mut self, scope: ScopeId, expression: &Expression) {
        match &expression.kind {
            ExpressionKind::Identifier => {
                self.resolve_value_name(scope, expression.id, expression.span);
            }
            ExpressionKind::SelfValue | ExpressionKind::Literal(_) => {}
            ExpressionKind::Group(inner) => self.resolve_expression(scope, inner),
            ExpressionKind::Block(block) => self.resolve_block(scope, block),
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.resolve_expression(scope, condition);
                self.resolve_block(scope, then_branch);
                if let Some(else_branch) = else_branch {
                    match else_branch {
                        ConditionalElse::Block(block) => self.resolve_block(scope, block),
                        ConditionalElse::If(expression) => {
                            self.resolve_expression(scope, expression);
                        }
                    }
                }
            }
            ExpressionKind::Loop { body } => self.resolve_block(scope, body),
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.resolve_expression(scope, condition);
                self.resolve_block(scope, body);
                if let Some(else_branch) = else_branch {
                    self.resolve_block(scope, else_branch);
                }
            }
            ExpressionKind::RangeFor {
                binding,
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                self.resolve_expression(scope, start);
                self.resolve_expression(scope, end);

                let body_scope = self.new_child_scope(scope);
                self.declare(
                    body_scope,
                    expression.id,
                    *binding,
                    SymbolKind::RangeBinding,
                );
                self.resolve_block_contents(body_scope, body);

                if let Some(else_branch) = else_branch {
                    self.resolve_block(scope, else_branch);
                }
            }
            ExpressionKind::Lambda {
                parameters,
                return_type,
                body,
            } => {
                self.resolve_parameter_types(scope, parameters);
                if let Some(return_type) = return_type {
                    self.resolve_type(scope, return_type);
                }

                let body_scope = self.new_child_scope(scope);
                self.declare_parameters(body_scope, parameters);
                self.resolve_block_contents(body_scope, body);
            }
            ExpressionKind::PrimitiveConversion { value, .. } => {
                self.resolve_expression(scope, value);
            }
            ExpressionKind::GcAllocate(value) => self.resolve_expression(scope, value),
            ExpressionKind::StructConstruction { name, fields } => {
                self.resolve_type_name(scope, expression.id, *name);
                for field in fields {
                    self.resolve_expression(scope, &field.value);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                for member in members {
                    match member {
                        AnonymousStructMember::Field(field) => {
                            if let Some(type_annotation) = &field.type_annotation {
                                self.resolve_type(scope, type_annotation);
                            }
                            self.resolve_expression(scope, &field.initializer);
                        }
                        AnonymousStructMember::Method(method) => {
                            self.resolve_function(method, scope);
                        }
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.resolve_expression(scope, callee);
                for argument in arguments {
                    self.resolve_expression(scope, argument);
                }
            }
            ExpressionKind::MemberAccess { object, .. } => {
                self.resolve_expression(scope, object);
            }
            ExpressionKind::AssociatedAccess { owner, .. } => {
                self.resolve_type(scope, owner);
            }
            ExpressionKind::Index { object, index } => {
                self.resolve_expression(scope, object);
                self.resolve_expression(scope, index);
            }
            ExpressionKind::Slice { object, start, end } => {
                self.resolve_expression(scope, object);
                if let Some(start) = start {
                    self.resolve_expression(scope, start);
                }
                if let Some(end) = end {
                    self.resolve_expression(scope, end);
                }
            }
            ExpressionKind::Try { expression } => self.resolve_expression(scope, expression),
            ExpressionKind::TypeTest { value, type_syntax } => {
                self.resolve_expression(scope, value);
                self.resolve_type(scope, type_syntax);
            }
            ExpressionKind::Unary { operand, .. } => self.resolve_expression(scope, operand),
            ExpressionKind::Binary { left, right, .. } => {
                self.resolve_expression(scope, left);
                self.resolve_expression(scope, right);
            }
            ExpressionKind::Assignment { target, value, .. } => {
                self.resolve_expression(scope, target);
                self.resolve_expression(scope, value);
            }
        }
    }

    fn resolve_type(&mut self, scope: ScopeId, type_syntax: &TypeSyntax) {
        match &type_syntax.kind {
            TypeKind::Primitive(_) => {}
            TypeKind::Builtin { arguments, .. } | TypeKind::Named { arguments, .. } => {
                if let TypeKind::Named { name, .. } = &type_syntax.kind {
                    self.resolve_type_name(scope, type_syntax.id, *name);
                }
                for argument in arguments {
                    self.resolve_type(scope, argument);
                }
            }
            TypeKind::Mutable(inner)
            | TypeKind::Gc(inner)
            | TypeKind::Group(inner) => self.resolve_type(scope, inner),
            TypeKind::Callable {
                parameters,
                return_type,
            } => {
                for parameter in parameters {
                    self.resolve_type(scope, parameter);
                }
                self.resolve_type(scope, return_type);
            }
            TypeKind::Intersection { members } | TypeKind::Union { members } => {
                for member in members {
                    self.resolve_type(scope, member);
                }
            }
        }
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        id: NodeId,
        span: Span,
        kind: SymbolKind,
    ) -> Option<SymbolId> {
        let name = self.text(span);
        match self.symbols.declare(scope, name, kind, span) {
            Ok(symbol) => {
                self.declarations.insert(id, symbol);
                Some(symbol)
            }
            Err(DeclareError::DuplicateDeclaration {
                name,
                original,
                duplicate,
            }) => {
                self.errors.push(NameResolutionError {
                    kind: NameResolutionErrorKind::DuplicateDeclaration {
                        name,
                        other: original,
                    },
                    span: duplicate,
                });
                None
            }
            Err(DeclareError::ScopeNotFound { scope }) => {
                panic!("resolver attempted to declare in missing scope {scope:?}")
            }
        }
    }

    fn resolve_value_name(&mut self, scope: ScopeId, id: NodeId, span: Span) {
        let name = self.text(span);
        match self.symbols.lookup_value(scope, name) {
            Ok(symbol) => {
                self.references.insert(id, symbol);
            }
            Err(SymbolLookupError::SymbolNotFound { namespace, name }) => {
                self.errors.push(NameResolutionError {
                    kind: NameResolutionErrorKind::UnknownName { namespace, name },
                    span,
                });
            }
            Err(SymbolLookupError::ScopeNotFound { scope }) => {
                panic!("resolver attempted to look up a value in missing scope {scope:?}")
            }
        }
    }

    fn resolve_type_name(&mut self, scope: ScopeId, id: NodeId, span: Span) {
        let name = self.text(span);
        match self.symbols.lookup_type(scope, name) {
            Ok(symbol) => {
                self.references.insert(id, symbol);
            }
            Err(SymbolLookupError::SymbolNotFound { namespace, name }) => {
                self.errors.push(NameResolutionError {
                    kind: NameResolutionErrorKind::UnknownName { namespace, name },
                    span,
                });
            }
            Err(SymbolLookupError::ScopeNotFound { scope }) => {
                panic!("resolver attempted to look up a type in missing scope {scope:?}")
            }
        }
    }

    fn new_child_scope(&mut self, parent: ScopeId) -> ScopeId {
        match self.symbols.new_child_scope(parent) {
            Ok(scope) => scope,
            Err(ScopeCreationError::ParentNotFound { parent }) => {
                panic!("resolver attempted to create a child of missing scope {parent:?}")
            }
        }
    }

    fn text(&self, span: Span) -> &'source str {
        self.module
            .text(span)
            .expect("AST name span must point into its source module")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lexer::Lexer,
        parser::{ParseContext, parse_program},
        source::SourceModuleRegistry,
    };

    fn parse(source: &str) -> (SourceModule, Program) {
        let module = SourceModuleRegistry::new().add(source);
        let mut context = ParseContext::new(module.module_id());
        let program =
            parse_program(&mut context, Lexer::new(&module)).expect("test program should parse");
        (module, program)
    }

    fn resolve(source: &str) -> (SourceModule, Program, NameResolution) {
        let (module, program) = parse(source);
        let resolution = resolve_program(&module, &program).expect("test program should resolve");
        (module, program, resolution)
    }

    fn nth_span(module: &SourceModule, text: &str, occurrence: usize) -> Span {
        let start = module
            .source()
            .match_indices(text)
            .nth(occurrence)
            .map(|(start, _)| start)
            .expect("requested source occurrence should exist");
        Span::new(module.module_id(), start, start + text.len())
    }

    fn function(declaration: &Declaration) -> &Function {
        let Declaration::Function(function) = declaration else {
            panic!("expected function declaration");
        };
        function
    }

    fn expression(statement: &Statement) -> &Expression {
        let StatementKind::Expression(expression) = &statement.kind else {
            panic!("expected expression statement");
        };
        expression
    }

    fn call_callee(expression: &Expression) -> &Expression {
        let ExpressionKind::Call { callee, .. } = &expression.kind else {
            panic!("expected call expression");
        };
        callee
    }

    #[test]
    fn resolves_forward_top_level_value_and_type_references() {
        let source = concat!(
            "struct Uses { value: Later, }\n",
            "struct Later {}\n",
            "fn main() { helper(); Later {} }\n",
            "fn helper() {}",
        );
        let (_, program, resolution) = resolve(source);
        let Declaration::Struct(uses) = &program.declarations[0] else {
            panic!("expected Uses struct");
        };
        let StructMember::Field(field) = &uses.members[0] else {
            panic!("expected Uses field");
        };
        let later_reference = field.type_annotation.id;
        let later_declaration = match &program.declarations[1] {
            Declaration::Struct(structure) => structure.id,
            _ => panic!("expected Later struct"),
        };
        let main = function(&program.declarations[2]);
        let helper_reference = call_callee(expression(&main.body.statements[0])).id;
        let construction_reference = main
            .body
            .value
            .as_ref()
            .expect("main should have a value")
            .id;
        let helper_declaration = function(&program.declarations[3]).id;

        let later = resolution
            .symbol_for_declaration(later_declaration)
            .expect("Later should be declared");
        assert_eq!(
            resolution.symbol_for_reference(later_reference),
            Some(later)
        );
        assert_eq!(
            resolution.symbol_for_reference(construction_reference),
            Some(later)
        );

        let helper = resolution
            .symbol_for_declaration(helper_declaration)
            .expect("helper should be declared");
        assert_eq!(
            resolution.symbol_for_reference(helper_reference),
            Some(helper)
        );
    }

    #[test]
    fn resolves_associated_access_through_the_type_namespace() {
        let source = concat!(
            "struct Point { fn origin() -> Point { Point {} } }\n",
            "fn main() { Point::origin(); }",
        );
        let (_, program, resolution) = resolve(source);
        let Declaration::Struct(point) = &program.declarations[0] else {
            panic!("expected Point struct");
        };
        let main = function(&program.declarations[1]);
        let callee = call_callee(expression(&main.body.statements[0]));
        let ExpressionKind::AssociatedAccess { owner, .. } = &callee.kind else {
            panic!("expected associated access");
        };

        assert_eq!(
            resolution.symbol_for_reference(owner.id),
            resolution.symbol_for_declaration(point.id)
        );
    }

    #[test]
    fn resolves_types_inside_builtin_associated_access() {
        let source = concat!("struct Item {}\n", "fn main() { Queue<Item>::new(); }",);
        let (_, program, resolution) = resolve(source);
        let Declaration::Struct(item) = &program.declarations[0] else {
            panic!("expected Item struct");
        };
        let main = function(&program.declarations[1]);
        let callee = call_callee(expression(&main.body.statements[0]));
        let ExpressionKind::AssociatedAccess { owner, .. } = &callee.kind else {
            panic!("expected associated access");
        };
        let TypeKind::Builtin { arguments, .. } = &owner.kind else {
            panic!("expected built-in owner");
        };

        assert_eq!(
            resolution.symbol_for_reference(arguments[0].id),
            resolution.symbol_for_declaration(item.id)
        );
    }

    #[test]
    fn resolves_nested_functions_before_their_block_statements() {
        let source = "fn main() { call(); fn call() {} }";
        let (_, program, resolution) = resolve(source);
        let main = function(&program.declarations[0]);
        let reference = call_callee(expression(&main.body.statements[0])).id;
        let StatementKind::Function(call) = &main.body.statements[1].kind else {
            panic!("expected nested call function");
        };
        let declaration = call.id;

        assert_eq!(
            resolution.symbol_for_reference(reference),
            resolution.symbol_for_declaration(declaration)
        );
    }

    #[test]
    fn resolves_enclosing_locals_for_later_named_function_capture_validation() {
        let source = concat!(
            "fn main() {\n",
            "    const outer = 1;\n",
            "    fn inner() { outer; }\n",
            "}",
        );
        let (_, program, resolution) = resolve(source);
        let main = function(&program.declarations[0]);
        let declaration = main.body.statements[0].id;
        let StatementKind::Function(inner) = &main.body.statements[1].kind else {
            panic!("expected nested inner function");
        };
        let reference = expression(&inner.body.statements[0]).id;

        assert_eq!(
            resolution.symbol_for_reference(reference),
            resolution.symbol_for_declaration(declaration)
        );
    }

    #[test]
    fn a_binding_initializer_sees_the_binding_being_shadowed() {
        let source = concat!(
            "fn main() {\n",
            "    const value = 1;\n",
            "    const value = value;\n",
            "    value\n",
            "}",
        );
        let (_, program, resolution) = resolve(source);
        let main = function(&program.declarations[0]);
        let first_declaration = main.body.statements[0].id;
        let second_declaration = main.body.statements[1].id;
        let StatementKind::Binding { initializer, .. } = &main.body.statements[1].kind else {
            panic!("expected shadowing binding");
        };
        let initializer_reference = initializer.id;
        let final_reference = main
            .body
            .value
            .as_ref()
            .expect("main should have a value")
            .id;

        let first = resolution
            .symbol_for_declaration(first_declaration)
            .expect("first binding should be declared");
        let second = resolution
            .symbol_for_declaration(second_declaration)
            .expect("second binding should be declared");

        assert_ne!(first, second);
        assert_eq!(
            resolution.symbol_for_reference(initializer_reference),
            Some(first)
        );
        assert_eq!(
            resolution.symbol_for_reference(final_reference),
            Some(second)
        );
    }

    #[test]
    fn resolves_range_bounds_before_introducing_the_induction_binding() {
        let source = concat!(
            "fn main() {\n",
            "    const index = 1;\n",
            "    for index in index..10 { index; }\n",
            "}",
        );
        let (_, program, resolution) = resolve(source);
        let main = function(&program.declarations[0]);
        assert_eq!(main.body.statements.len(), 1);
        let outer_declaration = main.body.statements[0].id;
        let range = main
            .body
            .value
            .as_deref()
            .expect("main should end with a range expression");
        let range_declaration = range.id;
        let ExpressionKind::RangeFor { start, body, .. } = &range.kind else {
            panic!("expected range expression");
        };
        let bound_reference = start.id;
        let body_reference = expression(&body.statements[0]).id;

        let outer = resolution
            .symbol_for_declaration(outer_declaration)
            .expect("outer binding should be declared");
        let range = resolution
            .symbol_for_declaration(range_declaration)
            .expect("range binding should be declared");

        assert_eq!(
            resolution.symbol_for_reference(bound_reference),
            Some(outer)
        );
        assert_eq!(resolution.symbol_for_reference(body_reference), Some(range));
    }

    #[test]
    fn resolves_builtin_values_through_the_prelude_scope() {
        let source = "fn main() { print(\"hello\"); yield(); }";
        let (_, program, resolution) = resolve(source);
        let main = function(&program.declarations[0]);

        for statement in &main.body.statements {
            let reference = call_callee(expression(statement)).id;
            let symbol = resolution
                .symbol_for_reference(reference)
                .expect("built-in should resolve");
            let builtin = resolution
                .symbols()
                .symbol(symbol)
                .expect("built-in symbol should exist");
            assert_eq!(builtin.kind, SymbolKind::BuiltinValue);
            assert_eq!(builtin.span.module_id, ModuleId::PRELUDE);
            assert!(builtin.span.is_empty());
        }
    }

    #[test]
    fn program_declarations_can_shadow_prelude_values() {
        let source = "fn print() {} fn main() { print(); }";
        let (_, program, resolution) = resolve(source);
        let declaration = function(&program.declarations[0]).id;
        let main = function(&program.declarations[1]);
        let reference = call_callee(expression(&main.body.statements[0])).id;

        let symbol = resolution
            .symbol_for_declaration(declaration)
            .expect("print should be declared");
        assert_eq!(resolution.symbol_for_reference(reference), Some(symbol));
        assert_eq!(
            resolution
                .symbols()
                .symbol(symbol)
                .expect("declared symbol should exist")
                .kind,
            SymbolKind::Function
        );
    }

    #[test]
    fn reports_unknown_value_and_type_names() {
        let source = "fn main(value: MissingType) { missing_value; }";
        let (module, program) = parse(source);
        let errors = resolve_program(&module, &program).expect_err("names are unknown");

        assert_eq!(
            errors,
            vec![
                NameResolutionError {
                    kind: NameResolutionErrorKind::UnknownName {
                        namespace: Namespace::Type,
                        name: "MissingType".to_string(),
                    },
                    span: nth_span(&module, "MissingType", 0),
                },
                NameResolutionError {
                    kind: NameResolutionErrorKind::UnknownName {
                        namespace: Namespace::Value,
                        name: "missing_value".to_string(),
                    },
                    span: nth_span(&module, "missing_value", 0),
                },
            ]
        );
    }

    #[test]
    fn bindings_do_not_escape_their_lexical_block() {
        let source = concat!(
            "fn main() {\n",
            "    if true { const hidden = 1; }\n",
            "    hidden;\n",
            "}",
        );
        let (module, program) = parse(source);
        let errors = resolve_program(&module, &program).expect_err("hidden is out of scope");

        assert!(errors.iter().any(|error| {
            error
                == &NameResolutionError {
                    kind: NameResolutionErrorKind::UnknownName {
                        namespace: Namespace::Value,
                        name: "hidden".to_string(),
                    },
                    span: nth_span(&module, "hidden", 1),
                }
        }));
    }

    #[test]
    fn reports_a_missing_top_level_main() {
        let source = "fn helper() {}";
        let (module, program) = parse(source);
        let errors = resolve_program(&module, &program).expect_err("main is missing");

        assert!(errors.iter().any(|error| {
            error.kind == NameResolutionErrorKind::MissingMain
                && error.span == Span::new(module.module_id(), source.len(), source.len())
        }));
    }

    #[test]
    fn reports_duplicate_top_level_main_functions() {
        let source = "fn main() {} fn main() {}";
        let (module, program) = parse(source);
        let errors = resolve_program(&module, &program).expect_err("main is not unique");

        assert!(errors.iter().any(|error| {
            matches!(
                &error.kind,
                NameResolutionErrorKind::DuplicateDeclaration { name, .. } if name == "main"
            ) && error.span == nth_span(&module, "main", 1)
        }));
    }
}
