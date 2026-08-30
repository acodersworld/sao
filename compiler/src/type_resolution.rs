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
        ExpressionKind, FormattedStringPart, Function, FunctionParameter, FunctionParameterKind,
        ComptimeParameterConstraint, InterfaceConstraint, InterfaceMethodRequirement, NodeId,
        PrimitiveType, Program, Statement, StatementKind, StructMember, TypeAliasDeclaration,
        TypeKind, TypeSyntax, expression_as_type_syntax,
    },
    name_resolution::NameResolution,
    semantic_types::{AccessCapability, SemanticType, TypeId, TypeStore},
    source::{SourceModule, Span},
    symbol_table::{SymbolId, SymbolKind},
};

/// Canonical types produced from concrete source type syntax in a program.
#[derive(Debug)]
pub struct TypeResolution {
    types: TypeStore,
    syntax_types: HashMap<NodeId, TypeId>,
    declaration_types: HashMap<NodeId, TypeId>,
    generated_structs: HashMap<TypeId, GeneratedStructInstantiation>,
    specialized_syntax_types: HashMap<(TypeId, NodeId), TypeId>,
    template_parameter_bounds: HashMap<NodeId, Option<TypeId>>,
    specialized_template_parameter_bounds: HashMap<(TypeId, NodeId), Option<TypeId>>,
    runtime_template_calls: HashMap<NodeId, RuntimeTemplateCall>,
    runtime_member_template_calls: HashMap<NodeId, RuntimeMemberTemplateCall>,
}

/// Concrete type arguments attached to one explicit top-level runtime-template
/// call. Runtime arguments remain in the call AST after this leading prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeTemplateCall {
    pub(crate) declaration: NodeId,
    pub(crate) type_arguments: Vec<TypeId>,
    pub(crate) comptime_argument_count: usize,
}

/// Canonical leading type arguments on a member call whose declaration is
/// selected later, after expression analysis has typed the receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeMemberTemplateCall {
    pub(crate) type_arguments: Vec<TypeId>,
    pub(crate) comptime_argument_count: usize,
}

/// One canonical struct materialized from a type-factory body.
#[derive(Debug, Clone)]
pub struct GeneratedStructInstantiation {
    pub type_id: TypeId,
    pub template: NodeId,
    pub factory: NodeId,
    pub arguments: Vec<TypeId>,
    pub field_types: HashMap<NodeId, TypeId>,
    pub substitutions: HashMap<SymbolId, TypeId>,
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

    #[must_use]
    pub const fn generated_structs(&self) -> &HashMap<TypeId, GeneratedStructInstantiation> {
        &self.generated_structs
    }

    #[must_use]
    pub fn specialized_type_for_syntax(&self, owner: TypeId, syntax: NodeId) -> Option<TypeId> {
        self.specialized_syntax_types.get(&(owner, syntax)).copied()
    }

    /// Returns the interface or interface intersection which limits member
    /// access on an unspecialized template parameter. `None` distinguishes an
    /// unconstrained `T: type` parameter from a non-parameter type.
    #[must_use]
    pub fn template_parameter_bound(&self, type_id: TypeId) -> Option<Option<TypeId>> {
        self.template_parameter_bound_for(None, type_id)
    }

    /// Looks up a bound after applying an optional generated-struct owner.
    /// Bounds inside generated methods may mention the factory's enclosing
    /// type arguments and therefore differ between owner instantiations.
    #[must_use]
    pub fn template_parameter_bound_for(
        &self,
        owner: Option<TypeId>,
        type_id: TypeId,
    ) -> Option<Option<TypeId>> {
        let SemanticType::TemplateParameter { declaration, .. } = self.types.get(type_id)? else {
            return None;
        };
        if let Some(owner) = owner
            && let Some(bound) = self
                .specialized_template_parameter_bounds
                .get(&(owner, *declaration))
        {
            return Some(*bound);
        }
        self.template_parameter_bounds.get(declaration).copied()
    }

    #[must_use]
    pub(crate) fn runtime_template_call(&self, call: NodeId) -> Option<&RuntimeTemplateCall> {
        self.runtime_template_calls.get(&call)
    }

