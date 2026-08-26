//! Defines the compiler's canonical semantic type representation.
//!
//! The [`TypeStore`] interns program-local type identities and provides the
//! capability, storage, copying, and value-transfer metadata used by later
//! semantic analysis and lowering. This module defines and canonicalizes types;
//! resolving source [`TypeSyntax`](crate::ast::TypeSyntax), checking expressions,
//! assignability, and escape behavior are performed by separate passes.

use std::collections::HashMap;

use crate::ast::{BuiltinType, NodeId, PrimitiveType, ValueCapability};

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

impl From<ValueCapability> for AccessCapability {
    fn from(value: ValueCapability) -> Self {
        match value {
            ValueCapability::Const => Self::Const,
            ValueCapability::Mut => Self::Mut,
        }
    }
}

/// Where a value's independently owned storage resides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageSemantics {
    /// Statically sized storage owned by a frame, aggregate, or return slot.
    Inline,
    /// An erased, non-escaping view whose concrete storage is owned elsewhere.
    BorrowedView,
    /// A stable reference to an independently traced GC allocation.
    Gc,
}

/// The compiler-defined behavior of the reserved `.copy()` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopySemantics {
    /// The representation can be copied directly without changing identity.
    Trivial,
    /// Inline members are copied recursively while nested GC references remain
    /// shared.
    Recursive,
    /// Explicit `.copy()` materializes a plain copy of the GC payload. A GC
    /// reference encountered as a nested field of another recursive copy is
    /// still copied trivially and remains shared.
    GcPayload,
    /// The erased value cannot cross an owning boundary without GC storage.
    NonEscapingErasedView,
}

/// The provenance category attached to a typed expression or place. This is
/// deliberately separate from [`SemanticType`]: the same plain `T` may denote
/// owned inline storage or a borrow of storage owned elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueCategory {
    FreshTemporary,
    OwnedInlinePlace,
    BorrowedPlace,
    GcReference,
}

/// A transfer selected by type checking and consumed by escape analysis and
/// typed-IR lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueTransfer {
    TrivialCopy,
    Borrow,
    MoveTemporary,
    RecursiveCopy,
    AllocateGc,
    CopyGcReference,
}

/// Owning/retaining destinations checked by the per-function escape pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EscapeDestination {
    GcReturn,
    GcParameter,
    RetainedField,
    QueueElement,
    EscapingCapture,
    GcReceiver,
}

/// A canonical semantic type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SemanticType {
    /// An escapable GC reference. Repeated qualification is normalized by
    /// [`TypeStore::gc`].
    Gc {
        target: TypeId,
        capability: AccessCapability,
    },
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
    /// A canonical set of at least two alternative member types.
    Union {
        members: Vec<TypeId>,
        capability: AccessCapability,
    },
    /// A canonical set of at least two simultaneously required member types.
    Intersection {
        members: Vec<TypeId>,
        capability: AccessCapability,
    },
    /// An invalid type used after emitting a diagnostic so checking can
    /// continue without producing cascading errors.
    Recovery,
    /// The type of a valid expression or path that never produces a value.
    Divergence,
}

impl SemanticType {
    /// Returns the capability attached directly to this type, if it has one.
    #[must_use]
    pub const fn capability(&self) -> Option<AccessCapability> {
        match self {
            Self::Gc { capability, .. }
            | Self::Primitive { capability, .. }
            | Self::Callable { capability, .. }
            | Self::NamedStruct { capability, .. }
            | Self::AnonymousStruct { capability, .. }
            | Self::Interface { capability, .. }
            | Self::Builtin { capability, .. }
            | Self::Union { capability, .. }
            | Self::Intersection { capability, .. } => Some(*capability),
            Self::Recovery | Self::Divergence => None,
        }
    }

