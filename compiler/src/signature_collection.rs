//! Collects declaration members and callable signatures before body checking.
//!
//! This pass consumes resolved source types, records every callable header whose
//! shape is explicit in the AST, validates declaration-level member namespaces,
//! and interns owner-independent method identities. It deliberately does not
//! type-check executable expressions.

use std::{collections::HashMap, fmt};

use crate::{
    ast::{
        AnonymousStructMember, Block, BuiltinType, ConditionalElse, Declaration, Expression,
        ExpressionKind, Function, FunctionParameter, FunctionParameterKind,
        InterfaceMethodRequirement, NodeId, PrimitiveType, Program, ReceiverStorage, Statement,
        StatementKind, StructMember,
    },
    context_resolution::{CallableKind, ContextResolution},
    name_resolution::NameResolution,
    semantic_types::{AccessCapability, TypeId, TypeStore},
    source::{SourceModule, Span},
    symbol_table::SymbolId,
    type_resolution::TypeResolution,
};

/// The receiver portion of an instance-method signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiverSignature {
    pub storage: ReceiverStorage,
    pub capability: AccessCapability,
}

/// The declared shape of a source callable, excluding its source parameter names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallableSignature {
    pub receiver: Option<ReceiverSignature>,
    pub parameters: Vec<TypeId>,
    pub return_type: TypeId,
}

/// A program-local, collision-free identity for one canonical method signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MethodId(usize);

impl MethodId {
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// The owner-independent components used for structural method matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodSignature {
    pub name: String,
    pub receiver: ReceiverSignature,
    pub parameters: Vec<TypeId>,
    pub return_type: TypeId,
}

/// One field recorded in a source struct's member namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSignature {
    pub declaration: NodeId,
    /// Anonymous fields without annotations remain pending until expression checking.
    pub type_id: Option<TypeId>,
}

/// The semantic category of one struct member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructMemberSignatureKind {
    Field(FieldSignature),
    Method {
        declaration: NodeId,
        method_id: MethodId,
    },
    AssociatedFunction {
        declaration: NodeId,
    },
}

/// One entry in a struct's shared field/function namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructMemberSignature {
    pub kind: StructMemberSignatureKind,
    pub span: Span,
}

/// The collected members for one named or anonymous struct type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructSignature {
    pub type_id: TypeId,
    members: HashMap<String, StructMemberSignature>,
}

impl StructSignature {
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&StructMemberSignature> {
        self.members.get(name)
    }

    #[must_use]
    pub const fn members(&self) -> &HashMap<String, StructMemberSignature> {
        &self.members
    }
}

/// One structurally matched interface requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceRequirementSignature {
    pub declaration: NodeId,
    pub method_id: MethodId,
    pub span: Span,
}

/// The requirements declared by one interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceSignature {
    pub type_id: TypeId,
    requirements: HashMap<String, InterfaceRequirementSignature>,
}

impl InterfaceSignature {
    #[must_use]
    pub fn requirement(&self, name: &str) -> Option<&InterfaceRequirementSignature> {
        self.requirements.get(name)
    }

    #[must_use]
    pub const fn requirements(&self) -> &HashMap<String, InterfaceRequirementSignature> {
        &self.requirements
    }
}

/// A symbolic type used by compiler-known generic signature templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinTypeTemplate {
    Primitive {
        primitive: PrimitiveType,
        capability: AccessCapability,
    },
    Parameter {
        index: usize,
        /// `None` preserves the substituted type exactly. `Some` applies a
        /// contextual capability, as required by immutable `Error.value`.
        capability: Option<AccessCapability>,
    },
    Builtin {
        builtin: BuiltinType,
        arguments: Vec<BuiltinTypeTemplate>,
        capability: AccessCapability,
    },
    Union {
        members: Vec<BuiltinTypeTemplate>,
        capability: AccessCapability,
    },
    Divergence,
}

impl BuiltinTypeTemplate {
    /// Instantiates this template using the supplied `T`/`K`/`V` substitutions.
    pub fn instantiate(
        &self,
        substitutions: &[TypeId],
        types: &mut TypeStore,
    ) -> Option<TypeId> {
        match self {
            Self::Primitive {
                primitive,
                capability,
            } => Some(types.primitive(*primitive, *capability)),
            Self::Parameter { index, capability } => {
                let substitution = *substitutions.get(*index)?;
                match capability {
                    Some(capability) => types.with_capability(substitution, *capability),
                    None => Some(substitution),
                }
            }
            Self::Builtin {
                builtin,
                arguments,
                capability,
            } => {
                let arguments = arguments
                    .iter()
                    .map(|argument| argument.instantiate(substitutions, types))
                    .collect::<Option<Vec<_>>>()?;
                Some(types.builtin(*builtin, arguments, *capability))
            }
            Self::Union {
                members,
                capability,
            } => {
                let members = members
                    .iter()
                    .map(|member| member.instantiate(substitutions, types))
                    .collect::<Option<Vec<_>>>()?;
                Some(types.union(members, *capability))
            }
            Self::Divergence => Some(types.divergence()),
        }
    }

