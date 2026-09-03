//! Internal foundation for core expression type checking.
//!
//! This module intentionally has no public whole-program entry point yet. It
//! records expression, binding, call, callable-result, control-flow, and
//! contextual union, narrowing, and runtime tag-lock facts needed by later
//! increments without treating not-yet-implemented expression forms as source
//! errors.

// The whole-program entry point remains intentionally private and unconnected
// until the core-expression phase covers every expression form in its scope.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use crate::{
    ast::{
        AnonymousStructMember, AssignmentOperator, BinaryOperator, BindingMutability,
        BindingQualifiers, Block, BuiltinType, ConditionalElse, Declaration, Expression,
        ExpressionKind, FormattedStringPart, Function, FunctionParameter, FunctionParameterKind,
        LiteralKind, NodeId, PrimitiveType, Program, ReceiverStorage, Statement, StatementKind,
        StructFieldInitializer, StructMember, TypeSyntax, UnaryOperator, ValueCapability,
    },
    context_resolution::ContextResolution,
    name_resolution::NameResolution,
    semantic_types::{
        AccessCapability, CopySemantics, SemanticType, StorageSemantics, TypeId, TypeStore,
        ValueCategory, ValueTransfer,
    },
    signature_collection::{
        BuiltinGlobalSignature, BuiltinMemberOwner, BuiltinMemberSignature, BuiltinNamespace,
        CallableSignature,
        InterfaceRequirementSignature, MethodId, ReceiverSignature, SignatureCollection,
        StructMemberSignatureKind, StructSignature,
    },
    source::{SourceModule, Span},
    symbol_table::{SymbolId, SymbolKind},
    type_resolution::{RuntimeMemberTemplateCall, TypeResolution},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Describes how control leaves one statement. Blocks only continue through
/// `Completes`; the other variants retain the reason so loop transfers can be
/// associated with their resolved target while returns remain callable exits.
enum StatementFlow {
    Completes,
    Returns,
    Breaks(NodeId),
    Continues(NodeId),
    /// Evaluation never completes this statement normally and therefore cannot
    /// proceed to the following statement. This is the conventional compiler
    /// and type-theory meaning of "diverges" and corresponds to Rust's `!`
    /// (never) type. An endless `loop {}` diverges; so does `loop { return; }`
    /// as an expression, because the loop itself never produces a local result.
    /// `Returns` is more specific: it records that this statement is the direct
    /// return transferring control from the callable.
    Diverges,
}

impl StatementFlow {
    const fn can_complete_normally(self) -> bool {
        matches!(self, Self::Completes)
    }
}

#[derive(Debug, Clone)]
/// One reachable way to leave a loop with a result. A valued break records its
/// expression node, while a bare break or implicit natural completion has no
/// expression node but still contributes unit and a post-path binding state.
struct LoopResultPath {
    value: Option<NodeId>,
    span: Span,
    typed: TypedExpression,
    categories: HashMap<SymbolId, ValueCategory>,
    tracked_bindings: TrackedBindingState,
    narrowings: NarrowingState,
}

#[derive(Debug, Clone)]
/// Accumulates reachable transfers while one loop body is being analyzed.
/// Context resolution supplies the target identity, so an inner loop cannot
/// accidentally contribute a break or continue to an outer loop.
struct ActiveLoop {
    expression: NodeId,
    body: NodeId,
    expected_result_type: Option<TypeId>,
    breaks: Vec<LoopResultPath>,
    continues: Vec<(
        HashMap<SymbolId, ValueCategory>,
        TrackedBindingState,
        NarrowingState,
    )>,
    entry_narrowings: NarrowingState,
}

#[derive(Debug, Clone)]
struct LoopIterationOutcome {
    breaks: Vec<LoopResultPath>,
    natural_categories: Option<HashMap<SymbolId, ValueCategory>>,
    natural_tracked_bindings: Option<TrackedBindingState>,
    natural_narrowings: Option<NarrowingState>,
    invalid: bool,
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

/// Records widening from one union to another whose member set is a superset.
/// Every source member has the identical canonical `TypeId` in the destination;
/// lowering uses those shared identities to remap only the runtime tag and
/// shallow-copy the active payload.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnionWidening {
    source_union: TypeId,
    destination_union: TypeId,
}

/// Records the one covariant conversion supported by a parameterized built-in.
/// The payload identities are retained explicitly so lowering does not need to
/// rediscover the relationship between the two `Error` applications.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ErrorWidening {
    source_error: TypeId,
    destination_error: TypeId,
    source_payload: TypeId,
    destination_payload: TypeId,
    payload_assignment: Box<ContextualAssignment>,
}

/// Describes formation of an erased interface view. This is not a conversion
/// of the concrete object: lowering preserves its address and concrete vtable.
/// Source alternatives and destination method requirements remain canonical
/// type metadata and are recovered through the two `TypeId`s. Only the source
/// expression's provenance must be retained separately because it is not part
/// of its semantic type.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterfaceView {
    source_type: TypeId,
    source_category: ValueCategory,
    destination_type: TypeId,
}

/// The stable physical storage from which a tracked reference is formed.
/// Expression roots cover temporaries and call results; named storage and
/// `self` use semantic identities so repeated source reads denote one owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PhysicalPlaceRoot {
    Symbol(SymbolId),
    /// Backing storage retained after its source-level reference slot was
    /// redirected. It remains stable but no longer denotes later symbol reads.
    DisplacedSymbol(SymbolId, NodeId),
    SelfValue(NodeId),
    Expression(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PhysicalPlaceProjection {
    Field(NodeId),
    TupleElement(usize),
    BuiltinErrorValue,
    /// A callable links this result to an input without exposing the private
    /// interior path selected by its implementation.
    OpaqueDerived,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PhysicalPlace {
    root: PhysicalPlaceRoot,
    projections: Vec<PhysicalPlaceProjection>,
    storage: ValueCategory,
}

/// Records the implicit conversion from plain or GC-backed storage to a
/// tracked reference. Callable lifetime linking consumes this provenance to
/// connect returned references to caller-owned storage.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedBorrow {
    source: PhysicalPlace,
    source_type: TypeId,
    target_type: TypeId,
}

/// A call-only view which lets a tracked reference supply the address expected
/// by an ordinary by-reference aggregate parameter. This is deliberately not
/// part of general contextual assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackedParameterBorrow {
    source_type: TypeId,
    parameter_type: TypeId,
}

/// The complete set of physical places which bound a tracked reference or an
/// inline value containing one. Multiple places represent the conservative
/// intersection carried through aggregates, unions, and callable boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedLifetimeLink {
    sources: Vec<PhysicalPlace>,
}

/// A stable source-level route to physical union storage. Expression node IDs
/// are deliberately absent: two reads such as `holder.value` must identify the
/// same place so a narrowing established by one read applies to the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NarrowingPlace {
    root: NarrowingRoot,
    fields: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NarrowingRoot {
    Symbol(SymbolId),
    SelfValue(NodeId),
}

/// One live proof that a union place has the indicated member or member subset.
/// Multiple entries for one place are intentional: nested tests and aliases
/// acquire independent runtime tag locks even when they prove the same type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NarrowingFact {
    source_union: TypeId,
    narrowed_type: TypeId,
}

type NarrowingState = HashMap<NarrowingPlace, Vec<NarrowingFact>>;
type TrackedBindingState = HashMap<SymbolId, TrackedLifetimeLink>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NarrowingLockKind {
    Acquire,
    Release,
}

/// A lowering-visible counter update on one control-flow edge. Acquiring a
/// fact increments the addressed union storage's tag-lock counter; releasing
/// it decrements the same counter. Lowering emits these operations on the edge,
/// not merely at lexical block boundaries, so guard-clause facts can outlive
/// the `if` which established them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NarrowingLockOperation {
    place: NarrowingPlace,
    narrowed_type: TypeId,
    kind: NarrowingLockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NarrowingEdgeKind {
    True,
    False,
    Join,
    Invalidate,
    Return,
    ErrorPropagation,
    Break,
    Continue,
    CallableCompletion,
    LoopBackedge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NarrowingEdge {
    source: NodeId,
    kind: NarrowingEdgeKind,
    /// Retained for invariant tests and for lowering to verify edge placement.
    from: NarrowingState,
    to: NarrowingState,
    operations: Vec<NarrowingLockOperation>,
}

/// Runtime behavior required when writing storage which may currently have
/// outstanding narrowed references. Payload-only mutation never changes the
/// tag; same-tag replacement keeps the lock; a guarded tag change releases the
/// writer's own fact and asks lowering to panic if any alias still holds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnionMutationKind {
    PayloadMutation,
    SameTagReplacement,
    GuardedTagChange,
}

#[derive(Debug, Clone)]
struct BooleanFlow {
    when_true: Option<NarrowingState>,
    when_false: Option<NarrowingState>,
    invalid: bool,
}

impl InterfaceView {
    fn source_members(&self, types: &TypeStore) -> Vec<TypeId> {
        match types.get(self.source_type) {
            Some(SemanticType::Union { members, .. }) => members.clone(),
            _ => vec![self.source_type],
        }
    }

    /// Derives the storage operation for one possible source alternative.
    /// This is lowering information, not an independently recorded type fact.
    fn backing_transfer_for(&self, types: &TypeStore, member: TypeId) -> ValueTransfer {
        if types
            .get(self.destination_type)
            .is_some_and(|semantic| semantic.storage_semantics() == Some(StorageSemantics::Gc))
        {
            ValueTransfer::CopyGcReference
        } else if types
            .get(member)
            .is_some_and(|semantic| semantic.storage_semantics() == Some(StorageSemantics::Gc))
        {
            ValueTransfer::Borrow
        } else if self.source_category == ValueCategory::FreshTemporary {
            ValueTransfer::MoveTemporary
        } else {
            ValueTransfer::Borrow
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContextualAssignment {
    /// The source representation already has the required shape.
    Exact,
    /// Existing plain or GC-backed storage is exposed as a tracked reference.
    TrackedBorrow {
        source_type: TypeId,
        target_type: TypeId,
    },
    /// Existing concrete storage is exposed through an interface view.
    InterfaceView(InterfaceView),
    /// One value is wrapped with the selected destination-union tag.
    UnionInjection {
        member_type: TypeId,
        interface_view: Option<InterfaceView>,
        tracked_borrow: Option<(TypeId, TypeId)>,
        error_widening: Option<ErrorWidening>,
    },
    /// An existing union needs active-tag remapping into a strict superset.
    UnionWidening(UnionWidening),
    /// `Error` alone is covariant in its immutable payload.
    ErrorWidening(ErrorWidening),
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
    /// Fixed declared shape before any flow-sensitive union narrowing.
    declared_type_id: TypeId,
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
    /// A zero-based tuple element selected by a numeric field designator.
    TupleElement { index: usize },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorConstructorInference {
    Explicit,
    Expected,
    Payload,
}

/// Stable identities for compiler-known construction and payload access.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedBuiltinOperation {
    Constructor {
        builtin: BuiltinType,
        type_arguments: Vec<TypeId>,
        error_inference: Option<ErrorConstructorInference>,
    },
    ErrorValue {
        error_type: TypeId,
        payload_type: TypeId,
    },
    AsciiEncode,
    AsciiDecode {
        result_type: TypeId,
        string_member: usize,
        error_member: usize,
    },
    Output {
        mode: OutputMode,
    },
    Panic,
    Yield,
}

/// One checked postfix `?`. The operand node is retained explicitly so typed
/// IR evaluates it once and branches on that stored union rather than
/// reconstructing either source expression or type lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedErrorPropagation {
    operand: NodeId,
    operand_type: TypeId,
    success_type: TypeId,
    success_category: ValueCategory,
    success_members: Vec<(TypeId, usize)>,
    propagated_error: TypeId,
    return_error: TypeId,
    error_member: usize,
    callable_result: TypeId,
    return_assignment: ContextualAssignment,
    success_transfer: ValueTransfer,
    return_transfer: ValueTransfer,
}

/// The callable selected by a checked coroutine start or deferred call. Static
/// targets retain their semantic identity, while a first-class callable retains
/// the evaluated callee node and concrete callable type which must be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedCoroutineCallTarget {
    Function { declaration: NodeId, symbol: SymbolId },
    CallableValue { callee: NodeId, callable_type: TypeId },
    Member(ResolvedMember),
    Builtin(ResolvedBuiltinOperation),
    Queue(ResolvedQueueOperation),
    Sequence(ResolvedSequenceOperation),
    RuntimeSpecialization(RuntimeCallableSpecializationId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoroutinePreparedRole {
    Callable,
    Receiver,
    Argument { index: usize },
}

/// One runtime value evaluated and retained while preparing a coroutine start
/// or deferred call. The ordered list is the source evaluation order.
/// Physical places and tracked sources let post-type escape analysis decide
/// whether a value may safely outlive the starting call site.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CoroutinePreparedValue {
    role: CoroutinePreparedRole,
    expression: NodeId,
    type_id: TypeId,
    category: ValueCategory,
    transfer: Option<ValueTransfer>,
    place: Option<PhysicalPlace>,
    tracked_sources: Vec<PhysicalPlace>,
}

/// Complete type-checking result for one `co call(...)` statement. The
/// eventual result is retained only as type information and is always
/// discarded; the statement itself has unit type.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCoroutineStart {
    call: NodeId,
    target: ResolvedCoroutineCallTarget,
    prepared: Vec<CoroutinePreparedValue>,
    discarded_result: TypeId,
    statement_type: TypeId,
}

/// Complete type-checking result for one lexical `defer call(...)`
/// registration. Preparation uses the same source-ordered value description
/// as coroutine starts because both constructs evaluate the callable or
/// receiver and every runtime argument at the statement, then invoke later.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedDeferredCall {
    call: NodeId,
    target: ResolvedCoroutineCallTarget,
    prepared: Vec<CoroutinePreparedValue>,
    discarded_result: TypeId,
    statement_type: TypeId,
    block: NodeId,
    registration_order: usize,
    reachable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredCleanupEdgeKind {
    Normal,
    Return,
    Break(NodeId),
    Continue(NodeId),
    ErrorPropagation,
}

/// One control-flow edge which leaves lexical scopes containing live defer
/// registrations. Blocks and registrations are ordered innermost-first, with
/// each block's registrations in reverse source order. `transfer_value`, when
/// present, is evaluated and saved before any cleanup call runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeferredCleanupEdge {
    source: NodeId,
    kind: DeferredCleanupEdgeKind,
    exited_blocks: Vec<NodeId>,
    registrations: Vec<NodeId>,
    transfer_value: Option<NodeId>,
}

#[derive(Debug, Clone)]
struct ActiveDeferredBlock {
    block: NodeId,
    registrations: Vec<NodeId>,
    next_registration_order: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Print,
    PrintLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueOperationKind {
    Send,
    TryReceive,
}

/// Concrete queue behavior selected by expression checking. Queue methods are
/// not ordinary source methods, so lowering needs their instantiated receiver,
/// element, transfer, and result representation without repeating catalogue
/// lookup or reconstructing canonical union member positions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedQueueOperation {
    kind: QueueOperationKind,
    queue_type: TypeId,
    element_type: TypeId,
    receiver_transfer: Option<ValueTransfer>,
    element_transfer: Option<ValueTransfer>,
    receive_union: Option<QueueReceiveUnion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueReceiveUnion {
    type_id: TypeId,
    element_member: usize,
    none_member: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceKind {
    String,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorrowStorageViolation {
    Gc(TypeId),
    ExternalBuffer(TypeId),
}

/// Identifies a compiler-provided sequence operation after type checking has
/// selected its concrete string/byte meaning. Typed IR and lowering consume
/// this fact instead of repeating member or operand-shape resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedSequenceOperation {
    Index { sequence: SequenceKind },
    Slice { sequence: SequenceKind },
    Length { sequence: SequenceKind },
    BytesConcat,
}

/// A dynamic precondition lowering must enforce for a resolved sequence
/// operation. Byte writes are intentionally checked at runtime because their
/// source-level type is `int`; constant evaluation is a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceRuntimeCheck {
    IndexBounds,
    SliceBounds,
    ByteValueRange,
}

/// Identifies the deliberately small set of primitive conversions expressed
/// by type ascription. The inner expression keeps its source type; this fact
/// tells typed IR that the surrounding ascription constructs a fresh value of
/// the destination primitive type rather than merely checking compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimitiveConversion {
    FloatToInt,
    IntToFloat,
    IntToChar,
    CharToInt,
}

/// A dynamic precondition required by a primitive conversion. Integer-to-float
/// needs no check because every integer has a defined rounded binary64 result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimitiveConversionRuntimeCheck {
    FiniteSignedIntRange,
    AsciiRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatAlignment {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatSign {
    Plus,
    Minus,
    Space,
}

/// The normalized, deliberately small Python-compatible format specification
/// consumed by lowering. Width and precision are literals, so formatting does
/// not introduce hidden expression evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FormatSpecification {
    fill: Option<u8>,
    alignment: Option<FormatAlignment>,
    sign: Option<FormatSign>,
    zero_padding: bool,
    width: Option<u32>,
    fixed_precision: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedInterpolation {
    value: NodeId,
    value_type: TypeId,
    format: FormatSpecification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LambdaCaptureSource {
    Symbol(SymbolId),
    SelfValue { method: NodeId },
}

/// The type-facing portion of a lambda capture.
///
/// This deliberately records only the source and its two capabilities.
/// Expression checking conservatively rejects borrow-containing sources; later
/// lowering still decides environment layout, recursive copies, shared mutable
/// cells, and tracing for the remaining captures.
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
    TypeValueOutsideFactory,
    IntegerLiteralOutOfRange,
    TypeMismatch {
        expected: TypeId,
        found: TypeId,
    },
    /// One source alternative satisfies multiple destination-union members.
    /// A source ascription such as `value: Reader` selects the intended member.
    AmbiguousUnionConversion {
        source: TypeId,
        destination: TypeId,
    },
    InvalidTypeTestSource {
        found: TypeId,
    },
    InvalidTypeTestMember {
        union: TypeId,
        tested: TypeId,
    },
    InvalidUnaryOperand {
        operator: UnaryOperator,
        found: TypeId,
    },
    InvalidBinaryOperand {
        operator: BinaryOperator,
        found: TypeId,
    },
    InvalidGcAllocationSource {
        found: TypeId,
        category: ValueCategory,
    },
    InvalidReturnSource {
        found: TypeId,
        category: ValueCategory,
    },
    TemporaryTrackedBorrowEscapes,
    InvalidTrackedReturnSource,
    BorrowContainingLambdaCapture,
    BorrowContainingGcStorage {
        found: TypeId,
    },
    BorrowContainingExternalBuffer {
        found: TypeId,
    },
    TrackedBorrowInvalidated,
    NotCallable {
        found: TypeId,
    },
    ArgumentCountMismatch {
        expected: usize,
        found: usize,
    },
    LoopElseRequired,
    ConditionalElseRequired,
    ConditionalBranchValueRequired,
    InvalidAssignmentTarget,
    ImmutableBinding,
    ImmutableValue,
    InvalidAssignmentOperand {
        operator: AssignmentOperator,
        found: TypeId,
    },
    InvalidSequenceOwner {
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
    CannotInferErrorPayload,
    InvalidTryOperand {
        found: TypeId,
    },
    TryMissingErrorMember {
        operand: TypeId,
    },
    TryAmbiguousErrorMembers {
        operand: TypeId,
    },
    TryRequiresSuccessMember {
        operand: TypeId,
    },
    PropagatedErrorNotAccepted {
        error: TypeId,
        callable_result: TypeId,
    },
    NamespaceRequiresMember,
    UnknownMember,
    InvalidMemberOwner {
        found: TypeId,
    },
    InvalidTupleElementOwner {
        found: TypeId,
    },
    TupleElementOutOfRange {
        index: usize,
        arity: usize,
    },
    FieldRequiresValue,
    AssociatedFunctionRequiresType,
    MethodRequiresValue,
    MethodRequiresCall,
    TemplateRequiresSpecialization,
    InvalidRuntimeTemplateArgument {
        found: TypeId,
    },
    ExpandingRuntimeTemplateSpecialization,
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
    InterfaceRequiresGcSource,
    InfiniteInlineLayout {
        owner: TypeId,
    },
    UnsupportedFormattedValue {
        found: TypeId,
    },
    DivergentFormattedValue,
    InvalidFormatSpecification,
}

#[derive(Debug, Clone, Default)]
struct ExpressionChecking {
    expressions: HashMap<NodeId, TypedExpression>,
    explicit_values: HashMap<NodeId, bool>,
    bindings: HashMap<SymbolId, BindingSemantics>,
    transfers: HashMap<NodeId, ValueTransfer>,
    union_injections: HashMap<NodeId, UnionInjection>,
    union_widenings: HashMap<NodeId, UnionWidening>,
    error_widenings: HashMap<NodeId, ErrorWidening>,
    interface_views: HashMap<NodeId, InterfaceView>,
    tracked_borrows: HashMap<NodeId, TrackedBorrow>,
    tracked_parameter_borrows: HashMap<NodeId, TrackedParameterBorrow>,
    tracked_lifetime_links: HashMap<NodeId, TrackedLifetimeLink>,
    /// Origins retained by a local reference slot or inline aggregate value.
    /// Reading the binding restores these rather than treating the slot itself
    /// as ownership of the referenced storage.
    tracked_binding_lifetimes: HashMap<SymbolId, TrackedLifetimeLink>,
    /// GC-backed owners which must remain traced while the tracked value held
    /// by a binding is live. The key is the binding or assignment node which
    /// established that lifetime.
    gc_owner_roots: HashMap<NodeId, Vec<PhysicalPlace>>,
    /// Writes rejected because they replace an ancestor of a live interior
    /// tracked reference. Retaining the conflicting sources lets later IR
    /// diagnostics and lowering avoid reconstructing physical-place overlap.
    borrow_invalidations: HashMap<NodeId, Vec<PhysicalPlace>>,
    displaced_roots: HashMap<NodeId, PhysicalPlaceRoot>,
    tracked_call_inputs: HashMap<NodeId, Vec<PhysicalPlace>>,
    borrow_containing_call_inputs: HashMap<NodeId, Vec<PhysicalPlace>>,
    /// Stable storage routes for place expressions. Keeping this separate from
    /// `Place` avoids conflating assignment rules with borrow provenance.
    physical_places: HashMap<NodeId, PhysicalPlace>,
    /// Runtime tag-counter updates, retained in control-flow order for typed IR.
    narrowing_edges: Vec<NarrowingEdge>,
    /// The narrowed type produced by each valid `is` expression on each edge.
    type_test_facts: HashMap<NodeId, (Option<TypeId>, Option<TypeId>)>,
    union_mutations: HashMap<NodeId, UnionMutationKind>,
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
    resolved_builtin_operations: HashMap<NodeId, ResolvedBuiltinOperation>,
    resolved_error_propagations: HashMap<NodeId, ResolvedErrorPropagation>,
    resolved_coroutine_starts: HashMap<NodeId, ResolvedCoroutineStart>,
    resolved_deferred_calls: HashMap<NodeId, ResolvedDeferredCall>,
    deferred_cleanup_edges: Vec<DeferredCleanupEdge>,
    resolved_queue_operations: HashMap<NodeId, ResolvedQueueOperation>,
    resolved_sequence_operations: HashMap<NodeId, ResolvedSequenceOperation>,
    sequence_runtime_checks: HashMap<NodeId, Vec<SequenceRuntimeCheck>>,
    primitive_conversions: HashMap<NodeId, PrimitiveConversion>,
    primitive_conversion_runtime_checks:
        HashMap<NodeId, PrimitiveConversionRuntimeCheck>,
    /// Source-ordered interpolation operations. Literal text remains in the
    /// AST; these facts identify each single-evaluation value and its parsed
    /// formatting behavior for typed IR and lowering.
    formatted_strings: HashMap<NodeId, Vec<ResolvedInterpolation>>,
    generated_methods: HashMap<(TypeId, NodeId), GeneratedMethodChecking>,
    runtime_specialization_calls: HashMap<NodeId, RuntimeCallableSpecializationId>,
    runtime_specializations: Vec<RuntimeCallableSpecialization>,
    /// Bindings written by assignment. For object-like locals, lowering uses
    /// this to decide when the source-level reference needs indirection in
    /// addition to any frame-owned backing storage.
    reassigned_bindings: HashSet<SymbolId>,
    errors: Vec<ExpressionCheckingError>,
}

/// A specialization-scoped snapshot for a generated method. Generated type
/// applications reuse one source AST, so `NodeId` alone cannot distinguish
/// `Box(int).get` from `Box(string).get`. Keeping the owner type in the key
/// prevents one checked application from overwriting another before typed IR
/// gives specializations their final identities. Retaining the complete
/// checking result is important: post-type escape analysis and lowering need
/// the same tracked-origin, lifetime, GC-root, and validity facts here as they
/// do for an ordinary callable or runtime specialization.
#[derive(Debug, Clone)]
struct GeneratedMethodChecking {
    checking: Box<ExpressionChecking>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RuntimeCallableSpecializationId(usize);

/// One canonical runtime-template instantiation. Method identities include
/// their concrete owner in addition to the declaration and ordered type
/// arguments; top-level functions have no owner.
#[derive(Debug, Clone)]
struct RuntimeCallableSpecialization {
    owner: Option<TypeId>,
    declaration: NodeId,
    type_arguments: Vec<TypeId>,
    signature: CallableSignature,
    /// Full ordinary-analysis result for this reuse of the source AST. A box
    /// breaks the recursive metadata shape because checking can itself refer
    /// to further callable specializations.
    checking: Box<ExpressionChecking>,
}

#[derive(Debug, Default)]
struct LexicalIndex {
    callable_parents: HashMap<NodeId, Option<NodeId>>,
    symbol_owners: HashMap<SymbolId, NodeId>,
    receiver_qualifiers: HashMap<NodeId, BindingQualifiers>,
    symbol_references: HashMap<SymbolId, Vec<NodeId>>,
    loop_symbol_references: HashMap<NodeId, HashSet<SymbolId>>,
    active_loops: Vec<NodeId>,
    expression_branches: HashMap<NodeId, Vec<(NodeId, u8)>>,
    active_branches: Vec<(NodeId, u8)>,
    expression_spans: HashMap<NodeId, Span>,
    mutation_ends: HashMap<NodeId, usize>,
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
                Declaration::Interface(_) | Declaration::TypeAlias(_) => {}
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
        let enclosing_loops = std::mem::take(&mut self.active_loops);
        let enclosing_branches = std::mem::take(&mut self.active_branches);
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
        self.active_loops = enclosing_loops;
        self.active_branches = enclosing_branches;
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
        self.expression_branches
            .insert(expression.id, self.active_branches.clone());
        self.expression_spans.insert(expression.id, expression.span);
        match &expression.kind {
            ExpressionKind::Identifier => {
                if let Some(symbol) = names.symbol_for_reference(expression.id) {
                    self.symbol_references
                        .entry(symbol)
                        .or_default()
                        .push(expression.id);
                    for loop_id in &self.active_loops {
                        self.loop_symbol_references
                            .entry(*loop_id)
                            .or_default()
                            .insert(symbol);
                    }
                }
            }
            ExpressionKind::SelfValue
            | ExpressionKind::Literal(_)
            | ExpressionKind::AssociatedAccess { .. } => {}
            ExpressionKind::TypeValue(type_syntax) => self.visit_type(type_syntax, names),
            ExpressionKind::FormattedString { parts } => {
                for part in parts {
                    if let FormattedStringPart::Interpolation { value, .. } = part {
                        self.visit_expression(value, callable, names);
                    }
                }
            }
            ExpressionKind::Group(inner)
            | ExpressionKind::GcAllocate(inner)
            | ExpressionKind::MemberAccess { object: inner, .. }
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::TypeTest { value: inner, .. }
            | ExpressionKind::TypeAscription { value: inner, .. }
            | ExpressionKind::Unary { operand: inner, .. } => {
                self.visit_expression(inner, callable, names);
            }
            ExpressionKind::Tuple { elements } => {
                for element in elements {
                    self.visit_expression(element, callable, names);
                }
            }
            ExpressionKind::Block(block) => {
                self.visit_block(block, callable, names);
            }
            ExpressionKind::Loop { body } => {
                self.active_loops.push(expression.id);
                self.visit_block(body, callable, names);
                self.active_loops.pop();
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expression(condition, callable, names);
                self.active_branches.push((expression.id, 0));
                self.visit_block(then_branch, callable, names);
                self.active_branches.pop();
                if let Some(else_branch) = else_branch {
                    self.active_branches.push((expression.id, 1));
                    match else_branch {
                        ConditionalElse::Block(block) => self.visit_block(block, callable, names),
                        ConditionalElse::If(expression) => {
                            self.visit_expression(expression, callable, names);
                        }
                    }
                    self.active_branches.pop();
                }
            }
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.active_loops.push(expression.id);
                self.visit_expression(condition, callable, names);
                self.visit_block(body, callable, names);
                if let Some(block) = else_branch {
                    self.visit_block(block, callable, names);
                }
                self.active_loops.pop();
            }
            ExpressionKind::RangeFor {
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                self.active_loops.push(expression.id);
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
                self.active_loops.pop();
            }
            ExpressionKind::Lambda {
                parameters, body, ..
            } => {
                self.callable_parents.insert(expression.id, Some(callable));
                self.record_parameters(expression.id, parameters, names);
                self.visit_block(body, expression.id, names);
            }
            ExpressionKind::StructConstruction { owner, fields } => {
                self.visit_type(owner, names);
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
            } => {
                self.visit_expression(object, callable, names);
                self.visit_expression(index, callable, names);
            }
            ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => {
                self.mutation_ends.insert(target.id, expression.span.end);
                self.expression_branches
                    .entry(target.id)
                    .or_insert_with(|| self.active_branches.clone());
                self.expression_spans.entry(target.id).or_insert(target.span);
                if *operator != AssignmentOperator::Assign
                    || !matches!(&target.kind, ExpressionKind::Identifier)
                {
                    self.visit_expression(target, callable, names);
                }
                self.visit_expression(value, callable, names);
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

    fn visit_type(&mut self, type_syntax: &TypeSyntax, names: &NameResolution) {
        match &type_syntax.kind {
            crate::ast::TypeKind::GeneratedStruct { members } => {
                for member in members {
                    match member {
                        StructMember::Field(_) => {}
                        StructMember::Function(function) => {
                            self.visit_function(function, None, names);
                        }
                    }
                }
            }
            crate::ast::TypeKind::Builtin { arguments, .. }
            | crate::ast::TypeKind::Named { arguments, .. } => {
                for argument in arguments {
                    self.visit_type(argument, names);
                }
            }
            crate::ast::TypeKind::Associated {
                owner, arguments, ..
            } => {
                self.visit_type(owner, names);
                for argument in arguments {
                    self.visit_type(argument, names);
                }
            }
            crate::ast::TypeKind::Mutable(inner)
            | crate::ast::TypeKind::Gc(inner)
            | crate::ast::TypeKind::Tracked(inner)
            | crate::ast::TypeKind::Group(inner) => self.visit_type(inner, names),
            crate::ast::TypeKind::Tuple { elements } => {
                for element in elements {
                    self.visit_type(element, names);
                }
            }
            crate::ast::TypeKind::Callable {
                parameters,
                return_type,
            } => {
                for parameter in parameters {
                    self.visit_type(parameter, names);
                }
                self.visit_type(return_type, names);
            }
            crate::ast::TypeKind::Intersection { members }
            | crate::ast::TypeKind::Union { members } => {
                for member in members {
                    self.visit_type(member, names);
                }
            }
            crate::ast::TypeKind::ComptimeType | crate::ast::TypeKind::Primitive(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LayoutField {
    declaration: NodeId,
    span: Span,
    type_id: TypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AggregateOwner {
    Source(NodeId),
    Generated(TypeId),
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
    callable_parents: HashMap<NodeId, Option<NodeId>>,
    symbol_owners: HashMap<SymbolId, NodeId>,
    receiver_qualifiers: HashMap<NodeId, BindingQualifiers>,
    symbol_references: HashMap<SymbolId, Vec<NodeId>>,
    loop_symbol_references: HashMap<NodeId, HashSet<SymbolId>>,
    expression_branches: HashMap<NodeId, Vec<(NodeId, u8)>>,
    expression_spans: HashMap<NodeId, Span>,
    mutation_ends: HashMap<NodeId, usize>,
    /// Aggregate declarations and expressions in source discovery order. The
    /// order is retained so recursive-layout diagnostics are deterministic.
    aggregate_order: Vec<TypeId>,
    aggregate_layouts: HashMap<TypeId, AggregateLayout>,
    /// Concrete generated owners first reached by substituting a runtime
    /// template, rather than by explicit source type syntax.
    runtime_generated_structs: HashMap<TypeId, StructSignature>,
    runtime_generated_callables: HashMap<(TypeId, NodeId), CallableSignature>,
    /// Flow-sensitive provenance of the storage currently denoted by each
    /// binding. Declared type and qualifiers remain in `checking.bindings`.
    current_binding_categories: HashMap<SymbolId, ValueCategory>,
    /// Flow-sensitive origins held by tracked-reference slots and inline
    /// values which transitively contain tracked references.
    current_tracked_bindings: TrackedBindingState,
    /// Flow facts currently guaranteed on this path. Each stack entry owns one
    /// runtime tag lock and therefore must be released exactly once.
    current_narrowings: NarrowingState,
    /// Whether the expression currently being analyzed is reachable from the
    /// beginning of its enclosing block. Unreachable syntax is still checked,
    /// but its loop transfers cannot contribute runtime exits.
    current_path_reachable: bool,
    /// Declared result of the callable whose body is currently being checked.
    /// Nested named functions and lambdas save and restore this context.
    current_callable_result: Option<TypeId>,
    /// Roots permitted to contribute to an escaping tracked result from the
    /// callable currently being checked.
    current_tracked_return_roots: HashSet<PhysicalPlaceRoot>,
    /// Plain parameters and receivers whose inline values themselves contain
    /// tracked references may contribute only to an aggregate tracked return.
    current_borrow_containing_return_roots: HashSet<PhysicalPlaceRoot>,
    current_specialized_owner: Option<TypeId>,
    /// Concrete `T` identities while rechecking one runtime-template body. The
    /// source AST and collected symbolic signature stay immutable.
    current_template_substitutions: Option<HashMap<NodeId, TypeId>>,
    /// Per-syntax-node view of the same substitution, used by annotations,
    /// constructions, ascriptions, and every other ordinary type boundary.
    current_template_syntax_types: Option<HashMap<NodeId, TypeId>>,
    runtime_templates: HashMap<NodeId, Function>,
    /// Canonical owner-plus-declaration-plus-argument identities. An entry is
    /// installed before body analysis so exact recursive calls reuse it.
    specialization_cache:
        HashMap<(Option<TypeId>, NodeId, Vec<TypeId>), RuntimeCallableSpecializationId>,
    /// In-progress keys used to distinguish exact recursion from a request
    /// which keeps expanding the same declaration's type arguments.
    active_specializations: Vec<(Option<TypeId>, NodeId, Vec<TypeId>)>,
    runtime_specialization_calls: HashMap<NodeId, RuntimeCallableSpecializationId>,
    runtime_specializations: Vec<RuntimeCallableSpecialization>,
    /// Loops whose bodies are currently being checked. Resolved transfer
    /// targets select an entry here, so nested-loop breaks never leak outward.
    active_loops: Vec<ActiveLoop>,
    /// Executable lexical blocks in the current callable. Nested callables
    /// isolate this stack just as they isolate loop transfer contexts.
    active_deferred_blocks: Vec<ActiveDeferredBlock>,
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
            symbol_references,
            loop_symbol_references,
            active_loops: _,
            expression_branches,
            active_branches: _,
            expression_spans,
            mutation_ends,
        } = LexicalIndex::build(program, names);
        let mut method_owners = HashMap::new();
        let mut aggregate_order = Vec::new();
        let mut aggregate_layouts = HashMap::new();
        for declaration in &program.declarations {
            let Declaration::Struct(structure) = declaration else {
                continue;
            };
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
            aggregate_order.push(owner);
            aggregate_layouts.insert(
                owner,
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
        let mut generated: Vec<_> = signatures.generated_structs().values().collect();
        generated.sort_by_key(|signature| signature.type_id.as_usize());
        for signature in generated {
            let fields = signature
                .field_order()
                .iter()
                .filter_map(|name| {
                    let member = signature.member(name)?;
                    let StructMemberSignatureKind::Field(field) = member.kind else {
                        return None;
                    };
                    Some(LayoutField {
                        declaration: field.declaration,
                        span: member.span,
                        type_id: field.type_id?,
                    })
                })
                .collect();
            aggregate_order.push(signature.type_id);
            aggregate_layouts.insert(
                signature.type_id,
                AggregateLayout {
                    type_id: signature.type_id,
                    fields,
                },
            );
        }

        Self {
            module,
            names,
            context,
            signatures,
            types,
            method_owners,
            callable_parents,
            symbol_owners,
            receiver_qualifiers,
            symbol_references,
            loop_symbol_references,
            expression_branches,
            expression_spans,
            mutation_ends,
            aggregate_order,
            aggregate_layouts,
            runtime_generated_structs: HashMap::new(),
            runtime_generated_callables: HashMap::new(),
            current_binding_categories: HashMap::new(),
            current_tracked_bindings: HashMap::new(),
            current_narrowings: HashMap::new(),
            current_path_reachable: true,
            current_callable_result: None,
            current_tracked_return_roots: HashSet::new(),
            current_borrow_containing_return_roots: HashSet::new(),
            current_specialized_owner: None,
            current_template_substitutions: None,
            current_template_syntax_types: None,
            runtime_templates: index_runtime_templates(program, signatures),
            specialization_cache: HashMap::new(),
            active_specializations: Vec::new(),
            runtime_specialization_calls: HashMap::new(),
            runtime_specializations: Vec::new(),
            active_loops: Vec::new(),
            active_deferred_blocks: Vec::new(),
            checking: ExpressionChecking::default(),
        }
    }

    fn check_program(mut self, program: &Program) -> ExpressionChecking {
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }
        self.visit_generated_struct_methods(program);
        self.validate_finite_inline_layouts();
        self.validate_borrow_storage_fields();
        self.checking.runtime_specialization_calls = self.runtime_specialization_calls;
        self.checking.runtime_specializations = self.runtime_specializations;
        Self::sort_checking_diagnostics(&mut self.checking);
        self.checking
    }

    /// Finishes one checking result in deterministic source order. Generated
    /// methods and runtime specializations are analyzed on demand in isolated
    /// result sets, so their diagnostics can otherwise be appended after later
    /// source declarations merely because their concrete types were reached
    /// later. Stable sorting preserves emission order for diagnostics attached
    /// to the same source range while making every result independently ready
    /// for reporting.
    fn sort_checking_diagnostics(checking: &mut ExpressionChecking) {
        checking.errors.sort_by_key(|error| {
            (
                error.span.module_id.as_u32(),
                error.span.start,
                error.span.end,
            )
        });
        for method in checking.generated_methods.values_mut() {
            Self::sort_checking_diagnostics(&mut method.checking);
        }
        for specialization in &mut checking.runtime_specializations {
            Self::sort_checking_diagnostics(&mut specialization.checking);
        }
    }

    fn visit_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function)
                if function
                    .return_type
                    .as_ref()
                    .is_some_and(|return_type| matches!(&return_type.kind, crate::ast::TypeKind::ComptimeType)) => {}
            Declaration::Function(function) => self.visit_function(function),
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    if let StructMember::Function(function) = member
                        && !function
                            .return_type
                            .as_ref()
                            .is_some_and(|return_type| matches!(&return_type.kind, crate::ast::TypeKind::ComptimeType))
                    {
                        self.visit_function(function);
                    }
                }
            }
            Declaration::Interface(_) | Declaration::TypeAlias(_) => {}
        }
    }

    fn visit_generated_struct_methods(&mut self, program: &Program) {
        let mut instances: Vec<_> = self
            .types
            .generated_structs()
            .values()
            .map(|instance| (instance.type_id, instance.template))
            .collect();
        instances.sort_by_key(|(type_id, _)| type_id.as_usize());
        for (owner, template) in instances {
            let Some(crate::ast::TypeKind::GeneratedStruct { members }) =
                find_type_syntax(program, template).map(|syntax| &syntax.kind)
            else {
                continue;
            };
            let functions: Vec<_> = members
                .iter()
                .filter_map(|member| {
                    let StructMember::Function(function) = member else {
                        return None;
                    };
                    self.signatures
                        .specialized_callable(owner, function.id)
                        .map(|_| function.clone())
                })
                .collect();
            let previous_owner = self.current_specialized_owner.replace(owner);
            for function in functions {
                if self
                    .signatures
                    .specialized_callable(owner, function.id)
                    .is_some_and(|signature| signature.receiver.is_some())
                {
                    self.method_owners.insert(function.id, owner);
                }
                // Every generated owner reuses the same source NodeIds. Check
                // it in an isolated result set so `Box(T).get` cannot leave a
                // cached `self.inner: T` which is then reused while checking
                // `Box(Item).get`.
                let parent_checking = std::mem::take(&mut self.checking);
                let parent_specialization_calls =
                    std::mem::take(&mut self.runtime_specialization_calls);
                self.visit_function(&function);
                let mut local_checking = std::mem::take(&mut self.checking);
                Self::sort_checking_diagnostics(&mut local_checking);
                let method_specialization_calls =
                    std::mem::take(&mut self.runtime_specialization_calls);
                self.checking = parent_checking;
                for error in &local_checking.errors {
                    if !self.checking.errors.contains(error) {
                        self.checking.errors.push(error.clone());
                    }
                }
                self.runtime_specialization_calls = parent_specialization_calls;
                let mut local_checking = local_checking;
                local_checking.runtime_specialization_calls = method_specialization_calls;
                let checking = GeneratedMethodChecking {
                    checking: Box::new(local_checking),
                };
                self.checking
                    .generated_methods
                    .insert((owner, function.id), checking);
            }
            self.current_specialized_owner = previous_owner;
        }
    }

    fn visit_function(&mut self, function: &Function) {
        let enclosing_categories = self.current_binding_categories.clone();
        let enclosing_tracked_bindings =
            std::mem::take(&mut self.current_tracked_bindings);
        let enclosing_narrowings = std::mem::take(&mut self.current_narrowings);
        let enclosing_reachability = self.current_path_reachable;
        let enclosing_loops = std::mem::take(&mut self.active_loops);
        let enclosing_deferred_blocks = std::mem::take(&mut self.active_deferred_blocks);
        let enclosing_tracked_roots = std::mem::take(&mut self.current_tracked_return_roots);
        let enclosing_borrow_containing_roots =
            std::mem::take(&mut self.current_borrow_containing_return_roots);
        self.current_path_reachable = true;
        self.seed_callable_parameters(function.id, &function.parameters);
        let signature = self.resolved_callable_signature(function.id);
        let mut semantic_parameters = signature.parameters.iter().copied();
        for parameter in &function.parameters {
            match &parameter.kind {
                FunctionParameterKind::Named { .. } => {
                    let type_id = semantic_parameters
                        .next()
                        .expect("collected signature must contain every named parameter");
                    self.validate_borrow_storage_type(type_id, parameter.span);
                    if self.tracked_reference_parts(type_id).is_some() {
                        let symbol = self
                            .names
                            .symbol_for_declaration(parameter.id)
                            .expect("named parameter must have a semantic symbol");
                        self.current_tracked_return_roots
                            .insert(PhysicalPlaceRoot::Symbol(symbol));
                    } else if self.type_contains_tracked_reference(type_id) {
                        let symbol = self
                            .names
                            .symbol_for_declaration(parameter.id)
                            .expect("named parameter must have a semantic symbol");
                        self.current_borrow_containing_return_roots
                            .insert(PhysicalPlaceRoot::Symbol(symbol));
                    }
                }
                FunctionParameterKind::Receiver { storage, .. }
                    if *storage == ReceiverStorage::Tracked =>
                {
                    self.current_tracked_return_roots
                        .insert(PhysicalPlaceRoot::SelfValue(function.id));
                }
                FunctionParameterKind::Receiver { storage, .. } => {
                    if *storage == ReceiverStorage::Gc
                        && self
                            .method_owners
                            .get(&function.id)
                            .copied()
                            .is_some_and(|owner| self.type_contains_tracked_reference(owner))
                    {
                        let owner = self.method_owners[&function.id];
                        self.checking.errors.push(ExpressionCheckingError {
                            kind: ExpressionCheckingErrorKind::BorrowContainingGcStorage {
                                found: owner,
                            },
                            span: parameter.span,
                        });
                    }
                }
                FunctionParameterKind::Comptime { .. } => {}
            }
        }
        if let Some(return_syntax) = &function.return_type {
            self.validate_borrow_storage_type(signature.return_type, return_syntax.span);
        }
        if signature.receiver.is_some()
            && self
                .method_owners
                .get(&function.id)
                .copied()
                .is_some_and(|owner| self.type_contains_tracked_reference(owner))
        {
            self.current_borrow_containing_return_roots
                .insert(PhysicalPlaceRoot::SelfValue(function.id));
        }
        let return_type = signature.return_type;
        self.visit_callable_body(&function.body, return_type);
        self.release_all_narrowings(function.body.id, NarrowingEdgeKind::CallableCompletion);
        self.current_binding_categories = enclosing_categories;
        self.current_tracked_bindings = enclosing_tracked_bindings;
        self.current_narrowings = enclosing_narrowings;
        self.current_path_reachable = enclosing_reachability;
        self.active_loops = enclosing_loops;
        self.active_deferred_blocks = enclosing_deferred_blocks;
        self.current_tracked_return_roots = enclosing_tracked_roots;
        self.current_borrow_containing_return_roots = enclosing_borrow_containing_roots;
    }

    /// Makes a named function's or lambda's parameters available while checking
    /// its body.
    ///
    /// Each parameter's collected semantic type, source qualifiers, and value
    /// category are recorded against its resolved symbol. Receivers are not
    /// included because `self` is typed separately from receiver metadata.
    fn seed_callable_parameters(&mut self, callable: NodeId, parameters: &[FunctionParameter]) {
        let signature = self.resolved_callable_signature(callable);
        let mut semantic_parameters = signature.parameters.into_iter();

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
            if self.type_contains_tracked_reference(type_id) {
                let link = TrackedLifetimeLink {
                    sources: vec![PhysicalPlace {
                        root: PhysicalPlaceRoot::Symbol(symbol),
                        projections: Vec::new(),
                        storage: category,
                    }],
                };
                self.current_tracked_bindings.insert(symbol, link.clone());
                self.checking.tracked_binding_lifetimes.insert(symbol, link);
            }
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
        let enclosing_callable_result = self.current_callable_result.replace(expected);
        self.enter_deferred_block(block.id);
        // A return or diverging statement prevents execution from reaching the
        // body's final value or implicit unit result.
        let can_reach_body_end = self.visit_block_statements(block);
        let mut completes_normally = can_reach_body_end;
        match (&block.value, can_reach_body_end) {
            (Some(value), true) if matches!(&value.kind, ExpressionKind::If { .. }) => {
                let outcome = self.analyze_conditional_expression(
                    value,
                    Some(expected),
                    ConditionalUse::CallableCompletion,
                    true,
                );
                if let Some(outcome) = outcome {
                    completes_normally &= !self.is_divergence(outcome.typed.type_id);
                    if outcome.explicitly_produces_value {
                        if self.type_contains_tracked_reference(expected)
                            && !self.validate_tracked_return_source(value)
                        {
                            self.checking.errors.push(ExpressionCheckingError {
                                kind: ExpressionCheckingErrorKind::InvalidTrackedReturnSource,
                                span: value.span,
                            });
                            self.checking.expressions.insert(
                                value.id,
                                TypedExpression {
                                    type_id: self.types.types().recovery(),
                                    category: outcome.typed.category,
                                },
                            );
                        } else {
                            self.record_return_transfer(value, outcome.typed);
                        }
                    }
                }
            }
            (Some(value), true) => {
                self.analyze_return_value(value, expected);
                completes_normally &= self
                    .checking
                    .expressions
                    .get(&value.id)
                    .is_none_or(|typed| !self.is_divergence(typed.type_id));
            }
            (Some(value), false) => {
                let enclosing_reachability = self.current_path_reachable;
                self.current_path_reachable = false;
                let _ = self.synthesize_discarded(value);
                self.current_path_reachable = enclosing_reachability;
            }
            (None, true) => self.check_absent_value(block.id, expected, block.span),
            (None, false) => {}
        }
        self.leave_deferred_block(
            block.id,
            block.value.as_deref().map_or(block.id, |value| value.id),
            block.value.as_deref().map(|value| value.id),
            completes_normally,
        );
        self.current_callable_result = enclosing_callable_result;
    }

    fn visit_statement(&mut self, statement: &Statement) -> StatementFlow {
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
                if source.is_some_and(|typed| self.is_divergence(typed.type_id)) {
                    StatementFlow::Diverges
                } else {
                    StatementFlow::Completes
                }
            }
            StatementKind::Expression(expression) => {
                if self
                    .synthesize_discarded(expression)
                    .is_some_and(|typed| self.is_divergence(typed.type_id))
                {
                    StatementFlow::Diverges
                } else {
                    StatementFlow::Completes
                }
            }
            StatementKind::Function(function) => {
                self.visit_function(function);
                StatementFlow::Completes
            }
            StatementKind::Return(value) => {
                let callable = self
                    .context
                    .callable_for_return(statement.id)
                    .expect("return statement must have a resolved callable target");
                let expected = self.resolved_callable_signature(callable).return_type;
                if let Some(value) = value {
                    self.analyze_return_value(value, expected);
                    if self
                        .checking
                        .expressions
                        .get(&value.id)
                        .is_some_and(|typed| self.is_divergence(typed.type_id))
                    {
                        return StatementFlow::Diverges;
                    }
                } else {
                    self.check_absent_value(statement.id, expected, statement.span);
                }
                self.record_deferred_exit(
                    statement.id,
                    DeferredCleanupEdgeKind::Return,
                    None,
                    value.as_ref().map(|value| value.id),
                );
                self.release_all_narrowings(statement.id, NarrowingEdgeKind::Return);
                StatementFlow::Returns
            }
            StatementKind::Break(value) => self.analyze_break(statement, value.as_ref()),
            StatementKind::Continue => self.analyze_continue(statement),
            StatementKind::Coroutine(call) => self.analyze_coroutine_start(statement, call),
            StatementKind::Defer(call) => self.analyze_deferred_call(statement, call),
        }
    }

    /// Checks a deferred call now, retaining every value needed to invoke it
    /// when the innermost executable lexical block exits.
    fn analyze_deferred_call(
        &mut self,
        statement: &Statement,
        call: &Expression,
    ) -> StatementFlow {
        let result = self.synthesize(call);
        let statement_typed = self.fresh_primitive(PrimitiveType::Unit);
        self.checking.expressions.insert(statement.id, statement_typed);
        self.checking.explicit_values.insert(statement.id, false);

        let ExpressionKind::Call { callee, arguments } = &call.kind else {
            unreachable!("the parser restricts `defer` to call expressions")
        };
        if self.call_preparation_diverges(callee, arguments) {
            return StatementFlow::Diverges;
        }
        let Some(result) = result else {
            return StatementFlow::Completes;
        };
        if self.is_recovery(result.type_id) {
            return StatementFlow::Completes;
        }
        let Some(target) = self.resolved_coroutine_call_target(call, callee) else {
            return StatementFlow::Completes;
        };

        let mut prepared = Vec::new();
        let receiver = self.coroutine_receiver(callee);
        if let Some(receiver) = receiver {
            if let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Receiver,
                receiver,
                None,
            ) {
                prepared.push(value);
            }
        } else if matches!(&target, ResolvedCoroutineCallTarget::CallableValue { .. })
            && let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Callable,
                callee,
                self.checking
                    .expressions
                    .get(&callee.id)
                    .copied()
                    .and_then(|typed| self.argument_transfer(typed)),
            )
        {
            prepared.push(value);
        }
        for (index, argument) in arguments.iter().enumerate() {
            if let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Argument { index },
                argument,
                None,
            ) {
                prepared.push(value);
            }
        }

        let active = self
            .active_deferred_blocks
            .last_mut()
            .expect("a defer statement is checked inside an executable block");
        let block = active.block;
        let registration_order = active.next_registration_order;
        active.next_registration_order += 1;
        let reachable = self.current_path_reachable;
        if reachable {
            active.registrations.push(statement.id);
        }
        self.checking.resolved_deferred_calls.insert(
            statement.id,
            ResolvedDeferredCall {
                call: call.id,
                target,
                prepared,
                discarded_result: result.type_id,
                statement_type: statement_typed.type_id,
                block,
                registration_order,
                reachable,
            },
        );
        StatementFlow::Completes
    }

    /// Checks a coroutine start through the ordinary call path, then records
    /// the values which must be retained without treating the call's eventual
    /// result (including divergence or Error) as the statement result.
    fn analyze_coroutine_start(
        &mut self,
        statement: &Statement,
        call: &Expression,
    ) -> StatementFlow {
        let result = self.synthesize(call);
        let statement_typed = self.fresh_primitive(PrimitiveType::Unit);
        self.checking.expressions.insert(statement.id, statement_typed);
        self.checking.explicit_values.insert(statement.id, false);

        let ExpressionKind::Call { callee, arguments } = &call.kind else {
            unreachable!("the parser restricts `co` to call expressions")
        };
        if self.call_preparation_diverges(callee, arguments) {
            return StatementFlow::Diverges;
        }
        let Some(result) = result else {
            return StatementFlow::Completes;
        };
        if self.is_recovery(result.type_id) {
            return StatementFlow::Completes;
        }
        let Some(target) = self.resolved_coroutine_call_target(call, callee) else {
            return StatementFlow::Completes;
        };

        let mut prepared = Vec::new();
        let receiver = self.coroutine_receiver(callee);
        if let Some(receiver) = receiver {
            if let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Receiver,
                receiver,
                None,
            ) {
                prepared.push(value);
            }
        } else if matches!(&target, ResolvedCoroutineCallTarget::CallableValue { .. })
            && let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Callable,
                callee,
                self.checking
                    .expressions
                    .get(&callee.id)
                    .copied()
                    .and_then(|typed| self.argument_transfer(typed)),
            )
        {
            prepared.push(value);
        }
        for (index, argument) in arguments.iter().enumerate() {
            // Compile-time template arguments are present in the source call
            // but deliberately have no checked runtime expression.
            if let Some(value) = self.coroutine_prepared_value(
                CoroutinePreparedRole::Argument { index },
                argument,
                None,
            ) {
                prepared.push(value);
            }
        }

        self.checking.resolved_coroutine_starts.insert(
            statement.id,
            ResolvedCoroutineStart {
                call: call.id,
                target,
                prepared,
                discarded_result: result.type_id,
                statement_type: statement_typed.type_id,
            },
        );
        StatementFlow::Completes
    }

    fn resolved_coroutine_call_target(
        &self,
        call: &Expression,
        callee: &Expression,
    ) -> Option<ResolvedCoroutineCallTarget> {
        if let Some(specialization) = self
            .runtime_specialization_calls
            .get(&call.id)
            .copied()
        {
            return Some(ResolvedCoroutineCallTarget::RuntimeSpecialization(
                specialization,
            ));
        }
        if let Some(operation) = self
            .checking
            .resolved_queue_operations
            .get(&callee.id)
            .cloned()
        {
            return Some(ResolvedCoroutineCallTarget::Queue(operation));
        }
        if let Some(operation) = self
            .checking
            .resolved_sequence_operations
            .get(&callee.id)
            .copied()
        {
            return Some(ResolvedCoroutineCallTarget::Sequence(operation));
        }
        if let Some(member) = self.checking.resolved_members.get(&callee.id).copied()
            && !matches!(
                member,
                ResolvedMember::Field { .. } | ResolvedMember::TupleElement { .. }
            )
        {
            return Some(ResolvedCoroutineCallTarget::Member(member));
        }
        if let Some(operation) = self
            .checking
            .resolved_builtin_operations
            .get(&callee.id)
            .cloned()
        {
            return Some(ResolvedCoroutineCallTarget::Builtin(operation));
        }
        if matches!(&callee.kind, ExpressionKind::Identifier)
            && let Some(symbol) = self.names.symbol_for_reference(callee.id)
            && self
                .names
                .symbols()
                .symbol(symbol)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
        {
            let declaration = self
                .names
                .declarations()
                .iter()
                .find_map(|(declaration, declared_symbol)| {
                    (*declared_symbol == symbol).then_some(*declaration)
                })
                .expect("function symbol must have a declaration identity");
            return Some(ResolvedCoroutineCallTarget::Function {
                declaration,
                symbol,
            });
        }
        let typed = self.checking.expressions.get(&callee.id)?;
        Some(ResolvedCoroutineCallTarget::CallableValue {
            callee: callee.id,
            callable_type: typed.type_id,
        })
    }

    fn coroutine_receiver<'expression>(
        &self,
        callee: &'expression Expression,
    ) -> Option<&'expression Expression> {
        let ExpressionKind::MemberAccess { object, .. } = &callee.kind else {
            return None;
        };
        let has_receiver = matches!(
            self.checking.resolved_members.get(&callee.id),
            Some(
                ResolvedMember::Method { .. }
                    | ResolvedMember::InterfaceMethod { .. }
                    | ResolvedMember::Copy { .. }
            )
        ) || self
            .checking
            .resolved_queue_operations
            .contains_key(&callee.id)
            || self
                .checking
                .resolved_sequence_operations
                .contains_key(&callee.id);
        has_receiver.then_some(object.as_ref())
    }

    fn coroutine_prepared_value(
        &self,
        role: CoroutinePreparedRole,
        expression: &Expression,
        transfer: Option<ValueTransfer>,
    ) -> Option<CoroutinePreparedValue> {
        let typed = self.checking.expressions.get(&expression.id).copied()?;
        Some(CoroutinePreparedValue {
            role,
            expression: expression.id,
            type_id: typed.type_id,
            category: typed.category,
            transfer: transfer.or_else(|| self.checking.transfers.get(&expression.id).copied()),
            place: self.checking.physical_places.get(&expression.id).cloned(),
            tracked_sources: self.tracked_lifetime_sources(expression),
        })
    }

    /// The target body runs later for `co` and `defer`, but evaluating a
    /// first-class callee, receiver, or runtime argument still happens now.
    /// Divergence during that preparation prevents the registration/start and
    /// follows panic's non-unwinding behavior.
    fn call_preparation_diverges(
        &self,
        callee: &Expression,
        arguments: &[Expression],
    ) -> bool {
        let prepared_callee = self.coroutine_receiver(callee).unwrap_or(callee);
        self.checking
            .expressions
            .get(&prepared_callee.id)
            .is_some_and(|typed| self.is_divergence(typed.type_id))
            || arguments.iter().any(|argument| {
                self.checking
                    .expressions
                    .get(&argument.id)
                    .is_some_and(|typed| self.is_divergence(typed.type_id))
            })
    }

    fn enter_deferred_block(&mut self, block: NodeId) {
        self.active_deferred_blocks.push(ActiveDeferredBlock {
            block,
            registrations: Vec::new(),
            next_registration_order: 0,
        });
    }

    /// Leaves one lexical block after its optional result has been evaluated.
    /// A divergent tail, including panic, has no normal cleanup edge.
    fn leave_deferred_block(
        &mut self,
        block: NodeId,
        normal_source: NodeId,
        transfer_value: Option<NodeId>,
        completes_normally: bool,
    ) {
        let active = self
            .active_deferred_blocks
            .pop()
            .expect("executable block analysis must retain defer context");
        assert_eq!(active.block, block, "defer blocks must remain lexical");
        if completes_normally && self.current_path_reachable && !active.registrations.is_empty() {
            self.checking.deferred_cleanup_edges.push(DeferredCleanupEdge {
                source: normal_source,
                kind: DeferredCleanupEdgeKind::Normal,
                exited_blocks: vec![block],
                registrations: active.registrations.into_iter().rev().collect(),
                transfer_value,
            });
        }
    }

    /// Records the active registrations executed by a non-local transfer. A
    /// loop transfer stops after its target body; callable exits consume every
    /// active lexical block. Empty scopes remain in `exited_blocks` so typed IR
    /// can preserve the exact scope boundary independently of cleanup count.
    fn record_deferred_exit(
        &mut self,
        source: NodeId,
        kind: DeferredCleanupEdgeKind,
        target_body: Option<NodeId>,
        transfer_value: Option<NodeId>,
    ) {
        if !self.current_path_reachable {
            return;
        }
        let first = target_body.map_or(0, |target| {
            self.active_deferred_blocks
                .iter()
                .rposition(|active| active.block == target)
                .expect("loop transfer target body must have an active defer scope")
        });
        let exited: Vec<_> = self.active_deferred_blocks[first..]
            .iter()
            .rev()
            .collect();
        let registrations: Vec<_> = exited
            .iter()
            .flat_map(|active| active.registrations.iter().rev().copied())
            .collect();
        if registrations.is_empty() {
            return;
        }
        self.checking.deferred_cleanup_edges.push(DeferredCleanupEdge {
            source,
            kind,
            exited_blocks: exited.iter().map(|active| active.block).collect(),
            registrations,
            transfer_value,
        });
    }

    fn analyze_break(
        &mut self,
        statement: &Statement,
        value: Option<&Expression>,
    ) -> StatementFlow {
        let target = self
            .context
            .loop_for_transfer(statement.id)
            .expect("break statement must have a resolved loop target");
        let active = self
            .active_loops
            .last()
            .expect("break statement must be checked inside an active loop");
        assert_eq!(
            active.expression, target,
            "resolved break target must be the innermost active loop"
        );
        let expected_result_type = active.expected_result_type;
        let typed = match value {
            Some(value) => match expected_result_type {
                Some(expected_result_type) => self.check(value, expected_result_type),
                None => self.synthesize(value),
            },
            None => Some(self.check_bare_break(statement, expected_result_type)),
        };
        let Some(typed) = typed else {
            let destination = self
                .active_loops
                .last()
                .expect("break statement must be checked inside an active loop")
                .entry_narrowings
                .clone();
            let target_body = self
                .active_loops
                .last()
                .expect("break statement must be checked inside an active loop")
                .body;
            self.record_deferred_exit(
                statement.id,
                DeferredCleanupEdgeKind::Break(target),
                Some(target_body),
                value.map(|value| value.id),
            );
            self.transition_current_narrowings(
                statement.id,
                NarrowingEdgeKind::Break,
                destination,
            );
            return StatementFlow::Breaks(target);
        };
        if self.is_divergence(typed.type_id) {
            return StatementFlow::Diverges;
        }
        if self.current_path_reachable {
            let categories = self.current_binding_categories.clone();
            let tracked_bindings = self.current_tracked_bindings.clone();
            let narrowings = self
                .active_loops
                .last()
                .expect("break statement must be checked inside an active loop")
                .entry_narrowings
                .clone();
            let active = self
                .active_loops
                .last_mut()
                .expect("break statement must be checked inside an active loop");
            assert_eq!(
                active.expression, target,
                "resolved break target must be the innermost active loop"
            );
            active.breaks.push(LoopResultPath {
                value: value.map(|value| value.id),
                span: value.map_or(statement.span, |value| value.span),
                typed,
                categories,
                tracked_bindings,
                narrowings,
            });
        }
        let destination = self
            .active_loops
            .last()
            .expect("break statement must be checked inside an active loop")
            .entry_narrowings
            .clone();
        let target_body = self
            .active_loops
            .last()
            .expect("break statement must be checked inside an active loop")
            .body;
        self.record_deferred_exit(
            statement.id,
            DeferredCleanupEdgeKind::Break(target),
            Some(target_body),
            value.map(|value| value.id),
        );
        self.transition_current_narrowings(statement.id, NarrowingEdgeKind::Break, destination);
        StatementFlow::Breaks(target)
    }

    fn check_bare_break(
        &mut self,
        statement: &Statement,
        expected: Option<TypeId>,
    ) -> TypedExpression {
        let unit = self.fresh_primitive(PrimitiveType::Unit);
        expected.map_or(unit, |expected| {
            self.check_implicit_value(statement.id, statement.span, expected, unit)
        })
    }

    fn analyze_continue(&mut self, statement: &Statement) -> StatementFlow {
        let target = self
            .context
            .loop_for_transfer(statement.id)
            .expect("continue statement must have a resolved loop target");
        if self.current_path_reachable {
            let categories = self.current_binding_categories.clone();
            let tracked_bindings = self.current_tracked_bindings.clone();
            let narrowings = self.current_narrowings.clone();
            let active = self
                .active_loops
                .last_mut()
                .expect("continue statement must be checked inside an active loop");
            assert_eq!(
                active.expression, target,
                "resolved continue target must be the innermost active loop"
            );
            active
                .continues
                .push((categories, tracked_bindings, narrowings));
        }
        let destination = self
            .active_loops
            .last()
            .expect("continue statement must be checked inside an active loop")
            .entry_narrowings
            .clone();
        let target_body = self
            .active_loops
            .last()
            .expect("continue statement must be checked inside an active loop")
            .body;
        self.record_deferred_exit(
            statement.id,
            DeferredCleanupEdgeKind::Continue(target),
            Some(target_body),
            None,
        );
        self.transition_current_narrowings(statement.id, NarrowingEdgeKind::Continue, destination);
        StatementFlow::Continues(target)
    }

    /// Analyzes every statement in source order and reports whether sequential
    /// execution can reach the block's final expression or closing brace.
    fn visit_block_statements(&mut self, block: &Block) -> bool {
        let enclosing_reachability = self.current_path_reachable;
        let mut can_reach_block_end = true;
        for statement in &block.statements {
            self.current_path_reachable = enclosing_reachability && can_reach_block_end;
            let unreachable_categories =
                (!self.current_path_reachable).then(|| self.current_binding_categories.clone());
            let unreachable_tracked_bindings =
                (!self.current_path_reachable).then(|| self.current_tracked_bindings.clone());
            let unreachable_narrowings =
                (!self.current_path_reachable).then(|| self.current_narrowings.clone());
            let flow = self.visit_statement(statement);
            if let Some(categories) = unreachable_categories {
                self.current_binding_categories = categories;
            }
            if let Some(bindings) = unreachable_tracked_bindings {
                self.current_tracked_bindings = bindings;
            }
            if let Some(narrowings) = unreachable_narrowings {
                self.current_narrowings = narrowings;
            }
            can_reach_block_end &= flow.can_complete_normally();
        }
        self.current_path_reachable = enclosing_reachability;
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
                .resolved_type_syntax(id)
                .expect("binding annotation must have a resolved type");
            let resolved = self.with_value_capability(resolved, qualifiers.value);
            self.validate_borrow_storage_type(resolved, statement.span);
            resolved
        });
        let source = match expected {
            Some(expected) => self.check(initializer, expected),
            None => self.synthesize(initializer),
        };
        let Some(mut source) = source else {
            return None;
        };

        if self.reject_escaping_temporary_tracked_borrow(initializer) {
            source = TypedExpression {
                type_id: self.types.types().recovery(),
                category: ValueCategory::BorrowedPlace,
            };
            self.checking.expressions.insert(initializer.id, source);
        }

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
        let sources = self.tracked_lifetime_sources(initializer);
        self.set_tracked_binding_sources(statement.id, symbol, sources);
        self.current_binding_categories.insert(symbol, category);
        Some(source)
    }

    fn set_tracked_binding_sources(
        &mut self,
        node: NodeId,
        symbol: SymbolId,
        sources: Vec<PhysicalPlace>,
    ) {
        if sources.is_empty() {
            self.current_tracked_bindings.remove(&symbol);
            self.checking.tracked_binding_lifetimes.remove(&symbol);
            self.checking.gc_owner_roots.remove(&node);
            return;
        }
        let link = TrackedLifetimeLink {
            sources: sources.clone(),
        };
        self.current_tracked_bindings.insert(symbol, link.clone());
        self.checking.tracked_binding_lifetimes.insert(symbol, link);
        let gc_roots: Vec<_> = sources
            .into_iter()
            .filter(|source| source.storage == ValueCategory::GcReference)
            .collect();
        if gc_roots.is_empty() || !self.symbol_is_live_after(symbol, node) {
            self.checking.gc_owner_roots.remove(&node);
        } else {
            self.checking.gc_owner_roots.insert(node, gc_roots);
        }
    }

    fn remove_tracked_borrow_for_expression(&mut self, expression: &Expression) {
        self.checking.tracked_borrows.remove(&expression.id);
        self.checking.tracked_lifetime_links.remove(&expression.id);
        if let ExpressionKind::Group(inner) = &expression.kind {
            self.remove_tracked_borrow_for_expression(inner);
        }
    }

    fn reject_escaping_temporary_tracked_borrow(&mut self, expression: &Expression) -> bool {
        let escapes = self
            .tracked_lifetime_sources(expression)
            .iter()
            .any(|source| matches!(source.root, PhysicalPlaceRoot::Expression(_)));
        if escapes {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TemporaryTrackedBorrowEscapes,
                span: expression.span,
            });
            self.remove_tracked_borrow_for_expression(expression);
            self.checking.expressions.insert(
                expression.id,
                TypedExpression {
                    type_id: self.types.types().recovery(),
                    category: ValueCategory::BorrowedPlace,
                },
            );
        }
        escapes
    }

    /// Checks a returned expression and records how its value enters the
    /// caller-owned result location.
    fn analyze_return_value(&mut self, value: &Expression, expected: TypeId) {
        let Some(source) = self.check_with_capability(value, expected, true) else {
            return;
        };
        if self.type_contains_tracked_reference(expected)
            && !self.validate_tracked_return_source(value)
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidTrackedReturnSource,
                span: value.span,
            });
            self.checking.expressions.insert(
                value.id,
                TypedExpression {
                    type_id: self.types.types().recovery(),
                    category: source.category,
                },
            );
            return;
        }
        self.record_return_transfer(value, source);
    }

    fn validate_tracked_return_source(&self, value: &Expression) -> bool {
        let sources = self.tracked_lifetime_sources(value);
        let aggregate_result = self
            .checking
            .expressions
            .get(&value.id)
            .is_some_and(|typed| {
                self.tracked_reference_parts(typed.type_id).is_none()
                    && self.type_contains_tracked_reference(typed.type_id)
            });
        !sources.is_empty()
            && sources
                .iter()
                .all(|source| {
                    self.return_roots_contain(&self.current_tracked_return_roots, source.root)
                        || (aggregate_result
                            && self.return_roots_contain(
                                &self.current_borrow_containing_return_roots,
                                source.root,
                            ))
                })
    }

    fn return_roots_contain(
        &self,
        permitted: &HashSet<PhysicalPlaceRoot>,
        source: PhysicalPlaceRoot,
    ) -> bool {
        permitted.contains(&source)
            || matches!(
                source,
                PhysicalPlaceRoot::DisplacedSymbol(symbol, _)
                    if permitted.contains(&PhysicalPlaceRoot::Symbol(symbol))
            )
    }

    fn tracked_lifetime_sources(&self, expression: &Expression) -> Vec<PhysicalPlace> {
        if let Some(link) = self.checking.tracked_lifetime_links.get(&expression.id) {
            return link.sources.clone();
        }
        if let Some(borrow) = self.checking.tracked_borrows.get(&expression.id) {
            return vec![borrow.source.clone()];
        }
        if let ExpressionKind::Group(inner) = &expression.kind {
            return self.tracked_lifetime_sources(inner);
        }
        if self
            .checking
            .expressions
            .get(&expression.id)
            .is_some_and(|typed| self.type_contains_tracked_reference(typed.type_id))
            && let Some(place) = self.checking.physical_places.get(&expression.id)
        {
            return vec![place.clone()];
        }
        Vec::new()
    }

    fn tracked_input_lifetime_sources(&self, expression: &Expression) -> Vec<PhysicalPlace> {
        let sources = self.tracked_lifetime_sources(expression);
        if !sources.is_empty() {
            return sources;
        }
        if let Some(place) = self.checking.physical_places.get(&expression.id) {
            return vec![place.clone()];
        }
        let storage = self
            .checking
            .expressions
            .get(&expression.id)
            .map_or(ValueCategory::FreshTemporary, |typed| typed.category);
        vec![PhysicalPlace {
            root: PhysicalPlaceRoot::Expression(expression.id),
            projections: Vec::new(),
            storage,
        }]
    }

    fn extend_tracked_lifetime_sources(
        &self,
        destination: &mut Vec<PhysicalPlace>,
        expression: &Expression,
    ) {
        for source in self.tracked_lifetime_sources(expression) {
            if !destination.contains(&source) {
                destination.push(source);
            }
        }
    }

    fn record_tracked_lifetime_link(&mut self, node: NodeId, sources: Vec<PhysicalPlace>) {
        if sources.is_empty() {
            self.checking.tracked_lifetime_links.remove(&node);
        } else {
            self.checking
                .tracked_lifetime_links
                .insert(node, TrackedLifetimeLink { sources });
        }
    }

    fn record_return_transfer(&mut self, value: &Expression, source: TypedExpression) {
        if self.is_recovery(source.type_id) || self.is_divergence(source.type_id) {
            return;
        }

        if let Some(transfer) = self.return_transfer(source) {
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

    fn return_transfer(&self, source: TypedExpression) -> Option<ValueTransfer> {
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("return type belongs to the program type store");
        if semantic.storage_semantics() == Some(StorageSemantics::TrackedReference) {
            Some(ValueTransfer::Borrow)
        } else if source.category == ValueCategory::BorrowedPlace
            && self.contains_non_escaping_erased_view(source.type_id)
        {
            None
        } else if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            Some(ValueTransfer::CopyGcReference)
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
                Some(CopySemantics::TrackedPayload) => None,
                Some(CopySemantics::GcPayload) => {
                    unreachable!("GC return storage was handled above")
                }
            }
        }
    }

    /// Checks an implicit unit result, such as a bare return or a callable body
    /// that reaches its closing brace without a final expression.
    fn check_absent_value(&mut self, node: NodeId, expected: TypeId, span: Span) {
        if self.is_recovery(expected) {
            return;
        }
        let unit = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Unit, AccessCapability::Const);
        let found = TypedExpression {
            type_id: unit,
            category: ValueCategory::FreshTemporary,
        };
        match self.classify_contextual_assignment(found, expected, false) {
            Ok(assignment) => {
                let _ = self.apply_contextual_assignment(node, expected, found, assignment);
            }
            Err(kind) => self.checking.errors.push(ExpressionCheckingError { kind, span }),
        }
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
        self.enter_deferred_block(block.id);
        let can_reach_block_end = self.visit_block_statements(block);
        let outcome = (|| {
            if !can_reach_block_end {
                if let Some(value) = &block.value {
                    let enclosing_reachability = self.current_path_reachable;
                    self.current_path_reachable = false;
                    let _ = self.synthesize_discarded(value);
                    self.current_path_reachable = enclosing_reachability;
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
                    explicitly_produces_value: self.explicitly_produces_value(value),
                }
            };
            Some(BlockOutcome {
                typed: outcome.typed,
                explicit_value: outcome.explicitly_produces_value.then_some(value.id),
            })
        })();
        let completes_normally = can_reach_block_end
            && outcome
                .as_ref()
                .is_none_or(|outcome| !self.is_divergence(outcome.typed.type_id));
        self.leave_deferred_block(
            block.id,
            block.value.as_deref().map_or(block.id, |value| value.id),
            block.value.as_deref().map(|value| value.id),
            completes_normally,
        );
        self.release_block_local_narrowings(block);
        outcome
    }

    fn release_block_local_narrowings(&mut self, block: &Block) {
        let local_symbols: HashSet<_> = block
            .statements
            .iter()
            .filter_map(|statement| {
                matches!(&statement.kind, StatementKind::Binding { .. }).then(|| {
                    self.names
                        .symbol_for_declaration(statement.id)
                        .expect("block binding must have a resolved symbol")
                })
            })
            .collect();
        if local_symbols.is_empty() {
            return;
        }
        self.current_tracked_bindings
            .retain(|symbol, _| !local_symbols.contains(symbol));
        let from = self.current_narrowings.clone();
        self.current_narrowings.retain(|place, _| {
            !matches!(place.root, NarrowingRoot::Symbol(symbol) if local_symbols.contains(&symbol))
        });
        let to = self.current_narrowings.clone();
        self.record_narrowing_transition(block.id, NarrowingEdgeKind::Join, &from, &to);
    }

    /// Analyzes an expression whose result is discarded by its containing
    /// statement, allowing a non-value-producing conditional to omit `else`.
    fn synthesize_discarded(&mut self, expression: &Expression) -> Option<TypedExpression> {
        let outcome = match &expression.kind {
            ExpressionKind::Group(inner) => {
                let typed = self.synthesize_discarded(inner)?;
                let explicitly_produces_value = self.explicitly_produces_value(inner);
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
            ExpressionKind::TypeAscription { value, type_syntax } => {
                let typed = self.synthesize_type_ascription(expression, value, type_syntax)?;
                ExpressionOutcome {
                    typed,
                    explicitly_produces_value: self.explicitly_produces_value(expression),
                }
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

    /// Analyzes a boolean as a pair of control-flow outcomes. Ordinary boolean
    /// expressions preserve the incoming facts; `is`, grouping, `!`, `&&`, and
    /// `||` refine and combine them with their real short-circuit semantics.
    fn analyze_boolean_condition(&mut self, expression: &Expression) -> Option<BooleanFlow> {
        let incoming = self.current_narrowings.clone();
        let bool_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Bool, AccessCapability::Const);
        let flow = match &expression.kind {
            ExpressionKind::Group(inner) => {
                let flow = self.analyze_boolean_condition(inner)?;
                self.checking.expressions.insert(
                    expression.id,
                    TypedExpression {
                        type_id: if flow.invalid {
                            self.types.types().recovery()
                        } else {
                            bool_type
                        },
                        category: ValueCategory::FreshTemporary,
                    },
                );
                self.checking.explicit_values.insert(expression.id, true);
                flow
            }
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => {
                let inner = self.analyze_boolean_condition(operand)?;
                self.checking.expressions.insert(
                    expression.id,
                    TypedExpression {
                        type_id: if inner.invalid {
                            self.types.types().recovery()
                        } else {
                            bool_type
                        },
                        category: ValueCategory::FreshTemporary,
                    },
                );
                self.checking.explicit_values.insert(expression.id, true);
                BooleanFlow {
                    when_true: inner.when_false,
                    when_false: inner.when_true,
                    invalid: inner.invalid,
                }
            }
            ExpressionKind::Binary {
                left,
                operator: BinaryOperator::LogicalAnd,
                right,
            } => {
                let left_flow = self.analyze_boolean_condition(left)?;
                let right_flow = self.analyze_short_circuit_operand(
                    right,
                    left_flow.when_true.as_ref(),
                    &incoming,
                )?;
                let when_false = self.merge_optional_narrowing_states([
                    left_flow.when_false.as_ref(),
                    right_flow.when_false.as_ref(),
                ]);
                if let Some(merged) = &when_false {
                    for state in [left_flow.when_false.as_ref(), right_flow.when_false.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        self.record_narrowing_transition(
                            expression.id,
                            NarrowingEdgeKind::False,
                            state,
                            merged,
                        );
                    }
                }
                let invalid = left_flow.invalid || right_flow.invalid;
                self.record_boolean_expression(expression, bool_type, invalid);
                BooleanFlow {
                    when_true: right_flow.when_true,
                    when_false,
                    invalid,
                }
            }
            ExpressionKind::Binary {
                left,
                operator: BinaryOperator::LogicalOr,
                right,
            } => {
                let left_flow = self.analyze_boolean_condition(left)?;
                let right_flow = self.analyze_short_circuit_operand(
                    right,
                    left_flow.when_false.as_ref(),
                    &incoming,
                )?;
                let when_true = self.merge_optional_narrowing_states([
                    left_flow.when_true.as_ref(),
                    right_flow.when_true.as_ref(),
                ]);
                if let Some(merged) = &when_true {
                    for state in [left_flow.when_true.as_ref(), right_flow.when_true.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        self.record_narrowing_transition(
                            expression.id,
                            NarrowingEdgeKind::True,
                            state,
                            merged,
                        );
                    }
                }
                let invalid = left_flow.invalid || right_flow.invalid;
                self.record_boolean_expression(expression, bool_type, invalid);
                BooleanFlow {
                    when_true,
                    when_false: right_flow.when_false,
                    invalid,
                }
            }
            ExpressionKind::TypeTest { value, type_syntax } => {
                self.analyze_type_test_flow(expression, value, type_syntax, &incoming)?
            }
            _ => {
                let checked = self.check(expression, bool_type)?;
                let invalid = self.is_recovery(checked.type_id);
                BooleanFlow {
                    when_true: Some(incoming.clone()),
                    when_false: Some(incoming.clone()),
                    invalid,
                }
            }
        };
        self.current_narrowings = incoming;
        Some(flow)
    }

    fn analyze_short_circuit_operand(
        &mut self,
        expression: &Expression,
        incoming: Option<&NarrowingState>,
        fallback: &NarrowingState,
    ) -> Option<BooleanFlow> {
        let reachable = incoming.is_some();
        self.current_narrowings = incoming.cloned().unwrap_or_else(|| fallback.clone());
        let enclosing_reachability = self.current_path_reachable;
        self.current_path_reachable &= reachable;
        let flow = self.analyze_boolean_condition(expression);
        self.current_path_reachable = enclosing_reachability;
        flow.map(|flow| {
            if reachable {
                flow
            } else {
                BooleanFlow {
                    when_true: None,
                    when_false: None,
                    invalid: flow.invalid,
                }
            }
        })
    }

    fn record_boolean_expression(&mut self, expression: &Expression, bool_type: TypeId, invalid: bool) {
        self.checking.expressions.insert(
            expression.id,
            TypedExpression {
                type_id: if invalid {
                    self.types.types().recovery()
                } else {
                    bool_type
                },
                category: ValueCategory::FreshTemporary,
            },
        );
        self.checking.explicit_values.insert(expression.id, true);
    }

    fn merge_optional_narrowing_states<'state>(
        &mut self,
        states: impl IntoIterator<Item = Option<&'state NarrowingState>>,
    ) -> Option<NarrowingState> {
        let reachable: Vec<_> = states.into_iter().flatten().collect();
        (!reachable.is_empty()).then(|| self.merge_narrowing_states(&reachable))
    }

    fn analyze_type_test_flow(
        &mut self,
        expression: &Expression,
        value: &Expression,
        type_syntax: &TypeSyntax,
        incoming: &NarrowingState,
    ) -> Option<BooleanFlow> {
        let typed = self.synthesize_type_test(expression, value, type_syntax)?;
        self.checking.expressions.insert(expression.id, typed);
        self.checking.explicit_values.insert(expression.id, true);
        if self.is_recovery(typed.type_id) {
            return Some(BooleanFlow {
                when_true: Some(incoming.clone()),
                when_false: Some(incoming.clone()),
                invalid: true,
            });
        }

        let place = self.narrowing_place(value);
        let source = self
            .synthesize(value)
            .expect("a valid type-test value was already synthesized");
        let previous = place
            .as_ref()
            .and_then(|place| self.effective_narrowing(place));
        let source_union = previous.map_or(source.type_id, |fact| fact.source_union);
        let possible_type = previous.map_or(source_union, |fact| fact.narrowed_type);
        let possible = self
            .union_members(possible_type)
            .unwrap_or_else(|| vec![possible_type]);
        let tested = self
            .resolved_type_syntax(type_syntax.id)
            .expect("type-test syntax must have been resolved");
        let tested_members = self.union_members(tested).unwrap_or_else(|| vec![tested]);
        let matching: Vec<_> = possible
            .iter()
            .copied()
            .filter(|member| tested_members.contains(member))
            .collect();
        let remaining: Vec<_> = possible
            .iter()
            .copied()
            .filter(|member| !tested_members.contains(member))
            .collect();

        let true_type = (!matching.is_empty())
            .then(|| self.narrowed_subset_type(source_union, matching));
        let false_type = (!remaining.is_empty())
            .then(|| self.narrowed_subset_type(source_union, remaining));
        let make_state = |narrowed_type: Option<TypeId>| -> Option<NarrowingState> {
            let narrowed_type = narrowed_type?;
            let mut state = incoming.clone();
            if narrowed_type != source_union
                && let Some(place) = &place
            {
                state.entry(place.clone()).or_default().push(NarrowingFact {
                    source_union,
                    narrowed_type,
                });
            }
            Some(state)
        };
        let when_true = make_state(true_type);
        let when_false = make_state(false_type);
        self.checking.type_test_facts.insert(
            expression.id,
            (true_type, false_type),
        );
        if let Some(state) = &when_true {
            self.record_narrowing_transition(expression.id, NarrowingEdgeKind::True, incoming, state);
        }
        if let Some(state) = &when_false {
            self.record_narrowing_transition(expression.id, NarrowingEdgeKind::False, incoming, state);
        }
        Some(BooleanFlow {
            when_true,
            when_false,
            invalid: false,
        })
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
        let unit_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Unit, AccessCapability::Const);
        let mut condition_invalid = false;
        let incoming_categories = self.current_binding_categories.clone();
        let incoming_tracked_bindings = self.current_tracked_bindings.clone();
        let incoming_narrowings = self.current_narrowings.clone();
        let mut fallthrough_categories = incoming_categories.clone();
        let mut fallthrough_tracked_bindings = incoming_tracked_bindings.clone();
        let mut fallthrough_narrowings = Some(incoming_narrowings.clone());
        let mut branches =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let mut branch_categories =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let mut branch_tracked_bindings =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let mut branch_narrowings =
            Vec::with_capacity(arms.len() + if final_else.is_some() { 1 } else { 0 });
        let conditional_nodes: Vec<_> = arms
            .iter()
            .map(|(conditional, _, _)| *conditional)
            .collect();
        for (_, condition, branch) in arms {
            self.current_binding_categories = fallthrough_categories;
            self.current_tracked_bindings = fallthrough_tracked_bindings;
            let condition_reachable = fallthrough_narrowings.is_some();
            self.current_narrowings = fallthrough_narrowings
                .clone()
                .unwrap_or_else(|| incoming_narrowings.clone());
            let enclosing_reachability = self.current_path_reachable;
            self.current_path_reachable &= condition_reachable;
            let condition_flow = self.analyze_boolean_condition(condition)?;
            self.current_path_reachable = enclosing_reachability;
            condition_invalid |= condition_flow.invalid;
            fallthrough_categories = self.current_binding_categories.clone();
            fallthrough_tracked_bindings = self.current_tracked_bindings.clone();
            fallthrough_narrowings = if condition_reachable {
                condition_flow.when_false
            } else {
                None
            };
            self.current_binding_categories = fallthrough_categories.clone();
            self.current_tracked_bindings = fallthrough_tracked_bindings.clone();
            let true_narrowings = if condition_reachable {
                condition_flow.when_true
            } else {
                None
            };
            self.current_narrowings = true_narrowings
                .clone()
                .unwrap_or_else(|| incoming_narrowings.clone());
            self.current_path_reachable = enclosing_reachability && true_narrowings.is_some();
            let mut branch_outcome = self.analyze_block(
                branch,
                expected,
                ConditionalUse::BranchCompletion,
                allow_recursive_copy,
            )?;
            self.current_path_reachable = enclosing_reachability;
            if true_narrowings.is_none() {
                branch_outcome = BlockOutcome {
                    typed: TypedExpression {
                        type_id: self.types.types().divergence(),
                        category: ValueCategory::FreshTemporary,
                    },
                    explicit_value: None,
                };
            }
            branches.push((branch, branch_outcome));
            branch_categories.push(self.current_binding_categories.clone());
            branch_tracked_bindings.push(self.current_tracked_bindings.clone());
            branch_narrowings.push(
                (!self.is_divergence(branch_outcome.typed.type_id))
                    .then(|| self.current_narrowings.clone()),
            );
        }
        if let Some(branch) = final_else {
            self.current_binding_categories = fallthrough_categories.clone();
            self.current_tracked_bindings = fallthrough_tracked_bindings.clone();
            let else_reachable = fallthrough_narrowings.is_some();
            self.current_narrowings = fallthrough_narrowings
                .clone()
                .unwrap_or_else(|| incoming_narrowings.clone());
            let enclosing_reachability = self.current_path_reachable;
            self.current_path_reachable &= else_reachable;
            let mut branch_outcome = self.analyze_block(
                branch,
                expected,
                ConditionalUse::BranchCompletion,
                allow_recursive_copy,
            )?;
            self.current_path_reachable = enclosing_reachability;
            if !else_reachable {
                branch_outcome = BlockOutcome {
                    typed: TypedExpression {
                        type_id: self.types.types().divergence(),
                        category: ValueCategory::FreshTemporary,
                    },
                    explicit_value: None,
                };
            }
            branches.push((branch, branch_outcome));
            branch_categories.push(self.current_binding_categories.clone());
            branch_tracked_bindings.push(self.current_tracked_bindings.clone());
            branch_narrowings.push(
                (!self.is_divergence(branch_outcome.typed.type_id))
                    .then(|| self.current_narrowings.clone()),
            );
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
        let mut completing_tracked_bindings: Vec<_> = branches
            .iter()
            .zip(&branch_tracked_bindings)
            .filter(|((_, outcome), _)| !self.is_divergence(outcome.typed.type_id))
            .map(|(_, bindings)| bindings)
            .collect();
        if !has_else {
            completing_tracked_bindings.push(&fallthrough_tracked_bindings);
        }
        self.current_tracked_bindings = self.merge_tracked_binding_states(
            &incoming_tracked_bindings,
            &completing_tracked_bindings,
        );
        let mut completing_narrowings: Vec<_> = branch_narrowings
            .iter()
            .filter_map(Option::as_ref)
            .collect();
        if !has_else
            && let Some(fallthrough) = &fallthrough_narrowings
        {
            completing_narrowings.push(fallthrough);
        }
        let merged_narrowings = self.merge_narrowing_states(&completing_narrowings);
        for (index, state) in branch_narrowings.iter().enumerate() {
            if let Some(state) = state {
                self.record_narrowing_transition(
                    branches[index].0.id,
                    NarrowingEdgeKind::Join,
                    state,
                    &merged_narrowings,
                );
            }
        }
        if let Some(state) = &fallthrough_narrowings
            && !has_else
        {
            self.record_narrowing_transition(
                expression.id,
                NarrowingEdgeKind::Join,
                state,
                &merged_narrowings,
            );
        }
        self.current_narrowings = merged_narrowings;
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
                    || self.accepts_implicit_unit(expected, unit_type)
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
        if self.type_contains_tracked_reference(outcome.typed.type_id) {
            let mut sources = Vec::new();
            for (block, branch) in &normally_completing {
                if branch.explicit_value.is_none() {
                    continue;
                }
                let Some(value) = block.value.as_deref() else {
                    continue;
                };
                for source in self.tracked_lifetime_sources(value) {
                    if !sources.contains(&source) {
                        sources.push(source);
                    }
                }
            }
            for conditional in &conditional_nodes {
                self.checking.tracked_lifetime_links.insert(
                    conditional.id,
                    TrackedLifetimeLink {
                        sources: sources.clone(),
                    },
                );
            }
        }
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
            if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
                (
                    ValueCategory::GcReference,
                    values
                        .iter()
                        .map(|(id, _)| (*id, ValueTransfer::CopyGcReference))
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
                            (ValueCategory::GcReference, _)
                            | (_, ValueCategory::GcReference) => {
                                ValueCategory::GcReference
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

    /// Merges tracked origins at a control-flow join. An origin present on any
    /// completing path is retained: a later use of the binding can observe
    /// any of those values, so its effective lifetime is their intersection.
    fn merge_tracked_binding_states(
        &self,
        incoming: &TrackedBindingState,
        completing: &[&TrackedBindingState],
    ) -> TrackedBindingState {
        if completing.is_empty() {
            return incoming.clone();
        }
        let mut merged = TrackedBindingState::new();
        let mut symbols: HashSet<_> = incoming.keys().copied().collect();
        for state in completing {
            symbols.extend(state.keys().copied());
        }
        for symbol in symbols {
            let mut sources = Vec::new();
            for state in completing {
                if let Some(link) = state.get(&symbol) {
                    for source in &link.sources {
                        if !sources.contains(source) {
                            sources.push(source.clone());
                        }
                    }
                }
            }
            if !sources.is_empty() {
                merged.insert(symbol, TrackedLifetimeLink { sources });
            }
        }
        merged
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

    fn analyze_loop_expression(
        &mut self,
        expression: &Expression,
        expected: Option<TypeId>,
    ) -> Option<TypedExpression> {
        let first_error = self.checking.errors.len();
        let incoming_categories = self.current_binding_categories.clone();
        let incoming_tracked_bindings = self.current_tracked_bindings.clone();
        let incoming_narrowings = self.current_narrowings.clone();
        let mut invalid = false;
        let (iteration, else_branch, naturally_terminating) = match &expression.kind {
            ExpressionKind::Loop { body } => (
                self.analyze_loop_iterations(
                    expression.id,
                    expected,
                    body,
                    None,
                    None,
                    true,
                )?,
                None,
                false,
            ),
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                let bool_type = self
                    .types
                    .types_mut()
                    .primitive(PrimitiveType::Bool, AccessCapability::Const);
                let iteration = self.analyze_loop_iterations(
                    expression.id,
                    expected,
                    body,
                    Some((condition, bool_type)),
                    None,
                    true,
                )?;
                (
                    iteration,
                    else_branch.as_ref(),
                    true,
                )
            }
            ExpressionKind::RangeFor {
                start,
                end,
                body,
                else_branch,
                ..
            } => {
                let int_type = self
                    .types
                    .types_mut()
                    .primitive(PrimitiveType::Int, AccessCapability::Const);
                let start = self.check(start, int_type)?;
                invalid |= self.is_recovery(start.type_id);
                let end = self.check(end, int_type)?;
                invalid |= self.is_recovery(end.type_id);
                let bounds_complete =
                    !self.is_divergence(start.type_id) && !self.is_divergence(end.type_id);
                let symbol = self
                    .names
                    .symbol_for_declaration(expression.id)
                    .expect("range binding must have a semantic symbol");
                let iteration = self.analyze_loop_iterations(
                    expression.id,
                    expected,
                    body,
                    None,
                    Some((symbol, int_type)),
                    bounds_complete,
                )?;
                (
                    iteration,
                    else_branch.as_ref(),
                    true,
                )
            }
            _ => unreachable!("loop analysis requires a loop expression"),
        };

        invalid |= iteration.invalid;
        let mut paths = iteration.breaks;
        let has_else = else_branch.is_some();
        if naturally_terminating {
            if let Some(natural_categories) = iteration.natural_categories {
                self.current_binding_categories = natural_categories;
                self.current_tracked_bindings = iteration
                    .natural_tracked_bindings
                    .clone()
                    .unwrap_or_else(|| incoming_tracked_bindings.clone());
                self.current_narrowings = iteration
                    .natural_narrowings
                    .clone()
                    .unwrap_or_else(|| incoming_narrowings.clone());
                if let Some(else_branch) = else_branch {
                    let outcome = self.analyze_block(
                        else_branch,
                        expected,
                        ConditionalUse::Value,
                        false,
                    )?;
                    let mut typed = outcome.typed;
                    if let Some(expected) = expected
                        && outcome.explicit_value.is_none()
                        && !self.is_divergence(typed.type_id)
                    {
                        typed = self.check_implicit_value(
                            else_branch.id,
                            else_branch.span,
                            expected,
                            typed,
                        );
                    }
                    if !self.is_divergence(typed.type_id) {
                        invalid |= self.is_recovery(typed.type_id);
                        paths.push(LoopResultPath {
                            value: outcome.explicit_value,
                            span: else_branch
                                .value
                                .as_deref()
                                .map_or(else_branch.span, |value| value.span),
                            typed,
                            categories: self.current_binding_categories.clone(),
                            tracked_bindings: self.current_tracked_bindings.clone(),
                            narrowings: self.current_narrowings.clone(),
                        });
                    }
                }
            } else if let Some(else_branch) = else_branch {
                let enclosing_reachability = self.current_path_reachable;
                self.current_path_reachable = false;
                let _ = self.analyze_block(
                    else_branch,
                    expected,
                    ConditionalUse::Value,
                    false,
                );
                self.current_path_reachable = enclosing_reachability;
            }
        }

        let typed = self.finish_loop_result(
            expression,
            expected,
            &mut paths,
            naturally_terminating,
            has_else,
            invalid,
        );
        let completing_categories: Vec<_> = paths
            .iter()
            .filter(|path| !self.is_divergence(path.typed.type_id))
            .map(|path| &path.categories)
            .collect();
        self.current_binding_categories =
            self.merge_binding_categories(&incoming_categories, &completing_categories);
        let completing_tracked_bindings: Vec<_> = paths
            .iter()
            .filter(|path| !self.is_divergence(path.typed.type_id))
            .map(|path| &path.tracked_bindings)
            .collect();
        self.current_tracked_bindings = self.merge_tracked_binding_states(
            &incoming_tracked_bindings,
            &completing_tracked_bindings,
        );
        let completing_narrowings: Vec<_> = paths
            .iter()
            .filter(|path| !self.is_divergence(path.typed.type_id))
            .map(|path| &path.narrowings)
            .collect();
        self.current_narrowings = self.merge_narrowing_states(&completing_narrowings);
        self.checking.expressions.insert(expression.id, typed);
        self.checking.explicit_values.insert(expression.id, true);
        self.checking.errors[first_error..]
            .sort_by_key(|error| (error.span.start, error.span.end));
        Some(typed)
    }

    /// Reanalyzes a loop iteration from progressively merged loop-head states
    /// until binding provenance stops changing. Semantic facts from speculative
    /// iterations are discarded, leaving only the stable analysis recorded.
    fn analyze_loop_iterations(
        &mut self,
        expression: NodeId,
        expected_result_type: Option<TypeId>,
        body: &Block,
        condition: Option<(&Expression, TypeId)>,
        range_binding: Option<(SymbolId, TypeId)>,
        header_can_complete: bool,
    ) -> Option<LoopIterationOutcome> {
        let base_categories = self.current_binding_categories.clone();
        let base_tracked_bindings = self.current_tracked_bindings.clone();
        let base_narrowings = self.current_narrowings.clone();
        let baseline_checking = self.checking.clone();
        let baseline_loops = self.active_loops.clone();
        let enclosing_reachability = self.current_path_reachable;
        let mut loop_head = base_categories.clone();
        let mut tracked_loop_head = base_tracked_bindings.clone();

        loop {
            self.checking = baseline_checking.clone();
            self.active_loops = baseline_loops.clone();
            self.current_binding_categories = loop_head.clone();
            self.current_tracked_bindings = tracked_loop_head.clone();
            self.current_narrowings = base_narrowings.clone();
            self.current_path_reachable = enclosing_reachability;

            let mut body_reachable = header_can_complete;
            let mut invalid = false;
            let mut natural_narrowings = header_can_complete.then(|| base_narrowings.clone());
            if let Some((condition, _)) = condition {
                let flow = self.analyze_boolean_condition(condition)?;
                invalid |= flow.invalid;
                natural_narrowings = flow.when_false;
                body_reachable &= flow.when_true.is_some();
                self.current_narrowings = flow
                    .when_true
                    .unwrap_or_else(|| base_narrowings.clone());
            }
            if let Some((symbol, int_type)) = range_binding {
                let qualifiers = BindingQualifiers::new(
                    BindingMutability::Const,
                    ValueCapability::Const,
                );
                self.checking.bindings.insert(
                    symbol,
                    BindingSemantics {
                        type_id: int_type,
                        qualifiers,
                        category: ValueCategory::OwnedInlinePlace,
                    },
                );
                self.current_binding_categories
                    .insert(symbol, ValueCategory::OwnedInlinePlace);
            }
            let mut natural_categories = natural_narrowings
                .is_some()
                .then(|| self.current_binding_categories.clone());
            let mut natural_tracked_bindings = natural_narrowings
                .is_some()
                .then(|| self.current_tracked_bindings.clone());
            self.active_loops.push(ActiveLoop {
                expression,
                body: body.id,
                expected_result_type,
                breaks: Vec::new(),
                continues: Vec::new(),
                entry_narrowings: base_narrowings.clone(),
            });
            self.current_path_reachable = enclosing_reachability && body_reachable;
            let body_completes = self.visit_loop_iteration_body(body);
            let normal_state = (body_reachable && body_completes)
                .then(|| self.current_binding_categories.clone());
            let normal_tracked_state = (body_reachable && body_completes)
                .then(|| self.current_tracked_bindings.clone());
            if normal_state.is_some() {
                self.transition_current_narrowings(
                    body.id,
                    NarrowingEdgeKind::LoopBackedge,
                    base_narrowings.clone(),
                );
            }
            let active = self
                .active_loops
                .pop()
                .expect("loop analysis must retain its active loop context");

            let mut looping_states: Vec<_> = active
                .continues
                .iter()
                .map(|(categories, _, _)| categories)
                .collect();
            if let Some(normal_state) = &normal_state {
                looping_states.push(normal_state);
            }
            let next_head = self.merge_binding_categories(&base_categories, &looping_states);
            let mut tracked_looping_states: Vec<_> = vec![&base_tracked_bindings];
            tracked_looping_states.extend(active
                .continues
                .iter()
                .map(|(_, bindings, _)| bindings));
            if let Some(normal_state) = &normal_tracked_state {
                tracked_looping_states.push(normal_state);
            }
            let next_tracked_head = self.merge_tracked_binding_states(
                &base_tracked_bindings,
                &tracked_looping_states,
            );
            if next_head == loop_head && next_tracked_head == tracked_loop_head {
                if let Some((symbol, _)) = range_binding {
                    if let Some(categories) = &mut natural_categories {
                        categories.remove(&symbol);
                    }
                    if let Some(bindings) = &mut natural_tracked_bindings {
                        bindings.remove(&symbol);
                    }
                }
                self.current_path_reachable = enclosing_reachability;
                return Some(LoopIterationOutcome {
                    breaks: active.breaks,
                    natural_categories,
                    natural_tracked_bindings,
                    natural_narrowings,
                    invalid,
                });
            }
            loop_head = next_head;
            tracked_loop_head = next_tracked_head;
        }
    }

    /// Checks an iteration body as discarded syntax. Its final expression is
    /// evaluated on each iteration but never contributes the loop's result.
    fn visit_loop_iteration_body(&mut self, body: &Block) -> bool {
        self.enter_deferred_block(body.id);
        let can_reach_value = self.visit_block_statements(body);
        let typed = body.value.as_deref().and_then(|value| {
            let enclosing_reachability = self.current_path_reachable;
            self.current_path_reachable &= can_reach_value;
            let typed = self.synthesize_discarded(value);
            self.current_path_reachable = enclosing_reachability;
            typed
        });
        let completes_normally = can_reach_value
            && typed.is_none_or(|typed| !self.is_divergence(typed.type_id));
        self.leave_deferred_block(
            body.id,
            body.value.as_deref().map_or(body.id, |value| value.id),
            body.value.as_deref().map(|value| value.id),
            completes_normally,
        );
        completes_normally
    }

    fn finish_loop_result(
        &mut self,
        expression: &Expression,
        expected: Option<TypeId>,
        paths: &mut Vec<LoopResultPath>,
        naturally_terminating: bool,
        has_else: bool,
        mut invalid: bool,
    ) -> TypedExpression {
        let unit = self.fresh_primitive(PrimitiveType::Unit);
        let mut comparable_paths = paths.len();
        if naturally_terminating && !has_else {
            let requires_else = expected.is_some_and(|expected| {
                !self.is_recovery(expected)
                    && !self.accepts_implicit_unit(expected, unit.type_id)
            }) || expected.is_none()
                && paths.iter().any(|path| {
                    !self.is_recovery(path.typed.type_id)
                        && !self
                            .types
                            .types()
                            .has_same_shape(path.typed.type_id, unit.type_id)
                            .expect("loop result types belong to the program type store")
                });
            if requires_else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::LoopElseRequired,
                    span: expression.span,
                });
                invalid = true;
            }
            paths.push(LoopResultPath {
                value: None,
                span: expression.span,
                typed: unit,
                categories: self.current_binding_categories.clone(),
                tracked_bindings: self.current_tracked_bindings.clone(),
                narrowings: self.current_narrowings.clone(),
            });
            if !requires_else {
                comparable_paths = paths.len();
            }
        }

        invalid |= paths
            .iter()
            .any(|path| self.is_recovery(path.typed.type_id));
        if paths.is_empty() {
            return if invalid {
                self.recovery_temporary()
            } else {
                TypedExpression {
                    type_id: self.types.types().divergence(),
                    category: ValueCategory::FreshTemporary,
                }
            };
        }

        let result_type = expected.unwrap_or_else(|| {
            paths
                .iter()
                .find(|path| !self.is_recovery(path.typed.type_id))
                .map_or(self.types.types().recovery(), |path| path.typed.type_id)
        });
        if expected.is_none() && !self.is_recovery(result_type) {
            for path in paths[..comparable_paths].iter() {
                if self.is_recovery(path.typed.type_id) {
                    continue;
                }
                if !self
                    .types
                    .types()
                    .has_same_shape(path.typed.type_id, result_type)
                    .expect("loop result types belong to the program type store")
                {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::TypeMismatch {
                            expected: result_type,
                            found: path.typed.type_id,
                        },
                        span: path.span,
                    });
                    if let Some(value) = path.value {
                        self.checking.expressions.insert(
                            value,
                            TypedExpression {
                                type_id: self.types.types().recovery(),
                                category: path.typed.category,
                            },
                        );
                    }
                    invalid = true;
                }
            }
        }
        if invalid || self.is_recovery(result_type) {
            self.recovery_temporary()
        } else {
            self.merge_loop_values(result_type, paths)
        }
    }

    fn merge_loop_values(
        &mut self,
        result_type: TypeId,
        paths: &[LoopResultPath],
    ) -> TypedExpression {
        if paths.len() == 1 {
            return paths[0].typed;
        }
        let semantic = self
            .types
            .types()
            .get(result_type)
            .expect("loop result type belongs to the program type store");
        let category = if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            for path in paths.iter().filter_map(|path| path.value) {
                self.checking
                    .transfers
                    .insert(path, ValueTransfer::CopyGcReference);
            }
            ValueCategory::GcReference
        } else if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
            for path in paths.iter().filter_map(|path| path.value) {
                self.checking
                    .transfers
                    .insert(path, ValueTransfer::TrivialCopy);
            }
            ValueCategory::FreshTemporary
        } else {
            let all_fresh = paths
                .iter()
                .all(|path| path.typed.category == ValueCategory::FreshTemporary);
            for path in paths {
                let Some(value) = path.value else {
                    continue;
                };
                self.checking.transfers.insert(
                    value,
                    if path.typed.category == ValueCategory::FreshTemporary {
                        ValueTransfer::MoveTemporary
                    } else {
                        ValueTransfer::Borrow
                    },
                );
            }
            if all_fresh {
                ValueCategory::FreshTemporary
            } else {
                ValueCategory::BorrowedPlace
            }
        };
        TypedExpression {
            type_id: result_type,
            category,
        }
    }

    fn check_implicit_value(
        &mut self,
        node: NodeId,
        span: Span,
        expected: TypeId,
        found: TypedExpression,
    ) -> TypedExpression {
        if self.is_recovery(expected) || self.is_recovery(found.type_id) {
            return found;
        }
        match self.classify_contextual_assignment(found, expected, false) {
            Ok(assignment) => {
                self.apply_contextual_assignment(node, expected, found, assignment)
            }
            Err(kind) => {
                self.checking.errors.push(ExpressionCheckingError { kind, span });
                self.recovery_temporary()
            }
        }
    }

    fn synthesize(&mut self, expression: &Expression) -> Option<TypedExpression> {
        if let Some(typed) = self.checking.expressions.get(&expression.id).copied() {
            return Some(typed);
        }

        let typed = match &expression.kind {
            ExpressionKind::Literal(literal) => self.synthesize_literal(expression, *literal),
            ExpressionKind::TypeValue(_) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::TypeValueOutsideFactory,
                    span: expression.span,
                });
                self.recovery_temporary()
            }
            ExpressionKind::FormattedString { parts } => {
                self.synthesize_formatted_string(expression, parts)?
            }
            ExpressionKind::Identifier => self.synthesize_identifier(expression)?,
            ExpressionKind::SelfValue => self.synthesize_self(expression),
            ExpressionKind::Group(inner) => {
                let typed = self.synthesize(inner)?;
                if let Some(place) = self.checking.physical_places.get(&inner.id).cloned() {
                    self.checking.physical_places.insert(expression.id, place);
                }
                if let Some(link) = self
                    .checking
                    .tracked_lifetime_links
                    .get(&inner.id)
                    .cloned()
                {
                    self.checking
                        .tracked_lifetime_links
                        .insert(expression.id, link);
                }
                let explicitly_produces_value = self.explicitly_produces_value(inner);
                self.checking
                    .explicit_values
                    .insert(expression.id, explicitly_produces_value);
                typed
            }
            ExpressionKind::Tuple { elements } => {
                self.synthesize_tuple_literal(expression, elements, None)?
            }
            ExpressionKind::Block(block) => {
                let outcome = self.analyze_block(block, None, ConditionalUse::Value, false)?;
                if let Some(value) = block.value.as_deref() {
                    let sources = self.tracked_lifetime_sources(value);
                    self.record_tracked_lifetime_link(expression.id, sources);
                }
                self.checking
                    .explicit_values
                    .insert(expression.id, outcome.explicit_value.is_some());
                outcome.typed
            }
            ExpressionKind::If { .. } => {
                self.synthesize_conditional_expression(expression, ConditionalUse::Value)?
                    .typed
            }
            ExpressionKind::Loop { .. }
            | ExpressionKind::While { .. }
            | ExpressionKind::RangeFor { .. } => {
                self.analyze_loop_expression(expression, None)?
            }
            ExpressionKind::Lambda {
                parameters, body, ..
            } => self.synthesize_lambda(expression, parameters, body)?,
            ExpressionKind::GcAllocate(value) => self.synthesize_gc_allocation(value)?,
            ExpressionKind::StructConstruction { owner, fields } => {
                self.synthesize_named_struct_construction(expression, owner, fields)?
            }
            ExpressionKind::AnonymousStruct { members } => {
                self.synthesize_anonymous_struct(expression, members)?
            }
            ExpressionKind::MemberAccess { object, member } => {
                self.synthesize_member_access(expression, object, *member)?
            }
            ExpressionKind::AssociatedAccess { owner, member } => {
                self.synthesize_associated_access(expression, owner, *member)?
            }
            ExpressionKind::Index { object, index } => {
                self.synthesize_sequence_index(expression, object, index)?
            }
            ExpressionKind::Slice { object, start, end } => self.synthesize_sequence_slice(
                expression,
                object,
                start.as_deref(),
                end.as_deref(),
            )?,
            ExpressionKind::Try { expression: operand } => {
                self.synthesize_error_propagation(expression, operand)?
            }
            ExpressionKind::TypeAscription { value, type_syntax } => {
                self.synthesize_type_ascription(expression, value, type_syntax)?
            }
            ExpressionKind::TypeTest { value, type_syntax } => {
                self.synthesize_type_test(expression, value, type_syntax)?
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
                ExpressionKind::Identifier
                    | ExpressionKind::MemberAccess { .. }
                    | ExpressionKind::Index { .. }
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

    /// Checks or explicitly converts an expression under a source-written type
    /// and exposes exactly that type to its surrounding expression.
    ///
    /// For example, `file: Reader` creates a borrowed `Reader` view when the
    /// concrete file satisfies that interface. In
    /// `const selected: Reader | Writer = file: Reader`, the surrounding union
    /// therefore sees the unambiguous `Reader` member. Primitive ascriptions
    /// additionally support `float -> int`, `int -> float`, `int -> char`, and
    /// `char -> int`. No ascription performs a runtime downcast or implicit
    /// object copy.
    fn synthesize_type_ascription(
        &mut self,
        expression: &Expression,
        value: &Expression,
        type_syntax: &TypeSyntax,
    ) -> Option<TypedExpression> {
        let expected = self
            .resolved_type_syntax(type_syntax.id)
            .expect("ascribed source type must have been resolved");
        if self.primitive_kind(expected).is_some() {
            let found = self.synthesize(value)?;
            if self.is_recovery(found.type_id) || self.is_divergence(found.type_id) {
                let explicitly_produces_value = self.explicitly_produces_value(value);
                self.checking
                    .explicit_values
                    .insert(expression.id, explicitly_produces_value);
                return Some(found);
            }
            if let Some(conversion) =
                self.primitive_conversion(found.type_id, expected)
            {
                self.checking
                    .primitive_conversions
                    .insert(expression.id, conversion);
                match conversion {
                    PrimitiveConversion::FloatToInt => {
                        self.checking.primitive_conversion_runtime_checks.insert(
                            expression.id,
                            PrimitiveConversionRuntimeCheck::FiniteSignedIntRange,
                        );
                    }
                    PrimitiveConversion::IntToChar => {
                        self.checking.primitive_conversion_runtime_checks.insert(
                            expression.id,
                            PrimitiveConversionRuntimeCheck::AsciiRange,
                        );
                    }
                    PrimitiveConversion::IntToFloat | PrimitiveConversion::CharToInt => {}
                }
                let explicitly_produces_value = self.explicitly_produces_value(value);
                self.checking
                    .explicit_values
                    .insert(expression.id, explicitly_produces_value);
                return Some(TypedExpression {
                    type_id: expected,
                    category: ValueCategory::FreshTemporary,
                });
            }
        }

        let checked = self.check(value, expected)?;
        if let Some(place) = self.checking.physical_places.get(&value.id).cloned() {
            self.checking.physical_places.insert(expression.id, place);
        }
        let tracked_sources = self.tracked_lifetime_sources(value);
        self.record_tracked_lifetime_link(expression.id, tracked_sources);
        let explicitly_produces_value = self.explicitly_produces_value(value);
        self.checking
            .explicit_values
            .insert(expression.id, explicitly_produces_value);

        if self.is_recovery(checked.type_id) || self.is_divergence(checked.type_id) {
            return Some(checked);
        }
        Some(TypedExpression {
            type_id: expected,
            category: checked.category,
        })
    }

    /// Checks the value-facing portion of `value is Member` and returns bool.
    /// Branch-specific member selection is performed by boolean-flow analysis;
    /// synthesis alone still validates that the test names only exact members
    /// (or an exact member subset) of a real union.
    fn synthesize_type_test(
        &mut self,
        expression: &Expression,
        value: &Expression,
        type_syntax: &TypeSyntax,
    ) -> Option<TypedExpression> {
        let source = self.synthesize(value)?;
        if self.is_recovery(source.type_id) {
            return Some(self.recovery_temporary());
        }
        let place = self.narrowing_place(value);
        let source_union = place
            .as_ref()
            .and_then(|place| self.effective_narrowing(place))
            .map_or(source.type_id, |fact| fact.source_union);
        let Some(source_members) = self.union_members(source_union) else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidTypeTestSource {
                    found: source.type_id,
                },
                span: value.span,
            });
            return Some(self.recovery_temporary());
        };
        let tested = self
            .resolved_type_syntax(type_syntax.id)
            .expect("type-test syntax must have been resolved");
        let tested_members = self.union_members(tested).unwrap_or_else(|| vec![tested]);
        if tested_members.is_empty()
            || tested_members
                .iter()
                .any(|member| !source_members.contains(member))
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidTypeTestMember {
                    union: source_union,
                    tested,
                },
                span: type_syntax.span,
            });
            return Some(self.recovery_temporary());
        }
        let remaining: Vec<_> = source_members
            .iter()
            .copied()
            .filter(|member| !tested_members.contains(member))
            .collect();
        let true_type = self.narrowed_subset_type(source_union, tested_members);
        let false_type = (!remaining.is_empty())
            .then(|| self.narrowed_subset_type(source_union, remaining));
        self.checking
            .type_test_facts
            .insert(expression.id, (Some(true_type), false_type));
        Some(self.fresh_primitive(PrimitiveType::Bool))
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
        let borrow_containing_capture = captures
            .iter()
            .any(|capture| self.lambda_capture_contains_tracked_reference(*capture));
        if borrow_containing_capture {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::BorrowContainingLambdaCapture,
                span: expression.span,
            });
        }
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
        let enclosing_tracked_bindings =
            std::mem::take(&mut self.current_tracked_bindings);
        let enclosing_narrowings = std::mem::take(&mut self.current_narrowings);
        let enclosing_reachability = self.current_path_reachable;
        let enclosing_loops = std::mem::take(&mut self.active_loops);
        let enclosing_deferred_blocks = std::mem::take(&mut self.active_deferred_blocks);
        let enclosing_tracked_roots =
            std::mem::take(&mut self.current_tracked_return_roots);
        let enclosing_borrow_containing_roots =
            std::mem::take(&mut self.current_borrow_containing_return_roots);
        self.current_path_reachable = true;
        self.seed_callable_parameters(expression.id, parameters);
        for parameter in parameters {
            let FunctionParameterKind::Named { .. } = &parameter.kind else {
                continue;
            };
            let symbol = self
                .names
                .symbol_for_declaration(parameter.id)
                .expect("lambda parameter must have a semantic symbol");
            let type_id = self.checking.bindings[&symbol].type_id;
            if self.tracked_reference_parts(type_id).is_some() {
                self.current_tracked_return_roots
                    .insert(PhysicalPlaceRoot::Symbol(symbol));
            } else if self.type_contains_tracked_reference(type_id) {
                self.current_borrow_containing_return_roots
                    .insert(PhysicalPlaceRoot::Symbol(symbol));
            }
        }
        self.visit_callable_body(body, signature.return_type);
        self.release_all_narrowings(body.id, NarrowingEdgeKind::CallableCompletion);
        self.current_binding_categories = enclosing_categories;
        self.current_tracked_bindings = enclosing_tracked_bindings;
        self.current_narrowings = enclosing_narrowings;
        self.current_path_reachable = enclosing_reachability;
        self.active_loops = enclosing_loops;
        self.active_deferred_blocks = enclosing_deferred_blocks;
        self.current_tracked_return_roots = enclosing_tracked_roots;
        self.current_borrow_containing_return_roots = enclosing_borrow_containing_roots;
        if borrow_containing_capture || self.checking.errors.len() != first_body_error {
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

    /// Discovers the free sources needed for lambda capability and conservative
    /// borrow-containing capture rejection. Lowering remains responsible for
    /// the representation of captures which pass this safety boundary.
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

    fn lambda_capture_contains_tracked_reference(&self, capture: LambdaCapture) -> bool {
        match capture.source {
            LambdaCaptureSource::Symbol(symbol) => self
                .checking
                .bindings
                .get(&symbol)
                .is_some_and(|binding| self.type_contains_tracked_reference(binding.type_id)),
            LambdaCaptureSource::SelfValue { method } => {
                let receiver = self
                    .current_specialized_owner
                    .and_then(|owner| self.signatures.specialized_callable(owner, method))
                    .or_else(|| self.signatures.callable(method))
                    .and_then(|signature| signature.receiver)
                    .expect("captured self must have a receiver signature");
                receiver.storage == ReceiverStorage::Tracked
                    || (receiver.storage == ReceiverStorage::Plain
                        && self.current_specialized_owner.or_else(|| {
                            self.method_owners.get(&method).copied()
                        })
                            .is_some_and(|owner| self.type_contains_tracked_reference(owner)))
            }
        }
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
            ExpressionKind::Literal(_)
            | ExpressionKind::TypeValue(_)
            | ExpressionKind::AssociatedAccess { .. } => {}
            ExpressionKind::FormattedString { parts } => {
                for part in parts {
                    if let FormattedStringPart::Interpolation { value, .. } = part {
                        self.collect_captures_from_expression(lambda, value, captures, seen);
                    }
                }
            }
            ExpressionKind::Group(inner)
            | ExpressionKind::GcAllocate(inner)
            | ExpressionKind::MemberAccess { object: inner, .. }
            | ExpressionKind::Try { expression: inner }
            | ExpressionKind::TypeTest { value: inner, .. }
            | ExpressionKind::TypeAscription { value: inner, .. }
            | ExpressionKind::Unary { operand: inner, .. } => {
                self.collect_captures_from_expression(lambda, inner, captures, seen);
            }
            ExpressionKind::Tuple { elements } => {
                for element in elements {
                    self.collect_captures_from_expression(lambda, element, captures, seen);
                }
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
        if !self.checking.expressions.contains_key(&expression.id) {
            if let ExpressionKind::Call { callee, arguments } = &expression.kind
                && self.inferred_error_constructor_callee(callee)
            {
                let contextual_error = self.contextual_error_type(expected);
                let found = self.synthesize_inferred_error_call(
                    expression,
                    callee,
                    arguments,
                    contextual_error,
                )?;
                self.checking.expressions.insert(expression.id, found);
                self.checking.explicit_values.insert(expression.id, true);
                return self.check_typed(
                    expression,
                    expected,
                    found,
                    allow_recursive_copy,
                );
            }
            if let ExpressionKind::AssociatedAccess { owner, member } = &expression.kind
                && self.is_inferred_error_constructor(owner, *member)
                && let Some(typed) =
                    self.synthesize_contextual_error_constructor_value(expression, expected)
            {
                self.checking.expressions.insert(expression.id, typed);
                self.checking.explicit_values.insert(expression.id, true);
                return Some(typed);
            }
        }
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
            if let Some(value) = block.value.as_deref() {
                let sources = self.tracked_lifetime_sources(value);
                self.record_tracked_lifetime_link(expression.id, sources);
            }
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
        if matches!(
            &expression.kind,
            ExpressionKind::Loop { .. }
                | ExpressionKind::While { .. }
                | ExpressionKind::RangeFor { .. }
        ) && !self.checking.expressions.contains_key(&expression.id)
        {
            return self.analyze_loop_expression(expression, Some(expected));
        }
        if let ExpressionKind::Tuple { elements } = &expression.kind
            && !self.checking.expressions.contains_key(&expression.id)
        {
            let expected_elements = match self.types.types().get(expected).cloned() {
                Some(SemanticType::Tuple {
                    elements: expected_elements,
                    ..
                }) if elements.len() == expected_elements.len() =>
                {
                    Some(expected_elements)
                }
                _ => self.contextual_tuple_union_member(elements, expected)?,
            };
            if let Some(expected_elements) = expected_elements
                && elements.len() == expected_elements.len()
            {
                let found =
                    self.synthesize_tuple_literal(expression, elements, Some(&expected_elements))?;
                let typed =
                    self.check_typed(expression, expected, found, allow_recursive_copy)?;
                self.checking.expressions.insert(expression.id, typed);
                self.checking.explicit_values.insert(expression.id, true);
                return Some(typed);
            }
        }
        if let ExpressionKind::Group(inner) = &expression.kind
            && !self.checking.expressions.contains_key(&expression.id)
        {
            let typed = self.check_with_capability(inner, expected, allow_recursive_copy)?;
            if let Some(place) = self.checking.physical_places.get(&inner.id).cloned() {
                self.checking.physical_places.insert(expression.id, place);
            }
            if let Some(link) = self
                .checking
                .tracked_lifetime_links
                .get(&inner.id)
                .cloned()
            {
                self.checking
                    .tracked_lifetime_links
                    .insert(expression.id, link);
            }
            self.checking.expressions.insert(expression.id, typed);
            let explicitly_produces_value = self.explicitly_produces_value(inner);
            self.checking
                .explicit_values
                .insert(expression.id, explicitly_produces_value);
            return Some(typed);
        }

        let found = self.synthesize(expression)?;
        self.check_typed(expression, expected, found, allow_recursive_copy)
    }

    /// Selects one tuple member of an expected union before constructing a
    /// tuple literal. Elements are synthesized once, then compatibility is
    /// probed without diagnostics or metadata changes. A unique candidate is
    /// checked contextually by `synthesize_tuple_literal`; zero or multiple
    /// candidates fall back to ordinary inferred-tuple union classification.
    fn contextual_tuple_union_member(
        &mut self,
        elements: &[Expression],
        expected: TypeId,
    ) -> Option<Option<Vec<TypeId>>> {
        let members = match self.types.types().get(expected) {
            Some(SemanticType::Union { members, .. }) => members.clone(),
            _ => return Some(None),
        };
        let mut found_elements = Vec::with_capacity(elements.len());
        for element in elements {
            found_elements.push(self.synthesize(element)?);
        }
        if found_elements
            .iter()
            .any(|found| self.is_recovery(found.type_id))
        {
            return Some(None);
        }

        let mut candidates = members.into_iter().filter_map(|member| {
            let Some(SemanticType::Tuple {
                elements: expected_elements,
                ..
            }) = self.types.types().get(member)
            else {
                return None;
            };
            if expected_elements.len() != found_elements.len()
                || !found_elements
                    .iter()
                    .zip(expected_elements)
                    .all(|(found, expected)| {
                        self.classify_contextual_assignment(*found, *expected, false)
                            .is_ok()
                    })
            {
                return None;
            }
            Some(expected_elements.clone())
        });
        let candidate = candidates.next();
        if candidates.next().is_some() {
            Some(None)
        } else {
            Some(candidate)
        }
    }

    /// Constructs one tuple while evaluating and storing its elements in
    /// source order. Without an expected tuple, every element contributes its
    /// synthesized type. A matching expected tuple checks each position
    /// contextually, but this path applies only to literal construction: an
    /// existing tuple still crosses expected-type boundaries as one exact
    /// structural value and is never converted element by element.
    fn synthesize_tuple_literal(
        &mut self,
        expression: &Expression,
        elements: &[Expression],
        expected_elements: Option<&[TypeId]>,
    ) -> Option<TypedExpression> {
        debug_assert!(
            expected_elements.is_none_or(|expected| expected.len() == elements.len()),
            "contextual tuple construction requires matching arity"
        );
        let mut element_types = Vec::with_capacity(elements.len());
        let mut valid = true;
        let mut all_supported = true;
        let mut tracked_sources = Vec::new();

        for (index, element) in elements.iter().enumerate() {
            let checked = match expected_elements {
                Some(expected) => self.check(element, expected[index]),
                None => self.synthesize(element),
            };
            let Some(checked) = checked else {
                all_supported = false;
                continue;
            };
            let element_type =
                expected_elements.map_or(checked.type_id, |expected| expected[index]);
            element_types.push(element_type);
            if self.is_recovery(checked.type_id) {
                valid = false;
                continue;
            }
            valid &= self.validate_owning_transfer(element, checked, true);
            self.extend_tracked_lifetime_sources(&mut tracked_sources, element);
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
            .tuple(element_types, AccessCapability::Mut);
        self.record_tracked_lifetime_link(expression.id, tracked_sources);
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
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
        match self.classify_contextual_assignment(found, expected, allow_recursive_copy) {
            Ok(assignment) => {
                let assigned = self.apply_contextual_assignment(
                    expression.id,
                    expected,
                    found,
                    assignment,
                );
                if assigned != found {
                    self.checking.expressions.insert(expression.id, assigned);
                }
                Some(assigned)
            }
            Err(kind) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind,
                    span: expression.span,
                });
                Some(self.recover_expression(expression, found.category))
            }
        }
    }

    /// Classifies one expected-type boundary without mutating checker state.
    /// This separation lets destination-union matching probe every candidate
    /// before it commits to one unambiguous conversion and emits diagnostics.
    fn classify_contextual_assignment(
        &self,
        found: TypedExpression,
        expected: TypeId,
        allow_recursive_copy: bool,
    ) -> Result<ContextualAssignment, ExpressionCheckingErrorKind> {
        if self
            .types
            .types()
            .has_same_shape(found.type_id, expected)
            .expect("checked types must belong to the program type store")
        {
            return if self.value_capability_is_compatible(
                found,
                expected,
                allow_recursive_copy,
            ) {
                Ok(ContextualAssignment::Exact)
            } else {
                Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                })
            };
        }

        if let Some(result) =
            self.classify_error_widening(found, expected, allow_recursive_copy)
        {
            return result;
        }

        if let Some((target, destination_capability)) =
            self.tracked_reference_parts(expected)
        {
            let Some((source_target, source_capability)) =
                self.tracked_borrow_source_parts(found.type_id)
            else {
                return Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                });
            };
            let same_target = self
                .types
                .types()
                .has_same_shape(source_target, target)
                .expect("tracked borrow types belong to the program type store");
            let capability_valid = !matches!(
                (source_capability, destination_capability),
                (AccessCapability::Const, AccessCapability::Mut)
            );
            if same_target && capability_valid {
                return Ok(ContextualAssignment::TrackedBorrow {
                    source_type: found.type_id,
                    target_type: target,
                });
            }
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }

        if let Some(destination_members) = self.union_members(expected) {
            if !self.value_capability_is_compatible(found, expected, allow_recursive_copy) {
                return Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                });
            }
            return self.classify_destination_union(
                found,
                expected,
                destination_members,
                allow_recursive_copy,
            );
        }

        // An existing tracked reference cannot be converted to its plain
        // target or to an interface view. It may still be injected unchanged
        // into a destination union containing that exact tracked type. Union
        // classification above probes its members through
        // `classify_non_union_assignment`, whose exact-type path preserves
        // this restriction while admitting the matching member.
        if self
            .types
            .types()
            .get(found.type_id)
            .is_some_and(|semantic| {
                semantic.storage_semantics() == Some(StorageSemantics::TrackedReference)
            })
        {
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }

        if let Some(source_members) = self.union_members(found.type_id) {
            if self.interface_destination(expected).is_none() {
                return Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                });
            }
            for source_member in source_members {
                let source = TypedExpression {
                    type_id: source_member,
                    category: self.union_member_category(found.category, source_member),
                };
                self.validate_interface_view_source(
                    source,
                    expected,
                    self.union_member_capability(found.type_id, source_member),
                )?;
            }
            return Ok(ContextualAssignment::InterfaceView(InterfaceView {
                source_type: found.type_id,
                source_category: found.category,
                destination_type: expected,
            }));
        }

        self.classify_non_union_assignment(found, expected, allow_recursive_copy, None)
    }

    fn classify_error_widening(
        &self,
        found: TypedExpression,
        expected: TypeId,
        allow_recursive_copy: bool,
    ) -> Option<Result<ContextualAssignment, ExpressionCheckingErrorKind>> {
        let Some(SemanticType::Builtin {
            builtin: BuiltinType::Error,
            arguments: source_arguments,
            ..
        }) = self.types.types().get(found.type_id)
        else {
            return None;
        };
        let Some(SemanticType::Builtin {
            builtin: BuiltinType::Error,
            arguments: destination_arguments,
            ..
        }) = self.types.types().get(expected)
        else {
            return None;
        };
        let (&[source_payload], &[destination_payload]) =
            (source_arguments.as_slice(), destination_arguments.as_slice())
        else {
            return None;
        };
        if !self.value_capability_is_compatible(found, expected, allow_recursive_copy) {
            return Some(Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            }));
        }
        let payload_category = if self
            .types
            .types()
            .get(source_payload)
            .is_some_and(|semantic| semantic.storage_semantics() == Some(StorageSemantics::Gc))
        {
            ValueCategory::GcReference
        } else {
            found.category
        };
        let payload = TypedExpression {
            type_id: source_payload,
            category: payload_category,
        };
        Some(
            self.classify_contextual_assignment(
                payload,
                destination_payload,
                allow_recursive_copy,
            )
            .map(|payload_assignment| {
                ContextualAssignment::ErrorWidening(ErrorWidening {
                    source_error: found.type_id,
                    destination_error: expected,
                    source_payload,
                    destination_payload,
                    payload_assignment: Box::new(payload_assignment),
                })
            }),
        )
    }

    /// Widens a union only when its canonical member set is a subset of the
    /// destination. Otherwise the complete source may still form one
    /// unambiguous interface view which is then injected as a single member;
    /// that separate composition never changes members during widening.
    fn classify_destination_union(
        &self,
        found: TypedExpression,
        expected: TypeId,
        destination_members: Vec<TypeId>,
        allow_recursive_copy: bool,
    ) -> Result<ContextualAssignment, ExpressionCheckingErrorKind> {
        if let Some(source_members) = self.union_members(found.type_id)
            && source_members
                .iter()
                .all(|source| destination_members.contains(source))
        {
            return Ok(ContextualAssignment::UnionWidening(UnionWidening {
                source_union: found.type_id,
                destination_union: expected,
            }));
        }

        let mut candidates = Vec::new();
        for destination_member in destination_members {
            let assignment = if self.union_members(found.type_id).is_some() {
                self.classify_contextual_assignment(
                    found,
                    destination_member,
                    allow_recursive_copy,
                )
            } else {
                self.classify_non_union_assignment(
                    found,
                    destination_member,
                    allow_recursive_copy,
                    None,
                )
            };
            if let Ok(assignment) = assignment {
                candidates.push((destination_member, assignment));
            }
        }
        if candidates.len() > 1 {
            return Err(ExpressionCheckingErrorKind::AmbiguousUnionConversion {
                source: found.type_id,
                destination: expected,
            });
        }
        let Some((member_type, assignment)) = candidates.pop() else {
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        };
        let (interface_view, tracked_borrow, error_widening) = match assignment {
            ContextualAssignment::Exact => (None, None, None),
            ContextualAssignment::InterfaceView(view) => (Some(view), None, None),
            ContextualAssignment::TrackedBorrow {
                source_type,
                target_type,
            } => (None, Some((source_type, target_type)), None),
            ContextualAssignment::UnionInjection { .. }
            | ContextualAssignment::UnionWidening(_) => {
                unreachable!("a normalized union member is not a destination union")
            }
            ContextualAssignment::ErrorWidening(widening) => (None, None, Some(widening)),
        };
        Ok(ContextualAssignment::UnionInjection {
            member_type,
            interface_view,
            tracked_borrow,
            error_widening,
        })
    }

    fn classify_non_union_assignment(
        &self,
        found: TypedExpression,
        expected: TypeId,
        allow_recursive_copy: bool,
        source_capability: Option<AccessCapability>,
    ) -> Result<ContextualAssignment, ExpressionCheckingErrorKind> {
        if self
            .types
            .types()
            .has_same_shape(found.type_id, expected)
            .expect("assignability candidates belong to the program type store")
        {
            return if self.value_capability_is_compatible(
                found,
                expected,
                allow_recursive_copy,
            ) {
                Ok(ContextualAssignment::Exact)
            } else {
                Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                })
            };
        }

        if let Some(result) =
            self.classify_error_widening(found, expected, allow_recursive_copy)
        {
            return result;
        }

        if let Some((target, destination_capability)) =
            self.tracked_reference_parts(expected)
        {
            let Some((source_target, source_capability)) =
                self.tracked_borrow_source_parts(found.type_id)
            else {
                return Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                });
            };
            if self
                .types
                .types()
                .has_same_shape(source_target, target)
                == Some(true)
                && !matches!(
                    (source_capability, destination_capability),
                    (AccessCapability::Const, AccessCapability::Mut)
                )
            {
                return Ok(ContextualAssignment::TrackedBorrow {
                    source_type: found.type_id,
                    target_type: target,
                });
            }
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }

        if self
            .types
            .types()
            .get(found.type_id)
            .is_some_and(|semantic| {
                semantic.storage_semantics() == Some(StorageSemantics::TrackedReference)
            })
        {
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }

        self.validate_interface_view_source(found, expected, source_capability)?;
        Ok(ContextualAssignment::InterfaceView(InterfaceView {
            source_type: found.type_id,
            source_category: found.category,
            destination_type: expected,
        }))
    }

    /// Forms an interface view over existing storage. Concrete structs expose
    /// their method dictionary, while interface and intersection sources prove
    /// compatibility from the methods they already guarantee. Neither case
    /// copies or changes the concrete object.
    fn validate_interface_view_source(
        &self,
        found: TypedExpression,
        expected: TypeId,
        source_capability: Option<AccessCapability>,
    ) -> Result<(), ExpressionCheckingErrorKind> {
        if self
            .types
            .types()
            .get(found.type_id)
            .is_some_and(|semantic| {
                semantic.storage_semantics() == Some(StorageSemantics::TrackedReference)
            })
        {
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }
        let (interface_type, destination_capability, destination_is_gc) = self
            .interface_destination(expected)
            .ok_or(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            })?;
        let requirements = self.interface_requirements(interface_type).map_err(
            |(first, second)| ExpressionCheckingErrorKind::ConflictingInterfaceRequirement {
                first,
                second,
            },
        )?;

        let concrete = self.aggregate_parts(found.type_id);
        let erased = self.interface_source(found.type_id);
        let (declared_source_capability, source_is_gc) = match (concrete, erased) {
            (Some((_, capability, is_gc)), _) => (capability, is_gc),
            (_, Some((_, capability, is_gc))) => (capability, is_gc),
            _ => {
                return Err(ExpressionCheckingErrorKind::TypeMismatch {
                    expected,
                    found: found.type_id,
                });
            }
        };
        let requires_gc_receiver = requirements.iter().any(|required| {
            self.signatures
                .method_signature(required.requirement.method_id)
                .is_some_and(|signature| signature.receiver.storage == ReceiverStorage::Gc)
        });
        if (destination_is_gc || requires_gc_receiver) && !source_is_gc {
            return Err(ExpressionCheckingErrorKind::InterfaceRequiresGcSource);
        }
        if source_capability.unwrap_or(declared_source_capability) == AccessCapability::Const
            && destination_capability == AccessCapability::Mut
            && found.category != ValueCategory::FreshTemporary
        {
            return Err(ExpressionCheckingErrorKind::TypeMismatch {
                expected,
                found: found.type_id,
            });
        }

        if let Some((owner, _, _)) = concrete {
            self.match_concrete_interface_methods(owner, &requirements)?
        } else {
            let (source_interface, _, _) =
                erased.expect("an erased interface source was classified above");
            self.match_erased_interface_methods(source_interface, &requirements)?
        };
        Ok(())
    }

    fn match_concrete_interface_methods(
        &self,
        owner: AggregateOwner,
        requirements: &[RequiredInterfaceMethod],
    ) -> Result<(), ExpressionCheckingErrorKind> {
        let signature = self
            .aggregate_signature(owner)
            .expect("concrete interface source must have a struct signature");
        for required in requirements {
            let implementation = signature.member(&required.name).copied().ok_or(
                ExpressionCheckingErrorKind::MissingInterfaceMethod {
                    declaration: required.requirement.declaration,
                },
            )?;
            let (implementation_declaration, implementation_method) = match implementation.kind {
                StructMemberSignatureKind::Method {
                    declaration,
                    method_id,
                } => (declaration, Some(method_id)),
                StructMemberSignatureKind::Field(field) => (field.declaration, None),
                StructMemberSignatureKind::AssociatedFunction { declaration }
                | StructMemberSignatureKind::AssociatedTypeFactory { declaration } => {
                    (declaration, None)
                }
            };
            if implementation_method != Some(required.requirement.method_id) {
                return Err(ExpressionCheckingErrorKind::IncompatibleInterfaceMethod {
                    requirement: required.requirement.declaration,
                    implementation: implementation_declaration,
                });
            }
        }
        Ok(())
    }

    fn match_erased_interface_methods(
        &self,
        source: TypeId,
        requirements: &[RequiredInterfaceMethod],
    ) -> Result<(), ExpressionCheckingErrorKind> {
        let guaranteed = self.interface_requirements(source).map_err(|(first, second)| {
            ExpressionCheckingErrorKind::ConflictingInterfaceRequirement { first, second }
        })?;
        let by_name: HashMap<_, _> = guaranteed
            .iter()
            .map(|required| (required.name.as_str(), required.requirement))
            .collect();
        for required in requirements {
            let implementation = by_name.get(required.name.as_str()).copied().ok_or(
                ExpressionCheckingErrorKind::MissingInterfaceMethod {
                    declaration: required.requirement.declaration,
                },
            )?;
            if implementation.method_id != required.requirement.method_id {
                return Err(ExpressionCheckingErrorKind::IncompatibleInterfaceMethod {
                    requirement: required.requirement.declaration,
                    implementation: implementation.declaration,
                });
            }
        }
        Ok(())
    }

    fn interface_source(&self, type_id: TypeId) -> Option<(TypeId, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Interface { capability, .. }
            | SemanticType::Intersection { capability, .. } => {
                Some((type_id, *capability, false))
            }
            SemanticType::Gc { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::Interface { .. } | SemanticType::Intersection { .. })
                ) => Some((*target, *capability, true)),
            SemanticType::TemplateParameter { capability, .. } => self
                .template_parameter_bound(type_id)
                .flatten()
                .map(|bound| (bound, *capability, false)),
            SemanticType::Gc { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::TemplateParameter { .. })
                ) => self
                    .template_parameter_bound(*target)
                    .flatten()
                    .map(|bound| (bound, *capability, true)),
            _ => None,
        }
    }

    fn union_members(&self, type_id: TypeId) -> Option<Vec<TypeId>> {
        match self.types.types().get(type_id)? {
            SemanticType::Union { members, .. } => Some(members.clone()),
            _ => None,
        }
    }

    fn narrowing_place(&self, expression: &Expression) -> Option<NarrowingPlace> {
        match &expression.kind {
            ExpressionKind::Identifier => Some(NarrowingPlace {
                root: NarrowingRoot::Symbol(
                    self.names
                        .symbol_for_reference(expression.id)
                        .expect("narrowed identifier must have a resolved symbol"),
                ),
                fields: Vec::new(),
            }),
            ExpressionKind::SelfValue => Some(NarrowingPlace {
                root: NarrowingRoot::SelfValue(
                    self.context
                        .method_for_self(expression.id)
                        .expect("narrowed self must have a resolved method"),
                ),
                fields: Vec::new(),
            }),
            ExpressionKind::Group(inner) => self.narrowing_place(inner),
            ExpressionKind::MemberAccess { object, .. } => {
                let mut place = self.narrowing_place(object)?;
                let ResolvedMember::Field { declaration } =
                    self.checking.resolved_members.get(&expression.id).copied()?
                else {
                    return None;
                };
                place.fields.push(declaration);
                Some(place)
            }
            _ => None,
        }
    }

    fn effective_narrowing(&self, place: &NarrowingPlace) -> Option<NarrowingFact> {
        self.current_narrowings
            .get(place)
            .and_then(|facts| facts.last())
            .copied()
    }

    /// Converts one narrowing state into another and records the exact counter
    /// updates required on that control-flow edge. Common stack prefixes are
    /// retained, which prevents an inner identical test from releasing its
    /// enclosing lock and prevents joins from double-acquiring a shared fact.
    fn record_narrowing_transition(
        &mut self,
        source: NodeId,
        kind: NarrowingEdgeKind,
        from: &NarrowingState,
        to: &NarrowingState,
    ) {
        if !self.current_path_reachable {
            return;
        }
        let mut places: Vec<_> = from.keys().chain(to.keys()).cloned().collect();
        places.sort_by_key(|place| format!("{place:?}"));
        places.dedup();
        let mut operations = Vec::new();
        for place in places {
            let old = from.get(&place).map(Vec::as_slice).unwrap_or(&[]);
            let new = to.get(&place).map(Vec::as_slice).unwrap_or(&[]);
            let common = old
                .iter()
                .zip(new)
                .take_while(|(left, right)| left == right)
                .count();
            for fact in old[common..].iter().rev() {
                operations.push(NarrowingLockOperation {
                    place: place.clone(),
                    narrowed_type: fact.narrowed_type,
                    kind: NarrowingLockKind::Release,
                });
            }
            for fact in &new[common..] {
                operations.push(NarrowingLockOperation {
                    place: place.clone(),
                    narrowed_type: fact.narrowed_type,
                    kind: NarrowingLockKind::Acquire,
                });
            }
        }
        if !operations.is_empty() {
            self.checking.narrowing_edges.push(NarrowingEdge {
                source,
                kind,
                from: from.clone(),
                to: to.clone(),
                operations,
            });
        }
    }

    fn release_all_narrowings(&mut self, source: NodeId, kind: NarrowingEdgeKind) {
        if self.current_narrowings.is_empty() {
            return;
        }
        let from = std::mem::take(&mut self.current_narrowings);
        self.record_narrowing_transition(source, kind, &from, &HashMap::new());
    }

    fn transition_current_narrowings(
        &mut self,
        source: NodeId,
        kind: NarrowingEdgeKind,
        destination: NarrowingState,
    ) {
        let from = std::mem::replace(&mut self.current_narrowings, destination.clone());
        self.record_narrowing_transition(source, kind, &from, &destination);
    }

    fn invalidate_place_narrowings(&mut self, source: NodeId, place: &NarrowingPlace) {
        let from = self.current_narrowings.clone();
        self.current_narrowings.retain(|candidate, _| {
            candidate.root != place.root
                || candidate.fields.len() < place.fields.len()
                || candidate.fields[..place.fields.len()] != place.fields
        });
        let to = self.current_narrowings.clone();
        self.record_narrowing_transition(source, NarrowingEdgeKind::Invalidate, &from, &to);
    }

    fn assignment_preserves_narrowing(
        &self,
        place: &NarrowingPlace,
        value: NodeId,
    ) -> bool {
        let Some(fact) = self.effective_narrowing(place) else {
            return true;
        };
        let narrowed_members = self
            .union_members(fact.narrowed_type)
            .unwrap_or_else(|| vec![fact.narrowed_type]);
        if narrowed_members.len() != 1 {
            return false;
        }
        self.checking
            .union_injections
            .get(&value)
            .is_some_and(|injection| {
                // The narrowed expression inherits the union place's outer
                // access capability, while an injection identifies the
                // canonical member stored in the union. `mut int` and `int`
                // therefore denote the same runtime tag here even though their
                // complete TypeIds differ.
                self.types
                    .types()
                    .has_same_shape(injection.member_type, narrowed_members[0])
                    .expect("assignment member types belong to the program type store")
            })
    }

    fn merge_narrowing_states(&mut self, states: &[&NarrowingState]) -> NarrowingState {
        let Some(first) = states.first() else {
            return HashMap::new();
        };
        let mut places: Vec<_> = states
            .iter()
            .flat_map(|state| state.keys().cloned())
            .collect();
        places.sort_by_key(|place| format!("{place:?}"));
        places.dedup();
        let mut merged = HashMap::new();
        for place in places {
            let first_facts = first.get(&place).map(Vec::as_slice).unwrap_or(&[]);
            let common_length = states[1..].iter().fold(first_facts.len(), |length, state| {
                let other = state.get(&place).map(Vec::as_slice).unwrap_or(&[]);
                first_facts[..length]
                    .iter()
                    .zip(other)
                    .take_while(|(left, right)| left == right)
                    .count()
            });
            let mut facts = first_facts[..common_length].to_vec();
            let alternatives: Option<Vec<_>> = states
                .iter()
                .map(|state| state.get(&place).and_then(|facts| facts.last()).copied())
                .collect();
            if let Some(alternatives) = alternatives {
                let source_union = alternatives[0].source_union;
                if alternatives
                    .iter()
                    .all(|fact| fact.source_union == source_union)
                {
                    let mut members = Vec::new();
                    for fact in &alternatives {
                        for member in self
                            .union_members(fact.narrowed_type)
                            .unwrap_or_else(|| vec![fact.narrowed_type])
                        {
                            if !members.contains(&member) {
                                members.push(member);
                            }
                        }
                    }
                    let narrowed_type = self.narrowed_subset_type(source_union, members);
                    let common_effective = facts.last().map(|fact| fact.narrowed_type);
                    if narrowed_type != source_union && common_effective != Some(narrowed_type) {
                        facts.push(NarrowingFact {
                            source_union,
                            narrowed_type,
                        });
                    }
                }
            }
            if !facts.is_empty() {
                merged.insert(place, facts);
            }
        }
        merged
    }

    fn narrowed_subset_type(&mut self, source_union: TypeId, members: Vec<TypeId>) -> TypeId {
        let capability = self
            .types
            .types()
            .get(source_union)
            .and_then(SemanticType::capability)
            .expect("narrowed union has an outer capability");
        self.types.types_mut().union(members, capability)
    }

    fn union_member_category(
        &self,
        union_category: ValueCategory,
        member: TypeId,
    ) -> ValueCategory {
        if self
            .types
            .types()
            .get(member)
            .is_some_and(|semantic| semantic.storage_semantics() == Some(StorageSemantics::Gc))
        {
            ValueCategory::GcReference
        } else {
            union_category
        }
    }

    /// Inline alternatives inherit access through the union container. GC and
    /// erased-view alternatives retain their own nested target capability, so
    /// mutable access to the union cannot recover mutable access to a const
    /// reference stored inside it.
    fn union_member_capability(
        &self,
        union: TypeId,
        member: TypeId,
    ) -> Option<AccessCapability> {
        let member_semantic = self.types.types().get(member)?;
        if matches!(
            member_semantic.storage_semantics(),
            Some(
                StorageSemantics::Gc
                    | StorageSemantics::BorrowedView
                    | StorageSemantics::TrackedReference
            )
        ) {
            return None;
        }
        self.types
            .types()
            .get(union)
            .and_then(|semantic| semantic.capability())
    }

    fn apply_contextual_assignment(
        &mut self,
        node: NodeId,
        expected: TypeId,
        found: TypedExpression,
        assignment: ContextualAssignment,
    ) -> TypedExpression {
        match assignment {
            ContextualAssignment::Exact => found,
            ContextualAssignment::TrackedBorrow {
                source_type,
                target_type,
            } => {
                self.record_tracked_borrow(node, found, source_type, target_type);
                TypedExpression {
                    type_id: expected,
                    category: ValueCategory::BorrowedPlace,
                }
            }
            ContextualAssignment::InterfaceView(view) => {
                let destination_is_gc = self
                    .interface_destination(expected)
                    .is_some_and(|(_, _, is_gc)| is_gc);
                self.checking.interface_views.insert(node, view);
                TypedExpression {
                    type_id: expected,
                    category: if destination_is_gc {
                        ValueCategory::GcReference
                    } else {
                        ValueCategory::BorrowedPlace
                    },
                }
            }
            ContextualAssignment::UnionInjection {
                member_type,
                interface_view,
                tracked_borrow,
                error_widening,
            } => {
                let borrowed_view = interface_view.is_some()
                    || error_widening.as_ref().is_some_and(|widening| {
                        self.contextual_assignment_borrows(
                            &widening.payload_assignment,
                            found,
                        )
                    })
                    || (found.category == ValueCategory::BorrowedPlace
                        && self
                            .types
                            .types()
                            .get(found.type_id)
                            .is_some_and(|semantic| {
                                semantic.copy_semantics()
                                    == Some(CopySemantics::NonEscapingErasedView)
                            }));
                if let Some(view) = interface_view {
                    self.checking.interface_views.insert(node, view);
                }
                if let Some((source_type, target_type)) = tracked_borrow {
                    self.record_tracked_borrow(node, found, source_type, target_type);
                }
                if let Some(widening) = error_widening {
                    self.checking.error_widenings.insert(node, widening);
                }
                self.checking.union_injections.insert(
                    node,
                    UnionInjection {
                        member_type,
                        union_type: expected,
                    },
                );
                TypedExpression {
                    type_id: expected,
                    category: if borrowed_view {
                        ValueCategory::BorrowedPlace
                    } else {
                        ValueCategory::FreshTemporary
                    },
                }
            }
            ContextualAssignment::UnionWidening(widening) => {
                let borrowed_payload = found.category != ValueCategory::FreshTemporary
                    && self
                        .union_members(widening.source_union)
                        .expect("union widening starts from a union")
                        .iter()
                        .any(|member| {
                            self.types.types().get(*member).is_some_and(|semantic| {
                                semantic.storage_semantics() != Some(StorageSemantics::Gc)
                                    && matches!(
                                        semantic.copy_semantics(),
                                        Some(
                                            CopySemantics::Recursive
                                                | CopySemantics::NonEscapingErasedView
                                        )
                                    )
                            })
                        });
                self.checking.union_widenings.insert(node, widening);
                TypedExpression {
                    type_id: expected,
                    category: if borrowed_payload {
                        ValueCategory::BorrowedPlace
                    } else {
                        ValueCategory::FreshTemporary
                    },
                }
            }
            ContextualAssignment::ErrorWidening(widening) => {
                let borrowed_payload = self.contextual_assignment_borrows(
                    &widening.payload_assignment,
                    found,
                );
                self.checking.error_widenings.insert(node, widening);
                TypedExpression {
                    type_id: expected,
                    category: if borrowed_payload {
                        ValueCategory::BorrowedPlace
                    } else {
                        ValueCategory::FreshTemporary
                    },
                }
            }
        }
    }

    fn contextual_assignment_borrows(
        &self,
        assignment: &ContextualAssignment,
        found: TypedExpression,
    ) -> bool {
        match assignment {
            ContextualAssignment::TrackedBorrow { .. }
            | ContextualAssignment::InterfaceView(_) => true,
            ContextualAssignment::UnionInjection {
                interface_view,
                tracked_borrow,
                error_widening,
                ..
            } => {
                interface_view.is_some()
                    || tracked_borrow.is_some()
                    || error_widening.as_ref().is_some_and(|widening| {
                        self.contextual_assignment_borrows(
                            &widening.payload_assignment,
                            found,
                        )
                    })
            }
            ContextualAssignment::ErrorWidening(widening) => self
                .contextual_assignment_borrows(&widening.payload_assignment, found),
            ContextualAssignment::UnionWidening(widening) => {
                found.category != ValueCategory::FreshTemporary
                    && self
                        .union_members(widening.source_union)
                        .expect("union widening starts from a union")
                        .iter()
                        .any(|member| {
                            self.types.types().get(*member).is_some_and(|semantic| {
                                semantic.storage_semantics() != Some(StorageSemantics::Gc)
                                    && matches!(
                                        semantic.copy_semantics(),
                                        Some(
                                            CopySemantics::Recursive
                                                | CopySemantics::NonEscapingErasedView
                                        )
                                    )
                            })
                        })
            }
            ContextualAssignment::Exact => false,
        }
    }

    fn record_tracked_borrow(
        &mut self,
        node: NodeId,
        found: TypedExpression,
        source_type: TypeId,
        target_type: TypeId,
    ) {
        let source = self
            .checking
            .physical_places
            .get(&node)
            .cloned()
            .unwrap_or(PhysicalPlace {
                root: PhysicalPlaceRoot::Expression(node),
                projections: Vec::new(),
                storage: found.category,
            });
        self.checking.tracked_borrows.insert(
            node,
            TrackedBorrow {
                source: source.clone(),
                source_type,
                target_type,
            },
        );
        self.checking.tracked_lifetime_links.insert(
            node,
            TrackedLifetimeLink {
                sources: vec![source],
            },
        );
    }

    /// Peels plain, GC-qualified, or tracked interfaces while preserving the
    /// access capability enforced at conversion or member-access boundaries.
    fn interface_destination(&self, type_id: TypeId) -> Option<(TypeId, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Interface { capability, .. }
            | SemanticType::Intersection { capability, .. } => Some((type_id, *capability, false)),
            SemanticType::Gc { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::Interface { .. } | SemanticType::Intersection { .. })
                ) =>
            {
                Some((*target, *capability, true))
            }
            SemanticType::Tracked { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::Interface { .. } | SemanticType::Intersection { .. })
                ) => Some((*target, *capability, false)),
            SemanticType::TemplateParameter { capability, .. } => self
                .template_parameter_bound(type_id)
                .flatten()
                .map(|bound| (bound, *capability, false)),
            SemanticType::Gc { target, capability }
                if matches!(
                    self.types.types().get(*target),
                    Some(SemanticType::TemplateParameter { .. })
                ) => self
                    .template_parameter_bound(*target)
                    .flatten()
                    .map(|bound| (bound, *capability, true)),
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
                (found.category == ValueCategory::FreshTemporary
                    && !matches!(
                        found_semantic.storage_semantics(),
                        Some(
                            StorageSemantics::Gc
                                | StorageSemantics::BorrowedView
                                | StorageSemantics::TrackedReference
                        )
                    ))
                    || found_semantic.copy_semantics() == Some(CopySemantics::Trivial)
                    || (allow_recursive_copy
                        && found_semantic.copy_semantics() == Some(CopySemantics::Recursive))
            }
            _ => true,
        }
    }

    fn tracked_reference_parts(&self, type_id: TypeId) -> Option<(TypeId, AccessCapability)> {
        match self.types.types().get(type_id)? {
            SemanticType::Tracked { target, capability } => Some((*target, *capability)),
            _ => None,
        }
    }

    /// Plain values and GC references can both provide storage for a tracked
    /// borrow. Existing tracked references are handled by exact assignment and
    /// are deliberately not unwrapped into another storage conversion.
    fn tracked_borrow_source_parts(
        &self,
        type_id: TypeId,
    ) -> Option<(TypeId, AccessCapability)> {
        match self.types.types().get(type_id)? {
            SemanticType::Gc { target, capability } => Some((*target, *capability)),
            semantic if semantic.storage_semantics() == Some(StorageSemantics::Inline) => {
                Some((type_id, semantic.capability()?))
            }
            _ => None,
        }
    }

    fn callable_capability(&self, type_id: TypeId) -> Option<AccessCapability> {
        match self.types.types().get(type_id)? {
            SemanticType::Callable { capability, .. } => Some(*capability),
            SemanticType::Gc { target, .. } => {
                match self.types.types().get(*target)? {
                    SemanticType::Callable { capability, .. } => Some(*capability),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn primitive_conversion(
        &self,
        source: TypeId,
        destination: TypeId,
    ) -> Option<PrimitiveConversion> {
        match (
            self.primitive_kind(source)?,
            self.primitive_kind(destination)?,
        ) {
            (PrimitiveType::Float, PrimitiveType::Int) => {
                Some(PrimitiveConversion::FloatToInt)
            }
            (PrimitiveType::Int, PrimitiveType::Float) => {
                Some(PrimitiveConversion::IntToFloat)
            }
            (PrimitiveType::Int, PrimitiveType::Char) => {
                Some(PrimitiveConversion::IntToChar)
            }
            (PrimitiveType::Char, PrimitiveType::Int) => {
                Some(PrimitiveConversion::CharToInt)
            }
            _ => None,
        }
    }

    /// Types prefix GC allocation and records how its operand enters GC storage.
    ///
    /// Fresh temporaries are moved into a new allocation, while applying `&`
    /// to an existing GC reference copies that reference to the same allocation.
    /// Plain places are rejected because allocation cannot change the storage
    /// identity of an existing value.
    fn synthesize_gc_allocation(&mut self, value: &Expression) -> Option<TypedExpression> {
        let source = self.synthesize(value)?;
        let semantic = self
            .types
            .types()
            .get(source.type_id)
            .expect("allocated value type belongs to the program type store");
        if matches!(semantic, SemanticType::Recovery | SemanticType::Divergence) {
            return Some(source);
        }
        if self.type_contains_tracked_reference(source.type_id) {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::BorrowContainingGcStorage {
                    found: source.type_id,
                },
                span: value.span,
            });
            return Some(self.recovery_temporary());
        }
        if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            self.checking
                .transfers
                .insert(value.id, ValueTransfer::CopyGcReference);
            return Some(TypedExpression {
                type_id: source.type_id,
                category: ValueCategory::GcReference,
            });
        }
        if matches!(semantic, SemanticType::Tuple { .. })
            && self.contains_non_escaping_erased_view(source.type_id)
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidGcAllocationSource {
                    found: source.type_id,
                    category: source.category,
                },
                span: value.span,
            });
            return Some(self.recovery_temporary());
        }
        if source.category != ValueCategory::FreshTemporary {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidGcAllocationSource {
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
            .gc(source.type_id)
            .expect("fresh value must have GC-qualifiable storage");
        self.checking
            .transfers
            .insert(value.id, ValueTransfer::AllocateGc);
        Some(TypedExpression {
            type_id,
            category: ValueCategory::GcReference,
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
    /// have their references copied. Named plain values therefore require an
    /// explicit `.copy()`.
    fn synthesize_named_struct_construction(
        &mut self,
        expression: &Expression,
        owner: &TypeSyntax,
        fields: &[StructFieldInitializer],
    ) -> Option<TypedExpression> {
        let Some(owner_type) = self.resolved_type_syntax(owner.id) else {
            return None;
        };
        let Some((aggregate_owner, _, _)) = self.aggregate_parts(owner_type) else {
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
            .aggregate_signature(aggregate_owner)
            .expect("constructible struct signature must have been collected")
            .clone();

        let mut seen = HashSet::new();
        let mut valid = true;
        let mut all_supported = true;
        let mut tracked_sources = Vec::new();
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
            self.extend_tracked_lifetime_sources(&mut tracked_sources, &field.value);
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
        self.record_tracked_lifetime_link(expression.id, tracked_sources);
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
        let mut tracked_sources = Vec::new();

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
                self.extend_tracked_lifetime_sources(
                    &mut tracked_sources,
                    &field.initializer,
                );
            }
        }

        if !self.aggregate_layouts.contains_key(&signature.type_id) {
            self.aggregate_order.push(signature.type_id);
        }
        self.aggregate_layouts.insert(
            signature.type_id,
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
        self.record_tracked_lifetime_link(expression.id, tracked_sources);
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
        if let Some(namespace) = self.builtin_namespace_reference(object) {
            return Some(self.synthesize_namespace_member_access(
                expression,
                namespace,
                member,
            ));
        }
        let typed_object = self.synthesize(object)?;
        if self.is_recovery(typed_object.type_id) {
            return Some(self.recovery_temporary());
        }
        let member_text = self
            .module
            .text(member)
            .expect("member name belongs to the source module")
            .to_string();
        if self.queue_parts(typed_object.type_id).is_some() {
            let selected = self.signatures.builtins().member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Queue),
                &member_text,
            );
            self.checking.errors.push(ExpressionCheckingError {
                kind: match selected {
                    Some(BuiltinMemberSignature::Callable(template))
                        if template.receiver.is_some() =>
                    {
                        ExpressionCheckingErrorKind::MethodRequiresCall
                    }
                    Some(BuiltinMemberSignature::Callable(_)) => {
                        ExpressionCheckingErrorKind::AssociatedFunctionRequiresType
                    }
                    Some(BuiltinMemberSignature::Field(_)) | None => {
                        ExpressionCheckingErrorKind::UnknownMember
                    }
                },
                span: member,
            });
            return Some(self.recovery_temporary());
        }
        if let Some((builtin, arguments, _)) =
            self.parameterized_builtin_parts(typed_object.type_id)
        {
            if builtin == BuiltinType::Error {
                return Some(self.synthesize_error_value_access(
                    expression,
                    object,
                    typed_object,
                    member,
                    &member_text,
                    &arguments,
                ));
            }
            let selected = self
                .signatures
                .builtins()
                .member(BuiltinMemberOwner::Parameterized(builtin), &member_text);
            if matches!(selected, Some(BuiltinMemberSignature::Callable(template)) if template.receiver.is_none())
            {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            if matches!(selected, Some(BuiltinMemberSignature::Callable(template)) if template.receiver.is_some())
            {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MethodRequiresCall,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            if selected.is_none() {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownMember,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            return None;
        }
        if member_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(self.synthesize_tuple_element_access(
                expression,
                object,
                typed_object,
                member,
                &member_text,
            ));
        }
        if self.tuple_parts(typed_object.type_id).is_some() {
            self.checking.errors.push(ExpressionCheckingError {
                kind: if member_text == "copy" {
                    ExpressionCheckingErrorKind::CopyRequiresCall
                } else {
                    ExpressionCheckingErrorKind::UnknownMember
                },
                span: member,
            });
            return Some(self.recovery_temporary());
        }
        if let Some((sequence, _, _)) = self.sequence_parts(typed_object.type_id) {
            let name = &member_text;
            let primitive = match sequence {
                SequenceKind::String => PrimitiveType::String,
                SequenceKind::Bytes => PrimitiveType::Bytes,
            };
            let selected = self
                .signatures
                .builtins()
                .member(BuiltinMemberOwner::Primitive(primitive), name);
            self.checking.errors.push(ExpressionCheckingError {
                kind: match selected {
                    Some(BuiltinMemberSignature::Callable(template))
                        if template.receiver.is_some() =>
                    {
                        ExpressionCheckingErrorKind::MethodRequiresCall
                    }
                    Some(BuiltinMemberSignature::Callable(_)) => {
                        ExpressionCheckingErrorKind::AssociatedFunctionRequiresType
                    }
                    Some(BuiltinMemberSignature::Field(_)) | None => {
                        ExpressionCheckingErrorKind::UnknownMember
                    }
                },
                span: member,
            });
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
                let declared_type_id = self.field_access_type(declared, object_capability);
                let mut narrowing_place = self.narrowing_place(object);
                if let Some(place) = &mut narrowing_place {
                    place.fields.push(field.declaration);
                }
                let type_id = narrowing_place
                    .as_ref()
                    .and_then(|place| self.effective_narrowing(place))
                    .map_or(declared_type_id, |fact| fact.narrowed_type);
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
                        declared_type_id,
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
                let mut physical = self.physical_place_for(object, typed_object);
                physical
                    .projections
                    .push(PhysicalPlaceProjection::Field(field.declaration));
                if category == ValueCategory::GcReference {
                    physical.storage = ValueCategory::GcReference;
                }
                self.checking.physical_places.insert(expression.id, physical);
                self.propagate_tracked_lifetime_projection(
                    object,
                    expression.id,
                    PhysicalPlaceProjection::Field(field.declaration),
                    type_id,
                    category,
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
            StructMemberSignatureKind::AssociatedFunction { .. }
            | StructMemberSignatureKind::AssociatedTypeFactory { .. } => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                    span: member,
                });
                Some(self.recovery_temporary())
            }
        }
    }

    fn synthesize_namespace_member_access(
        &mut self,
        expression: &Expression,
        namespace: BuiltinNamespace,
        member: Span,
    ) -> TypedExpression {
        let name = self
            .module
            .text(member)
            .expect("built-in namespace member belongs to the source module")
            .to_string();
        let selected = self
            .signatures
            .builtins()
            .member(BuiltinMemberOwner::Namespace(namespace), &name)
            .cloned();
        let Some(BuiltinMemberSignature::Callable(template)) = selected else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::UnknownMember,
                span: member,
            });
            return self.recovery_temporary();
        };
        let signature = template
            .instantiate(&[], self.types.types_mut())
            .expect("built-in namespace member has no type substitutions");
        let operation = match (namespace, name.as_str()) {
            (BuiltinNamespace::Ascii, "encode") => ResolvedBuiltinOperation::AsciiEncode,
            (BuiltinNamespace::Ascii, "decode") => {
                let members = self
                    .union_members(signature.return_type)
                    .expect("ascii.decode returns string-or-Error(string)");
                let string_member = members
                    .iter()
                    .position(|member| self.primitive_kind(*member) == Some(PrimitiveType::String))
                    .expect("ascii.decode result contains string");
                let error_member = members
                    .iter()
                    .position(|member| self.error_payload_type(*member).is_some())
                    .expect("ascii.decode result contains Error(string)");
                ResolvedBuiltinOperation::AsciiDecode {
                    result_type: signature.return_type,
                    string_member,
                    error_member,
                }
            }
            _ => unreachable!("the built-in namespace catalogue exposes only known members"),
        };
        let type_id = self.types.types_mut().callable(
            signature.parameters,
            signature.return_type,
            AccessCapability::Const,
        );
        self.checking
            .resolved_builtin_operations
            .insert(expression.id, operation);
        TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        }
    }

    fn synthesize_error_value_access(
        &mut self,
        expression: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        member: Span,
        name: &str,
        arguments: &[TypeId],
    ) -> TypedExpression {
        let selected = self
            .signatures
            .builtins()
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Error),
                name,
            )
            .cloned();
        let invalid_kind = match &selected {
            Some(BuiltinMemberSignature::Callable(_)) => {
                ExpressionCheckingErrorKind::AssociatedFunctionRequiresType
            }
            Some(BuiltinMemberSignature::Field(_)) | None => {
                ExpressionCheckingErrorKind::UnknownMember
            }
        };
        let Some(BuiltinMemberSignature::Field(template)) = selected else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: invalid_kind,
                span: member,
            });
            return self.recovery_temporary();
        };
        let payload_type = template
            .instantiate(arguments, self.types.types_mut())
            .expect("Error.value has one payload substitution");
        let category = self.field_category(typed_object, payload_type);
        self.checking.places.insert(
            expression.id,
            Place {
                symbol: None,
                declared_type_id: payload_type,
                type_id: payload_type,
                category,
                binding_mutability: None,
                value_capability: ValueCapability::Const,
            },
        );
        self.checking.resolved_builtin_operations.insert(
            expression.id,
            ResolvedBuiltinOperation::ErrorValue {
                error_type: typed_object.type_id,
                payload_type,
            },
        );
        let mut physical = self.physical_place_for(object, typed_object);
        physical
            .projections
            .push(PhysicalPlaceProjection::BuiltinErrorValue);
        if category == ValueCategory::GcReference {
            physical.storage = ValueCategory::GcReference;
        }
        self.checking.physical_places.insert(expression.id, physical);
        self.propagate_tracked_lifetime_projection(
            object,
            expression.id,
            PhysicalPlaceProjection::BuiltinErrorValue,
            payload_type,
            category,
        );
        TypedExpression {
            type_id: payload_type,
            category,
        }
    }

    /// Selects one statically known tuple element and records it as an
    /// ordinary place. Tuple fields are numeric syntax rather than dynamic
    /// indices, so no runtime bounds check or common element type is needed.
    fn synthesize_tuple_element_access(
        &mut self,
        expression: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        member: Span,
        member_text: &str,
    ) -> TypedExpression {
        let Some((elements, object_capability, is_gc)) =
            self.tuple_parts(typed_object.type_id)
        else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidTupleElementOwner {
                    found: typed_object.type_id,
                },
                span: member,
            });
            return self.recovery_temporary();
        };
        let index = member_text.parse::<usize>().unwrap_or(usize::MAX);
        let Some(declared) = elements.get(index).copied() else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TupleElementOutOfRange {
                    index,
                    arity: elements.len(),
                },
                span: member,
            });
            return self.recovery_temporary();
        };
        let object_capability =
            if !is_gc && typed_object.category == ValueCategory::FreshTemporary {
                AccessCapability::Mut
            } else {
                object_capability
            };
        let declared_type_id = self.field_access_type(declared, object_capability);
        let category = self.field_category(typed_object, declared_type_id);
        let capability = self
            .types
            .types()
            .get(declared_type_id)
            .and_then(SemanticType::capability)
            .expect("tuple element type has a value capability");
        self.checking.places.insert(
            expression.id,
            Place {
                symbol: None,
                declared_type_id,
                type_id: declared_type_id,
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
            ResolvedMember::TupleElement { index },
        );
        let mut physical = self.physical_place_for(object, typed_object);
        physical
            .projections
            .push(PhysicalPlaceProjection::TupleElement(index));
        if category == ValueCategory::GcReference {
            physical.storage = ValueCategory::GcReference;
        }
        self.checking.physical_places.insert(expression.id, physical);
        self.propagate_tracked_lifetime_projection(
            object,
            expression.id,
            PhysicalPlaceProjection::TupleElement(index),
            declared_type_id,
            category,
        );
        TypedExpression {
            type_id: declared_type_id,
            category,
        }
    }

    fn physical_place_for(
        &self,
        expression: &Expression,
        typed: TypedExpression,
    ) -> PhysicalPlace {
        self.checking
            .physical_places
            .get(&expression.id)
            .cloned()
            .unwrap_or(PhysicalPlace {
                root: PhysicalPlaceRoot::Expression(expression.id),
                projections: Vec::new(),
                storage: typed.category,
            })
    }

    fn propagate_tracked_lifetime_projection(
        &mut self,
        source: &Expression,
        target: NodeId,
        projection: PhysicalPlaceProjection,
        target_type: TypeId,
        category: ValueCategory,
    ) {
        if !self.type_contains_tracked_reference(target_type) {
            return;
        }
        let Some(link) = self
            .checking
            .tracked_lifetime_links
            .get(&source.id)
            .cloned()
        else {
            return;
        };
        let source_is_tracked_reference = self
            .checking
            .expressions
            .get(&source.id)
            .is_some_and(|typed| self.tracked_reference_parts(typed.type_id).is_some());
        let sources = link
            .sources
            .into_iter()
            .map(|mut place| {
                if source_is_tracked_reference {
                    place.projections.push(projection);
                }
                if category == ValueCategory::GcReference {
                    place.storage = ValueCategory::GcReference;
                }
                place
            })
            .collect();
        self.checking
            .tracked_lifetime_links
            .insert(target, TrackedLifetimeLink { sources });
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
    fn synthesize_associated_access(
        &mut self,
        expression: &Expression,
        owner: &TypeSyntax,
        member: Span,
    ) -> Option<TypedExpression> {
        if self.is_inferred_error_constructor(owner, member) {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::CannotInferErrorPayload,
                span: expression.span,
            });
            return Some(self.recovery_temporary());
        }
        let owner_type = self.resolved_type_syntax(owner.id)?;
        if let Some((builtin, arguments, _)) = self.parameterized_builtin_parts(owner_type) {
            let name = self
                .module
                .text(member)
                .expect("built-in associated member belongs to the source module");
            let selected = self
                .signatures
                .builtins()
                .member(BuiltinMemberOwner::Parameterized(builtin), name)
                .cloned();
            let invalid_kind = match &selected {
                Some(BuiltinMemberSignature::Field(_)) => {
                    ExpressionCheckingErrorKind::FieldRequiresValue
                }
                Some(BuiltinMemberSignature::Callable(_)) | None => {
                    ExpressionCheckingErrorKind::UnknownMember
                }
            };
            let Some(BuiltinMemberSignature::Callable(template)) = selected else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: invalid_kind,
                    span: member,
                });
                return Some(self.recovery_temporary());
            };
            if template.receiver.is_some() {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MethodRequiresValue,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            let signature = template
                .instantiate(&arguments, self.types.types_mut())
                .expect("resolved built-in application supplies the catalogue arity");
            let type_id = self.types.types_mut().callable(
                signature.parameters,
                signature.return_type,
                AccessCapability::Const,
            );
            self.checking.resolved_builtin_operations.insert(
                expression.id,
                ResolvedBuiltinOperation::Constructor {
                    builtin,
                    type_arguments: arguments,
                    error_inference: (builtin == BuiltinType::Error)
                        .then_some(ErrorConstructorInference::Explicit),
                },
            );
            return Some(TypedExpression {
                type_id,
                category: ValueCategory::FreshTemporary,
            });
        }
        if self.primitive_kind(owner_type) == Some(PrimitiveType::Bytes) {
            let name = self
                .module
                .text(member)
                .expect("byte associated member belongs to the source module");
            let selected = self
                .signatures
                .builtins()
                .member(
                    BuiltinMemberOwner::Primitive(PrimitiveType::Bytes),
                    name,
                )
                .cloned();
            let Some(BuiltinMemberSignature::Callable(template)) = selected else {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownMember,
                    span: member,
                });
                return Some(self.recovery_temporary());
            };
            if template.receiver.is_some() {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::MethodRequiresValue,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
            let signature = template
                .instantiate(&[], self.types.types_mut())
                .expect("byte associated member has no substitutions");
            let type_id = self.types.types_mut().callable(
                signature.parameters,
                signature.return_type,
                AccessCapability::Const,
            );
            self.checking.resolved_sequence_operations.insert(
                expression.id,
                ResolvedSequenceOperation::BytesConcat,
            );
            return Some(TypedExpression {
                type_id,
                category: ValueCategory::FreshTemporary,
            });
        }
        let Some((aggregate_owner, _, false)) = self.aggregate_parts(owner_type) else {
            if matches!(
                self.types.types().get(owner_type),
                Some(SemanticType::TemplateParameter { .. })
            ) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::InvalidMemberOwner { found: owner_type },
                    span: owner.span,
                });
                return Some(self.recovery_temporary());
            }
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
            .aggregate_signature(aggregate_owner)
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
            StructMemberSignatureKind::AssociatedTypeFactory { .. } => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownMember,
                    span: member,
                });
                return Some(self.recovery_temporary());
            }
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
        if self.signatures.is_runtime_template(declaration) {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TemplateRequiresSpecialization,
                span: member,
            });
            return Some(self.recovery_temporary());
        }
        let signature = match aggregate_owner {
            AggregateOwner::Source(_) => self.signatures.callable(declaration),
            AggregateOwner::Generated(owner) => self
                .signatures
                .specialized_callable(owner, declaration)
                .or_else(|| self.runtime_generated_callables.get(&(owner, declaration))),
        }
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

    fn parameterized_builtin_parts(
        &self,
        type_id: TypeId,
    ) -> Option<(BuiltinType, Vec<TypeId>, AccessCapability)> {
        let SemanticType::Builtin {
            builtin,
            arguments,
            capability,
        } = self.types.types().get(type_id)?
        else {
            return None;
        };
        Some((*builtin, arguments.clone(), *capability))
    }

    fn queue_parts(&self, type_id: TypeId) -> Option<(Vec<TypeId>, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Builtin {
                builtin: BuiltinType::Queue,
                arguments,
                capability,
            } => Some((arguments.clone(), *capability, false)),
            SemanticType::Gc { target, capability }
            | SemanticType::Tracked { target, capability } => {
                let SemanticType::Builtin {
                    builtin: BuiltinType::Queue,
                    arguments,
                    ..
                } = self.types.types().get(*target)?
                else {
                    return None;
                };
                Some((
                    arguments.clone(),
                    *capability,
                    matches!(self.types.types().get(type_id), Some(SemanticType::Gc { .. })),
                ))
            }
            _ => None,
        }
    }

    fn is_inferred_error_constructor(&self, owner: &TypeSyntax, member: Span) -> bool {
        matches!(
            &owner.kind,
            crate::ast::TypeKind::Builtin {
                builtin: BuiltinType::Error,
                arguments,
            } if arguments.is_empty()
                && self
                    .module
                    .text(member)
                    .is_ok_and(|name| name == "new")
        )
    }

    /// Peels a plain, GC-qualified, or tracked concrete struct into the
    /// information shared by field, method, copy, and structural-interface
    /// checking. The boolean distinguishes GC storage for receiver validation;
    /// tracked references automatically dereference as plain receivers.
    fn aggregate_parts(&self, type_id: TypeId) -> Option<(AggregateOwner, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::NamedStruct {
                declaration,
                capability,
            }
            | SemanticType::AnonymousStruct {
                expression: declaration,
                capability,
            } => Some((AggregateOwner::Source(*declaration), *capability, false)),
            SemanticType::GeneratedStruct { capability, .. } => Some((
                AggregateOwner::Generated(self.canonical_generated_owner(type_id)?),
                *capability,
                false,
            )),
            SemanticType::Gc { target, capability } => {
                match self.types.types().get(*target)? {
                    SemanticType::NamedStruct { declaration, .. }
                    | SemanticType::AnonymousStruct {
                        expression: declaration,
                        ..
                    } => Some((AggregateOwner::Source(*declaration), *capability, true)),
                    SemanticType::GeneratedStruct { .. } => Some((
                        AggregateOwner::Generated(self.canonical_generated_owner(*target)?),
                        *capability,
                        true,
                    )),
                    _ => None,
                }
            }
            SemanticType::Tracked { target, capability } => {
                match self.types.types().get(*target)? {
                    SemanticType::NamedStruct { declaration, .. }
                    | SemanticType::AnonymousStruct {
                        expression: declaration,
                        ..
                    } => Some((AggregateOwner::Source(*declaration), *capability, false)),
                    SemanticType::GeneratedStruct { .. } => Some((
                        AggregateOwner::Generated(self.canonical_generated_owner(*target)?),
                        *capability,
                        false,
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Maps a capability-selected generated struct back to the canonical
    /// factory instance that owns its collected fields and specialized
    /// callables. Capability is part of a value's `TypeId`, but it does not
    /// create a second declaration or a second method specialization.
    fn canonical_generated_owner(&self, type_id: TypeId) -> Option<TypeId> {
        if self.signatures.generated_struct(type_id).is_some()
            || self.runtime_generated_structs.contains_key(&type_id)
        {
            return Some(type_id);
        }
        self.signatures
            .generated_structs()
            .values()
            .find(|signature| {
                self.types
                    .types()
                    .has_same_shape(signature.type_id, type_id)
                    == Some(true)
            })
            .map(|signature| signature.type_id)
            .or_else(|| {
                self.runtime_generated_structs
                    .values()
                    .find(|signature| {
                        self.types
                            .types()
                            .has_same_shape(signature.type_id, type_id)
                            == Some(true)
                    })
                    .map(|signature| signature.type_id)
            })
    }

    /// Peels a plain, GC-qualified, or tracked tuple. Elements retain their
    /// declared canonical identities; the returned capability is the effective
    /// access to the tuple payload, and the flag distinguishes GC-backed
    /// storage.
    fn tuple_parts(
        &self,
        type_id: TypeId,
    ) -> Option<(Vec<TypeId>, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Tuple {
                elements,
                capability,
            } => Some((elements.clone(), *capability, false)),
            SemanticType::Gc { target, capability } => {
                let SemanticType::Tuple { elements, .. } = self.types.types().get(*target)? else {
                    return None;
                };
                Some((elements.clone(), *capability, true))
            }
            SemanticType::Tracked { target, capability } => {
                let SemanticType::Tuple { elements, .. } = self.types.types().get(*target)? else {
                    return None;
                };
                Some((elements.clone(), *capability, false))
            }
            _ => None,
        }
    }

    /// Peels a plain, GC-qualified, or tracked string/byte sequence. The
    /// capability is the effective access to the sequence payload, while the
    /// final flag records whether that payload lives behind a GC reference.
    fn sequence_parts(
        &self,
        type_id: TypeId,
    ) -> Option<(SequenceKind, AccessCapability, bool)> {
        match self.types.types().get(type_id)? {
            SemanticType::Primitive {
                primitive: PrimitiveType::String,
                capability,
            } => Some((SequenceKind::String, *capability, false)),
            SemanticType::Primitive {
                primitive: PrimitiveType::Bytes,
                capability,
            } => Some((SequenceKind::Bytes, *capability, false)),
            SemanticType::Gc { target, capability } => {
                let sequence = match self.types.types().get(*target)? {
                    SemanticType::Primitive {
                        primitive: PrimitiveType::String,
                        ..
                    } => SequenceKind::String,
                    SemanticType::Primitive {
                        primitive: PrimitiveType::Bytes,
                        ..
                    } => SequenceKind::Bytes,
                    _ => return None,
                };
                Some((sequence, *capability, true))
            }
            SemanticType::Tracked { target, capability } => {
                let sequence = match self.types.types().get(*target)? {
                    SemanticType::Primitive {
                        primitive: PrimitiveType::String,
                        ..
                    } => SequenceKind::String,
                    SemanticType::Primitive {
                        primitive: PrimitiveType::Bytes,
                        ..
                    } => SequenceKind::Bytes,
                    _ => return None,
                };
                Some((sequence, *capability, false))
            }
            _ => None,
        }
    }

    /// Reads one sequence element and records the bounds check required at
    /// runtime. Reads are trivial values, but the same expression also records
    /// index-place access so assignment can reuse the evaluated receiver/index.
    fn synthesize_sequence_index(
        &mut self,
        expression: &Expression,
        object: &Expression,
        index: &Expression,
    ) -> Option<TypedExpression> {
        let typed_object = self.synthesize(object)?;
        let deferred_owner = matches!(
            self.types.types().get(typed_object.type_id),
            Some(SemanticType::Builtin { .. })
        );
        let sequence = if self.is_recovery(typed_object.type_id) {
            None
        } else if deferred_owner {
            None
        } else {
            match self.sequence_parts(typed_object.type_id) {
                Some(parts) => Some(parts),
                None => {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::InvalidSequenceOwner {
                            found: typed_object.type_id,
                        },
                        span: object.span,
                    });
                    None
                }
            }
        };
        let int_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Int, AccessCapability::Const);
        let typed_index = self.check(index, int_type)?;
        if deferred_owner {
            return None;
        }
        if sequence.is_none() || self.is_recovery(typed_index.type_id) {
            return Some(self.recovery_temporary());
        }
        let (sequence, capability, _) = sequence.expect("valid sequence was classified above");
        let element = match sequence {
            SequenceKind::String => PrimitiveType::Char,
            SequenceKind::Bytes => PrimitiveType::Int,
        };
        let type_id = self
            .types
            .types_mut()
            .primitive(element, AccessCapability::Const);
        self.checking.places.insert(
            expression.id,
            Place {
                symbol: None,
                declared_type_id: type_id,
                type_id,
                category: ValueCategory::OwnedInlinePlace,
                binding_mutability: None,
                value_capability: match capability {
                    AccessCapability::Const => ValueCapability::Const,
                    AccessCapability::Mut => ValueCapability::Mut,
                },
            },
        );
        self.checking.resolved_sequence_operations.insert(
            expression.id,
            ResolvedSequenceOperation::Index { sequence },
        );
        self.checking
            .sequence_runtime_checks
            .entry(expression.id)
            .or_default()
            .push(SequenceRuntimeCheck::IndexBounds);
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Copies an end-exclusive range into a new independently owned mutable
    /// sequence. Optional bounds are still checked left-to-right when present;
    /// lowering performs negative-bound normalization and range validation.
    fn synthesize_sequence_slice(
        &mut self,
        expression: &Expression,
        object: &Expression,
        start: Option<&Expression>,
        end: Option<&Expression>,
    ) -> Option<TypedExpression> {
        let typed_object = self.synthesize(object)?;
        let deferred_owner = matches!(
            self.types.types().get(typed_object.type_id),
            Some(SemanticType::Builtin { .. })
        );
        let sequence = if self.is_recovery(typed_object.type_id) {
            None
        } else if deferred_owner {
            None
        } else {
            match self.sequence_parts(typed_object.type_id) {
                Some(parts) => Some(parts),
                None => {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::InvalidSequenceOwner {
                            found: typed_object.type_id,
                        },
                        span: object.span,
                    });
                    None
                }
            }
        };
        let int_type = self
            .types
            .types_mut()
            .primitive(PrimitiveType::Int, AccessCapability::Const);
        let typed_start = match start {
            Some(bound) => Some(self.check(bound, int_type)?),
            None => None,
        };
        let typed_end = match end {
            Some(bound) => Some(self.check(bound, int_type)?),
            None => None,
        };
        let invalid_bound = typed_start
            .into_iter()
            .chain(typed_end)
            .any(|typed| self.is_recovery(typed.type_id));
        if deferred_owner {
            return None;
        }
        if sequence.is_none() || invalid_bound {
            return Some(self.recovery_temporary());
        }
        let (sequence, _, _) = sequence.expect("valid sequence was classified above");
        let primitive = match sequence {
            SequenceKind::String => PrimitiveType::String,
            SequenceKind::Bytes => PrimitiveType::Bytes,
        };
        let type_id = self
            .types
            .types_mut()
            .primitive(primitive, AccessCapability::Mut);
        self.checking.resolved_sequence_operations.insert(
            expression.id,
            ResolvedSequenceOperation::Slice { sequence },
        );
        self.checking
            .sequence_runtime_checks
            .entry(expression.id)
            .or_default()
            .push(SequenceRuntimeCheck::SliceBounds);
        Some(TypedExpression {
            type_id,
            category: ValueCategory::FreshTemporary,
        })
    }

    /// Finds the collected member table for a source aggregate or a canonical
    /// factory-generated struct instance.
    fn aggregate_signature(&self, owner: AggregateOwner) -> Option<&StructSignature> {
        match owner {
            AggregateOwner::Source(owner) => self
                .signatures
                .named_struct(owner)
                .or_else(|| self.signatures.anonymous_struct(owner)),
            AggregateOwner::Generated(type_id) => self
                .signatures
                .generated_struct(type_id)
                .or_else(|| self.runtime_generated_structs.get(&type_id)),
        }
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

    /// Identifies owners that cannot expose concrete aggregate members.
    /// Strings, bytes, built-ins, and interfaces use separate member families;
    /// a symbolic template parameter reaches this check only when it has no
    /// interface bound, so it deliberately exposes no members at all.
    fn member_owner_is_definitively_invalid(&self, type_id: TypeId) -> bool {
        match self.types.types().get(type_id) {
            Some(SemanticType::Tracked { target, .. }) => {
                self.member_owner_is_definitively_invalid(*target)
            }
            Some(
                SemanticType::Primitive {
                    primitive: PrimitiveType::Unit
                        | PrimitiveType::None
                        | PrimitiveType::Int
                        | PrimitiveType::Float
                        | PrimitiveType::Bool
                        | PrimitiveType::Char,
                    ..
                } | SemanticType::TemplateParameter { .. },
            ) => true,
            _ => false,
        }
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
            if matches!(
                declared_semantic.storage_semantics(),
                Some(StorageSemantics::Gc | StorageSemantics::TrackedReference)
            ) {
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
            semantic.storage_semantics() == Some(StorageSemantics::Gc)
        }) {
            return ValueCategory::GcReference;
        }
        match object.category {
            ValueCategory::FreshTemporary => ValueCategory::FreshTemporary,
            ValueCategory::OwnedInlinePlace => ValueCategory::OwnedInlinePlace,
            ValueCategory::BorrowedPlace | ValueCategory::GcReference => {
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
        let transfer = if semantic.storage_semantics() == Some(StorageSemantics::TrackedReference) {
            Some(ValueTransfer::Borrow)
        } else if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            Some(ValueTransfer::CopyGcReference)
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
            ExpressionKind::Index { .. } => {
                self.synthesize_sequence_index_assignment(target, operator, value)
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
            let checked = self.check(value, place.declared_type_id)?;
            if self.is_recovery(checked.type_id) {
                return Some(self.recovery_temporary());
            }
            let preserves_borrows = self.validate_place_replacement(target, value);
            if !self.validate_owning_transfer(value, checked, mutable)
                || !mutable
                || !preserves_borrows
            {
                return Some(self.recovery_temporary());
            }
            if let Some(narrowing_place) = self.narrowing_place(target)
                && self.union_members(place.declared_type_id).is_some()
            {
                if self.effective_narrowing(&narrowing_place).is_some()
                    && self.assignment_preserves_narrowing(&narrowing_place, value.id)
                {
                    self.checking.union_mutations.insert(
                        target.id,
                        UnionMutationKind::SameTagReplacement,
                    );
                } else {
                    self.checking.union_mutations.insert(
                        target.id,
                        UnionMutationKind::GuardedTagChange,
                    );
                    if self.effective_narrowing(&narrowing_place).is_some() {
                        self.invalidate_place_narrowings(target.id, &narrowing_place);
                    }
                }
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
        if let Some(narrowing_place) = self.narrowing_place(target)
            && self.effective_narrowing(&narrowing_place).is_some()
        {
            self.checking
                .union_mutations
                .insert(target.id, UnionMutationKind::PayloadMutation);
        }
        Some(self.fresh_primitive(PrimitiveType::Unit))
    }

    /// Rejects replacing storage which is a strict ancestor of an outstanding
    /// tracked interior address. Replacing the referenced leaf itself keeps
    /// that address stable, and redirecting an identifier root is handled by
    /// root assignment without touching its old backing storage.
    fn validate_place_replacement(
        &mut self,
        target: &Expression,
        value: &Expression,
    ) -> bool {
        let Some(replaced) = self.checking.physical_places.get(&target.id).cloned() else {
            return true;
        };
        let changes_locked_union_member = self
            .checking
            .places
            .get(&target.id)
            .is_some_and(|place| self.union_members(place.declared_type_id).is_some())
            && self.narrowing_place(target).is_some_and(|place| {
                self.effective_narrowing(&place).is_some()
                    && !self.assignment_preserves_narrowing(&place, value.id)
            });
        let mut conflicts = Vec::new();
        for (holder, link) in &self.current_tracked_bindings {
            if !self.symbol_is_live_after(*holder, target.id) {
                continue;
            }
            for source in &link.sources {
                let replaces_ancestor = replaced.projections.len() < source.projections.len()
                    && source.projections.starts_with(&replaced.projections);
                let overlaps_opaque_derivation = source
                    .projections
                    .iter()
                    .position(|projection| {
                        *projection == PhysicalPlaceProjection::OpaqueDerived
                    })
                    .is_some_and(|opaque| {
                        let known = &source.projections[..opaque];
                        replaced.projections.starts_with(known)
                            || known.starts_with(&replaced.projections)
                    });
                let replaces_narrowed_union_payload = changes_locked_union_member
                    && source.projections == replaced.projections;
                if source.root == replaced.root
                    && (replaces_ancestor
                        || overlaps_opaque_derivation
                        || replaces_narrowed_union_payload)
                    && !conflicts.contains(source)
                {
                    conflicts.push(source.clone());
                }
            }
        }
        if conflicts.is_empty() {
            return true;
        }
        self.checking.errors.push(ExpressionCheckingError {
            kind: ExpressionCheckingErrorKind::TrackedBorrowInvalidated,
            span: target.span,
        });
        self.checking.borrow_invalidations.insert(target.id, conflicts);
        false
    }

    fn symbol_is_live_after(&self, symbol: SymbolId, node: NodeId) -> bool {
        self.active_loops.iter().any(|active| {
            self.loop_symbol_references
                .get(&active.expression)
                .is_some_and(|symbols| symbols.contains(&symbol))
        }) || self.symbol_references.get(&symbol).is_some_and(|references| {
            let mutation_end = self.mutation_ends.get(&node).copied().unwrap_or_else(|| {
                self.expression_spans
                    .get(&node)
                    .map_or(0, |span| span.end)
            });
            references.iter().any(|reference| {
                self.expression_spans.get(reference).is_some_and(|span| {
                    span.module_id == node.module_id && span.start >= mutation_end
                })
                    && self.control_paths_are_compatible(node, *reference)
            })
        })
    }

    fn control_paths_are_compatible(&self, left: NodeId, right: NodeId) -> bool {
        let Some(left_path) = self.expression_branches.get(&left) else {
            return true;
        };
        let Some(right_path) = self.expression_branches.get(&right) else {
            return true;
        };
        !left_path.iter().any(|(conditional, arm)| {
            right_path
                .iter()
                .any(|(other, other_arm)| conditional == other && arm != other_arm)
        })
    }

    fn displace_rebound_storage(&mut self, symbol: SymbolId, assignment: NodeId) {
        let displaced = PhysicalPlaceRoot::DisplacedSymbol(symbol, assignment);
        self.checking.displaced_roots.insert(assignment, displaced);
        for link in self.current_tracked_bindings.values_mut() {
            for source in &mut link.sources {
                if source.root == PhysicalPlaceRoot::Symbol(symbol) {
                    source.root = displaced;
                }
            }
        }
        for (holder, link) in &self.current_tracked_bindings {
            self.checking
                .tracked_binding_lifetimes
                .insert(*holder, link.clone());
        }
    }

    /// Checks writes through string/byte index places. Index-place mutation is
    /// governed by access to the sequence payload, never by mutability of the
    /// binding which holds the sequence reference.
    fn synthesize_sequence_index_assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
    ) -> Option<TypedExpression> {
        let typed_target = match self.synthesize(target) {
            Some(typed) => typed,
            None => {
                let _ = self.synthesize(value);
                return None;
            }
        };
        let Some(place) = self.checking.places.get(&target.id).copied() else {
            let _ = self.synthesize(value);
            return Some(self.recovery_temporary());
        };
        let Some(ResolvedSequenceOperation::Index { sequence }) = self
            .checking
            .resolved_sequence_operations
            .get(&target.id)
            .copied()
        else {
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
            let checked = self.check(value, place.declared_type_id)?;
            if !mutable || self.is_recovery(checked.type_id) {
                return Some(self.recovery_temporary());
            }
            self.checking
                .transfers
                .insert(value.id, ValueTransfer::TrivialCopy);
            if sequence == SequenceKind::Bytes {
                self.checking
                    .sequence_runtime_checks
                    .entry(target.id)
                    .or_default()
                    .push(SequenceRuntimeCheck::ByteValueRange);
            }
            return Some(self.fresh_primitive(PrimitiveType::Unit));
        }

        let valid = sequence == SequenceKind::Bytes
            && matches!(
                operator,
                AssignmentOperator::Add
                    | AssignmentOperator::Subtract
                    | AssignmentOperator::Multiply
                    | AssignmentOperator::Divide
                    | AssignmentOperator::Remainder
                    | AssignmentOperator::BitwiseAnd
                    | AssignmentOperator::BitwiseXor
                    | AssignmentOperator::BitwiseOr
                    | AssignmentOperator::ShiftLeft
                    | AssignmentOperator::ShiftRight
            );
        if !valid {
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
        let checked = self.check(value, place.declared_type_id)?;
        if !mutable || self.is_recovery(checked.type_id) {
            return Some(self.recovery_temporary());
        }
        self.checking
            .transfers
            .insert(value.id, ValueTransfer::TrivialCopy);
        self.checking
            .sequence_runtime_checks
            .entry(target.id)
            .or_default()
            .push(SequenceRuntimeCheck::ByteValueRange);
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
            let checked = self.check(value, place.declared_type_id)?;
            let valid_value = !self.is_recovery(checked.type_id)
                && !self.reject_escaping_temporary_tracked_borrow(value);
            if mutable && valid_value {
                if let Some(narrowing_place) = self.narrowing_place(target)
                    && self.union_members(place.declared_type_id).is_some()
                {
                    if self.effective_narrowing(&narrowing_place).is_some()
                        && self.assignment_preserves_narrowing(&narrowing_place, value.id)
                    {
                        self.checking.union_mutations.insert(
                            target.id,
                            UnionMutationKind::SameTagReplacement,
                        );
                    } else {
                        self.checking.union_mutations.insert(
                            target.id,
                            UnionMutationKind::GuardedTagChange,
                        );
                        if self.effective_narrowing(&narrowing_place).is_some() {
                            self.invalidate_place_narrowings(target.id, &narrowing_place);
                        }
                    }
                }
                let (category, transfer) = self.assignment_transfer(checked);
                self.current_binding_categories.insert(symbol, category);
                let mut sources = self.tracked_lifetime_sources(value);
                let redirects_storage = self
                    .types
                    .types()
                    .get(place.declared_type_id)
                    .is_some_and(|semantic| {
                        semantic.copy_semantics() != Some(CopySemantics::Trivial)
                    });
                let preserves_old_backing = self.current_tracked_bindings.iter().any(
                    |(holder, link)| {
                        self.symbol_is_live_after(*holder, target.id)
                            && link.sources.iter().any(|source| {
                                source.root == PhysicalPlaceRoot::Symbol(symbol)
                            })
                    },
                );
                if redirects_storage && preserves_old_backing {
                    self.displace_rebound_storage(symbol, target.id);
                    for source in &mut sources {
                        if source.root == PhysicalPlaceRoot::Symbol(symbol) {
                            source.root =
                                PhysicalPlaceRoot::DisplacedSymbol(symbol, target.id);
                        }
                    }
                }
                self.set_tracked_binding_sources(target.id, symbol, sources);
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
        if let Some(narrowing_place) = self.narrowing_place(target)
            && self.effective_narrowing(&narrowing_place).is_some()
        {
            self.checking
                .union_mutations
                .insert(target.id, UnionMutationKind::PayloadMutation);
        }
        Some(self.fresh_primitive(PrimitiveType::Unit))
    }

    fn synthesize_call(
        &mut self,
        expression: &Expression,
        callee: &Expression,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        if self.inferred_error_constructor_callee(callee) {
            return self.synthesize_inferred_error_call(expression, callee, arguments, None);
        }
        if let Some((error_type, payload_type)) = self.explicit_error_constructor_callee(callee) {
            let _ = self.synthesize(callee)?;
            return self.synthesize_explicit_error_call(
                expression,
                arguments,
                error_type,
                payload_type,
            );
        }
        if self.types.runtime_template_call(expression.id).is_some() {
            return self.synthesize_runtime_template_call(expression, arguments);
        }
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
            Some(SemanticType::Gc { target, .. }) => {
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
        Some(self.call_result(expression, return_type, None))
    }

    fn inferred_error_constructor_callee(&self, callee: &Expression) -> bool {
        let ExpressionKind::AssociatedAccess { owner, member } = &callee.kind else {
            return false;
        };
        self.is_inferred_error_constructor(owner, *member)
    }

    fn explicit_error_constructor_callee(&self, callee: &Expression) -> Option<(TypeId, TypeId)> {
        let ExpressionKind::AssociatedAccess { owner, member } = &callee.kind else {
            return None;
        };
        if self.module.text(*member).ok()? != "new" {
            return None;
        }
        let owner_type = self.resolved_type_syntax(owner.id)?;
        self.error_payload_type(owner_type)
            .map(|payload| (owner_type, payload))
    }

    fn contextual_error_type(&self, expected: TypeId) -> Option<(TypeId, TypeId)> {
        if let Some(payload) = self.error_payload_type(expected) {
            return Some((expected, payload));
        }
        let members = self.union_members(expected)?;
        let mut errors = members
            .into_iter()
            .filter_map(|member| self.error_payload_type(member).map(|payload| (member, payload)));
        let selected = errors.next()?;
        errors.next().is_none().then_some(selected)
    }

    fn error_payload_type(&self, type_id: TypeId) -> Option<TypeId> {
        let SemanticType::Builtin {
            builtin: BuiltinType::Error,
            arguments,
            ..
        } = self.types.types().get(type_id)?
        else {
            return None;
        };
        let [payload] = arguments.as_slice() else {
            return None;
        };
        Some(*payload)
    }

    /// Checks postfix error propagation as a projection plus a conditional
    /// callable return. The operand is synthesized exactly once; both branch
    /// descriptions retain its node and canonical member positions.
    fn synthesize_error_propagation(
        &mut self,
        expression: &Expression,
        operand: &Expression,
    ) -> Option<TypedExpression> {
        let found = self.synthesize(operand)?;
        if self.is_recovery(found.type_id) || self.is_divergence(found.type_id) {
            return Some(found);
        }
        let Some(members) = self.union_members(found.type_id) else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: if self.error_payload_type(found.type_id).is_some() {
                    ExpressionCheckingErrorKind::TryRequiresSuccessMember {
                        operand: found.type_id,
                    }
                } else {
                    ExpressionCheckingErrorKind::InvalidTryOperand {
                        found: found.type_id,
                    }
                },
                span: operand.span,
            });
            return Some(self.recovery_temporary());
        };
        let error_members: Vec<_> = members
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, member)| self.error_payload_type(*member).is_some())
            .collect();
        let (error_member, propagated_error) = match error_members.as_slice() {
            [] => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::TryMissingErrorMember {
                        operand: found.type_id,
                    },
                    span: operand.span,
                });
                return Some(self.recovery_temporary());
            }
            [selected] => *selected,
            _ => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::TryAmbiguousErrorMembers {
                        operand: found.type_id,
                    },
                    span: operand.span,
                });
                return Some(self.recovery_temporary());
            }
        };
        let success_members: Vec<_> = members
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, _)| *index != error_member)
            .map(|(index, member)| (member, index))
            .collect();
        if success_members.is_empty() {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TryRequiresSuccessMember {
                    operand: found.type_id,
                },
                span: operand.span,
            });
            return Some(self.recovery_temporary());
        }
        let capability = self
            .types
            .types()
            .get(found.type_id)
            .and_then(SemanticType::capability)
            .expect("a normalized try union has an outer capability");
        let success_type = self.types.types_mut().union(
            success_members.iter().map(|(member, _)| *member).collect(),
            capability,
        );
        let success_category = if success_members.len() == 1
            && self
                .types
                .types()
                .get(success_type)
                .is_some_and(|semantic| {
                    semantic.storage_semantics() == Some(StorageSemantics::TrackedReference)
                })
        {
            ValueCategory::BorrowedPlace
        } else if success_members.len() == 1 {
            self.union_member_category(found.category, success_members[0].0)
        } else {
            found.category
        };
        let success = TypedExpression {
            type_id: success_type,
            category: success_category,
        };
        let propagated = TypedExpression {
            type_id: propagated_error,
            category: self.union_member_category(found.category, propagated_error),
        };
        let callable_result = self
            .current_callable_result
            .expect("postfix error propagation is checked inside a callable body");
        let return_assignment = match self.classify_contextual_assignment(
            propagated,
            callable_result,
            true,
        ) {
            Ok(assignment) => assignment,
            Err(_) => {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::PropagatedErrorNotAccepted {
                        error: propagated_error,
                        callable_result,
                    },
                    span: expression.span,
                });
                return Some(self.recovery_temporary());
            }
        };
        let return_error = match &return_assignment {
            ContextualAssignment::UnionInjection { member_type, .. } => *member_type,
            ContextualAssignment::ErrorWidening(widening) => widening.destination_error,
            ContextualAssignment::Exact => propagated_error,
            ContextualAssignment::TrackedBorrow { .. }
            | ContextualAssignment::InterfaceView(_)
            | ContextualAssignment::UnionWidening(_) => callable_result,
        };
        if self.type_contains_tracked_reference(return_error)
            && !self.validate_tracked_return_source(operand)
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidTrackedReturnSource,
                span: expression.span,
            });
            return Some(self.recovery_temporary());
        }
        let Some(success_transfer) = self.projection_transfer(success) else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidOwningSource {
                    found: success.type_id,
                    category: success.category,
                },
                span: expression.span,
            });
            return Some(self.recovery_temporary());
        };
        let Some(return_transfer) = self.return_transfer(propagated) else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InvalidReturnSource {
                    found: propagated.type_id,
                    category: propagated.category,
                },
                span: expression.span,
            });
            return Some(self.recovery_temporary());
        };

        let tracked_sources = self.tracked_lifetime_sources(operand);
        if self.type_contains_tracked_reference(success_type) {
            self.record_tracked_lifetime_link(expression.id, tracked_sources);
        }
        if let Some(place) = self.checking.physical_places.get(&operand.id).cloned() {
            self.checking.physical_places.insert(expression.id, place);
        }
        self.record_deferred_exit(
            expression.id,
            DeferredCleanupEdgeKind::ErrorPropagation,
            None,
            Some(operand.id),
        );
        let current_narrowings = self.current_narrowings.clone();
        self.record_narrowing_transition(
            expression.id,
            NarrowingEdgeKind::ErrorPropagation,
            &current_narrowings,
            &HashMap::new(),
        );
        self.checking.resolved_error_propagations.insert(
            expression.id,
            ResolvedErrorPropagation {
                operand: operand.id,
                operand_type: found.type_id,
                success_type,
                success_category,
                success_members,
                propagated_error,
                return_error,
                error_member,
                callable_result,
                return_assignment,
                success_transfer,
                return_transfer,
            },
        );
        Some(success)
    }

    fn projection_transfer(&self, projected: TypedExpression) -> Option<ValueTransfer> {
        let semantic = self.types.types().get(projected.type_id)?;
        if semantic.storage_semantics() == Some(StorageSemantics::TrackedReference) {
            return Some(ValueTransfer::Borrow);
        }
        if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            return Some(ValueTransfer::CopyGcReference);
        }
        match semantic.copy_semantics()? {
            CopySemantics::Trivial => Some(ValueTransfer::TrivialCopy),
            CopySemantics::Recursive => Some(
                if projected.category == ValueCategory::FreshTemporary {
                    ValueTransfer::MoveTemporary
                } else {
                    ValueTransfer::Borrow
                },
            ),
            CopySemantics::NonEscapingErasedView | CopySemantics::TrackedPayload => {
                Some(ValueTransfer::Borrow)
            }
            CopySemantics::GcPayload => None,
        }
    }

    fn synthesize_inferred_error_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        arguments: &[Expression],
        contextual_error: Option<(TypeId, TypeId)>,
    ) -> Option<TypedExpression> {
        let arity_matches = arguments.len() == 1;
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: 1,
                    found: arguments.len(),
                },
                span: call.span,
            });
        }
        let inferred_payload = if let Some(payload) = arguments.first() {
            let checked = if let Some((_, expected_payload)) = contextual_error {
                self.check(payload, expected_payload)
            } else {
                self.synthesize(payload)
            }?;
            let payload_type = contextual_error.map_or(checked.type_id, |(_, payload)| payload);
            let valid = !self.is_recovery(checked.type_id)
                && self.validate_owning_transfer(payload, checked, true);
            for surplus in arguments.get(1..).unwrap_or(&[]) {
                let _ = self.synthesize(surplus);
            }
            if valid && arity_matches {
                let mut tracked = Vec::new();
                self.extend_tracked_lifetime_sources(&mut tracked, payload);
                self.checking
                    .borrow_containing_call_inputs
                    .insert(call.id, tracked);
                Some(payload_type)
            } else {
                None
            }
        } else {
            None
        };
        let Some(payload_type) = inferred_payload else {
            return Some(self.recovery_temporary());
        };
        let error_type = contextual_error.map_or_else(
            || {
                self.types.types_mut().builtin(
                    BuiltinType::Error,
                    vec![payload_type],
                    AccessCapability::Const,
                )
            },
            |(error, _)| error,
        );
        let inference = if contextual_error.is_some() {
            ErrorConstructorInference::Expected
        } else {
            ErrorConstructorInference::Payload
        };
        self.record_inferred_error_constructor(callee, payload_type, error_type, inference);
        Some(self.call_result(call, error_type, None))
    }

    fn synthesize_explicit_error_call(
        &mut self,
        call: &Expression,
        arguments: &[Expression],
        error_type: TypeId,
        payload_type: TypeId,
    ) -> Option<TypedExpression> {
        let arity_matches = arguments.len() == 1;
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: 1,
                    found: arguments.len(),
                },
                span: call.span,
            });
        }
        let mut valid = arity_matches;
        if let Some(payload) = arguments.first() {
            let checked = self.check(payload, payload_type)?;
            if self.is_recovery(checked.type_id)
                || !self.validate_owning_transfer(payload, checked, true)
            {
                valid = false;
            } else {
                let mut tracked = Vec::new();
                self.extend_tracked_lifetime_sources(&mut tracked, payload);
                self.checking
                    .borrow_containing_call_inputs
                    .insert(call.id, tracked);
            }
        }
        for surplus in arguments.get(1..).unwrap_or(&[]) {
            let _ = self.synthesize(surplus);
        }
        if !valid {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(call, error_type, None))
    }

    fn synthesize_contextual_error_constructor_value(
        &mut self,
        expression: &Expression,
        expected: TypeId,
    ) -> Option<TypedExpression> {
        let SemanticType::Callable {
            parameters,
            return_type,
            ..
        } = self.types.types().get(expected)?
        else {
            return None;
        };
        let [payload_parameter] = parameters.as_slice() else {
            return None;
        };
        let error_payload = self.error_payload_type(*return_type)?;
        if self.types.types().has_same_shape(*payload_parameter, error_payload) != Some(true) {
            return None;
        }
        self.checking.resolved_builtin_operations.insert(
            expression.id,
            ResolvedBuiltinOperation::Constructor {
                builtin: BuiltinType::Error,
                type_arguments: vec![error_payload],
                error_inference: Some(ErrorConstructorInference::Expected),
            },
        );
        Some(TypedExpression {
            type_id: expected,
            category: ValueCategory::FreshTemporary,
        })
    }

    fn record_inferred_error_constructor(
        &mut self,
        callee: &Expression,
        payload_type: TypeId,
        error_type: TypeId,
        inference: ErrorConstructorInference,
    ) {
        let callable_type = self.types.types_mut().callable(
            vec![payload_type],
            error_type,
            AccessCapability::Const,
        );
        self.checking.expressions.insert(
            callee.id,
            TypedExpression {
                type_id: callable_type,
                category: ValueCategory::FreshTemporary,
            },
        );
        self.checking.explicit_values.insert(callee.id, true);
        self.checking.resolved_builtin_operations.insert(
            callee.id,
            ResolvedBuiltinOperation::Constructor {
                builtin: BuiltinType::Error,
                type_arguments: vec![payload_type],
                error_inference: Some(inference),
            },
        );
    }

    /// Checks one explicit top-level template invocation. Compile-time type
    /// arguments occupy the declaration's leading parameter positions; only
    /// the remaining expressions are evaluated at runtime.
    fn synthesize_runtime_template_call(
        &mut self,
        call: &Expression,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let request = self
            .types
            .runtime_template_call(call.id)
            .expect("runtime template call was classified by type resolution")
            .clone();
        let function = self
            .runtime_templates
            .get(&request.declaration)
            .expect("top-level runtime template declaration must be indexed")
            .clone();

        let mut concrete_arguments = Vec::new();
        for argument in request.type_arguments {
            let concrete = if let Some(substitutions) = self.current_template_substitutions.clone()
            {
                self.types
                    .types_mut()
                    .substitute_template_parameters(argument, &substitutions)
                    .expect("template argument belongs to the program type store")
            } else {
                argument
            };
            concrete_arguments.push(concrete);
        }

        let type_parameters: Vec<_> = function
            .parameters
            .iter()
            .take_while(|parameter| {
                matches!(&parameter.kind, FunctionParameterKind::Comptime { .. })
            })
            .collect();
        let runtime_arguments = arguments
            .get(request.comptime_argument_count..)
            .unwrap_or(&[]);
        let base_signature = self
            .signatures
            .callable(function.id)
            .expect("runtime template signature must have been collected")
            .clone();
        let expected_total = type_parameters.len() + base_signature.parameters.len();
        let arity_matches = expected_total == arguments.len()
            && concrete_arguments.len() == type_parameters.len();
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: expected_total,
                    found: arguments.len(),
                },
                span: call.span,
            });
        }

        let mut constraints_valid = concrete_arguments.len() == type_parameters.len();
        for (index, (parameter, concrete)) in type_parameters
            .iter()
            .zip(concrete_arguments.iter().copied())
            .enumerate()
        {
            let symbolic = self
                .types
                .types_mut()
                .template_parameter(parameter.id, AccessCapability::Const);
            let Some(bound) = self.types.template_parameter_bound(symbolic).flatten() else {
                continue;
            };
            if !self.runtime_template_constraint_source_is_concrete(concrete) {
                constraints_valid = false;
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::InvalidRuntimeTemplateArgument {
                        found: concrete,
                    },
                    span: arguments
                        .get(index)
                        .map_or(call.span, |argument| argument.span),
                });
                continue;
            }
            let category = if matches!(
                self.types.types().get(concrete),
                Some(SemanticType::Gc { .. })
            ) {
                ValueCategory::GcReference
            } else {
                ValueCategory::BorrowedPlace
            };
            let found = TypedExpression {
                type_id: concrete,
                category,
            };
            if let Err(kind) = self.validate_interface_view_source(found, bound, None) {
                constraints_valid = false;
                self.checking.errors.push(ExpressionCheckingError {
                    kind,
                    span: arguments
                        .get(index)
                        .map_or(call.span, |argument| argument.span),
                });
            }
        }

        let substitutions: HashMap<_, _> = type_parameters
            .iter()
            .zip(concrete_arguments.iter().copied())
            .map(|(parameter, concrete)| (parameter.id, concrete))
            .collect();
        let signature = self.substitute_callable_signature(base_signature, &substitutions);
        let symbolic_request = concrete_arguments
            .iter()
            .copied()
            .any(|argument| self.type_contains_template_parameter(argument));
        let specialization = if constraints_valid && arity_matches && !symbolic_request {
            self.request_runtime_specialization(
                None,
                &function,
                concrete_arguments,
                signature.clone(),
                call.span,
            )
        } else {
            None
        };
        if let Some(specialization) = specialization {
            self.runtime_specialization_calls
                .insert(call.id, specialization);
        }

        let runtime_valid = self.analyze_runtime_template_arguments(
            call,
            runtime_arguments,
            &signature.parameters,
        )?;
        if (!symbolic_request && specialization.is_none())
            || !arity_matches
            || !constraints_valid
            || !runtime_valid
            || self.is_recovery(signature.return_type)
        {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(call, signature.return_type, None))
    }

    fn substitute_callable_signature(
        &mut self,
        mut signature: CallableSignature,
        substitutions: &HashMap<NodeId, TypeId>,
    ) -> CallableSignature {
        signature.parameters = signature
            .parameters
            .into_iter()
            .map(|parameter| {
                self.types
                    .types_mut()
                    .substitute_template_parameters(parameter, substitutions)
                    .expect("specialized parameter type belongs to the program store")
            })
            .collect();
        signature.return_type = self
            .types
            .types_mut()
            .substitute_template_parameters(signature.return_type, substitutions)
            .expect("specialized return type belongs to the program store");
        signature
    }

    fn request_runtime_specialization(
        &mut self,
        owner: Option<TypeId>,
        function: &Function,
        type_arguments: Vec<TypeId>,
        signature: CallableSignature,
        request_span: Span,
    ) -> Option<RuntimeCallableSpecializationId> {
        let key = (owner, function.id, type_arguments.clone());
        if let Some(existing) = self.specialization_cache.get(&key).copied() {
            return Some(existing);
        }
        if self
            .active_specializations
            .iter()
            .any(|(_, declaration, _)| *declaration == function.id)
        {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ExpandingRuntimeTemplateSpecialization,
                span: request_span,
            });
            return None;
        }

        let id = RuntimeCallableSpecializationId(self.runtime_specializations.len());
        self.specialization_cache.insert(key.clone(), id);
        self.runtime_specializations.push(RuntimeCallableSpecialization {
            owner,
            declaration: function.id,
            type_arguments: type_arguments.clone(),
            signature: signature.clone(),
            checking: Box::default(),
        });

        let substitutions: HashMap<_, _> = function
            .parameters
            .iter()
            .filter(|parameter| {
                matches!(&parameter.kind, FunctionParameterKind::Comptime { .. })
            })
            .zip(type_arguments.iter().copied())
            .map(|(parameter, concrete)| (parameter.id, concrete))
            .collect();
        let mut generated_signatures: Vec<_> = self
            .signatures
            .generated_structs()
            .values()
            .filter(|signature| self.type_contains_template_parameter(signature.type_id))
            .cloned()
            .collect();
        generated_signatures.sort_by_key(|signature| signature.type_id.as_usize());
        for source in generated_signatures {
            let mut callable_declarations: Vec<_> = source
                .members()
                .values()
                .filter_map(|member| match member.kind {
                    StructMemberSignatureKind::Method { declaration, .. }
                    | StructMemberSignatureKind::AssociatedFunction { declaration } => {
                        Some(declaration)
                    }
                    StructMemberSignatureKind::Field(_)
                    | StructMemberSignatureKind::AssociatedTypeFactory { .. } => None,
                })
                .collect();
            callable_declarations
                .sort_by_key(|declaration| (declaration.module_id.as_u32(), declaration.node_id));
            let specialized = source.substitute_template_parameters(
                &substitutions,
                self.types.types_mut(),
            );
            if specialized.type_id == source.type_id {
                continue;
            }
            for declaration in callable_declarations {
                if let Some(signature) = self
                    .signatures
                    .specialized_callable(source.type_id, declaration)
                    .cloned()
                {
                    let signature =
                        self.substitute_callable_signature(signature, &substitutions);
                    self.runtime_generated_callables
                        .insert((specialized.type_id, declaration), signature);
                }
            }
            let fields = specialized
                .field_order()
                .iter()
                .filter_map(|name| {
                    let member = specialized.member(name)?;
                    let StructMemberSignatureKind::Field(field) = member.kind else {
                        return None;
                    };
                    Some(LayoutField {
                        declaration: field.declaration,
                        span: member.span,
                        type_id: field.type_id?,
                    })
                })
                .collect();
            self.aggregate_layouts.insert(
                specialized.type_id,
                AggregateLayout {
                    type_id: specialized.type_id,
                    fields,
                },
            );
            if !self.aggregate_order.contains(&specialized.type_id) {
                self.aggregate_order.push(specialized.type_id);
            }
            self.runtime_generated_structs
                .insert(specialized.type_id, specialized);
        }
        let syntax_entries: Vec<_> = self
            .types
            .syntax_types()
            .iter()
            .map(|(syntax, type_id)| (*syntax, *type_id))
            .collect();
        let specialized_syntax = syntax_entries
            .into_iter()
            .map(|(syntax, type_id)| {
                let type_id = owner
                    .and_then(|owner| self.types.specialized_type_for_syntax(owner, syntax))
                    .unwrap_or(type_id);
                let specialized = self
                    .types
                    .types_mut()
                    .substitute_template_parameters(type_id, &substitutions)
                    .expect("specialized syntax type belongs to the program store");
                (syntax, specialized)
            })
            .collect();

        self.active_specializations.push(key);
        let previous_owner = self.current_specialized_owner;
        self.current_specialized_owner = owner.or(previous_owner);
        let previous_substitutions = self
            .current_template_substitutions
            .replace(substitutions);
        let previous_syntax = self
            .current_template_syntax_types
            .replace(specialized_syntax);
        let parent_checking = std::mem::take(&mut self.checking);
        let parent_calls = std::mem::take(&mut self.runtime_specialization_calls);
        self.visit_function(function);
        let mut local_checking = std::mem::take(&mut self.checking);
        Self::sort_checking_diagnostics(&mut local_checking);
        let local_calls = std::mem::take(&mut self.runtime_specialization_calls);
        self.checking = parent_checking;
        for error in &local_checking.errors {
            if !self.checking.errors.contains(error) {
                self.checking.errors.push(error.clone());
            }
        }
        self.runtime_specialization_calls = parent_calls;
        self.current_template_substitutions = previous_substitutions;
        self.current_template_syntax_types = previous_syntax;
        self.current_specialized_owner = previous_owner;
        self.active_specializations.pop();

        local_checking.runtime_specialization_calls = local_calls;
        self.runtime_specializations[id.0].checking = Box::new(local_checking);
        Some(id)
    }

    fn type_contains_template_parameter(&self, type_id: TypeId) -> bool {
        match self.types.types().get(type_id) {
            Some(SemanticType::TemplateParameter { .. }) => true,
            Some(
                SemanticType::Gc { target, .. }
                | SemanticType::Tracked { target, .. },
            ) => {
                self.type_contains_template_parameter(*target)
            }
            Some(SemanticType::Callable {
                parameters,
                return_type,
                ..
            }) => parameters
                .iter()
                .copied()
                .any(|parameter| self.type_contains_template_parameter(parameter))
                || self.type_contains_template_parameter(*return_type),
            Some(SemanticType::Tuple { elements, .. }) => elements
                .iter()
                .copied()
                .any(|element| self.type_contains_template_parameter(element)),
            Some(
                SemanticType::GeneratedStruct { arguments, .. }
                | SemanticType::Builtin { arguments, .. },
            ) => arguments
                .iter()
                .copied()
                .any(|argument| self.type_contains_template_parameter(argument)),
            Some(
                SemanticType::Union { members, .. }
                | SemanticType::Intersection { members, .. },
            ) => members
                .iter()
                .copied()
                .any(|member| self.type_contains_template_parameter(member)),
            _ => false,
        }
    }

    /// Interface constraints are implemented by concrete object method tables.
    /// An unconstrained `T: type` does not use this restriction and may be a
    /// primitive, callable, union, built-in, or any other valid semantic type.
    fn runtime_template_constraint_source_is_concrete(&self, type_id: TypeId) -> bool {
        match self.types.types().get(type_id) {
            Some(
                SemanticType::NamedStruct { .. }
                | SemanticType::GeneratedStruct { .. }
                | SemanticType::TemplateParameter { .. },
            ) => true,
            Some(SemanticType::Gc { target, .. }) => matches!(
                self.types.types().get(*target),
                Some(
                    SemanticType::NamedStruct { .. }
                        | SemanticType::GeneratedStruct { .. }
                        | SemanticType::TemplateParameter { .. }
                )
            ),
            _ => false,
        }
    }

    fn analyze_runtime_template_arguments(
        &mut self,
        call: &Expression,
        arguments: &[Expression],
        parameters: &[TypeId],
    ) -> Option<bool> {
        let mut valid = arguments.len() == parameters.len();
        let mut supported = true;
        let mut tracked_inputs = Vec::new();
        let mut borrow_containing_inputs = Vec::new();
        for (index, argument) in arguments.iter().enumerate() {
            let Some(expected) = parameters.get(index).copied() else {
                supported &= self.synthesize(argument).is_some();
                continue;
            };
            let Some(checked) = self.check_call_argument(argument, expected) else {
                supported = false;
                continue;
            };
            if self.is_recovery(checked.type_id) {
                valid = false;
            } else {
                if self.tracked_reference_parts(expected).is_some() {
                    for source in self.tracked_input_lifetime_sources(argument) {
                        if !tracked_inputs.contains(&source) {
                            tracked_inputs.push(source);
                        }
                    }
                } else if self.type_contains_tracked_reference(expected) {
                    for source in self.tracked_input_lifetime_sources(argument) {
                        if !borrow_containing_inputs.contains(&source) {
                            borrow_containing_inputs.push(source);
                        }
                    }
                }
                if let Some(transfer) = self.argument_transfer(checked) {
                    self.checking.transfers.insert(argument.id, transfer);
                }
            }
        }
        self.checking
            .tracked_call_inputs
            .insert(call.id, tracked_inputs);
        self.checking
            .borrow_containing_call_inputs
            .insert(call.id, borrow_containing_inputs);
        supported.then_some(valid)
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
        if self.builtin_namespace_reference(object).is_some() {
            let typed_callee = match self.synthesize(callee) {
                Some(typed) => typed,
                None => return Some(None),
            };
            if self.is_recovery(typed_callee.type_id) {
                for argument in arguments {
                    let _ = self.synthesize(argument);
                }
                return Some(Some(self.recovery_temporary()));
            }
            let Some(SemanticType::Callable {
                parameters,
                return_type,
                ..
            }) = self.types.types().get(typed_callee.type_id).cloned()
            else {
                unreachable!("a resolved namespace member is callable")
            };
            let arguments_valid =
                match self.analyze_call_arguments(call, arguments, &parameters) {
                    Some(valid) => valid,
                    None => return Some(None),
                };
            if !arguments_valid {
                return Some(Some(self.recovery_temporary()));
            }
            return Some(Some(self.call_result(call, return_type, None)));
        }
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
        let name = self
            .module
            .text(*member)
            .expect("member name belongs to the source module")
            .to_string();
        if let Some((sequence, _, _)) = self.sequence_parts(typed_object.type_id) {
            return Some(self.synthesize_sequence_member_call(
                call,
                callee,
                object,
                typed_object,
                *member,
                &name,
                sequence,
                arguments,
            ));
        }
        if let Some((type_arguments, object_capability, is_gc)) =
            self.queue_parts(typed_object.type_id)
        {
            return Some(self.synthesize_queue_member_call(
                call,
                callee,
                object,
                typed_object,
                *member,
                &name,
                &type_arguments,
                object_capability,
                is_gc,
                arguments,
            ));
        }
        if let Some((elements, _, _)) = self.tuple_parts(typed_object.type_id) {
            if name.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            if name != "copy" {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::UnknownMember,
                    span: *member,
                });
                for argument in arguments {
                    let _ = self.synthesize(argument);
                }
                return Some(Some(self.recovery_temporary()));
            }
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
            let type_id = self
                .types
                .types_mut()
                .tuple(elements, AccessCapability::Mut);
            let sources = self.tracked_lifetime_sources(object);
            self.record_tracked_lifetime_link(call.id, sources);
            return Some(Some(TypedExpression {
                type_id,
                category: ValueCategory::FreshTemporary,
            }));
        }
        let aggregate = self.aggregate_parts(typed_object.type_id);
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
            let sources = self.tracked_lifetime_sources(object);
            self.record_tracked_lifetime_link(call.id, sources);
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
                    | StructMemberSignatureKind::AssociatedTypeFactory { .. }
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
        if self.signatures.is_runtime_template(declaration)
            && let Some(request) = self
                .types
                .runtime_member_template_call(call.id)
                .cloned()
        {
            let signature = match owner {
                AggregateOwner::Source(_) => self.signatures.callable(declaration),
                AggregateOwner::Generated(owner) => self
                    .signatures
                    .specialized_callable(owner, declaration)
                    .or_else(|| self.runtime_generated_callables.get(&(owner, declaration))),
            }
            .expect("method template signature must have been collected")
            .clone();
            return Some(self.synthesize_runtime_method_template_call(
                call,
                callee,
                object,
                typed_object,
                owner,
                object_capability,
                is_gc,
                declaration,
                method_id,
                signature,
                &request,
                arguments,
            ));
        }
        if self.signatures.is_runtime_template(declaration) {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::TemplateRequiresSpecialization,
                span: *member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(Some(self.recovery_temporary()));
        }
        let signature = match owner {
            AggregateOwner::Source(_) => self.signatures.callable(declaration),
            AggregateOwner::Generated(owner) => self
                .signatures
                .specialized_callable(owner, declaration)
                .or_else(|| self.runtime_generated_callables.get(&(owner, declaration))),
        }
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
        Some(Some(self.call_result(
            call,
            signature.return_type,
            self.receiver_lifetime_source(
                receiver,
                object,
                typed_object.type_id,
                signature.return_type,
            ),
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn synthesize_queue_member_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        member: Span,
        name: &str,
        type_arguments: &[TypeId],
        object_capability: AccessCapability,
        is_gc: bool,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let selected = self
            .signatures
            .builtins()
            .member(
                BuiltinMemberOwner::Parameterized(BuiltinType::Queue),
                name,
            )
            .cloned();
        let Some(BuiltinMemberSignature::Callable(template)) = selected else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::UnknownMember,
                span: member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        };
        let Some(receiver) = template.receiver else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                span: member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        };
        let signature = template
            .instantiate(type_arguments, self.types.types_mut())
            .expect("resolved Queue application supplies one element type");
        let receiver_valid = self.check_method_receiver(
            object,
            typed_object,
            receiver,
            object_capability,
            is_gc,
        );
        let [element_type] = type_arguments else {
            unreachable!("resolved Queue applications have one element type")
        };

        let (kind, arguments_valid, element_transfer, receive_union) = match name {
            "send" => {
                let (valid, transfer) =
                    self.analyze_queue_send_argument(call, arguments, *element_type)?;
                (QueueOperationKind::Send, valid, transfer, None)
            }
            "try_receive" => {
                let valid = self.analyze_call_arguments(call, arguments, &signature.parameters)?;
                let members = self
                    .union_members(signature.return_type)
                    .expect("Queue.try_receive returns an element-or-none union");
                let none = self
                    .types
                    .types_mut()
                    .primitive(PrimitiveType::None, AccessCapability::Const);
                let element_member = members
                    .iter()
                    .position(|member| *member == *element_type)
                    .expect("Queue receive union contains its element type");
                let none_member = members
                    .iter()
                    .position(|member| *member == none)
                    .expect("Queue receive union contains none");
                (
                    QueueOperationKind::TryReceive,
                    valid,
                    None,
                    Some(QueueReceiveUnion {
                        type_id: signature.return_type,
                        element_member,
                        none_member,
                    }),
                )
            }
            _ => unreachable!("the Queue catalogue exposes only known methods"),
        };
        self.checking.resolved_queue_operations.insert(
            callee.id,
            ResolvedQueueOperation {
                kind,
                queue_type: typed_object.type_id,
                element_type: *element_type,
                receiver_transfer: receiver_valid.then_some(ValueTransfer::Borrow),
                element_transfer,
                receive_union,
            },
        );
        if !receiver_valid || !arguments_valid || self.is_recovery(signature.return_type) {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(call, signature.return_type, None))
    }

    /// Queue elements have already been restricted by type resolution to
    /// trivial inline values or stable GC references. Sending therefore never
    /// retains an ordinary by-reference object argument in external storage.
    fn analyze_queue_send_argument(
        &mut self,
        call: &Expression,
        arguments: &[Expression],
        element_type: TypeId,
    ) -> Option<(bool, Option<ValueTransfer>)> {
        let arity_matches = arguments.len() == 1;
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: 1,
                    found: arguments.len(),
                },
                span: call.span,
            });
        }
        let mut valid = arity_matches;
        let mut element_transfer = None;
        if let Some(argument) = arguments.first() {
            let checked = self.check_call_argument(argument, element_type)?;
            if self.is_recovery(checked.type_id) {
                valid = false;
            } else {
                let semantic = self
                    .types
                    .types()
                    .get(checked.type_id)
                    .expect("Queue element type belongs to the program type store");
                let transfer = if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
                    ValueTransfer::CopyGcReference
                } else if semantic.copy_semantics() == Some(CopySemantics::Trivial) {
                    ValueTransfer::TrivialCopy
                } else {
                    unreachable!("Queue elements are validated during type resolution")
                };
                self.checking.transfers.insert(argument.id, transfer);
                element_transfer = Some(transfer);
            }
        }
        for surplus in arguments.get(1..).unwrap_or(&[]) {
            let _ = self.synthesize(surplus);
        }
        self.checking.tracked_call_inputs.insert(call.id, Vec::new());
        self.checking
            .borrow_containing_call_inputs
            .insert(call.id, Vec::new());
        Some((valid, element_transfer))
    }

    #[allow(clippy::too_many_arguments)]
    fn synthesize_runtime_method_template_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        owner: AggregateOwner,
        object_capability: AccessCapability,
        is_gc: bool,
        declaration: NodeId,
        method_id: MethodId,
        base_signature: CallableSignature,
        request: &RuntimeMemberTemplateCall,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let function = self
            .runtime_templates
            .get(&declaration)
            .expect("runtime method template declaration must be indexed")
            .clone();
        let owner_type = self
            .aggregate_signature(owner)
            .expect("runtime method template has a concrete aggregate owner")
            .type_id;
        self.method_owners.insert(declaration, owner_type);
        let mut concrete_arguments = Vec::new();
        for argument in request.type_arguments.iter().copied() {
            let concrete = if let Some(substitutions) = self.current_template_substitutions.clone()
            {
                self.types
                    .types_mut()
                    .substitute_template_parameters(argument, &substitutions)
                    .expect("method template argument belongs to the program type store")
            } else {
                argument
            };
            concrete_arguments.push(concrete);
        }
        let type_parameters: Vec<_> = function
            .parameters
            .iter()
            .filter(|parameter| {
                matches!(&parameter.kind, FunctionParameterKind::Comptime { .. })
            })
            .collect();
        let runtime_arguments = arguments
            .get(request.comptime_argument_count..)
            .unwrap_or(&[]);
        let expected_total = type_parameters.len() + base_signature.parameters.len();
        let arity_matches = expected_total == arguments.len()
            && concrete_arguments.len() == type_parameters.len();
        if !arity_matches {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::ArgumentCountMismatch {
                    expected: expected_total,
                    found: arguments.len(),
                },
                span: call.span,
            });
        }

        let mut constraints_valid = concrete_arguments.len() == type_parameters.len();
        for (index, (parameter, concrete)) in type_parameters
            .iter()
            .zip(concrete_arguments.iter().copied())
            .enumerate()
        {
            let symbolic = self
                .types
                .types_mut()
                .template_parameter(parameter.id, AccessCapability::Const);
            let Some(bound) = self
                .types
                .template_parameter_bound_for(Some(owner_type), symbolic)
                .flatten()
            else {
                continue;
            };
            if !self.runtime_template_constraint_source_is_concrete(concrete) {
                constraints_valid = false;
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::InvalidRuntimeTemplateArgument {
                        found: concrete,
                    },
                    span: arguments
                        .get(index)
                        .map_or(call.span, |argument| argument.span),
                });
                continue;
            }
            let category = if matches!(
                self.types.types().get(concrete),
                Some(SemanticType::Gc { .. })
            ) {
                ValueCategory::GcReference
            } else {
                ValueCategory::BorrowedPlace
            };
            if let Err(kind) = self.validate_interface_view_source(
                TypedExpression {
                    type_id: concrete,
                    category,
                },
                bound,
                None,
            ) {
                constraints_valid = false;
                self.checking.errors.push(ExpressionCheckingError {
                    kind,
                    span: arguments
                        .get(index)
                        .map_or(call.span, |argument| argument.span),
                });
            }
        }

        let substitutions: HashMap<_, _> = type_parameters
            .iter()
            .zip(concrete_arguments.iter().copied())
            .map(|(parameter, concrete)| (parameter.id, concrete))
            .collect();
        let signature = self.substitute_callable_signature(base_signature, &substitutions);
        let symbolic_request = concrete_arguments
            .iter()
            .copied()
            .any(|argument| self.type_contains_template_parameter(argument));
        let specialization = if constraints_valid && arity_matches && !symbolic_request {
            self.request_runtime_specialization(
                Some(owner_type),
                &function,
                concrete_arguments,
                signature.clone(),
                call.span,
            )
        } else {
            None
        };
        if let Some(specialization) = specialization {
            self.runtime_specialization_calls
                .insert(call.id, specialization);
        }

        let receiver = signature
            .receiver
            .expect("runtime method template must have a receiver signature");
        let receiver_valid =
            self.check_method_receiver(object, typed_object, receiver, object_capability, is_gc);
        self.checking.resolved_members.insert(
            callee.id,
            ResolvedMember::Method {
                declaration,
                method_id,
            },
        );
        let runtime_valid = self.analyze_runtime_template_arguments(
            call,
            runtime_arguments,
            &signature.parameters,
        )?;
        if (!symbolic_request && specialization.is_none())
            || !arity_matches
            || !constraints_valid
            || !receiver_valid
            || !runtime_valid
            || self.is_recovery(signature.return_type)
        {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(
            call,
            signature.return_type,
            self.receiver_lifetime_source(
                receiver,
                object,
                typed_object.type_id,
                signature.return_type,
            ),
        ))
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
            ReceiverStorage::Gc => ValueTransfer::CopyGcReference,
            ReceiverStorage::Plain | ReceiverStorage::Tracked => ValueTransfer::Borrow,
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
        Some(self.call_result(
            call,
            signature.return_type,
            (receiver.storage == ReceiverStorage::Tracked).then_some(object),
        ))
    }

    /// Checks the compiler-provided `length()` method on strings and bytes.
    /// Sequence methods are resolved before aggregate lookup because primitive
    /// receivers have no source declaration or `MethodId`.
    fn synthesize_sequence_member_call(
        &mut self,
        call: &Expression,
        callee: &Expression,
        object: &Expression,
        typed_object: TypedExpression,
        member: Span,
        name: &str,
        sequence: SequenceKind,
        arguments: &[Expression],
    ) -> Option<TypedExpression> {
        let primitive = match sequence {
            SequenceKind::String => PrimitiveType::String,
            SequenceKind::Bytes => PrimitiveType::Bytes,
        };
        let owner = BuiltinMemberOwner::Primitive(primitive);
        let selected = self.signatures.builtins().member(owner, name).cloned();
        let Some(BuiltinMemberSignature::Callable(template)) = selected else {
            self.checking.errors.push(ExpressionCheckingError {
                kind: if name == "concat" {
                    ExpressionCheckingErrorKind::AssociatedFunctionRequiresType
                } else {
                    ExpressionCheckingErrorKind::UnknownMember
                },
                span: member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        };
        if template.receiver.is_none() {
            self.checking.errors.push(ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::AssociatedFunctionRequiresType,
                span: member,
            });
            for argument in arguments {
                let _ = self.synthesize(argument);
            }
            return Some(self.recovery_temporary());
        }
        let signature = template
            .instantiate(&[], self.types.types_mut())
            .expect("primitive sequence member has no type substitutions");
        let arguments_valid = self.analyze_call_arguments(call, arguments, &signature.parameters)?;
        self.checking.transfers.insert(
            object.id,
            if typed_object.category == ValueCategory::GcReference {
                ValueTransfer::CopyGcReference
            } else {
                ValueTransfer::Borrow
            },
        );
        self.checking.resolved_sequence_operations.insert(
            callee.id,
            ResolvedSequenceOperation::Length { sequence },
        );
        if !arguments_valid {
            return Some(self.recovery_temporary());
        }
        Some(self.call_result(call, signature.return_type, None))
    }

    /// Validates the hidden receiver supplied by a direct method call and
    /// records how it is passed. Plain methods use `Borrow` regardless of the
    /// object's storage class; `&self` methods copy a GC reference. A fresh
    /// plain temporary may independently select mut access.
    fn check_method_receiver(
        &mut self,
        object: &Expression,
        typed_object: TypedExpression,
        receiver: ReceiverSignature,
        object_capability: AccessCapability,
        is_gc: bool,
    ) -> bool {
        let storage_valid = match receiver.storage {
            ReceiverStorage::Plain | ReceiverStorage::Tracked => true,
            ReceiverStorage::Gc => is_gc,
        };
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
                ReceiverStorage::Gc => ValueTransfer::CopyGcReference,
                ReceiverStorage::Plain | ReceiverStorage::Tracked => ValueTransfer::Borrow,
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
        let mut tracked_inputs = Vec::new();
        let mut borrow_containing_inputs = Vec::new();
        for (index, argument) in arguments.iter().enumerate() {
            let Some(expected) = parameters.get(index).copied() else {
                all_supported &= self.synthesize(argument).is_some();
                continue;
            };
            let Some(checked) = self.check_call_argument(argument, expected) else {
                all_supported = false;
                continue;
            };
            if self.is_recovery(checked.type_id) {
                valid = false;
                continue;
            }
            if self.tracked_reference_parts(expected).is_some() {
                for source in self.tracked_input_lifetime_sources(argument) {
                    if !tracked_inputs.contains(&source) {
                        tracked_inputs.push(source);
                    }
                }
            } else if self.type_contains_tracked_reference(expected) {
                for source in self.tracked_input_lifetime_sources(argument) {
                    if !borrow_containing_inputs.contains(&source) {
                        borrow_containing_inputs.push(source);
                    }
                }
            }
            if let Some(transfer) = self.argument_transfer(checked) {
                self.checking.transfers.insert(argument.id, transfer);
            }
        }
        self.checking
            .tracked_call_inputs
            .insert(call.id, tracked_inputs);
        self.checking
            .borrow_containing_call_inputs
            .insert(call.id, borrow_containing_inputs);
        all_supported.then_some(valid)
    }

    /// Checks one actual argument with the one dereference rule that is
    /// specific to the calling convention: `*T` may provide the address for a
    /// plain aggregate parameter `T`. No general expected-type boundary uses
    /// this path, so bindings, fields, and returns still reject `*T -> T`.
    fn check_call_argument(
        &mut self,
        argument: &Expression,
        expected: TypeId,
    ) -> Option<TypedExpression> {
        if matches!(
            &argument.kind,
            ExpressionKind::Tuple { .. }
                | ExpressionKind::Block(_)
                | ExpressionKind::If { .. }
                | ExpressionKind::Loop { .. }
                | ExpressionKind::While { .. }
                | ExpressionKind::RangeFor { .. }
        ) {
            return self.check(argument, expected);
        }
        if let ExpressionKind::Group(inner) = &argument.kind {
            let checked = self.check_call_argument(inner, expected)?;
            if let Some(borrow) = self
                .checking
                .tracked_parameter_borrows
                .remove(&inner.id)
            {
                self.checking
                    .tracked_parameter_borrows
                    .insert(argument.id, borrow);
            }
            if let Some(place) = self.checking.physical_places.get(&inner.id).cloned() {
                self.checking.physical_places.insert(argument.id, place);
            }
            if let Some(link) = self
                .checking
                .tracked_lifetime_links
                .get(&inner.id)
                .cloned()
            {
                self.checking
                    .tracked_lifetime_links
                    .insert(argument.id, link);
            }
            self.checking.expressions.insert(argument.id, checked);
            let explicitly_produces_value = self.explicitly_produces_value(inner);
            self.checking
                .explicit_values
                .insert(argument.id, explicitly_produces_value);
            return Some(checked);
        }
        let found = self.synthesize(argument)?;
        if self.is_recovery(found.type_id) || self.is_divergence(found.type_id) {
            return Some(found);
        }
        if let Some((target, source_capability)) =
            self.tracked_reference_parts(found.type_id)
            && self.plain_parameter_is_passed_by_reference(expected)
        {
            let destination_capability = self
                .types
                .types()
                .get(expected)
                .and_then(SemanticType::capability)
                .expect("plain parameter type has an access capability");
            let same_target = self
                .types
                .types()
                .has_same_shape(target, expected)
                .expect("call argument types belong to the program type store");
            let capability_valid = !matches!(
                (source_capability, destination_capability),
                (AccessCapability::Const, AccessCapability::Mut)
            );
            if same_target && capability_valid {
                self.checking.tracked_parameter_borrows.insert(
                    argument.id,
                    TrackedParameterBorrow {
                        source_type: found.type_id,
                        parameter_type: expected,
                    },
                );
                let checked = TypedExpression {
                    type_id: expected,
                    category: ValueCategory::BorrowedPlace,
                };
                self.checking.expressions.insert(argument.id, checked);
                return Some(checked);
            }
        }
        self.check_typed(argument, expected, found, false)
    }

    fn plain_parameter_is_passed_by_reference(&self, type_id: TypeId) -> bool {
        self.types.types().get(type_id).is_some_and(|semantic| {
            semantic.storage_semantics() == Some(StorageSemantics::Inline)
                && semantic.copy_semantics() == Some(CopySemantics::Recursive)
        })
    }

    /// Gives all successful ordinary calls their declared result type. GC,
    /// tracked, and transitively borrow-containing results preserve their
    /// storage provenance; other results are fresh callee-supplied values.
    fn call_result(
        &mut self,
        call: &Expression,
        return_type: TypeId,
        tracked_receiver: Option<&Expression>,
    ) -> TypedExpression {
        if !self.validate_borrow_storage_type(return_type, call.span) {
            self.checking.tracked_call_inputs.remove(&call.id);
            self.checking.borrow_containing_call_inputs.remove(&call.id);
            return self.recovery_temporary();
        }
        let category = match self
            .types
            .types()
            .get(return_type)
            .and_then(SemanticType::storage_semantics)
        {
            Some(StorageSemantics::Gc) => ValueCategory::GcReference,
            Some(StorageSemantics::TrackedReference) => ValueCategory::BorrowedPlace,
            _ => ValueCategory::FreshTemporary,
        };
        if self.type_contains_tracked_reference(return_type) {
            let direct_tracked_result = self.tracked_reference_parts(return_type).is_some();
            let mut sources = self
                .checking
                .tracked_call_inputs
                .remove(&call.id)
                .unwrap_or_default();
            for source in &mut sources {
                source
                    .projections
                    .push(PhysicalPlaceProjection::OpaqueDerived);
            }
            if !direct_tracked_result {
                for source in self
                    .checking
                    .borrow_containing_call_inputs
                    .remove(&call.id)
                    .unwrap_or_default()
                {
                    if !sources.contains(&source) {
                        sources.push(source);
                    }
                }
            } else {
                self.checking.borrow_containing_call_inputs.remove(&call.id);
            }
            if let Some(receiver) = tracked_receiver {
                for mut source in self.tracked_input_lifetime_sources(receiver) {
                    source
                        .projections
                        .push(PhysicalPlaceProjection::OpaqueDerived);
                    if !sources.contains(&source) {
                        sources.push(source);
                    }
                }
            }
            if sources.len() == 1 {
                self.checking
                    .physical_places
                    .insert(call.id, sources[0].clone());
            }
            self.checking
                .tracked_lifetime_links
                .insert(call.id, TrackedLifetimeLink { sources });
        } else {
            self.checking.tracked_call_inputs.remove(&call.id);
            self.checking.borrow_containing_call_inputs.remove(&call.id);
        }
        TypedExpression {
            type_id: return_type,
            category,
        }
    }

    fn receiver_lifetime_source<'expression>(
        &self,
        receiver: ReceiverSignature,
        object: &'expression Expression,
        object_type: TypeId,
        return_type: TypeId,
    ) -> Option<&'expression Expression> {
        if receiver.storage == ReceiverStorage::Tracked {
            return Some(object);
        }
        (self.tracked_reference_parts(return_type).is_none()
            && self.type_contains_tracked_reference(return_type)
            && self.type_contains_tracked_reference(object_type))
        .then_some(object)
    }

    fn primitive_kind(&self, type_id: TypeId) -> Option<PrimitiveType> {
        match self.types.types().get(type_id) {
            Some(SemanticType::Primitive { primitive, .. }) => Some(*primitive),
            _ => None,
        }
    }

    /// Checks each interpolation exactly once in source order and normalizes
    /// its optional Python-compatible format specification for lowering.
    fn synthesize_formatted_string(
        &mut self,
        expression: &Expression,
        parts: &[FormattedStringPart],
    ) -> Option<TypedExpression> {
        let mut resolved = Vec::new();
        let mut valid = true;

        for part in parts {
            let FormattedStringPart::Interpolation {
                value,
                format_spec,
                ..
            } = part
            else {
                continue;
            };
            let typed = self.synthesize(value)?;
            if self.is_divergence(typed.type_id) {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::DivergentFormattedValue,
                    span: value.span,
                });
                valid = false;
            }

            let format = match format_spec {
                Some(span) => {
                    let source = self
                        .module
                        .text(*span)
                        .expect("format specification belongs to the source module");
                    match parse_format_specification(source) {
                        Some(format) => format,
                        None => {
                            self.checking.errors.push(ExpressionCheckingError {
                                kind: ExpressionCheckingErrorKind::InvalidFormatSpecification,
                                span: *span,
                            });
                            valid = false;
                            FormatSpecification::default()
                        }
                    }
                }
                None => FormatSpecification::default(),
            };

            if !self.is_recovery(typed.type_id) && !self.is_divergence(typed.type_id) {
                if !self.formatted_value_is_supported(typed.type_id) {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::UnsupportedFormattedValue {
                            found: typed.type_id,
                        },
                        span: value.span,
                    });
                    valid = false;
                } else if !self.formatted_value_supports_specification(typed.type_id, format) {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::InvalidFormatSpecification,
                        span: format_spec.unwrap_or(value.span),
                    });
                    valid = false;
                }
            } else if self.is_recovery(typed.type_id) {
                valid = false;
            }

            resolved.push(ResolvedInterpolation {
                value: value.id,
                value_type: typed.type_id,
                format,
            });
        }

        if !valid {
            return Some(self.recovery_temporary());
        }
        self.checking
            .formatted_strings
            .insert(expression.id, resolved);
        Some(TypedExpression {
            type_id: self
                .types
                .types_mut()
                .primitive(PrimitiveType::String, AccessCapability::Mut),
            category: ValueCategory::FreshTemporary,
        })
    }

    fn formatted_value_is_supported(&self, type_id: TypeId) -> bool {
        self.primitive_kind(type_id).is_some_and(|primitive| {
            matches!(
                primitive,
                PrimitiveType::String
                    | PrimitiveType::Int
                    | PrimitiveType::Float
                    | PrimitiveType::Bool
                    | PrimitiveType::Char
                    | PrimitiveType::Unit
                    | PrimitiveType::None
            )
        })
    }

    fn formatted_value_supports_specification(
        &self,
        type_id: TypeId,
        format: FormatSpecification,
    ) -> bool {
        let Some(primitive) = self.primitive_kind(type_id) else {
            return false;
        };
        let numeric = matches!(primitive, PrimitiveType::Int | PrimitiveType::Float);
        let precision_valid = format.fixed_precision.is_none() || primitive == PrimitiveType::Float;
        let numeric_options_valid = numeric
            || (format.sign.is_none() && !format.zero_padding);
        precision_valid && numeric_options_valid
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

    fn builtin_namespace_reference(&self, expression: &Expression) -> Option<BuiltinNamespace> {
        if !matches!(&expression.kind, ExpressionKind::Identifier) {
            return None;
        }
        let symbol = self.names.symbol_for_reference(expression.id)?;
        if self.names.symbols().symbol(symbol)?.kind != SymbolKind::BuiltinValue {
            return None;
        }
        let name = self.module.text(expression.span).ok()?;
        match self.signatures.builtins().global(name) {
            Some(BuiltinGlobalSignature::Namespace(namespace)) => Some(*namespace),
            Some(BuiltinGlobalSignature::Callable(_)) | None => None,
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
            let narrowing_place = NarrowingPlace {
                root: NarrowingRoot::Symbol(symbol),
                fields: Vec::new(),
            };
            let type_id = self
                .effective_narrowing(&narrowing_place)
                .map_or(binding.type_id, |fact| fact.narrowed_type);
            let typed = TypedExpression {
                type_id,
                category,
            };
            self.checking.places.insert(
                expression.id,
                Place {
                    symbol: Some(symbol),
                    declared_type_id: binding.type_id,
                    type_id,
                    category,
                    binding_mutability: Some(binding.qualifiers.binding),
                    value_capability: binding.qualifiers.value,
                },
            );
            self.checking.physical_places.insert(
                expression.id,
                PhysicalPlace {
                    root: PhysicalPlaceRoot::Symbol(symbol),
                    projections: Vec::new(),
                    storage: category,
                },
            );
            if let Some(link) = self.current_tracked_bindings.get(&symbol).cloned()
            {
                self.checking
                    .tracked_lifetime_links
                    .insert(expression.id, link);
            }
            return Some(typed);
        }
        if self
            .names
            .symbols()
            .symbol(symbol)
            .is_some_and(|symbol| symbol.kind == SymbolKind::BuiltinValue)
        {
            let name = self
                .module
                .text(expression.span)
                .expect("built-in identifier belongs to the source module");
            match self.signatures.builtins().global(name) {
                Some(BuiltinGlobalSignature::Namespace(_)) => {
                    self.checking.errors.push(ExpressionCheckingError {
                        kind: ExpressionCheckingErrorKind::NamespaceRequiresMember,
                        span: expression.span,
                    });
                    return Some(self.recovery_temporary());
                }
                Some(BuiltinGlobalSignature::Callable(_)) => {
                    let operation = match name {
                        "print" => ResolvedBuiltinOperation::Output {
                            mode: OutputMode::Print,
                        },
                        "println" => ResolvedBuiltinOperation::Output {
                            mode: OutputMode::PrintLine,
                        },
                        "panic" => ResolvedBuiltinOperation::Panic,
                        "yield" => ResolvedBuiltinOperation::Yield,
                        _ => unreachable!("the global built-in catalogue contains known names"),
                    };
                    self.checking
                        .resolved_builtin_operations
                        .insert(expression.id, operation);
                }
                None => unreachable!("prelude built-in symbol must have a catalogue entry"),
            }
        }
        let Some(type_id) = self.signatures.callable_value_type(symbol) else {
            if self
                .names
                .symbols()
                .symbol(symbol)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Function)
            {
                self.checking.errors.push(ExpressionCheckingError {
                    kind: ExpressionCheckingErrorKind::TemplateRequiresSpecialization,
                    span: expression.span,
                });
                return Some(self.recovery_temporary());
            }
            return None;
        };
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
        let declared_owner = *self
            .method_owners
            .get(&method)
            .expect("named method must have a recorded owner type");
        let owner = self.current_specialized_owner.unwrap_or(declared_owner);
        let receiver = self
            .current_specialized_owner
            .and_then(|owner| self.signatures.specialized_callable(owner, method))
            .or_else(|| self.signatures.callable(method))
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
            ReceiverStorage::Gc => TypedExpression {
                type_id: self
                    .types
                    .types_mut()
                    .gc(owner)
                    .expect("method owner is a value type"),
                category: ValueCategory::GcReference,
            },
            ReceiverStorage::Tracked => TypedExpression {
                type_id: self
                    .types
                    .types_mut()
                    .tracked(owner)
                    .expect("method owner is a value type"),
                category: ValueCategory::BorrowedPlace,
            },
        };
        self.checking.places.insert(
            expression.id,
            Place {
                symbol: None,
                declared_type_id: typed.type_id,
                type_id: typed.type_id,
                category: typed.category,
                binding_mutability: None,
                value_capability: match receiver.capability {
                    AccessCapability::Const => ValueCapability::Const,
                    AccessCapability::Mut => ValueCapability::Mut,
                },
            },
        );
        self.checking.physical_places.insert(
            expression.id,
            PhysicalPlace {
                root: PhysicalPlaceRoot::SelfValue(method),
                projections: Vec::new(),
                storage: typed.category,
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
            Some(StorageSemantics::Gc) => ValueCategory::GcReference,
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
        if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            return (
                ValueCategory::GcReference,
                Some(ValueTransfer::CopyGcReference),
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
        if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            return (
                ValueCategory::GcReference,
                ValueTransfer::CopyGcReference,
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
        let mut edges: HashMap<TypeId, Vec<(TypeId, LayoutField)>> = HashMap::new();
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
            let members: HashSet<TypeId> = component.iter().copied().collect();
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

    fn validate_borrow_storage_fields(&mut self) {
        let violations: Vec<_> = self
            .aggregate_order
            .iter()
            .filter_map(|owner| self.aggregate_layouts.get(owner))
            .flat_map(|layout| layout.fields.iter())
            .filter_map(|field| {
                self.borrow_storage_violation(field.type_id)
                    .map(|violation| (violation, field.span))
            })
            .collect();
        for (violation, span) in violations {
            self.push_borrow_storage_violation(violation, span);
        }
    }

    fn validate_borrow_storage_type(&mut self, type_id: TypeId, span: Span) -> bool {
        let Some(violation) = self.borrow_storage_violation(type_id) else {
            return true;
        };
        self.push_borrow_storage_violation(violation, span);
        false
    }

    fn push_borrow_storage_violation(
        &mut self,
        violation: BorrowStorageViolation,
        span: Span,
    ) {
        let kind = match violation {
            BorrowStorageViolation::Gc(found) => {
                ExpressionCheckingErrorKind::BorrowContainingGcStorage { found }
            }
            BorrowStorageViolation::ExternalBuffer(found) => {
                ExpressionCheckingErrorKind::BorrowContainingExternalBuffer { found }
            }
        };
        self.checking
            .errors
            .push(ExpressionCheckingError { kind, span });
    }

    fn inline_aggregate_dependencies(
        &self,
        type_id: TypeId,
        dependencies: &mut Vec<TypeId>,
        visited: &mut HashSet<TypeId>,
    ) {
        if !visited.insert(type_id) {
            return;
        }
        match self.types.types().get(type_id) {
            Some(SemanticType::NamedStruct { declaration, .. }) => {
                let dependency = self
                    .types
                    .type_for_declaration(*declaration)
                    .expect("named aggregate must have a declared type");
                if !dependencies.contains(&dependency) {
                    dependencies.push(dependency);
                }
            }
            Some(SemanticType::AnonymousStruct { expression, .. }) => {
                if let Some(dependency) = self.aggregate_layouts.keys().copied().find(|candidate| {
                    matches!(
                        self.types.types().get(*candidate),
                        Some(SemanticType::AnonymousStruct { expression: candidate, .. })
                            if candidate == expression
                    )
                }) && !dependencies.contains(&dependency) {
                    dependencies.push(dependency);
                }
            }
            Some(SemanticType::GeneratedStruct { .. }) => {
                if let Some(dependency) = self.aggregate_layouts.keys().copied().find(|candidate| {
                    self.types.types().has_same_shape(*candidate, type_id) == Some(true)
                }) && !dependencies.contains(&dependency) {
                    dependencies.push(dependency);
                }
            }
            Some(SemanticType::Union { members, .. }) => {
                for member in members {
                    self.inline_aggregate_dependencies(*member, dependencies, visited);
                }
            }
            Some(SemanticType::Tuple { elements, .. }) => {
                for element in elements {
                    self.inline_aggregate_dependencies(*element, dependencies, visited);
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
                SemanticType::Gc { .. }
                | SemanticType::Tracked { .. }
                | SemanticType::Primitive { .. }
                | SemanticType::Callable { .. }
                | SemanticType::Interface { .. }
                | SemanticType::TemplateParameter { .. }
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
        if semantic.storage_semantics() == Some(StorageSemantics::Gc) {
            return Some(ValueTransfer::CopyGcReference);
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

    /// Returns a callable header in the currently active specialization. The
    /// declaration collector owns the symbolic header; concrete instances are
    /// derived canonically rather than mutating or duplicating source AST.
    fn resolved_callable_signature(&mut self, declaration: NodeId) -> CallableSignature {
        let mut signature = self
            .current_specialized_owner
            .and_then(|owner| self.signatures.specialized_callable(owner, declaration))
            .or_else(|| self.signatures.callable(declaration))
            .expect("callable signature must have been collected")
            .clone();
        if let Some(substitutions) = self.current_template_substitutions.clone() {
            signature.parameters = signature
                .parameters
                .into_iter()
                .map(|parameter| {
                    self.types
                        .types_mut()
                        .substitute_template_parameters(parameter, &substitutions)
                        .expect("specialized parameter type belongs to the program store")
                })
                .collect();
            signature.return_type = self
                .types
                .types_mut()
                .substitute_template_parameters(signature.return_type, &substitutions)
                .expect("specialized return type belongs to the program store");
        }
        signature
    }

    fn resolved_type_syntax(&self, syntax: NodeId) -> Option<TypeId> {
        self.current_template_syntax_types
            .as_ref()
            .and_then(|types| types.get(&syntax).copied())
            .or_else(|| self.current_specialized_owner
            .and_then(|owner| self.types.specialized_type_for_syntax(owner, syntax))
            .or_else(|| self.types.type_for_syntax(syntax)))
    }

    fn template_parameter_bound(&self, type_id: TypeId) -> Option<Option<TypeId>> {
        self.types
            .template_parameter_bound_for(self.current_specialized_owner, type_id)
    }

    /// Returns whether an already analyzed expression explicitly produces a
    /// value. For example, `{ 1 }` does, while `{ 1; }` completes implicitly
    /// with unit. Successful expression analysis must always record this fact.
    fn explicitly_produces_value(&self, expression: &Expression) -> bool {
        self.checking
            .explicit_values
            .get(&expression.id)
            .copied()
            .expect("analyzed expression must record explicit-value status")
    }

    /// Tests whether natural fallthrough can supply unit to an expected type,
    /// including injection into a union such as `() | int`.
    fn accepts_implicit_unit(&self, expected: TypeId, unit: TypeId) -> bool {
        self.classify_contextual_assignment(
            TypedExpression {
                type_id: unit,
                category: ValueCategory::FreshTemporary,
            },
            expected,
            false,
        )
        .is_ok()
    }

    /// Detects borrowed callable/interface alternatives hidden inside a union,
    /// so wrapping a view in a tag cannot make it legal to return or retain.
    fn contains_non_escaping_erased_view(&self, type_id: TypeId) -> bool {
        match self.types.types().get(type_id) {
            Some(SemanticType::Callable { .. }
            | SemanticType::Interface { .. }
            | SemanticType::Intersection { .. }) => true,
            Some(SemanticType::Union { members, .. }) => members
                .iter()
                .any(|member| self.contains_non_escaping_erased_view(*member)),
            Some(SemanticType::Tuple { elements, .. }) => elements
                .iter()
                .any(|element| self.contains_non_escaping_erased_view(*element)),
            Some(SemanticType::Gc { .. }
            | SemanticType::Tracked { .. }
            | SemanticType::Primitive { .. }
            | SemanticType::NamedStruct { .. }
            | SemanticType::GeneratedStruct { .. }
            | SemanticType::AnonymousStruct { .. }
            | SemanticType::TemplateParameter { .. }
            | SemanticType::Builtin { .. }
            | SemanticType::Recovery
            | SemanticType::Divergence)
            | None => false,
        }
    }

    /// Tracked references contribute lifetimes only through inline storage.
    /// GC references and external-buffer collections are ownership boundaries,
    /// while `Error`, unions, tuples, and struct fields remain inline and are
    /// traversed transitively.
    fn type_contains_tracked_reference(&self, type_id: TypeId) -> bool {
        self.type_contains_tracked_reference_inner(type_id, &mut HashSet::new())
    }

    fn type_contains_tracked_reference_inner(
        &self,
        type_id: TypeId,
        visited: &mut HashSet<TypeId>,
    ) -> bool {
        if !visited.insert(type_id) {
            return false;
        }
        match self.types.types().get(type_id) {
            Some(SemanticType::Tracked { .. }) => true,
            Some(SemanticType::Tuple { elements, .. }
            | SemanticType::Union {
                members: elements, ..
            }) => elements
                .iter()
                .any(|element| self.type_contains_tracked_reference_inner(*element, visited)),
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Error,
                arguments,
                ..
            }) => arguments
                .iter()
                .any(|argument| self.type_contains_tracked_reference_inner(*argument, visited)),
            Some(
                SemanticType::NamedStruct { .. }
                | SemanticType::GeneratedStruct { .. }
                | SemanticType::AnonymousStruct { .. },
            ) => self
                .aggregate_layout_for_type(type_id)
                .is_some_and(|layout| {
                    layout.fields.iter().any(|field| {
                        self.type_contains_tracked_reference_inner(field.type_id, visited)
                    })
                }),
            Some(
                SemanticType::Gc { .. }
                | SemanticType::Primitive { .. }
                | SemanticType::Callable { .. }
                | SemanticType::Interface { .. }
                | SemanticType::TemplateParameter { .. }
                | SemanticType::Builtin { .. }
                | SemanticType::Intersection { .. }
                | SemanticType::Recovery
                | SemanticType::Divergence,
            )
            | None => false,
        }
    }

    fn aggregate_layout_for_type(&self, type_id: TypeId) -> Option<&AggregateLayout> {
        self.aggregate_layouts.get(&type_id).or_else(|| {
            self.aggregate_layouts.values().find(|layout| {
                self.types.types().has_same_shape(layout.type_id, type_id) == Some(true)
            })
        })
    }

    fn borrow_storage_violation(&self, type_id: TypeId) -> Option<BorrowStorageViolation> {
        self.borrow_storage_violation_inner(type_id, &mut HashSet::new())
    }

    fn borrow_storage_violation_inner(
        &self,
        type_id: TypeId,
        visited: &mut HashSet<TypeId>,
    ) -> Option<BorrowStorageViolation> {
        if !visited.insert(type_id) {
            return None;
        }
        match self.types.types().get(type_id) {
            Some(SemanticType::Gc { target, .. })
                if self.type_contains_tracked_reference(*target) =>
            {
                Some(BorrowStorageViolation::Gc(type_id))
            }
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Queue | BuiltinType::Vector | BuiltinType::Map,
                arguments,
                ..
            }) if arguments
                .iter()
                .any(|argument| self.type_contains_tracked_reference(*argument)) =>
            {
                Some(BorrowStorageViolation::ExternalBuffer(type_id))
            }
            Some(SemanticType::Tuple { elements, .. }
            | SemanticType::Union {
                members: elements, ..
            }) => elements.iter().find_map(|element| {
                self.borrow_storage_violation_inner(*element, visited)
            }),
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Error,
                arguments,
                ..
            }) => arguments.iter().find_map(|argument| {
                self.borrow_storage_violation_inner(*argument, visited)
            }),
            Some(
                SemanticType::NamedStruct { .. }
                | SemanticType::GeneratedStruct { .. }
                | SemanticType::AnonymousStruct { .. },
            ) => self.aggregate_layout_for_type(type_id).and_then(|layout| {
                layout.fields.iter().find_map(|field| {
                    self.borrow_storage_violation_inner(field.type_id, visited)
                })
            }),
            _ => None,
        }
    }

    fn is_recovery(&self, type_id: TypeId) -> bool {
        type_id == self.types.types().recovery()
    }

    fn is_divergence(&self, type_id: TypeId) -> bool {
        type_id == self.types.types().divergence()
    }
}

fn parse_format_specification(source: &str) -> Option<FormatSpecification> {
    let bytes = decode_format_specification(source)?;
    let mut offset = 0;
    let mut format = FormatSpecification::default();

    if bytes.get(1).copied().and_then(format_alignment).is_some() {
        format.fill = Some(bytes[0]);
        format.alignment = format_alignment(bytes[1]);
        offset = 2;
    } else if let Some(alignment) = bytes.first().copied().and_then(format_alignment) {
        format.alignment = Some(alignment);
        offset = 1;
    }

    if let Some(sign) = bytes.get(offset).copied().and_then(format_sign) {
        format.sign = Some(sign);
        offset += 1;
    }
    if bytes.get(offset) == Some(&b'0') {
        format.zero_padding = true;
        offset += 1;
    }

    let width_start = offset;
    while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
        offset += 1;
    }
    if width_start != offset {
        format.width = Some(parse_ascii_u32(&bytes[width_start..offset])?);
    }

    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let precision_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if precision_start == offset || bytes.get(offset) != Some(&b'f') {
            return None;
        }
        format.fixed_precision = Some(parse_ascii_u32(&bytes[precision_start..offset])?);
        offset += 1;
    }

    (offset == bytes.len()).then_some(format)
}

fn decode_format_specification(source: &str) -> Option<Vec<u8>> {
    let source = source.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut offset = 0;
    while offset < source.len() {
        if source[offset] != b'\\' {
            decoded.push(source[offset]);
            offset += 1;
            continue;
        }
        let escaped = *source.get(offset + 1)?;
        match escaped {
            b'\\' | b'"' | b'\'' => {
                decoded.push(escaped);
                offset += 2;
            }
            b'n' => {
                decoded.push(b'\n');
                offset += 2;
            }
            b'r' => {
                decoded.push(b'\r');
                offset += 2;
            }
            b't' => {
                decoded.push(b'\t');
                offset += 2;
            }
            b'0' => {
                decoded.push(0);
                offset += 2;
            }
            b'x' => {
                let high = ascii_hex_value(*source.get(offset + 2)?)?;
                let low = ascii_hex_value(*source.get(offset + 3)?)?;
                decoded.push(high * 16 + low);
                offset += 4;
            }
            _ => return None,
        }
    }
    Some(decoded)
}

fn parse_ascii_u32(digits: &[u8]) -> Option<u32> {
    digits.iter().try_fold(0_u32, |value, digit| {
        value.checked_mul(10)?.checked_add(u32::from(*digit - b'0'))
    })
}

const fn ascii_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const fn format_alignment(byte: u8) -> Option<FormatAlignment> {
    match byte {
        b'<' => Some(FormatAlignment::Left),
        b'>' => Some(FormatAlignment::Right),
        b'^' => Some(FormatAlignment::Center),
        _ => None,
    }
}

const fn format_sign(byte: u8) -> Option<FormatSign> {
    match byte {
        b'+' => Some(FormatSign::Plus),
        b'-' => Some(FormatSign::Minus),
        b' ' => Some(FormatSign::Space),
        _ => None,
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

fn index_runtime_templates(
    program: &Program,
    signatures: &SignatureCollection,
) -> HashMap<NodeId, Function> {
    let mut templates = HashMap::new();
    for declaration in &program.declarations {
        match declaration {
            Declaration::Function(function) => {
                index_runtime_template_function(function, signatures, &mut templates);
            }
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    if let StructMember::Function(function) = member {
                        index_runtime_template_function(function, signatures, &mut templates);
                    }
                }
            }
            Declaration::Interface(_) | Declaration::TypeAlias(_) => {}
        }
    }
    templates
}

fn index_runtime_template_function(
    function: &Function,
    signatures: &SignatureCollection,
    templates: &mut HashMap<NodeId, Function>,
) {
    if signatures.is_runtime_template(function.id) {
        templates.insert(function.id, function.clone());
    }
    let values = function
        .body
        .value
        .iter()
        .map(Box::as_ref)
        .chain(function.body.statements.iter().filter_map(|statement| {
            let StatementKind::Return(Some(value)) = &statement.kind else {
                return None;
            };
            Some(value)
        }));
    for value in values {
        if let ExpressionKind::TypeValue(type_syntax) = &value.kind {
            index_runtime_templates_in_type(type_syntax, signatures, templates);
        }
    }
}

fn index_runtime_templates_in_type(
    type_syntax: &TypeSyntax,
    signatures: &SignatureCollection,
    templates: &mut HashMap<NodeId, Function>,
) {
    match &type_syntax.kind {
        crate::ast::TypeKind::GeneratedStruct { members } => {
            for member in members {
                match member {
                    StructMember::Field(field) => index_runtime_templates_in_type(
                        &field.type_annotation,
                        signatures,
                        templates,
                    ),
                    StructMember::Function(function) => {
                        index_runtime_template_function(function, signatures, templates);
                    }
                }
            }
        }
        crate::ast::TypeKind::Builtin { arguments, .. }
        | crate::ast::TypeKind::Named { arguments, .. } => {
            for argument in arguments {
                index_runtime_templates_in_type(argument, signatures, templates);
            }
        }
        crate::ast::TypeKind::Associated {
            owner, arguments, ..
        } => {
            index_runtime_templates_in_type(owner, signatures, templates);
            for argument in arguments {
                index_runtime_templates_in_type(argument, signatures, templates);
            }
        }
        crate::ast::TypeKind::Mutable(inner)
        | crate::ast::TypeKind::Gc(inner)
        | crate::ast::TypeKind::Tracked(inner)
        | crate::ast::TypeKind::Group(inner) => {
            index_runtime_templates_in_type(inner, signatures, templates);
        }
        crate::ast::TypeKind::Tuple { elements } => {
            for element in elements {
                index_runtime_templates_in_type(element, signatures, templates);
            }
        }
        crate::ast::TypeKind::Callable {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                index_runtime_templates_in_type(parameter, signatures, templates);
            }
            index_runtime_templates_in_type(return_type, signatures, templates);
        }
        crate::ast::TypeKind::Intersection { members }
        | crate::ast::TypeKind::Union { members } => {
            for member in members {
                index_runtime_templates_in_type(member, signatures, templates);
            }
        }
        crate::ast::TypeKind::ComptimeType | crate::ast::TypeKind::Primitive(_) => {}
    }
}

fn find_type_syntax(program: &Program, target: NodeId) -> Option<&TypeSyntax> {
    for declaration in &program.declarations {
        let functions: Vec<&Function> = match declaration {
            Declaration::Function(function) => vec![function],
            Declaration::Struct(structure) => structure
                .members
                .iter()
                .filter_map(|member| match member {
                    StructMember::Function(function) => Some(function),
                    StructMember::Field(_) => None,
                })
                .collect(),
            Declaration::Interface(_) | Declaration::TypeAlias(_) => Vec::new(),
        };
        for function in functions {
            if let Some(found) = find_type_in_factory(function, target) {
                return Some(found);
            }
        }
    }
    None
}

fn find_type_in_factory(function: &Function, target: NodeId) -> Option<&TypeSyntax> {
    let values = function
        .body
        .value
        .iter()
        .map(Box::as_ref)
        .chain(function.body.statements.iter().filter_map(|statement| {
            let StatementKind::Return(Some(value)) = &statement.kind else {
                return None;
            };
            Some(value)
        }));
    for value in values {
        if let ExpressionKind::TypeValue(type_syntax) = &value.kind
            && let Some(found) = find_nested_type(type_syntax, target)
        {
            return Some(found);
        }
    }
    None
}

fn find_nested_type(type_syntax: &TypeSyntax, target: NodeId) -> Option<&TypeSyntax> {
    if type_syntax.id == target {
        return Some(type_syntax);
    }
    match &type_syntax.kind {
        crate::ast::TypeKind::Builtin { arguments, .. }
        | crate::ast::TypeKind::Named { arguments, .. } => arguments
            .iter()
            .find_map(|argument| find_nested_type(argument, target)),
        crate::ast::TypeKind::GeneratedStruct { members } => {
            members.iter().find_map(|member| match member {
                StructMember::Field(field) => find_nested_type(&field.type_annotation, target),
                StructMember::Function(function) => find_type_in_factory(function, target),
            })
        }
        crate::ast::TypeKind::Associated {
            owner, arguments, ..
        } => find_nested_type(owner, target).or_else(|| {
            arguments
                .iter()
                .find_map(|argument| find_nested_type(argument, target))
        }),
        crate::ast::TypeKind::Mutable(inner)
        | crate::ast::TypeKind::Gc(inner)
        | crate::ast::TypeKind::Tracked(inner)
        | crate::ast::TypeKind::Group(inner) => find_nested_type(inner, target),
        crate::ast::TypeKind::Tuple { elements } => elements
            .iter()
            .find_map(|element| find_nested_type(element, target)),
        crate::ast::TypeKind::Callable {
            parameters,
            return_type,
        } => parameters
            .iter()
            .find_map(|parameter| find_nested_type(parameter, target))
            .or_else(|| find_nested_type(return_type, target)),
        crate::ast::TypeKind::Intersection { members }
        | crate::ast::TypeKind::Union { members } => members
            .iter()
            .find_map(|member| find_nested_type(member, target)),
        crate::ast::TypeKind::ComptimeType | crate::ast::TypeKind::Primitive(_) => None,
    }
}

/// Partitions the inline aggregate-containment graph using Robert Tarjan's
/// strongly connected components algorithm.
///
/// Robert Tarjan introduced this depth-first-search algorithm in 1972. A
/// strongly connected component is a maximal group of graph nodes in which
/// every node can reach every other node. Here, nodes are named or anonymous
/// structs (including factory-generated structs) and an edge `A -> B` means
/// that `A` contains `B` inline. A component
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
    nodes: &[TypeId],
    edges: &HashMap<TypeId, Vec<(TypeId, LayoutField)>>,
) -> Vec<Vec<TypeId>> {
    struct Tarjan {
        next_index: usize,
        indices: HashMap<TypeId, usize>,
        low_links: HashMap<TypeId, usize>,
        stack: Vec<TypeId>,
        on_stack: HashSet<TypeId>,
        components: Vec<Vec<TypeId>>,
    }

    fn visit(
        node: TypeId,
        node_set: &HashSet<TypeId>,
        edges: &HashMap<TypeId, Vec<(TypeId, LayoutField)>>,
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

    let node_set: HashSet<TypeId> = nodes.iter().copied().collect();
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

    fn tuple_elements(expression: &Expression) -> &[Expression] {
        let ExpressionKind::Tuple { elements } = &expression.kind else {
            panic!("expected tuple expression")
        };
        elements
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

    fn coroutine_call(statement: &Statement) -> &Expression {
        let StatementKind::Coroutine(call) = &statement.kind else {
            panic!("expected coroutine statement")
        };
        call
    }

    fn deferred_call(statement: &Statement) -> &Expression {
        let StatementKind::Defer(call) = &statement.kind else {
            panic!("expected defer statement")
        };
        call
    }

    fn member_object(expression: &Expression) -> &Expression {
        let ExpressionKind::MemberAccess { object, .. } = &expression.kind else {
            panic!("expected member-access expression")
        };
        object
    }

    fn gc(expression: &Expression) -> &Expression {
        let ExpressionKind::GcAllocate(value) = &expression.kind else {
            panic!("expected garbage-collection expression")
        };
        value
    }

    fn ascription(expression: &Expression) -> (&Expression, &TypeSyntax) {
        let ExpressionKind::TypeAscription { value, type_syntax } = &expression.kind else {
            panic!("expected type-ascription expression")
        };
        (value, type_syntax)
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

    /// Simulates every recorded control-flow edge independently. The source
    /// fact depths initialize physical counters, each emitted operation is
    /// applied in order, and the result must exactly equal the destination
    /// depths without ever becoming negative.
    fn assert_narrowing_locks_balance(checking: &ExpressionChecking) {
        for edge in &checking.narrowing_edges {
            let mut counters: HashMap<_, isize> = edge
                .from
                .iter()
                .map(|(place, facts)| (place.clone(), facts.len() as isize))
                .collect();
            for operation in &edge.operations {
                let counter = counters.entry(operation.place.clone()).or_default();
                match operation.kind {
                    NarrowingLockKind::Acquire => *counter += 1,
                    NarrowingLockKind::Release => *counter -= 1,
                }
                assert!(
                    *counter >= 0,
                    "a narrowing lock was released before acquisition on {edge:#?}"
                );
            }
            let destination: HashMap<_, isize> = edge
                .to
                .iter()
                .map(|(place, facts)| (place.clone(), facts.len() as isize))
                .collect();
            counters.retain(|_, counter| *counter != 0);
            assert_eq!(counters, destination, "unbalanced narrowing edge: {edge:#?}");
        }
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
            ValueCategory::GcReference
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
                ValueTransfer::CopyGcReference
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
            ValueCategory::GcReference,
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
            Some(&ValueTransfer::CopyGcReference)
        );
    }

    #[test]
    fn types_plain_and_gc_self() {
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
            ValueCategory::GcReference
        );
        assert!(matches!(
            types.types().get(checking.expressions[&shared.id].type_id),
            Some(SemanticType::Gc {
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
    fn checks_and_records_primitive_conversion_ascriptions() {
        let source = concat!(
            "fn main() { ",
            "1.0: int; 1: float; 65: char; 'A': int; 1: int; ",
            "const bad: float = 1; (1.0: char) + 1; ",
            "}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let main = function(&program.declarations[0]);
        let expected_conversions = [
            (PrimitiveType::Int, PrimitiveConversion::FloatToInt),
            (PrimitiveType::Float, PrimitiveConversion::IntToFloat),
            (PrimitiveType::Char, PrimitiveConversion::IntToChar),
            (PrimitiveType::Int, PrimitiveConversion::CharToInt),
        ];
        for index in 0..expected_conversions.len() {
            let (primitive, conversion) = expected_conversions[index];
            let converted = expression(&main.body.statements[index]);
            assert_primitive_expression(
                &types,
                &checking,
                converted,
                primitive,
                AccessCapability::Const,
            );
            assert_eq!(
                checking.primitive_conversions.get(&converted.id),
                Some(&conversion)
            );
            assert_eq!(
                checking.expressions[&converted.id].category,
                ValueCategory::FreshTemporary
            );
        }
        assert_eq!(
            checking.primitive_conversion_runtime_checks
                [&expression(&main.body.statements[0]).id],
            PrimitiveConversionRuntimeCheck::FiniteSignedIntRange
        );
        assert!(!checking
            .primitive_conversion_runtime_checks
            .contains_key(&expression(&main.body.statements[1]).id));
        assert_eq!(
            checking.primitive_conversion_runtime_checks
                [&expression(&main.body.statements[2]).id],
            PrimitiveConversionRuntimeCheck::AsciiRange
        );
        assert!(!checking
            .primitive_conversion_runtime_checks
            .contains_key(&expression(&main.body.statements[3]).id));
        assert!(!checking
            .primitive_conversions
            .contains_key(&expression(&main.body.statements[4]).id));
        assert_eq!(checking.errors.len(), 2);
        assert!(
            checking.errors.iter().all(|error| matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ))
        );
        assert_eq!(
            checking.expressions[&expression(&main.body.statements[6]).id].type_id,
            types.types().recovery()
        );
    }

    #[test]
    fn keeps_ordinary_expected_type_boundaries_non_converting() {
        let source = concat!(
            "fn take(value: float) {}\n",
            "fn wrong_return() -> float { 1 }\n",
            "fn main() { const bad: float = 1; take(1); }",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);

        assert_eq!(checking.errors.len(), 3);
        assert!(checking.errors.iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        )));
        assert!(checking.primitive_conversions.is_empty());
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
    fn checks_tuple_literals_contextually_and_records_aggregate_transfers() {
        let source = concat!(
            "fn return_named(value: (int, string)) -> (int, string) { value }\n",
            "fn inspect() {\n",
            "    const inferred = (1, \"one\",);\n",
            "    const contextual: (int | string, string) = (2, \"two\");\n",
            "    const alias = inferred;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let returned = body_value(function(&program.declarations[0]));
        assert_eq!(
            checking.transfers[&returned.id],
            ValueTransfer::RecursiveCopy
        );

        let inspect = function(&program.declarations[1]);
        let inferred = binding_initializer(&inspect.body.statements[0]);
        let inferred_elements = tuple_elements(inferred);
        let inferred_type = checking.expressions[&inferred.id].type_id;
        assert!(matches!(
            types.types().get(inferred_type),
            Some(SemanticType::Tuple {
                elements,
                capability: AccessCapability::Mut,
            }) if elements.len() == 2
        ));
        assert_eq!(
            checking.transfers[&inferred_elements[0].id],
            ValueTransfer::TrivialCopy
        );
        assert_eq!(
            checking.transfers[&inferred_elements[1].id],
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.transfers[&inferred.id],
            ValueTransfer::MoveTemporary
        );

        let contextual = binding_initializer(&inspect.body.statements[1]);
        let contextual_elements = tuple_elements(contextual);
        assert!(checking
            .union_injections
            .contains_key(&contextual_elements[0].id));
        assert_eq!(
            checking.transfers[&contextual_elements[0].id],
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.transfers[&contextual_elements[1].id],
            ValueTransfer::MoveTemporary
        );

        let alias = binding_initializer(&inspect.body.statements[2]);
        assert_eq!(checking.transfers[&alias.id], ValueTransfer::Borrow);
    }

    #[test]
    fn requires_tuple_reconstruction_and_owned_element_sources() {
        let source = concat!(
            "struct Leaf {}\n",
            "fn reconstructed(number: int) -> (int | string,) { (number,) }\n",
            "fn wrong_shape(value: (int,)) -> (int | string,) { value }\n",
            "fn wrong_arity() -> (int, int) { (1,) }\n",
            "fn bad_owner(value: Leaf) { const invalid = (value,); }\n",
            "fn good_owner() { const valid = (Leaf {},); }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);

        assert_eq!(checking.errors.len(), 3, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::InvalidOwningSource {
                category: ValueCategory::BorrowedPlace,
                ..
            }
        ));

        let reconstructed = body_value(function(&program.declarations[1]));
        let reconstructed_element = &tuple_elements(reconstructed)[0];
        assert!(checking
            .union_injections
            .contains_key(&reconstructed_element.id));
        assert_eq!(
            checking.transfers[&reconstructed_element.id],
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.transfers[&reconstructed.id],
            ValueTransfer::MoveTemporary
        );

        let good_owner = function(&program.declarations[5]);
        let valid = binding_initializer(&good_owner.body.statements[0]);
        let fresh_leaf = &tuple_elements(valid)[0];
        assert_eq!(
            checking.transfers[&fresh_leaf.id],
            ValueTransfer::MoveTemporary
        );
    }

    #[test]
    fn resolves_tuple_elements_as_capability_checked_places() {
        let source = concat!(
            "struct Leaf { value: int, }\n",
            "fn inspect(const vmut pair: (int, Leaf), readonly: (int, Leaf), const vmut heap: &mut (int, Leaf)) {\n",
            "    pair.0 = 2;\n",
            "    pair.0 += 3;\n",
            "    pair.1 = Leaf { value: 4 };\n",
            "    heap.0 = 5;\n",
            "    const first = pair.0;\n",
            "    readonly.0 = 6;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ImmutableValue
        );

        let inspect = function(&program.declarations[1]);
        for statement in &inspect.body.statements[..4] {
            let assignment = expression(statement);
            let ExpressionKind::Assignment { target, .. } = &assignment.kind else {
                panic!("expected tuple element assignment")
            };
            assert!(matches!(
                checking.resolved_members[&target.id],
                ResolvedMember::TupleElement { .. }
            ));
            assert_eq!(
                checking.places[&target.id].value_capability,
                ValueCapability::Mut
            );
        }
        let first = binding_initializer(&inspect.body.statements[4]);
        assert!(matches!(
            checking.resolved_members[&first.id],
            ResolvedMember::TupleElement { index: 0 }
        ));
        assert_eq!(checking.places[&first.id].category, ValueCategory::BorrowedPlace);
        let invalid = expression(&inspect.body.statements[5]);
        let ExpressionKind::Assignment { target, .. } = &invalid.kind else {
            panic!("expected rejected tuple element assignment")
        };
        assert_eq!(
            checking.places[&target.id].value_capability,
            ValueCapability::Const
        );
    }

    #[test]
    fn copies_and_gc_allocates_tuples_with_recursive_storage_semantics() {
        let source = concat!(
            "struct Leaf { value: int, }\n",
            "fn duplicate(value: (int, Leaf)) -> (int, Leaf) { value.copy() }\n",
            "fn inspect(value: (int, Leaf)) {\n",
            "    const copied = value.copy();\n",
            "    const managed = &(1, Leaf { value: 2 });\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let duplicate = body_value(function(&program.declarations[1]));
        let (duplicate_callee, _) = call(duplicate);
        let ExpressionKind::MemberAccess {
            object: duplicate_source,
            ..
        } = &duplicate_callee.kind
        else {
            panic!("expected tuple copy member")
        };
        assert!(matches!(
            checking.resolved_members[&duplicate_callee.id],
            ResolvedMember::Copy { .. }
        ));
        assert_eq!(
            checking.transfers[&duplicate_source.id],
            ValueTransfer::RecursiveCopy
        );
        assert_eq!(checking.transfers[&duplicate.id], ValueTransfer::MoveTemporary);

        let inspect = function(&program.declarations[2]);
        let copied = binding_initializer(&inspect.body.statements[0]);
        let (copy_callee, _) = call(copied);
        let ExpressionKind::MemberAccess {
            object: copy_source,
            ..
        } = &copy_callee.kind
        else {
            panic!("expected tuple copy member")
        };
        assert_eq!(checking.transfers[&copy_source.id], ValueTransfer::RecursiveCopy);
        let managed = binding_initializer(&inspect.body.statements[1]);
        let allocated = gc(managed);
        assert_eq!(checking.transfers[&allocated.id], ValueTransfer::AllocateGc);
        assert!(matches!(
            types.types().get(checking.expressions[&managed.id].type_id),
            Some(SemanticType::Gc { target, .. })
                if matches!(types.types().get(*target), Some(SemanticType::Tuple { .. }))
        ));
    }

    #[test]
    fn diagnoses_invalid_tuple_fields_members_escapes_and_recursive_layouts() {
        let source = concat!(
            "struct Recursive { nested: (Recursive,), }\n",
            "fn invalid_return(value: (fn() -> int,)) -> (fn() -> int,) { value }\n",
            "fn inspect(number: int, pair: (int, int)) {\n",
            "    number.0;\n",
            "    pair.2;\n",
            "    pair.missing;\n",
            "    pair.copy(1);\n",
            "    (1, 2) == (1, 2);\n",
            "    const invalid_gc = &(lambda() { number; },);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InfiniteInlineLayout { .. }
        )));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InvalidTupleElementOwner { .. }
        )));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::TupleElementOutOfRange { index: 2, arity: 2 }
        )));
        assert!(checking.errors.iter().any(|error| {
            error.kind == ExpressionCheckingErrorKind::UnknownMember
        }));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 0,
                found: 1,
            }
        )));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand { .. }
        )));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InvalidGcAllocationSource { .. }
        )));
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::InvalidReturnSource { .. }
        )));
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
                ValueTransfer::CopyGcReference,
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
        let allocation_source = gc(allocated);
        assert_eq!(
            checking.transfers.get(&allocation_source.id),
            Some(&ValueTransfer::AllocateGc)
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
            "fn return_union(value: Reader) -> Reader | int { value }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        let returned = [
            body_value(function(&program.declarations[2])),
            body_value(function(&program.declarations[3])),
            body_value(function(&program.declarations[4])),
            body_value(function(&program.declarations[5])),
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
                ValueCategory::GcReference,
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
            ValueTransfer::CopyGcReference,
        ]) {
            assert_eq!(checking.transfers.get(&argument.id), Some(&transfer));
        }
        assert!(checking.errors.is_empty());
    }

    #[test]
    fn allocates_fresh_values_and_copies_gc_references() {
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
                ValueCategory::GcReference
            );
            initializers.push(initializer);
        }
        for initializer in &initializers {
            assert_eq!(
                checking.expressions[&initializer.id].category,
                ValueCategory::GcReference
            );
        }

        let number_value = gc(initializers[0]);
        let number_type = checking.expressions[&initializers[0].id].type_id;
        let number_target = types
            .types()
            .gc_target(number_type)
            .expect("allocated integer should have a GC target");
        assert!(matches!(
            types.types().get(number_target),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Const,
            })
        ));

        let text_value = gc(initializers[1]);
        let text_type = checking.expressions[&initializers[1].id].type_id;
        let text_target = types
            .types()
            .gc_target(text_type)
            .expect("allocated string should have a GC target");
        assert!(matches!(
            types.types().get(text_target),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                capability: AccessCapability::Mut,
            })
        ));

        let item_value = gc(initializers[2]);
        let item_type = checking.expressions[&initializers[2].id].type_id;
        assert_eq!(
            types.types().gc_target(item_type),
            Some(
                signatures
                    .callable(function(&program.declarations[1]).id)
                    .expect("make should have a signature")
                    .return_type
            )
        );

        let again_value = gc(initializers[4]);
        assert_eq!(
            checking.expressions[&initializers[4].id].type_id,
            checking.expressions[&initializers[3].id].type_id
        );
        for value in [number_value, text_value, item_value] {
            assert_eq!(
                checking.transfers.get(&value.id),
                Some(&ValueTransfer::AllocateGc)
            );
        }
        assert_eq!(
            checking.transfers.get(&again_value.id),
            Some(&ValueTransfer::CopyGcReference)
        );
        for initializer in initializers {
            assert_eq!(
                checking.transfers.get(&initializer.id),
                Some(&ValueTransfer::CopyGcReference)
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
        let local = gc(recovered);
        let borrowed = expression(&inspect.body.statements[2]);
        let parameter = gc(borrowed);
        let overflow = expression(&inspect.body.statements[3]);
        let overflow_value = gc(overflow);

        assert_eq!(checking.errors.len(), 3);
        assert_eq!(checking.errors[0].span, local.span);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidGcAllocationSource {
                category: ValueCategory::OwnedInlinePlace,
                ..
            }
        ));
        assert_eq!(checking.errors[1].span, parameter.span);
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::InvalidGcAllocationSource {
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
        let heap_lambda = gc(heap);
        assert_eq!(
            checking.transfers[&heap_lambda.id],
            ValueTransfer::AllocateGc
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
    fn rejects_direct_transitive_receiver_and_gc_backed_tracked_captures() {
        let source = concat!(
            "struct Inner {}\n",
            "struct Holder {\n",
            "    reference: *Inner,\n",
            "    fn capture(*self) { const invalid = lambda() { self; }; }\n",
            "    fn capture_inline(self) { const invalid = lambda() { self; }; }\n",
            "}\n",
            "fn inspect(value: *Inner, aggregate: (*Inner, int), heap: &Inner) {\n",
            "    const direct = lambda() { value; };\n",
            "    const transitive = lambda() { aggregate; };\n",
            "    const gc_backed: *Inner = heap;\n",
            "    const rooted = lambda() { gc_backed; };\n",
            "    const ordinary = 1;\n",
            "    const valid = lambda() -> int { ordinary };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| {
            error.kind == ExpressionCheckingErrorKind::BorrowContainingLambdaCapture
        }));

        let holder = structure(&program.declarations[1]);
        let method = holder
            .members
            .iter()
            .find_map(|member| match member {
                StructMember::Function(method) => Some(method),
                StructMember::Field(_) => None,
            })
            .expect("holder should have a method");
        let receiver_capture = binding_initializer(&method.body.statements[0]);
        assert_eq!(
            checking.expressions[&receiver_capture.id].type_id,
            types.types().recovery()
        );

        let inspect = function(&program.declarations[2]);
        for statement in [&inspect.body.statements[0], &inspect.body.statements[1], &inspect.body.statements[3]] {
            let capture = binding_initializer(statement);
            assert_eq!(
                checking.expressions[&capture.id].type_id,
                types.types().recovery()
            );
        }
        let valid = binding_initializer(&inspect.body.statements[5]);
        assert!(!matches!(
            types.types().get(checking.expressions[&valid.id].type_id),
            Some(SemanticType::Recovery)
        ));
    }

    #[test]
    fn isolates_lambda_tracked_return_roots_from_the_enclosing_callable() {
        let source = concat!(
            "struct Inner {}\n",
            "fn inspect(enclosing: *Inner) {\n",
            "    const captured = lambda() -> *Inner { enclosing };\n",
            "    const local = lambda() -> *Inner {\n",
            "        const value = Inner {};\n",
            "        value\n",
            "    };\n",
            "    const temporary = lambda() -> *Inner { Inner {} };\n",
            "    const valid = lambda(input: *Inner) -> *Inner { input };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 4, "{:#?}", checking.errors);
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::InvalidTrackedReturnSource
                })
                .count(),
            3
        );
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::BorrowContainingLambdaCapture
                })
                .count(),
            1
        );

        let inspect = function(&program.declarations[1]);
        let valid = binding_initializer(&inspect.body.statements[3]);
        assert!(!matches!(
            types.types().get(checking.expressions[&valid.id].type_id),
            Some(SemanticType::Recovery)
        ));
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
            ValueTransfer::CopyGcReference
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
            ValueTransfer::CopyGcReference
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
    fn copies_gc_references_stored_in_named_fields() {
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
            ValueTransfer::CopyGcReference
        );
        let read = binding_initializer(&inspect.body.statements[1]);
        assert_eq!(
            checking.expressions[&read.id].category,
            ValueCategory::GcReference
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
                checking.interface_views[&converted.id]
                    .backing_transfer_for(types.types(), checking.interface_views[&converted.id].source_type),
                ValueTransfer::Borrow
            );
        }
        assert_eq!(
            checking.interface_views[&fresh_conversion.id].backing_transfer_for(
                types.types(),
                checking.interface_views[&fresh_conversion.id].source_type,
            ),
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.interface_views[&empty_conversion.id]
                .source_members(types.types())
                .len(),
            1
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
    fn checks_ascriptions_and_uses_them_to_select_union_members() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface Writer { fn write(self) -> int; }\n",
            "struct File {\n",
            "    fn read(self) -> int { 1 }\n",
            "    fn write(self) -> int { 2 }\n",
            "}\n",
            "fn main() {\n",
            "    const file = File {};\n",
            "    const selected: Reader | Writer = file: Reader;\n",
            "    const fresh = File {}: Reader;\n",
            "    const vmut inferred = 1: mut int;\n",
            "    const direct_union = 1: int | float;\n",
            "    const existing: int | float = 2;\n",
            "    const preserved = existing: int | float;\n",
            "    const both = file: Reader & Writer;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[3]);
        let selected = binding_initializer(&main.body.statements[1]);
        let (selected_value, selected_type) = ascription(selected);
        let conversion = &checking.interface_views[&selected_value.id];
        let injection = &checking.union_injections[&selected.id];
        assert_eq!(conversion.destination_type, injection.member_type);
        assert_eq!(
            conversion.backing_transfer_for(types.types(), conversion.source_type),
            ValueTransfer::Borrow
        );
        assert_eq!(checking.expressions[&selected.id].category, ValueCategory::BorrowedPlace);
        assert_eq!(injection.union_type, checking.expressions[&selected.id].type_id);
        assert_eq!(
            types.type_for_syntax(selected_type.id),
            Some(injection.member_type)
        );

        let fresh = binding_initializer(&main.body.statements[2]);
        let (fresh_value, _) = ascription(fresh);
        let fresh_view = &checking.interface_views[&fresh_value.id];
        assert_eq!(
            fresh_view.backing_transfer_for(types.types(), fresh_view.source_type),
            ValueTransfer::MoveTemporary
        );
        assert_eq!(checking.expressions[&fresh.id].category, ValueCategory::BorrowedPlace);

        let mutable_int = binding_initializer(&main.body.statements[3]);
        assert!(matches!(
            types.types().get(checking.expressions[&mutable_int.id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Mut,
            })
        ));

        let direct_union = binding_initializer(&main.body.statements[4]);
        let (direct_value, direct_type) = ascription(direct_union);
        assert_eq!(
            checking.union_injections[&direct_value.id].union_type,
            types.type_for_syntax(direct_type.id).expect("ascribed union should resolve")
        );
        let preserved = binding_initializer(&main.body.statements[6]);
        let (preserved_value, _) = ascription(preserved);
        assert!(!checking.union_injections.contains_key(&preserved_value.id));
        let both = binding_initializer(&main.body.statements[7]);
        let (both_value, _) = ascription(both);
        assert!(matches!(
            types
                .types()
                .get(checking.interface_views[&both_value.id].destination_type),
            Some(SemanticType::Intersection { members, .. }) if members.len() == 2
        ));
    }

    #[test]
    fn ascriptions_preserve_explicit_values_divergence_and_recovery() {
        let source = concat!(
            "fn inspect() {\n",
            "    const implicit = { (); }: ();\n",
            "    const wrong: int | float = true: int;\n",
            "}\n",
            "fn diverges() -> int { loop {}: int }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));

        let inspect = function(&program.declarations[0]);
        let implicit = binding_initializer(&inspect.body.statements[0]);
        assert_eq!(checking.explicit_values[&implicit.id], false);
        let wrong = binding_initializer(&inspect.body.statements[1]);
        let (wrong_value, _) = ascription(wrong);
        assert_eq!(checking.errors[0].span, wrong_value.span);
        assert_eq!(checking.expressions[&wrong.id].type_id, types.types().recovery());
        assert!(!checking.union_injections.contains_key(&wrong.id));

        let diverges = body_value(function(&program.declarations[1]));
        assert!(matches!(
            types.types().get(checking.expressions[&diverges.id].type_id),
            Some(SemanticType::Divergence)
        ));
    }

    #[test]
    fn ascriptions_reject_downcasts_escalation_and_ambiguous_union_selection() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface Writer { fn write(self) -> int; }\n",
            "struct File {\n",
            "    fn read(self) -> int { 1 }\n",
            "    fn write(self) -> int { 2 }\n",
            "}\n",
            "fn inspect(reader: Reader) {\n",
            "    const file = File {};\n",
            "    const ambiguous: Reader | Writer = file;\n",
            "    const downcast = reader: File;\n",
            "    const escalation = reader: mut Reader;\n",
            "    const numeric = 1: float;\n",
            "    const anonymous = struct { fn read(self) -> int { 3 } }: Reader;\n",
            "    const heap = &File {};\n",
            "    const heap_reader = heap: &Reader;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::AmbiguousUnionConversion { .. }
        ));
        for error in &checking.errors[1..] {
            assert!(matches!(
                error.kind,
                ExpressionCheckingErrorKind::TypeMismatch { .. }
            ));
        }

        let inspect = function(&program.declarations[3]);
        let numeric = binding_initializer(&inspect.body.statements[4]);
        assert_eq!(
            checking.primitive_conversions.get(&numeric.id),
            Some(&PrimitiveConversion::IntToFloat)
        );
        let anonymous = binding_initializer(&inspect.body.statements[5]);
        let (anonymous_value, _) = ascription(anonymous);
        let anonymous_view = &checking.interface_views[&anonymous_value.id];
        assert_eq!(
            anonymous_view.backing_transfer_for(types.types(), anonymous_view.source_type),
            ValueTransfer::MoveTemporary
        );
        assert_eq!(checking.expressions[&anonymous.id].category, ValueCategory::BorrowedPlace);

        let heap_reader = binding_initializer(&inspect.body.statements[7]);
        let (heap_value, _) = ascription(heap_reader);
        let heap_view = &checking.interface_views[&heap_value.id];
        assert_eq!(
            heap_view.backing_transfer_for(types.types(), heap_view.source_type),
            ValueTransfer::CopyGcReference
        );
        assert_eq!(checking.expressions[&heap_reader.id].category, ValueCategory::GcReference);
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
            ExpressionCheckingErrorKind::InterfaceRequiresGcSource
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
            ValueCategory::GcReference
        );
        assert_eq!(
            checking.expressions[&borrowed.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.interface_views[&borrowed.id].backing_transfer_for(
                types.types(),
                checking.interface_views[&borrowed.id].source_type,
            ),
            ValueTransfer::Borrow
        );
    }

    #[test]
    fn widens_unions_with_deterministic_tag_remapping() {
        let source = concat!(
            "fn accept(value: int | float | none) {}\n",
            "fn inspect(value: int | float) {\n",
            "    const widened: int | float | none = value;\n",
            "    const exact: float | int = value;\n",
            "    accept(value);\n",
            "}\n",
            "fn optional_unit() -> () | int { return; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[1]);
        let widened = binding_initializer(&inspect.body.statements[0]);
        let exact = binding_initializer(&inspect.body.statements[1]);
        let called = expression(&inspect.body.statements[2]);
        let (_, arguments) = call(called);
        let widening = &checking.union_widenings[&widened.id];
        let Some(SemanticType::Union {
            members: source_members,
            ..
        }) = types.types().get(widening.source_union)
        else {
            panic!("widening source should be a union")
        };
        let Some(SemanticType::Union {
            members: destination_members,
            ..
        }) = types.types().get(widening.destination_union)
        else {
            panic!("widening destination should be a union")
        };
        assert!(source_members
            .iter()
            .all(|member| destination_members.contains(member)));
        assert_eq!(
            checking.expressions[&widened.id].category,
            ValueCategory::FreshTemporary
        );
        assert!(!checking.union_widenings.contains_key(&exact.id));
        assert!(checking.union_widenings.contains_key(&arguments[0].id));
        let optional_unit = function(&program.declarations[2]);
        assert!(checking
            .union_injections
            .contains_key(&optional_unit.body.statements[0].id));
    }

    #[test]
    fn borrows_structural_views_from_interfaces_and_union_members() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface ReaderAlias { fn read(self) -> int; }\n",
            "interface Writer { fn write(self, value: int); }\n",
            "struct File { fn read(self) -> int { 1 } }\n",
            "struct Socket { fn read(self) -> int { 2 } }\n",
            "fn inspect(\n",
            "    both: Reader & Writer,\n",
            "    alias: ReaderAlias,\n",
            "    choice: File | Socket,\n",
            "    managed: &File | &Socket,\n",
            "    mixed: File | &Socket,\n",
            "    boxed: &(File | Socket)\n",
            ") {\n",
            "    const reduced: Reader = both;\n",
            "    const equivalent: Reader = alias;\n",
            "    const selected: Reader = choice;\n",
            "    const exact_widened: File | Socket | Reader = choice;\n",
            "    const injected_view: Reader | none = choice;\n",
            "    const retained: &Reader = managed;\n",
            "    const mixed_view: Reader = mixed;\n",
            "    const injected: Reader | Writer = File {};\n",
            "    const vmut mutable_choice: File | Socket = if true {\n",
            "        File {}\n",
            "    } else {\n",
            "        Socket {}\n",
            "    };\n",
            "    const vmut mutable_view: Reader = mutable_choice;\n",
            "    const invalid_inline: &Reader = choice;\n",
            "    const invalid_boxed: &Reader = boxed;\n",
            "    const vmut invalid_escalation: Reader = choice;\n",
            "    const vmut invalid_gc_escalation: &Reader = managed;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 4, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InterfaceRequiresGcSource
        );
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));

        let inspect = function(&program.declarations[5]);
        let reduced = binding_initializer(&inspect.body.statements[0]);
        let equivalent = binding_initializer(&inspect.body.statements[1]);
        let selected = binding_initializer(&inspect.body.statements[2]);
        let exact_widened = binding_initializer(&inspect.body.statements[3]);
        let injected_view = binding_initializer(&inspect.body.statements[4]);
        let retained = binding_initializer(&inspect.body.statements[5]);
        let mixed = binding_initializer(&inspect.body.statements[6]);
        let injected = binding_initializer(&inspect.body.statements[7]);
        let mutable_view = binding_initializer(&inspect.body.statements[9]);
        for view in [reduced, equivalent] {
            let metadata = &checking.interface_views[&view.id];
            let members = metadata.source_members(types.types());
            assert_eq!(members.len(), 1);
            assert_eq!(
                metadata.backing_transfer_for(types.types(), members[0]),
                ValueTransfer::Borrow
            );
        }
        let selected_view = &checking.interface_views[&selected.id];
        let selected_members = selected_view.source_members(types.types());
        assert_eq!(selected_members.len(), 2);
        assert!(selected_members
            .iter()
            .all(|member| selected_view.backing_transfer_for(types.types(), *member)
                == ValueTransfer::Borrow));
        assert!(checking.union_widenings.contains_key(&exact_widened.id));
        assert!(!checking.interface_views.contains_key(&exact_widened.id));
        assert!(checking.union_injections.contains_key(&injected_view.id));
        assert_eq!(
            checking.interface_views[&injected_view.id]
                .source_members(types.types())
                .len(),
            2
        );
        assert_eq!(
            checking.expressions[&injected_view.id].category,
            ValueCategory::BorrowedPlace
        );
        let retained_view = &checking.interface_views[&retained.id];
        assert!(retained_view
            .source_members(types.types())
            .iter()
            .all(|member| retained_view.backing_transfer_for(types.types(), *member)
                == ValueTransfer::CopyGcReference));
        let mixed_view = &checking.interface_views[&mixed.id];
        assert!(mixed_view
            .source_members(types.types())
            .iter()
            .all(|member| mixed_view.backing_transfer_for(types.types(), *member)
                == ValueTransfer::Borrow));
        assert!(checking.union_injections.contains_key(&injected.id));
        let injected_metadata = &checking.interface_views[&injected.id];
        assert_eq!(
            injected_metadata.backing_transfer_for(
                types.types(),
                injected_metadata.source_type,
            ),
            ValueTransfer::MoveTemporary
        );
        assert_eq!(
            checking.expressions[&injected.id].category,
            ValueCategory::BorrowedPlace
        );
        assert_eq!(
            checking.interface_views[&mutable_view.id]
                .source_members(types.types())
                .len(),
            2
        );
        assert_eq!(
            checking.expressions[&retained.id].category,
            ValueCategory::GcReference
        );
    }

    #[test]
    fn diagnoses_ambiguous_union_interface_injections() {
        let source = concat!(
            "interface Reader { fn read(self); }\n",
            "interface Writer { fn write(self); }\n",
            "struct File { fn read(self) {} fn write(self) {} }\n",
            "struct Socket { fn read(self) {} fn write(self) {} }\n",
            "fn inspect(value: File | Socket) {\n",
            "    const selected: Reader | Writer = value: Reader;\n",
            "    const ambiguous: Reader | Writer = value;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::AmbiguousUnionConversion { .. }
        ));
        let inspect = function(&program.declarations[4]);
        let selected = binding_initializer(&inspect.body.statements[0]);
        let (selected_value, _) = ascription(selected);
        assert_eq!(
            checking.interface_views[&selected_value.id]
                .source_members(types.types())
                .len(),
            2
        );
        assert!(checking.union_injections.contains_key(&selected.id));
        let ambiguous = binding_initializer(&inspect.body.statements[1]);
        assert_eq!(
            checking.expressions[&ambiguous.id].type_id,
            types.types().recovery()
        );
        assert!(!checking.union_widenings.contains_key(&ambiguous.id));
    }

    #[test]
    fn rejects_union_widening_when_one_source_member_has_no_destination() {
        let source = concat!(
            "fn inspect(value: int | bool) {\n",
            "    const invalid: int | float = value;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let inspect = function(&program.declarations[0]);
        let invalid = binding_initializer(&inspect.body.statements[0]);
        assert_eq!(
            checking.expressions[&invalid.id].type_id,
            types.types().recovery()
        );
        assert!(!checking.union_widenings.contains_key(&invalid.id));
    }

    #[test]
    fn checks_loop_results_range_bindings_and_control_flow() {
        let source = concat!(
            "fn main() {\n",
            "    const selected = loop {\n",
            "        if true { break 1; }\n",
            "        continue;\n",
            "    };\n",
            "    while true { break; } else {};\n",
            "    for index in 0..=3 { index; break; } else {};\n",
            "    selected;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[0]);
        let selected = binding_initializer(&main.body.statements[0]);
        assert!(matches!(
            types.types().get(checking.expressions[&selected.id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));
        let ExpressionKind::Loop { body } = &selected.kind else {
            panic!("expected value-producing loop")
        };
        let conditional = expression(&body.statements[0]);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected conditional break")
        };
        let StatementKind::Break(Some(value)) = &then_branch.statements[0].kind else {
            panic!("expected valued break")
        };
        assert_eq!(
            checking.transfers.get(&value.id),
            None,
            "a sole result path preserves its category without a merge transfer"
        );

        let range = expression(&main.body.statements[2]);
        let ExpressionKind::RangeFor { body, .. } = &range.kind else {
            panic!("expected range loop")
        };
        let symbol = names
            .symbol_for_declaration(range.id)
            .expect("range binding should resolve");
        let binding = checking.bindings[&symbol];
        assert_eq!(binding.qualifiers.binding, BindingMutability::Const);
        assert_eq!(binding.qualifiers.value, ValueCapability::Const);
        let index = expression(&body.statements[0]);
        assert_eq!(
            checking.places[&index.id].binding_mutability,
            Some(BindingMutability::Const)
        );
    }

    #[test]
    fn diagnoses_loop_else_and_result_mismatches_without_cascades() {
        let source = concat!(
            "fn main() {\n",
            "    const missing = while true { break 1; };\n",
            "    const mixed = loop { if true { break 1; } break 2.0; };\n",
            "    const annotated: int = loop { break 3.0; };\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::LoopElseRequired
        );
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        let main = function(&program.declarations[0]);
        for statement in &main.body.statements {
            assert!(matches!(
                types
                    .types()
                    .get(checking.expressions[&binding_initializer(statement).id].type_id),
                Some(SemanticType::Recovery)
            ));
        }
    }

    #[test]
    fn records_union_plain_and_gc_loop_result_transfers() {
        let source = concat!(
            "struct Item {}\n",
            "fn main() {\n",
            "    const number: int | float = loop {\n",
            "        if true { break 1; }\n",
            "        break 2.0;\n",
            "    };\n",
            "    const original = Item {};\n",
            "    const plain = loop {\n",
            "        if true { break Item {}; }\n",
            "        break original;\n",
            "    };\n",
            "    const first = &Item {};\n",
            "    const second = &Item {};\n",
            "    const shared: &Item = loop {\n",
            "        if true { break first; }\n",
            "        break second;\n",
            "    };\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[1]);

        let number = binding_initializer(&main.body.statements[0]);
        let ExpressionKind::Loop { body: number_body } = &number.kind else {
            panic!("expected union loop")
        };
        let number_if = expression(&number_body.statements[0]);
        let ExpressionKind::If { then_branch, .. } = &number_if.kind else {
            panic!("expected union break conditional")
        };
        let StatementKind::Break(Some(integer)) = &then_branch.statements[0].kind else {
            panic!("expected integer break")
        };
        let StatementKind::Break(Some(float)) = &number_body.statements[1].kind else {
            panic!("expected float break")
        };
        assert!(checking.union_injections.contains_key(&integer.id));
        assert!(checking.union_injections.contains_key(&float.id));

        let plain = binding_initializer(&main.body.statements[2]);
        assert_eq!(
            checking.expressions[&plain.id].category,
            ValueCategory::BorrowedPlace
        );
        let ExpressionKind::Loop { body: plain_body } = &plain.kind else {
            panic!("expected plain loop")
        };
        let plain_if = expression(&plain_body.statements[0]);
        let ExpressionKind::If { then_branch, .. } = &plain_if.kind else {
            panic!("expected plain break conditional")
        };
        let StatementKind::Break(Some(fresh)) = &then_branch.statements[0].kind else {
            panic!("expected fresh break")
        };
        let StatementKind::Break(Some(named)) = &plain_body.statements[1].kind else {
            panic!("expected named break")
        };
        assert_eq!(checking.transfers[&fresh.id], ValueTransfer::MoveTemporary);
        assert_eq!(checking.transfers[&named.id], ValueTransfer::Borrow);

        let shared = binding_initializer(&main.body.statements[5]);
        assert_eq!(checking.expressions[&shared.id].category, ValueCategory::GcReference);
        let ExpressionKind::Loop { body: shared_body } = &shared.kind else {
            panic!("expected GC loop")
        };
        let shared_if = expression(&shared_body.statements[0]);
        let ExpressionKind::If { then_branch, .. } = &shared_if.kind else {
            panic!("expected GC break conditional")
        };
        let StatementKind::Break(Some(first)) = &then_branch.statements[0].kind else {
            panic!("expected first GC break")
        };
        let StatementKind::Break(Some(second)) = &shared_body.statements[1].kind else {
            panic!("expected second GC break")
        };
        assert_eq!(checking.transfers[&first.id], ValueTransfer::CopyGcReference);
        assert_eq!(checking.transfers[&second.id], ValueTransfer::CopyGcReference);
    }

    #[test]
    fn excludes_unreachable_and_nested_breaks_from_the_wrong_loop() {
        let source = concat!(
            "fn main() {\n",
            "    const direct = loop { break 1; break 2.0; };\n",
            "    const nested = loop {\n",
            "        while true { break; } else { break 2; }\n",
            "    };\n",
            "    direct; nested;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[0]);
        for index in 0..2 {
            let loop_expression = binding_initializer(&main.body.statements[index]);
            assert!(matches!(
                types.types().get(checking.expressions[&loop_expression.id].type_id),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::Int,
                    ..
                })
            ));
        }
    }

    #[test]
    fn reaches_a_fixed_point_for_loop_binding_provenance() {
        let source = concat!(
            "struct Item {}\n",
            "fn inspect(mut borrowed: Item) {\n",
            "    mut x = Item {};\n",
            "    mut y = Item {};\n",
            "    while true {\n",
            "        x = y;\n",
            "        y = borrowed;\n",
            "        continue;\n",
            "    };\n",
            "    x;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[1]);
        let x = expression(&inspect.body.statements[3]);
        assert_eq!(
            checking.expressions[&x.id].category,
            ValueCategory::BorrowedPlace
        );
    }

    #[test]
    fn rejects_assignment_to_the_range_binding() {
        let source = "fn main() { for index in 0..3 { index = 0; } }";
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ImmutableBinding
        );
    }

    #[test]
    fn propagates_loop_divergence_into_callable_completion() {
        let source = concat!(
            "fn spins() -> int { loop {} }\n",
            "fn returns() -> int { loop { return 1; } }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        for declaration in &program.declarations[..2] {
            let function = function(declaration);
            let value = function
                .body
                .value
                .as_deref()
                .expect("callable should end with a loop");
            assert!(matches!(
                types.types().get(checking.expressions[&value.id].type_id),
                Some(SemanticType::Divergence)
            ));
        }
    }

    #[test]
    fn reports_loop_headers_left_to_right_and_checks_unreachable_bodies() {
        let source = concat!(
            "fn main() {\n",
            "    while 0 { 9223372036854775808; } else {};\n",
            "    for index in 0.0..false {};\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 4, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::IntegerLiteralOutOfRange
        );
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
    }

    #[test]
    fn rejects_only_unbounded_inline_aggregate_cycles() {
        let source = concat!(
            "struct Direct { next: Direct, }\n",
            "struct Left { right: Right | none, }\n",
            "struct Right { left: Left, }\n",
            "struct Safe { next: &Safe | none, items: Vector(Safe), }\n",
            "struct Wrapped { failure: Error(Wrapped), }\n",
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

    #[test]
    fn narrows_exact_union_members_in_branches_and_after_guards() {
        let source = concat!(
            "fn take_int(value: int) {}\n",
            "fn inspect(value: int | float | none) {\n",
            "    if value is int { take_int(value); }\n",
            "    if !(value is int) { return; }\n",
            "    take_int(value);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[1]);
        let guarded_call = expression(&inspect.body.statements[2]);
        let (_, arguments) = call(guarded_call);
        assert!(matches!(
            types.types().get(checking.expressions[&arguments[0].id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));
        assert!(checking.narrowing_edges.iter().any(|edge| {
            edge
                .operations
                .iter()
                .any(|operation| operation.kind == NarrowingLockKind::Acquire)
        }));
        assert!(checking.narrowing_edges.iter().any(|edge| {
            edge
                .operations
                .iter()
                .any(|operation| operation.kind == NarrowingLockKind::Release)
        }));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn composes_subset_narrowing_through_boolean_operators() {
        let source = concat!(
            "fn take_number(value: int | float) {}\n",
            "fn inspect(value: int | float | none) {\n",
            "    if value is int || value is float { take_number(value); }\n",
            "    if value is int && (value is int) { take_number(value); }\n",
            "    if !(value is int | float) { return; }\n",
            "    take_number(value);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn narrows_resolved_field_places() {
        let source = concat!(
            "struct Holder { value: int | float, }\n",
            "fn take_int(value: int) {}\n",
            "fn inspect(holder: Holder) {\n",
            "    if holder.value is int { take_int(holder.value); }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[2]);
        let conditional = body_value(inspect);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected field-narrowing conditional")
        };
        let (_, arguments) = call(expression(&then_branch.statements[0]));
        assert!(matches!(
            types.types().get(checking.expressions[&arguments[0].id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn records_same_tag_and_guarded_tag_changing_assignments() {
        let source = concat!(
            "fn inspect(mut value: int | float) {\n",
            "    if value is int {\n",
            "        value = 2;\n",
            "        value = 2.5;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[0]);
        let conditional = body_value(inspect);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected assignment conditional")
        };
        let same_tag = expression(&then_branch.statements[0]);
        let changed_tag = expression(&then_branch.statements[1]);
        let ExpressionKind::Assignment { target: same, .. } = &same_tag.kind else {
            panic!("expected same-tag assignment")
        };
        let ExpressionKind::Assignment {
            target: changed, ..
        } = &changed_tag.kind
        else {
            panic!("expected tag-changing assignment")
        };
        assert_eq!(
            checking.union_mutations[&same.id],
            UnionMutationKind::SameTagReplacement
        );
        assert_eq!(
            checking.union_mutations[&changed.id],
            UnionMutationKind::GuardedTagChange
        );
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn rejects_non_union_and_nonmember_type_tests() {
        let source = concat!(
            "fn main() {\n",
            "    const number = 1;\n",
            "    number is int;\n",
            "    const choice: int | float = 1;\n",
            "    choice is bool;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidTypeTestSource { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::InvalidTypeTestMember { .. }
        ));
    }

    #[test]
    fn balances_recorded_narrowing_lock_operations() {
        let source = concat!(
            "fn inspect(value: int | float | none) {\n",
            "    if value is int {\n",
            "        if value is int { value; }\n",
            "    } else if value is float {\n",
            "        value;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        assert!(checking.narrowing_edges.iter().any(|edge| {
            edge
                .operations
                .iter()
                .any(|operation| operation.kind == NarrowingLockKind::Acquire)
        }));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn releases_narrowing_locks_on_loop_control_edges() {
        let source = concat!(
            "fn inspect(value: int | float | none) {\n",
            "    while value is int { value; } else { value; };\n",
            "    loop {\n",
            "        if value is float { break; }\n",
            "        continue;\n",
            "    };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        for kind in [
            NarrowingEdgeKind::LoopBackedge,
            NarrowingEdgeKind::Break,
            NarrowingEdgeKind::Continue,
            NarrowingEdgeKind::CallableCompletion,
        ] {
            assert!(
                checking.narrowing_edges.iter().any(|edge| edge.kind == kind),
                "missing {kind:?} lock edge: {:#?}",
                checking.narrowing_edges
            );
        }
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn aliases_acquire_independent_locks_on_the_same_borrowed_union() {
        let source = concat!(
            "fn inspect(value: int | float) {\n",
            "    const alias = value;\n",
            "    if value is int && alias is int { value; }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let acquired_roots: HashSet<_> = checking
            .narrowing_edges
            .iter()
            .flat_map(|edge| &edge.operations)
            .filter(|operation| operation.kind == NarrowingLockKind::Acquire)
            .map(|operation| operation.place.root)
            .collect();
        assert_eq!(acquired_roots.len(), 2);
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn checks_sequence_index_slice_length_and_concat_operations() {
        let source = concat!(
            "fn inspect(text: string, data: bytes, shared: &bytes) {\n",
            "    const character = text[0];\n",
            "    const byte = shared[1];\n",
            "    const text_part = text[-1..];\n",
            "    const byte_part = data[..2];\n",
            "    const text_size = text.length();\n",
            "    const byte_size = shared.length();\n",
            "    const combine = bytes::concat;\n",
            "    const joined = combine(data, data);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[0]);
        let character = binding_initializer(&inspect.body.statements[0]);
        let byte = binding_initializer(&inspect.body.statements[1]);
        assert_primitive_expression(
            &types,
            &checking,
            character,
            PrimitiveType::Char,
            AccessCapability::Const,
        );
        assert_primitive_expression(
            &types,
            &checking,
            byte,
            PrimitiveType::Int,
            AccessCapability::Const,
        );
        for indexed in [character, byte] {
            assert!(matches!(
                checking.resolved_sequence_operations[&indexed.id],
                ResolvedSequenceOperation::Index { .. }
            ));
            assert_eq!(
                checking.sequence_runtime_checks[&indexed.id],
                vec![SequenceRuntimeCheck::IndexBounds]
            );
        }

        for (statement, sequence) in [
            (2, SequenceKind::String),
            (3, SequenceKind::Bytes),
        ] {
            let slice = binding_initializer(&inspect.body.statements[statement]);
            assert_eq!(
                checking.resolved_sequence_operations[&slice.id],
                ResolvedSequenceOperation::Slice { sequence }
            );
            assert_eq!(
                checking.expressions[&slice.id].category,
                ValueCategory::FreshTemporary
            );
            assert_eq!(checking.transfers[&slice.id], ValueTransfer::MoveTemporary);
            assert!(matches!(
                types.types().get(checking.expressions[&slice.id].type_id),
                Some(SemanticType::Primitive {
                    capability: AccessCapability::Mut,
                    ..
                })
            ));
            assert_eq!(
                checking.sequence_runtime_checks[&slice.id],
                vec![SequenceRuntimeCheck::SliceBounds]
            );
        }

        for (statement, sequence) in [
            (4, SequenceKind::String),
            (5, SequenceKind::Bytes),
        ] {
            let call_expression = binding_initializer(&inspect.body.statements[statement]);
            let (callee, _) = call(call_expression);
            assert_eq!(
                checking.resolved_sequence_operations[&callee.id],
                ResolvedSequenceOperation::Length { sequence }
            );
        }
        let combine = binding_initializer(&inspect.body.statements[6]);
        assert_eq!(
            checking.resolved_sequence_operations[&combine.id],
            ResolvedSequenceOperation::BytesConcat
        );
        let concat = binding_initializer(&inspect.body.statements[7]);
        let (_, arguments) = call(concat);
        assert_eq!(checking.transfers[&concat.id], ValueTransfer::MoveTemporary);
        assert_eq!(arguments.len(), 2);
        assert!(arguments
            .iter()
            .all(|argument| checking.transfers[&argument.id] == ValueTransfer::Borrow));
    }

    #[test]
    fn checks_mutable_sequence_index_places_and_byte_ranges() {
        let source = concat!(
            "fn inspect(const vmut data: bytes, const vmut text: string) {\n",
            "    data[0] = 300;\n",
            "    data[1] += 10;\n",
            "    data[2] -= 10;\n",
            "    data[3] *= 10;\n",
            "    data[4] /= 10;\n",
            "    data[5] %= 10;\n",
            "    data[6] &= 10;\n",
            "    data[7] ^= 10;\n",
            "    data[8] |= 10;\n",
            "    data[9] <<= 1;\n",
            "    data[10] >>= 1;\n",
            "    text[0] = 'x';\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[0]);
        for statement in 0..=11 {
            let assignment = expression(&inspect.body.statements[statement]);
            let ExpressionKind::Assignment { target, value, .. } = &assignment.kind else {
                panic!("expected indexed assignment")
            };
            assert_eq!(
                checking.places[&target.id].value_capability,
                ValueCapability::Mut
            );
            assert_eq!(checking.transfers[&value.id], ValueTransfer::TrivialCopy);
            let checks = &checking.sequence_runtime_checks[&target.id];
            assert_eq!(checks[0], SequenceRuntimeCheck::IndexBounds);
            assert_eq!(
                checks.contains(&SequenceRuntimeCheck::ByteValueRange),
                statement < 11
            );
        }
    }

    #[test]
    fn reports_sequence_errors_without_stopping_later_operands() {
        let source = concat!(
            "fn bad(const vmut data: bytes, const vmut text: string, readonly: bytes) {\n",
            "    data[true];\n",
            "    1[0.0];\n",
            "    data[0] = 'x';\n",
            "    data[0] += true;\n",
            "    text[0] += 1;\n",
            "    readonly[0] = 1;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 7, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::InvalidSequenceOwner { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::InvalidAssignmentOperand { .. }
        ));
        assert_eq!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::ImmutableValue
        );
    }

    #[test]
    fn checks_sequence_bounds_and_member_arity_deterministically() {
        let source = concat!(
            "fn bad(data: bytes) {\n",
            "    data[true..false];\n",
            "    data.length(1);\n",
            "    bytes::concat(data);\n",
            "    data.length;\n",
            "    bytes::length;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 6, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 0,
                found: 1,
            }
        );
        assert_eq!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 2,
                found: 1,
            }
        );
        assert_eq!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::MethodRequiresCall
        );
        assert_eq!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::MethodRequiresValue
        );
    }


    #[test]
    fn checks_formatted_strings_and_records_normalized_specs() {
        let source = r#"
fn main() {
    const text = "name";
    const integer = 42;
    const decimal = 3.5;
    const truth = true;
    const character = 'A';
    const rendered = f"{text:\x2a<10}|{integer:+010}|{decimal:^12.2f}|{integer:>-8}|{integer:> 8}|{truth}|{character}|{()}|{none}";
    rendered;
}
"#;
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[0]);
        let formatted = binding_initializer(&main.body.statements[5]);
        let typed = checking.expressions[&formatted.id];
        assert!(matches!(
            types.types().get(typed.type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                capability: AccessCapability::Mut,
            })
        ));
        assert_eq!(typed.category, ValueCategory::FreshTemporary);
        assert_eq!(checking.transfers[&formatted.id], ValueTransfer::MoveTemporary);

        let interpolations = &checking.formatted_strings[&formatted.id];
        assert_eq!(interpolations.len(), 9);
        assert_eq!(
            interpolations[0].format,
            FormatSpecification {
                fill: Some(b'*'),
                alignment: Some(FormatAlignment::Left),
                width: Some(10),
                ..FormatSpecification::default()
            }
        );
        assert_eq!(
            interpolations[1].format,
            FormatSpecification {
                sign: Some(FormatSign::Plus),
                zero_padding: true,
                width: Some(10),
                ..FormatSpecification::default()
            }
        );
        assert_eq!(
            interpolations[2].format,
            FormatSpecification {
                alignment: Some(FormatAlignment::Center),
                width: Some(12),
                fixed_precision: Some(2),
                ..FormatSpecification::default()
            }
        );
        assert_eq!(
            interpolations[3].format,
            FormatSpecification {
                alignment: Some(FormatAlignment::Right),
                sign: Some(FormatSign::Minus),
                width: Some(8),
                ..FormatSpecification::default()
            }
        );
        assert_eq!(
            interpolations[4].format,
            FormatSpecification {
                alignment: Some(FormatAlignment::Right),
                sign: Some(FormatSign::Space),
                width: Some(8),
                ..FormatSpecification::default()
            }
        );
    }

    #[test]
    fn reports_unsupported_formatted_values_and_invalid_specs_in_source_order() {
        let source = r#"
interface Reader {}
struct Item {}
fn bad(data: bytes, choice: int | float, action: fn() -> int, reader: Reader, queue: Queue(int)) {
    const result: string = f"{Item {}}|{data}|{choice}|{action}|{reader}|{queue}|{panic("stop")}|{"text":+}|{1:.2f}|{1:q}";
}
fn main() {}
"#;
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 10, "{:#?}", checking.errors);
        assert!(checking.errors[..6].iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::UnsupportedFormattedValue { .. }
        )));
        assert_eq!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::DivergentFormattedValue
        );
        assert!(checking.errors[7..].iter().all(|error| {
            error.kind == ExpressionCheckingErrorKind::InvalidFormatSpecification
        }));

        let bad = function(&program.declarations[2]);
        let formatted = binding_initializer(&bad.body.statements[0]);
        assert!(matches!(
            types.types().get(checking.expressions[&formatted.id].type_id),
            Some(SemanticType::Recovery)
        ));
        assert!(!checking.formatted_strings.contains_key(&formatted.id));
    }

    #[test]
    fn records_formatted_interpolations_once_in_evaluation_order() {
        let source = r#"
fn first() -> int { 1 }
fn second() -> int { 2 }
fn main() {
    const rendered = f"{first()} then {second()}";
}
"#;
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[2]);
        let formatted = binding_initializer(&main.body.statements[0]);
        let ExpressionKind::FormattedString { parts } = &formatted.kind else {
            panic!("expected formatted initializer")
        };
        let values = parts
            .iter()
            .filter_map(|part| match part {
                FormattedStringPart::Interpolation { value, .. } => Some(value.id),
                FormattedStringPart::Text(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            checking.formatted_strings[&formatted.id]
                .iter()
                .map(|interpolation| interpolation.value)
                .collect::<Vec<_>>(),
            values
        );
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn checks_generated_construction_methods_and_associated_functions() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn get(self) -> T { self.inner }\n",
            "        fn make(value: T) -> Box(T) { Box(T) { inner: value } }\n",
            "    }\n",
            "}\n",
            "interface Reader { fn get(self) -> int; }\n",
            "fn main() {\n",
            "    const box: Box(int) = Box(int) { inner: 10 };\n",
            "    const read: int = box.get();\n",
            "    const made: Box(int) = Box(int)::make(20);\n",
            "    const direct: int = Box(int) { inner: 30 }.get();\n",
            "    const reader: Reader = box;\n",
            "    const dynamic: int = reader.get();\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.generated_methods.len(), 2);

        let main = function(&program.declarations[2]);
        for statement in &main.body.statements {
            let initializer = binding_initializer(statement);
            assert!(
                checking.expressions.contains_key(&initializer.id),
                "every generated-type boundary must be checked"
            );
        }
    }

    #[test]
    fn rejects_infinite_factory_generated_inline_layouts() {
        let source = concat!(
            "fn Node(comptime T: type) -> type { struct { value: T, next: Node(T), } }\n",
            "type IntNode = Node(int);\n",
            "fn main() {}",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(matches!(
            checking.errors.as_slice(),
            [ExpressionCheckingError {
                kind: ExpressionCheckingErrorKind::InfiniteInlineLayout { .. },
                ..
            }]
        ));
    }

    #[test]
    fn checks_runtime_templates_symbolically_through_their_bounds() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "interface Writer { fn write(self, value: int); }\n",
            "fn inspect(comptime T: type, value: T) -> T\n",
            "where T: Reader & Writer, { value.read(); value.write(1); value }\n",
            "fn inspect_private(comptime T: type, value: T) -> int\n",
            "where T: interface { fn read(self) -> int; }, { value.read() }\n",
            "struct Wrapper {\n",
            "    fn inspect(self, comptime T: Reader, value: T) -> int { value.read() }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let inspect = function(&program.declarations[2]);
        let parameter = named_parameter(inspect, 0);
        let FunctionParameterKind::Named { type_annotation, .. } = &parameter.kind else {
            panic!("expected runtime parameter")
        };
        let symbolic = types
            .type_for_syntax(type_annotation.id)
            .expect("template parameter use must resolve symbolically");
        assert!(matches!(
            types.types().get(symbolic),
            Some(SemanticType::TemplateParameter { .. })
        ));
        assert!(types.template_parameter_bound(symbolic).flatten().is_some());
        assert!(signatures.is_runtime_template(inspect.id));
    }

    #[test]
    fn rejects_unbounded_members_and_unspecialized_template_values() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "fn invalid(comptime T: Reader, value: T) {\n",
            "    value.missing();\n",
            "    value.copy();\n",
            "    value + value;\n",
            "    T::make;\n",
            "}\n",
            "fn main() { invalid; }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert_eq!(checking.errors[0].kind, ExpressionCheckingErrorKind::UnknownMember);
        assert_eq!(checking.errors[1].kind, ExpressionCheckingErrorKind::UnknownMember);
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::InvalidMemberOwner { .. }
        ));
        assert_eq!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::TemplateRequiresSpecialization
        );
    }

    #[test]
    fn specializes_top_level_runtime_templates_and_reuses_canonical_instances() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct File { fn read(self) -> int { 7 } }\n",
            "fn inspect(comptime T: Reader, value: T) -> T { value.read(); value }\n",
            "fn main() {\n",
            "    const first = inspect(File, File {});\n",
            "    const second = inspect(File, File {});\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);

        let main = function(&program.declarations[3]);
        let first = binding_initializer(&main.body.statements[0]);
        let second = binding_initializer(&main.body.statements[1]);
        let first_id = checking.runtime_specialization_calls[&first.id];
        assert_eq!(checking.runtime_specialization_calls[&second.id], first_id);
        let specialization = &checking.runtime_specializations[first_id.0];
        assert_eq!(specialization.declaration, function(&program.declarations[2]).id);
        assert_eq!(specialization.type_arguments.len(), 1);
        assert_eq!(specialization.signature.parameters, specialization.type_arguments);
        assert_eq!(specialization.signature.return_type, specialization.type_arguments[0]);
        assert!(!specialization.checking.expressions.is_empty());
    }

    #[test]
    fn reports_template_constraint_failures_and_still_checks_runtime_arguments() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct Missing {}\n",
            "fn inspect(comptime T: Reader, value: T, count: int) -> int { count }\n",
            "fn main() { inspect(Missing, Missing {}, false); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::MissingInterfaceMethod { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(checking.runtime_specializations.is_empty());
    }

    #[test]
    fn specializes_unconstrained_runtime_templates_with_primitive_types() {
        let source = concat!(
            "fn inspect(comptime T: type, value: T) {}\n",
            "fn main() { inspect(int, 1); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert_eq!(specialization.type_arguments.len(), 1);
        assert_eq!(
            types.types().get(specialization.type_arguments[0]),
            Some(&SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                capability: AccessCapability::Const,
            })
        );
        assert_eq!(specialization.signature.parameters, specialization.type_arguments);
    }

    #[test]
    fn integrates_tuples_with_templates_unions_and_narrowing() {
        let source = concat!(
            "type Entry = (int, string);\n",
            "fn identity(comptime T: type, value: T) -> T { value }\n",
            "fn take_entry(value: Entry) -> int { value.0 }\n",
            "fn inspect(value: Entry | (string, int)) {\n",
            "    if value is Entry { take_entry(value); }\n",
            "}\n",
            "fn main() {\n",
            "    const injected: Entry | bool = (1, \"one\");\n",
            "    const specialized = identity((int, string), (2, \"two\"));\n",
            "    inspect(specialized);\n",
            "    injected;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);

        let specialization = &checking.runtime_specializations[0];
        assert!(matches!(
            types.types().get(specialization.type_arguments[0]),
            Some(SemanticType::Tuple { elements, .. }) if elements.len() == 2
        ));
        assert_eq!(
            specialization.signature.parameters,
            specialization.type_arguments
        );
        assert_eq!(
            specialization.signature.return_type,
            specialization.type_arguments[0]
        );

        let inspect = function(&program.declarations[3]);
        let conditional = body_value(inspect);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected tuple-narrowing conditional")
        };
        let (_, arguments) = call(expression(&then_branch.statements[0]));
        assert!(matches!(
            types.types().get(checking.expressions[&arguments[0].id].type_id),
            Some(SemanticType::Tuple { elements, .. }) if elements.len() == 2
        ));

        let main = function(&program.declarations[4]);
        let injected = binding_initializer(&main.body.statements[0]);
        assert!(checking.union_injections.contains_key(&injected.id));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn specializes_gc_qualified_concrete_type_arguments() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct File { fn read(self) -> int { 1 } }\n",
            "fn inspect(comptime T: Reader, value: T) -> int { value.read() }\n",
            "fn main() { inspect(&File, &File {}); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let specialization = &checking.runtime_specializations[0];
        assert!(matches!(
            types.types().get(specialization.type_arguments[0]),
            Some(SemanticType::Gc { .. })
        ));
        assert_eq!(specialization.signature.parameters, specialization.type_arguments);
    }

    #[test]
    fn diagnoses_runtime_template_arity_without_skipping_surplus_values() {
        let source = concat!(
            "struct Item {}\n",
            "fn inspect(comptime T: type, value: T) {}\n",
            "fn main() { inspect(Item); inspect(Item, Item {}, false); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 2,
                ..
            }
        )));
        assert!(checking.runtime_specializations.is_empty());
    }

    #[test]
    fn forms_tracked_borrows_and_auto_dereferences_member_places() {
        let source = concat!(
            "struct Inner { value: int, }\n",
            "struct User { inner: Inner, fn read(self) -> int { self.inner.value } }\n",
            "fn inspect(readonly: *User, const vmut writable: *mut User, heap: *User) {\n",
            "    readonly.inner.value;\n",
            "    writable.inner.value = 1;\n",
            "    heap.read();\n",
            "}\n",
            "fn inspect_inner(inner: *Inner) {}\n",
            "fn main() {\n",
            "    const vmut user = User { inner: Inner { value: 1 } };\n",
            "    const vmut writable = User { inner: Inner { value: 2 } };\n",
            "    const heap = &User { inner: Inner { value: 3 } };\n",
            "    const borrowed: *User = user;\n",
            "    inspect(user, writable, heap);\n",
            "    inspect_inner(heap.inner);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[4]);
        let borrowed = binding_initializer(&main.body.statements[3]);
        assert_eq!(checking.transfers[&borrowed.id], ValueTransfer::Borrow);
        assert!(matches!(
            types.types().get(checking.expressions[&borrowed.id].type_id),
            Some(SemanticType::Tracked { .. })
        ));
        let (_, arguments) = call(expression(&main.body.statements[4]));
        assert_eq!(checking.tracked_borrows.len(), 5);
        for argument in arguments {
            let borrow = checking
                .tracked_borrows
                .get(&argument.id)
                .expect("each plain or GC argument should form a tracked borrow");
            assert!(matches!(borrow.source.root, PhysicalPlaceRoot::Symbol(_)));
            assert!(borrow.source.projections.is_empty());
        }
        assert!(matches!(
            types.types().get(checking.tracked_borrows[&arguments[2].id].source_type),
            Some(SemanticType::Gc { .. })
        ));
        let (_, interior_arguments) = call(expression(&main.body.statements[5]));
        let interior = &checking.tracked_borrows[&interior_arguments[0].id];
        assert_eq!(interior.source.storage, ValueCategory::GcReference);
        assert_eq!(interior.source.projections.len(), 1);

        let inspect = function(&program.declarations[2]);
        let value = expression(&inspect.body.statements[0]);
        let place = checking
            .physical_places
            .get(&value.id)
            .expect("automatically dereferenced fields should retain a physical path");
        assert!(matches!(place.root, PhysicalPlaceRoot::Symbol(_)));
        assert_eq!(place.projections.len(), 2);
        assert!(place
            .projections
            .iter()
            .all(|projection| matches!(projection, PhysicalPlaceProjection::Field(_))));
    }

    #[test]
    fn tracked_borrows_preserve_capability_and_only_dereference_for_plain_parameters() {
        let source = concat!(
            "struct User {}\n",
            "fn wants_plain(value: User) {}\n",
            "fn wants_gc(value: &User) {}\n",
            "fn wants_mut(const vmut value: *mut User) {}\n",
            "fn reject_existing(value: *User) { wants_mut(value); wants_plain(value); wants_gc(value); }\n",
            "fn main() {\n",
            "    const user = User {};\n",
            "    const heap = &User {};\n",
            "    wants_mut(user);\n",
            "    wants_mut(heap);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 4, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        )));
        assert!(checking.tracked_borrows.is_empty());
        let reject_existing = function(&program.declarations[4]);
        let plain_argument = call(expression(&reject_existing.body.statements[1])).1[0].id;
        assert!(checking
            .tracked_parameter_borrows
            .contains_key(&plain_argument));
        assert_eq!(checking.transfers[&plain_argument], ValueTransfer::Borrow);
    }

    #[test]
    fn rejects_tracked_bindings_formed_from_plain_and_gc_temporaries() {
        let source = concat!(
            "struct User {}\n",
            "fn inspect(value: *User) {}\n",
            "fn main() {\n",
            "    const invalid_plain: *User = User {};\n",
            "    const invalid_gc: *User = &User {};\n",
            "    const stable = User {};\n",
            "    mut vconst reference: *User = stable;\n",
            "    reference = User {};\n",
            "    inspect(User {});\n",
            "    inspect(&User {});\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 3, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| {
            error.kind == ExpressionCheckingErrorKind::TemporaryTrackedBorrowEscapes
        }));
        let main = function(&program.declarations[2]);
        for statement in &main.body.statements[..2] {
            let initializer = binding_initializer(statement);
            assert!(!checking.tracked_borrows.contains_key(&initializer.id));
        }
        let ExpressionKind::Assignment { value, .. } =
            &expression(&main.body.statements[4]).kind
        else {
            panic!("expected tracked-reference assignment")
        };
        assert!(!checking.tracked_borrows.contains_key(&value.id));
        for statement in &main.body.statements[5..] {
            let (_, arguments) = call(expression(statement));
            assert!(checking.tracked_borrows.contains_key(&arguments[0].id));
        }
    }

    #[test]
    fn links_tracked_results_to_parameters_and_tracked_receivers() {
        let source = concat!(
            "struct Inner {}\n",
            "struct Item {\n",
            "    inner: Inner,\n",
            "    fn project(*self) -> *Inner { self.inner }\n",
            "    fn invalid(self) -> *Inner { self.inner }\n",
            "}\n",
            "fn project(first: *Item, second: *Item) -> *Inner { first.inner }\n",
            "fn forward(first: *Item, second: *Item) -> *Inner { project(first, second) }\n",
            "fn invalid_forward(input: *Item) -> *Inner {\n",
            "    project(input, Item { inner: Inner {} })\n",
            "}\n",
            "fn invalid_gc(input: &Item) -> *Inner { input.inner }\n",
            "fn invalid_plain(input: Item) -> *Inner { input.inner }\n",
            "fn invalid_local(input: *Item) -> *Inner {\n",
            "    const local = Item { inner: Inner {} };\n",
            "    local.inner\n",
            "}\n",
            "fn main() {\n",
            "    const left = Item { inner: Inner {} };\n",
            "    const right = &Item { inner: Inner {} };\n",
            "    const linked: *Inner = forward(left, right);\n",
            "    const method: *Inner = left.project();\n",
            "    const invalid_temporary = project(Item { inner: Inner {} }, right);\n",
            "    const invalid_gc_temporary = project(&Item { inner: Inner {} }, right);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 7, "{:#?}", checking.errors);
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::InvalidTrackedReturnSource
                })
                .count(),
            5
        );
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::TemporaryTrackedBorrowEscapes
                })
                .count(),
            2
        );

        let main = function(&program.declarations[8]);
        let linked = binding_initializer(&main.body.statements[2]);
        let linked_sources = &checking.tracked_lifetime_links[&linked.id].sources;
        assert_eq!(linked_sources.len(), 2);
        assert!(linked_sources
            .iter()
            .all(|source| matches!(source.root, PhysicalPlaceRoot::Symbol(_))));

        let method = binding_initializer(&main.body.statements[3]);
        let method_sources = &checking.tracked_lifetime_links[&method.id].sources;
        assert_eq!(method_sources.len(), 1);
        assert!(matches!(
            method_sources[0].root,
            PhysicalPlaceRoot::Symbol(_)
        ));
    }

    #[test]
    fn propagates_tracked_lifetimes_through_inline_aggregates_and_unions() {
        let source = concat!(
            "struct Inner {}\n",
            "struct Item { inner: Inner, }\n",
            "struct Holder { reference: *Inner, }\n",
            "struct InvalidGc { holder: &Holder, }\n",
            "struct InvalidBuffer { holders: Vector(Holder), }\n",
            "fn pack(first: *Item, second: *Item) -> (Holder, *Inner) {\n",
            "    (Holder { reference: first.inner }, second.inner)\n",
            "}\n",
            "fn forward(value: (Holder, *Inner)) -> (Holder, *Inner) { value.copy() }\n",
            "fn main() {\n",
            "    const first = Item { inner: Inner {} };\n",
            "    const second = Item { inner: Inner {} };\n",
            "    const packed = pack(first, second);\n",
            "    const forwarded = forward(packed);\n",
            "    const selected: *Inner | int = first.inner;\n",
            "    const invalid_temporary = pack(Item { inner: Inner {} }, second);\n",
            "    const invalid_heap = &Holder { reference: first.inner };\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 4, "{:#?}", checking.errors);
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::TemporaryTrackedBorrowEscapes
                })
                .count(),
            1
        );
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| matches!(
                    error.kind,
                    ExpressionCheckingErrorKind::BorrowContainingGcStorage { .. }
                ))
                .count(),
            2
        );
        assert!(checking.errors.iter().any(|error| matches!(
            error.kind,
            ExpressionCheckingErrorKind::BorrowContainingExternalBuffer { .. }
        )));

        let main = function(&program.declarations[7]);
        for statement in &main.body.statements[2..5] {
            let initializer = binding_initializer(statement);
            let sources = &checking.tracked_lifetime_links[&initializer.id].sources;
            assert!(!sources.is_empty());
            assert!(sources
                .iter()
                .all(|source| matches!(source.root, PhysicalPlaceRoot::Symbol(_))));
        }
        let packed = binding_initializer(&main.body.statements[2]);
        assert_eq!(checking.tracked_lifetime_links[&packed.id].sources.len(), 2);
        let forwarded = binding_initializer(&main.body.statements[3]);
        assert_eq!(checking.tracked_lifetime_links[&forwarded.id].sources.len(), 2);
    }

    #[test]
    fn tracks_flow_sensitive_borrow_validity_and_gc_owner_roots() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut outer: Outer) {\n",
            "    const reference: *Leaf = outer.inner.leaf;\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "}\n",
            "fn rooted(heap: &Outer) {\n",
            "    const reference: *Leaf = heap.inner.leaf;\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::TrackedBorrowInvalidated
                })
                .count(),
            1,
            "{:#?}",
            checking.errors
        );

        let inspect = function(&program.declarations[3]);
        let first_target = match &expression(&inspect.body.statements[1]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected assignment"),
        };
        let second_target = match &expression(&inspect.body.statements[3]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected assignment"),
        };
        assert!(checking.borrow_invalidations.contains_key(&first_target.id));
        assert!(!checking.borrow_invalidations.contains_key(&second_target.id));

        let rooted = function(&program.declarations[4]);
        assert!(checking
            .gc_owner_roots
            .get(&rooted.body.statements[0].id)
            .is_some_and(|roots| roots.len() == 1
                && roots[0].storage == ValueCategory::GcReference));
    }

    #[test]
    fn merges_reassigned_tracked_origins_across_control_flow() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(flag: bool, const vmut left: Outer, const vmut right: Outer) {\n",
            "    mut vconst reference: *Leaf = left.inner.leaf;\n",
            "    if flag { reference = right.inner.leaf; }\n",
            "    left.inner = Inner { leaf: Leaf {} };\n",
            "    right.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert_eq!(
            checking
                .errors
                .iter()
                .filter(|error| {
                    error.kind == ExpressionCheckingErrorKind::TrackedBorrowInvalidated
                })
                .count(),
            2,
            "{:#?}",
            checking.errors
        );
        let inspect = function(&program.declarations[3]);
        let reference = expression(&inspect.body.statements[4]);
        assert_eq!(
            checking.tracked_lifetime_links[&reference.id].sources.len(),
            2
        );
    }

    #[test]
    fn preserves_borrows_into_displaced_root_backing_storage() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(mut vconst outer: Outer) {\n",
            "    const reference: *Leaf = outer.inner.leaf;\n",
            "    outer = Outer { inner: Inner { leaf: Leaf {} } };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[3]);
        let rebound_target = match &expression(&inspect.body.statements[1]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected assignment"),
        };
        assert!(matches!(
            checking.displaced_roots.get(&rebound_target.id),
            Some(PhysicalPlaceRoot::DisplacedSymbol(_, assignment))
                if *assignment == rebound_target.id
        ));
        let reference = expression(&inspect.body.statements[2]);
        assert!(matches!(
            checking.tracked_lifetime_links[&reference.id].sources[0].root,
            PhysicalPlaceRoot::DisplacedSymbol(_, _)
        ));
    }

    #[test]
    fn ends_borrow_constraints_on_mutually_exclusive_and_completed_paths() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(flag: bool, const vmut outer: Outer) {\n",
            "    const reference: *Leaf = outer.inner.leaf;\n",
            "    if flag {\n",
            "        outer.inner = Inner { leaf: Leaf {} };\n",
            "    } else {\n",
            "        reference;\n",
            "    }\n",
            "    { const local: *Leaf = outer.inner.leaf; local; };\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert!(checking.borrow_invalidations.is_empty());
    }

    #[test]
    fn keeps_shadowed_tracked_slots_and_their_scopes_independent() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut outer: Outer, const vmut other: Outer) {\n",
            "    const reference: *Leaf = outer.inner.leaf;\n",
            "    { const reference: *Leaf = other.inner.leaf; reference; };\n",
            "    other.inner = Inner { leaf: Leaf {} };\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[3]);
        let other_target = match &expression(&inspect.body.statements[2]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected other assignment"),
        };
        let outer_target = match &expression(&inspect.body.statements[3]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected outer assignment"),
        };
        assert!(!checking.borrow_invalidations.contains_key(&other_target.id));
        assert!(checking.borrow_invalidations.contains_key(&outer_target.id));
    }

    #[test]
    fn permits_leaf_replacement_but_rejects_ancestor_replacement_for_aliases() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut outer: Outer) {\n",
            "    const first: *Leaf = outer.inner.leaf;\n",
            "    const second: *Leaf = outer.inner.leaf;\n",
            "    outer.inner.leaf = Leaf {};\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "    first;\n",
            "    second;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[3]);
        let leaf_target = match &expression(&inspect.body.statements[2]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected leaf assignment"),
        };
        let ancestor_target = match &expression(&inspect.body.statements[3]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected ancestor assignment"),
        };
        assert!(!checking.borrow_invalidations.contains_key(&leaf_target.id));
        assert_eq!(checking.borrow_invalidations[&ancestor_target.id].len(), 1);
    }

    #[test]
    fn reassigning_a_tracked_slot_releases_its_previous_origin() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut left: Outer, const vmut right: Outer) {\n",
            "    mut vconst reference: *Leaf = left.inner.leaf;\n",
            "    reference = right.inner.leaf;\n",
            "    left.inner = Inner { leaf: Leaf {} };\n",
            "    right.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[3]);
        let left_target = match &expression(&inspect.body.statements[2]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected left assignment"),
        };
        let right_target = match &expression(&inspect.body.statements[3]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected right assignment"),
        };
        assert!(!checking.borrow_invalidations.contains_key(&left_target.id));
        assert!(checking.borrow_invalidations.contains_key(&right_target.id));
    }

    #[test]
    fn carries_tracked_origins_through_loop_backedges_and_continue() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(flag: bool, const vmut left: Outer, const vmut right: Outer) {\n",
            "    mut vconst reference: *Leaf = left.inner.leaf;\n",
            "    while flag { reference = right.inner.leaf; continue; }\n",
            "    left.inner = Inner { leaf: Leaf {} };\n",
            "    right.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert!(checking.errors.iter().all(|error| {
            error.kind == ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        }));
        let inspect = function(&program.declarations[3]);
        let reference = expression(&inspect.body.statements[4]);
        assert_eq!(checking.tracked_lifetime_links[&reference.id].sources.len(), 2);
    }

    #[test]
    fn carries_the_selected_tracked_origin_through_break() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut left: Outer, const vmut right: Outer) {\n",
            "    mut vconst reference: *Leaf = left.inner.leaf;\n",
            "    loop { reference = right.inner.leaf; break; };\n",
            "    left.inner = Inner { leaf: Leaf {} };\n",
            "    right.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[3]);
        let reference = expression(&inspect.body.statements[4]);
        assert_eq!(checking.tracked_lifetime_links[&reference.id].sources.len(), 1);
    }

    #[test]
    fn applies_flow_validity_and_gc_rooting_to_linked_call_results() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn project(input: *Outer) -> *Leaf { input.inner.leaf }\n",
            "fn inspect(const vmut outer: Outer, heap: &Outer) {\n",
            "    const direct = project(outer);\n",
            "    const rooted = project(heap);\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "    direct;\n",
            "    rooted;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[4]);
        assert!(checking
            .gc_owner_roots
            .get(&inspect.body.statements[1].id)
            .is_some_and(|roots| roots.len() == 1
                && roots[0].storage == ValueCategory::GcReference));
    }

    #[test]
    fn tracks_tuple_element_paths_during_replacement() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "fn inspect(const vmut pair: (Inner, int)) {\n",
            "    const reference: *Leaf = pair.0.leaf;\n",
            "    pair.1 = 2;\n",
            "    pair.0 = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[2]);
        let unrelated = match &expression(&inspect.body.statements[1]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected tuple assignment"),
        };
        let ancestor = match &expression(&inspect.body.statements[2]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected tuple assignment"),
        };
        assert!(!checking.borrow_invalidations.contains_key(&unrelated.id));
        assert!(checking.borrow_invalidations.contains_key(&ancestor.id));
    }

    #[test]
    fn combines_borrow_validity_with_union_tag_locks() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Holder { choice: Leaf | int, }\n",
            "fn inspect(const vmut holder: Holder) {\n",
            "    if holder.choice is Leaf {\n",
            "        const reference: *Leaf = holder.choice;\n",
            "        holder.choice = Leaf {};\n",
            "        reference;\n",
            "        holder.choice = 1;\n",
            "        reference;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        let inspect = function(&program.declarations[2]);
        let conditional = body_value(inspect);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected union conditional")
        };
        let same_tag = match &expression(&then_branch.statements[1]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected same-tag assignment"),
        };
        let changed_tag = match &expression(&then_branch.statements[3]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected tag-changing assignment"),
        };
        assert!(!checking.borrow_invalidations.contains_key(&same_tag.id));
        assert!(checking.borrow_invalidations.contains_key(&changed_tag.id));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn preserves_valid_tracked_state_after_failed_slot_assignment() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(const vmut outer: Outer) {\n",
            "    mut vconst reference: *Leaf = outer.inner.leaf;\n",
            "    reference = Leaf {};\n",
            "    outer.inner = Inner { leaf: Leaf {} };\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TemporaryTrackedBorrowEscapes
        );
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
    }

    #[test]
    fn records_gc_root_transitions_when_a_tracked_slot_is_reassigned() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer { inner: Inner, }\n",
            "fn inspect(left: &Outer, right: &Outer) {\n",
            "    mut vconst reference: *Leaf = left.inner.leaf;\n",
            "    reference = right.inner.leaf;\n",
            "    reference;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[3]);
        let assignment_target = match &expression(&inspect.body.statements[1]).kind {
            ExpressionKind::Assignment { target, .. } => target,
            _ => panic!("expected tracked-slot assignment"),
        };
        for node in [inspect.body.statements[0].id, assignment_target.id] {
            assert!(checking.gc_owner_roots.get(&node).is_some_and(|roots| {
                roots.len() == 1 && roots[0].storage == ValueCategory::GcReference
            }));
        }
        let reference = expression(&inspect.body.statements[2]);
        assert_eq!(checking.tracked_lifetime_links[&reference.id].sources.len(), 1);
    }

    #[test]
    fn propagates_flow_sensitive_origins_through_tracked_interface_views() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "fn inspect(flag: bool, mut vconst reader: *Reader, other: *Reader) {\n",
            "    if flag { reader = other; }\n",
            "    reader.read();\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let inspect = function(&program.declarations[1]);
        let call_expression = expression(&inspect.body.statements[1]);
        let (callee, _) = call(call_expression);
        let ExpressionKind::MemberAccess { object, .. } = &callee.kind else {
            panic!("expected tracked interface method call")
        };
        assert_eq!(checking.tracked_lifetime_links[&object.id].sources.len(), 2);
    }

    #[test]
    fn applies_borrow_validity_to_receiver_derived_references() {
        let source = concat!(
            "struct Leaf {}\n",
            "struct Inner { leaf: Leaf, }\n",
            "struct Outer {\n",
            "    inner: Inner,\n",
            "    fn inspect(mut self) {\n",
            "        const reference: *Leaf = self.inner.leaf;\n",
            "        self.inner = Inner { leaf: Leaf {} };\n",
            "        reference;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::TrackedBorrowInvalidated
        );
        assert!(checking.borrow_invalidations.values().any(|sources| {
            sources.iter().any(|source| {
                matches!(source.root, PhysicalPlaceRoot::SelfValue(_))
            })
        }));
    }

    #[test]
    fn retains_complete_tracked_metadata_for_generated_methods_and_runtime_specializations() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn project(*self, other: *T) -> *T { other }\n",
            "        fn redirect(self, flag: bool, left: &T, right: &T) {\n",
            "            mut vconst reference: *T = left;\n",
            "            if flag { reference = right; }\n",
            "            reference;\n",
            "        }\n",
            "    }\n",
            "}\n",
            "struct Item {}\n",
            "fn choose(comptime T: type, first: *T, second: *T) -> *T { first }\n",
            "fn main() {\n",
            "    const boxed = Box(Item) { inner: Item {} };\n",
            "    const heap = &Item {};\n",
            "    const other = &Item {};\n",
            "    const projected: *Item = boxed.project(heap);\n",
            "    boxed.redirect(false, heap, other);\n",
            "    const selected: *Item = choose(Item, boxed.inner, heap);\n",
            "    projected;\n",
            "    selected;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let main = function(&program.declarations[3]);
        for statement in [&main.body.statements[3], &main.body.statements[5]] {
            let result = binding_initializer(statement);
            let sources = &checking.tracked_lifetime_links[&result.id].sources;
            assert_eq!(sources.len(), 2);
            assert!(sources.iter().all(|source| {
                source
                    .projections
                    .contains(&PhysicalPlaceProjection::OpaqueDerived)
            }));
            assert!(sources
                .iter()
                .any(|source| source.storage == ValueCategory::GcReference));
            assert!(checking
                .gc_owner_roots
                .get(&statement.id)
                .is_some_and(|roots| roots.len() == 1
                    && roots[0].storage == ValueCategory::GcReference));
        }

        assert_eq!(checking.generated_methods.len(), 2);
        assert!(checking.generated_methods.values().any(|method| {
            method
                .checking
                .tracked_lifetime_links
                .values()
                .any(|link| link.sources.len() == 2)
                && method.checking.gc_owner_roots.len() == 2
        }));
        assert!(checking.generated_methods.values().any(|method| {
            method.checking.tracked_lifetime_links.values().any(|link| {
                link.sources.iter().all(|source| {
                    !source
                        .projections
                        .contains(&PhysicalPlaceProjection::OpaqueDerived)
                })
            })
        }));

        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert!(specialization
            .checking
            .tracked_lifetime_links
            .values()
            .any(|link| link.sources.len() == 1
                && link.sources[0].projections.is_empty()));
    }

    #[test]
    fn permits_exact_recursive_runtime_specialization() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct File { fn read(self) -> int { 1 } }\n",
            "fn read_many(comptime T: Reader, value: T, count: int) -> int {\n",
            "    if count == 0 { return value.read(); }\n",
            "    read_many(T, value, count - 1)\n",
            "}\n",
            "fn main() { read_many(File, File {}, 2); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert_eq!(specialization.checking.runtime_specialization_calls.len(), 1);
        assert!(specialization
            .checking
            .runtime_specialization_calls
            .values()
            .all(|called| *called == RuntimeCallableSpecializationId(0)));
    }

    #[test]
    fn rejects_type_expanding_runtime_specialization() {
        let source = concat!(
            "fn Box(comptime T: type) -> type { struct { inner: T, } }\n",
            "struct Item {}\n",
            "fn expand(comptime T: type) {\n",
            "    expand(Box(T));\n",
            "}\n",
            "fn main() { expand(Item); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.iter().any(|error| {
            error.kind
                == ExpressionCheckingErrorKind::ExpandingRuntimeTemplateSpecialization
        }), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
    }

    #[test]
    fn specializes_generated_types_used_by_top_level_templates() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct { inner: T, fn get(self) -> T { self.inner } }\n",
            "}\n",
            "struct Item {}\n",
            "fn inspect(comptime T: type, boxed: Box(T)) -> T {\n",
            "    boxed.get()\n",
            "}\n",
            "fn main() { const item = inspect(Item, Box(Item) { inner: Item {} }); item; }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
        let main = function(&program.declarations[3]);
        let item = binding_initializer(&main.body.statements[0]);
        assert!(matches!(
            types.types().get(checking.expressions[&item.id].type_id),
            Some(SemanticType::NamedStruct { .. })
        ));
        assert!(checking.runtime_specializations[0]
            .checking
            .resolved_members
            .values()
            .any(|member| matches!(member, ResolvedMember::Method { .. })));
    }

    #[test]
    fn specializes_named_methods_and_reuses_the_owner_aware_instance() {
        let source = concat!(
            "struct Mapper {\n",
            "    fn echo(self, comptime T: type, value: T) -> T { value }\n",
            "}\n",
            "fn main() {\n",
            "    const mapper = Mapper {};\n",
            "    const first = mapper.echo(int, 1);\n",
            "    const second = mapper.echo(int, 2);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);

        let main = function(&program.declarations[1]);
        let first = binding_initializer(&main.body.statements[1]);
        let second = binding_initializer(&main.body.statements[2]);
        let first_id = checking.runtime_specialization_calls[&first.id];
        assert_eq!(checking.runtime_specialization_calls[&second.id], first_id);
        let specialization = &checking.runtime_specializations[first_id.0];
        assert_eq!(
            specialization.owner,
            types.type_for_declaration(match &program.declarations[0] {
                Declaration::Struct(structure) => structure.id,
                _ => panic!("expected Mapper declaration"),
            })
        );
        assert_eq!(specialization.signature.parameters.len(), 1);
        assert_eq!(specialization.signature.return_type, specialization.type_arguments[0]);
    }

    #[test]
    fn method_specialization_identity_includes_the_generated_owner() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn echo(self, comptime U: type, value: U) -> U { value }\n",
            "    }\n",
            "}\n",
            "fn main() {\n",
            "    const numbers = Box(int) { inner: 1 };\n",
            "    const words = Box(string) { inner: \"value\" };\n",
            "    numbers.echo(int, 2);\n",
            "    words.echo(int, 3);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 2);
        assert_ne!(
            checking.runtime_specializations[0].owner,
            checking.runtime_specializations[1].owner
        );
        assert_eq!(
            checking.runtime_specializations[0].declaration,
            checking.runtime_specializations[1].declaration
        );
        assert_eq!(
            checking.runtime_specializations[0].type_arguments,
            checking.runtime_specializations[1].type_arguments
        );
    }

    #[test]
    fn composes_generated_owner_and_method_template_substitutions() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct File { fn read(self) -> int { 1 } }\n",
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn inspect(self, comptime U: Reader, value: U) -> T {\n",
            "            value.read();\n",
            "            self.inner\n",
            "        }\n",
            "    }\n",
            "}\n",
            "fn main() {\n",
            "    const result: int = Box(int) { inner: 1 }.inspect(File, File {});\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert!(specialization.owner.is_some());
        assert!(matches!(
            types.types().get(specialization.signature.return_type),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));
        assert!(specialization
            .checking
            .resolved_members
            .values()
            .any(|member| matches!(member, ResolvedMember::Field { .. })));
    }

    #[test]
    fn checks_method_template_constraints_and_recovers_runtime_arguments() {
        let source = concat!(
            "interface Reader { fn read(self) -> int; }\n",
            "struct File { fn read(self) -> int { 1 } }\n",
            "struct Missing {}\n",
            "struct Inspector {\n",
            "    fn inspect(self, comptime T: Reader, value: T, count: int) -> int {\n",
            "        value.read() + count\n",
            "    }\n",
            "}\n",
            "fn main() {\n",
            "    const inspector = Inspector {};\n",
            "    inspector.inspect(File, File {}, 1);\n",
            "    inspector.inspect(Missing, Missing {}, false);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 2, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::MissingInterfaceMethod { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(checking.runtime_specializations.len(), 1);
    }

    #[test]
    fn permits_exact_recursive_method_specialization() {
        let source = concat!(
            "struct Repeater {\n",
            "    fn repeat(self, comptime T: type, value: T, count: int) -> T {\n",
            "        if count == 0 { return value; }\n",
            "        self.repeat(T, value, count - 1)\n",
            "    }\n",
            "}\n",
            "fn main() { Repeater {}.repeat(int, 1, 2); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert_eq!(specialization.checking.runtime_specialization_calls.len(), 1);
        assert!(specialization
            .checking
            .runtime_specialization_calls
            .values()
            .all(|called| *called == RuntimeCallableSpecializationId(0)));
    }

    #[test]
    fn rejects_expanding_method_specialization() {
        let source = concat!(
            "fn Box(comptime T: type) -> type { struct { inner: T, } }\n",
            "struct Expander {\n",
            "    fn expand(self, comptime T: type) { self.expand(Box(T)); }\n",
            "}\n",
            "fn main() { Expander {}.expand(int); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.iter().any(|error| {
            error.kind
                == ExpressionCheckingErrorKind::ExpandingRuntimeTemplateSpecialization
        }), "{:#?}", checking.errors);
        assert_eq!(checking.runtime_specializations.len(), 1);
    }

    #[test]
    fn checks_parameterized_builtin_constructors_and_first_class_selection() {
        let source = concat!(
            "fn main() {\n",
            "    const queue_new = Queue(int)::new;\n",
            "    const vector_new = Vector(string)::new;\n",
            "    const map_new = Map(string, int)::new;\n",
            "    const queue = queue_new();\n",
            "    const vector = vector_new();\n",
            "    const map = map_new();\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[0]);
        for (statement, builtin) in main.body.statements[..3].iter().zip([
            BuiltinType::Queue,
            BuiltinType::Vector,
            BuiltinType::Map,
        ]) {
            let selected = binding_initializer(statement);
            assert!(matches!(
                checking.resolved_builtin_operations.get(&selected.id),
                Some(ResolvedBuiltinOperation::Constructor {
                    builtin: found,
                    error_inference: None,
                    ..
                }) if *found == builtin
            ));
        }
        for (statement, builtin) in main.body.statements[3..].iter().zip([
            BuiltinType::Queue,
            BuiltinType::Vector,
            BuiltinType::Map,
        ]) {
            let constructed = binding_initializer(statement);
            let typed = checking.expressions[&constructed.id];
            assert_eq!(typed.category, ValueCategory::FreshTemporary);
            assert!(matches!(
                types.types().get(typed.type_id),
                Some(SemanticType::Builtin {
                    builtin: found,
                    capability: AccessCapability::Mut,
                    ..
                }) if *found == builtin
            ));
        }
    }

    #[test]
    fn checks_explicit_payload_and_expected_error_constructor_inference() {
        let source = concat!(
            "fn main() {\n",
            "    const explicit = Error(string)::new(\"explicit\");\n",
            "    const inferred = Error::new(\"inferred\");\n",
            "    const expected: Error(int | string) = Error::new(1);\n",
            "    const make: fn(string) -> Error(string) = Error::new;\n",
            "    const made = make(\"made\");\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let main = function(&program.declarations[0]);
        let explicit = binding_initializer(&main.body.statements[0]);
        let inferred = binding_initializer(&main.body.statements[1]);
        let expected = binding_initializer(&main.body.statements[2]);
        let constructor_value = binding_initializer(&main.body.statements[3]);
        for (constructed, inference) in [
            (explicit, ErrorConstructorInference::Explicit),
            (inferred, ErrorConstructorInference::Payload),
            (expected, ErrorConstructorInference::Expected),
        ] {
            let (callee, _) = call(constructed);
            assert!(matches!(
                checking.resolved_builtin_operations.get(&callee.id),
                Some(ResolvedBuiltinOperation::Constructor {
                    builtin: BuiltinType::Error,
                    error_inference: Some(found),
                    ..
                }) if *found == inference
            ));
        }
        assert!(matches!(
            checking.resolved_builtin_operations.get(&constructor_value.id),
            Some(ResolvedBuiltinOperation::Constructor {
                builtin: BuiltinType::Error,
                error_inference: Some(ErrorConstructorInference::Expected),
                ..
            })
        ));
    }

    #[test]
    fn widens_error_payloads_and_exposes_only_const_value_access() {
        let source = concat!(
            "struct Io {}\n",
            "struct Parse {}\n",
            "struct Item {}\n",
            "fn inspect(error: Error(*mut Item)) { const payload: *Item = error.value; }\n",
            "fn main() {\n",
            "    const narrow = Error(Io)::new(Io {});\n",
            "    const wide: Error(Io | Parse) = narrow;\n",
            "    wide.value;\n",
            "    wide.value = Io {};\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert_eq!(checking.errors[0].kind, ExpressionCheckingErrorKind::ImmutableValue);

        let main = function(&program.declarations[4]);
        let widened = binding_initializer(&main.body.statements[1]);
        assert!(checking.error_widenings.contains_key(&widened.id));
        let accessed = expression(&main.body.statements[2]);
        assert!(matches!(
            checking.resolved_builtin_operations.get(&accessed.id),
            Some(ResolvedBuiltinOperation::ErrorValue { .. })
        ));
        assert_eq!(
            checking.places[&accessed.id].value_capability,
            ValueCapability::Const
        );

        let inspect = function(&program.declarations[3]);
        let payload = binding_initializer(&inspect.body.statements[0]);
        let payload_type = checking.expressions[&payload.id].type_id;
        assert!(matches!(
            types.types().get(payload_type),
            Some(SemanticType::Tracked {
                capability: AccessCapability::Const,
                ..
            })
        ));
    }

    #[test]
    fn diagnoses_error_constructor_arity_payload_and_inference_once() {
        let source = concat!(
            "fn main() {\n",
            "    const bad_queue = Queue(int)::new(1);\n",
            "    const missing_explicit = Error(int)::new();\n",
            "    const missing_inferred = Error::new();\n",
            "    const wrong_payload = Error(int)::new(\"wrong\");\n",
            "    const unknown = Error::new;\n",
            "    const recovered: int = wrong_payload;\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 0,
                found: 1,
            }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::CannotInferErrorPayload
        );
    }

    #[test]
    fn preserves_tracked_payload_origins_through_error_construction_and_widening() {
        let source = concat!(
            "struct Item {}\n",
            "fn wrap(value: *Item) -> Error(*Item) { Error::new(value) }\n",
            "fn reduce(value: Error(*mut Item)) -> Error(*Item) { value }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);
        let wrapped = body_value(function(&program.declarations[1]));
        let reduced = body_value(function(&program.declarations[2]));
        assert!(checking.tracked_lifetime_links.contains_key(&wrapped.id));
        assert!(checking.tracked_lifetime_links.contains_key(&reduced.id));
        assert!(checking.error_widenings.contains_key(&reduced.id));
    }

    #[test]
    fn checks_queue_sends_receives_and_lowering_metadata() {
        let source = concat!(
            "struct Item { value: int, }\n",
            "fn use_queues(\n",
            "    const vmut ints: Queue(int),\n",
            "    const vmut heaps: &mut Queue(&Item),\n",
            "    const vmut borrowed: *mut Queue(int),\n",
            "    heap: &mut Item,\n",
            ") {\n",
            "    ints.send(1);\n",
            "    heaps.send(heap);\n",
            "    borrowed.send(2);\n",
            "    const empty = ints.try_receive();\n",
            "    const nonempty = heaps.try_receive();\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let use_queues = function(&program.declarations[1]);
        for (statement, transfer) in [
            (0, ValueTransfer::TrivialCopy),
            (1, ValueTransfer::CopyGcReference),
            (2, ValueTransfer::TrivialCopy),
        ] {
            let invoked = expression(&use_queues.body.statements[statement]);
            let (callee, arguments) = call(invoked);
            assert_eq!(checking.transfers[&arguments[0].id], transfer);
            assert_eq!(checking.transfers[&member_object(callee).id], ValueTransfer::Borrow);
            assert!(matches!(
                checking.resolved_queue_operations.get(&callee.id),
                Some(ResolvedQueueOperation {
                    kind: QueueOperationKind::Send,
                    receiver_transfer: Some(ValueTransfer::Borrow),
                    element_transfer: Some(found),
                    receive_union: None,
                    ..
                }) if *found == transfer
            ));
        }

        for statement in &use_queues.body.statements[3..=4] {
            let received = binding_initializer(statement);
            let (callee, _) = call(received);
            let operation = &checking.resolved_queue_operations[&callee.id];
            assert_eq!(operation.kind, QueueOperationKind::TryReceive);
            let receive = operation
                .receive_union
                .expect("try_receive should retain its union layout");
            assert_eq!(checking.expressions[&received.id].type_id, receive.type_id);
            let members = checking
                .expressions
                .get(&received.id)
                .and_then(|typed| match types.types().get(typed.type_id) {
                    Some(SemanticType::Union { members, .. }) => Some(members),
                    _ => None,
                })
                .expect("try_receive should produce a union");
            assert_eq!(members[receive.element_member], operation.element_type);
            assert!(matches!(
                types.types().get(members[receive.none_member]),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::None,
                    ..
                })
            ));
        }
    }

    #[test]
    fn diagnoses_queue_receiver_transfer_arity_and_member_misuse() {
        let source = concat!(
            "struct Item {}\n",
            "fn bad(\n",
            "    readonly: Queue(int),\n",
            "    const vmut references: Queue(&Item),\n",
            "    heap: &Item,\n",
            ") {\n",
            "    readonly.send(1);\n",
            "    references.send(Item {});\n",
            "    references.send(heap, heap);\n",
            "    references.try_receive(heap);\n",
            "    references.unknown();\n",
            "    references.send;\n",
            "    Queue(&Item)::send;\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 7, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ReceiverCapabilityMismatch
        );
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 2,
            }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 0,
                found: 1,
            }
        ));
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::UnknownMember
        ));
        assert_eq!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::MethodRequiresCall
        );
        assert_eq!(
            checking.errors[6].kind,
            ExpressionCheckingErrorKind::MethodRequiresValue
        );

        let bad = function(&program.declarations[1]);
        let wrong_arity = expression(&bad.body.statements[2]);
        let (_, arguments) = call(wrong_arity);
        assert!(arguments
            .iter()
            .all(|argument| checking.expressions.contains_key(&argument.id)));
    }

    #[test]
    fn checks_ascii_output_panic_and_yield_builtin_metadata() {
        let source = concat!(
            "fn diverges() -> int { panic(\"stop\") }\n",
            "fn main() {\n",
            "    const encode = ascii.encode;\n",
            "    const encoded = encode(\"hello\");\n",
            "    const decode = ascii.decode;\n",
            "    const decoded = decode(encoded);\n",
            "    const direct = ascii.decode(encoded);\n",
            "    const output = println;\n",
            "    print(\"plain\");\n",
            "    output(\"line\");\n",
            "    yield();\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let diverges = function(&program.declarations[0]);
        let panic_call = body_value(diverges);
        let (panic_callee, panic_arguments) = call(panic_call);
        assert_eq!(checking.transfers[&panic_arguments[0].id], ValueTransfer::Borrow);
        assert_eq!(
            checking.resolved_builtin_operations[&panic_callee.id],
            ResolvedBuiltinOperation::Panic
        );
        assert!(matches!(
            types.types().get(checking.expressions[&panic_call.id].type_id),
            Some(SemanticType::Divergence)
        ));

        let main = function(&program.declarations[1]);
        let encode = binding_initializer(&main.body.statements[0]);
        assert_eq!(
            checking.resolved_builtin_operations[&encode.id],
            ResolvedBuiltinOperation::AsciiEncode
        );
        let encoded = binding_initializer(&main.body.statements[1]);
        assert_eq!(checking.expressions[&encoded.id].category, ValueCategory::FreshTemporary);
        assert!(matches!(
            types.types().get(checking.expressions[&encoded.id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Bytes,
                capability: AccessCapability::Mut,
            })
        ));

        let decode = binding_initializer(&main.body.statements[2]);
        let ResolvedBuiltinOperation::AsciiDecode {
            result_type,
            string_member,
            error_member,
        } = &checking.resolved_builtin_operations[&decode.id]
        else {
            panic!("expected resolved ASCII decode metadata")
        };
        let decoded = binding_initializer(&main.body.statements[3]);
        assert_eq!(checking.expressions[&decoded.id].type_id, *result_type);
        assert_eq!(checking.expressions[&decoded.id].category, ValueCategory::FreshTemporary);
        let Some(SemanticType::Union { members, .. }) = types.types().get(*result_type) else {
            panic!("ASCII decode should return a union")
        };
        assert!(matches!(
            types.types().get(members[*string_member]),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                capability: AccessCapability::Const,
            })
        ));
        assert!(matches!(
            types.types().get(members[*error_member]),
            Some(SemanticType::Builtin {
                builtin: BuiltinType::Error,
                ..
            })
        ));

        let direct = binding_initializer(&main.body.statements[4]);
        let (direct_callee, direct_arguments) = call(direct);
        assert!(matches!(
            checking.resolved_builtin_operations.get(&direct_callee.id),
            Some(ResolvedBuiltinOperation::AsciiDecode { .. })
        ));
        assert_eq!(checking.transfers[&direct_arguments[0].id], ValueTransfer::Borrow);

        let output = binding_initializer(&main.body.statements[5]);
        assert_eq!(
            checking.resolved_builtin_operations[&output.id],
            ResolvedBuiltinOperation::Output {
                mode: OutputMode::PrintLine,
            }
        );
        let printed = expression(&main.body.statements[6]);
        let (print_callee, print_arguments) = call(printed);
        assert_eq!(
            checking.resolved_builtin_operations[&print_callee.id],
            ResolvedBuiltinOperation::Output {
                mode: OutputMode::Print,
            }
        );
        assert_eq!(checking.transfers[&print_arguments[0].id], ValueTransfer::Borrow);
        assert!(matches!(
            types.types().get(checking.expressions[&printed.id].type_id),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Unit,
                ..
            })
        ));
        let yielded = expression(&main.body.statements[8]);
        let (yield_callee, _) = call(yielded);
        assert_eq!(
            checking.resolved_builtin_operations[&yield_callee.id],
            ResolvedBuiltinOperation::Yield
        );
        assert_eq!(checking.expressions[&yielded.id].category, ValueCategory::FreshTemporary);
    }

    #[test]
    fn diagnoses_builtin_namespace_type_arity_and_unreachable_tails() {
        let source = concat!(
            "fn bad() {\n",
            "    ascii;\n",
            "    ascii();\n",
            "    ascii.unknown;\n",
            "    ascii.unknown(1);\n",
            "    ascii.encode();\n",
            "    ascii.decode(\"text\");\n",
            "    print();\n",
            "    println(\"a\", \"b\");\n",
            "    panic();\n",
            "    yield(1);\n",
            "}\n",
            "fn unreachable() { panic(\"stop\"); false + true; }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 11, "{:#?}", checking.errors);
        assert_eq!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::NamespaceRequiresMember
        );
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::NamespaceRequiresMember
        );
        assert_eq!(checking.errors[2].kind, ExpressionCheckingErrorKind::UnknownMember);
        assert_eq!(checking.errors[3].kind, ExpressionCheckingErrorKind::UnknownMember);
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        ));
        assert!(matches!(
            checking.errors[5].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        for error in &checking.errors[6..10] {
            assert!(matches!(
                error.kind,
                ExpressionCheckingErrorKind::ArgumentCountMismatch { .. }
            ));
        }
        assert!(matches!(
            checking.errors[10].kind,
            ExpressionCheckingErrorKind::InvalidBinaryOperand { .. }
        ));

        let unreachable = function(&program.declarations[1]);
        let tail = expression(&unreachable.body.statements[1]);
        assert!(checking.expressions.contains_key(&tail.id));
        let panic_call = expression(&unreachable.body.statements[0]);
        let (panic_callee, _) = call(panic_call);
        assert_eq!(
            checking.resolved_builtin_operations[&panic_callee.id],
            ResolvedBuiltinOperation::Panic
        );
    }

    #[test]
    fn checks_postfix_error_propagation_metadata_and_widening() {
        let source = concat!(
            "fn varied(which: bool) -> int | string | Error(string) {\n",
            "    if which { 1 } else if false { \"ok\" } else { Error::new(\"failed\") }\n",
            "}\n",
            "fn caller() -> int | string | Error(string) { varied(true)? }\n",
            "fn narrow() -> int | Error(string) { 1 }\n",
            "fn widened() -> int | Error(int | string) { narrow()? }\n",
            "struct Item {}\n",
            "fn make() -> Item | Error(string) { Item {} }\n",
            "fn moved() -> Item | Error(string) { make()? }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let caller_try = function(&program.declarations[1])
            .body
            .value
            .as_deref()
            .expect("caller has a final propagation expression");
        let ExpressionKind::Try { expression: caller_operand } = &caller_try.kind else {
            panic!("expected caller try expression")
        };
        let caller_metadata = &checking.resolved_error_propagations[&caller_try.id];
        assert_eq!(caller_metadata.operand, caller_operand.id);
        assert_eq!(caller_metadata.success_members.len(), 2);
        assert!(matches!(
            types.types().get(caller_metadata.success_type),
            Some(SemanticType::Union { members, .. }) if members.len() == 2
        ));
        assert_eq!(caller_metadata.success_transfer, ValueTransfer::MoveTemporary);
        assert_eq!(caller_metadata.return_transfer, ValueTransfer::MoveTemporary);

        let widened_try = function(&program.declarations[3])
            .body
            .value
            .as_deref()
            .expect("widened has a final propagation expression");
        let widened_metadata = &checking.resolved_error_propagations[&widened_try.id];
        assert!(matches!(
            &widened_metadata.return_assignment,
            ContextualAssignment::UnionInjection {
                error_widening: Some(_),
                ..
            }
        ));

        let moved_try = function(&program.declarations[6])
            .body
            .value
            .as_deref()
            .expect("moved has a final propagation expression");
        assert_eq!(
            checking.resolved_error_propagations[&moved_try.id].success_transfer,
            ValueTransfer::MoveTemporary
        );
        assert_eq!(checking.expressions[&moved_try.id].category, ValueCategory::FreshTemporary);
    }

    #[test]
    fn propagates_tracked_origins_and_releases_narrowing_locks_on_error() {
        let source = concat!(
            "struct Item {}\n",
            "fn borrow(value: *Item) -> *Item | Error(string) { value }\n",
            "fn pass(value: *Item) -> *Item | Error(string) { borrow(value)? }\n",
            "fn status() -> () | Error(string) { () }\n",
            "fn checked(tag: int | string) -> () | Error(string) {\n",
            "    if tag is int { status()?; }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let pass_try = function(&program.declarations[2])
            .body
            .value
            .as_deref()
            .expect("pass has a final propagation expression");
        assert!(checking.tracked_lifetime_links.contains_key(&pass_try.id));
        assert_eq!(
            checking.resolved_error_propagations[&pass_try.id].success_transfer,
            ValueTransfer::Borrow
        );
        assert_eq!(
            checking.resolved_error_propagations[&pass_try.id].success_category,
            ValueCategory::BorrowedPlace
        );
        assert!(!checking.tracked_lifetime_links[&pass_try.id].sources.is_empty());

        let checked = function(&program.declarations[4]);
        let conditional = checked
            .body
            .value
            .as_deref()
            .expect("checked has a final statement-like conditional");
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected narrowing conditional")
        };
        let propagation = expression(&then_branch.statements[0]);
        assert!(checking.narrowing_edges.iter().any(|edge| {
            edge.source == propagation.id
                && edge.kind == NarrowingEdgeKind::ErrorPropagation
                && edge
                    .operations
                    .iter()
                    .any(|operation| operation.kind == NarrowingLockKind::Release)
        }));
        assert_narrowing_locks_balance(&checking);
    }

    #[test]
    fn diagnoses_invalid_postfix_error_propagation_without_parent_cascades() {
        let source = concat!(
            "fn no_error() -> int | string { 1 }\n",
            "fn ambiguous() -> int | Error(string) | Error(int) { 1 }\n",
            "fn fallible() -> int | Error(string) { 1 }\n",
            "fn invalid_operand() -> int | Error(string) { const bad: bool = 1?; 1 }\n",
            "fn missing_error() -> int | Error(string) { no_error()?; 1 }\n",
            "fn ambiguous_error() -> int | Error(string) | Error(int) { ambiguous()?; 1 }\n",
            "fn no_success() -> Error(string) { Error::new(\"failed\")? }\n",
            "fn incompatible() -> int { fallible()? }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::InvalidTryOperand { .. }
        ));
        assert!(matches!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::TryMissingErrorMember { .. }
        ));
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TryAmbiguousErrorMembers { .. }
        ));
        assert!(matches!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::TryRequiresSuccessMember { .. }
        ));
        assert!(matches!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::PropagatedErrorNotAccepted { .. }
        ));
    }

    #[test]
    fn checks_coroutine_starts_and_records_prepared_values_in_source_order() {
        let source = concat!(
            "struct Worker {\n",
            "    fn run(self, left: int, right: int) -> string { \"done\" }\n",
            "}\n",
            "struct Item {}\n",
            "fn fallible(value: int) -> int | Error(string) { value }\n",
            "fn consume(value: *Item, item: Item) {}\n",
            "fn schedule(\n",
            "    callback: fn(int) -> int,\n",
            "    worker: Worker,\n",
            "    tracked: *Item,\n",
            "    item: Item,\n",
            ") {\n",
            "    co callback(1);\n",
            "    co worker.run(2, 3);\n",
            "    co fallible(4);\n",
            "    co panic(\"later\");\n",
            "    co consume(tracked, item);\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let schedule = function(&program.declarations[4]);
        let callback_statement = &schedule.body.statements[0];
        let callback_call = coroutine_call(callback_statement);
        let (callback_callee, callback_arguments) = call(callback_call);
        let callback = &checking.resolved_coroutine_starts[&callback_statement.id];
        assert_eq!(callback.call, callback_call.id);
        assert!(matches!(
            &callback.target,
            ResolvedCoroutineCallTarget::CallableValue {
                callee,
                callable_type,
            } if *callee == callback_callee.id
                && *callable_type == checking.expressions[&callback_callee.id].type_id
        ));
        assert_eq!(callback.prepared.len(), 2);
        assert_eq!(callback.prepared[0].role, CoroutinePreparedRole::Callable);
        assert_eq!(callback.prepared[0].expression, callback_callee.id);
        assert_eq!(
            callback.prepared[1].role,
            CoroutinePreparedRole::Argument { index: 0 }
        );
        assert_eq!(callback.prepared[1].expression, callback_arguments[0].id);
        assert!(matches!(
            types.types().get(callback.discarded_result),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::Int,
                ..
            })
        ));

        let method_statement = &schedule.body.statements[1];
        let method_call = coroutine_call(method_statement);
        let (method_callee, method_arguments) = call(method_call);
        let method = &checking.resolved_coroutine_starts[&method_statement.id];
        assert!(matches!(
            &method.target,
            ResolvedCoroutineCallTarget::Member(ResolvedMember::Method { .. })
        ));
        let receiver = member_object(method_callee);
        assert_eq!(method.prepared.len(), 3);
        assert_eq!(method.prepared[0].role, CoroutinePreparedRole::Receiver);
        assert_eq!(method.prepared[0].expression, receiver.id);
        assert_eq!(method.prepared[0].transfer, Some(ValueTransfer::Borrow));
        for (index, argument) in method_arguments.iter().enumerate() {
            assert_eq!(
                method.prepared[index + 1].role,
                CoroutinePreparedRole::Argument { index }
            );
            assert_eq!(method.prepared[index + 1].expression, argument.id);
        }
        assert!(matches!(
            types.types().get(method.discarded_result),
            Some(SemanticType::Primitive {
                primitive: PrimitiveType::String,
                ..
            })
        ));

        let fallible_statement = &schedule.body.statements[2];
        let fallible = &checking.resolved_coroutine_starts[&fallible_statement.id];
        assert!(matches!(
            &fallible.target,
            ResolvedCoroutineCallTarget::Function { .. }
        ));
        assert!(matches!(
            types.types().get(fallible.discarded_result),
            Some(SemanticType::Union { members, .. })
                if members.iter().any(|member| matches!(
                    types.types().get(*member),
                    Some(SemanticType::Builtin {
                        builtin: BuiltinType::Error,
                        ..
                    })
                ))
        ));

        let panic_statement = &schedule.body.statements[3];
        let panic_start = &checking.resolved_coroutine_starts[&panic_statement.id];
        assert!(matches!(
            &panic_start.target,
            ResolvedCoroutineCallTarget::Builtin(ResolvedBuiltinOperation::Panic)
        ));
        assert!(matches!(
            types.types().get(panic_start.discarded_result),
            Some(SemanticType::Divergence)
        ));

        let retained_statement = &schedule.body.statements[4];
        let retained = &checking.resolved_coroutine_starts[&retained_statement.id];
        assert_eq!(retained.prepared.len(), 2);
        assert!(!retained.prepared[0].tracked_sources.is_empty());
        assert!(retained.prepared[0].place.is_some());
        assert_eq!(retained.prepared[0].transfer, Some(ValueTransfer::Borrow));
        assert!(retained.prepared[1].place.is_some());
        assert_eq!(retained.prepared[1].transfer, Some(ValueTransfer::Borrow));

        for statement in &schedule.body.statements {
            let metadata = &checking.resolved_coroutine_starts[&statement.id];
            assert_eq!(
                checking.expressions[&statement.id].type_id,
                metadata.statement_type
            );
            assert!(matches!(
                types.types().get(metadata.statement_type),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::Unit,
                    ..
                })
            ));
        }
    }

    #[test]
    fn diagnoses_invalid_coroutine_calls_and_recovers_with_unit_statements() {
        let source = concat!(
            "struct Worker { fn run(mut self, value: int) -> int { value } }\n",
            "fn bad(value: int, worker: Worker) {\n",
            "    co value();\n",
            "    co worker.run(\"wrong\");\n",
            "    co worker.run();\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 5, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::NotCallable { .. }
        ));
        assert_eq!(
            checking.errors[1].kind,
            ExpressionCheckingErrorKind::ReceiverCapabilityMismatch
        );
        assert!(matches!(
            checking.errors[2].kind,
            ExpressionCheckingErrorKind::TypeMismatch { .. }
        ));
        assert_eq!(
            checking.errors[3].kind,
            ExpressionCheckingErrorKind::ReceiverCapabilityMismatch
        );
        assert_eq!(
            checking.errors[4].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        );

        let bad = function(&program.declarations[1]);
        assert!(checking.resolved_coroutine_starts.is_empty());
        for statement in &bad.body.statements {
            assert!(matches!(
                types.types().get(checking.expressions[&statement.id].type_id),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::Unit,
                    ..
                })
            ));
        }
        let wrong_argument = call(coroutine_call(&bad.body.statements[1])).1[0].id;
        assert!(checking.expressions.contains_key(&wrong_argument));
    }

    #[test]
    fn checks_lexical_defer_preparation_and_normal_lifo_cleanup() {
        let source = concat!(
            "struct Worker { fn run(self, value: int) -> string { \"done\" } }\n",
            "struct Item {}\n",
            "fn clean(value: *Item) {}\n",
            "fn schedule(callback: fn(int) -> int, worker: Worker, tracked: *Item) {\n",
            "    defer callback(1);\n",
            "    defer worker.run(2);\n",
            "    if true {\n",
            "        defer clean(tracked);\n",
            "        defer yield();\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let schedule = function(&program.declarations[3]);
        let callback_statement = &schedule.body.statements[0];
        let callback_call = deferred_call(callback_statement);
        let (callback_callee, callback_arguments) = call(callback_call);
        let callback = &checking.resolved_deferred_calls[&callback_statement.id];
        assert_eq!(callback.block, schedule.body.id);
        assert_eq!(callback.registration_order, 0);
        assert!(callback.reachable);
        assert!(matches!(
            &callback.target,
            ResolvedCoroutineCallTarget::CallableValue { callee, .. }
                if *callee == callback_callee.id
        ));
        assert_eq!(callback.prepared.len(), 2);
        assert_eq!(callback.prepared[0].role, CoroutinePreparedRole::Callable);
        assert_eq!(callback.prepared[0].expression, callback_callee.id);
        assert_eq!(callback.prepared[1].expression, callback_arguments[0].id);

        let method_statement = &schedule.body.statements[1];
        let method_call = deferred_call(method_statement);
        let (method_callee, _) = call(method_call);
        let method = &checking.resolved_deferred_calls[&method_statement.id];
        assert_eq!(method.registration_order, 1);
        assert!(matches!(
            &method.target,
            ResolvedCoroutineCallTarget::Member(ResolvedMember::Method { .. })
        ));
        assert_eq!(method.prepared[0].role, CoroutinePreparedRole::Receiver);
        assert_eq!(method.prepared[0].expression, member_object(method_callee).id);

        let conditional = expression(&schedule.body.statements[2]);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected conditional defer scope")
        };
        let tracked_statement = &then_branch.statements[0];
        let tracked = &checking.resolved_deferred_calls[&tracked_statement.id];
        assert_eq!(tracked.block, then_branch.id);
        assert!(!tracked.prepared[0].tracked_sources.is_empty());
        assert!(tracked.prepared[0].place.is_some());
        let yielding_statement = &then_branch.statements[1];
        assert!(matches!(
            &checking.resolved_deferred_calls[&yielding_statement.id].target,
            ResolvedCoroutineCallTarget::Builtin(ResolvedBuiltinOperation::Yield)
        ));

        let branch_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| {
                edge.kind == DeferredCleanupEdgeKind::Normal
                    && edge.exited_blocks == [then_branch.id]
            })
            .expect("the conditional block should clean up normally");
        assert_eq!(
            branch_edge.registrations,
            [yielding_statement.id, tracked_statement.id]
        );
        let callable_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| {
                edge.kind == DeferredCleanupEdgeKind::Normal
                    && edge.exited_blocks == [schedule.body.id]
            })
            .expect("the callable body should clean up normally");
        assert_eq!(
            callable_edge.registrations,
            [method_statement.id, callback_statement.id]
        );
        assert_eq!(callable_edge.transfer_value, None);

        for statement in [
            callback_statement,
            method_statement,
            tracked_statement,
            yielding_statement,
        ] {
            let deferred = &checking.resolved_deferred_calls[&statement.id];
            assert_eq!(checking.expressions[&statement.id].type_id, deferred.statement_type);
            assert!(matches!(
                types.types().get(deferred.statement_type),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::Unit,
                    ..
                })
            ));
        }
    }

    #[test]
    fn records_defer_cleanup_for_callable_error_and_loop_exits_but_not_panic() {
        let source = concat!(
            "fn clean(value: int) {}\n",
            "fn fallible() -> () | Error(string) { () }\n",
            "fn exits(flag: bool) -> () | Error(string) {\n",
            "    defer clean(0);\n",
            "    if flag { defer clean(1); return (); }\n",
            "    fallible()?;\n",
            "}\n",
            "fn looped(flag: bool) {\n",
            "    loop {\n",
            "        defer clean(2);\n",
            "        if flag { defer clean(3); continue; }\n",
            "        break;\n",
            "    };\n",
            "}\n",
            "fn stopped() { defer clean(4); panic(\"stop\"); }\n",
            "fn preparation_panics() { defer clean(5); defer clean(panic(\"arg\")); }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        let exits = function(&program.declarations[2]);
        let outer = &exits.body.statements[0];
        let conditional = expression(&exits.body.statements[1]);
        let ExpressionKind::If { then_branch, .. } = &conditional.kind else {
            panic!("expected return branch")
        };
        let inner = &then_branch.statements[0];
        let return_statement = &then_branch.statements[1];
        let return_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| edge.source == return_statement.id)
            .expect("return should unwind both lexical scopes");
        assert_eq!(return_edge.kind, DeferredCleanupEdgeKind::Return);
        assert_eq!(return_edge.exited_blocks, [then_branch.id, exits.body.id]);
        assert_eq!(return_edge.registrations, [inner.id, outer.id]);
        let return_value = match &return_statement.kind {
            StatementKind::Return(Some(value)) => value.id,
            _ => panic!("expected valued return"),
        };
        assert_eq!(return_edge.transfer_value, Some(return_value));

        let propagation = expression(&exits.body.statements[2]);
        let ExpressionKind::Try { expression: operand } = &propagation.kind else {
            panic!("expected error propagation")
        };
        let error_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| edge.source == propagation.id)
            .expect("error propagation should unwind the callable scope");
        assert_eq!(error_edge.kind, DeferredCleanupEdgeKind::ErrorPropagation);
        assert_eq!(error_edge.registrations, [outer.id]);
        assert_eq!(error_edge.transfer_value, Some(operand.id));

        let looped = function(&program.declarations[3]);
        let loop_expression = expression(&looped.body.statements[0]);
        let ExpressionKind::Loop { body } = &loop_expression.kind else {
            panic!("expected loop expression")
        };
        let iteration_defer = &body.statements[0];
        let branch = expression(&body.statements[1]);
        let ExpressionKind::If { then_branch, .. } = &branch.kind else {
            panic!("expected continue branch")
        };
        let branch_defer = &then_branch.statements[0];
        let continue_statement = &then_branch.statements[1];
        let continue_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| edge.source == continue_statement.id)
            .expect("continue should clean the current iteration");
        assert_eq!(
            continue_edge.kind,
            DeferredCleanupEdgeKind::Continue(loop_expression.id)
        );
        assert_eq!(continue_edge.exited_blocks, [then_branch.id, body.id]);
        assert_eq!(
            continue_edge.registrations,
            [branch_defer.id, iteration_defer.id]
        );
        let break_statement = &body.statements[2];
        let break_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| edge.source == break_statement.id)
            .expect("break should clean the current iteration");
        assert_eq!(
            break_edge.kind,
            DeferredCleanupEdgeKind::Break(loop_expression.id)
        );
        assert_eq!(break_edge.exited_blocks, [body.id]);
        assert_eq!(break_edge.registrations, [iteration_defer.id]);

        let stopped = function(&program.declarations[4]);
        let stopped_defer = &stopped.body.statements[0];
        assert!(checking.resolved_deferred_calls.contains_key(&stopped_defer.id));
        assert!(!checking
            .deferred_cleanup_edges
            .iter()
            .any(|edge| edge.exited_blocks.contains(&stopped.body.id)));

        let preparation_panics = function(&program.declarations[5]);
        let earlier = &preparation_panics.body.statements[0];
        let diverging = &preparation_panics.body.statements[1];
        assert!(checking.resolved_deferred_calls.contains_key(&earlier.id));
        assert!(!checking.resolved_deferred_calls.contains_key(&diverging.id));
        assert!(!checking
            .deferred_cleanup_edges
            .iter()
            .any(|edge| edge.exited_blocks.contains(&preparation_panics.body.id)));
    }

    #[test]
    fn diagnoses_invalid_deferred_calls_and_excludes_unreachable_registrations() {
        let source = concat!(
            "fn clean(value: int) {}\n",
            "fn recover(value: int) { defer value(); defer clean(1); }\n",
            "fn unreachable() { panic(\"stop\"); defer clean(2); }\n",
            "fn main() {}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 1, "{:#?}", checking.errors);
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::NotCallable { .. }
        ));

        let recover = function(&program.declarations[1]);
        let invalid = &recover.body.statements[0];
        let valid = &recover.body.statements[1];
        assert!(!checking.resolved_deferred_calls.contains_key(&invalid.id));
        assert!(checking.resolved_deferred_calls[&valid.id].reachable);
        let recovery_edge = checking
            .deferred_cleanup_edges
            .iter()
            .find(|edge| edge.exited_blocks == [recover.body.id])
            .expect("a later valid defer should survive earlier call recovery");
        assert_eq!(recovery_edge.registrations, [valid.id]);

        let unreachable = function(&program.declarations[2]);
        let unreachable_defer = &unreachable.body.statements[1];
        assert!(!checking.resolved_deferred_calls[&unreachable_defer.id].reachable);
        assert!(!checking
            .deferred_cleanup_edges
            .iter()
            .any(|edge| edge.registrations.contains(&unreachable_defer.id)));
    }

    #[test]
    fn retains_defer_metadata_for_generated_methods_and_runtime_specializations() {
        let source = concat!(
            "fn clean() {}\n",
            "fn Box(comptime T: type) -> type {\n",
            "    struct { inner: T, fn dispose(self) { defer clean(); } }\n",
            "}\n",
            "fn generic(comptime T: type) { defer clean(); }\n",
            "fn main() {\n",
            "    Box(int) { inner: 1 }.dispose();\n",
            "    generic(int);\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        assert!(checking.generated_methods.values().any(|method| {
            method.checking.resolved_deferred_calls.len() == 1
                && method.checking.deferred_cleanup_edges.len() == 1
        }));
        assert_eq!(checking.runtime_specializations.len(), 1);
        let specialization = &checking.runtime_specializations[0];
        assert_eq!(specialization.checking.resolved_deferred_calls.len(), 1);
        assert_eq!(specialization.checking.deferred_cleanup_edges.len(), 1);
    }

    #[test]
    fn integrates_phase_7_7_operations_across_all_callable_kinds() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct {\n",
            "        inner: T,\n",
            "        fn process(self) -> T {\n",
            "            const vmut local = Queue(int)::new();\n",
            "            defer yield();\n",
            "            local.send(1);\n",
            "            self.inner\n",
            "        }\n",
            "    }\n",
            "}\n",
            "fn generic(comptime T: type, value: T) -> T {\n",
            "    defer yield();\n",
            "    co println(\"later\");\n",
            "    value\n",
            "}\n",
            "fn decode(data: bytes, fail: bool) -> string | Error(string | int) {\n",
            "    defer println(\"decoded\");\n",
            "    if fail { Error(int)::new(1) } else { ascii.decode(data)? }\n",
            "}\n",
            "fn worker(const vmut queue: &mut Queue(int), value: int) {\n",
            "    defer yield();\n",
            "    queue.send(value);\n",
            "    yield();\n",
            "}\n",
            "fn main() {\n",
            "    const vmut queue: &mut Queue(int) = &Queue(int)::new();\n",
            "    defer println(\"main\");\n",
            "    {\n",
            "        defer print(\"nested\");\n",
            "        const encoded = ascii.encode(\"A\");\n",
            "        const decoded = decode(encoded, false);\n",
            "        decoded;\n",
            "    };\n",
            "    co worker(queue, 7);\n",
            "    const received = queue.try_receive();\n",
            "    if received is int { print(\"received\"); };\n",
            "    Box(int) { inner: 1 }.process();\n",
            "    generic(int, 1);\n",
            "    co panic(\"later\");\n",
            "}\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert!(checking.errors.is_empty(), "{:#?}", checking.errors);

        assert!(checking.resolved_builtin_operations.values().any(|operation| {
            matches!(operation, ResolvedBuiltinOperation::AsciiEncode)
        }));
        assert!(checking.resolved_builtin_operations.values().any(|operation| {
            matches!(operation, ResolvedBuiltinOperation::AsciiDecode { .. })
        }));
        assert!(checking.resolved_builtin_operations.values().any(|operation| {
            matches!(operation, ResolvedBuiltinOperation::Output { .. })
        }));
        assert!(checking.resolved_builtin_operations.values().any(|operation| {
            matches!(operation, ResolvedBuiltinOperation::Panic)
        }));
        assert!(checking.resolved_builtin_operations.values().any(|operation| {
            matches!(operation, ResolvedBuiltinOperation::Yield)
        }));
        for builtin in [BuiltinType::Queue, BuiltinType::Error] {
            assert!(checking.resolved_builtin_operations.values().any(|operation| {
                matches!(
                    operation,
                    ResolvedBuiltinOperation::Constructor {
                        builtin: found,
                        ..
                    } if *found == builtin
                )
            }));
        }
        assert!(checking.error_widenings.len() >= 1);
        assert_eq!(checking.resolved_error_propagations.len(), 1);
        assert!(checking.resolved_queue_operations.values().any(|operation| {
            operation.kind == QueueOperationKind::Send
        }));
        assert!(checking.resolved_queue_operations.values().any(|operation| {
            operation.kind == QueueOperationKind::TryReceive
        }));
        assert_eq!(checking.resolved_coroutine_starts.len(), 2);
        assert_eq!(checking.resolved_deferred_calls.len(), 4);
        assert!(checking.deferred_cleanup_edges.iter().any(|edge| {
            edge.kind == DeferredCleanupEdgeKind::ErrorPropagation
        }));
        assert!(checking.deferred_cleanup_edges.iter().any(|edge| {
            edge.kind == DeferredCleanupEdgeKind::Normal && edge.exited_blocks.len() == 1
        }));

        assert!(checking.generated_methods.values().any(|method| {
            method.checking.resolved_queue_operations.values().any(|operation| {
                operation.kind == QueueOperationKind::Send
            }) && method.checking.resolved_deferred_calls.len() == 1
                && method.checking.deferred_cleanup_edges.len() == 1
        }));
        assert!(checking.runtime_specializations.iter().any(|specialization| {
            specialization.checking.resolved_deferred_calls.len() == 1
                && specialization.checking.resolved_coroutine_starts.len() == 1
                && specialization
                    .checking
                    .resolved_builtin_operations
                    .values()
                    .any(|operation| matches!(operation, ResolvedBuiltinOperation::Output { .. }))
        }));

        for declaration in &program.declarations {
            if let Declaration::Function(function) = declaration
                && !matches!(
                    function.return_type.as_ref().map(|syntax| &syntax.kind),
                    Some(crate::ast::TypeKind::ComptimeType)
                )
            {
                assert!(signatures.callable(function.id).is_some());
            }
        }
        assert!(checking.bindings.len() >= 8);
        assert!(checking
            .expressions
            .values()
            .all(|typed| !matches!(types.types().get(typed.type_id), Some(SemanticType::Recovery))));
    }

    #[test]
    fn orders_final_diagnostics_and_preserves_independent_recovery() {
        let source = concat!(
            "fn Box(comptime T: type) -> type {\n",
            "    struct { inner: T, fn invalid(self) { print(); } }\n",
            "}\n",
            "fn failures(const vmut queue: Queue(int)) {\n",
            "    Queue(int)::new(1);\n",
            "    queue.send(\"wrong\");\n",
            "    ascii.decode(\"wrong\");\n",
            "    const bad: bool = Error::new(1);\n",
            "    1?;\n",
            "    co 1();\n",
            "    defer 2();\n",
            "    print();\n",
            "}\n",
            "fn main() { Box(int) { inner: 1 }.invalid(); }\n",
        );
        let (module, program, names, context, mut types, signatures) = prepare(source);
        let checking = check(&module, &program, &names, &context, &mut types, &signatures);
        assert_eq!(checking.errors.len(), 9, "{:#?}", checking.errors);
        assert!(checking.errors.windows(2).all(|errors| {
            let left = errors[0].span;
            let right = errors[1].span;
            (left.module_id.as_u32(), left.start, left.end)
                <= (right.module_id.as_u32(), right.start, right.end)
        }));
        assert!(matches!(
            checking.errors[0].kind,
            ExpressionCheckingErrorKind::ArgumentCountMismatch {
                expected: 1,
                found: 0,
            }
        ));

        let failures = function(&program.declarations[1]);
        for statement in &failures.body.statements {
            let semantic_node = match &statement.kind {
                StatementKind::Expression(expression) => expression.id,
                StatementKind::Binding { initializer, .. } => initializer.id,
                StatementKind::Coroutine(_) | StatementKind::Defer(_) => statement.id,
                StatementKind::Function(_)
                | StatementKind::Return(_)
                | StatementKind::Break(_)
                | StatementKind::Continue => continue,
            };
            assert!(checking.expressions.contains_key(&semantic_node));
        }
        let coroutine = &failures.body.statements[5];
        let deferred = &failures.body.statements[6];
        for statement in [coroutine, deferred] {
            assert!(matches!(
                types.types().get(checking.expressions[&statement.id].type_id),
                Some(SemanticType::Primitive {
                    primitive: PrimitiveType::Unit,
                    ..
                })
            ));
        }
        assert!(checking.resolved_coroutine_starts.is_empty());
        assert!(checking.resolved_deferred_calls.is_empty());
    }
}