    /// Returns the storage representation required by this type. Whether a
    /// plain value is owned or borrowed is recorded separately as a
    /// [`ValueCategory`].
    #[must_use]
    pub const fn storage_semantics(&self) -> Option<StorageSemantics> {
        match self {
            Self::Gc { .. } => Some(StorageSemantics::Gc),
            Self::Callable { .. } | Self::Interface { .. } | Self::Intersection { .. } => {
                Some(StorageSemantics::BorrowedView)
            }
            Self::Primitive { .. }
            | Self::NamedStruct { .. }
            | Self::AnonymousStruct { .. }
            | Self::Builtin { .. }
            | Self::Union { .. } => Some(StorageSemantics::Inline),
            Self::Recovery | Self::Divergence => None,
        }
    }

    #[must_use]
    pub const fn copy_semantics(&self) -> Option<CopySemantics> {
        match self {
            Self::Gc { .. } => Some(CopySemantics::GcPayload),
            Self::Primitive {
                primitive:
                    PrimitiveType::Unit
                    | PrimitiveType::None
                    | PrimitiveType::Int
                    | PrimitiveType::Float
                    | PrimitiveType::Bool
                    | PrimitiveType::Char,
                ..
            } => Some(CopySemantics::Trivial),
            Self::Callable { .. } | Self::Interface { .. } | Self::Intersection { .. } => {
                Some(CopySemantics::NonEscapingErasedView)
            }
            Self::Primitive { .. }
            | Self::NamedStruct { .. }
            | Self::AnonymousStruct { .. }
            | Self::Builtin { .. }
            | Self::Union { .. } => Some(CopySemantics::Recursive),
            Self::Recovery | Self::Divergence => None,
        }
    }

    fn has_same_shape(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Gc { target: left, .. },
                Self::Gc { target: right, .. },
            ) => left == right,
            (
                Self::Primitive {
                    primitive: left, ..
                },
                Self::Primitive {
                    primitive: right, ..
                },
            ) => left == right,
            (
                Self::Callable {
                    parameters: left_parameters,
                    return_type: left_return,
                    ..
                },
                Self::Callable {
                    parameters: right_parameters,
                    return_type: right_return,
                    ..
                },
            ) => left_parameters == right_parameters && left_return == right_return,
            (
                Self::NamedStruct {
                    declaration: left, ..
                },
                Self::NamedStruct {
                    declaration: right, ..
                },
            )
            | (
                Self::Interface {
                    declaration: left, ..
                },
                Self::Interface {
                    declaration: right, ..
                },
            ) => left == right,
            (
                Self::AnonymousStruct {
                    expression: left, ..
                },
                Self::AnonymousStruct {
                    expression: right, ..
                },
            ) => left == right,
            (
                Self::Builtin {
                    builtin: left_builtin,
                    arguments: left_arguments,
                    ..
                },
                Self::Builtin {
                    builtin: right_builtin,
                    arguments: right_arguments,
                    ..
                },
            ) => left_builtin == right_builtin && left_arguments == right_arguments,
            (Self::Union { members: left, .. }, Self::Union { members: right, .. })
            | (
                Self::Intersection { members: left, .. },
                Self::Intersection { members: right, .. },
            ) => left == right,
            (Self::Recovery, Self::Recovery) | (Self::Divergence, Self::Divergence) => true,
            _ => false,
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

    /// Returns the canonical escapable GC reference for `target`.
    ///
    /// GC qualification is idempotent. Its access capability mirrors the
    /// target capability so `&T` and `&mut T` remain distinct without making
    /// GC ownership itself mutable.
    pub fn gc(&mut self, target: TypeId) -> Option<TypeId> {
        let semantic_type = self.get(target)?.clone();
        if matches!(
            semantic_type,
            SemanticType::Recovery | SemanticType::Divergence
        ) {
            return Some(target);
        }
        if matches!(semantic_type, SemanticType::Gc { .. }) {
            return Some(target);
        }
        let capability = semantic_type.capability()?;
        Some(self.intern(SemanticType::Gc { target, capability }))
    }

    /// Returns the plain target of a GC-qualified type, or `None` when `id` is
    /// valid but plain. An unknown ID also returns `None`; use [`Self::contains`]
    /// when that distinction matters.
    #[must_use]
    pub fn gc_target(&self, id: TypeId) -> Option<TypeId> {
        match self.get(id)? {
            SemanticType::Gc { target, .. } => Some(*target),
            _ => None,
        }
    }

