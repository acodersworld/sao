use std::{collections::HashMap, fmt};

use crate::lexer::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCreationError {
    ParentNotFound { parent: ScopeId },
}

impl fmt::Display for ScopeCreationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParentNotFound { parent } => {
                write!(formatter, "parent scope {parent:?} does not exist")
            }
        }
    }
}

impl std::error::Error for ScopeCreationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Value,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclareError {
    ScopeNotFound {
        scope: ScopeId,
    },
    DuplicateDeclaration {
        name: String,
        original: Span,
        duplicate: Span,
    },
}

impl fmt::Display for DeclareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScopeNotFound { scope } => write!(formatter, "scope {scope:?} does not exist"),
            Self::DuplicateDeclaration {
                name,
                original,
                duplicate,
            } => write!(
                formatter,
                "duplicate declaration of `{name}` at {}..{}; first declared at {}..{}",
                duplicate.start, duplicate.end, original.start, original.end
            ),
        }
    }
}

impl std::error::Error for DeclareError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolLookupError {
    ScopeNotFound { scope: ScopeId },
    SymbolNotFound { namespace: Namespace, name: String },
}

impl fmt::Display for SymbolLookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScopeNotFound { scope } => write!(formatter, "scope {scope:?} does not exist"),
            Self::SymbolNotFound { namespace, name } => {
                write!(formatter, "{namespace:?} symbol `{name}` was not found")
            }
        }
    }
}

impl std::error::Error for SymbolLookupError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    BuiltinValue,
    Function,
    Binding,
    Parameter,
    RangeBinding,
    Struct,
    Interface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
}

#[derive(Debug)]
struct Scope {
    parent: Option<ScopeId>,
    values: HashMap<String, SymbolId>,
    types: HashMap<String, SymbolId>,
}

