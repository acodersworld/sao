use std::{collections::HashMap, fmt, sync::Arc};

/// Identifies one source module within a compilation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(u32);

impl ModuleId {
    pub const PRELUDE: Self = Self(0);
    #[cfg(test)]
    pub(crate) const TEST_SOURCE: Self = Self(1);

    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub module_id: ModuleId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn new(module_id: ModuleId, start: usize, end: usize) -> Self {
        Self {
            module_id,
            start,
            end,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A lightweight handle pairing a registered module ID with shared source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceModule {
    module_id: ModuleId,
    source: Arc<str>,
}

impl SourceModule {
    #[must_use]
    pub const fn module_id(&self) -> ModuleId {
        self.module_id
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn text(&self, span: Span) -> Result<&str, SourceLookupError> {
        if span.module_id != self.module_id {
            return Err(SourceLookupError::ModuleMismatch {
                expected: self.module_id,
                found: span.module_id,
            });
        }

        self.source
            .get(span.start..span.end)
            .ok_or(SourceLookupError::InvalidSpan { span })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLookupError {
    UnknownModule { module_id: ModuleId },
    ModuleMismatch { expected: ModuleId, found: ModuleId },
    InvalidSpan { span: Span },
}

impl fmt::Display for SourceLookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModule { module_id } => {
                write!(formatter, "unknown source module {module_id:?}")
            }
            Self::ModuleMismatch { expected, found } => write!(
                formatter,
                "source span belongs to module {found:?}, not module {expected:?}"
            ),
            Self::InvalidSpan { span } => write!(
                formatter,
                "invalid source span {}..{} in module {:?}",
                span.start, span.end, span.module_id
            ),
        }
    }
}

impl std::error::Error for SourceLookupError {}

/// Owns immutable source text and allocates ordinary module identities.
///
/// This registry deliberately has no concept of a root or entry module.
#[derive(Debug)]
pub struct SourceModuleRegistry {
    sources: HashMap<ModuleId, Arc<str>>,
    next_module_id: u32,
}

impl Default for SourceModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceModuleRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
            next_module_id: 1,
        }
    }

    pub fn add(&mut self, source: impl Into<Arc<str>>) -> SourceModule {
        let module_id = self.allocate_module_id();
        let source = source.into();

        let replaced = self.sources.insert(module_id, Arc::clone(&source));
        assert!(
            replaced.is_none(),
            "allocated source module ID must be unique"
        );

        SourceModule { module_id, source }
    }

    pub fn module(&self, module_id: ModuleId) -> Result<SourceModule, SourceLookupError> {
        let source = self
            .sources
            .get(&module_id)
            .ok_or(SourceLookupError::UnknownModule { module_id })?;

        Ok(SourceModule {
            module_id,
            source: Arc::clone(source),
        })
    }

    pub fn text(&self, span: Span) -> Result<&str, SourceLookupError> {
        self.sources
            .get(&span.module_id)
            .ok_or(SourceLookupError::UnknownModule {
                module_id: span.module_id,
            })?
            .get(span.start..span.end)
            .ok_or(SourceLookupError::InvalidSpan { span })
    }

    fn allocate_module_id(&mut self) -> ModuleId {
        let module_id = ModuleId(self.next_module_id);
        self.next_module_id = self
            .next_module_id
            .checked_add(1)
            .expect("module ID space exhausted");
        module_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_prelude_and_allocates_source_modules_from_one() {
        let mut registry = SourceModuleRegistry::new();
        let first = registry.add("first");
        let second = registry.add("second");

        assert_eq!(ModuleId::PRELUDE.as_u32(), 0);
        assert_eq!(first.module_id().as_u32(), 1);
        assert_eq!(second.module_id().as_u32(), 2);
    }

    #[test]
    fn returns_handles_to_shared_registered_source() {
        let mut registry = SourceModuleRegistry::new();
        let original = registry.add("source");
        let retrieved = registry
            .module(original.module_id())
            .expect("module should be registered");

        assert_eq!(retrieved.module_id(), original.module_id());
        assert_eq!(retrieved.source(), original.source());
        assert!(Arc::ptr_eq(&original.source, &retrieved.source));
    }

    #[test]
    fn registration_order_only_determines_identity() {
        let mut registry = SourceModuleRegistry::new();
        let library = registry.add("fn helper() {}");
        let eventual_entry = registry.add("fn main() {}");

        assert_eq!(library.module_id().as_u32(), 1);
        assert_eq!(eventual_entry.module_id().as_u32(), 2);
        assert_eq!(
            registry
                .module(eventual_entry.module_id())
                .expect("entry candidate should be retrievable")
                .source(),
            "fn main() {}"
        );
    }

    #[test]
    fn resolves_valid_spans_and_reports_invalid_lookups() {
        let mut registry = SourceModuleRegistry::new();
        let first = registry.add("hello");
        let second = registry.add("world");
        let span = Span::new(first.module_id(), 1, 4);

        assert_eq!(first.text(span), Ok("ell"));
        assert_eq!(registry.text(span), Ok("ell"));
        assert_eq!(
            second.text(span),
            Err(SourceLookupError::ModuleMismatch {
                expected: second.module_id(),
                found: first.module_id(),
            })
        );

        let invalid = Span::new(first.module_id(), 0, 10);
        assert_eq!(
            registry.text(invalid),
            Err(SourceLookupError::InvalidSpan { span: invalid })
        );
        let unknown = ModuleId(99);
        assert_eq!(
            registry.module(unknown),
            Err(SourceLookupError::UnknownModule { module_id: unknown })
        );
        assert_eq!(
            registry.text(Span::new(unknown, 0, 0)),
            Err(SourceLookupError::UnknownModule { module_id: unknown })
        );
    }

    #[test]
    #[should_panic(expected = "module ID space exhausted")]
    fn module_id_overflow_panics_clearly() {
        let mut registry = SourceModuleRegistry {
            sources: HashMap::new(),
            next_module_id: u32::MAX,
        };

        let _ = registry.add("unallocatable");
    }
}