    fn required_substitutions(&self) -> usize {
        match self {
            Self::Parameter { index, .. } => index + 1,
            Self::Builtin { arguments, .. } => arguments
                .iter()
                .map(Self::required_substitutions)
                .max()
                .unwrap_or(0),
            Self::Union { members, .. } => members
                .iter()
                .map(Self::required_substitutions)
                .max()
                .unwrap_or(0),
            Self::Primitive { .. } | Self::Divergence => 0,
        }
    }
}

/// A symbolic callable signature for a compiler-known operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinCallableTemplate {
    pub receiver: Option<ReceiverSignature>,
    pub parameters: Vec<BuiltinTypeTemplate>,
    pub return_type: BuiltinTypeTemplate,
}

impl BuiltinCallableTemplate {
    /// Materializes a concrete callable header after generic arguments are known.
    pub fn instantiate(
        &self,
        substitutions: &[TypeId],
        types: &mut TypeStore,
    ) -> Option<CallableSignature> {
        let required = self
            .parameters
            .iter()
            .map(BuiltinTypeTemplate::required_substitutions)
            .chain([self.return_type.required_substitutions()])
            .max()
            .unwrap_or(0);
        if substitutions.len() != required {
            return None;
        }
        let parameters = self
            .parameters
            .iter()
            .map(|parameter| parameter.instantiate(substitutions, types))
            .collect::<Option<Vec<_>>>()?;
        let return_type = self.return_type.instantiate(substitutions, types)?;
        Some(CallableSignature {
            receiver: self.receiver,
            parameters,
            return_type,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinNamespace {
    Ascii,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinGlobalSignature {
    Callable(BuiltinCallableTemplate),
    Namespace(BuiltinNamespace),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinMemberOwner {
    Namespace(BuiltinNamespace),
    Primitive(PrimitiveType),
    Parameterized(BuiltinType),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinMemberSignature {
    Callable(BuiltinCallableTemplate),
    Field(BuiltinTypeTemplate),
}

/// The complete source-visible built-in catalogue whose signatures are specified.
#[derive(Debug, Clone)]
pub struct BuiltinSignatures {
    globals: HashMap<&'static str, BuiltinGlobalSignature>,
    members: HashMap<BuiltinMemberOwner, HashMap<&'static str, BuiltinMemberSignature>>,
}

impl BuiltinSignatures {
    fn new() -> Self {
        let const_receiver = Some(ReceiverSignature {
            storage: ReceiverStorage::Plain,
            capability: AccessCapability::Const,
        });
        let mut_receiver = Some(ReceiverSignature {
            storage: ReceiverStorage::Plain,
            capability: AccessCapability::Mut,
        });
        let unit = primitive(PrimitiveType::Unit, AccessCapability::Const);
        let none = primitive(PrimitiveType::None, AccessCapability::Const);
        let int = primitive(PrimitiveType::Int, AccessCapability::Const);
        let string = primitive(PrimitiveType::String, AccessCapability::Const);
        let bytes = primitive(PrimitiveType::Bytes, AccessCapability::Const);
        let mut_bytes = primitive(PrimitiveType::Bytes, AccessCapability::Mut);
        let parameter = BuiltinTypeTemplate::Parameter {
            index: 0,
            capability: None,
        };
        let error_string = BuiltinTypeTemplate::Builtin {
            builtin: BuiltinType::Error,
            arguments: vec![string.clone()],
            capability: AccessCapability::Const,
        };

        let globals = HashMap::from([
            (
                "print",
                BuiltinGlobalSignature::Callable(callable(
                    None,
                    vec![string.clone()],
                    unit.clone(),
                )),
            ),
            (
                "println",
                BuiltinGlobalSignature::Callable(callable(
                    None,
                    vec![string.clone()],
                    unit.clone(),
                )),
            ),
            (
                "panic",
                BuiltinGlobalSignature::Callable(callable(
                    None,
                    vec![string.clone()],
                    BuiltinTypeTemplate::Divergence,
                )),
            ),
            (
                "yield",
                BuiltinGlobalSignature::Callable(callable(None, vec![], unit.clone())),
            ),
            (
                "ascii",
                BuiltinGlobalSignature::Namespace(BuiltinNamespace::Ascii),
            ),
        ]);

        let mut members = HashMap::new();
        members.insert(
            BuiltinMemberOwner::Namespace(BuiltinNamespace::Ascii),
            HashMap::from([
                (
                    "encode",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![string.clone()],
                        mut_bytes.clone(),
                    )),
                ),
                (
                    "decode",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![bytes.clone()],
                        BuiltinTypeTemplate::Union {
                            members: vec![string.clone(), error_string],
                            capability: AccessCapability::Const,
                        },
                    )),
                ),
            ]),
        );
        members.insert(
            BuiltinMemberOwner::Primitive(PrimitiveType::String),
            HashMap::from([(
                "length",
                BuiltinMemberSignature::Callable(callable(
                    const_receiver,
                    vec![],
                    int.clone(),
                )),
            )]),
        );
        members.insert(
            BuiltinMemberOwner::Primitive(PrimitiveType::Bytes),
            HashMap::from([
                (
                    "length",
                    BuiltinMemberSignature::Callable(callable(
                        const_receiver,
                        vec![],
                        int.clone(),
                    )),
                ),
                (
                    "concat",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![bytes.clone(), bytes.clone()],
                        mut_bytes,
                    )),
                ),
            ]),
        );
        members.insert(
            BuiltinMemberOwner::Parameterized(BuiltinType::Queue),
            HashMap::from([
                (
                    "new",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![],
                        BuiltinTypeTemplate::Builtin {
                            builtin: BuiltinType::Queue,
                            arguments: vec![parameter.clone()],
                            capability: AccessCapability::Mut,
                        },
                    )),
                ),
                (
                    "send",
                    BuiltinMemberSignature::Callable(callable(
                        mut_receiver,
                        vec![parameter.clone()],
                        unit.clone(),
                    )),
                ),
                (
                    "try_receive",
                    BuiltinMemberSignature::Callable(callable(
                        mut_receiver,
                        vec![],
                        BuiltinTypeTemplate::Union {
                            members: vec![parameter.clone(), none],
                            capability: AccessCapability::Const,
                        },
                    )),
                ),
            ]),
        );
        for builtin in [BuiltinType::Vector, BuiltinType::Map] {
            let arguments = if builtin == BuiltinType::Map {
                vec![
                    parameter.clone(),
                    BuiltinTypeTemplate::Parameter {
                        index: 1,
                        capability: None,
                    },
                ]
            } else {
                vec![parameter.clone()]
            };
            members.insert(
                BuiltinMemberOwner::Parameterized(builtin),
                HashMap::from([(
                    "new",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![],
                        BuiltinTypeTemplate::Builtin {
                            builtin,
                            arguments,
                            capability: AccessCapability::Mut,
                        },
                    )),
                )]),
            );
        }
        members.insert(
            BuiltinMemberOwner::Parameterized(BuiltinType::Error),
            HashMap::from([
                (
                    "new",
                    BuiltinMemberSignature::Callable(callable(
                        None,
                        vec![parameter.clone()],
                        BuiltinTypeTemplate::Builtin {
                            builtin: BuiltinType::Error,
                            arguments: vec![parameter.clone()],
                            capability: AccessCapability::Const,
                        },
                    )),
                ),
                (
                    "value",
                    BuiltinMemberSignature::Field(BuiltinTypeTemplate::Parameter {
                        index: 0,
                        capability: Some(AccessCapability::Const),
                    }),
                ),
            ]),
        );

        Self { globals, members }
    }

    #[must_use]
    pub fn global(&self, name: &str) -> Option<&BuiltinGlobalSignature> {
        self.globals.get(name)
    }

    #[must_use]
    pub fn member(
        &self,
        owner: BuiltinMemberOwner,
        name: &str,
    ) -> Option<&BuiltinMemberSignature> {
        self.members.get(&owner)?.get(name)
    }

    #[must_use]
    pub const fn globals(&self) -> &HashMap<&'static str, BuiltinGlobalSignature> {
        &self.globals
    }

    #[must_use]
    pub const fn members(
        &self,
    ) -> &HashMap<BuiltinMemberOwner, HashMap<&'static str, BuiltinMemberSignature>> {
        &self.members
    }
}

