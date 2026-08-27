//! Resolves explicit source type syntax into canonical semantic type identities.
//!
//! This pass runs after name resolution and before signature collection or
//! expression type checking. It predeclares nominal types for recursive and
//! forward references, records a [`TypeId`] for every concrete [`TypeSyntax`]
//! node, and reports type-syntax legality errors while using recovery types to
//! continue resolving independent annotations. The unspecialized owner in
//! `Error::new(...)` remains for built-in inference because it is not a
//! concrete source type.

use std::{collections::HashMap, fmt};

use crate::{
    ast::{
        AnonymousStructMember, Block, BuiltinType, ConditionalElse, Declaration, Expression,
        ExpressionKind, Function, FunctionParameter, FunctionParameterKind,
        InterfaceMethodRequirement, NodeId, PrimitiveType, Program, Statement, StatementKind,
        StructMember, TypeKind, TypeSyntax,
    },
    name_resolution::NameResolution,
    semantic_types::{AccessCapability, SemanticType, TypeId, TypeStore},
    source::{SourceModule, Span},
    symbol_table::SymbolId,
};

/// Canonical types produced from concrete source type syntax in a program.
#[derive(Debug)]
pub struct TypeResolution {
    types: TypeStore,
    syntax_types: HashMap<NodeId, TypeId>,
    declaration_types: HashMap<NodeId, TypeId>,
}

impl TypeResolution {
    /// Returns the canonical type store owned by this resolution.
    #[must_use]
    pub const fn types(&self) -> &TypeStore {
        &self.types
    }

    /// Returns the program type store for later semantic passes that intern
    /// signatures or inferred expression types.
    #[must_use]
    pub const fn types_mut(&mut self) -> &mut TypeStore {
        &mut self.types
    }

    /// Returns the canonical type resolved for one [`TypeSyntax`] node.
    #[must_use]
    pub fn type_for_syntax(&self, id: NodeId) -> Option<TypeId> {
        self.syntax_types.get(&id).copied()
    }

    /// Returns the canonical plain type predeclared for a struct or interface.
    #[must_use]
    pub fn type_for_declaration(&self, id: NodeId) -> Option<TypeId> {
        self.declaration_types.get(&id).copied()
    }

    /// Returns every resolved source type keyed by its syntax-node identity.
    #[must_use]
    pub const fn syntax_types(&self) -> &HashMap<NodeId, TypeId> {
        &self.syntax_types
    }

    /// Returns every predeclared nominal type keyed by declaration identity.
    #[must_use]
    pub const fn declaration_types(&self) -> &HashMap<NodeId, TypeId> {
        &self.declaration_types
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeResolutionError {
    pub kind: TypeResolutionErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeResolutionErrorKind {
    TypeArgumentsNotSupported {
        found: usize,
    },
    InvalidBuiltinTypeArgumentCount {
        builtin: BuiltinType,
        expected: usize,
        found: usize,
    },
    QueueElementContainsNone,
    InvalidIntersectionMember,
}

impl fmt::Display for TypeResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TypeResolutionErrorKind::TypeArgumentsNotSupported { found } => write!(
                formatter,
                "named type does not accept type arguments, but {found} were supplied at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidBuiltinTypeArgumentCount {
                builtin,
                expected,
                found,
            } => write!(
                formatter,
                "{builtin:?} expects {expected} type arguments, but {found} were supplied at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::QueueElementContainsNone => write!(
                formatter,
                "Queue element type cannot contain `none` as a top-level alternative at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidIntersectionMember => write!(
                formatter,
                "intersection members must be plain interface types at {}..{}",
                self.span.start, self.span.end
            ),
        }
    }
}

impl std::error::Error for TypeResolutionError {}

pub type TypeResolutionResult = Result<TypeResolution, Vec<TypeResolutionError>>;

/// Resolves every concrete source type after successful name resolution.
///
/// Struct and interface types are predeclared before annotations are visited,
/// so forward and recursive references resolve to stable canonical identities.
/// Invalid syntax nodes receive the recovery type internally, allowing later
/// annotations to be resolved and independently diagnosed.
pub fn resolve_types(
    module: &SourceModule,
    program: &Program,
    names: &NameResolution,
) -> TypeResolutionResult {
    assert_eq!(
        module.module_id(),
        program.id.module_id,
        "program types must be resolved with their source module"
    );
    Resolver::new(module, names).resolve(program)
}