#[derive(Debug)]
pub struct SymbolTable {
    symbols: HashMap<SymbolId, Symbol>,
    scopes: HashMap<ScopeId, Scope>,
    next_scope_id: usize,
    next_symbol_id: usize,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: Default::default(),
            scopes: HashMap::from([(
                ScopeId(0),
                Scope {
                    parent: None,
                    values: Default::default(),
                    types: Default::default(),
                },
            )]),
            next_scope_id: 1,
            next_symbol_id: 0,
        }
    }

    pub fn root_scope(&self) -> ScopeId {
        ScopeId(0)
    }

    pub fn new_child_scope(&mut self, parent: ScopeId) -> Result<ScopeId, ScopeCreationError> {
        if !self.scopes.contains_key(&parent) {
            return Err(ScopeCreationError::ParentNotFound { parent });
        }

        let scope_id = ScopeId(self.next_scope_id);
        self.next_scope_id += 1;

        let new_scope = Scope {
            parent: Some(parent),
            values: Default::default(),
            types: Default::default(),
        };

        self.scopes.insert(scope_id, new_scope);
        Ok(scope_id)
    }

    fn next_symbol_id(&mut self) -> SymbolId {
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        id
    }

    fn new_symbol(&mut self, symbol_id: SymbolId, name: &str, kind: SymbolKind, span: Span) {
        self.symbols.insert(
            symbol_id,
            Symbol {
                name: name.to_string(),
                kind,
                span,
            },
        );
    }

    pub fn declare(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: SymbolKind,
        span: Span,
    ) -> Result<SymbolId, DeclareError> {
        let symbol_id = self.next_symbol_id();
        let Some(scope) = self.scopes.get_mut(&scope) else {
            return Err(DeclareError::ScopeNotFound { scope });
        };

        let table = match kind {
            SymbolKind::Struct | SymbolKind::Interface => {
                if let Some(existing) = scope.types.get(name) {
                    let original = self
                        .symbols
                        .get(existing)
                        .expect("declared symbol must exist");
                    return Err(DeclareError::DuplicateDeclaration {
                        name: name.to_string(),
                        original: original.span,
                        duplicate: span,
                    });
                }

                &mut scope.types
            }
            SymbolKind::Parameter => {
                if let Some(id) = scope.values.get(name) {
                    let dup = self.symbols.get(&id).unwrap();
                    if matches!(
                        dup.kind,
                        SymbolKind::Parameter | SymbolKind::Function | SymbolKind::BuiltinValue
                    ) {
                        return Err(DeclareError::DuplicateDeclaration {
                            name: name.to_string(),
                            original: dup.span,
                            duplicate: span,
                        });
                    }
                }
                &mut scope.values
            }
            SymbolKind::BuiltinValue | SymbolKind::Function => {
                if let Some(existing) = scope.values.get(name) {
                    let original = self
                        .symbols
                        .get(existing)
                        .expect("declared symbol must exist");
                    return Err(DeclareError::DuplicateDeclaration {
                        name: name.to_string(),
                        original: original.span,
                        duplicate: span,
                    });
                }
                &mut scope.values
            }
            SymbolKind::Binding | SymbolKind::RangeBinding => {
                if let Some(id) = scope.values.get(name) {
                    let dup = self.symbols.get(&id).unwrap();
                    if matches!(dup.kind, SymbolKind::Function | SymbolKind::BuiltinValue) {
                        return Err(DeclareError::DuplicateDeclaration {
                            name: name.to_string(),
                            original: dup.span,
                            duplicate: span,
                        });
                    }
                }
                &mut scope.values
            }
        };

        table.insert(name.to_string(), symbol_id);

        self.new_symbol(symbol_id, name, kind, span);
        Ok(symbol_id)
    }

    pub fn lookup_value(&self, scope: ScopeId, name: &str) -> Result<SymbolId, SymbolLookupError> {
        let Some(scope) = self.scopes.get(&scope) else {
            return Err(SymbolLookupError::ScopeNotFound { scope });
        };

        if let Some(symbol) = scope.values.get(name) {
            Ok(*symbol)
        } else if let Some(parent) = scope.parent {
            self.lookup_value(parent, name)
        } else {
            Err(SymbolLookupError::SymbolNotFound {
                namespace: Namespace::Value,
                name: name.to_string(),
            })
        }
    }

    pub fn lookup_type(&self, scope: ScopeId, name: &str) -> Result<SymbolId, SymbolLookupError> {
        let Some(scope) = self.scopes.get(&scope) else {
            return Err(SymbolLookupError::ScopeNotFound { scope });
        };

        if let Some(symbol) = scope.types.get(name) {
            Ok(*symbol)
        } else if let Some(parent) = scope.parent {
            self.lookup_type(parent, name)
        } else {
            Err(SymbolLookupError::SymbolNotFound {
                namespace: Namespace::Type,
                name: name.to_string(),
            })
        }
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_declared(result: Result<SymbolId, DeclareError>) -> SymbolId {
        match result {
            Ok(symbol) => symbol,
            Err(_) => panic!("expected declaration to succeed"),
        }
    }

    fn expect_found(result: Result<SymbolId, SymbolLookupError>) -> SymbolId {
        match result {
            Ok(symbol) => symbol,
            Err(_) => panic!("expected symbol to be found"),
        }
    }

    #[test]
    fn looks_up_a_value_in_the_root_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let declaration = Span::new(0, 4);

        let main =
            expect_declared(symbols.declare(root, "main", SymbolKind::Function, declaration));

        assert_eq!(expect_found(symbols.lookup_value(root, "main")), main);
        assert!(matches!(
            symbols.lookup_type(root, "main"),
            Err(SymbolLookupError::SymbolNotFound {
                namespace: Namespace::Type,
                name,
            }) if name == "main"
        ));
        assert_eq!(
            symbols.symbol(main),
            Some(&Symbol {
                name: "main".to_string(),
                kind: SymbolKind::Function,
                span: declaration,
            })
        );
    }

    #[test]
    fn reports_the_missing_parent_when_creating_a_child_scope() {
        let mut symbols = SymbolTable::new();
        let missing = ScopeId(99);

        let error = symbols.new_child_scope(missing);

        assert_eq!(
            error,
            Err(ScopeCreationError::ParentNotFound { parent: missing })
        );
        assert_eq!(
            error.unwrap_err().to_string(),
            "parent scope ScopeId(99) does not exist"
        );
    }

    #[test]
    fn looks_up_a_value_from_a_parent_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let outer =
            expect_declared(symbols.declare(root, "outer", SymbolKind::Binding, Span::new(0, 5)));
        let child = symbols
            .new_child_scope(root)
            .expect("root scope should exist");

        assert_eq!(expect_found(symbols.lookup_value(child, "outer")), outer);
    }

    #[test]
    fn an_inner_declaration_shadows_its_parent() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let outer =
            expect_declared(symbols.declare(root, "value", SymbolKind::Binding, Span::new(0, 5)));
        let child = symbols
            .new_child_scope(root)
            .expect("root scope should exist");
        let inner = expect_declared(symbols.declare(
            child,
            "value",
            SymbolKind::Binding,
            Span::new(10, 15),
        ));

        assert_ne!(inner, outer);
        assert_eq!(expect_found(symbols.lookup_value(child, "value")), inner);
        assert_eq!(expect_found(symbols.lookup_value(root, "value")), outer);
    }

    #[test]
    fn a_function_in_a_child_scope_shadows_a_parent_function() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let outer =
            expect_declared(symbols.declare(root, "run", SymbolKind::Function, Span::new(0, 3)));
        let child = symbols
            .new_child_scope(root)
            .expect("root scope should exist");
        let inner =
            expect_declared(symbols.declare(child, "run", SymbolKind::Function, Span::new(10, 13)));

        assert_ne!(inner, outer);
        assert_eq!(expect_found(symbols.lookup_value(child, "run")), inner);
        assert_eq!(expect_found(symbols.lookup_value(root, "run")), outer);
    }

    #[test]
    fn rejects_duplicate_functions_in_the_same_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let original =
            expect_declared(symbols.declare(root, "run", SymbolKind::Function, Span::new(0, 3)));

        let duplicate = symbols.declare(root, "run", SymbolKind::Function, Span::new(10, 13));

        assert_eq!(
            duplicate,
            Err(DeclareError::DuplicateDeclaration {
                name: "run".to_string(),
                original: Span::new(0, 3),
                duplicate: Span::new(10, 13),
            })
        );
        assert_eq!(expect_found(symbols.lookup_value(root, "run")), original);
        assert_eq!(symbols.symbols.len(), 1);
    }

    #[test]
    fn rejects_a_binding_that_conflicts_with_a_same_scope_function() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let function =
            expect_declared(symbols.declare(root, "run", SymbolKind::Function, Span::new(0, 3)));

        let binding = symbols.declare(root, "run", SymbolKind::Binding, Span::new(10, 13));

        assert!(matches!(
            binding,
            Err(DeclareError::DuplicateDeclaration { .. })
        ));
        assert_eq!(expect_found(symbols.lookup_value(root, "run")), function);
        assert_eq!(symbols.symbols.len(), 1);
    }

    #[test]
    fn rejects_a_function_that_conflicts_with_a_same_scope_binding() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let binding =
            expect_declared(symbols.declare(root, "run", SymbolKind::Binding, Span::new(0, 3)));

        let function = symbols.declare(root, "run", SymbolKind::Function, Span::new(10, 13));

        assert!(matches!(
            function,
            Err(DeclareError::DuplicateDeclaration { .. })
        ));
        assert_eq!(expect_found(symbols.lookup_value(root, "run")), binding);
        assert_eq!(symbols.symbols.len(), 1);
    }

    #[test]
    fn a_later_binding_shadows_an_earlier_binding_in_the_same_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let earlier =
            expect_declared(symbols.declare(root, "value", SymbolKind::Binding, Span::new(0, 5)));

        assert_eq!(expect_found(symbols.lookup_value(root, "value")), earlier);

        let later =
            expect_declared(symbols.declare(root, "value", SymbolKind::Binding, Span::new(10, 15)));

        assert_ne!(later, earlier);
        assert_eq!(expect_found(symbols.lookup_value(root, "value")), later);
        assert_eq!(symbols.symbols.len(), 2);
    }

    #[test]
    fn a_binding_shadows_a_parameter_in_the_same_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let parameter =
            expect_declared(symbols.declare(root, "value", SymbolKind::Parameter, Span::new(0, 5)));

        assert_eq!(expect_found(symbols.lookup_value(root, "value")), parameter);

        let binding =
            expect_declared(symbols.declare(root, "value", SymbolKind::Binding, Span::new(10, 15)));

        assert_ne!(binding, parameter);
        assert_eq!(expect_found(symbols.lookup_value(root, "value")), binding);
        assert_eq!(symbols.symbols.len(), 2);
    }

    #[test]
    fn rejects_duplicate_parameters_in_the_same_scope() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let original =
            expect_declared(symbols.declare(root, "value", SymbolKind::Parameter, Span::new(0, 5)));

        let duplicate = symbols.declare(root, "value", SymbolKind::Parameter, Span::new(10, 15));

        assert!(matches!(
            duplicate,
            Err(DeclareError::DuplicateDeclaration { .. })
        ));
        assert_eq!(expect_found(symbols.lookup_value(root, "value")), original);
        assert_eq!(symbols.symbols.len(), 1);
    }

    #[test]
    fn permits_the_same_name_in_value_and_type_namespaces() {
        let mut symbols = SymbolTable::new();
        let root = symbols.root_scope();
        let value =
            expect_declared(symbols.declare(root, "item", SymbolKind::Binding, Span::new(0, 4)));
        let ty =
            expect_declared(symbols.declare(root, "item", SymbolKind::Struct, Span::new(10, 14)));

        assert_eq!(expect_found(symbols.lookup_value(root, "item")), value);
        assert_eq!(expect_found(symbols.lookup_type(root, "item")), ty);
    }
}
