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
Plain unions and aggregates may contain tracked references under the transitive
origin rules in Section 21.5.

## 21.4 Callable lifetime links

A callable which returns `*T` links that result to every tracked parameter in
its signature. At each call site, the result carries the intersection of the
physical lifetimes supplied for those parameters. Plain and GC-backed values
may form the tracked arguments locally, but ordinary plain aggregate parameters
and declared `&T` parameters never contribute merely because their values are
borrowable inside the callable.

`*self` and `*mut self` are the tracked receiver forms. They contribute to a
tracked method result exactly like a named tracked parameter. Plain `self`
remains a call-scoped receiver and `&self` remains GC-owned; neither can be the
origin of an escaping tracked result.

Every tracked return expression must derive exclusively from tracked parameters
or a tracked receiver, possibly through stable inline field and tuple paths or
through another linked call. Locals, ordinary parameters, GC parameters, and
fresh plain or GC temporaries are invalid return origins. The signature-level
link is deliberately conservative: all tracked inputs contribute even when the
implementation's returned path uses only one of them.

A linked result cannot escape a complete expression when any contributing
caller-side input is temporary. Calls may still borrow such an input when their
tracked result is consumed without escaping. No hidden storage or temporary
lifetime extension is introduced.

## 21.5 Borrow-containing inline values

A plain struct, tuple, union, or other inline aggregate may contain tracked
references. The complete value carries the intersection of every origin stored
within it, including origins nested through other inline aggregates. Constructing,
copying, binding, projecting, injecting or widening a union, returning, and
passing such a value preserves that intersection. Projecting one tracked-bearing
part currently retains the complete aggregate intersection conservatively.

A callable returning a borrow-containing inline value links the result to its
tracked parameters and to parameters which themselves contain tracked references.
The latter do not become valid origins for a direct `*T` return: the stricter
tracked-return rule in Section 21.4 still applies. An inline tracked-bearing
receiver contributes in the same way when a method propagates an inline value.

Tracked-bearing values cannot cross storage boundaries which could outlive or
relocate their backing storage. They cannot be GC allocated, nested beneath a GC
field, or used as an element, key, or value in `Queue`, `Vector`, or `Map`
external-buffer storage. `Error(T)`, tuples, unions, and plain structs remain
inline and therefore propagate tracked origins transitively.

## 21.6 Current implementation boundary

The frontend currently parses, names, formats, resolves, interns, compares,
capability-qualifies, and template-substitutes tracked-reference types. Its
semantic metadata distinguishes tracked non-owning references from inline,
erased borrowed-view, and GC storage. Expression checking forms tracked borrows,
automatically dereferences them for member access, and records physical place
provenance.

Callable lifetime links, tracked receivers, tracked-return origin validation,
caller-side temporary escape checks, and borrow-containing inline aggregates
and unions are implemented. Flow-sensitive validity and GC-owner rooting remain
later Phase 7.6 work.
