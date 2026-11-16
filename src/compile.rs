//! Schema compiler: turns the user's schema dicts into a program the engine
//! can run without re-interpreting them, plus a guard that detects in-place
//! mutation of those dicts.
//!
//! The compiler is total. Whatever it does not understand is marked
//! *unsupported* at the smallest enclosing unit (rules set or level); such
//! units are processed by the Python reference implementation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyFrozenSet, PyList, PySet, PyString, PyTuple};
use smallvec::SmallVec;

use crate::refs::Refs;
use crate::types::{Exact, TypeCheck, exact_of, is_iterable};

/// Bumped whenever a validator class is modified.
pub static CLASS_EPOCH: AtomicU64 = AtomicU64::new(1);
/// Bumped by `clear_caches()`.
pub static CACHE_GENERATION: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// class analysis
// ---------------------------------------------------------------------------

/// What the engine may do for instances of one validator class.
#[pyclass(frozen, module = "ferberus._core")]
pub struct ClassInfo {
    pub owner: usize,
    pub epoch: u64,
    /// Engine internals are overridden: always use the reference engine.
    pub exotic: bool,
    pub native_rules: Option<Py<PyFrozenSet>>,
    pub python_rules: Option<Py<PyFrozenSet>>,
    pub normalization_rules: Option<Py<PyFrozenSet>>,
    pub schema_is_plain: bool,
    pub sensitive: Option<Py<PyFrozenSet>>,
    /// `__new__` and `__init__` are the stock ones: child validators can be
    /// assembled natively.
    pub plain_construction: bool,
}

impl ClassInfo {
    /// Cached analysis of `cls`, refreshed if the class epoch moved on.
    pub fn of<'py>(refs: &Refs, cls: &Bound<'py, PyAny>) -> PyResult<Bound<'py, ClassInfo>> {
        let py = cls.py();
        let name = pyo3::intern!(py, "_ferberus_class_info");
        let epoch = CLASS_EPOCH.load(Ordering::Acquire);
        if let Some(found) = cls.getattr_opt(name)?
            && let Ok(info) = found.cast_into_exact::<ClassInfo>()
        {
            let this = info.get();
            if this.owner == cls.as_ptr() as usize && this.epoch == epoch {
                return Ok(info);
            }
        }
        let analysis = refs.analyze_class.bind(py).call1((cls,))?;
        let info = if analysis.is_none() {
            ClassInfo {
                owner: cls.as_ptr() as usize,
                epoch,
                exotic: true,
                native_rules: None,
                python_rules: None,
                normalization_rules: None,
                schema_is_plain: false,
                sensitive: None,
                plain_construction: false,
            }
        } else {
            let (native, python, normalization, schema_is_plain, sensitive, plain_construction): (
                Bound<'py, PyFrozenSet>,
                Bound<'py, PyFrozenSet>,
                Bound<'py, PyFrozenSet>,
                bool,
                Bound<'py, PyFrozenSet>,
                bool,
            ) = analysis.extract()?;
            ClassInfo {
                owner: cls.as_ptr() as usize,
                epoch,
                exotic: false,
                native_rules: Some(native.unbind()),
                python_rules: Some(python.unbind()),
                normalization_rules: Some(normalization.unbind()),
                schema_is_plain,
                sensitive: Some(sensitive.unbind()),
                plain_construction,
            }
        };
        let info = Bound::new(py, info)?;
        // `type.__setattr__` directly: the metaclass' `__setattr__` would
        // bump the class epoch.
        refs.type_setattr.bind(py).call1((cls, name, &info))?;
        Ok(info)
    }
}

// ---------------------------------------------------------------------------
// compiled forms
// ---------------------------------------------------------------------------

/// Built-in rule names; the discriminant is the bit used in drop masks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RuleId {
    Nullable = 0,
    Readonly,
    Type,
    Empty,
    Allowed,
    Forbidden,
    Contains,
    Min,
    Max,
    MinLength,
    MaxLength,
    Regex,
    Items,
    Schema,
    KeysRules,
    ValuesRules,
    Dependencies,
    Excludes,
    AnyOf,
    AllOf,
    NoneOf,
    OneOf,
    CheckWith,
    Other,
}

impl RuleId {
    pub fn of(name: &str) -> RuleId {
        match name {
            "nullable" => RuleId::Nullable,
            "readonly" => RuleId::Readonly,
            "type" => RuleId::Type,
            "empty" => RuleId::Empty,
            "allowed" => RuleId::Allowed,
            "forbidden" => RuleId::Forbidden,
            "contains" => RuleId::Contains,
            "min" => RuleId::Min,
            "max" => RuleId::Max,
            "minlength" => RuleId::MinLength,
            "maxlength" => RuleId::MaxLength,
            "regex" => RuleId::Regex,
            "items" => RuleId::Items,
            "schema" => RuleId::Schema,
            "keysrules" => RuleId::KeysRules,
            "valuesrules" => RuleId::ValuesRules,
            "dependencies" => RuleId::Dependencies,
            "excludes" => RuleId::Excludes,
            "anyof" => RuleId::AnyOf,
            "allof" => RuleId::AllOf,
            "noneof" => RuleId::NoneOf,
            "oneof" => RuleId::OneOf,
            "check_with" => RuleId::CheckWith,
            _ => RuleId::Other,
        }
    }