fn primitive(
    primitive: PrimitiveType,
    capability: AccessCapability,
) -> BuiltinTypeTemplate {
    BuiltinTypeTemplate::Primitive {
        primitive,
        capability,
    }
}

fn callable(
    receiver: Option<ReceiverSignature>,
    parameters: Vec<BuiltinTypeTemplate>,
    return_type: BuiltinTypeTemplate,
) -> BuiltinCallableTemplate {
    BuiltinCallableTemplate {
        receiver,
        parameters,
        return_type,
    }
}

/// Semantic declaration metadata produced before expression type checking.
#[derive(Debug)]
pub struct SignatureCollection {
    callables: HashMap<NodeId, CallableSignature>,
    callable_value_types: HashMap<SymbolId, TypeId>,
    named_structs: HashMap<NodeId, StructSignature>,
    anonymous_structs: HashMap<NodeId, StructSignature>,
    interfaces: HashMap<NodeId, InterfaceSignature>,
    method_signatures: Vec<MethodSignature>,
    builtins: BuiltinSignatures,
}

impl SignatureCollection {
    #[must_use]
    pub fn callable(&self, id: NodeId) -> Option<&CallableSignature> {
        self.callables.get(&id)
    }

    #[must_use]
    pub fn callable_value_type(&self, symbol: SymbolId) -> Option<TypeId> {
        self.callable_value_types.get(&symbol).copied()
    }

    #[must_use]
    pub fn named_struct(&self, declaration: NodeId) -> Option<&StructSignature> {
        self.named_structs.get(&declaration)
    }

    #[must_use]
    pub fn anonymous_struct(&self, expression: NodeId) -> Option<&StructSignature> {
        self.anonymous_structs.get(&expression)
    }

    #[must_use]
    pub fn interface(&self, declaration: NodeId) -> Option<&InterfaceSignature> {
        self.interfaces.get(&declaration)
    }

    #[must_use]
    pub fn method_signature(&self, id: MethodId) -> Option<&MethodSignature> {
        self.method_signatures.get(id.0)
    }

    #[must_use]
    pub const fn builtins(&self) -> &BuiltinSignatures {
        &self.builtins
    }

    #[must_use]
    pub const fn callables(&self) -> &HashMap<NodeId, CallableSignature> {
        &self.callables
    }

    #[must_use]
    pub const fn callable_value_types(&self) -> &HashMap<SymbolId, TypeId> {
        &self.callable_value_types
    }