    /// Returns the canonical identity for a capability-qualified primitive.
    pub fn primitive(&mut self, primitive: PrimitiveType, capability: AccessCapability) -> TypeId {
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
    pub fn named_struct(&mut self, declaration: NodeId, capability: AccessCapability) -> TypeId {
        self.intern(SemanticType::NamedStruct {
            declaration,
            capability,
        })
    }

    /// Returns the canonical identity for an anonymous nominal struct.
    pub fn anonymous_struct(&mut self, expression: NodeId, capability: AccessCapability) -> TypeId {
        self.intern(SemanticType::AnonymousStruct {
            expression,
            capability,
        })
    }

    /// Returns the canonical identity for a declared structural interface.
    pub fn interface(&mut self, declaration: NodeId, capability: AccessCapability) -> TypeId {
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

    /// Returns the canonical identity for a union of the supplied members.
    ///
    /// Union construction is associative, commutative, and idempotent: nested
    /// unions are flattened, exact duplicate members are removed, and member
    /// order does not affect identity. A single remaining member is returned
    /// with the requested outer capability. At least one member must be
    /// supplied.
    pub fn union(&mut self, members: Vec<TypeId>, capability: AccessCapability) -> TypeId {
        let members = self.normalize_members(members, TypeSetKind::Union);
        self.intern_normalized_type_set(members, TypeSetKind::Union, capability)
    }

    /// Returns the canonical identity for an intersection of the supplied members.
    ///
    /// Intersection construction is associative, commutative, and idempotent:
    /// nested intersections are flattened, exact duplicate members are
    /// removed, and member order does not affect identity. A single remaining
    /// member is returned with the requested outer capability. Member legality
    /// is checked during source type resolution. At least one member must be
    /// supplied.
    pub fn intersection(&mut self, members: Vec<TypeId>, capability: AccessCapability) -> TypeId {
        let members = self.normalize_members(members, TypeSetKind::Intersection);
        self.intern_normalized_type_set(members, TypeSetKind::Intersection, capability)
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

    /// Returns whether an ID resolves to a type in this store.
    ///
    /// Like every [`TypeId`] operation, this assumes the ID originated from
    /// this store. IDs from different stores are not interchangeable.
    #[must_use]
    pub fn contains(&self, id: TypeId) -> bool {
        self.get(id).is_some()
    }

    /// Tests exact canonical type identity.
    ///
    /// Capability is part of identity. `None` is returned if either ID is
    /// unknown to this store.
    #[must_use]
    pub fn is_identical(&self, left: TypeId, right: TypeId) -> Option<bool> {
        self.get(left)?;
        self.get(right)?;
        Some(left == right)
    }

    /// Tests equality while ignoring only the types' outer capabilities.
    ///
    /// The payload capability of an outer GC qualifier is treated as that
    /// qualifier's outer capability. Capabilities on nested callable
    /// parameters, return types, built-in type arguments, and union or
    /// intersection members remain significant. This
    /// is structural comparison of canonical representations, not
    /// assignability or an implicit-conversion check. `None` is returned if
    /// either ID is unknown to this store.
    #[must_use]
    pub fn has_same_shape(&self, left: TypeId, right: TypeId) -> Option<bool> {
        match (self.get(left)?, self.get(right)?) {
            (
                SemanticType::Gc {
                    target: left_target,
                    ..
                },
                SemanticType::Gc {
                    target: right_target,
                    ..
                },
            ) => self.has_same_shape(*left_target, *right_target),
            (left, right) => Some(left.has_same_shape(right)),
        }
    }

    /// Returns the canonical form of a type with the requested capability.
    ///
    /// This operation constructs a type; it does not decide whether increasing
    /// a capability is legal at a particular use site. Union and intersection
    /// members are unchanged because the aggregate carries its own outer
    /// capability. Recovery and divergence do not represent values and are
    /// returned unchanged.
    pub fn with_capability(&mut self, id: TypeId, capability: AccessCapability) -> Option<TypeId> {
        match self.get(id)?.clone() {
            SemanticType::Gc { target, .. } => {
                let target = self.with_capability(target, capability)?;
                self.gc(target)
            }
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
            SemanticType::Union { members, .. } => Some(self.union(members, capability)),
            SemanticType::Intersection { members, .. } => {
                Some(self.intersection(members, capability))
            }
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

    fn normalize_members(&self, members: Vec<TypeId>, kind: TypeSetKind) -> Vec<TypeId> {
        assert!(
            !members.is_empty(),
            "a union or intersection requires at least one member"
        );

        let mut normalized = Vec::new();
        for member in members {
            match (kind, self.get(member)) {
                (TypeSetKind::Union, Some(SemanticType::Union { members, .. }))
                | (TypeSetKind::Intersection, Some(SemanticType::Intersection { members, .. })) => {
                    normalized.extend(members.iter().copied())
                }
                _ => normalized.push(member),
            }
        }

        normalized.sort_unstable_by_key(|member| member.0);
        normalized.dedup();
        normalized
    }

    fn intern_normalized_type_set(
        &mut self,
        members: Vec<TypeId>,
        kind: TypeSetKind,
        capability: AccessCapability,
    ) -> TypeId {
        if let [member] = members.as_slice() {
            return self
                .with_capability(*member, capability)
                .expect("a normalized member belongs to this type store");
        }

        match kind {
            TypeSetKind::Union => self.intern(SemanticType::Union {
                members,
                capability,
            }),
            TypeSetKind::Intersection => self.intern(SemanticType::Intersection {
                members,
                capability,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeSetKind {
    Union,
    Intersection,
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
    fn source_value_capabilities_map_to_semantic_capabilities() {
        assert_eq!(
            AccessCapability::from(ValueCapability::Const),
            AccessCapability::Const
        );
        assert_eq!(
            AccessCapability::from(ValueCapability::Mut),
            AccessCapability::Mut
        );
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
    fn primitive_metadata_distinguishes_trivial_and_recursive_copies() {
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
            assert_eq!(
                semantic_type.storage_semantics(),
                Some(StorageSemantics::Inline)
            );
            assert_eq!(semantic_type.copy_semantics(), Some(CopySemantics::Trivial));
        }

        for primitive in [PrimitiveType::String, PrimitiveType::Bytes] {
            let id = types.primitive(primitive, AccessCapability::Mut);
            let semantic_type = types.get(id).expect("primitive should be interned");

            assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
            assert_eq!(
                semantic_type.storage_semantics(),
                Some(StorageSemantics::Inline)
            );
            assert_eq!(
                semantic_type.copy_semantics(),
                Some(CopySemantics::Recursive)
            );
        }
    }

    #[test]
    fn repeated_compound_and_declared_type_construction_reuses_canonical_identities() {
        let mut types = TypeStore::new();
        let parameter = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let return_type = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let callable = types.callable(vec![parameter], return_type, AccessCapability::Const);
        assert_eq!(
            types.callable(vec![parameter], return_type, AccessCapability::Const,),
            callable
        );

        let named = types.named_struct(node(1), AccessCapability::Const);
        assert_eq!(types.named_struct(node(1), AccessCapability::Const), named);

        let anonymous = types.anonymous_struct(node(2), AccessCapability::Const);
        assert_eq!(
            types.anonymous_struct(node(2), AccessCapability::Const),
            anonymous
        );

        let interface = types.interface(node(3), AccessCapability::Const);
        assert_eq!(types.interface(node(3), AccessCapability::Const), interface);

        let builtin = types.builtin(BuiltinType::Queue, vec![parameter], AccessCapability::Const);
        assert_eq!(
            types.builtin(BuiltinType::Queue, vec![parameter], AccessCapability::Const,),
            builtin
        );
    }

    #[test]
    fn gc_allocation_is_explicit_idempotent_and_capability_qualified() {
        let mut types = TypeStore::new();
        let plain = types.named_struct(node(1), AccessCapability::Const);
        let mutable_plain = types.named_struct(node(1), AccessCapability::Mut);
        let gc = types
            .gc(plain)
            .expect("plain values can be GC qualified");
        let mutable_gc = types
            .gc(mutable_plain)
            .expect("mutable plain values can be GC qualified");

        assert_eq!(types.gc(gc), Some(gc));
        assert_eq!(types.gc_target(gc), Some(plain));
        assert_eq!(types.gc_target(plain), None);
        assert_ne!(gc, mutable_gc);
        assert_eq!(types.has_same_shape(gc, mutable_gc), Some(true));
        assert_eq!(
            types.with_capability(gc, AccessCapability::Mut),
            Some(mutable_gc)
        );
        assert_eq!(
            types.get(gc).and_then(SemanticType::storage_semantics),
            Some(StorageSemantics::Gc)
        );
        assert_eq!(
            types.get(gc).and_then(SemanticType::copy_semantics),
            Some(CopySemantics::GcPayload)
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
        let different_return = types.callable(vec![int, string], int, AccessCapability::Const);

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

        let map = types.builtin(BuiltinType::Map, vec![int, string], AccessCapability::Const);
        let reversed = types.builtin(BuiltinType::Map, vec![string, int], AccessCapability::Const);
        let mutable = types.builtin(BuiltinType::Map, vec![int, string], AccessCapability::Mut);
        let queue = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Const);
        let vector = types.builtin(BuiltinType::Vector, vec![int], AccessCapability::Const);

        assert_ne!(map, reversed);
        assert_ne!(map, mutable);
        assert_ne!(queue, vector);
    }

    #[test]
    fn compound_and_declared_type_metadata_tracks_storage_and_copy_semantics() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let unit = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let callable = types.callable(vec![int], unit, AccessCapability::Mut);
        let named = types.named_struct(node(1), AccessCapability::Mut);
        let anonymous = types.anonymous_struct(node(2), AccessCapability::Mut);
        let interface = types.interface(node(3), AccessCapability::Mut);
        let queue = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Mut);
        let vector = types.builtin(BuiltinType::Vector, vec![int], AccessCapability::Mut);
        let map = types.builtin(BuiltinType::Map, vec![int, int], AccessCapability::Mut);

        for id in [named, anonymous, queue, vector, map] {
            let semantic_type = types.get(id).expect("type should be interned");
            assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
            assert_eq!(
                semantic_type.storage_semantics(),
                Some(StorageSemantics::Inline)
            );
            assert_eq!(
                semantic_type.copy_semantics(),
                Some(CopySemantics::Recursive)
            );
        }

        for id in [callable, interface] {
            let semantic_type = types.get(id).expect("type should be interned");
            assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
            assert_eq!(
                semantic_type.storage_semantics(),
                Some(StorageSemantics::BorrowedView)
            );
            assert_eq!(
                semantic_type.copy_semantics(),
                Some(CopySemantics::NonEscapingErasedView)
            );
        }

        let error = types.builtin(BuiltinType::Error, vec![int], AccessCapability::Mut);
        let semantic_type = types.get(error).expect("Error should be interned");
        assert_eq!(semantic_type.capability(), Some(AccessCapability::Mut));
        assert_eq!(
            semantic_type.storage_semantics(),
            Some(StorageSemantics::Inline)
        );
    }

    #[test]
    fn union_identity_is_order_independent_and_reuses_the_canonical_type() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let string = types.primitive(PrimitiveType::String, AccessCapability::Const);

        let forward = types.union(vec![int, string], AccessCapability::Const);
        let reversed = types.union(vec![string, int], AccessCapability::Const);

        assert_eq!(forward, reversed);
        assert_eq!(
            types.get(forward),
            Some(&SemanticType::Union {
                members: vec![int, string],
                capability: AccessCapability::Const,
            })
        );
    }

    #[test]
    fn intersection_identity_is_order_independent_and_reuses_the_canonical_type() {
        let mut types = TypeStore::new();
        let reader = types.interface(node(1), AccessCapability::Const);
        let writer = types.interface(node(2), AccessCapability::Const);

        let forward = types.intersection(vec![reader, writer], AccessCapability::Const);
        let reversed = types.intersection(vec![writer, reader], AccessCapability::Const);

        assert_eq!(forward, reversed);
        assert_eq!(
            types.get(forward),
            Some(&SemanticType::Intersection {
                members: vec![reader, writer],
                capability: AccessCapability::Const,
            })
        );
    }

    #[test]
    fn unions_are_flattened_deduplicated_and_collapsed() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let mut_int = types.primitive(PrimitiveType::Int, AccessCapability::Mut);
        let string = types.primitive(PrimitiveType::String, AccessCapability::Const);
        let none = types.primitive(PrimitiveType::None, AccessCapability::Const);

        let nested = types.union(vec![int, string], AccessCapability::Const);
        let flattened = types.union(vec![none, nested, int], AccessCapability::Const);
        let direct = types.union(vec![string, none, int], AccessCapability::Const);

        assert_eq!(flattened, direct);
        assert_eq!(types.union(vec![int, int], AccessCapability::Const), int);
        assert_eq!(types.union(vec![string], AccessCapability::Const), string);
        assert_eq!(types.union(vec![int], AccessCapability::Mut), mut_int);
    }

    #[test]
    fn intersections_are_flattened_deduplicated_and_collapsed() {
        let mut types = TypeStore::new();
        let reader = types.interface(node(1), AccessCapability::Const);
        let mut_reader = types.interface(node(1), AccessCapability::Mut);
        let writer = types.interface(node(2), AccessCapability::Const);
        let closer = types.interface(node(3), AccessCapability::Const);

        let nested = types.intersection(vec![reader, writer], AccessCapability::Const);
        let flattened = types.intersection(vec![closer, nested, reader], AccessCapability::Const);
        let direct = types.intersection(vec![writer, closer, reader], AccessCapability::Const);

        assert_eq!(flattened, direct);
        assert_eq!(
            types.intersection(vec![reader, reader], AccessCapability::Const),
            reader
        );
        assert_eq!(
            types.intersection(vec![writer], AccessCapability::Const),
            writer
        );
        assert_eq!(
            types.intersection(vec![reader], AccessCapability::Mut),
            mut_reader
        );
    }

    #[test]
    fn normalization_only_flattens_the_same_type_operator() {
        let mut types = TypeStore::new();
        let reader = types.interface(node(1), AccessCapability::Const);
        let writer = types.interface(node(2), AccessCapability::Const);
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);

        let intersection = types.intersection(vec![reader, writer], AccessCapability::Const);
        let union = types.union(vec![intersection, int], AccessCapability::Const);

        let Some(SemanticType::Union { members, .. }) = types.get(union) else {
            panic!("expected a union");
        };
        assert_eq!(members.len(), 2);
        assert!(members.contains(&intersection));
        assert!(members.contains(&int));
    }

    #[test]
    fn type_set_identity_includes_its_outer_capability() {
        let mut types = TypeStore::new();
        let first = types.named_struct(node(1), AccessCapability::Const);
        let second = types.named_struct(node(2), AccessCapability::Const);

        let const_union = types.union(vec![first, second], AccessCapability::Const);
        let mut_union = types.union(vec![first, second], AccessCapability::Mut);
        let const_intersection = types.intersection(vec![first, second], AccessCapability::Const);
        let mut_intersection = types.intersection(vec![first, second], AccessCapability::Mut);

        assert_ne!(const_union, mut_union);
        assert_ne!(const_intersection, mut_intersection);
        assert_ne!(const_union, const_intersection);
        assert_eq!(
            types.get(mut_union),
            Some(&SemanticType::Union {
                members: vec![first, second],
                capability: AccessCapability::Mut,
            })
        );
    }

    #[test]
    fn type_set_metadata_reflects_aggregate_runtime_representation() {
        let mut types = TypeStore::new();
        let reader = types.interface(node(1), AccessCapability::Const);
        let writer = types.interface(node(2), AccessCapability::Const);
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);

        let union = types.union(vec![reader, int], AccessCapability::Mut);
        let intersection = types.intersection(vec![reader, writer], AccessCapability::Mut);

        let union_type = types.get(union).expect("union should be interned");
        assert_eq!(union_type.capability(), Some(AccessCapability::Mut));
        assert_eq!(
            union_type.storage_semantics(),
            Some(StorageSemantics::Inline)
        );

        let intersection_type = types
            .get(intersection)
            .expect("intersection should be interned");
        assert_eq!(intersection_type.capability(), Some(AccessCapability::Mut));
        assert_eq!(
            intersection_type.storage_semantics(),
            Some(StorageSemantics::BorrowedView)
        );
    }

    #[test]
    fn capability_replacement_preserves_type_set_members() {
        let mut types = TypeStore::new();
        let first = types.named_struct(node(1), AccessCapability::Const);
        let second = types.named_struct(node(2), AccessCapability::Const);
        let const_union = types.union(vec![first, second], AccessCapability::Const);
        let mut_union = types.union(vec![first, second], AccessCapability::Mut);
        let const_intersection = types.intersection(vec![first, second], AccessCapability::Const);
        let mut_intersection = types.intersection(vec![first, second], AccessCapability::Mut);

        assert_eq!(
            types.with_capability(const_union, AccessCapability::Mut),
            Some(mut_union)
        );
        assert_eq!(
            types.with_capability(mut_union, AccessCapability::Const),
            Some(const_union)
        );
        assert_eq!(
            types.with_capability(const_intersection, AccessCapability::Mut),
            Some(mut_intersection)
        );
        assert_eq!(
            types.with_capability(mut_intersection, AccessCapability::Const),
            Some(const_intersection)
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
            assert_eq!(semantic_type.storage_semantics(), None);
            assert_eq!(semantic_type.copy_semantics(), None);
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

        let const_callable = types.callable(vec![parameter], return_type, AccessCapability::Const);
        let mut_callable = types.callable(vec![parameter], return_type, AccessCapability::Mut);
        let const_named = types.named_struct(node(1), AccessCapability::Const);
        let mut_named = types.named_struct(node(1), AccessCapability::Mut);
        let const_anonymous = types.anonymous_struct(node(2), AccessCapability::Const);
        let mut_anonymous = types.anonymous_struct(node(2), AccessCapability::Mut);
        let const_interface = types.interface(node(3), AccessCapability::Const);
        let mut_interface = types.interface(node(3), AccessCapability::Mut);
        let const_builtin =
            types.builtin(BuiltinType::Queue, vec![parameter], AccessCapability::Const);
        let mut_builtin = types.builtin(BuiltinType::Queue, vec![parameter], AccessCapability::Mut);

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
            assert_eq!(types.with_capability(id, AccessCapability::Const), Some(id));
            assert_eq!(types.with_capability(id, AccessCapability::Mut), Some(id));
        }
    }

    #[test]
    fn exact_identity_is_store_validated_and_capability_sensitive() {
        let mut types = TypeStore::new();
        let const_int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let same_const_int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let mut_int = types.primitive(PrimitiveType::Int, AccessCapability::Mut);
        let string = types.primitive(PrimitiveType::String, AccessCapability::Const);
        let unknown = TypeId(usize::MAX);

        assert_eq!(types.is_identical(const_int, same_const_int), Some(true));
        assert_eq!(types.is_identical(const_int, mut_int), Some(false));
        assert_eq!(types.is_identical(const_int, string), Some(false));
        assert_eq!(types.is_identical(const_int, unknown), None);
        assert_eq!(types.is_identical(unknown, const_int), None);
    }

    #[test]
    fn same_shape_ignores_the_outer_capability_of_every_value_type() {
        let mut types = TypeStore::new();
        let int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let unit = types.primitive(PrimitiveType::Unit, AccessCapability::Const);
        let first_interface = types.interface(node(3), AccessCapability::Const);
        let second_interface = types.interface(node(4), AccessCapability::Const);

        let const_callable = types.callable(vec![int], unit, AccessCapability::Const);
        let mut_callable = types.callable(vec![int], unit, AccessCapability::Mut);
        let const_named = types.named_struct(node(1), AccessCapability::Const);
        let mut_named = types.named_struct(node(1), AccessCapability::Mut);
        let const_anonymous = types.anonymous_struct(node(2), AccessCapability::Const);
        let mut_anonymous = types.anonymous_struct(node(2), AccessCapability::Mut);
        let const_interface = types.interface(node(3), AccessCapability::Const);
        let mut_interface = types.interface(node(3), AccessCapability::Mut);
        let const_builtin = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Const);
        let mut_builtin = types.builtin(BuiltinType::Queue, vec![int], AccessCapability::Mut);
        let const_union = types.union(vec![int, first_interface], AccessCapability::Const);
        let mut_union = types.union(vec![int, first_interface], AccessCapability::Mut);
        let const_intersection = types.intersection(
            vec![first_interface, second_interface],
            AccessCapability::Const,
        );
        let mut_intersection = types.intersection(
            vec![first_interface, second_interface],
            AccessCapability::Mut,
        );
        let mut_int = types.primitive(PrimitiveType::Int, AccessCapability::Mut);

        for (const_type, mut_type) in [
            (int, mut_int),
            (const_callable, mut_callable),
            (const_named, mut_named),
            (const_anonymous, mut_anonymous),
            (const_interface, mut_interface),
            (const_builtin, mut_builtin),
            (const_union, mut_union),
            (const_intersection, mut_intersection),
        ] {
            assert_eq!(types.has_same_shape(const_type, mut_type), Some(true));
        }

        assert_eq!(
            types.has_same_shape(types.recovery(), types.recovery()),
            Some(true)
        );
        assert_eq!(
            types.has_same_shape(types.divergence(), types.divergence()),
            Some(true)
        );
        assert_eq!(
            types.has_same_shape(types.recovery(), types.divergence()),
            Some(false)
        );
    }

    #[test]
    fn same_shape_preserves_nested_capabilities_and_nominal_identity() {
        let mut types = TypeStore::new();
        let const_int = types.primitive(PrimitiveType::Int, AccessCapability::Const);
        let mut_int = types.primitive(PrimitiveType::Int, AccessCapability::Mut);
        let unit = types.primitive(PrimitiveType::Unit, AccessCapability::Const);

        let const_parameter = types.callable(vec![const_int], unit, AccessCapability::Const);
        let mut_parameter = types.callable(vec![mut_int], unit, AccessCapability::Const);
        assert_eq!(
            types.has_same_shape(const_parameter, mut_parameter),
            Some(false)
        );

        let const_argument =
            types.builtin(BuiltinType::Queue, vec![const_int], AccessCapability::Const);
        let mut_argument =
            types.builtin(BuiltinType::Queue, vec![mut_int], AccessCapability::Const);
        assert_eq!(
            types.has_same_shape(const_argument, mut_argument),
            Some(false)
        );

        let first_named = types.named_struct(node(1), AccessCapability::Const);
        let second_named = types.named_struct(node(2), AccessCapability::Const);
        assert_eq!(types.has_same_shape(first_named, second_named), Some(false));

        let const_member_union = types.union(vec![const_int, first_named], AccessCapability::Const);
        let mut_member_union = types.union(vec![mut_int, first_named], AccessCapability::Const);
        assert_eq!(
            types.has_same_shape(const_member_union, mut_member_union),
            Some(false)
        );
    }

    #[test]
    fn unknown_type_ids_are_rejected_without_panicking() {
        let mut types = TypeStore::new();
        let unknown = TypeId(usize::MAX);

        assert_eq!(types.get(unknown), None);
        assert!(!types.contains(unknown));
        assert_eq!(types.has_same_shape(types.recovery(), unknown), None);
        assert_eq!(types.with_capability(unknown, AccessCapability::Mut), None);
    }
}