    pub const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

const fn bits(ids: &[RuleId]) -> u32 {
    let mut mask = 0;
    let mut i = 0;
    while i < ids.len() {
        mask |= ids[i].bit();
        i += 1;
    }
    mask
}

/// Rules dropped by `_validate_nullable` for a `None` value.
pub const NULLABLE_DROPS: u32 = bits(&[
    RuleId::AllOf,
    RuleId::Allowed,
    RuleId::AnyOf,
    RuleId::Empty,
    RuleId::Forbidden,
    RuleId::Items,
    RuleId::KeysRules,
    RuleId::Min,
    RuleId::Max,
    RuleId::MinLength,
    RuleId::MaxLength,
    RuleId::NoneOf,
    RuleId::OneOf,
    RuleId::Regex,
    RuleId::Schema,
    RuleId::Type,
    RuleId::ValuesRules,
]);

/// Rules dropped by `_validate_empty` for an empty value.
pub const EMPTY_DROPS: u32 = bits(&[
    RuleId::Allowed,
    RuleId::Forbidden,
    RuleId::Items,
    RuleId::MinLength,
    RuleId::MaxLength,
    RuleId::Regex,
    RuleId::CheckWith,
]);

pub const DROP_ALL: u32 = u32::MAX;

/// `coerce` / `rename_handler` processors.
pub enum Processor {
    Callable(Py<PyAny>),
    Chain(Vec<Processor>),
    /// `_normalize_coerce_<name>` of the validator.
    Named(Handler),
}

/// A method of the validator that is looked up by a name from the schema.
pub struct Handler {
    /// The name as given in the schema.
    pub rule: Py<PyString>,
    /// The attribute, e.g. `_normalize_coerce_<rule with underscores>`.
    pub method: Py<PyString>,
    pub domain: &'static str,
}

impl Handler {
    fn new(py: Python<'_>, domain: &'static str, rule: &Bound<'_, PyString>) -> PyResult<Handler> {
        let method = format!("_{}_{}", domain, rule.to_str()?.replace(' ', "_"));
        Ok(Handler { rule: rule.clone().unbind(), method: PyString::intern(py, &method).unbind(), domain })
    }

    /// `__get_rule_handler(domain, rule)`
    pub fn resolve<'py>(&self, validator: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = validator.py();
        match validator.getattr_opt(self.method.bind(py))? {
            Some(handler) if !handler.is_none() => Ok(handler),
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "There's no handler for '{}' in the '{}' domain.",
                self.rule.bind(py).to_str()?,
                self.domain
            ))),
        }
    }
}

impl Processor {
    fn has_named(&self) -> bool {
        match self {
            Processor::Callable(_) => false,
            Processor::Named(_) => true,
            Processor::Chain(chain) => chain.iter().any(Processor::has_named),
        }
    }
}

pub enum Setter {
    Callable(Py<PyAny>),
    Named(Handler),
}

/// One alternative of a `type` rule.
pub enum TypeTest {
    Definition(TypeCheck),
    /// Legacy `_validate_type_<name>` method.
    Method(Handler),
}

/// A rules set used for keys, values or unknown fields.
pub struct Sub {
    pub raw: Py<PyAny>,
    pub rules: Option<Arc<RuleSet>>,
}

/// The constraint of a `schema` rule: a rules set when applied to a sequence,
/// a schema when applied to a mapping.
pub struct SchemaC {
    pub raw: Py<PyAny>,
    pub rules: Option<Arc<RuleSet>>,
    pub level: Option<Arc<Level>>,
}

pub struct ItemsC {
    pub raw: Py<PyAny>,
    /// `None` for members that are not plain dicts.
    pub rules: Vec<Option<Arc<RuleSet>>>,
}

pub struct LogicalDef {
    pub raw: Py<PyDict>,
    /// The definition merged with the inherited `type` / `allow_unknown`.
    pub merged: Arc<RuleSet>,
}

pub struct LogicalC {
    pub defs: Vec<LogicalDef>,
}

pub struct RegexC {
    /// The pattern as Cerberus compiles it (`$` appended unless present).
    pub effective: Py<PyString>,
    pub compiled: PyOnceLock<Py<PyAny>>,
    /// The `regex` crate's equivalent, if the pattern has one; built on
    /// first use.
    pub fast: std::sync::OnceLock<Option<regex::bytes::Regex>>,
}

pub enum Op {
    Nullable,
    Readonly(Py<PyAny>),
    /// The prepared checks; none if the constraint is falsy.
    Type(Vec<TypeTest>),
    Empty(Py<PyAny>),
    Allowed(Py<PyAny>),
    Forbidden(Py<PyAny>),
    Contains(Py<PyAny>),
    Min(Py<PyAny>),
    Max(Py<PyAny>),
    MinLength(Py<PyAny>),
    MaxLength(Py<PyAny>),
    Regex(RegexC),
    Items(ItemsC),
    Schema,
    KeysRules,
    ValuesRules,
    Dependencies(Py<PyAny>),
    Excludes(Py<PyAny>),
    AnyOf(LogicalC),
    AllOf(LogicalC),
    NoneOf(LogicalC),
    OneOf(LogicalC),
    CheckWith(Py<PyAny>),
    /// A rule implemented by a Python method of the validator class.
    Python(Handler, Py<PyAny>),
}

pub struct QEntry {
    /// Drop-mask bit of the rule's literal name.
    pub bit: u32,
    pub name: Py<PyString>,
    pub op: Op,
}

pub struct RuleSet {
    pub src: Py<PyDict>,
    /// Why the native engine cannot process this rules set at all.
    pub unsupported: Option<&'static str>,
    /// Uses methods of the validator (or passes `self._error` on): needs a
    /// validator object, which only the root frame has.
    pub hooks: bool,
    /// Has `excludes` or `*of` rules, which read `self.schema[field]`.
    pub reads_own_definition: bool,
    /// Has validation rules that run Python code with access to the
    /// validator, which may then manipulate the rules queue.
    pub queue_hooks: bool,
    pub nullable: Option<Py<PyAny>>,
    pub required: Option<Py<PyAny>>,
    pub readonly: Option<Py<PyAny>>,
    pub rename: Option<Py<PyAny>>,
    pub rename_handler: Option<Processor>,
    pub coerce: Option<Processor>,
    pub default: Option<Py<PyAny>>,
    pub default_setter: Option<Setter>,
    pub purge_unknown: Option<Py<PyAny>>,
    pub allow_unknown: Option<Sub>,
    pub require_all: Option<Py<PyAny>>,
    pub keysrules: Option<Sub>,
    pub valuesrules: Option<Sub>,
    pub schema: Option<SchemaC>,
    pub items: Option<ItemsC>,
    /// Normalizing a value under these rules can neither change it nor fail.
    pub inert: bool,
    /// A merged `*of` definition without own `allow_unknown`: Cerberus fills
    /// in the validator's current `allow_unknown` at validation time.
    pub inherit_allow_unknown: bool,
    pub queue: Vec<QEntry>,
}