    #[must_use]
    pub const fn named_structs(&self) -> &HashMap<NodeId, StructSignature> {
        &self.named_structs
    }

    #[must_use]
    pub const fn anonymous_structs(&self) -> &HashMap<NodeId, StructSignature> {
        &self.anonymous_structs
    }

    #[must_use]
    pub const fn interfaces(&self) -> &HashMap<NodeId, InterfaceSignature> {
        &self.interfaces
    }

    #[must_use]
    pub fn method_signatures(&self) -> impl ExactSizeIterator<Item = (MethodId, &MethodSignature)> {
        self.method_signatures
            .iter()
            .enumerate()
            .map(|(index, signature)| (MethodId(index), signature))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureCollectionError {
    pub kind: SignatureCollectionErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCollectionErrorKind {
    DuplicateMember { name: String, first: Span },
    MainMustHaveNoParameters { found: usize },
    MainMustReturnUnit,
}

impl fmt::Display for SignatureCollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            SignatureCollectionErrorKind::DuplicateMember { name, first } => write!(
                formatter,
                "duplicate member `{name}` at {}..{}; first declared at {}..{}",
                self.span.start, self.span.end, first.start, first.end
            ),
            SignatureCollectionErrorKind::MainMustHaveNoParameters { found } => write!(
                formatter,
                "`main` must have no parameters, but {found} were declared at {}..{}",
                self.span.start, self.span.end
            ),
            SignatureCollectionErrorKind::MainMustReturnUnit => write!(
                formatter,
                "`main` must return `()` at {}..{}",
                self.span.start, self.span.end
            ),
        }
    }
}

impl std::error::Error for SignatureCollectionError {}

pub type SignatureCollectionResult =
    Result<SignatureCollection, Vec<SignatureCollectionError>>;

/// Collects all declaration and explicit callable signatures in one program.
pub fn collect_signatures(
    module: &SourceModule,
    program: &Program,
    names: &NameResolution,
    context: &ContextResolution,
    types: &mut TypeResolution,
) -> SignatureCollectionResult {
    assert_eq!(
        module.module_id(),
        program.id.module_id,
        "program must be collected with its source module"
    );
    Collector::new(module, names, context, types).collect(program)
}

struct Collector<'source, 'semantic> {
    module: &'source SourceModule,
    names: &'semantic NameResolution,
    context: &'semantic ContextResolution,
    types: &'semantic mut TypeResolution,
    callables: HashMap<NodeId, CallableSignature>,
    callable_value_types: HashMap<SymbolId, TypeId>,
    named_structs: HashMap<NodeId, StructSignature>,
    anonymous_structs: HashMap<NodeId, StructSignature>,
    interfaces: HashMap<NodeId, InterfaceSignature>,
    method_ids: HashMap<MethodSignature, MethodId>,
    method_signatures: Vec<MethodSignature>,
    builtins: BuiltinSignatures,
    errors: Vec<SignatureCollectionError>,
}

impl<'source, 'semantic> Collector<'source, 'semantic> {
    fn new(
        module: &'source SourceModule,
        names: &'semantic NameResolution,
        context: &'semantic ContextResolution,
        types: &'semantic mut TypeResolution,
    ) -> Self {
        Self {
            module,
            names,
            context,
            types,
            callables: HashMap::new(),
            callable_value_types: HashMap::new(),
            named_structs: HashMap::new(),
            anonymous_structs: HashMap::new(),
            interfaces: HashMap::new(),
            method_ids: HashMap::new(),
            method_signatures: Vec::new(),
            builtins: BuiltinSignatures::new(),
            errors: Vec::new(),
        }
    }

    fn collect(mut self, program: &Program) -> SignatureCollectionResult {
        self.collect_builtin_value_types();
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }

