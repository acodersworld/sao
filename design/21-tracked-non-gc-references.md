# 21. Tracked non-GC references

Tracked references express a lifetime relationship to storage owned elsewhere.
They are not raw pointers and do not introduce a separate calling convention.

## 21.1 Type and capability syntax

`*T` is a const tracked reference to `T`, while `*mut T` permits mutation of
the target through that view. As with GC references, target capability is
written inside the reference qualifier; `mut *T` is invalid. A tracked
reference cannot itself be tracked, so `**T` and `*(*T)` are invalid.

The language therefore has three distinct storage contracts:

- `T` is a plain inline value or a call-scoped borrowed view.
- `&T` is a reference to independently owned, traced GC storage.
- `*T` is a tracked non-owning reference to storage owned elsewhere.

These types have distinct canonical identities. Changing `*T` to `*mut T`
changes the target access capability but does not make the binding itself
mutable. Tracked qualification composes with named, primitive, callable,
aggregate, union, intersection, built-in, alias, and template-produced types.

## 21.2 Parameter distinction

Ordinary plain aggregate parameters continue to use the existing by-reference
implementation. A declaration `value: T` is nevertheless only a call-scoped
borrow. A declaration `value: *T` records a tracked lifetime relationship and
may eventually contribute to the lifetime of an escaping tracked result. The
type spelling does not select a different ABI on its own.

## 21.3 Borrow formation and member access

A plain `T` or GC-owned `&T` expression may satisfy an expected `*T`. The
conversion borrows the existing storage: it neither copies the value nor
allocates new storage. The source may preserve or reduce its access capability,
so mutable storage can form either `*mut T` or `*T`, while const storage cannot
form `*mut T`.

Tracked references use ordinary `.` member access. Field, tuple-element, method,
and supported primitive-member lookup automatically dereference the tracked
wrapper and apply its target access capability transitively. There is no `->`
operator.

The checker records each newly formed borrow's physical root and its stable
field or tuple-element path. Named bindings and `self` use semantic identities;
temporaries use their expression identity. A path rooted in a GC reference
retains that GC-backed root classification for later rooting analysis.

A tracked binding or reference-slot assignment cannot retain a borrow formed
from a plain or GC temporary, because the complete expression leaves no stable
owner behind. The same temporary may satisfy a `*T` call parameter when the
borrow ends with that call.

There is no general implicit conversion from `*T` to `T` or `&T`: those
operations would require an explicit payload copy or GC allocation. One
call-only rule is distinct from conversion: because a plain aggregate parameter
`value: T` is passed by reference, a `*T` argument may supply its existing
address as that parameter's call-scoped borrow. Bindings, fields, and returns do
not receive this exception, and `*T` never supplies an `&T` parameter.
Borrow-containing unions and aggregates remain deferred until their transitive
origin rules are implemented.

## 21.4 Current implementation boundary

The frontend currently parses, names, formats, resolves, interns, compares,
capability-qualifies, and template-substitutes tracked-reference types. Its
semantic metadata distinguishes tracked non-owning references from inline,
erased borrowed-view, and GC storage. Expression checking forms tracked borrows,
automatically dereferences them for member access, and records physical place
provenance.

Callable lifetime links, borrow-containing aggregates and unions,
flow-sensitive validity, and GC-owner rooting remain later Phase 7.6 work.
Tracked results therefore cannot yet be returned successfully. Direct tracked
bindings and reference-slot assignments reject temporary sources now; escape
through returned call results is checked with callable lifetime links next.