impl RuleSet {
    fn empty(src: Py<PyDict>) -> RuleSet {
        RuleSet {
            src,
            unsupported: None,
            hooks: false,
            reads_own_definition: false,
            queue_hooks: false,
            nullable: None,
            required: None,
            readonly: None,
            rename: None,
            rename_handler: None,
            coerce: None,
            default: None,
            default_setter: None,
            purge_unknown: None,
            allow_unknown: None,
            require_all: None,
            keysrules: None,
            valuesrules: None,
            schema: None,
            items: None,
            inert: true,
            inherit_allow_unknown: false,
            queue: Vec::new(),
        }
    }
}

/// A schema: field → rules set.
pub struct Level {
    pub src: Py<PyDict>,
    pub unsupported: Option<&'static str>,
    /// Some field needs a validator object, see [`RuleSet::hooks`].
    pub hooks: bool,
    /// field → position in `fields`
    pub index: Py<PyDict>,
    pub fields: Vec<(Py<PyAny>, Arc<RuleSet>)>,
    pub any_rename: bool,
    pub any_readonly: bool,
    pub any_default: bool,
    pub any_coerce: bool,
    pub any_required: bool,
    /// No field has rules that normalization of containers looks at.
    pub all_inert: bool,
}

impl Level {
    #[inline]
    pub fn get<'a>(&'a self, field: &Bound<'_, PyAny>) -> PyResult<Option<&'a Arc<RuleSet>>> {
        match self.index.bind(field.py()).get_item(field)? {
            None => Ok(None),
            Some(position) => {
                // SAFETY: `index` only holds small non-negative ints we put there.
                let i = unsafe { ffi::PyLong_AsSsize_t(position.as_ptr()) } as usize;
                Ok(Some(&self.fields[i].1))
            }
        }
    }

    /// A level over already compiled fields; `index` maps field → position.
    pub fn from_fields(py: Python<'_>, index: Bound<'_, PyDict>, fields: Vec<(Py<PyAny>, Arc<RuleSet>)>) -> Level {
        let mut level = Level {
            src: PyDict::new(py).unbind(),
            unsupported: fields.iter().find_map(|(_, rules)| rules.unsupported),
            hooks: false,
            index: index.unbind(),
            fields,
            any_rename: false,
            any_readonly: false,
            any_default: false,
            any_coerce: false,
            any_required: false,
            all_inert: true,
        };
        level.summarize();
        level
    }

    /// A level that maps every one of `keys` to the same rules set.
    pub fn uniform<'py>(
        py: Python<'py>,
        keys: impl Iterator<Item = Bound<'py, PyAny>>,
        rules: &Arc<RuleSet>,
    ) -> PyResult<Level> {
        let index = PyDict::new(py);
        let mut fields = Vec::new();
        for key in keys {
            index.set_item(&key, fields.len())?;
            fields.push((key.unbind(), rules.clone()));
        }
        let mut level = Level {
            src: PyDict::new(py).unbind(),
            unsupported: rules.unsupported,
            hooks: false,
            index: index.unbind(),
            fields,
            any_rename: false,
            any_readonly: false,
            any_default: false,
            any_coerce: false,
            any_required: false,
            all_inert: true,
        };
        level.summarize();
        Ok(level)
    }

    /// A stand-in for a schema the engine does not process.
    fn unsupported(py: Python<'_>, schema: &Bound<'_, PyDict>, reason: &'static str) -> Level {
        Level {
            src: schema.clone().unbind(),
            unsupported: Some(reason),
            hooks: false,
            index: PyDict::new(py).unbind(),
            fields: Vec::new(),
            any_rename: false,
            any_readonly: false,
            any_default: false,
            any_coerce: false,
            any_required: false,
            all_inert: true,
        }
    }

    fn summarize(&mut self) {
        for (_, rules) in &self.fields {
            self.hooks |= rules.hooks;
            self.any_rename |= rules.rename.is_some() || rules.rename_handler.is_some();
            self.any_readonly |= rules.readonly.is_some();
            self.any_default |= rules.default.is_some() || rules.default_setter.is_some();
            self.any_coerce |= rules.coerce.is_some();
            self.any_required |= rules.required.is_some();
            self.all_inert &= rules.inert;
        }
    }
}

// ---------------------------------------------------------------------------
// mutation guard
// ---------------------------------------------------------------------------

/// What a guarded dict is expected to hold under a key.
enum Held {
    /// This very object.
    Same(Py<PyAny>),
    /// Any exact list with these very items. Cerberus itself replaces the
    /// lists of `*of` and `items` rules with equal ones whenever it expands
    /// a schema (i.e. on every normalization done by the reference engine).
    List(Vec<Py<PyAny>>),
}

enum GuardNode {
    Dict(Py<PyDict>, Vec<(Py<PyAny>, Held)>),
    Set(Py<PySet>, Vec<Py<PyAny>>),
    /// A registry entry that was resolved (or found missing) at compile time.
    Reference(Py<PyDict>, Py<PyAny>, Option<Py<PyAny>>),
}

/// Identity snapshot of every container `mapping_hash` would traverse, plus
/// the registry entries a program depends on.
#[derive(Default)]
pub struct Guard {
    nodes: Vec<GuardNode>,
    /// The schema contains containers whose mutation cannot be detected.
    pub opaque: bool,
}

