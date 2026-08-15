use std::collections::HashMap;

use crate::ast::{BuiltinType, NodeId, PrimitiveType};

/// The canonical identity of one semantic type within a [`TypeStore`].
///
/// A type ID is meaningful only with the store that created it. IDs from
/// different stores must not be compared or used interchangeably.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(usize);

/// The access capability carried as part of a semantic type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessCapability {
    Const,
    Mut,
}

/// Whether values of a type are copied directly or refer to shared storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueSemantics {
    Copied,
    Reference,
}

/// A canonical semantic type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SemanticType {
    Primitive {
        primitive: PrimitiveType,
        capability: AccessCapability,
    },
    Callable {
        parameters: Vec<TypeId>,
        return_type: TypeId,
        capability: AccessCapability,
    },
    NamedStruct {
        declaration: NodeId,
        capability: AccessCapability,
    },
    AnonymousStruct {
        expression: NodeId,
        capability: AccessCapability,
    },
    Interface {
        declaration: NodeId,
        capability: AccessCapability,
    },
    Builtin {
        builtin: BuiltinType,
        arguments: Vec<TypeId>,
        capability: AccessCapability,
    },
    /// An invalid type used after emitting a diagnostic so checking can
    /// continue without producing cascading errors.
    Recovery,
    /// The type of a valid expression or path that never produces a value.
    Divergence,
}

impl SemanticType {
    /// Returns this type's access capability, if it represents a value.
    #[must_use]
    pub const fn capability(&self) -> Option<AccessCapability> {
        match self {
            Self::Primitive { capability, .. }
            | Self::Callable { capability, .. }
            | Self::NamedStruct { capability, .. }
            | Self::AnonymousStruct { capability, .. }
            | Self::Interface { capability, .. }
            | Self::Builtin { capability, .. } => Some(*capability),
            Self::Recovery | Self::Divergence => None,
        }
    }

    /// Returns how values of this type are passed and stored.
    #[must_use]
    pub const fn value_semantics(&self) -> Option<ValueSemantics> {
        match self {
            Self::Primitive {
                primitive: PrimitiveType::String | PrimitiveType::Bytes,
                ..
            }
            | Self::Callable { .. }
            | Self::NamedStruct { .. }
            | Self::AnonymousStruct { .. }
            | Self::Interface { .. }
            | Self::Builtin {
                builtin: BuiltinType::Queue | BuiltinType::Vector | BuiltinType::Map,
                ..
            } => Some(ValueSemantics::Reference),
            Self::Primitive { .. }
            | Self::Builtin {
                builtin: BuiltinType::Error,
                ..
            } => Some(ValueSemantics::Copied),
            Self::Recovery | Self::Divergence => None,
        }
    }
}

/// Owns and canonically interns the semantic types for one program.
#[derive(Debug)]
pub struct TypeStore {
    types: Vec<SemanticType>,
    type_ids: HashMap<SemanticType, TypeId>,
    recovery_id: TypeId,
    divergence_id: TypeId,
}

impl Default for TypeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeStore {
    /// Creates a type store containing the canonical internal types.
    #[must_use]
    pub fn new() -> Self {
        let recovery = TypeId(0);
        let divergence = TypeId(1);
        let types = vec![SemanticType::Recovery, SemanticType::Divergence];
        let type_ids = HashMap::from([
            (SemanticType::Recovery, recovery),
            (SemanticType::Divergence, divergence),
        ]);

        Self {
            types,
            type_ids,
            recovery_id: recovery,
            divergence_id: divergence,
        }
    }