    #[must_use]
    pub(crate) fn runtime_member_template_call(
        &self,
        call: NodeId,
    ) -> Option<&RuntimeMemberTemplateCall> {
        self.runtime_member_template_calls.get(&call)
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
    AliasCycle,
    InvalidTypeFactorySignature,
    TypeFactoryNotAllowedHere,
    InvalidTypeFactoryArgumentCount { expected: usize, found: usize },
    ExpandingTypeFactoryInstantiation,
    RecursiveTypeFactoryWithoutNominalResult,
    AssociatedTypeFactoryThroughParameter,
    GeneratedStructOutsideFactory,
    InvalidAssociatedTypeFactoryOwner,
    UnknownAssociatedTypeFactory,
    InvalidRuntimeTemplateDeclaration,
    InvalidTemplateConstraint,
    InvalidInterfaceRequirementType,
    InvalidRuntimeTemplateTypeArgument,
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
            TypeResolutionErrorKind::AliasCycle => write!(
                formatter,
                "type alias forms a direct or indirect cycle at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidTypeFactorySignature => write!(
                formatter,
                "a type factory may currently declare only unconstrained compile-time type parameters at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::TypeFactoryNotAllowedHere => write!(
                formatter,
                "type factories may be declared only at file scope or as receiverless struct members at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidTypeFactoryArgumentCount { expected, found } => write!(
                formatter,
                "type factory expects {expected} type arguments, but {found} were supplied at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::ExpandingTypeFactoryInstantiation => write!(
                formatter,
                "type factory recursively requests an expanding specialization at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::RecursiveTypeFactoryWithoutNominalResult => write!(
                formatter,
                "recursive type factory must establish a generated nominal type at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::AssociatedTypeFactoryThroughParameter => write!(
                formatter,
                "associated type factories cannot be selected through a type parameter at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::GeneratedStructOutsideFactory => write!(
                formatter,
                "generated struct types may appear only as type-factory results at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidAssociatedTypeFactoryOwner => write!(
                formatter,
                "associated type factory requires a statically known concrete struct at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::UnknownAssociatedTypeFactory => write!(
                formatter,
                "unknown associated type factory at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidRuntimeTemplateDeclaration => write!(
                formatter,
                "a runtime template may be declared only as a top-level function or instance method at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidTemplateConstraint => write!(
                formatter,
                "a compile-time type constraint must be an interface or interface intersection at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidInterfaceRequirementType => write!(
                formatter,
                "an interface requirement cannot return `type` at {}..{}",
                self.span.start, self.span.end
            ),
            TypeResolutionErrorKind::InvalidRuntimeTemplateTypeArgument => write!(
                formatter,
                "runtime template argument must be a type at {}..{}",
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
    aliases: HashMap<SymbolId, TypeAliasDeclaration>,
    factories: HashMap<SymbolId, Function>,
    factories_by_id: HashMap<NodeId, Function>,
    runtime_templates: HashMap<SymbolId, Function>,
    associated_factories: HashMap<(NodeId, String), Function>,
    alias_cache: HashMap<SymbolId, TypeId>,
    alias_stack: Vec<SymbolId>,
    factory_cache: HashMap<(NodeId, Vec<TypeId>), TypeId>,
    factory_stack: Vec<(NodeId, Vec<TypeId>)>,
    environments: Vec<HashMap<SymbolId, TypeId>>,
    current_factory: Option<(NodeId, Vec<TypeId>)>,
    current_specialized_owner: Option<TypeId>,
    generated_structs: HashMap<TypeId, GeneratedStructInstantiation>,
    specialized_syntax_types: HashMap<(TypeId, NodeId), TypeId>,
    template_parameter_bounds: HashMap<NodeId, Option<TypeId>>,
    specialized_template_parameter_bounds: HashMap<(TypeId, NodeId), Option<TypeId>>,
    runtime_template_calls: HashMap<NodeId, RuntimeTemplateCall>,
    runtime_member_template_calls: HashMap<NodeId, RuntimeMemberTemplateCall>,
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
            aliases: HashMap::new(),
            factories: HashMap::new(),
            factories_by_id: HashMap::new(),
            runtime_templates: HashMap::new(),
            associated_factories: HashMap::new(),
            alias_cache: HashMap::new(),
            alias_stack: Vec::new(),
            factory_cache: HashMap::new(),
            factory_stack: Vec::new(),
            environments: Vec::new(),
            current_factory: None,
            current_specialized_owner: None,
            generated_structs: HashMap::new(),
            specialized_syntax_types: HashMap::new(),
            template_parameter_bounds: HashMap::new(),
            specialized_template_parameter_bounds: HashMap::new(),
            runtime_template_calls: HashMap::new(),
            runtime_member_template_calls: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn resolve(mut self, program: &Program) -> TypeResolutionResult {
        self.index_type_declarations(program);
        self.predeclare_nominal_types(program);
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }

        if self.errors.is_empty() {
            Ok(TypeResolution {
                types: self.types,
                syntax_types: self.syntax_types,
                declaration_types: self.declaration_types,
                generated_structs: self.generated_structs,
                specialized_syntax_types: self.specialized_syntax_types,
                template_parameter_bounds: self.template_parameter_bounds,
                specialized_template_parameter_bounds: self.specialized_template_parameter_bounds,
                runtime_template_calls: self.runtime_template_calls,
                runtime_member_template_calls: self.runtime_member_template_calls,
            })
        } else {
            Err(self.errors)
        }
    }

    fn index_type_declarations(&mut self, program: &Program) {
        for declaration in &program.declarations {
            match declaration {
                Declaration::TypeAlias(alias) => {
                    let symbol = self
                        .names
                        .symbol_for_declaration(alias.id)
                        .expect("type alias must have a resolved symbol");
                    self.aliases.insert(symbol, alias.clone());
                }
                Declaration::Function(function)
                    if function
                        .return_type
                        .as_ref()
                        .is_some_and(|return_type| matches!(&return_type.kind, TypeKind::ComptimeType)) =>
                {
                    let symbol = self
                        .names
                        .symbol_for_declaration(function.id)
                        .expect("type factory must have a resolved symbol");
                    self.factories.insert(symbol, function.clone());
                    self.factories_by_id.insert(function.id, function.clone());
                }
                Declaration::Function(function) if self.is_runtime_template(function) => {
                    let symbol = self
                        .names
                        .symbol_for_declaration(function.id)
                        .expect("runtime template must have a resolved symbol");
                    self.runtime_templates.insert(symbol, function.clone());
                }
                Declaration::Struct(structure) => {
                    for member in &structure.members {
                        if let StructMember::Function(function) = member
                            && self.is_type_factory(function)
                        {
                            let name = self
                                .module
                                .text(function.name)
                                .expect("member name belongs to the source module")
                                .to_string();
                            self.associated_factories
                                .insert((structure.id, name), function.clone());
                            self.factories_by_id.insert(function.id, function.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        for factory in self.factories_by_id.values().cloned().collect::<Vec<_>>() {
            if let Some(type_syntax) = type_factory_result(&factory)
                && let TypeKind::GeneratedStruct { members } = &type_syntax.kind
            {
                self.index_generated_factories(type_syntax.id, members);
            }
        }
    }

    fn index_generated_factories(
        &mut self,
        template: NodeId,
        members: &[StructMember],
    ) {
        for member in members {
            if let StructMember::Function(function) = member
                && self.is_type_factory(function)
            {
                let name = self
                    .module
                    .text(function.name)
                    .expect("member name belongs to the source module")
                    .to_string();
                self.associated_factories
                    .insert((template, name), function.clone());
                self.factories_by_id.insert(function.id, function.clone());
                if let Some(type_syntax) = type_factory_result(function)
                    && let TypeKind::GeneratedStruct { members } = &type_syntax.kind
                {
                    self.index_generated_factories(type_syntax.id, members);
                }
            }
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
                Declaration::Function(_) | Declaration::TypeAlias(_) => continue,
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
            Declaration::Function(function) if self.is_type_factory(function) => {
                self.validate_type_factory_signature(function);
            }
            Declaration::Function(function) => self.visit_function(function),
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    match member {
                        StructMember::Field(field) => {
                            self.resolve_type(&field.type_annotation);
                        }
                        StructMember::Function(function) if self.is_type_factory(function) => {
                            self.validate_type_factory_signature(function);
                        }
                        StructMember::Function(function) => {
                            if self.is_runtime_template(function)
                                && !function.parameters.iter().any(|parameter| {
                                    matches!(&parameter.kind, FunctionParameterKind::Receiver { .. })
                                })
                            {
                                self.error(
                                    TypeResolutionErrorKind::InvalidRuntimeTemplateDeclaration,
                                    function.name,
                                );
                            }
                            self.visit_function(function);
                        }
                    }
                }
            }
            Declaration::Interface(interface) => {
                for requirement in &interface.requirements {
                    self.visit_interface_requirement(requirement);
                }
            }
            Declaration::TypeAlias(alias) => {
                let symbol = self
                    .names
                    .symbol_for_declaration(alias.id)
                    .expect("type alias must have a resolved symbol");
                let _ = self.resolve_alias(symbol, alias.span);
            }
        }
    }

    fn is_type_factory(&self, function: &Function) -> bool {
        function
            .return_type
            .as_ref()
            .is_some_and(|return_type| matches!(&return_type.kind, TypeKind::ComptimeType))
    }

    fn is_runtime_template(&self, function: &Function) -> bool {
        !self.is_type_factory(function)
            && function.parameters.iter().any(|parameter| {
                matches!(&parameter.kind, FunctionParameterKind::Comptime { .. })
            })
    }

    fn validate_type_factory_signature(&mut self, function: &Function) {
        let valid = function.where_clause.is_none()
            && function.parameters.iter().all(|parameter| {
                matches!(
                    &parameter.kind,
                    FunctionParameterKind::Comptime {
                        constraint: crate::ast::ComptimeParameterConstraint::Type { .. },
                        ..
                    }
                )
            });
        if !valid {
            self.error(
                TypeResolutionErrorKind::InvalidTypeFactorySignature,
                function.name,
            );
        }
    }

    fn visit_interface_requirement(&mut self, requirement: &InterfaceMethodRequirement) {
        self.visit_parameters(&requirement.parameters);
        if let Some(return_type) = &requirement.return_type {
            if matches!(&return_type.kind, TypeKind::ComptimeType) {
                self.error(
                    TypeResolutionErrorKind::InvalidInterfaceRequirementType,
                    return_type.span,
                );
            }
            self.resolve_type(return_type);
        }
    }

    fn visit_function(&mut self, function: &Function) {
        if self.is_runtime_template(function) {
            self.visit_runtime_template(function);
            return;
        }
        self.visit_parameters(&function.parameters);
        if let Some(return_type) = &function.return_type {
            self.resolve_type(return_type);
        }
        self.visit_block(&function.body);
    }

    /// Resolves an unspecialized runtime template with a distinct symbolic
    /// semantic type for each compile-time parameter. Bounds are retained as
    /// separate metadata: annotations still mean exactly `T`, while expression
    /// checking may expose only the methods promised by `T`'s interface.
    fn visit_runtime_template(&mut self, function: &Function) {
        let mut environment = HashMap::new();
        for parameter in &function.parameters {
            let FunctionParameterKind::Comptime { .. } = &parameter.kind else {
                continue;
            };
            let symbol = self
                .names
                .symbol_for_declaration(parameter.id)
                .expect("compile-time parameter must have a resolved symbol");
            let symbolic = self
                .types
                .template_parameter(parameter.id, AccessCapability::Const);
            environment.insert(symbol, symbolic);
            if let Some(owner) = self.current_specialized_owner {
                self.specialized_template_parameter_bounds
                    .insert((owner, parameter.id), None);
            } else {
                self.template_parameter_bounds.insert(parameter.id, None);
            }
        }
        self.environments.push(environment);

        for parameter in &function.parameters {
            let FunctionParameterKind::Comptime { constraint, .. } = &parameter.kind else {
                continue;
            };
            let bound = match constraint {
                ComptimeParameterConstraint::Type { .. } => None,
                ComptimeParameterConstraint::Interface(interface) => {
                    Some(self.resolve_type(interface))
                }
            };
            self.record_template_bound(parameter.id, bound, parameter.span);
        }

        if let Some(where_clause) = &function.where_clause {
            for constraint in &where_clause.constraints {
                let bound = match &constraint.interface {
                    InterfaceConstraint::Named(interface) => self.resolve_type(interface),
                    InterfaceConstraint::Anonymous { requirements, .. } => {
                        let interface = self
                            .types
                            .interface(constraint.id, AccessCapability::Const);
                        self.declaration_types.insert(constraint.id, interface);
                        for requirement in requirements {
                            self.visit_interface_requirement(requirement);
                        }
                        interface
                    }
                };
                let parameter = self
                    .names
                    .symbol_for_reference(constraint.id)
                    .expect("where constraint must resolve its template parameter");
                let symbolic = self
                    .environments
                    .last()
                    .and_then(|environment| environment.get(&parameter))
                    .copied()
                    .expect("where constraint must target a compile-time parameter");
                self.validate_template_bound(symbolic, Some(bound), constraint.span);
            }
        }

        self.visit_parameters(&function.parameters);
        if let Some(return_type) = &function.return_type {
            self.resolve_type(return_type);
        }
        self.visit_block(&function.body);
        self.environments.pop();
    }

    fn record_template_bound(
        &mut self,
        parameter: NodeId,
        bound: Option<TypeId>,
        span: Span,
    ) {
        let symbol = self
            .names
            .symbol_for_declaration(parameter)
            .expect("compile-time parameter must have a resolved symbol");
        let symbolic = self
            .environments
            .last()
            .and_then(|environment| environment.get(&symbol))
            .copied()
            .expect("runtime template environment must contain its parameter");
        self.validate_template_bound(symbolic, bound, span);
    }

    fn validate_template_bound(
        &mut self,
        symbolic: TypeId,
        bound: Option<TypeId>,
        span: Span,
    ) {
        let declaration = match self.types.get(symbolic) {
            Some(SemanticType::TemplateParameter { declaration, .. }) => *declaration,
            _ => unreachable!("runtime template environment contains only parameter types"),
        };
        if let Some(bound) = bound
            && !self.is_plain_interface_type(bound)
        {
            self.error(TypeResolutionErrorKind::InvalidTemplateConstraint, span);
            self.insert_template_bound(declaration, None);
        } else {
            self.insert_template_bound(declaration, bound);
        }
    }

    fn insert_template_bound(&mut self, declaration: NodeId, bound: Option<TypeId>) {
        if let Some(owner) = self.current_specialized_owner {
            self.specialized_template_parameter_bounds
                .insert((owner, declaration), bound);
        } else {
            self.template_parameter_bounds.insert(declaration, bound);
        }
    }

    fn visit_parameters(&mut self, parameters: &[FunctionParameter]) {
        for parameter in parameters {
            match &parameter.kind {
                FunctionParameterKind::Named {
                    type_annotation, ..
                } => {
                    self.resolve_type(type_annotation);
                }
                FunctionParameterKind::Comptime { .. }
                | FunctionParameterKind::Receiver { .. } => {}
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
            StatementKind::Function(function) => {
                if self.is_type_factory(function) {
                    self.error(
                        TypeResolutionErrorKind::TypeFactoryNotAllowedHere,
                        function.name,
                    );
                } else {
                    if self.is_runtime_template(function) {
                        self.error(
                            TypeResolutionErrorKind::InvalidRuntimeTemplateDeclaration,
                            function.name,
                        );
                    }
                    self.visit_function(function);
                }
            }
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
            ExpressionKind::TypeValue(type_syntax) => {
                self.resolve_type(type_syntax);
            }
            ExpressionKind::FormattedString { parts } => {
                for part in parts {
                    if let FormattedStringPart::Interpolation { value, .. } = part {
                        self.visit_expression(value);
                    }
                }
            }
            ExpressionKind::Group(inner)
            | ExpressionKind::GcAllocate(inner)
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::Unary { operand: inner, .. } => self.visit_expression(inner),
            ExpressionKind::Tuple { elements } => {
                for element in elements {
                    self.visit_expression(element);
                }
            }
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
            ExpressionKind::StructConstruction { owner, fields } => {
                self.resolve_type(owner);
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
                        AnonymousStructMember::Method(function) => {
                            if self.is_type_factory(function) {
                                self.error(
                                    TypeResolutionErrorKind::TypeFactoryNotAllowedHere,
                                    function.name,
                                );
                            } else {
                                self.visit_function(function);
                            }
                        }
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.visit_expression(callee);
                let template = self
                    .names
                    .symbol_for_reference(callee.id)
                    .and_then(|symbol| self.runtime_templates.get(&symbol))
                    .cloned();
                if let Some(template) = template {
                    let comptime_argument_count = template
                        .parameters
                        .iter()
                        .take_while(|parameter| {
                            matches!(&parameter.kind, FunctionParameterKind::Comptime { .. })
                        })
                        .count();
                    let mut type_arguments = Vec::new();
                    for argument in arguments.iter().take(comptime_argument_count) {
                        if let Some(type_syntax) = expression_as_type_syntax(argument) {
                            type_arguments.push(self.resolve_type(&type_syntax));
                        } else {
                            self.error(
                                TypeResolutionErrorKind::InvalidRuntimeTemplateTypeArgument,
                                argument.span,
                            );
                            type_arguments.push(self.types.recovery());
                            self.visit_expression(argument);
                        }
                    }
                    for argument in arguments.iter().skip(comptime_argument_count) {
                        self.visit_expression(argument);
                    }
                    self.runtime_template_calls.insert(
                        expression.id,
                        RuntimeTemplateCall {
                            declaration: template.id,
                            type_arguments,
                            comptime_argument_count,
                        },
                    );
                } else if matches!(
                    &callee.kind,
                    ExpressionKind::MemberAccess { .. }
                        | ExpressionKind::AssociatedAccess { .. }
                ) {
                    let mut type_arguments = Vec::new();
                    let mut comptime_argument_count = 0;
                    for argument in arguments {
                        let Some(type_syntax) = expression_as_type_syntax(argument) else {
                            break;
                        };
                        if !self.was_resolved_as_type(&type_syntax) {
                            break;
                        }
                        type_arguments.push(self.resolve_type(&type_syntax));
                        comptime_argument_count += 1;
                    }
                    for argument in arguments.iter().skip(comptime_argument_count) {
                        self.visit_expression(argument);
                    }
                    if comptime_argument_count != 0 {
                        self.runtime_member_template_calls.insert(
                            expression.id,
                            RuntimeMemberTemplateCall {
                                type_arguments,
                                comptime_argument_count,
                            },
                        );
                    }
                } else {
                    for argument in arguments {
                        self.visit_expression(argument);
                    }
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

    /// Name resolution provisionally places leading member-call arguments in
    /// the type namespace. This recognizes those nodes without guessing from
    /// spelling, allowing receiver typing to select the actual method later.
    fn was_resolved_as_type(&self, type_syntax: &TypeSyntax) -> bool {
        match &type_syntax.kind {
            TypeKind::ComptimeType | TypeKind::Primitive(_) | TypeKind::Builtin { .. } => true,
            TypeKind::Named { .. } => self
                .names
                .symbol_for_reference(type_syntax.id)
                .and_then(|symbol| self.names.symbols().symbol(symbol))
                .is_some_and(|symbol| {
                    matches!(
                        symbol.kind,
                        SymbolKind::TypeFactory
                            | SymbolKind::ComptimeParameter
                            | SymbolKind::Struct
                            | SymbolKind::Interface
                            | SymbolKind::TypeAlias
                    )
                }),
            TypeKind::Associated { owner, .. } => self.was_resolved_as_type(owner),
            TypeKind::Mutable(inner) | TypeKind::Gc(inner) | TypeKind::Group(inner) => {
                self.was_resolved_as_type(inner)
            }
            TypeKind::Callable { .. }
            | TypeKind::Tuple { .. }
            | TypeKind::Intersection { .. }
            | TypeKind::Union { .. }
            | TypeKind::GeneratedStruct { .. } => true,
        }
    }

    fn resolve_type(&mut self, syntax: &TypeSyntax) -> TypeId {
        let resolved = match &syntax.kind {
            TypeKind::ComptimeType => self.types.recovery(),
            TypeKind::Primitive(primitive) => {
                self.types.primitive(*primitive, AccessCapability::Const)
            }
            TypeKind::Builtin { builtin, arguments } => {
                self.resolve_builtin(*builtin, arguments, syntax.span)
            }
            TypeKind::Named { arguments, .. } => self.resolve_named(syntax, arguments),
            TypeKind::GeneratedStruct { members } => {
                self.resolve_generated_struct(syntax, members)
            }
            TypeKind::Associated {
                owner,
                member,
                arguments,
            } => self.resolve_associated_factory(owner, *member, arguments, syntax.span),
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
            TypeKind::Tuple { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.resolve_type(element))
                    .collect();
                self.types.tuple(elements, AccessCapability::Const)
            }
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
        if let Some(owner) = self.current_specialized_owner {
            self.specialized_syntax_types
                .insert((owner, syntax.id), resolved);
        }
        resolved
    }

    fn resolve_named(&mut self, syntax: &TypeSyntax, arguments: &[TypeSyntax]) -> TypeId {
        let resolved_arguments: Vec<_> = arguments
            .iter()
            .map(|argument| self.resolve_type(argument))
            .collect();

        let symbol = self
            .names
            .symbol_for_reference(syntax.id)
            .expect("named type syntax must have name-resolution metadata");
        match self
            .names
            .symbols()
            .symbol(symbol)
            .expect("resolved type symbol must exist")
            .kind
        {
            SymbolKind::TypeAlias => {
                if !arguments.is_empty() {
                    self.error(
                        TypeResolutionErrorKind::TypeArgumentsNotSupported {
                            found: arguments.len(),
                        },
                        syntax.span,
                    );
                    self.types.recovery()
                } else {
                    self.resolve_alias(symbol, syntax.span)
                }
            }
            SymbolKind::ComptimeParameter => {
                if !arguments.is_empty() {
                    self.error(
                        TypeResolutionErrorKind::TypeArgumentsNotSupported {
                            found: arguments.len(),
                        },
                        syntax.span,
                    );
                    self.types.recovery()
                } else {
                    self.environments
                        .iter()
                        .rev()
                        .find_map(|environment| environment.get(&symbol).copied())
                        .unwrap_or_else(|| self.types.recovery())
                }
            }
            SymbolKind::TypeFactory => {
                let factory = self
                    .factories
                    .get(&symbol)
                    .expect("resolved type-factory symbol must be indexed")
                    .clone();
                self.instantiate_factory(
                    &factory,
                    resolved_arguments,
                    HashMap::new(),
                    Vec::new(),
                    syntax.span,
                )
            }
            SymbolKind::Struct | SymbolKind::Interface => {
                if !arguments.is_empty() {
                    self.error(
                        TypeResolutionErrorKind::TypeArgumentsNotSupported {
                            found: arguments.len(),
                        },
                        syntax.span,
                    );
                    self.types.recovery()
                } else {
                    *self
                        .symbol_types
                        .get(&symbol)
                        .expect("nominal type declaration must have been predeclared")
                }
            }
            _ => unreachable!("name resolution only records type-context symbols here"),
        }
    }

    fn resolve_alias(&mut self, symbol: SymbolId, use_span: Span) -> TypeId {
        if let Some(type_id) = self.alias_cache.get(&symbol).copied() {
            return type_id;
        }
        if self.alias_stack.contains(&symbol) {
            self.error(TypeResolutionErrorKind::AliasCycle, use_span);
            return self.types.recovery();
        }
        let alias = self
            .aliases
            .get(&symbol)
            .expect("resolved alias symbol must be indexed")
            .clone();
        self.alias_stack.push(symbol);
        let resolved = self.resolve_type(&alias.target);
        self.alias_stack.pop();
        self.alias_cache.insert(symbol, resolved);
        resolved
    }

    fn instantiate_factory(
        &mut self,
        factory: &Function,
        arguments: Vec<TypeId>,
        mut inherited_environment: HashMap<SymbolId, TypeId>,
        mut identity_arguments: Vec<TypeId>,
        span: Span,
    ) -> TypeId {
        let parameters: Vec<_> = factory
            .parameters
            .iter()
            .filter(|parameter| matches!(&parameter.kind, FunctionParameterKind::Comptime { .. }))
            .collect();
        if parameters.len() != arguments.len() {
            self.error(
                TypeResolutionErrorKind::InvalidTypeFactoryArgumentCount {
                    expected: parameters.len(),
                    found: arguments.len(),
                },
                span,
            );
            return self.types.recovery();
        }
        identity_arguments.extend(arguments.iter().copied());
        let key = (factory.id, identity_arguments.clone());
        if let Some(type_id) = self.factory_cache.get(&key).copied() {
            return type_id;
        }
        if self.factory_stack.contains(&key) {
            self.error(
                TypeResolutionErrorKind::RecursiveTypeFactoryWithoutNominalResult,
                span,
            );
            return self.types.recovery();
        }
        if self
            .factory_stack
            .iter()
            .any(|(active, active_arguments)| {
                *active == factory.id && *active_arguments != identity_arguments
            })
        {
            self.error(
                TypeResolutionErrorKind::ExpandingTypeFactoryInstantiation,
                span,
            );
            return self.types.recovery();
        }
        for (parameter, argument) in parameters.iter().zip(&arguments) {
            let symbol = self
                .names
                .symbol_for_declaration(parameter.id)
                .expect("compile-time parameter must have a resolved symbol");
            inherited_environment.insert(symbol, *argument);
        }
        let Some(result) = type_factory_result(factory) else {
            self.error(TypeResolutionErrorKind::InvalidTypeFactorySignature, factory.name);
            return self.types.recovery();
        };

        self.factory_stack.push(key.clone());
        self.environments.push(inherited_environment);
        let previous_factory = self
            .current_factory
            .replace((factory.id, identity_arguments.clone()));

        let resolved = if matches!(&result.kind, TypeKind::GeneratedStruct { .. }) {
            let placeholder = self.types.generated_struct(
                result.id,
                identity_arguments.clone(),
                AccessCapability::Const,
            );
            self.factory_cache.insert(key.clone(), placeholder);
            self.resolve_type(result)
        } else {
            self.resolve_type(result)
        };

        self.current_factory = previous_factory;
        self.environments.pop();
        self.factory_stack.pop();
        self.factory_cache.insert(key, resolved);
        resolved
    }

    fn resolve_generated_struct(
        &mut self,
        syntax: &TypeSyntax,
        members: &[StructMember],
    ) -> TypeId {
        let Some((factory, arguments)) = self.current_factory.clone() else {
            self.error(
                TypeResolutionErrorKind::GeneratedStructOutsideFactory,
                syntax.span,
            );
            return self.types.recovery();
        };
        let type_id = self.types.generated_struct(
            syntax.id,
            arguments.clone(),
            AccessCapability::Const,
        );
        if self.generated_structs.contains_key(&type_id) {
            return type_id;
        }
        self.generated_structs.insert(
            type_id,
            GeneratedStructInstantiation {
                type_id,
                template: syntax.id,
                factory,
                arguments,
                field_types: HashMap::new(),
                substitutions: self.environments.last().cloned().unwrap_or_default(),
            },
        );

        let mut field_types = HashMap::new();
        for member in members {
            match member {
                StructMember::Field(field) => {
                    let resolved = self.resolve_type(&field.type_annotation);
                    self.specialized_syntax_types
                        .insert((type_id, field.type_annotation.id), resolved);
                    field_types.insert(field.id, resolved);
                }
                StructMember::Function(function) if self.is_type_factory(function) => {
                    self.validate_type_factory_signature(function);
                }
                StructMember::Function(function) => {
                    if self.is_runtime_template(function)
                        && !function.parameters.iter().any(|parameter| {
                            matches!(&parameter.kind, FunctionParameterKind::Receiver { .. })
                        })
                    {
                        self.error(
                            TypeResolutionErrorKind::InvalidRuntimeTemplateDeclaration,
                            function.name,
                        );
                    }
                    self.resolve_specialized_function(type_id, function);
                }
            }
        }
        self.generated_structs
            .get_mut(&type_id)
            .expect("generated struct placeholder must remain installed")
            .field_types = field_types;
        type_id
    }

    fn resolve_specialized_function(&mut self, owner: TypeId, function: &Function) {
        let previous = self.current_specialized_owner.replace(owner);
        if self.is_runtime_template(function) {
            self.visit_runtime_template(function);
            self.current_specialized_owner = previous;
            return;
        }
        for parameter in &function.parameters {
            if let FunctionParameterKind::Named { type_annotation, .. } = &parameter.kind {
                let resolved = self.resolve_type(type_annotation);
                self.specialized_syntax_types
                    .insert((owner, type_annotation.id), resolved);
            }
        }
        if let Some(return_type) = &function.return_type {
            let resolved = self.resolve_type(return_type);
            self.specialized_syntax_types
                .insert((owner, return_type.id), resolved);
        }
        self.visit_block(&function.body);
        self.current_specialized_owner = previous;
    }

    fn resolve_associated_factory(
        &mut self,
        owner: &TypeSyntax,
        member: Span,
        arguments: &[TypeSyntax],
        span: Span,
    ) -> TypeId {
        let selected_through_parameter = if let TypeKind::Named { .. } = &owner.kind {
            let symbol = self
                .names
                .symbol_for_reference(owner.id)
                .expect("associated owner must have resolved type metadata");
            self
                .names
                .symbols()
                .symbol(symbol)
                .is_some_and(|symbol| symbol.kind == SymbolKind::ComptimeParameter)
        } else {
            false
        };
        let owner_type = self.resolve_type(owner);
        let resolved_arguments = arguments
            .iter()
            .map(|argument| self.resolve_type(argument))
            .collect();
        if selected_through_parameter {
            self.error(
                TypeResolutionErrorKind::AssociatedTypeFactoryThroughParameter,
                owner.span,
            );
            return self.types.recovery();
        }
        let (owner_key, inherited) = match self.types.get(owner_type).cloned() {
            Some(SemanticType::NamedStruct { declaration, .. }) => {
                (declaration, HashMap::new())
            }
            Some(SemanticType::GeneratedStruct {
                template,
                ..
            }) => {
                let environment = self
                    .generated_structs
                    .get(&owner_type)
                    .map(|instance| instance.substitutions.clone())
                    .unwrap_or_default();
                (template, environment)
            }
            _ => {
                self.error(
                    TypeResolutionErrorKind::InvalidAssociatedTypeFactoryOwner,
                    owner.span,
                );
                return self.types.recovery();
            }
        };
        let name = self
            .module
            .text(member)
            .expect("associated member belongs to the source module")
            .to_string();
        let Some(factory) = self.associated_factories.get(&(owner_key, name)).cloned() else {
            self.error(TypeResolutionErrorKind::UnknownAssociatedTypeFactory, member);
            return self.types.recovery();
        };
        self.instantiate_factory(
            &factory,
            resolved_arguments,
            inherited,
            vec![owner_type],
            span,
        )
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

fn type_factory_result(function: &Function) -> Option<&TypeSyntax> {
    if let Some(value) = &function.body.value
        && let ExpressionKind::TypeValue(type_syntax) = &value.kind
    {
        return Some(type_syntax);
    }
    if function.body.statements.len() == 1
        && let StatementKind::Return(Some(value)) = &function.body.statements[0].kind
        && let ExpressionKind::TypeValue(type_syntax) = &value.kind
    {
        return Some(type_syntax);
    }
    None
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
            "    table: Map(string, Vector(int)),\n",
            "    boxed_none: Queue(&none),\n",
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
    fn resolves_tuple_types_by_ordered_structural_identity() {
        let (program, resolution) = resolve(concat!(
            "fn inspect(",
            "    first: (int, string),",
            "    second: (int, string,),",
            "    reversed: (string, int),",
            "    singleton: (int,),",
            ") {}",
            "fn main() {}",
        ));
        let inspect = top_level_function(&program, 0);
        let resolved = |index| {
            resolution
                .type_for_syntax(named_parameter_type(inspect, index).id)
                .expect("tuple parameter type should resolve")
        };

        assert_eq!(resolved(0), resolved(1));
        assert_ne!(resolved(0), resolved(2));
        assert_ne!(resolved(0), resolved(3));
        assert!(matches!(
            resolution.types().get(resolved(0)),
            Some(SemanticType::Tuple { elements, .. }) if elements.len() == 2
        ));
        assert!(matches!(
            resolution.types().get(resolved(3)),
            Some(SemanticType::Tuple { elements, .. }) if elements.len() == 1
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

        // This program contains sixteen TypeSyntax nodes, all deliberately in
        // distinct annotation-bearing AST locations. Nested type syntax would
        // add further entries to this same map.
        assert_eq!(resolution.syntax_types().len(), 16);
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
            "    generic: User(int),\n",
            "    messages: Queue(int | none),\n",
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
        let (module, mut program) = parse("fn main(value: Queue(int)) {}");
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

        let (module, mut program) = parse("fn main(value: Error(int)) {}");
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

    #[test]
    fn resolves_and_caches_generated_type_factory_applications() {
        let source = concat!(
            "fn Box(comptime T: type) -> type { struct { inner: T, } }\n",
            "type First = Box(int);\n",
            "type Second = Box(int);\n",
            "type Different = Box(string);\n",
            "fn main(left: First, right: Second, other: Different) {}",
        );
        let (module, program) = parse(source);
        let names = resolve_program(&module, &program).expect("factory names should resolve");
        let resolution =
            resolve_types(&module, &program, &names).expect("factory types should resolve");
        assert_eq!(resolution.generated_structs().len(), 2);
        let Declaration::Function(main) = &program.declarations[4] else {
            panic!("expected main");
        };
        assert_eq!(
            resolution.type_for_syntax(named_parameter_type(main, 0).id),
            resolution.type_for_syntax(named_parameter_type(main, 1).id),
        );
        assert_ne!(
            resolution.type_for_syntax(named_parameter_type(main, 0).id),
            resolution.type_for_syntax(named_parameter_type(main, 2).id),
        );
    }

    #[test]
    fn resolves_forward_aliases_and_concrete_associated_type_factories() {
        let source = concat!(
            "type Forward = Pair;\n",
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn Pair(comptime U: type) -> type { struct { left: T, right: U, } }\n",
            "    }\n",
            "}\n",
            "type Pair = Box(int)::Pair(string);\n",
            "fn use_pair(value: Forward) {}\n",
            "fn main() {}",
        );
        let (program, resolution) = resolve(source);
        assert_eq!(resolution.generated_structs().len(), 2);

        let Declaration::Function(use_pair) = &program.declarations[3] else {
            panic!("expected use_pair")
        };
        let forward = resolution
            .type_for_syntax(named_parameter_type(use_pair, 0).id)
            .expect("forward alias use must have a type");
        let pair = resolution
            .generated_structs()
            .get(&forward)
            .expect("forward alias must preserve the generated pair identity");
        assert!(pair.field_types.values().any(|field| matches!(
            resolution.types().get(*field),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        )));
        assert!(pair.field_types.values().any(|field| matches!(
            resolution.types().get(*field),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                ..
            })
        )));
    }

    #[test]
    fn caches_exact_recursive_factory_applications_and_rejects_parameter_selection() {
        let (program, resolution) = resolve(concat!(
            "fn Node(comptime T: type) -> type { struct { value: T, next: &Node(T), } }\n",
            "type IntNode = Node(int);\n",
            "fn main() {}",
        ));
        assert_eq!(resolution.generated_structs().len(), 1);
        let Declaration::TypeAlias(alias) = &program.declarations[1] else {
            panic!("expected IntNode alias")
        };
        let node = resolution
            .type_for_syntax(alias.target.id)
            .expect("recursive application must resolve");
        let instance = resolution.generated_structs()[&node].clone();
        assert!(instance.field_types.values().any(|field| matches!(
            resolution.types().get(*field),
            Some(SemanticType::Gc { target, .. }) if *target == node
        )));

        let (module, program) = parse(concat!(
            "fn Invalid(comptime T: type) -> type { T::Member(int) }\n",
            "type Result = Invalid(int);\n",
            "fn main() {}",
        ));
        let names = resolve_program(&module, &program).expect("test names should resolve");
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if errors.iter().any(|error| {
                error.kind == TypeResolutionErrorKind::AssociatedTypeFactoryThroughParameter
            })
        ));
    }

    #[test]
    fn rejects_local_type_factories() {
        let (module, program) = parse(concat!(
            "fn main() {\n",
            "    fn Local(comptime T: type) -> type { T }\n",
            "}\n",
        ));
        let names = resolve_program(&module, &program).expect("test names should resolve");
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if errors.iter().any(|error| {
                error.kind == TypeResolutionErrorKind::TypeFactoryNotAllowedHere
            })
        ));
    }

    #[test]
    fn reports_alias_cycles_and_expanding_factory_instantiation() {
        let (module, program) = parse(concat!(
            "type A = B;\n",
            "type B = A;\n",
            "fn main() {}",
        ));
        let names = resolve_program(&module, &program).expect("cyclic aliases still resolve names");
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if errors.iter().any(|error| error.kind == TypeResolutionErrorKind::AliasCycle)
        ));

        let (module, program) = parse(concat!(
            "fn Bad(comptime T: type) -> type {\n",
            "    struct { next: Bad(Vector(T)), }\n",
            "}\n",
            "type Expanded = Bad(int);\n",
            "fn main() {}",
        ));
        let names = resolve_program(&module, &program).expect("factory recursion names should resolve");
        assert!(matches!(
            resolve_types(&module, &program, &names),
            Err(errors) if errors.iter().any(|error| {
                error.kind == TypeResolutionErrorKind::ExpandingTypeFactoryInstantiation
            })
        ));
    }

    #[test]
    fn preserves_symbolic_template_parameters_and_interface_bounds() {
        let (program, resolution) = resolve(concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface Writer { fn write(self, value: int); }\n",
            "fn inspect(comptime T: type, value: T) -> T\n",
            "where T: Reader & Writer, { value }\n",
            "fn main() {}\n",
        ));
        let inspect = top_level_function(&program, 2);
        let value = inspect
            .parameters
            .iter()
            .find_map(|parameter| match &parameter.kind {
                FunctionParameterKind::Named { type_annotation, .. } => Some(type_annotation),
                _ => None,
            })
            .expect("template must have a runtime parameter");
        let symbolic = resolution
            .type_for_syntax(value.id)
            .expect("runtime use of T must resolve");
        assert!(matches!(
            resolution.types().get(symbolic),
            Some(SemanticType::TemplateParameter { .. })
        ));
        let bound = resolution
            .template_parameter_bound(symbolic)
            .flatten()
            .expect("where clause must record a bound");
        assert!(matches!(
            resolution.types().get(bound),
            Some(SemanticType::Intersection { members, .. }) if members.len() == 2
        ));
        assert_eq!(
            resolution.type_for_syntax(inspect.return_type.as_ref().unwrap().id),
            Some(symbolic)
        );
    }

    #[test]
    fn rejects_runtime_templates_outside_supported_declarations() {
        let (module, program) = parse(concat!(
            "struct Item { fn associated(comptime T: type, value: T) {} }\n",
            "fn main() { fn local(comptime T: type, value: T) {} }\n",
        ));
        let names = resolve_program(&module, &program).expect("template names should resolve");
        let errors = resolve_types(&module, &program, &names)
            .expect_err("associated and local runtime templates are deferred");
        assert_eq!(
            errors
                .iter()
                .filter(|error| {
                    error.kind == TypeResolutionErrorKind::InvalidRuntimeTemplateDeclaration
                })
                .count(),
            2
        );
    }

    #[test]
    fn rejects_non_interface_bounds_and_type_returning_requirements() {
        let (module, program) = parse(concat!(
            "struct Item {}\n",
            "type ItemAlias = Item;\n",
            "interface Invalid { fn make(self) -> type; }\n",
            "fn inspect(comptime T: type, value: T) where T: ItemAlias {}\n",
            "fn main() {}\n",
        ));
        let names = resolve_program(&module, &program).expect("constraint names should resolve");
        let errors = resolve_types(&module, &program, &names)
            .expect_err("bounds and requirements must remain interface-only and runtime-only");
        assert!(errors.iter().any(|error| {
            error.kind == TypeResolutionErrorKind::InvalidTemplateConstraint
        }));
        assert!(errors.iter().any(|error| {
            error.kind == TypeResolutionErrorKind::InvalidInterfaceRequirementType
        }));
    }
}