impl Guard {
    fn add_mapping(&mut self, refs: &Refs, mapping: &Bound<'_, PyDict>, depth: usize) -> PyResult<()> {
        if depth > 64 {
            self.opaque = true;
            return Ok(());
        }
        let mut held = Vec::new();
        for (key, value) in dict_snapshot(mapping) {
            let expectation = match exact_of(&value) {
                Exact::Dict => {
                    self.add_mapping(refs, value.cast_exact()?, depth + 1)?;
                    Held::Same(value.unbind())
                }
                Exact::List => {
                    let items: Vec<_> = value.cast_exact::<PyList>()?.iter().collect();
                    self.add_members(refs, &items, depth)?;
                    Held::List(items.into_iter().map(Bound::unbind).collect())
                }
                Exact::Tuple => {
                    let items: Vec<_> = value.cast_exact::<PyTuple>()?.iter().collect();
                    self.add_members(refs, &items, depth)?;
                    Held::Same(value.unbind())
                }
                Exact::Set => {
                    let set = value.cast_exact::<PySet>()?;
                    let items: Vec<_> = set.iter().map(Bound::unbind).collect();
                    self.nodes.push(GuardNode::Set(set.clone().unbind(), items));
                    Held::Same(value.unbind())
                }
                Exact::None
                | Exact::Bool
                | Exact::Int
                | Exact::Float
                | Exact::Str
                | Exact::Bytes
                | Exact::FrozenSet => Held::Same(value.unbind()),
                Exact::ByteArray => {
                    self.opaque = true;
                    Held::Same(value.unbind())
                }
                Exact::Other => {
                    let py = value.py();
                    // Other mappings, sequences and sets are hashed by content
                    // by `mapping_hash`; their mutation cannot be tracked
                    // cheaply. Ranges are immutable.
                    if (value.is_instance(refs.mapping.bind(py))?
                        || value.is_instance(refs.sequence.bind(py))?
                        || value.is_instance(refs.abc_set.bind(py))?)
                        && !value.is_exact_instance_of::<pyo3::types::PyRange>()
                    {
                        self.opaque = true;
                    }
                    Held::Same(value.unbind())
                }
            };
            held.push((key.unbind(), expectation));
        }
        self.nodes.push(GuardNode::Dict(mapping.clone().unbind(), held));
        Ok(())
    }