    /// Returns the canonical identity for a capability-qualified primitive.
    pub fn primitive(
        &mut self,
        primitive: PrimitiveType,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::Primitive {
            primitive,
            capability,
        })
    }

    /// Returns the canonical identity for a capability-qualified callable.
    ///
    /// Parameter binding mutability is local declaration metadata and is not
    /// part of a callable signature. Each parameter's value capability is
    /// already represented by its semantic type.
    pub fn callable(
        &mut self,
        parameters: Vec<TypeId>,
        return_type: TypeId,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::Callable {
            parameters,
            return_type,
            capability,
        })
    }

    /// Returns the canonical identity for a named nominal struct declaration.
    pub fn named_struct(
        &mut self,
        declaration: NodeId,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::NamedStruct {
            declaration,
            capability,
        })
    }

    /// Returns the canonical identity for an anonymous nominal struct.
    pub fn anonymous_struct(
        &mut self,
        expression: NodeId,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::AnonymousStruct {
            expression,
            capability,
        })
    }

    /// Returns the canonical identity for a declared structural interface.
    pub fn interface(
        &mut self,
        declaration: NodeId,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::Interface {
            declaration,
            capability,
        })
    }

    /// Returns the canonical identity for a compiler-known parameterized type.
    ///
    /// Type argument arity and legality are validated when source type syntax
    /// is resolved, not by the interner.
    pub fn builtin(
        &mut self,
        builtin: BuiltinType,
        arguments: Vec<TypeId>,
        capability: AccessCapability,
    ) -> TypeId {
        self.intern(SemanticType::Builtin {
            builtin,
            arguments,
            capability,
        })
    }

    /// Returns the store's canonical recovery type.
    #[must_use]
    pub const fn recovery(&self) -> TypeId {
        self.recovery_id
    }

    /// Returns the store's canonical divergence type.
    #[must_use]
    pub const fn divergence(&self) -> TypeId {
        self.divergence_id
    }

    /// Looks up a semantic type by its store-local identity.
    #[must_use]
    pub fn get(&self, id: TypeId) -> Option<&SemanticType> {
        self.types.get(id.0)
    }

    /// Returns the canonical form of a type with the requested capability.
    ///
    /// This operation constructs a type; it does not decide whether increasing
    /// a capability is legal at a particular use site. Recovery and divergence
    /// do not represent values and are returned unchanged.
    pub fn with_capability(
        &mut self,
        id: TypeId,
        capability: AccessCapability,
    ) -> Option<TypeId> {
        match self.get(id)?.clone() {
            SemanticType::Primitive { primitive, .. } => {
                Some(self.primitive(primitive, capability))
            }
            SemanticType::Callable {
                parameters,
                return_type,
                ..
            } => Some(self.callable(parameters, return_type, capability)),
            SemanticType::NamedStruct { declaration, .. } => {
                Some(self.named_struct(declaration, capability))
            }
            SemanticType::AnonymousStruct { expression, .. } => {
                Some(self.anonymous_struct(expression, capability))
            }
            SemanticType::Interface { declaration, .. } => {
                Some(self.interface(declaration, capability))
            }
            SemanticType::Builtin {
                builtin, arguments, ..
            } => Some(self.builtin(builtin, arguments, capability)),
            SemanticType::Recovery | SemanticType::Divergence => Some(id),
        }
    }

    /// Returns the number of canonical types currently held by this store.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.types.len()
    }

    /// Returns whether this store contains no canonical types.
    ///
    /// A normally constructed store contains recovery and divergence types, so
    /// it is not empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    fn intern(&mut self, semantic_type: SemanticType) -> TypeId {
        if let Some(id) = self.type_ids.get(&semantic_type) {
            return *id;
        }

        let id = TypeId(self.types.len());
        self.types.push(semantic_type.clone());
        self.type_ids.insert(semantic_type, id);
        id
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::source::ModuleId;

    const PRIMITIVES: [PrimitiveType; 8] = [
        PrimitiveType::Unit,
        PrimitiveType::None,
        PrimitiveType::Int,
        PrimitiveType::Float,
        PrimitiveType::Bool,
        PrimitiveType::Char,
        PrimitiveType::String,
        PrimitiveType::Bytes,
    ];

    const fn node(node_id: u32) -> NodeId {
        NodeId {
            module_id: ModuleId::TEST_SOURCE,
            node_id,
        }
    }

    #[test]
    fn repeated_primitive_construction_reuses_the_canonical_identity() {
        let mut types = TypeStore::new();

        let first = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let second = types.primitive(PrimitiveType::Int, AccessCapability::Const);

        assert_eq!(first, second);
    }

    #[test]
    fn primitive_identity_includes_capability() {
        let mut types = TypeStore::new();

        for primitive in PRIMITIVES {
            let const_type = types.primitive(primitive, AccessCapability::Const);
            let mut_type = types.primitive(primitive, AccessCapability::Mut);

            assert_ne!(const_type, mut_type, "{primitive:?}");
        }
    }

    #[test]
    fn different_primitives_have_different_identities() {
        let mut types = TypeStore::new();
        let identities: HashSet<_> = PRIMITIVES
            .into_iter()
            .map(|primitive| types.primitive(primitive, AccessCapability::Const))
            .collect();

        assert_eq!(identities.len(), PRIMITIVES.len());
    }

    #[test]
    fn primitive_metadata_distinguishes_copied_and_reference_values() {
        let mut types = TypeStore::new();

        for primitive in [
            PrimitiveType::Unit,
            PrimitiveType::None,
            PrimitiveType::Int,
            PrimitiveType::Float,
            PrimitiveType::Bool,
            PrimitiveType::Char,
        ] {
            let id = types.primitive(primitive, AccessCapability::Const);
            let semantic_type = types.get(id).expect("primitive should be interned");

            assert_eq!(semantic_type.capability(), Some(AccessCapability::Const));
            assert_eq!(semantic_type.value_semantics(), Some(ValueSemantics::Copied));
        }

        for primitive in [PrimitiveType::String, PrimitiveType::Bytes] {
            let id = types.primitive(primitive, AccessCapability::Mut);
            let semantic_type = types.get(id).expect("primitive should be interned");

            assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
            assert_eq!(
                semantic_type.value_semantics(),
                Some(ValueSemantics::Reference)
            );
        }
    }

    #[test]
    fn repeated_compound_and_declared_type_construction_reuses_canonical_identities() {
        let mut types = TypeStore::new();
        let parameter = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let return_type = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let callable = types.callable(
            vec![parameter],
            return_type,
            AccessCapability::Const,
        );
        assert_eq!(
            types.callable(
                vec![parameter],
                return_type,
                AccessCapability::Const,
            ),
            callable
        );

        let named = types.named_struct(node(1), AccessCapability::Const);
        assert_eq!(
            types.named_struct(node(1), AccessCapability::Const),
            named
        );

        let anonymous = types.anonymous_struct(node(2), AccessCapability::Const);
        assert_eq!(
            types.anonymous_struct(node(2), AccessCapability::Const),
            anonymous
        );

        let interface = types.interface(node(3), AccessCapability::Const);
        assert_eq!(types.interface(node(3), AccessCapability::Const), interface);

        let builtin = types.builtin(
            BuiltinType::Queue,
            vec![parameter],
            AccessCapability::Const,
        );
        assert_eq!(
            types.builtin(
                BuiltinType::Queue,
                vec![parameter],
                AccessCapability::Const,
            ),
            builtin
        );
    }

    #[test]
    fn callable_identity_includes_capability_parameters_and_return_type() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let string = types.primitive(PrimitiveType::String, AccessCapability::Const);
        let unit = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let original = types.callable(vec![int, string], unit, AccessCapability::Const);
        let mutable = types.callable(vec![int, string], unit, AccessCapability::Mut);
        let reversed = types.callable(vec![string, int], unit, AccessCapability::Const);
        let different_return =
            types.callable(vec![int, string], int, AccessCapability::Const);

        assert_ne!(original, mutable);
        assert_ne!(original, reversed);
        assert_ne!(original, different_return);
    }

    #[test]
    fn declared_type_identity_uses_the_owning_node_and_capability() {
        let mut types = TypeStore::new();

        let named = types.named_struct(node(1), AccessCapability::Const);
        let other_named = types.named_struct(node(2), AccessCapability::Const);
        let mutable_named = types.named_struct(node(1), AccessCapability::Mut);
        assert_ne!(named, other_named);
        assert_ne!(named, mutable_named);

        let anonymous = types.anonymous_struct(node(3), AccessCapability::Const);
        let other_anonymous = types.anonymous_struct(node(4), AccessCapability::Const);
        let mutable_anonymous = types.anonymous_struct(node(3), AccessCapability::Mut);
        assert_ne!(anonymous, other_anonymous);
        assert_ne!(anonymous, mutable_anonymous);

        let interface = types.interface(node(5), AccessCapability::Const);
        let other_interface = types.interface(node(6), AccessCapability::Const);
        let mutable_interface = types.interface(node(5), AccessCapability::Mut);
        assert_ne!(interface, other_interface);
        assert_ne!(interface, mutable_interface);
    }

    #[test]
    fn builtin_identity_includes_constructor_arguments_and_capability() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let string = types.primitive(PrimitiveType::String, AccessCapability::Const);

        let map = types.builtin(
            BuiltinType::Map,
            vec![int, string],
            AccessCapability::Const,
        );
        let reversed = types.builtin(
            BuiltinType::Map,
            vec![string, int],
            AccessCapability::Const,
        );
        let mutable = types.builtin(
            BuiltinType::Map,
            vec![int, string],
            AccessCapability::Mut,
        );
        let queue = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Const);
        let vector = types.builtin(BuiltinType::Vector, vec![int], AccessCapability::Const);

        assert_ne!(map, reversed);
        assert_ne!(map, mutable);
        assert_ne!(queue, vector);
    }

    #[test]
    fn compound_and_declared_type_metadata_tracks_capability_and_value_semantics() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let unit = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let callable = types.callable(vec![int], unit, AccessCapability::Mut);
        let named = types.named_struct(node(1), AccessCapability::Mut);
        let anonymous = types.anonymous_struct(node(2), AccessCapability::Mut);
        let interface = types.interface(node(3), AccessCapability::Mut);
        let queue = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Mut);
        let vector = types.builtin(BuiltinType::Vector, vec![int], AccessCapability::Mut);
        let map = types.builtin(
            BuiltinType::Map,
            vec![int, int],
            AccessCapability::Mut,
        );

        for id in [callable, named, anonymous, interface, queue, vector, map] {
            let semantic_type = types.get(id).expect("type should be interned");
            assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
            assert_eq!(
                semantic_type.value_semantics(),
                Some(ValueSemantics::Reference)
            );
        }

        let error = types.builtin(BuiltinType::Error, vec![int], AccessCapability::Mut);
        let semantic_type = types.get(error).expect("Error should be interned");
        assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
        assert_eq!(
            semantic_type.value_semantics(),
            Some(ValueSemantics::Copied)
        );
    }

    #[test]
    fn recovery_and_divergence_are_stable_distinct_internal_types() {
        let types = TypeStore::new();

        assert_ne!(types.recovery(), types.divergence());
        assert_eq!(types.get(types.recovery()), Some(&SemanticType::Recovery));
        assert_eq!(
            types.get(types.divergence()),
            Some(&SemanticType::Divergence)
        );

        for id in [types.recovery(), types.divergence()] {
            let semantic_type = types.get(id).expect("internal type should exist");
            assert_eq!(semantic_type.capability(), None);
            assert_eq!(semantic_type.value_semantics(), None);
        }
    }

    #[test]
    fn capability_replacement_uses_canonical_primitive_types() {
        let mut types = TypeStore::new();
        let const_type = types.primitive(PrimitiveType::String, AccessCapability::Const);
        let mut_type = types.primitive(PrimitiveType::String, AccessCapability::Mut);

        assert_eq!(
            types.with_capability(const_type, AccessCapability::Mut),
            Some(mut_type)
        );
        assert_eq!(
            types.with_capability(mut_type, AccessCapability::Const),
            Some(const_type)
        );
        assert_eq!(
            types.with_capability(mut_type, AccessCapability::Mut),
            Some(mut_type)
        );
    }

    #[test]
    fn capability_replacement_preserves_compound_and_declared_type_identity() {
        let mut types = TypeStore::new();
        let parameter = types.primitive(PrimitiveType::Int, AccessCapability::Mut);
        let return_type = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let const_callable = types.callable(
            vec![parameter],
            return_type,
            AccessCapability::Const,
        );
        let mut_callable = types.callable(
            vec![parameter],
            return_type,
            AccessCapability::Mut,
        );
        let const_named = types.named_struct(node(1), AccessCapability::Const);
        let mut_named = types.named_struct(node(1), AccessCapability::Mut);
        let const_anonymous = types.anonymous_struct(node(2), AccessCapability::Const);
        let mut_anonymous = types.anonymous_struct(node(2), AccessCapability::Mut);
        let const_interface = types.interface(node(3), AccessCapability::Const);
        let mut_interface = types.interface(node(3), AccessCapability::Mut);
        let const_builtin = types.builtin(
            BuiltinType::Queue,
            vec![parameter],
            AccessCapability::Const,
        );
        let mut_builtin = types.builtin(
            BuiltinType::Queue,
            vec![parameter],
            AccessCapability::Mut,
        );

        for (const_type, mut_type) in [
            (const_callable, mut_callable),
            (const_named, mut_named),
            (const_anonymous, mut_anonymous),
            (const_interface, mut_interface),
            (const_builtin, mut_builtin),
        ] {
            assert_eq!(
                types.with_capability(const_type, AccessCapability::Mut),
                Some(mut_type)
            );
            assert_eq!(
                types.with_capability(mut_type, AccessCapability::Const),
                Some(const_type)
            );
        }
    }

    #[test]
    fn internal_types_are_unchanged_by_capability_replacement() {
        let mut types = TypeStore::new();

        for id in [types.recovery(), types.divergence()] {
            assert_eq!(
                types.with_capability(id, AccessCapability::Const),
                Some(id)
            );
            assert_eq!(
                types.with_capability(id, AccessCapability::Mut),
                Some(id)
            );
        }
    }

    #[test]
    fn unknown_type_ids_are_rejected_without_panicking() {
        let mut types = TypeStore::new();
        let unknown = TypeId(usize::MAX);

        assert_eq!(types.get(unknown), None);
        assert_eq!(
            types.with_capability(unknown, AccessCapability::Mut),
            None
        );
    }
}