struct Resolver<'source, 'names> {
    module: &'source SourceModule,
    names: &'names NameResolution,
    types: TypeStore,
    syntax_types: HashMap<NodeId, TypeId>,
    declaration_types: HashMap<NodeId, TypeId>,
    symbol_types: HashMap<SymbolId, TypeId>,
    errors: Vec<TypeResolutionError>,
}

impl<'source, 'names> Resolver<'source, 'names> {
    fn new(module: &'source SourceModule, names: &'names NameResolution) -> Self {
        Self {
            module,
            names,
            types: TypeStore::new(),
            syntax_types: HashMap::new(),
            declaration_types: HashMap::new(),
            symbol_types: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn resolve(mut self, program: &Program) -> TypeResolutionResult {
        self.predeclare_nominal_types(program);
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }

        if self.errors.is_empty() {
            Ok(TypeResolution {
                types: self.types,
                syntax_types: self.syntax_types,
                declaration_types: self.declaration_types,
            })
        } else {
            Err(self.errors)
        }
    }

    fn predeclare_nominal_types(&mut self, program: &Program) {
        for declaration in &program.declarations {
            let (id, type_id) = match declaration {
                Declaration::Struct(structure) => (
                    structure.id,
                    self.types
                        .named_struct(structure.id, AccessCapability::Const),
                ),
                Declaration::Interface(interface) => (
                    interface.id,
                    self.types.interface(interface.id, AccessCapability::Const),
                ),
                Declaration::Function(_) => continue,
            };
            let symbol = self
                .names
                .symbol_for_declaration(id)
                .expect("type declaration must have name-resolution metadata");
            self.declaration_types.insert(id, type_id);
            self.symbol_types.insert(symbol, type_id);
        }
    }

    fn visit_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function) => self.visit_function(function),
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    match member {
                        StructMember::Field(field) => {
                            self.resolve_type(&field.type_annotation);
                        }
                        StructMember::Function(function) => self.visit_function(function),
                    }
                }
            }
            Declaration::Interface(interface) => {
                for requirement in &interface.requirements {
                    self.visit_interface_requirement(requirement);
                }
            }
        }
    }

    fn visit_interface_requirement(&mut self, requirement: &InterfaceMethodRequirement) {
        self.visit_parameters(&requirement.parameters);
        if let Some(return_type) = &requirement.return_type {
            self.resolve_type(return_type);
        }
    }

    fn visit_function(&mut self, function: &Function) {
        self.visit_parameters(&function.parameters);
        if let Some(return_type) = &function.return_type {
            self.resolve_type(return_type);
        }
        self.visit_block(&function.body);
    }

    fn visit_parameters(&mut self, parameters: &[FunctionParameter]) {
        for parameter in parameters {
            if let FunctionParameterKind::Named {
                type_annotation, ..
            } = &parameter.kind
            {
                self.resolve_type(type_annotation);
            }
        }
    }

    fn visit_block(&mut self, block: &Block) {
        for statement in &block.statements {
            self.visit_statement(statement);
        }
        if let Some(value) = &block.value {
            self.visit_expression(value);
        }
    }

    fn visit_statement(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::Binding {
                type_annotation,
                initializer,
                ..
            } => {
                if let Some(type_annotation) = type_annotation {
                    self.resolve_type(type_annotation);
                }
                self.visit_expression(initializer);
            }
            StatementKind::Expression(expression)
            | StatementKind::Defer(expression)
            | StatementKind::Coroutine(expression) => self.visit_expression(expression),
            StatementKind::Function(function) => self.visit_function(function),
            StatementKind::Break(value) | StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.visit_expression(value);
                }
            }
            StatementKind::Continue => {}
        }
    }

    fn visit_expression(&mut self, expression: &Expression) {
        match &expression.kind {
            ExpressionKind::Identifier | ExpressionKind::SelfValue | ExpressionKind::Literal(_) => {
            }
            ExpressionKind::Group(inner)
            | ExpressionKind::GcAllocate(inner)
            | ExpressionKind::PrimitiveConversion { value: inner, .. }
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::Unary { operand: inner, .. } => self.visit_expression(inner),
            ExpressionKind::Block(block) | ExpressionKind::Loop { body: block } => {
                self.visit_block(block);
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expression(condition);
                self.visit_block(then_branch);
                if let Some(else_branch) = else_branch {
                    match else_branch {
                        ConditionalElse::Block(block) => self.visit_block(block),
                        ConditionalElse::If(expression) => self.visit_expression(expression),
                    }
                }
            }
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.visit_expression(condition);
                self.visit_block(body);
                if let Some(else_branch) = else_branch {
                    self.visit_block(else_branch);
                }
            }
            ExpressionKind::RangeFor {
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                self.visit_expression(start);
                self.visit_expression(end);
                self.visit_block(body);
                if let Some(else_branch) = else_branch {
                    self.visit_block(else_branch);
                }
            }
            ExpressionKind::Lambda {
                parameters,
                return_type,
                body,
            } => {
                self.visit_parameters(parameters);
                if let Some(return_type) = return_type {
                    self.resolve_type(return_type);
                }
                self.visit_block(body);
            }
            ExpressionKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.visit_expression(&field.value);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                for member in members {
                    match member {
                        AnonymousStructMember::Field(field) => {
                            if let Some(type_annotation) = &field.type_annotation {
                                self.resolve_type(type_annotation);
                            }
                            self.visit_expression(&field.initializer);
                        }
                        AnonymousStructMember::Method(function) => self.visit_function(function),
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.visit_expression(callee);
                for argument in arguments {
                    self.visit_expression(argument);
                }
            }
            ExpressionKind::MemberAccess { object, .. } => self.visit_expression(object),
            ExpressionKind::AssociatedAccess { owner, member } => {
                // `Error::new` uses an intentionally unspecialized built-in
                // owner whose payload type is inferred while checking the
                // associated function. It is not valid ordinary type syntax,
                // so it has no concrete TypeId at this stage.
                if !matches!(
                    &owner.kind,
                    TypeKind::Builtin {
                        builtin: BuiltinType::Error,
                        arguments,
                    } if arguments.is_empty()
                        && self
                            .module
                            .text(*member)
                            .expect("associated member span belongs to the source module")
                            == "new"
                ) {
                    self.resolve_type(owner);
                }
            }
            ExpressionKind::Index { object, index } => {
                self.visit_expression(object);
                self.visit_expression(index);
            }
            ExpressionKind::Slice { object, start, end } => {
                self.visit_expression(object);
                if let Some(start) = start {
                    self.visit_expression(start);
                }
                if let Some(end) = end {
                    self.visit_expression(end);
                }
            }
            ExpressionKind::TypeTest { value, type_syntax }
            | ExpressionKind::TypeAscription { value, type_syntax } => {
                self.visit_expression(value);
                self.resolve_type(type_syntax);
            }
            ExpressionKind::Binary { left, right, .. }
            | ExpressionKind::Assignment {
                target: left,
                value: right,
                ..
            } => {
                self.visit_expression(left);
                self.visit_expression(right);
            }
        }
    }

    fn resolve_type(&mut self, syntax: &TypeSyntax) -> TypeId {
        let resolved = match &syntax.kind {
            TypeKind::Primitive(primitive) => {
                self.types.primitive(*primitive, AccessCapability::Const)
            }
            TypeKind::Builtin { builtin, arguments } => {
                self.resolve_builtin(*builtin, arguments, syntax.span)
            }
            TypeKind::Named { arguments, .. } => self.resolve_named(syntax, arguments),
            TypeKind::Mutable(inner) => {
                let inner = self.resolve_type(inner);
                self.types
                    .with_capability(inner, AccessCapability::Mut)
                    .expect("resolved type belongs to this type store")
            }
            TypeKind::Gc(inner) => {
                let inner = self.resolve_type(inner);
                self.types
                    .gc(inner)
                    .expect("resolved type belongs to this type store")
            }
            TypeKind::Group(inner) => self.resolve_type(inner),
            TypeKind::Callable {
                parameters,
                return_type,
            } => {
                let parameters = parameters
                    .iter()
                    .map(|parameter| self.resolve_type(parameter))
                    .collect();
                let return_type = self.resolve_type(return_type);
                self.types
                    .callable(parameters, return_type, AccessCapability::Const)
            }
            TypeKind::Intersection { members } => self.resolve_intersection(members),
            TypeKind::Union { members } => {
                let members = members
                    .iter()
                    .map(|member| self.resolve_type(member))
                    .collect();
                self.types.union(members, AccessCapability::Const)
            }
        };

        self.syntax_types.insert(syntax.id, resolved);
        resolved
    }

    fn resolve_named(&mut self, syntax: &TypeSyntax, arguments: &[TypeSyntax]) -> TypeId {
        for argument in arguments {
            self.resolve_type(argument);
        }
        if !arguments.is_empty() {
            self.error(
                TypeResolutionErrorKind::TypeArgumentsNotSupported {
                    found: arguments.len(),
                },
                syntax.span,
            );
            return self.types.recovery();
        }

        let symbol = self
            .names
            .symbol_for_reference(syntax.id)
            .expect("named type syntax must have name-resolution metadata");
        *self
            .symbol_types
            .get(&symbol)
            .expect("resolved type symbol must identify a struct or interface declaration")
    }

    fn resolve_builtin(
        &mut self,
        builtin: BuiltinType,
        arguments: &[TypeSyntax],
        span: Span,
    ) -> TypeId {
        let resolved_arguments: Vec<_> = arguments
            .iter()
            .map(|argument| self.resolve_type(argument))
            .collect();
        let expected = builtin_type_argument_count(builtin);
        if arguments.len() != expected {
            self.error(
                TypeResolutionErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin,
                    expected,
                    found: arguments.len(),
                },
                span,
            );
            return self.types.recovery();
        }

        if builtin == BuiltinType::Queue
            && self.contains_plain_none_alternative(resolved_arguments[0])
        {
            self.error(TypeResolutionErrorKind::QueueElementContainsNone, span);
            return self.types.recovery();
        }

        self.types
            .builtin(builtin, resolved_arguments, AccessCapability::Const)
    }

    fn resolve_intersection(&mut self, members: &[TypeSyntax]) -> TypeId {
        let mut resolved_members = Vec::with_capacity(members.len());
        let mut invalid = false;
        for member in members {
            let resolved = self.resolve_type(member);
            if !self.is_plain_interface_type(resolved) {
                self.error(
                    TypeResolutionErrorKind::InvalidIntersectionMember,
                    member.span,
                );
                invalid = true;
            }
            resolved_members.push(resolved);
        }

        if invalid {
            self.types.recovery()
        } else {
            self.types
                .intersection(resolved_members, AccessCapability::Const)
        }
    }

    fn is_plain_interface_type(&self, id: TypeId) -> bool {
        match self.types.get(id) {
            Some(SemanticType::Interface { .. }) => true,
            Some(SemanticType::Intersection { members, .. }) => members
                .iter()
                .all(|member| self.is_plain_interface_type(*member)),
            Some(SemanticType::Recovery) => true,
            _ => false,
        }
    }

    fn contains_plain_none_alternative(&self, id: TypeId) -> bool {
        match self.types.get(id) {
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::None,
                ..
            }) => true,
            Some(SemanticType::Union { members, .. }) => members
                .iter()
                .any(|member| self.contains_plain_none_alternative(*member)),
            _ => false,
        }
    }

    fn error(&mut self, kind: TypeResolutionErrorKind, span: Span) {
        self.errors.push(TypeResolutionError { kind, span });
    }
}

