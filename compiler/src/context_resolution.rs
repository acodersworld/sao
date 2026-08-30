//! Resolves structural context that can be determined without type information.
//!
//! This pass classifies callables, connects `self`, `return`, `break`, and
//! `continue` uses to their enclosing targets, and validates receiver placement,
//! control-flow context, deferred and coroutine calls, and assignment-target
//! shapes. Name lookup, mutability, type checking, and capture analysis remain
//! the responsibility of other semantic passes.

use std::{collections::HashMap, fmt, mem};

use crate::{
    ast::{
        AnonymousStructMember, Block, ConditionalElse, Declaration, Expression, ExpressionKind,
        FormattedStringPart,
        Function, FunctionParameter, FunctionParameterKind, InterfaceMethodRequirement, NodeId,
        Program, Statement, StatementKind, StructMember, TypeKind, TypeSyntax,
    },
    source::Span,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallableKind {
    TopLevelFunction,
    NestedFunction,
    NamedStructAssociatedFunction,
    NamedStructMethod,
    AnonymousStructMethod,
    GeneratedStructMethod,
    GeneratedStructAssociatedFunction,
    Lambda,
    InterfaceRequirement,
}

#[derive(Debug)]
pub struct ContextResolution {
    /// The semantic role of every named function, lambda, and interface
    /// requirement, keyed by that callable's AST node ID.
    callable_kinds: HashMap<NodeId, CallableKind>,
    /// Maps each `self` expression to the method whose receiver it references.
    self_targets: HashMap<NodeId, NodeId>,
    /// Maps each `return` statement to the callable that it exits.
    return_targets: HashMap<NodeId, NodeId>,
    /// Maps each `break` or `continue` statement to the loop that it targets.
    loop_targets: HashMap<NodeId, NodeId>,
}

impl ContextResolution {
    #[must_use]
    pub fn callable_kind(&self, id: NodeId) -> Option<CallableKind> {
        self.callable_kinds.get(&id).copied()
    }

    #[must_use]
    pub fn method_for_self(&self, id: NodeId) -> Option<NodeId> {
        self.self_targets.get(&id).copied()
    }

    #[must_use]
    pub fn callable_for_return(&self, id: NodeId) -> Option<NodeId> {
        self.return_targets.get(&id).copied()
    }

    #[must_use]
    pub fn loop_for_transfer(&self, id: NodeId) -> Option<NodeId> {
        self.loop_targets.get(&id).copied()
    }

    #[must_use]
    pub const fn callable_kinds(&self) -> &HashMap<NodeId, CallableKind> {
        &self.callable_kinds
    }

    #[must_use]
    pub const fn self_targets(&self) -> &HashMap<NodeId, NodeId> {
        &self.self_targets
    }

    #[must_use]
    pub const fn return_targets(&self) -> &HashMap<NodeId, NodeId> {
        &self.return_targets
    }

    #[must_use]
    pub const fn loop_targets(&self) -> &HashMap<NodeId, NodeId> {
        &self.loop_targets
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextResolutionError {
    pub kind: ContextResolutionErrorKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextResolutionErrorKind {
    ReceiverRequired,
    ReceiverNotAllowed,
    ReceiverMustBeFirst,
    DuplicateReceiver { first: Span },
    SelfOutsideMethod,
    ReturnOutsideCallable,
    BreakOutsideLoop,
    ContinueOutsideLoop,
    DeferOutsideExecutableBlock,
    CoroutineOutsideExecutableBlock,
    InvalidAssignmentTarget,
}

impl fmt::Display for ContextResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ContextResolutionErrorKind::ReceiverRequired => write!(
                formatter,
                "method requires a first-position `self` receiver at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::ReceiverNotAllowed => write!(
                formatter,
                "receiver is not allowed here at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::ReceiverMustBeFirst => write!(
                formatter,
                "receiver must be the first parameter at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::DuplicateReceiver { first } => write!(
                formatter,
                "duplicate receiver at {}..{}; first receiver at {}..{}",
                self.span.start, self.span.end, first.start, first.end
            ),
            ContextResolutionErrorKind::SelfOutsideMethod => write!(
                formatter,
                "`self` is not available outside a method at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::ReturnOutsideCallable => write!(
                formatter,
                "`return` is not inside a callable at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::BreakOutsideLoop => write!(
                formatter,
                "`break` is not inside a loop at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::ContinueOutsideLoop => write!(
                formatter,
                "`continue` is not inside a loop at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::DeferOutsideExecutableBlock => write!(
                formatter,
                "`defer` is not inside an executable block at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::CoroutineOutsideExecutableBlock => write!(
                formatter,
                "`co` is not inside an executable block at {}..{}",
                self.span.start, self.span.end
            ),
            ContextResolutionErrorKind::InvalidAssignmentTarget => write!(
                formatter,
                "expression is not a valid assignment target at {}..{}",
                self.span.start, self.span.end
            ),
        }
    }
}

impl std::error::Error for ContextResolutionError {}

pub type ContextResolutionResult = Result<ContextResolution, Vec<ContextResolutionError>>;

/// Resolves structural context and validates its rules for one parsed program.
///
/// This pass deliberately does not depend on name resolution or type
/// information. In particular, every mutability restriction is left to type
/// checking. The semantic pipeline should still invoke this pass after
/// successful name resolution.
pub fn resolve_program_context(program: &Program) -> ContextResolutionResult {
    ContextResolver::new().resolve(program)
}

#[derive(Clone, Copy)]
enum ReceiverPolicy {
    Forbidden,
    OptionalFirst,
    RequiredFirst,
}

struct ContextResolver {
    // Semantic metadata accumulated for the successful resolution result.
    callable_kinds: HashMap<NodeId, CallableKind>,
    self_targets: HashMap<NodeId, NodeId>,
    return_targets: HashMap<NodeId, NodeId>,
    loop_targets: HashMap<NodeId, NodeId>,
    // Diagnostics accumulated in deterministic source traversal order.
    errors: Vec<ContextResolutionError>,
    // Active callables. The last entry is the target of `return`.
    callable_stack: Vec<NodeId>,
    // Lexically enclosing methods. Unlike loop context, this remains visible
    // through lambdas and nested named functions so capture analysis can later
    // accept or reject the reference as appropriate.
    method_stack: Vec<NodeId>,
    // Active loops in the current callable. Entering a callable temporarily
    // clears this stack so `break` and `continue` cannot cross that boundary.
    loop_stack: Vec<NodeId>,
    // Number of executable blocks currently being visited. `defer` and `co`
    // statements require this to be nonzero.
    executable_block_depth: usize,
}

impl ContextResolver {
    fn new() -> Self {
        Self {
            callable_kinds: HashMap::new(),
            self_targets: HashMap::new(),
            return_targets: HashMap::new(),
            loop_targets: HashMap::new(),
            errors: Vec::new(),
            callable_stack: Vec::new(),
            method_stack: Vec::new(),
            loop_stack: Vec::new(),
            executable_block_depth: 0,
        }
    }

    fn resolve(mut self, program: &Program) -> ContextResolutionResult {
        for declaration in &program.declarations {
            self.visit_declaration(declaration);
        }

        if self.errors.is_empty() {
            Ok(ContextResolution {
                callable_kinds: self.callable_kinds,
                self_targets: self.self_targets,
                return_targets: self.return_targets,
                loop_targets: self.loop_targets,
            })
        } else {
            Err(self.errors)
        }
    }

    fn visit_declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Function(function) => {
                self.validate_receivers(
                    function.name,
                    &function.parameters,
                    ReceiverPolicy::Forbidden,
                );
                self.visit_function(function, CallableKind::TopLevelFunction);
            }
            Declaration::Struct(structure) => {
                for member in &structure.members {
                    if let StructMember::Function(function) = member {
                        let has_receiver = self.validate_receivers(
                            function.name,
                            &function.parameters,
                            ReceiverPolicy::OptionalFirst,
                        );
                        let kind = if has_receiver {
                            CallableKind::NamedStructMethod
                        } else {
                            CallableKind::NamedStructAssociatedFunction
                        };
                        self.visit_function(function, kind);
                    }
                }
            }
            Declaration::Interface(interface) => {
                for requirement in &interface.requirements {
                    self.visit_interface_requirement(requirement);
                }
            }
            Declaration::TypeAlias(_) => {}
        }
    }

    fn visit_interface_requirement(&mut self, requirement: &InterfaceMethodRequirement) {
        self.validate_receivers(
            requirement.name,
            &requirement.parameters,
            ReceiverPolicy::RequiredFirst,
        );
        self.callable_kinds
            .insert(requirement.id, CallableKind::InterfaceRequirement);
    }

    fn validate_receivers(
        &mut self,
        callable_name: Span,
        parameters: &[FunctionParameter],
        policy: ReceiverPolicy,
    ) -> bool {
        let receivers: Vec<_> = parameters
            .iter()
            .enumerate()
            .filter(|(_, parameter)| {
                matches!(&parameter.kind, FunctionParameterKind::Receiver { .. })
            })
            .collect();

        if matches!(policy, ReceiverPolicy::Forbidden) {
            for (_, receiver) in &receivers {
                self.error(
                    ContextResolutionErrorKind::ReceiverNotAllowed,
                    receiver.span,
                );
            }
            return false;
        }

        let Some((first_index, first_receiver)) = receivers.first().copied() else {
            if matches!(policy, ReceiverPolicy::RequiredFirst) {
                self.error(ContextResolutionErrorKind::ReceiverRequired, callable_name);
            }
            return false;
        };

        if first_index != 0 {
            self.error(
                ContextResolutionErrorKind::ReceiverMustBeFirst,
                first_receiver.span,
            );
        }

        for (_, receiver) in receivers.iter().skip(1) {
            self.error(
                ContextResolutionErrorKind::DuplicateReceiver {
                    first: first_receiver.span,
                },
                receiver.span,
            );
        }

        true
    }

    fn visit_function(&mut self, function: &Function, kind: CallableKind) {
        let establishes_self_context = matches!(
            kind,
            CallableKind::NamedStructMethod
                | CallableKind::AnonymousStructMethod
                | CallableKind::GeneratedStructMethod
        );
        self.callable_kinds.insert(function.id, kind);
        self.enter_callable(function.id, establishes_self_context, &function.body);
    }

    fn enter_callable(&mut self, id: NodeId, is_method: bool, body: &Block) {
        let enclosing_loops = mem::take(&mut self.loop_stack);
        self.callable_stack.push(id);
        if is_method {
            self.method_stack.push(id);
        }

        self.visit_block(body);

        if is_method {
            self.method_stack.pop();
        }
        self.callable_stack.pop();
        self.loop_stack = enclosing_loops;
    }

    fn visit_block(&mut self, block: &Block) {
        self.executable_block_depth += 1;
        for statement in &block.statements {
            self.visit_statement(statement);
        }
        if let Some(value) = &block.value {
            self.visit_expression(value);
        }
        self.executable_block_depth -= 1;
    }

    fn visit_statement(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::Binding { initializer, .. } | StatementKind::Expression(initializer) => {
                self.visit_expression(initializer)
            }
            StatementKind::Function(function) => {
                self.validate_receivers(
                    function.name,
                    &function.parameters,
                    ReceiverPolicy::Forbidden,
                );
                self.visit_function(function, CallableKind::NestedFunction);
            }
            StatementKind::Defer(call) => {
                if self.executable_block_depth == 0 {
                    self.error(
                        ContextResolutionErrorKind::DeferOutsideExecutableBlock,
                        statement.span,
                    );
                }
                self.visit_expression(call);
            }
            StatementKind::Coroutine(call) => {
                if self.executable_block_depth == 0 {
                    self.error(
                        ContextResolutionErrorKind::CoroutineOutsideExecutableBlock,
                        statement.span,
                    );
                }
                self.visit_expression(call);
            }
            StatementKind::Break(value) => {
                if let Some(target) = self.loop_stack.last() {
                    self.loop_targets.insert(statement.id, *target);
                } else {
                    self.error(ContextResolutionErrorKind::BreakOutsideLoop, statement.span);
                }
                if let Some(value) = value {
                    self.visit_expression(value);
                }
            }
            StatementKind::Continue => {
                if let Some(target) = self.loop_stack.last() {
                    self.loop_targets.insert(statement.id, *target);
                } else {
                    self.error(
                        ContextResolutionErrorKind::ContinueOutsideLoop,
                        statement.span,
                    );
                }
            }
            StatementKind::Return(value) => {
                if let Some(target) = self.callable_stack.last() {
                    self.return_targets.insert(statement.id, *target);
                } else {
                    self.error(
                        ContextResolutionErrorKind::ReturnOutsideCallable,
                        statement.span,
                    );
                }
                if let Some(value) = value {
                    self.visit_expression(value);
                }
            }
        }
    }

    fn visit_expression(&mut self, expression: &Expression) {
        match &expression.kind {
            ExpressionKind::Identifier | ExpressionKind::Literal(_) => {}
            ExpressionKind::TypeValue(type_syntax) => self.visit_type(type_syntax),
            ExpressionKind::FormattedString { parts } => {
                for part in parts {
                    if let FormattedStringPart::Interpolation { value, .. } = part {
                        self.visit_expression(value);
                    }
                }
            }
            ExpressionKind::SelfValue => {
                if let Some(method) = self.method_stack.last() {
                    self.self_targets.insert(expression.id, *method);
                } else {
                    self.error(
                        ContextResolutionErrorKind::SelfOutsideMethod,
                        expression.span,
                    );
                }
            }
            ExpressionKind::Group(inner) => self.visit_expression(inner),
            ExpressionKind::Tuple { elements } => {
                for element in elements {
                    self.visit_expression(element);
                }
            }
            ExpressionKind::Block(block) => self.visit_block(block),
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
            ExpressionKind::Loop { body } => {
                self.visit_loop_body(expression.id, body);
            }
            ExpressionKind::While {
                condition,
                body,
                else_branch,
            } => {
                self.visit_expression(condition);
                self.visit_loop_body(expression.id, body);
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
                self.visit_loop_body(expression.id, body);
                if let Some(else_branch) = else_branch {
                    self.visit_block(else_branch);
                }
            }
            ExpressionKind::Lambda { body, .. } => {
                self.callable_kinds
                    .insert(expression.id, CallableKind::Lambda);
                self.enter_callable(expression.id, false, body);
            }
            ExpressionKind::GcAllocate(value) => self.visit_expression(value),
            ExpressionKind::StructConstruction { owner, fields } => {
                self.visit_type(owner);
                for field in fields {
                    self.visit_expression(&field.value);
                }
            }
            ExpressionKind::AnonymousStruct { members } => {
                for member in members {
                    match member {
                        AnonymousStructMember::Field(field) => {
                            self.visit_expression(&field.initializer);
                        }
                        AnonymousStructMember::Method(method) => {
                            self.validate_receivers(
                                method.name,
                                &method.parameters,
                                ReceiverPolicy::RequiredFirst,
                            );
                            self.visit_function(method, CallableKind::AnonymousStructMethod);
                        }
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
            ExpressionKind::Try { expression } => self.visit_expression(expression),
            ExpressionKind::TypeTest { value, .. }
            | ExpressionKind::TypeAscription { value, .. } => self.visit_expression(value),
            ExpressionKind::Unary { operand, .. } => self.visit_expression(operand),
            ExpressionKind::Binary { left, right, .. } => {
                self.visit_expression(left);
                self.visit_expression(right);
            }
            ExpressionKind::Assignment { target, value, .. } => {
                if !matches!(
                    &target.kind,
                    ExpressionKind::Identifier
                        | ExpressionKind::MemberAccess { .. }
                        | ExpressionKind::Index { .. }
                ) {
                    self.error(
                        ContextResolutionErrorKind::InvalidAssignmentTarget,
                        target.span,
                    );
                }
                self.visit_expression(target);
                self.visit_expression(value);
            }
        }
    }

    fn visit_loop_body(&mut self, id: NodeId, body: &Block) {
        self.loop_stack.push(id);
        self.visit_block(body);
        self.loop_stack.pop();
    }

    fn visit_type(&mut self, type_syntax: &TypeSyntax) {
        match &type_syntax.kind {
            TypeKind::GeneratedStruct { members } => {
                for member in members {
                    if let StructMember::Function(function) = member {
                        let has_receiver = self.validate_receivers(
                            function.name,
                            &function.parameters,
                            ReceiverPolicy::OptionalFirst,
                        );
                        let kind = if has_receiver {
                            CallableKind::GeneratedStructMethod
                        } else {
                            CallableKind::GeneratedStructAssociatedFunction
                        };
                        self.visit_function(function, kind);
                    }
                }
            }
            TypeKind::Associated {
                owner, arguments, ..
            } => {
                self.visit_type(owner);
                for argument in arguments {
                    self.visit_type(argument);
                }
            }
            TypeKind::Builtin { arguments, .. } | TypeKind::Named { arguments, .. } => {
                for argument in arguments {
                    self.visit_type(argument);
                }
            }
            TypeKind::Mutable(inner)
            | TypeKind::Gc(inner)
            | TypeKind::Tracked(inner)
            | TypeKind::Group(inner) => {
                self.visit_type(inner);
            }
            TypeKind::Tuple { elements } => {
                for element in elements {
                    self.visit_type(element);
                }
            }
            TypeKind::Callable {
                parameters,
                return_type,
            } => {
                for parameter in parameters {
                    self.visit_type(parameter);
                }
                self.visit_type(return_type);
            }
            TypeKind::Intersection { members } | TypeKind::Union { members } => {
                for member in members {
                    self.visit_type(member);
                }
            }
            TypeKind::ComptimeType | TypeKind::Primitive(_) => {}
        }
    }

    fn error(&mut self, kind: ContextResolutionErrorKind, span: Span) {
        self.errors.push(ContextResolutionError { kind, span });
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

    fn parse(source: &str) -> Program {
        let module = SourceModuleRegistry::new().add(source);
        let mut context = ParseContext::new(module.module_id());
        parse_program(&mut context, Lexer::new(&module)).expect("test program should parse")
    }

    fn function(declaration: &Declaration) -> &Function {
        let Declaration::Function(function) = declaration else {
            panic!("expected a function declaration");
        };
        function
    }

    fn expression(statement: &Statement) -> &Expression {
        let StatementKind::Expression(expression) = &statement.kind else {
            panic!("expected an expression statement");
        };
        expression
    }

    #[test]
    fn classifies_every_callable_context() {
        let program = parse(concat!(
            "fn helper() {}\n",
            "struct Named {\n",
            "    fn associated() {}\n",
            "    fn method(self) {}\n",
            "}\n",
            "interface Required { fn call(self); }\n",
            "fn main() {\n",
            "    fn nested() {}\n",
            "    const anonymous = struct { fn run(self) {} };\n",
            "    const closure = lambda() {};\n",
            "}\n",
        ));
        let resolution =
            resolve_program_context(&program).expect("callable contexts should be valid");

        let helper = function(&program.declarations[0]);
        let Declaration::Struct(named) = &program.declarations[1] else {
            panic!("expected a struct declaration");
        };
        let StructMember::Function(associated) = &named.members[0] else {
            panic!("expected an associated function");
        };
        let StructMember::Function(method) = &named.members[1] else {
            panic!("expected a method");
        };
        let Declaration::Interface(interface) = &program.declarations[2] else {
            panic!("expected an interface declaration");
        };
        let requirement = &interface.requirements[0];
        let main = function(&program.declarations[3]);
        let StatementKind::Function(nested) = &main.body.statements[0].kind else {
            panic!("expected a nested function");
        };
        let StatementKind::Binding {
            initializer: anonymous,
            ..
        } = &main.body.statements[1].kind
        else {
            panic!("expected an anonymous struct binding");
        };
        let ExpressionKind::AnonymousStruct { members } = &anonymous.kind else {
            panic!("expected an anonymous struct");
        };
        let AnonymousStructMember::Method(anonymous_method) = &members[0] else {
            panic!("expected an anonymous struct method");
        };
        let StatementKind::Binding {
            initializer: lambda,
            ..
        } = &main.body.statements[2].kind
        else {
            panic!("expected a lambda binding");
        };

        assert_eq!(
            resolution.callable_kind(helper.id),
            Some(CallableKind::TopLevelFunction)
        );
        assert_eq!(
            resolution.callable_kind(associated.id),
            Some(CallableKind::NamedStructAssociatedFunction)
        );
        assert_eq!(
            resolution.callable_kind(method.id),
            Some(CallableKind::NamedStructMethod)
        );
        assert_eq!(
            resolution.callable_kind(requirement.id),
            Some(CallableKind::InterfaceRequirement)
        );
        assert_eq!(
            resolution.callable_kind(main.id),
            Some(CallableKind::TopLevelFunction)
        );
        assert_eq!(
            resolution.callable_kind(nested.id),
            Some(CallableKind::NestedFunction)
        );
        assert_eq!(
            resolution.callable_kind(anonymous_method.id),
            Some(CallableKind::AnonymousStructMethod)
        );
        assert_eq!(
            resolution.callable_kind(lambda.id),
            Some(CallableKind::Lambda)
        );
    }

    #[test]
    fn reports_receiver_errors_without_redundant_missing_receiver_errors() {
        let program = parse(concat!(
            "fn forbidden(self) {}\n",
            "struct Named {\n",
            "    fn late(value: int, self) {}\n",
            "    fn duplicate(self, mut self) {}\n",
            "}\n",
            "interface Required { fn missing(); }\n",
            "fn main() {\n",
            "    const value = struct { fn missing() {} };\n",
            "}\n",
        ));
        let errors = resolve_program_context(&program).expect_err("receivers are invalid");

        assert_eq!(errors.len(), 5);
        assert_eq!(
            errors[0].kind,
            ContextResolutionErrorKind::ReceiverNotAllowed
        );
        assert_eq!(
            errors[1].kind,
            ContextResolutionErrorKind::ReceiverMustBeFirst
        );
        assert!(matches!(
            &errors[2].kind,
            ContextResolutionErrorKind::DuplicateReceiver { .. }
        ));
        assert_eq!(errors[3].kind, ContextResolutionErrorKind::ReceiverRequired);
        assert_eq!(errors[4].kind, ContextResolutionErrorKind::ReceiverRequired);
    }

    #[test]
    fn resolves_lexically_captured_self_to_the_owning_method() {
        let program = parse(concat!(
            "struct Outer {\n",
            "    fn method(self) {\n",
            "        self;\n",
            "        const closure = lambda() { self; };\n",
            "        fn nested() { self; }\n",
            "        const anonymous = struct {\n",
            "            captured = self;\n",
            "            fn own(self) { self; }\n",
            "        };\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        ));
        let resolution = resolve_program_context(&program).expect("self uses should be contextual");
        let Declaration::Struct(outer) = &program.declarations[0] else {
            panic!("expected a struct declaration");
        };
        let StructMember::Function(method) = &outer.members[0] else {
            panic!("expected a method");
        };
        let StatementKind::Binding {
            initializer: anonymous,
            ..
        } = &method.body.statements[3].kind
        else {
            panic!("expected an anonymous struct binding");
        };
        let ExpressionKind::AnonymousStruct { members } = &anonymous.kind else {
            panic!("expected an anonymous struct");
        };
        let AnonymousStructMember::Method(own) = &members[1] else {
            panic!("expected an anonymous struct method");
        };

        assert_eq!(resolution.self_targets().len(), 5);
        assert_eq!(
            resolution
                .self_targets()
                .values()
                .filter(|target| **target == method.id)
                .count(),
            4
        );
        assert_eq!(
            resolution
                .self_targets()
                .values()
                .filter(|target| **target == own.id)
                .count(),
            1
        );
    }

    #[test]
    fn rejects_self_without_an_enclosing_method() {
        let program = parse(concat!(
            "struct Named { fn associated() { self; } }\n",
            "fn main() { self; }\n",
        ));
        let errors = resolve_program_context(&program).expect_err("self is unavailable");

        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.kind == ContextResolutionErrorKind::SelfOutsideMethod)
        );
    }

    #[test]
    fn records_nearest_control_targets_and_excludes_loop_else_blocks() {
        let program = parse(concat!(
            "fn main() {\n",
            "    loop {\n",
            "        while true {\n",
            "            break;\n",
            "            continue;\n",
            "        } else {\n",
            "            break;\n",
            "            continue;\n",
            "        };\n",
            "        break;\n",
            "        continue;\n",
            "    };\n",
            "    const closure = lambda() { return; };\n",
            "    return;\n",
            "}\n",
        ));
        let resolution =
            resolve_program_context(&program).expect("control transfers should be valid");
        let main = function(&program.declarations[0]);
        let outer_loop = expression(&main.body.statements[0]);
        let ExpressionKind::Loop { body: outer_body } = &outer_loop.kind else {
            panic!("expected an outer loop");
        };
        let inner_loop = expression(&outer_body.statements[0]);
        let ExpressionKind::While {
            body: inner_body,
            else_branch: Some(else_body),
            ..
        } = &inner_loop.kind
        else {
            panic!("expected a while loop with else");
        };

        for statement in &inner_body.statements {
            assert_eq!(
                resolution.loop_for_transfer(statement.id),
                Some(inner_loop.id)
            );
        }
        for statement in else_body
            .statements
            .iter()
            .chain(outer_body.statements[1..].iter())
        {
            assert_eq!(
                resolution.loop_for_transfer(statement.id),
                Some(outer_loop.id)
            );
        }

        let StatementKind::Binding {
            initializer: lambda,
            ..
        } = &main.body.statements[1].kind
        else {
            panic!("expected a lambda binding");
        };
        let ExpressionKind::Lambda {
            body: lambda_body, ..
        } = &lambda.kind
        else {
            panic!("expected a lambda");
        };
        assert_eq!(
            resolution.callable_for_return(lambda_body.statements[0].id),
            Some(lambda.id)
        );
        assert_eq!(
            resolution.callable_for_return(main.body.statements[2].id),
            Some(main.id)
        );
    }

    #[test]
    fn loop_control_cannot_cross_a_callable_boundary() {
        let program = parse(concat!(
            "fn main() {\n",
            "    loop {\n",
            "        fn nested() { break; continue; }\n",
            "        const closure = lambda() { break; continue; };\n",
            "    };\n",
            "}\n",
        ));
        let errors =
            resolve_program_context(&program).expect_err("callables cannot escape outer loops");

        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0].kind, ContextResolutionErrorKind::BreakOutsideLoop);
        assert_eq!(
            errors[1].kind,
            ContextResolutionErrorKind::ContinueOutsideLoop
        );
        assert_eq!(errors[2].kind, ContextResolutionErrorKind::BreakOutsideLoop);
        assert_eq!(
            errors[3].kind,
            ContextResolutionErrorKind::ContinueOutsideLoop
        );
    }

    #[test]
    fn accepts_only_ungrouped_location_assignment_roots() {
        let valid = parse(concat!(
            "struct Named {\n",
            "    field: int,\n",
            "    fn method(mut self, object: Named, items: Vector(int), index: int) {\n",
            "        object = self;\n",
            "        self.field = 1;\n",
            "        (object).field = 2;\n",
            "        items[index] = 3;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        ));
        resolve_program_context(&valid).expect("location roots should be valid");

        let invalid = parse(concat!(
            "struct Named {\n",
            "    fn method(mut self) {\n",
            "        (value) = 1;\n",
            "        self = other;\n",
            "        items[0..1] = other;\n",
            "        make() = other;\n",
            "        value + 1 = 2;\n",
            "    }\n",
            "}\n",
            "fn main() {}\n",
        ));
        let errors =
            resolve_program_context(&invalid).expect_err("assignment roots should be invalid");

        assert_eq!(errors.len(), 5);
        assert!(
            errors
                .iter()
                .all(|error| error.kind == ContextResolutionErrorKind::InvalidAssignmentTarget)
        );
    }

    #[test]
    fn aggregates_diagnostics_in_source_traversal_order() {
        let program = parse("fn main() { break; self; (value) = 1; continue; }");
        let errors = resolve_program_context(&program).expect_err("program is invalid");
        let kinds: Vec<_> = errors.into_iter().map(|error| error.kind).collect();

        assert_eq!(
            kinds,
            vec![
                ContextResolutionErrorKind::BreakOutsideLoop,
                ContextResolutionErrorKind::SelfOutsideMethod,
                ContextResolutionErrorKind::InvalidAssignmentTarget,
                ContextResolutionErrorKind::ContinueOutsideLoop,
            ]
        );
    }

    #[test]
    fn accepts_defer_and_coroutine_calls_in_executable_blocks() {
        let program = parse(concat!(
            "fn main() {\n",
            "    {\n",
            "        defer cleanup();\n",
            "        co worker();\n",
            "    };\n",
            "}\n",
        ));

        resolve_program_context(&program).expect("calls are in an executable block");
    }
}