    /// The members of a list or tuple: only mappings are traversed.
    fn add_members(&mut self, refs: &Refs, items: &[Bound<'_, PyAny>], depth: usize) -> PyResult<()> {
        for item in items {
            match exact_of(item) {
                Exact::Dict => self.add_mapping(refs, item.cast_exact()?, depth + 1)?,
                // mutable members that are compared by content
                Exact::List | Exact::Set | Exact::ByteArray => self.opaque = true,
                Exact::Other if item.is_instance(refs.mapping.bind(item.py()))? => self.opaque = true,
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether every guarded container still holds the very same objects.
    pub fn holds(&self, py: Python<'_>) -> bool {
        self.nodes.iter().all(|node| match node {
            GuardNode::Dict(dict, items) => dict_matches(dict.bind(py), items),
            GuardNode::Set(set, items) => {
                let set = set.bind(py);
                set.len() == items.len() && set.iter().zip(items).all(|(a, b)| a.as_ptr() == b.as_ptr())
            }
            GuardNode::Reference(storage, name, resolved) => match storage.bind(py).get_item(name.bind(py)) {
                Ok(current) => current.map(|c| c.as_ptr()) == resolved.as_ref().map(|r| r.as_ptr()),
                Err(_) => false,
            },
        })
    }
}

/// The items of a dict; small dicts do not need an allocation.
pub type Items<'py> = SmallVec<[(Bound<'py, PyAny>, Bound<'py, PyAny>); 12]>;

/// The items of a dict as owned references, in order.
pub fn dict_snapshot<'py>(dict: &Bound<'py, PyDict>) -> Items<'py> {
    let py = dict.py();
    pyo3::sync::critical_section::with_critical_section(dict.as_any(), || {
        // SAFETY: `PyDict_Next` yields borrowed references that are turned
        // into owned ones before anything else can run.
        unsafe {
            let mut items = Items::with_capacity(ffi::PyDict_Size(dict.as_ptr()) as usize);
            let mut pos: ffi::Py_ssize_t = 0;
            let mut key = std::ptr::null_mut();
            let mut value = std::ptr::null_mut();
            while ffi::PyDict_Next(dict.as_ptr(), &mut pos, &mut key, &mut value) != 0 {
                items.push((Bound::from_borrowed_ptr(py, key), Bound::from_borrowed_ptr(py, value)));
            }
            items
        }
    })
}

fn list_matches(list: &Bound<'_, PyAny>, items: &[Py<PyAny>]) -> bool {
    if exact_of(list) != Exact::List {
        return false;
    }
    pyo3::sync::critical_section::with_critical_section(list, || {
        // SAFETY: an exact list, locked (or under the GIL) while its items
        // are read; the pointers are only compared.
        unsafe {
            ffi::PyList_GET_SIZE(list.as_ptr()) as usize == items.len()
                && items
                    .iter()
                    .enumerate()
                    .all(|(i, item)| ffi::PyList_GET_ITEM(list.as_ptr(), i as isize) == item.as_ptr())
        }
    })
}

fn dict_matches(dict: &Bound<'_, PyDict>, items: &[(Py<PyAny>, Held)]) -> bool {
    let py = dict.py();
    pyo3::sync::critical_section::with_critical_section(dict.as_any(), || {
        // SAFETY: borrowed references from `PyDict_Next` are compared, or
        // turned into owned ones before they are looked into.
        unsafe {
            if ffi::PyDict_Size(dict.as_ptr()) as usize != items.len() {
                return false;
            }
            let mut pos: ffi::Py_ssize_t = 0;
            let mut key = std::ptr::null_mut();
            let mut value = std::ptr::null_mut();
            let mut expected = items.iter();
            while ffi::PyDict_Next(dict.as_ptr(), &mut pos, &mut key, &mut value) != 0 {
                match expected.next() {
                    Some((k, Held::Same(v))) if k.as_ptr() == key && v.as_ptr() == value => {}
                    Some((k, Held::List(members)))
                        if k.as_ptr() == key && list_matches(&Bound::from_borrowed_ptr(py, value), members) => {}
                    _ => return false,
                }
            }
            true
        }
    })
}

// ---------------------------------------------------------------------------
// program
// ---------------------------------------------------------------------------

/// The registries a validator resolves references with.
#[derive(Clone)]
pub struct Registries<'py> {
    pub rules_sets: Bound<'py, PyAny>,
    pub schemas: Bound<'py, PyAny>,
}

impl<'py> Registries<'py> {
    /// `validator.rules_set_registry` and `validator.schema_registry`.
    pub fn of(refs: &Refs, config: &Bound<'py, PyDict>) -> PyResult<Registries<'py>> {
        let py = config.py();
        Ok(Registries {
            rules_sets: match config.get_item(pyo3::intern!(py, "rules_set_registry"))? {
                Some(registry) => registry,
                None => refs.rules_set_registry.bind(py).clone(),
            },
            schemas: match config.get_item(pyo3::intern!(py, "schema_registry"))? {
                Some(registry) => registry,
                None => refs.schema_registry.bind(py).clone(),
            },
        })
    }

    /// The dict behind a registry, if it is one of ours with the stock `get`.
    fn storage(refs: &Refs, registry: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let py = registry.py();
        let registry_type = registry.get_type();
        if !(registry_type.is(refs.rules_set_registry_class.bind(py))
            || registry_type.is(refs.schema_registry_class.bind(py)))
        {
            return Ok(None);
        }
        Ok(registry.getattr(pyo3::intern!(py, "_storage"))?.cast_into_exact::<PyDict>().ok())
    }
}

/// A compiled schema, cached on its `DefinitionSchema`.
#[pyclass(frozen, module = "ferberus._core")]
pub struct Program {
    pub class: usize,
    pub epoch: u64,
    pub level: Arc<Level>,
    pub guard: Guard,
    types_mapping: Py<PyAny>,
    rules_sets: Py<PyAny>,
    schemas: Py<PyAny>,
    /// `CACHE_GENERATION` at which the schema-copy dance was last performed.
    pub danced: AtomicU64,
}

impl Program {
    /// Whether this program still describes `schema` for class `info`.
    pub fn is_current(
        &self,
        py: Python<'_>,
        info: &ClassInfo,
        schema: &Bound<'_, PyDict>,
        types_mapping: &Bound<'_, PyAny>,
        registries: &Registries<'_>,
    ) -> bool {
        self.class == info.owner
            && self.epoch == info.epoch
            && self.level.src.as_ptr() == schema.as_ptr()
            && self.types_mapping.as_ptr() == types_mapping.as_ptr()
            && self.rules_sets.as_ptr() == registries.rules_sets.as_ptr()
            && self.schemas.as_ptr() == registries.schemas.as_ptr()
            && self.guard.holds(py)
    }
}

pub struct Compiler<'a, 'py> {
    pub py: Python<'py>,
    pub refs: &'a Refs,
    pub info: &'a ClassInfo,
    pub types_mapping: Bound<'py, PyAny>,
    registries: Registries<'py>,
    rules_storage: Option<Bound<'py, PyDict>>,
    schema_storage: Option<Bound<'py, PyDict>>,
    guard: Guard,
    rules_memo: HashMap<usize, Arc<RuleSet>>,
    level_memo: HashMap<usize, Arc<Level>>,
    /// Dicts whose compilation is underway (recursive references).
    in_progress: HashSet<usize>,
}

const MAX_DEPTH: usize = 48;

impl<'a, 'py> Compiler<'a, 'py> {
    pub fn new(
        refs: &'a Refs,
        info: &'a ClassInfo,
        types_mapping: Bound<'py, PyAny>,
        registries: &Registries<'py>,
    ) -> PyResult<Self> {
        Ok(Compiler {
            py: types_mapping.py(),
            refs,
            info,
            types_mapping,
            rules_storage: Registries::storage(refs, &registries.rules_sets)?,
            schema_storage: Registries::storage(refs, &registries.schemas)?,
            registries: registries.clone(),
            guard: Guard::default(),
            rules_memo: HashMap::new(),
            level_memo: HashMap::new(),
            in_progress: HashSet::new(),
        })
    }

    pub fn program(mut self, schema: &Bound<'py, PyDict>) -> PyResult<Program> {
        self.guard.add_mapping(self.refs, schema, 0)?;
        match self.types_mapping.clone().cast_into_exact::<PyDict>() {
            // types_mapping is part of the key of `_valid_schemas`: all of it
            // is guarded, not only the types the schema uses
            Ok(mapping) => {
                let held =
                    dict_snapshot(&mapping).into_iter().map(|(k, v)| (k.unbind(), Held::Same(v.unbind()))).collect();
                self.guard.nodes.push(GuardNode::Dict(mapping.unbind(), held));
            }
            Err(_) => self.guard.opaque = true,
        }
        let mut level = self.level(schema, 0)?;
        if self.guard.opaque {
            level = Arc::new(Level::unsupported(self.py, schema, "schema or types_mapping cannot be guarded"));
        }
        Ok(Program {
            class: self.info.owner,
            epoch: self.info.epoch,
            level,
            guard: self.guard,
            types_mapping: self.types_mapping.unbind(),
            rules_sets: self.registries.rules_sets.unbind(),
            schemas: self.registries.schemas.unbind(),
            danced: AtomicU64::new(0),
        })
    }