        if self.errors.is_empty() {
            Ok(SignatureCollection {
                callables: self.callables,
                callable_value_types: self.callable_value_types,
                named_structs: self.named_structs,
                anonymous_structs: self.anonymous_structs,
                interfaces: self.interfaces,
                method_signatures: self.method_signatures,
                builtins: self.builtins,
            })
        } else {
            Err(self.errors)
        }
    }

    fn collect_builtin_value_types(&mut self) {
        for name in ["print", "println", "panic", "yield"] {
            let Some(BuiltinGlobalSignature::Callable(template)) = self.builtins.global(name)
            else {
                unreachable!("fixed built-in callable must have a signature")
            };
            let template = template.clone();
            let signature = template
                .instantiate(&[], self.types.types_mut())
                .expect("fixed built-in signature must not require substitutions");
            let type_id = self.callable_type(&signature, AccessCapability::Const);
            let symbol = self
                .names
                .symbols()
                .lookup_value(self.names.program_scope(), name)
                .expect("built-in value must exist in the prelude");
            self.callable_value_types.insert(symbol, type_id);
        }
    }

    fn visit_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function) => {
                self.collect_function_header(function);
                if self.text(function.name) == "main" {
                    self.validate_main(function);
                }
                self.visit_block(&function.body);
            }
            Declaration::Struct(structure) => self.collect_named_struct(structure),
            Declaration::Interface(interface) => {
                let type_id = self
                    .types
                    .type_for_declaration(interface.id)
                    .expect("resolved interface declaration must have a type");
                let mut requirements = HashMap::new();
                for requirement in &interface.requirements {
                    let signature = self.collect_interface_requirement(requirement);
                    let method_id = self.intern_method(requirement.name, &signature);
                    let name = self.text(requirement.name).to_string();
                    let entry = InterfaceRequirementSignature {
                        declaration: requirement.id,
                        method_id,
                        span: requirement.name,
                    };
                    self.insert_interface_requirement(&mut requirements, name, entry);
                }
                self.interfaces.insert(
                    interface.id,
                    InterfaceSignature {
                        type_id,
                        requirements,
                    },
                );
            }
        }
    }

    fn collect_named_struct(&mut self, structure: &crate::ast::StructDeclaration) {
        let type_id = self
            .types
            .type_for_declaration(structure.id)
            .expect("resolved struct declaration must have a type");
        let mut members = HashMap::new();
        for member in &structure.members {
            match member {
                StructMember::Field(field) => {
                    let name = self.text(field.name).to_string();
                    let entry = StructMemberSignature {
                        kind: StructMemberSignatureKind::Field(FieldSignature {
                            declaration: field.id,
                            type_id: Some(self.resolved_type(field.type_annotation.id)),
                        }),
                        span: field.name,
                    };
                    self.insert_struct_member(&mut members, name, entry);
                }
                StructMember::Function(function) => {
                    let signature = self.collect_function_header(function);
                    let kind = match self.context.callable_kind(function.id) {
                        Some(CallableKind::NamedStructMethod) => {
                            let method_id = self.intern_method(function.name, &signature);
                            StructMemberSignatureKind::Method {
                                declaration: function.id,
                                method_id,
                            }
                        }
                        Some(CallableKind::NamedStructAssociatedFunction) => {
                            StructMemberSignatureKind::AssociatedFunction {
                                declaration: function.id,
                            }
                        }
                        _ => unreachable!("named struct function must be context-classified"),
                    };
                    let name = self.text(function.name).to_string();
                    self.insert_struct_member(
                        &mut members,
                        name,
                        StructMemberSignature {
                            kind,
                            span: function.name,
                        },
                    );
                    self.visit_block(&function.body);
                }
            }
        }
        self.named_structs
            .insert(structure.id, StructSignature { type_id, members });
    }

    fn collect_function(&mut self, function: &Function) -> CallableSignature {
        let signature = self.collect_function_header(function);
        self.visit_block(&function.body);
        signature
    }

    fn collect_function_header(&mut self, function: &Function) -> CallableSignature {
        let signature = self.source_signature(&function.parameters, function.return_type.as_ref());
        self.callables.insert(function.id, signature.clone());

        if matches!(
            self.context.callable_kind(function.id),
            Some(CallableKind::TopLevelFunction | CallableKind::NestedFunction)
        ) {
            let symbol = self
                .names
                .symbol_for_declaration(function.id)
                .expect("named function must have a semantic symbol");
            let type_id = self.callable_type(&signature, AccessCapability::Const);
            self.callable_value_types.insert(symbol, type_id);
        }
        signature
    }

    fn collect_interface_requirement(
        &mut self,
        requirement: &InterfaceMethodRequirement,
    ) -> CallableSignature {
        let signature =
            self.source_signature(&requirement.parameters, requirement.return_type.as_ref());
        self.callables.insert(requirement.id, signature.clone());
        signature
    }

    fn source_signature(
        &mut self,
        parameters: &[FunctionParameter],
        return_type: Option<&crate::ast::TypeSyntax>,
    ) -> CallableSignature {
        let mut receiver = None;
        let mut semantic_parameters = Vec::new();
        for parameter in parameters {
            match &parameter.kind {
                FunctionParameterKind::Receiver { storage, .. } => {
                    receiver = Some(ReceiverSignature {
                        storage: *storage,
                        capability: parameter.qualifiers.value.into(),
                    });
                }
                FunctionParameterKind::Named {
                    type_annotation, ..
                } => {
                    let resolved = self.resolved_type(type_annotation.id);
                    let resolved = self
                        .types
                        .types_mut()
                        .with_capability(resolved, parameter.qualifiers.value.into())
                        .expect("resolved parameter type belongs to the program type store");
                    semantic_parameters.push(resolved);
                }
            }
        }
        let return_type = match return_type {
            Some(syntax) => self.resolved_type(syntax.id),
            None => {
                self.types
                    .types_mut()
                    .primitive(PrimitiveType::Unit, AccessCapability::Const)
            }
        };
        CallableSignature {
            receiver,
            parameters: semantic_parameters,
            return_type,
        }
    }

    fn callable_type(
        &mut self,
        signature: &CallableSignature,
        capability: AccessCapability,
    ) -> TypeId {
        self.types.types_mut().callable(
            signature.parameters.clone(),
            signature.return_type,
            capability,
        )
    }

    fn intern_method(&mut self, name: Span, signature: &CallableSignature) -> MethodId {
        let method = MethodSignature {
            name: self.text(name).to_string(),
            receiver: signature
                .receiver
                .expect("context resolution guarantees that methods have receivers"),
            parameters: signature.parameters.clone(),
            return_type: signature.return_type,
        };
        if let Some(id) = self.method_ids.get(&method) {
            return *id;
        }
        let id = MethodId(self.method_signatures.len());
        self.method_signatures.push(method.clone());
        self.method_ids.insert(method, id);
        id
    }

    fn validate_main(&mut self, function: &Function) {
        if !function.parameters.is_empty() {
            self.error(
                SignatureCollectionErrorKind::MainMustHaveNoParameters {
                    found: function.parameters.len(),
                },
                function.parameters[0].span,
            );
        }
        let return_type = self
            .callables
            .get(&function.id)
            .expect("main signature was just collected")
            .return_type;
        let unit = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Unit, AccessCapability::Const);
        if return_type != unit {
            self.error(
                SignatureCollectionErrorKind::MainMustReturnUnit,
                function
                    .return_type
                    .as_ref()
                    .map_or(function.name, |syntax| syntax.span),
            );
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
            StatementKind::Binding { initializer, .. }
            | StatementKind::Expression(initializer)
            | StatementKind::Defer(initializer)
            | StatementKind::Coroutine(initializer) => self.visit_expression(initializer),
            StatementKind::Function(function) => {
                self.collect_function(function);
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
            ExpressionKind::Identifier
            | ExpressionKind::SelfValue
            | ExpressionKind::Literal(_) => {}
            ExpressionKind::Group(inner)
            | ExpressionKind::GarbageCollect(inner)
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
                let signature = self.source_signature(parameters, return_type.as_ref());
                self.callables.insert(expression.id, signature);
                self.visit_block(body);
            }
            ExpressionKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.visit_expression(&field.value);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                self.collect_anonymous_struct(expression.id, members);
            }
            ExpressionKind::Call { callee, arguments } => {
                self.visit_expression(callee);
                for argument in arguments {
                    self.visit_expression(argument);
                }
            }
            ExpressionKind::MemberAccess { object, .. } => self.visit_expression(object),
            ExpressionKind::AssociatedAccess { .. } => {}
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
            ExpressionKind::TypeTest { value, .. } => self.visit_expression(value),
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

    fn collect_anonymous_struct(
        &mut self,
        expression: NodeId,
        source_members: &[AnonymousStructMember],
    ) {
        let type_id = self
            .types
            .types_mut()
            .anonymous_struct(expression, AccessCapability::Const);
        let mut members = HashMap::new();
        for member in source_members {
            match member {
                AnonymousStructMember::Field(field) => {
                    let name = self.text(field.name).to_string();
                    let entry = StructMemberSignature {
                        kind: StructMemberSignatureKind::Field(FieldSignature {
                            declaration: field.id,
                            type_id: field
                                .type_annotation
                                .as_ref()
                                .map(|syntax| self.resolved_type(syntax.id)),
                        }),
                        span: field.name,
                    };
                    self.insert_struct_member(&mut members, name, entry);
                    self.visit_expression(&field.initializer);
                }
                AnonymousStructMember::Method(function) => {
                    let signature = self.collect_function_header(function);
                    let method_id = self.intern_method(function.name, &signature);
                    let name = self.text(function.name).to_string();
                    self.insert_struct_member(
                        &mut members,
                        name,
                        StructMemberSignature {
                            kind: StructMemberSignatureKind::Method {
                                declaration: function.id,
                                method_id,
                            },
                            span: function.name,
                        },
                    );
                    self.visit_block(&function.body);
                }
            }
        }
        self.anonymous_structs
            .insert(expression, StructSignature { type_id, members });
    }

    fn insert_struct_member(
        &mut self,
        members: &mut HashMap<String, StructMemberSignature>,
        name: String,
        entry: StructMemberSignature,
    ) {
        if let Some(first) = members.get(&name) {
            self.error(
                SignatureCollectionErrorKind::DuplicateMember {
                    name,
                    first: first.span,
                },
                entry.span,
            );
        } else {
            members.insert(name, entry);
        }
    }

    fn insert_interface_requirement(
        &mut self,
        requirements: &mut HashMap<String, InterfaceRequirementSignature>,
        name: String,
        entry: InterfaceRequirementSignature,
    ) {
        if let Some(first) = requirements.get(&name) {
            self.error(
                SignatureCollectionErrorKind::DuplicateMember {
                    name,
                    first: first.span,
                },
                entry.span,
            );
        } else {
            requirements.insert(name, entry);
        }
    }

    fn resolved_type(&self, syntax: NodeId) -> TypeId {
        self.types
            .type_for_syntax(syntax)
            .expect("signature type syntax must have type-resolution metadata")
    }

    fn error(&mut self, kind: SignatureCollectionErrorKind, span: Span) {
        self.errors.push(SignatureCollectionError { kind, span });
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
        ast::{AnonymousStructMember, Declaration, ExpressionKind, StatementKind, StructMember},
        context_resolution::resolve_program_context,
        lexer::Lexer,
        name_resolution::resolve_program,
        parser::{ParseContext, parse_program},
        semantic_types::SemanticType,
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
    ) {
        let mut registry = SourceModuleRegistry::new();
        let module = registry.add(source);
        let mut parse_context = ParseContext::new(module.module_id());
        let program = parse_program(&mut parse_context, Lexer::new(&module))
            .expect("test source should parse");
        let names = resolve_program(&module, &program).expect("test names should resolve");
        let context =
            resolve_program_context(&program).expect("test context should resolve");
        let types =
            resolve_types(&module, &program, &names).expect("test types should resolve");
        (module, program, names, context, types)
    }

    fn collect(
        module: &SourceModule,
        program: &Program,
        names: &NameResolution,
        context: &ContextResolution,
        types: &mut TypeResolution,
    ) -> SignatureCollection {
        collect_signatures(module, program, names, context, types)
            .expect("test signatures should collect")
    }

    #[test]
    fn collects_forward_and_recursive_named_struct_fields() {
        let (module, program, names, context, mut types) = prepare(concat!(
            "struct First { later: Later, }\n",
            "struct Later { first: &First, }\n",
            "fn main() {}\n",
        ));
        let signatures = collect(&module, &program, &names, &context, &mut types);
        let Declaration::Struct(first) = &program.declarations[0] else {
            panic!("expected First")
        };
        let Declaration::Struct(later) = &program.declarations[1] else {
            panic!("expected Later")
        };

        let Some(StructMemberSignature {
            kind:
                StructMemberSignatureKind::Field(FieldSignature {
                    type_id: Some(later_field_type),
                    ..
                }),
            ..
        }) = signatures.named_struct(first.id).and_then(|item| item.member("later"))
        else {
            panic!("expected First.later")
        };
        assert_eq!(
            Some(*later_field_type),
            types.type_for_declaration(later.id)
        );

        let Some(StructMemberSignature {
            kind:
                StructMemberSignatureKind::Field(FieldSignature {
                    type_id: Some(first_field_type),
                    ..
                }),
            ..
        }) = signatures.named_struct(later.id).and_then(|item| item.member("first"))
        else {
            panic!("expected Later.first")
        };
        assert!(matches!(
            types.types().get(*first_field_type),
            Some(SemanticType::GarbageCollected { target, .. })
                if Some(*target) == types.type_for_declaration(first.id)
        ));
    }

    #[test]
    fn interns_matching_struct_and_interface_methods_once() {
        let (module, program, names, context, mut types) = prepare(concat!(
            "interface First { fn read(self, count: int) -> bytes; }\n",
            "interface Second { fn read(self, amount: int) -> bytes; }\n",
            "struct File { fn read(self, count: int) -> bytes { \"\"[0..0] } }\n",
            "fn main() {}\n",
        ));
        let signatures = collect(&module, &program, &names, &context, &mut types);
        let Declaration::Interface(first) = &program.declarations[0] else {
            panic!("expected First")
        };
        let Declaration::Interface(second) = &program.declarations[1] else {
            panic!("expected Second")
        };
        let Declaration::Struct(file) = &program.declarations[2] else {
            panic!("expected File")
        };
        let first_id = signatures
            .interface(first.id)
            .and_then(|item| item.requirement("read"))
            .expect("expected First.read")
            .method_id;
        let second_id = signatures
            .interface(second.id)
            .and_then(|item| item.requirement("read"))
            .expect("expected Second.read")
            .method_id;
        let Some(StructMemberSignature {
            kind: StructMemberSignatureKind::Method { method_id, .. },
            ..
        }) = signatures.named_struct(file.id).and_then(|item| item.member("read"))
        else {
            panic!("expected File.read")
        };

        assert_eq!(first_id, second_id);
        assert_eq!(first_id, *method_id);
        assert_eq!(signatures.method_signatures().len(), 1);
        assert_eq!(
            signatures
                .method_signature(first_id)
                .expect("method ID should be valid")
                .parameters
                .len(),
            1
        );
    }

    #[test]
    fn method_identity_includes_every_semantic_component() {
        let (module, program, names, context, mut types) = prepare(concat!(
            "interface Base { fn read(self, value: int) -> int; }\n",
            "interface Name { fn write(self, value: int) -> int; }\n",
            "interface Mut { fn read(mut self, value: int) -> int; }\n",
            "interface Gc { fn read(&self, value: int) -> int; }\n",
            "interface GcMut { fn read(&mut self, value: int) -> int; }\n",
            "interface Parameter { fn read(self, value: string) -> int; }\n",
            "interface Return { fn read(self, value: int) -> string; }\n",
            "fn main() {}\n",
        ));
        let signatures = collect(&module, &program, &names, &context, &mut types);
        assert_eq!(signatures.method_signatures().len(), 7);
    }

    #[test]
    fn collects_all_explicit_callable_forms_and_pending_anonymous_fields() {
        let (module, program, names, context, mut types) = prepare(concat!(
            "interface Run { fn run(&mut self, const vmut value: int) -> bool; }\n",
            "struct Worker {\n",
            "    fn make() -> Worker { Worker {} }\n",
            "    fn run(&mut self, const vmut value: int) -> bool { true }\n",
            "}\n",
            "fn helper(value: int) -> int { value }\n",
            "fn main() {\n",
            "    fn nested() {}\n",
            "    const closure = lambda(value: int) -> bool { true };\n",
            "    const object = struct {\n",
            "        explicit: int = 1;\n",
            "        inferred = 2;\n",
            "        fn run(self) {}\n",
            "    };\n",
            "}\n",
        ));
        let signatures = collect(&module, &program, &names, &context, &mut types);
        assert_eq!(signatures.callables().len(), 8);

        let Declaration::Function(helper) = &program.declarations[2] else {
            panic!("expected helper")
        };
        let helper_symbol = names
            .symbol_for_declaration(helper.id)
            .expect("helper should have a symbol");
        assert!(matches!(
            signatures
                .callable_value_type(helper_symbol)
                .and_then(|type_id| types.types().get(type_id)),
            Some(SemanticType::Callable { .. })
        ));

        let Declaration::Function(main) = &program.declarations[3] else {
            panic!("expected main")
        };
        let StatementKind::Binding { initializer, .. } = &main.body.statements[2].kind else {
            panic!("expected anonymous struct binding")
        };
        let ExpressionKind::AnonymousStruct { members } = &initializer.kind else {
            panic!("expected anonymous struct")
        };
        let anonymous = signatures
            .anonymous_struct(initializer.id)
            .expect("anonymous type should be collected");
        let Some(StructMemberSignature {
            kind:
                StructMemberSignatureKind::Field(FieldSignature {
                    type_id: Some(_),
                    ..
                }),
            ..
        }) = anonymous.member("explicit")
        else {
            panic!("annotated field should be complete")
        };
        let Some(StructMemberSignature {
            kind:
                StructMemberSignatureKind::Field(FieldSignature {
                    type_id: None, ..
                }),
            ..
        }) = anonymous.member("inferred")
        else {
            panic!("inferred field should remain pending")
        };
        let AnonymousStructMember::Method(method) = &members[2] else {
            panic!("expected anonymous method")
        };
        assert!(signatures.callable(method.id).is_some());

        let Declaration::Struct(worker) = &program.declarations[1] else {
            panic!("expected Worker")
        };
        let StructMember::Function(run) = &worker.members[1] else {
            panic!("expected Worker.run")
        };
        let run = signatures.callable(run.id).expect("run should be collected");
        assert_eq!(
            run.receiver,
            Some(ReceiverSignature {
                storage: ReceiverStorage::GarbageCollected,
                capability: AccessCapability::Mut,
            })
        );
        assert_eq!(run.parameters.len(), 1);
        assert!(matches!(
            types.types().get(run.parameters[0]),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Mut,
            })
        ));
    }

    #[test]
    fn reports_member_and_main_errors_in_source_order() {
        let (module, program, names, context, mut types) = prepare(concat!(
            "struct Item { value: int, fn value() {} }\n",
            "struct Calls { fn use() {} fn use(self) {} }\n",
            "interface Repeat { fn use(self); fn use(self); }\n",
            "fn main(value: int) -> int {\n",
            "    const object = struct { field = 1; fn field(self) {} };\n",
            "    0\n",
            "}\n",
        ));
        let errors = collect_signatures(&module, &program, &names, &context, &mut types)
            .expect_err("signatures should be invalid");
        assert_eq!(errors.len(), 6);
        assert!(matches!(
            errors[0].kind,
            SignatureCollectionErrorKind::DuplicateMember { ref name, .. } if name == "value"
        ));
        assert!(matches!(
            errors[1].kind,
            SignatureCollectionErrorKind::DuplicateMember { ref name, .. } if name == "use"
        ));
        assert!(matches!(
            errors[2].kind,
            SignatureCollectionErrorKind::DuplicateMember { ref name, .. } if name == "use"
        ));
        assert!(matches!(
            errors[3].kind,
            SignatureCollectionErrorKind::MainMustHaveNoParameters { found: 1 }
        ));
        assert!(matches!(
            errors[4].kind,
            SignatureCollectionErrorKind::MainMustReturnUnit
        ));
        assert!(matches!(
            errors[5].kind,
            SignatureCollectionErrorKind::DuplicateMember { ref name, .. } if name == "field"
        ));
        assert!(errors.windows(2).all(|pair| pair[0].span.start < pair[1].span.start));
    }

    #[test]
    fn exposes_only_the_specified_builtin_catalogue() {
        let (module, program, names, context, mut types) = prepare("fn main() {}");
        let signatures = collect(&module, &program, &names, &context, &mut types);
        let builtins = signatures.builtins();
        assert!(matches!(
            builtins.global("ascii"),
            Some(BuiltinGlobalSignature::Namespace(BuiltinNamespace::Ascii))
        ));
        let print_symbol = names
            .symbols()
            .lookup_value(names.program_scope(), "print")
            .expect("print should be a prelude symbol");
        assert!(matches!(
            signatures
                .callable_value_type(print_symbol)
                .and_then(|type_id| types.types().get(type_id)),
            Some(SemanticType::Callable { .. })
        ));
        assert!(builtins
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Queue),
                "try_receive"
            )
            .is_some());
        assert!(builtins
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Error),
                "value"
            )
            .is_some());
        assert!(builtins
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Vector),
                "length"
            )
            .is_none());
        assert!(builtins
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Map),
                "insert"
            )
            .is_none());

        let int = types
            .types_mut()
            .primitive(PrimitiveType::Int, AccessCapability::Const);
        let Some(BuiltinMemberSignature::Callable(template)) = builtins.member(
            BuiltinMemberOwner::Parameterized(BuiltinType::Queue),
            "try_receive",
        ) else {
            panic!("expected Queue.try_receive")
        };
        let instantiated = template
            .instantiate(&[int], types.types_mut())
            .expect("Queue template should accept T");
        assert!(matches!(
            types.types().get(instantiated.return_type),
            Some(SemanticType::Union { members, .. }) if members.len() == 2
        ));
    }
}