const fn builtin_type_argument_count(builtin: BuiltinType) -> usize {
    match builtin {
        BuiltinType::Queue | BuiltinType::Vector | BuiltinType::Error => 1,
        BuiltinType::Map => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lexer::Lexer,
        name_resolution::resolve_program,
        parser::{ParseContext, parse_program},
        source::{SourceModule, SourceModuleRegistry},
    };

    fn parse(source: &str) -> (SourceModule, Program) {
        let module = SourceModuleRegistry::new().add(source);
        let mut context = ParseContext::new(module.module_id());
        let program =
            parse_program(&mut context, Lexer::new(&module)).expect("test program should parse");
        (module, program)
    }

    fn resolve(source: &str) -> (Program, TypeResolution) {
        let (module, program) = parse(source);
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let resolution =
            resolve_types(&module, &program, &names).expect("test types should resolve");
        (program, resolution)
    }

    fn named_parameter_type(function: &Function, index: usize) -> &TypeSyntax {
        let FunctionParameterKind::Named {
            type_annotation, ..
        } = &function.parameters[index].kind
        else {
            panic!("expected a named parameter");
        };
        type_annotation
    }

    fn top_level_function(program: &Program, index: usize) -> &Function {
        let Declaration::Function(function) = &program.declarations[index] else {
            panic!("expected a function declaration");
        };
        function
    }

    #[test]
    fn resolves_source_forms_to_canonical_types() {
        let (program, resolution) = resolve(concat!(
            "interface Reader { fn read(self) -> string; }\n",
            "interface Writer { fn write(self, value: string); }\n",
            "struct User { name: string, }\n",
            "fn main(\n",
            "    first: int,\n",
            "    second: int,\n",
            "    user: &mut User,\n",
            "    stream: &mut (Reader & Writer),\n",
            "    callback: fn(int) -> string,\n",
            "    table: Map<string, Vector<int>>,\n",
            "    boxed_none: Queue<&none>,\n",
            ") {}\n",
        ));
        let main = top_level_function(&program, 3);
        let first = resolution
            .type_for_syntax(named_parameter_type(main, 0).id)
            .expect("first parameter type should resolve");
        let second = resolution
            .type_for_syntax(named_parameter_type(main, 1).id)
            .expect("second parameter type should resolve");
        assert_eq!(first, second);

        let user = resolution
            .type_for_syntax(named_parameter_type(main, 2).id)
            .expect("GC user type should resolve");
        let Some(SemanticType::Gc { target, capability }) =
            resolution.types().get(user)
        else {
            panic!("expected a GC-qualified user type");
        };
        assert_eq!(*capability, AccessCapability::Mut);
        let user_declaration = match &program.declarations[2] {
            Declaration::Struct(structure) => structure.id,
            _ => panic!("expected a struct declaration"),
        };
        let declared_user = resolution
            .type_for_declaration(user_declaration)
            .expect("user declaration type should resolve");
        assert_eq!(
            resolution.types().has_same_shape(*target, declared_user),
            Some(true)
        );
        assert!(matches!(
            resolution.types().get(*target),
            Some(SemanticType::NamedStruct {
                declaration,
                capability: AccessCapability::Mut,
            }) if *declaration == user_declaration
        ));

        let stream = resolution
            .type_for_syntax(named_parameter_type(main, 3).id)
            .expect("GC intersection should resolve");
        let Some(SemanticType::Gc { target, capability }) =
            resolution.types().get(stream)
        else {
            panic!("expected a GC-qualified intersection");
        };
        assert_eq!(*capability, AccessCapability::Mut);
        assert!(matches!(
            resolution.types().get(*target),
            Some(SemanticType::Intersection { members, .. }) if members.len() == 2
        ));

        assert!(matches!(
            resolution.types().get(
                resolution
                    .type_for_syntax(named_parameter_type(main, 4).id)
                    .expect("callable type should resolve")
            ),
            Some(SemanticType::Callable { .. })
        ));
        assert!(matches!(
            resolution.types().get(
                resolution
                    .type_for_syntax(named_parameter_type(main, 5).id)
                    .expect("map type should resolve")
            ),
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Map,
                arguments,
                ..
            }) if arguments.len() == 2
        ));
    }

    #[test]
    fn predeclares_forward_and_recursive_nominal_references() {
        let (program, resolution) = resolve(concat!(
            "struct First { second: Second, }\n",
            "struct Second { first: &First, }\n",
            "fn main() {}\n",
        ));
        let Declaration::Struct(first) = &program.declarations[0] else {
            panic!("expected First");
        };
        let Declaration::Struct(second) = &program.declarations[1] else {
            panic!("expected Second");
        };
        let StructMember::Field(second_field) = &first.members[0] else {
            panic!("expected a field");
        };
        let StructMember::Field(first_field) = &second.members[0] else {
            panic!("expected a field");
        };

        assert_eq!(
            resolution.type_for_syntax(second_field.type_annotation.id),
            resolution.type_for_declaration(second.id)
        );
        let recursive_reference = resolution
            .type_for_syntax(first_field.type_annotation.id)
            .expect("recursive GC reference should resolve");
        assert_eq!(
            resolution
                .types()
                .gc_target(recursive_reference),
            resolution.type_for_declaration(first.id)
        );
    }

    #[test]
    fn visits_annotations_in_declarations_and_executable_syntax() {
        let (program, resolution) = resolve(concat!(
            "struct Thing {\n",
            "    value: int,\n",
            "    fn make(input: string) -> Thing {\n",
            "        const local: bool = true;\n",
            "        Thing { value: 0 }\n",
            "    }\n",
            "}\n",
            "interface Read { fn read(self, count: int) -> string; }\n",
            "fn main(argument: Thing) {\n",
            "    fn nested(value: float) -> char { 'a' }\n",
            "    const closure = lambda(value: bytes) -> () {};\n",
            "    const object = struct { field: int = 1; fn run(self, value: bool) {} };\n",
            "    argument is Thing;\n",
            "    Thing::make;\n",
            "}\n",
        ));

        // This program contains fifteen TypeSyntax nodes, all deliberately in
        // distinct annotation-bearing AST locations. Nested type syntax would
        // add further entries to this same map.
        assert_eq!(resolution.syntax_types().len(), 15);
        assert!(
            resolution
                .syntax_types()
                .values()
                .all(|type_id| resolution.types().contains(*type_id))
        );
        assert_eq!(program.declarations.len(), 3);
    }

    #[test]
    fn reports_source_type_errors_in_traversal_order() {
        let (module, program) = parse(concat!(
            "struct User {}\n",
            "interface Reader { fn read(self); }\n",
            "fn main(\n",
            "    generic: User<int>,\n",
            "    messages: Queue<int | none>,\n",
            "    mixed: User & Reader,\n",
            "    qualified_member: &Reader & Reader,\n",
            ") {}\n",
        ));
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let errors = resolve_types(&module, &program, &names).expect_err("types should be invalid");

        assert_eq!(errors.len(), 4);
        assert!(matches!(
            errors[0].kind,
            TypeResolutionErrorKind::TypeArgumentsNotSupported { found: 1 }
        ));
        assert_eq!(
            errors[1].kind,
            TypeResolutionErrorKind::QueueElementContainsNone
        );
        assert_eq!(
            errors[2].kind,
            TypeResolutionErrorKind::InvalidIntersectionMember
        );
        assert_eq!(
            errors[3].kind,
            TypeResolutionErrorKind::InvalidIntersectionMember
        );
        assert!(
            errors
                .windows(2)
                .all(|pair| pair[0].span.start < pair[1].span.start)
        );
    }

    #[test]
    fn defensively_rejects_invalid_builtin_arity() {
        let (module, mut program) = parse("fn main(value: Queue<int>) {}");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let main = match &mut program.declarations[0] {
            Declaration::Function(function) => function,
            _ => panic!("expected main"),
        };
        let FunctionParameterKind::Named {
            type_annotation, ..
        } = &mut main.parameters[0].kind
        else {
            panic!("expected a named parameter");
        };
        let TypeKind::Builtin { arguments, .. } = &mut type_annotation.kind else {
            panic!("expected Queue type syntax");
        };
        arguments.clear();
        let annotation_span = type_annotation.span;

        let errors = resolve_types(&module, &program, &names).expect_err("arity should be invalid");
        assert_eq!(
            errors,
            vec![TypeResolutionError {
                kind: TypeResolutionErrorKind::InvalidBuiltinTypeArgumentCount {
                    builtin: BuiltinType::Queue,
                    expected: 1,
                    found: 0,
                },
                span: annotation_span,
            }]
        );
    }

    #[test]
    fn permits_unspecialized_error_only_as_an_associated_owner() {
        let (module, program) = parse("fn main() { Error::new(1); }");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let resolution = resolve_types(&module, &program, &names)
            .expect("Error::new should leave its payload type for call inference");

        let Declaration::Function(main) = &program.declarations[0] else {
            panic!("expected main")
        };
        let StatementKind::Expression(Expression {
            kind: ExpressionKind::Call { callee, .. },
            ..
        }) = &main.body.statements[0].kind
        else {
            panic!("expected Error::new call")
        };
        let ExpressionKind::AssociatedAccess { owner, .. } = &callee.kind else {
            panic!("expected associated access")
        };
        assert_eq!(resolution.type_for_syntax(owner.id), None);

        let (module, mut program) = parse("fn main(value: Error<int>) {}");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let Declaration::Function(main) = &mut program.declarations[0] else {
            panic!("expected main")
        };
        let FunctionParameterKind::Named {
            type_annotation, ..
        } = &mut main.parameters[0].kind
        else {
            panic!("expected named parameter")
        };
        let TypeKind::Builtin { arguments, .. } = &mut type_annotation.kind else {
            panic!("expected Error type")
        };
        arguments.clear();
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if matches!(
                errors.as_slice(),
                [TypeResolutionError {
                    kind: TypeResolutionErrorKind::InvalidBuiltinTypeArgumentCount {
                        builtin: BuiltinType::Error,
                        expected: 1,
                        found: 0,
                    },
                    ..
                }]
            )
        ));

        let (module, program) = parse("fn main() { Error::other; }");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if matches!(
                errors.as_slice(),
                [TypeResolutionError {
                    kind: TypeResolutionErrorKind::InvalidBuiltinTypeArgumentCount {
                        builtin: BuiltinType::Error,
                        expected: 1,
                        found: 0,
                    },
                    ..
                }]
            )
        ));
    }
}