    /// Looks `name` up in a registry and makes the program depend on the result.
    fn resolve(
        &mut self,
        storage: Option<Bound<'py, PyDict>>,
        name: &Bound<'py, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Some(storage) = storage else {
            // an exotic registry: nothing can be said ahead of time
            return Ok(None);
        };
        let resolved = storage.get_item(name)?;
        self.guard.nodes.push(GuardNode::Reference(
            storage.unbind(),
            name.clone().unbind(),
            resolved.as_ref().map(|r| r.clone().unbind()),
        ));
        let Some(Ok(definition)) = resolved.map(|r| r.cast_into_exact::<PyDict>()) else {
            return Ok(None);
        };
        if !self.rules_memo.contains_key(&(definition.as_ptr() as usize))
            && !self.level_memo.contains_key(&(definition.as_ptr() as usize))
            && !self.in_progress.contains(&(definition.as_ptr() as usize))
        {
            self.guard.add_mapping(self.refs, &definition, 0)?;
        }
        Ok(Some(definition))
    }

    /// The rules set a registry reference stands for, if a native frame can
    /// use it in place of the reference.
    fn rules_reference(&mut self, name: &Bound<'py, PyAny>, depth: usize) -> PyResult<Option<Arc<RuleSet>>> {
        let storage = self.rules_storage.clone();
        let Some(definition) = self.resolve(storage, name)? else {
            return Ok(None);
        };
        let rules = self.rules(&definition, depth + 1)?;
        // `_validate_excludes` and the `*of` rules look at `self.schema[field]`
        // without resolving it; with a reference there that ends in an
        // exception which is left to the reference implementation.
        Ok(if rules.reads_own_definition { None } else { Some(rules) })
    }

    pub fn level(&mut self, schema: &Bound<'py, PyDict>, depth: usize) -> PyResult<Arc<Level>> {
        let key = schema.as_ptr() as usize;
        if let Some(found) = self.level_memo.get(&key) {
            return Ok(found.clone());
        }
        let py = self.py;
        if depth > MAX_DEPTH || !self.in_progress.insert(key) {
            return Ok(Arc::new(Level::unsupported(py, schema, "schema nested too deeply or recursive")));
        }
        let index = PyDict::new(py);
        let mut fields = Vec::new();
        let mut unsupported = None;
        for (field, definition) in dict_snapshot(schema) {
            let rules = match exact_of(&definition) {
                Exact::Dict => Some(self.rules(definition.cast_exact()?, depth + 1)?),
                Exact::Str => self.rules_reference(&definition, depth)?,
                _ => None,
            };
            let Some(rules) = rules else {
                unsupported = Some("field definition is not a rules set");
                break;
            };
            if unsupported.is_none() {
                unsupported = rules.unsupported;
            }
            index.set_item(&field, fields.len())?;
            fields.push((field.unbind(), rules));
        }
        self.in_progress.remove(&key);
        let mut level = Level {
            src: schema.clone().unbind(),
            unsupported,
            hooks: false,
            index: index.unbind(),
            fields,
            any_rename: false,
            any_readonly: false,
            any_default: false,
            any_coerce: false,
            any_required: false,
            all_inert: true,
        };
        level.summarize();
        let level = Arc::new(level);
        self.level_memo.insert(key, level.clone());
        Ok(level)
    }

    pub fn rules(&mut self, src: &Bound<'py, PyDict>, depth: usize) -> PyResult<Arc<RuleSet>> {
        let key = src.as_ptr() as usize;
        if let Some(found) = self.rules_memo.get(&key) {
            return Ok(found.clone());
        }
        if !self.in_progress.insert(key) {
            let mut recursive = RuleSet::empty(src.clone().unbind());
            recursive.unsupported = Some("recursive reference");
            return Ok(Arc::new(recursive));
        }
        let rules = self.rules_uncached(src, depth, false);
        self.in_progress.remove(&key);
        let rules = Arc::new(rules?);
        self.rules_memo.insert(key, rules.clone());
        Ok(rules)
    }

    fn sub(&mut self, raw: &Bound<'py, PyAny>, depth: usize) -> PyResult<Sub> {
        Ok(Sub {
            raw: raw.clone().unbind(),
            rules: match exact_of(raw) {
                Exact::Dict => Some(self.rules(raw.cast_exact()?, depth + 1)?),
                Exact::Str => self.rules_reference(raw, depth)?,
                _ => None,
            },
        })
    }

    fn processor(&self, raw: &Bound<'py, PyAny>) -> PyResult<Option<Processor>> {
        if let Ok(name) = raw.cast::<PyString>() {
            return Ok(Some(Processor::Named(Handler::new(self.py, "normalize_coerce", name)?)));
        }
        match exact_of(raw) {
            Exact::List | Exact::Tuple => {
                let mut chain = Vec::new();
                for item in raw.try_iter()? {
                    match self.processor(&item?)? {
                        Some(p) => chain.push(p),
                        None => return Ok(None),
                    }
                }
                Ok(Some(Processor::Chain(chain)))
            }
            _ if is_iterable(self.refs, raw)? => Ok(None),
            _ => Ok(Some(Processor::Callable(raw.clone().unbind()))),
        }
    }

    fn logical(
        &mut self,
        src: &Bound<'py, PyDict>,
        raw: &Bound<'py, PyAny>,
        depth: usize,
    ) -> PyResult<Option<LogicalC>> {
        let py = self.py;
        let definitions: Vec<Bound<'py, PyAny>> = match exact_of(raw) {
            Exact::List | Exact::Tuple => raw.try_iter()?.collect::<PyResult<_>>()?,
            _ => return Ok(None),
        };
        let mut defs = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let Ok(definition) = definition.cast_into_exact::<PyDict>() else {
                return Ok(None);
            };
            // schema = {field: definition.copy()}, completed with the
            // field's own 'allow_unknown' and 'type'
            let merged = definition.copy()?;
            for rule in [pyo3::intern!(py, "allow_unknown"), pyo3::intern!(py, "type")] {
                if !merged.contains(rule)?
                    && let Some(inherited) = src.get_item(rule)?
                {
                    merged.set_item(rule, inherited)?;
                }
            }
            let inherit = !merged.contains(pyo3::intern!(py, "allow_unknown"))?;
            let merged = Arc::new(self.rules_uncached(&merged, depth + 1, inherit)?);
            defs.push(LogicalDef { raw: definition.unbind(), merged });
        }
        Ok(Some(LogicalC { defs }))
    }

    fn rules_uncached(
        &mut self,
        src: &Bound<'py, PyDict>,
        depth: usize,
        inherit_allow_unknown: bool,
    ) -> PyResult<RuleSet> {
        let py = self.py;
        let mut rs = RuleSet::empty(src.clone().unbind());
        rs.inherit_allow_unknown = inherit_allow_unknown;
        if depth > MAX_DEPTH {
            rs.unsupported = Some("schema nested too deeply");
            return Ok(rs);
        }
        let native_rules = self.info.native_rules.as_ref().expect("non-exotic class").bind(py);
        let python_rules = self.info.python_rules.as_ref().expect("non-exotic class").bind(py);
        let normalization_rules = self.info.normalization_rules.as_ref().expect("non-exotic class").bind(py);
        let items = dict_snapshot(src);

        macro_rules! unsupported {
            ($reason:literal) => {{
                rs.unsupported = Some($reason);
                return Ok(rs);
            }};
        }

        // -- first pass: everything that is looked up by name ------------------
        let mut names = Vec::with_capacity(items.len());
        for (name, value) in &items {
            let Ok(name_str) = name.cast_exact::<PyString>() else {
                unsupported!("rule name is not a str");
            };
            let name_str = name_str.to_str()?;
            if name_str.contains(' ') {
                unsupported!("rule name with a space");
            }
            names.push(name_str);
            let value_owned = || Some(value.clone().unbind());
            match name_str {
                "nullable" => rs.nullable = value_owned(),
                "required" => rs.required = value_owned(),
                "readonly" => {
                    rs.readonly = value_owned();
                    rs.inert = false;
                }
                "rename" => {
                    rs.rename = value_owned();
                    rs.inert = false;
                }
                "default" => {
                    rs.default = value_owned();
                    rs.inert = false;
                }
                "default_setter" => {
                    rs.default_setter = Some(match value.cast::<PyString>() {
                        Ok(name) => {
                            rs.hooks = true;
                            Setter::Named(Handler::new(py, "normalize_default_setter", name)?)
                        }
                        Err(_) => Setter::Callable(value.clone().unbind()),
                    });
                    rs.inert = false;
                }
                "rename_handler" => {
                    match self.processor(value)? {
                        Some(p) => {
                            rs.hooks |= p.has_named();
                            rs.rename_handler = Some(p)
                        }
                        None => unsupported!("rename_handler is an exotic iterable"),
                    }
                    rs.inert = false;
                }
                "coerce" => {
                    match self.processor(value)? {
                        Some(p) => {
                            rs.hooks |= p.has_named();
                            rs.coerce = Some(p)
                        }
                        None => unsupported!("coerce is an exotic iterable"),
                    }
                    rs.inert = false;
                }
                "purge_unknown" => {
                    rs.purge_unknown = value_owned();
                    rs.inert = false;
                }
                "require_all" => rs.require_all = value_owned(),
                "allow_unknown" => {
                    rs.allow_unknown = Some(self.sub(value, depth)?);
                    rs.inert = false;
                }
                "keysrules" => {
                    rs.keysrules = Some(self.sub(value, depth)?);
                    rs.inert = false;
                }
                "valuesrules" => {
                    rs.valuesrules = Some(self.sub(value, depth)?);
                    rs.inert = false;
                }
                "schema" => {
                    let (rules, level) = match exact_of(value) {
                        Exact::Dict => {
                            let dict = value.cast_exact::<PyDict>()?;
                            (Some(self.rules(dict, depth + 1)?), Some(self.level(dict, depth + 1)?))
                        }
                        // a reference: to a rules set for the members of a
                        // sequence, to a schema for a mapping
                        Exact::Str => {
                            let rules = self.rules_reference(value, depth)?;
                            let storage = self.schema_storage.clone();
                            let level = match self.resolve(storage, value)? {
                                Some(definition) => Some(self.level(&definition, depth + 1)?),
                                None => None,
                            };
                            (rules, level)
                        }
                        _ => (None, None),
                    };
                    rs.schema = Some(SchemaC { raw: value.clone().unbind(), rules, level });
                    rs.inert = false;
                }
                "items" => {
                    rs.inert = false;
                    match self.items(value, depth)? {
                        Some(items) => rs.items = Some(items),
                        None => unsupported!("items is not a list of rules sets"),
                    }
                }
                _ => {}
            }
        }

        // -- second pass: the validation queue ---------------------------------
        // priority rules first; 'nullable' is mandatory
        let mut order: Vec<usize> = Vec::with_capacity(items.len() + 1);
        let mut nullable_position = None;
        for priority in ["nullable", "readonly", "type", "empty"] {
            match names.iter().position(|n| *n == priority) {
                Some(i) => order.push(i),
                None if priority == "nullable" => nullable_position = Some(order.len()),
                None => {}
            }
        }
        for (i, name) in names.iter().enumerate() {
            if matches!(*name, "nullable" | "readonly" | "type" | "empty") {
                continue;
            }
            if matches!(*name, "allow_unknown" | "require_all" | "meta" | "required") {
                continue;
            }
            if normalization_rules.contains(items[i].0.as_any())? {
                continue;
            }
            order.push(i);
        }
        if nullable_position.is_some() {
            rs.queue.push(QEntry {
                bit: RuleId::Nullable.bit(),
                name: PyString::intern(py, "nullable").unbind(),
                op: Op::Nullable,
            });
        }
        for i in order {
            let (name, value) = &items[i];
            let name_str = names[i];
            let id = RuleId::of(name_str);
            let constraint = value.clone().unbind();
            if python_rules.contains(name.as_any())? {
                rs.hooks = true;
                rs.queue_hooks = true;
                let handler = Handler::new(py, "validate", name.cast::<PyString>()?)?;
                rs.queue.push(QEntry {
                    bit: id.bit(),
                    name: name.clone().cast_into::<PyString>()?.unbind(),
                    op: Op::Python(handler, constraint),
                });
                continue;
            }
            if !native_rules.contains(name.as_any())? {
                unsupported!("rule without handler");
            }
            let op = match id {
                RuleId::Nullable => Op::Nullable,
                RuleId::Readonly => Op::Readonly(constraint),
                RuleId::Type => match self.type_checks(value)? {
                    Some(checks) => {
                        rs.hooks |= checks.iter().any(|check| matches!(check, TypeTest::Method(_)));
                        Op::Type(checks)
                    }
                    None => unsupported!("type constraint cannot be resolved ahead of time"),
                },
                RuleId::Empty => Op::Empty(constraint),
                RuleId::Allowed => Op::Allowed(constraint),
                RuleId::Forbidden => Op::Forbidden(constraint),
                RuleId::Contains => Op::Contains(constraint),
                RuleId::Min => Op::Min(constraint),
                RuleId::Max => Op::Max(constraint),
                RuleId::MinLength => Op::MinLength(constraint),
                RuleId::MaxLength => Op::MaxLength(constraint),
                RuleId::Regex => {
                    let Ok(pattern) = value.cast_exact::<PyString>() else {
                        unsupported!("regex is not a str");
                    };
                    let text = pattern.to_str()?;
                    let effective =
                        if text.ends_with('$') { pattern.clone() } else { PyString::new(py, &format!("{text}$")) };
                    Op::Regex(RegexC {
                        effective: effective.unbind(),
                        compiled: PyOnceLock::new(),
                        fast: std::sync::OnceLock::new(),
                    })
                }
                RuleId::Items => match self.items(value, depth)? {
                    Some(items) => Op::Items(items),
                    None => unsupported!("items is not a list of rules sets"),
                },
                RuleId::Schema => Op::Schema,
                RuleId::KeysRules => Op::KeysRules,
                RuleId::ValuesRules => Op::ValuesRules,
                RuleId::Dependencies => Op::Dependencies(constraint),
                RuleId::Excludes => {
                    rs.reads_own_definition = true;
                    Op::Excludes(constraint)
                }
                RuleId::AnyOf | RuleId::AllOf | RuleId::NoneOf | RuleId::OneOf => {
                    let Some(logical) = self.logical(src, value, depth)? else {
                        unsupported!("*of constraint is not a list of rules sets");
                    };
                    rs.reads_own_definition = true;
                    match id {
                        RuleId::AnyOf => Op::AnyOf(logical),
                        RuleId::AllOf => Op::AllOf(logical),
                        RuleId::NoneOf => Op::NoneOf(logical),
                        _ => Op::OneOf(logical),
                    }
                }
                RuleId::CheckWith => {
                    rs.hooks = true;
                    rs.queue_hooks = true;
                    Op::CheckWith(constraint)
                }
                RuleId::Other => unsupported!("rule without native implementation"),
            };
            rs.queue.push(QEntry { bit: id.bit(), name: name.clone().cast_into::<PyString>()?.unbind(), op });
        }
        Ok(rs)
    }

    fn items(&mut self, raw: &Bound<'py, PyAny>, depth: usize) -> PyResult<Option<ItemsC>> {
        match exact_of(raw) {
            Exact::List | Exact::Tuple => {}
            _ => return Ok(None),
        }
        let mut rules = Vec::new();
        for item in raw.try_iter()? {
            let item = item?;
            rules.push(match exact_of(&item) {
                Exact::Dict => Some(self.rules(item.cast_exact()?, depth + 1)?),
                Exact::Str => self.rules_reference(&item, depth)?,
                _ => None,
            });
        }
        Ok(Some(ItemsC { raw: raw.clone().unbind(), rules }))
    }

    /// Resolves the constraint of a `type` rule against `types_mapping`.
    fn type_checks(&self, constraint: &Bound<'py, PyAny>) -> PyResult<Option<Vec<TypeTest>>> {
        let names: Vec<Bound<'py, PyAny>> = match exact_of(constraint) {
            Exact::Str => {
                if constraint.cast_exact::<PyString>()?.to_str()?.is_empty() {
                    return Ok(Some(Vec::new()));
                }
                vec![constraint.clone()]
            }
            Exact::List | Exact::Tuple => constraint.try_iter()?.collect::<PyResult<_>>()?,
            _ => return Ok(None),
        };
        let Ok(mapping) = self.types_mapping.cast_exact::<PyDict>() else {
            return Ok(None);
        };
        let mut checks = Vec::with_capacity(names.len());
        for name in names {
            if exact_of(&name) != Exact::Str {
                return Ok(None);
            }
            match mapping.get_item(&name)? {
                // not in types_mapping: a `_validate_type_<name>` method
                Some(definition) if !definition.is_none() => match TypeCheck::new(self.refs, &definition) {
                    Ok(check) => checks.push(TypeTest::Definition(check)),
                    Err(_) => return Ok(None),
                },
                _ => checks.push(TypeTest::Method(Handler::new(self.py, "validate_type", name.cast::<PyString>()?)?)),
            }
        }
        Ok(Some(checks))
    }
}
