//! The native engine: runs the body of `Validator.validate()` and
//! `Validator.normalized()` over frames that mirror Cerberus' child
//! validators.
//!
//! Every function names the Cerberus method it reproduces; the logic follows
//! `cerberus/validator.py` statement by statement, including its quirks.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pyo3::exceptions::{PyException, PyKeyError, PyTypeError};
use pyo3::ffi;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedStr;
use pyo3::types::{PyDict, PyList, PySet, PyString, PyTuple};

use crate::compile::{
    CACHE_GENERATION, ClassInfo, Compiler, DROP_ALL, EMPTY_DROPS, ItemsC, Level, LogicalC, NULLABLE_DROPS, Op,
    Processor, Program, QEntry, Registries, RuleId, RuleSet, Setter, Sub, TypeTest, dict_snapshot,
};
use crate::errors::{drop_nodes_from_errorpaths, log_has_error_at, log_has_node_at, sort_errors};
use crate::refs::{ErrDef, Refs};
use crate::stats::{self, Stat};
use crate::types::{
    Exact, exact_of, is_exact_dict, is_hashable, is_iterable, is_mapping, is_sequence, is_sized, is_str,
};

/// Native frames nested deeper than this are handed to the reference
/// implementation, which ends in Python's own `RecursionError`.
const MAX_NATIVE_DEPTH: usize = 80;

// ---------------------------------------------------------------------------
// frames
// ---------------------------------------------------------------------------

/// The document of a frame. Sequences and key sets are validated as the
/// virtual mappings Cerberus builds for them (`{i: value}`, `{key: key}`).
enum DocKind<'py> {
    Dict(Bound<'py, PyDict>),
    Seq(Vec<Bound<'py, PyAny>>),
    Pairs(Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)>),
}

struct Doc<'py> {
    kind: DocKind<'py>,
    /// The real dict of a virtual document, built on demand.
    materialized: OnceCell<Bound<'py, PyDict>>,
}

impl<'py> Doc<'py> {
    fn new(kind: DocKind<'py>) -> Rc<Self> {
        Rc::new(Doc { kind, materialized: OnceCell::new() })
    }

    /// The document as the mapping Cerberus would have at hand.
    fn as_dict(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyDict>> {
        if let DocKind::Dict(dict) = &self.kind {
            return Ok(dict);
        }
        if let Some(dict) = self.materialized.get() {
            return Ok(dict);
        }
        let dict = PyDict::new(py);
        match &self.kind {
            DocKind::Dict(_) => unreachable!(),
            DocKind::Seq(items) => {
                for (i, item) in items.iter().enumerate() {
                    dict.set_item(i, item)?;
                }
            }
            DocKind::Pairs(pairs) => {
                for (key, value) in pairs {
                    dict.set_item(key, value)?;
                }
            }
        }
        Ok(self.materialized.get_or_init(|| dict))
    }
}

/// The schema of a frame.
#[derive(Clone, Copy)]
enum Lvl<'a> {
    Fields(&'a Level),
    /// Every key of the document has these rules.
    Uniform(&'a Arc<RuleSet>),
    /// The document is a sequence; one rules set per position.
    Indexed(&'a [Option<Arc<RuleSet>>]),
    /// A single field (the key of the frame's `single` slot) has rules.
    Single(&'a Arc<RuleSet>),
}

/// A field name: either a Python object or a position in a sequence that is
/// turned into an `int` only when needed.
#[derive(Clone, Copy)]
enum Key<'b, 'py> {
    Obj(&'b Bound<'py, PyAny>),
    Index(usize),
}

impl<'b, 'py> Key<'b, 'py> {
    fn object(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        match self {
            Key::Obj(obj) => (*obj).clone(),
            Key::Index(i) => i.into_pyobject(py).expect("int creation").into_any(),
        }
    }
}

/// The effective configuration of a (child) validator.
#[derive(Clone)]
struct Cfg<'py> {
    allow_unknown: Bound<'py, PyAny>,
    au_truthy: bool,
    au_is_mapping: bool,
    au_is_str: bool,
    /// Compiled `allow_unknown` if it is a mapping.
    au_rules: Option<Arc<RuleSet>>,
    /// `allow_unknown` refers to methods of the validator.
    au_hooks: bool,
    require_all: Bound<'py, PyAny>,
    /// `None`: not part of the configuration (reads as `False`).
    purge_unknown: Option<Bound<'py, PyAny>>,
    purge_readonly: bool,
    ignore_none_values: bool,
}

impl<'py> Cfg<'py> {
    fn purge_unknown_truthy(&self) -> PyResult<bool> {
        match &self.purge_unknown {
            Some(value) => value.is_truthy(),
            None => Ok(false),
        }
    }

    /// The configuration with another `allow_unknown`; `None` if the engine
    /// cannot work with it.
    fn with_allow_unknown(
        &self,
        refs: &Refs,
        value: &Bound<'py, PyAny>,
        rules: Option<&Arc<RuleSet>>,
    ) -> PyResult<Option<Cfg<'py>>> {
        let mut cfg = self.clone();
        cfg.au_truthy = value.is_truthy()?;
        cfg.au_is_str = is_str(value);
        cfg.au_is_mapping = !cfg.au_is_str && is_mapping(refs, value)?;
        cfg.au_rules = None;
        cfg.au_hooks = false;
        if cfg.au_is_mapping {
            match rules {
                Some(rules) if rules.unsupported.is_none() && rules.src.as_ptr() == value.as_ptr() => {
                    cfg.au_hooks = rules.hooks;
                    cfg.au_rules = Some(rules.clone())
                }
                _ => return Ok(None),
            }
        }
        cfg.allow_unknown = value.clone();
        Ok(Some(cfg))
    }
}

struct Frame<'a, 'py> {
    parent: Option<&'a Frame<'a, 'py>>,
    /// The validator object of the frame: the validator that was called for
    /// the root, a real child validator for frames whose rules need one,
    /// nothing for all other frames.
    obj: Option<Bound<'py, PyAny>>,
    /// `obj._config`
    config: Option<Bound<'py, PyDict>>,
    /// Python code with access to `obj` runs while the frame is processed
    /// (rule methods, checks, an emitting error handler): the validator's
    /// state can change behind the engine's back.
    live: bool,
    doc: Rc<Doc<'py>>,
    lvl: Lvl<'a>,
    /// The field of a `Lvl::Single` schema.
    single: Option<Bound<'py, PyAny>>,
    cfg: Cfg<'py>,
    dcrumb: Option<Bound<'py, PyAny>>,
    scrumb: Crumbs<'py>,
    update: bool,
    is_normalized: Cell<bool>,
    errors: RefCell<Option<Bound<'py, PyList>>>,
    log: RefCell<Option<Bound<'py, PyList>>>,
    /// Error trees, only kept once the order of errors became ambiguous.
    trees: RefCell<Option<(Bound<'py, PyAny>, Bound<'py, PyAny>)>>,
    clean: Cell<bool>,
    unrequired: RefCell<Option<Bound<'py, PySet>>>,
    /// In the child of an `*of` rule: the `allow_unknown` of the validator
    /// that evaluates the rule, with its compiled form.
    logical_au: Option<(Bound<'py, PyAny>, Option<Arc<RuleSet>>)>,
    dpath: OnceCell<Bound<'py, PyTuple>>,
    spath: OnceCell<Bound<'py, PyTuple>>,
    depth: usize,
    /// Root only: `_remaining_rules` still holds the leftovers of a call
    /// that ended with an exception.
    stale_remaining_rules: Cell<bool>,
}

/// The crumbs a child validator extends its schema path with.
type Crumbs<'py> = smallvec::SmallVec<[Bound<'py, PyAny>; 3]>;

/// Outcome of a rule.
enum Flow {
    Continue,
    /// The rule returned a truthy value: stop processing the field.
    Stop,
}

/// How `_error` obtains the value of the field (`self.document.get(field)`).
#[derive(Clone, Copy)]
enum Value<'b, 'py> {
    Given(&'b Bound<'py, PyAny>),
    Lookup,
}

pub struct Engine<'py> {
    py: Python<'py>,
    refs: &'static Refs,
    validator: Bound<'py, PyAny>,
    /// The validator's class can be instantiated natively, see
    /// [`ClassInfo::plain_construction`].
    plain_children: bool,
    /// `validator.__dict__`
    instance: Bound<'py, PyDict>,
    config: Bound<'py, PyDict>,
    root_document: Bound<'py, PyAny>,
    /// The error handler, if its `emit` does something.
    emitter: Option<Bound<'py, PyAny>>,
}

// ---------------------------------------------------------------------------
// entry points
// ---------------------------------------------------------------------------

/// Result of the entry points that replace a whole `validate()` /
/// `normalized()` call.
pub enum Outcome {
    /// Nothing was done; the reference implementation has to run the call.
    NotStarted,
    /// The validator was reset and handed the document, but its schema turned
    /// out to be unsupported: the reference implementation continues with the
    /// body of the call.
    PreludeDone,
    /// All done.
    Done,
}

/// What the entry points learn about a validator instance without touching it.
struct Checked<'py> {
    refs: &'static Refs,
    info: Bound<'py, ClassInfo>,
    instance: Bound<'py, PyDict>,
    config: Bound<'py, PyDict>,
    schema_object: Bound<'py, PyAny>,
    schema: Bound<'py, PyDict>,
    /// The schema is a `DefinitionSchema` that can carry its program.
    cacheable: bool,
    types_mapping: Bound<'py, PyAny>,
    registries: Registries<'py>,
}

fn attribute<'py>(
    instance_dict: &Bound<'py, PyDict>,
    validator: &Bound<'py, PyAny>,
    name: &Bound<'py, PyString>,
) -> PyResult<Bound<'py, PyAny>> {
    match instance_dict.get_item(name)? {
        Some(value) => Ok(value),
        None => validator.getattr(name),
    }
}

/// Number of instance attributes a validator has after processing a document.
const INSTANCE_ATTRIBUTES: usize = 14;

/// Whether the engine can work for this validator at all. No side effects.
fn check<'py>(validator: &Bound<'py, PyAny>) -> PyResult<Option<Checked<'py>>> {
    let py = validator.py();
    let refs = Refs::get(py)?;
    let cls = validator.get_type();
    let info = ClassInfo::of(refs, cls.as_any())?;
    if info.get().exotic {
        stats::count(Stat::DeclinedExoticClass);
        return Ok(None);
    }

    // SAFETY: plain call; returns a new reference or NULL with an exception.
    let instance = unsafe {
        Bound::from_owned_ptr_or_err(py, ffi::PyObject_GenericGetDict(validator.as_ptr(), std::ptr::null_mut()))?
    };
    let Ok(instance) = instance.cast_into_exact::<PyDict>() else {
        return Ok(None);
    };
    if instance.len() != INSTANCE_ATTRIBUTES {
        // unusual set of attributes: make sure none shadows a handler
        let sensitive = info.get().sensitive.as_ref().expect("non-exotic class").bind(py);
        for (name, _) in dict_snapshot(&instance) {
            if sensitive.contains(&name)? {
                stats::count(Stat::DeclinedInstanceOverride);
                return Ok(None);
            }
        }
    }

    let Ok(config) = attribute(&instance, validator, intern!(py, "_config"))?.cast_into_exact::<PyDict>() else {
        return Ok(None);
    };
    if let Some(is_child) = config.get_item(intern!(py, "is_child"))?
        && is_child.is_truthy()?
    {
        stats::count(Stat::DeclinedChild);
        return Ok(None);
    }

    let schema_object = if info.get().schema_is_plain {
        attribute(&instance, validator, intern!(py, "_schema"))?
    } else {
        validator.getattr(intern!(py, "schema"))?
    };
    let (schema, cacheable) = if let Ok(dict) = schema_object.cast_exact::<PyDict>() {
        (dict.clone(), false)
    } else if !schema_object.is_none() && schema_object.is_instance(refs.definition_schema.bind(py))? {
        match schema_object.getattr(intern!(py, "schema"))?.cast_into_exact::<PyDict>() {
            Ok(dict) => (dict, true),
            Err(_) => {
                stats::count(Stat::DeclinedSchemaType);
                return Ok(None);
            }
        }
    } else {
        stats::count(Stat::DeclinedSchemaType);
        return Ok(None);
    };

    let types_mapping = validator.getattr(intern!(py, "types_mapping"))?;
    let registries = Registries::of(refs, &config)?;
    Ok(Some(Checked { refs, info, instance, config, schema_object, schema, cacheable, types_mapping, registries }))
}

impl<'py> Checked<'py> {
    fn is_current(&self, program: &Bound<'py, Program>) -> bool {
        program.get().is_current(self.schema.py(), self.info.get(), &self.schema, &self.types_mapping, &self.registries)
    }

    /// The program of the schema: the cached one if it is current, else a
    /// fresh one. No side effects other than caching.
    fn program(&self) -> PyResult<Bound<'py, Program>> {
        let py = self.schema.py();
        let name = intern!(py, "_ferberus_program");
        if self.cacheable
            && let Some(found) = self.schema_object.getattr_opt(name)?
            && let Ok(found) = found.cast_into_exact::<Program>()
            && self.is_current(&found)
        {
            return Ok(found);
        }
        stats::count(Stat::Compilations);
        let compiled = Compiler::new(self.refs, self.info.get(), self.types_mapping.clone(), &self.registries)?
            .program(&self.schema)?;
        let compiled = Bound::new(py, compiled)?;
        if self.cacheable {
            self.schema_object.setattr(name, &compiled)?;
        }
        Ok(compiled)
    }

    /// Performs the schema handling at the top of `__normalize_mapping` if
    /// it is due for `program` and returns the program that is valid
    /// afterwards.
    fn dance(&self, validator: &Bound<'py, PyAny>, program: Bound<'py, Program>) -> PyResult<Bound<'py, Program>> {
        let py = validator.py();
        let generation = CACHE_GENERATION.load(Ordering::Acquire);
        if program.get().danced.load(Ordering::Acquire) == generation {
            return Ok(program);
        }
        self.refs.schema_copy_dance.bind(py).call1((validator,))?;
        // expanding the schema may have changed it in place
        let program = if self.is_current(&program) { program } else { self.program()? };
        program.get().danced.store(generation, Ordering::Release);
        Ok(program)
    }

    /// The validator's configuration; `None` if its `allow_unknown` is
    /// beyond the engine.
    fn cfg(&self) -> PyResult<Option<Cfg<'py>>> {
        let py = self.config.py();
        let config = &self.config;
        let false_ = pyo3::types::PyBool::new(py, false).to_owned().into_any();
        let allow_unknown = config.get_item(intern!(py, "allow_unknown"))?.unwrap_or_else(|| false_.clone());
        let truthy = |value: Option<Bound<'py, PyAny>>| -> PyResult<bool> {
            match value {
                Some(value) => value.is_truthy(),
                None => Ok(false),
            }
        };
        let base = Cfg {
            allow_unknown: false_.clone(),
            au_truthy: false,
            au_is_mapping: false,
            au_is_str: false,
            au_rules: None,
            au_hooks: false,
            require_all: config.get_item(intern!(py, "require_all"))?.unwrap_or_else(|| false_.clone()),
            purge_unknown: config.get_item(intern!(py, "purge_unknown"))?,
            purge_readonly: truthy(config.get_item(intern!(py, "purge_readonly"))?)?,
            ignore_none_values: truthy(config.get_item(intern!(py, "ignore_none_values"))?)?,
        };
        let au_rules = match allow_unknown.cast_exact::<PyDict>() {
            Ok(dict) => Some(
                Compiler::new(self.refs, self.info.get(), self.types_mapping.clone(), &self.registries)?
                    .rules(dict, 0)?,
            ),
            Err(_) => None,
        };
        let cfg = base.with_allow_unknown(self.refs, &allow_unknown, au_rules.as_ref())?;
        if cfg.is_none() {
            stats::count(Stat::DeclinedUnsupportedSchema);
            stats::reason("allow_unknown of the validator");
        }
        Ok(cfg)
    }

    /// The error handler and whether it is a stock one, whose `start` only
    /// resets its tree and whose `emit` and `end` do nothing.
    fn handler(&self, validator: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, bool)> {
        let py = validator.py();
        let handler = attribute(&self.instance, validator, intern!(py, "error_handler"))?;
        let handler_type = handler.get_type();
        let stock = handler_type.is(self.refs.basic_error_handler.bind(py))
            || handler_type.is(self.refs.schema_error_handler.bind(py));
        Ok((handler, stock))
    }

    fn engine(&self, validator: &Bound<'py, PyAny>, document: &Bound<'py, PyDict>) -> PyResult<Engine<'py>> {
        let py = validator.py();
        let (handler, stock) = self.handler(validator)?;
        let emitter = if stock {
            None
        } else {
            let emit = intern!(py, "emit");
            if handler.get_type().getattr(emit)?.is(&self.refs.base_error_handler.bind(py).getattr(emit)?) {
                None
            } else {
                Some(handler)
            }
        };
        Ok(Engine {
            py,
            refs: self.refs,
            validator: validator.clone(),
            plain_children: self.info.get().plain_construction && self.info.get().schema_is_plain,
            instance: self.instance.clone(),
            config: self.config.clone(),
            root_document: document.clone().into_any(),
            emitter,
        })
    }
}

fn unsupported(program: &Bound<'_, Program>) -> bool {
    match program.get().level.unsupported {
        Some(reason) => {
            stats::count(Stat::DeclinedUnsupportedSchema);
            stats::reason(reason);
            true
        }
        None => false,
    }
}

/// Runs the body of `validate()` for a validator that was already reset and
/// handed its document; `false` means "not handled, use the reference
/// implementation".
pub fn validate_body(validator: &Bound<'_, PyAny>, normalize: bool, update: bool) -> PyResult<bool> {
    let py = validator.py();
    let Some(checked) = check(validator)? else {
        return Ok(false);
    };
    let Ok(document) = attribute(&checked.instance, validator, intern!(py, "document"))?.cast_into_exact::<PyDict>()
    else {
        stats::count(Stat::DeclinedDocumentType);
        return Ok(false);
    };
    let mut program = checked.program()?;
    if normalize {
        program = checked.dance(validator, program)?;
    }
    if unsupported(&program) {
        return Ok(false);
    }
    let Some(cfg) = checked.cfg()? else {
        return Ok(false);
    };
    stats::count(Stat::NativeCalls);
    let engine = checked.engine(validator, &document)?;
    let level: &Level = &program.get().level;
    let frame = engine.root_frame(document, level, cfg, update)?;
    if normalize {
        engine.normalize_mapping(&frame, level)?;
    }
    engine.validate_frame(&frame)?;
    Ok(true)
}

/// Runs the body of `normalized()`, see [`validate_body`].
pub fn normalize_body(validator: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = validator.py();
    let Some(checked) = check(validator)? else {
        return Ok(false);
    };
    let Ok(document) = attribute(&checked.instance, validator, intern!(py, "document"))?.cast_into_exact::<PyDict>()
    else {
        stats::count(Stat::DeclinedDocumentType);
        return Ok(false);
    };
    let program = checked.program()?;
    let program = checked.dance(validator, program)?;
    if unsupported(&program) {
        return Ok(false);
    }
    let Some(cfg) = checked.cfg()? else {
        return Ok(false);
    };
    stats::count(Stat::NativeCalls);
    let engine = checked.engine(validator, &document)?;
    let level: &Level = &program.get().level;
    let frame = engine.root_frame(document, level, cfg, false)?;
    engine.normalize_mapping(&frame, level)?;
    Ok(true)
}

/// A complete `validate()` (`validation: Some((update, normalize))`) or
/// `normalized()` call without a schema argument: everything from resetting
/// the validator to `error_handler.end()`.
pub fn process(
    validator: &Bound<'_, PyAny>,
    document: &Bound<'_, PyAny>,
    validation: Option<(&Bound<'_, PyAny>, bool)>,
) -> PyResult<Outcome> {
    let py = validator.py();
    // Anything but a plain dict goes the reference way: `copy()` and the
    // DocumentError checks are its business.
    let Ok(document) = document.cast_exact::<PyDict>() else {
        stats::count(Stat::DeclinedDocumentType);
        return Ok(Outcome::NotStarted);
    };
    let Some(checked) = check(validator)? else {
        return Ok(Outcome::NotStarted);
    };
    let normalize = validation.is_none_or(|(_, normalize)| normalize);
    let mut program = checked.program()?;
    if unsupported(&program) {
        return Ok(Outcome::NotStarted);
    }
    let Some(cfg) = checked.cfg()? else {
        return Ok(Outcome::NotStarted);
    };

    // -- the head of validate() / normalized(), and __init_processing ---------
    let instance = &checked.instance;
    let update = match validation {
        Some((update, _)) => {
            instance.set_item(intern!(py, "update"), update)?;
            instance.set_item(intern!(py, "_unrequired_by_excludes"), PySet::empty(py)?)?;
            update.is_truthy()?
        }
        None => false,
    };
    instance.set_item(intern!(py, "_errors"), checked.refs.error_list.bind(py).call0()?)?;
    instance.set_item(intern!(py, "recent_error"), py.None())?;
    instance.set_item(intern!(py, "_error_log"), PyList::empty(py))?;
    instance.set_item(intern!(py, "_document_error_tree"), py.None())?;
    instance.set_item(intern!(py, "_schema_error_tree"), py.None())?;
    let document = document.copy()?;
    instance.set_item(intern!(py, "document"), &document)?;
    checked.config.set_item(intern!(py, "_is_normalized"), false)?;
    let (handler, stock) = checked.handler(validator)?;
    if stock {
        // BasicErrorHandler.start(): self.clear()
        handler.setattr(intern!(py, "tree"), PyDict::new(py))?;
    } else {
        handler.call_method1(intern!(py, "start"), (validator,))?;
    }

    // -- the body --------------------------------------------------------------
    if normalize {
        program = checked.dance(validator, program)?;
        if unsupported(&program) {
            return Ok(Outcome::PreludeDone);
        }
    }
    stats::count(Stat::NativeCalls);
    let engine = checked.engine(validator, &document)?;
    let level: &Level = &program.get().level;
    let frame = engine.root_frame(document, level, cfg, update)?;
    if normalize {
        engine.normalize_mapping(&frame, level)?;
    }
    if validation.is_some() {
        engine.validate_frame(&frame)?;
    }
    if !stock {
        handler.call_method1(intern!(py, "end"), (validator,))?;
    }
    Ok(Outcome::Done)
}

// ---------------------------------------------------------------------------
// engine: infrastructure
// ---------------------------------------------------------------------------

impl<'py> Engine<'py> {
    fn root_frame<'a>(
        &self,
        document: Bound<'py, PyDict>,
        level: &'a Level,
        cfg: Cfg<'py>,
        update: bool,
    ) -> PyResult<Frame<'a, 'py>> {
        let py = self.py;
        let validator = &self.validator;
        let errors = attribute(&self.instance, validator, intern!(py, "_errors"))?.cast_into::<PyList>()?;
        let log = attribute(&self.instance, validator, intern!(py, "_error_log"))?.cast_into::<PyList>()?;
        let is_normalized = match self.config.get_item(intern!(py, "_is_normalized"))? {
            Some(value) => value.is_truthy()?,
            None => false,
        };
        Ok(Frame {
            parent: None,
            obj: Some(self.validator.clone()),
            config: Some(self.config.clone()),
            live: self.emitter.is_some() || level.hooks || cfg.au_hooks,
            doc: Doc::new(DocKind::Dict(document)),
            lvl: Lvl::Fields(level),
            single: None,
            cfg,
            dcrumb: None,
            scrumb: Crumbs::new(),
            update,
            is_normalized: Cell::new(is_normalized),
            clean: Cell::new(errors.is_empty()),
            errors: RefCell::new(Some(errors)),
            log: RefCell::new(Some(log)),
            trees: RefCell::new(None),
            unrequired: RefCell::new(None),
            logical_au: None,
            dpath: OnceCell::new(),
            spath: OnceCell::new(),
            depth: 0,
            stale_remaining_rules: Cell::new(
                attribute(&self.instance, validator, intern!(py, "_remaining_rules"))?.is_truthy()?,
            ),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn child_frame<'a>(
        &self,
        parent: &'a Frame<'a, 'py>,
        doc: Rc<Doc<'py>>,
        lvl: Lvl<'a>,
        cfg: Cfg<'py>,
        dcrumb: Option<Bound<'py, PyAny>>,
        scrumb: Crumbs<'py>,
        update: bool,
    ) -> Frame<'a, 'py> {
        stats::count(Stat::NativeFrames);
        Frame {
            parent: Some(parent),
            obj: None,
            config: None,
            live: false,
            doc,
            lvl,
            single: None,
            cfg,
            // `_get_child_validator` takes `None` for "no crumb", so a field
            // named `None` does not extend the document path
            dcrumb: dcrumb.filter(|crumb| !crumb.is_none()),
            scrumb,
            update,
            is_normalized: Cell::new(parent.is_normalized.get()),
            errors: RefCell::new(None),
            log: RefCell::new(None),
            trees: RefCell::new(None),
            clean: Cell::new(true),
            unrequired: RefCell::new(None),
            logical_au: None,
            dpath: OnceCell::new(),
            spath: OnceCell::new(),
            depth: parent.depth + 1,
            stale_remaining_rules: Cell::new(false),
        }
    }

    /// A frame for a real child validator that was just handed its document
    /// (see `begin_validation` / `begin_normalization`).
    fn object_frame<'a>(
        &self,
        parent: &'a Frame<'a, 'py>,
        obj: Bound<'py, PyAny>,
        lvl: Lvl<'a>,
        cfg: Cfg<'py>,
        update: bool,
    ) -> PyResult<Frame<'a, 'py>> {
        let py = self.py;
        stats::count(Stat::ObjectFrames);
        let document = obj.getattr(intern!(py, "document"))?.cast_into_exact::<PyDict>()?;
        let errors = obj.getattr(intern!(py, "_errors"))?.cast_into::<PyList>()?;
        let log = obj.getattr(intern!(py, "_error_log"))?.cast_into::<PyList>()?;
        let config = obj.getattr(intern!(py, "_config"))?.cast_into_exact::<PyDict>()?;
        Ok(Frame {
            parent: Some(parent),
            obj: Some(obj),
            config: Some(config),
            live: true,
            doc: Doc::new(DocKind::Dict(document)),
            lvl,
            single: None,
            cfg,
            dcrumb: None,
            scrumb: Crumbs::new(),
            update,
            is_normalized: Cell::new(parent.is_normalized.get()),
            errors: RefCell::new(Some(errors)),
            log: RefCell::new(Some(log)),
            trees: RefCell::new(None),
            clean: Cell::new(true),
            unrequired: RefCell::new(None),
            logical_au: None,
            dpath: OnceCell::new(),
            spath: OnceCell::new(),
            depth: parent.depth + 1,
            stale_remaining_rules: Cell::new(false),
        })
    }

    /// The validator object of a frame whose rules need one.
    fn obj<'f>(&self, f: &'f Frame<'_, 'py>) -> PyResult<&'f Bound<'py, PyAny>> {
        f.obj.as_ref().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("ferberus: a hook was reached in a frame without validator")
        })
    }

    /// The head of `validate()` for a child validator.
    fn begin_validation(&self, child: &Bound<'py, PyAny>, document: &Bound<'py, PyAny>, update: bool) -> PyResult<()> {
        self.refs.begin_validation.bind(self.py).call1((child, document, update)).map(drop)
    }

    /// The head of `normalized()` for a child validator.
    fn begin_normalization(&self, child: &Bound<'py, PyAny>, document: &Bound<'py, PyAny>) -> PyResult<()> {
        self.refs.begin_normalization.bind(self.py).call1((child, document)).map(drop)
    }

    /// `self.document_path`
    fn dpath<'f>(&self, f: &'f Frame<'_, 'py>) -> PyResult<&'f Bound<'py, PyTuple>> {
        if let Some(path) = f.dpath.get() {
            return Ok(path);
        }
        let py = self.py;
        let path = match (&f.obj, f.parent) {
            (Some(obj), _) => obj.getattr(intern!(py, "document_path"))?.cast_into::<PyTuple>()?,
            (None, None) => unreachable!("the root frame has a validator"),
            (None, Some(parent)) => {
                let base = self.dpath(parent)?;
                match &f.dcrumb {
                    None => base.clone(),
                    // a tuple as crumb is spliced into the path, see
                    // `_get_child_validator`
                    Some(crumb) => match crumb.cast::<PyTuple>() {
                        Ok(tuple) => base.as_any().add(tuple)?.cast_into()?,
                        Err(_) => extend_path(base, std::slice::from_ref(crumb))?,
                    },
                }
            }
        };
        Ok(f.dpath.get_or_init(|| path))
    }

    /// `self.schema_path`
    fn spath<'f>(&self, f: &'f Frame<'_, 'py>) -> PyResult<&'f Bound<'py, PyTuple>> {
        if let Some(path) = f.spath.get() {
            return Ok(path);
        }
        let py = self.py;
        let path = match (&f.obj, f.parent) {
            (Some(obj), _) => obj.getattr(intern!(py, "schema_path"))?.cast_into::<PyTuple>()?,
            (None, None) => unreachable!("the root frame has a validator"),
            (None, Some(parent)) => extend_path(self.spath(parent)?, &f.scrumb)?,
        };
        Ok(f.spath.get_or_init(|| path))
    }

    /// The rules of `field` in the frame's schema (`self.schema.get(field)`).
    fn rules_of<'a>(&self, f: &Frame<'a, 'py>, key: Key<'_, 'py>) -> PyResult<Option<&'a Arc<RuleSet>>> {
        match f.lvl {
            Lvl::Fields(level) => level.get(&key.object(self.py)),
            Lvl::Uniform(rules) => Ok(Some(rules)),
            Lvl::Indexed(rules) => Ok(match key {
                Key::Index(i) => rules.get(i).and_then(Option::as_ref),
                // the positions as keys of a real `{i: member}` dict
                Key::Obj(position) => match exact_of(position) {
                    Exact::Int => position.extract::<usize>().ok().and_then(|i| rules.get(i)).and_then(Option::as_ref),
                    _ => None,
                },
            }),
            Lvl::Single(rules) => {
                let single = f.single.as_ref().expect("single field");
                let field = key.object(self.py);
                Ok(if field.as_ptr() == single.as_ptr() || field.eq(single)? { Some(rules) } else { None })
            }
        }
    }

    /// `field in self.schema`
    fn schema_contains(&self, f: &Frame<'_, 'py>, field: &Bound<'py, PyAny>) -> PyResult<bool> {
        match f.lvl {
            Lvl::Fields(level) => level.index.bind(self.py).contains(field),
            Lvl::Uniform(_) | Lvl::Indexed(_) => f.doc.as_dict(self.py)?.contains(field),
            Lvl::Single(_) => {
                let single = f.single.as_ref().expect("single field");
                Ok(field.as_ptr() == single.as_ptr() || (field.hash().is_ok() && field.eq(single)?))
            }
        }
    }

    /// `self.schema.items()` as `(field, rules)` pairs.
    fn schema_items<'a>(&self, f: &Frame<'a, 'py>) -> PyResult<Vec<(Bound<'py, PyAny>, &'a Arc<RuleSet>)>> {
        let py = self.py;
        Ok(match f.lvl {
            Lvl::Fields(level) => level.fields.iter().map(|(key, rules)| (key.bind(py).clone(), rules)).collect(),
            Lvl::Uniform(rules) => match &f.doc.kind {
                DocKind::Dict(dict) => dict_snapshot(dict).into_iter().map(|(key, _)| (key, rules)).collect(),
                DocKind::Seq(items) => (0..items.len()).map(|i| (Key::Index(i).object(py), rules)).collect(),
                DocKind::Pairs(pairs) => pairs.iter().map(|(key, _)| (key.clone(), rules)).collect(),
            },
            Lvl::Indexed(all) => all
                .iter()
                .enumerate()
                .filter_map(|(i, rules)| rules.as_ref().map(|rules| (Key::Index(i).object(py), rules)))
                .collect(),
            Lvl::Single(rules) => vec![(f.single.clone().expect("single field"), rules)],
        })
    }

    /// `self.document.get(field)`
    fn document_get(&self, f: &Frame<'_, 'py>, field: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
        f.doc.as_dict(self.py)?.get_item(field)
    }

    fn is_exception(&self, error: &PyErr) -> bool {
        error.is_instance_of::<PyException>(self.py)
    }

    fn is_schema_rule_type_error(&self, error: &PyErr) -> bool {
        error.is_instance(self.py, self.refs.schema_rule_type_error.bind(self.py))
    }

    // -- errors -----------------------------------------------------------------

    /// `self._error(field, definition, *info)`
    fn error(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        def: &ErrDef,
        rules: Option<&RuleSet>,
        value: Value<'_, 'py>,
        info: Bound<'py, PyTuple>,
    ) -> PyResult<()> {
        let py = self.py;
        let refs = self.refs;
        let field = key.object(py);
        let document_path = extend_path(self.dpath(f)?, std::slice::from_ref(&field))?;
        let mut schema_path = self.spath(f)?.clone().into_any();
        let (rule, constraint) = match &def.rule {
            None => (py.None().into_bound(py), py.None().into_bound(py)),
            Some(rule) => {
                let rule = rule.bind(py);
                if def.code != refs.errs.unknown_field.code {
                    schema_path = extend_path(self.spath(f)?, &[field.clone(), rule.clone().into_any()])?.into_any();
                }
                // rules_set = self._resolve_rules_set(self._resolve_schema(self.schema)[field])
                let Some(rules) = rules else {
                    return Err(PyKeyError::new_err((field.unbind(),)));
                };
                let constraint = if def.code == refs.errs.not_nullable.code {
                    match &rules.nullable {
                        Some(value) => value.bind(py).clone(),
                        None => pyo3::types::PyBool::new(py, false).to_owned().into_any(),
                    }
                } else if def.code == refs.errs.required_field.code {
                    match &rules.required {
                        Some(value) => value.bind(py).clone(),
                        None => {
                            schema_path = intern!(py, "__require_all__").clone().into_any();
                            f.cfg.require_all.clone()
                        }
                    }
                } else {
                    match rules.src.bind(py).get_item(rule)? {
                        Some(value) => value,
                        None => return Err(PyKeyError::new_err((rule.clone().unbind(),))),
                    }
                };
                (rule.clone().into_any(), constraint)
            }
        };
        let value = match value {
            Value::Given(value) => value.clone(),
            Value::Lookup => self.document_get(f, &field)?.unwrap_or_else(|| py.None().into_bound(py)),
        };
        let error = refs.validation_error.bind(py).call1((
            document_path,
            schema_path,
            def.code_obj.bind(py),
            rule,
            constraint,
            value,
            info,
        ))?;
        if let Some(obj) = &f.obj {
            obj.setattr(intern!(py, "recent_error"), &error)?;
        }
        self.submit(f, &[error])
    }

    /// `self._error(errors)`: bulk addition.
    fn submit(&self, f: &Frame<'_, 'py>, new: &[Bound<'py, PyAny>]) -> PyResult<()> {
        let py = self.py;
        if new.is_empty() {
            return Ok(());
        }
        let errors = self.error_list(f)?;
        let sorted = errors.len();
        for error in new {
            errors.append(error)?;
            if f.clean.get() && !crate::errors::has_plain_paths(error)? {
                f.clean.set(false);
            }
        }
        let mut clean = f.clean.get();
        let sorting = sort_errors(&errors, sorted, &mut clean);
        f.clean.set(clean);
        sorting?;

        let log = self.error_log(f)?;
        if f.live {
            // Python code may have replaced or built the trees meanwhile
            *f.trees.borrow_mut() = None;
        }
        if f.trees.borrow().is_none() && (!clean || self.object_trees_exist(f)?) {
            // From here on the trees are maintained eagerly: once paths are
            // not plain or their order is ambiguous, adding to the trees can
            // raise like upstream does during validation.
            let trees = if let Some(obj) = &f.obj {
                (obj.getattr(intern!(py, "document_error_tree"))?, obj.getattr(intern!(py, "schema_error_tree"))?)
            } else {
                let errors_module = py.import("ferberus.errors")?;
                (
                    errors_module.getattr("DocumentErrorTree")?.call1((&log,))?,
                    errors_module.getattr("SchemaErrorTree")?.call1((&log,))?,
                )
            };
            *f.trees.borrow_mut() = Some(trees);
        }
        for error in new {
            log.append(error)?;
            if let Some((document_tree, schema_tree)) = f.trees.borrow().as_ref() {
                document_tree.call_method1(intern!(py, "add"), (error,))?;
                schema_tree.call_method1(intern!(py, "add"), (error,))?;
            }
            if f.parent.is_none()
                && let Some(handler) = &self.emitter
            {
                handler.call_method1(intern!(py, "emit"), (error,))?;
            }
        }
        Ok(())
    }

    /// Did Python code build the trees of the frame's validator?
    fn object_trees_exist(&self, f: &Frame<'_, 'py>) -> PyResult<bool> {
        match &f.obj {
            Some(obj) if f.live => Ok(!obj.getattr(intern!(self.py, "_document_error_tree"))?.is_none()
                || !obj.getattr(intern!(self.py, "_schema_error_tree"))?.is_none()),
            _ => Ok(false),
        }
    }

    fn error_list(&self, f: &Frame<'_, 'py>) -> PyResult<Bound<'py, PyList>> {
        if let Some(errors) = f.errors.borrow().as_ref() {
            return Ok(errors.clone());
        }
        let errors = self.refs.error_list.bind(self.py).call0()?.cast_into::<PyList>()?;
        *f.errors.borrow_mut() = Some(errors.clone());
        Ok(errors)
    }

    fn error_log(&self, f: &Frame<'_, 'py>) -> PyResult<Bound<'py, PyList>> {
        if let Some(log) = f.log.borrow().as_ref() {
            return Ok(log.clone());
        }
        let log = PyList::empty(self.py);
        *f.log.borrow_mut() = Some(log.clone());
        Ok(log)
    }

    /// `validator._errors` of a finished native child, if there are any.
    ///
    /// The parent inherits the child's doubts about the order of errors.
    fn errors_of(&self, f: &Frame<'_, 'py>) -> Option<Bound<'py, PyList>> {
        let errors = f.errors.borrow().as_ref().filter(|errors| !errors.is_empty()).cloned();
        if errors.is_some()
            && !f.clean.get()
            && let Some(parent) = f.parent
        {
            parent.clean.set(false);
        }
        errors
    }

    /// `definition in self.document_error_tree.fetch_errors_from(self.document_path + (field,))`
    fn has_error_at(&self, f: &Frame<'_, 'py>, key: Key<'_, 'py>, def: &ErrDef) -> PyResult<bool> {
        let Some(log) = f.log.borrow().as_ref().cloned() else {
            return Ok(false);
        };
        if log.is_empty() {
            return Ok(false);
        }
        let path = extend_path(self.dpath(f)?, &[key.object(self.py)])?;
        log_has_error_at(log.as_any(), &path, def.code)
    }

    // -- delegation ---------------------------------------------------------------

    /// Creates a real child validator of `f`, like `_get_child_validator`.
    fn delegated_child(
        &self,
        f: &Frame<'_, 'py>,
        dcrumb: Option<&Bound<'py, PyAny>>,
        scrumb: &[Bound<'py, PyAny>],
        kwargs: &[(&Bound<'py, PyString>, Bound<'py, PyAny>)],
        python: Option<&'static str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        // `python`: why the reference implementation runs the child
        if let Some(reason) = python {
            stats::count(Stat::DelegatedFrames);
            stats::reason(reason);
        }
        let dcrumb = dcrumb.filter(|crumb| !crumb.is_none());
        let schema_crumb = PyTuple::new(py, scrumb)?;
        if let Some(obj) = &f.obj {
            let arguments = PyDict::new(py);
            if let Some(crumb) = dcrumb {
                arguments.set_item(intern!(py, "document_crumb"), crumb)?;
            }
            arguments.set_item(intern!(py, "schema_crumb"), schema_crumb)?;
            for (name, value) in kwargs {
                arguments.set_item(name, value)?;
            }
            return obj.call_method(intern!(py, "_get_child_validator"), (), Some(&arguments));
        }

        let config = self.flattened_config(f, kwargs)?;
        let child = self.validator.get_type().call((), Some(&config))?;
        let (document_path, schema_path) = self.child_paths(f, dcrumb, scrumb)?;
        child.setattr(intern!(py, "document_path"), document_path)?;
        child.setattr(intern!(py, "schema_path"), schema_path)?;
        Ok(child)
    }

    /// `(self.document_path + document_crumb, self.schema_path + schema_crumb)`
    fn child_paths(
        &self,
        f: &Frame<'_, 'py>,
        dcrumb: Option<&Bound<'py, PyAny>>,
        scrumb: &[Bound<'py, PyAny>],
    ) -> PyResult<(Bound<'py, PyTuple>, Bound<'py, PyTuple>)> {
        let document_path = match dcrumb.filter(|crumb| !crumb.is_none()) {
            None => self.dpath(f)?.clone(),
            // a tuple as crumb is spliced into the path
            Some(crumb) => match crumb.cast::<PyTuple>() {
                Ok(tuple) => self.dpath(f)?.as_any().add(tuple)?.cast_into()?,
                Err(_) => extend_path(self.dpath(f)?, std::slice::from_ref(crumb))?,
            },
        };
        Ok((document_path, extend_path(self.spath(f)?, scrumb)?))
    }

    /// The configuration of a child of a frame without validator object:
    /// what `_get_child_validator` would derive along the chain of parents.
    fn flattened_config(
        &self,
        f: &Frame<'_, 'py>,
        kwargs: &[(&Bound<'py, PyString>, Bound<'py, PyAny>)],
    ) -> PyResult<Bound<'py, PyDict>> {
        let py = self.py;
        let config = self.config.copy()?;
        config.set_item(intern!(py, "is_child"), true)?;
        // (no 'error_handler': the child of a child validator gets a new
        // BasicErrorHandler, only first-level children share the toy handler)
        let root = &self.validator;
        config.set_item(intern!(py, "root_allow_unknown"), root.getattr(intern!(py, "allow_unknown"))?)?;
        config.set_item(intern!(py, "root_require_all"), root.getattr(intern!(py, "require_all"))?)?;
        config.set_item(intern!(py, "root_document"), &self.root_document)?;
        config.set_item(intern!(py, "root_schema"), root.getattr(intern!(py, "schema"))?)?;
        config.set_item(intern!(py, "allow_unknown"), &f.cfg.allow_unknown)?;
        config.set_item(intern!(py, "require_all"), &f.cfg.require_all)?;
        if let Some(purge_unknown) = &f.cfg.purge_unknown {
            config.set_item(intern!(py, "purge_unknown"), purge_unknown)?;
        }
        config.set_item(intern!(py, "_is_normalized"), f.is_normalized.get())?;
        for (name, value) in kwargs {
            config.set_item(name, value)?;
        }
        Ok(config)
    }

    /// Assembles a child validator of `f` without running Python code: the
    /// result of `_get_child_validator(...)` followed by the head of
    /// `validate()` (`validation: Some(update)`) or `normalized()`.
    ///
    /// `None` if that is not possible for the validator class or document.
    fn assembled_child(
        &self,
        f: &Frame<'_, 'py>,
        dcrumb: Option<&Bound<'py, PyAny>>,
        scrumb: &[Bound<'py, PyAny>],
        kwargs: &[(&Bound<'py, PyString>, Bound<'py, PyAny>)],
        document: &Bound<'py, PyAny>,
        validation: Option<bool>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = self.py;
        let refs = self.refs;
        if !self.plain_children {
            return Ok(None);
        }
        let Ok(document) = document.cast_exact::<PyDict>() else {
            return Ok(None);
        };
        let false_ = || pyo3::types::PyBool::new(py, false).to_owned().into_any();

        // -- _get_child_validator: child_config ---------------------------------
        let (config, first_level) = match (&f.obj, &f.config) {
            (Some(obj), Some(parent_config)) => {
                let config = parent_config.copy()?;
                for (name, value) in kwargs {
                    config.set_item(name, value)?;
                }
                let first_level = f.parent.is_none();
                if first_level {
                    config.set_item(intern!(py, "is_child"), true)?;
                    let get = |name| -> PyResult<Bound<'py, PyAny>> {
                        Ok(parent_config.get_item(name)?.unwrap_or_else(false_))
                    };
                    config.set_item(intern!(py, "root_allow_unknown"), get(intern!(py, "allow_unknown"))?)?;
                    config.set_item(intern!(py, "root_require_all"), get(intern!(py, "require_all"))?)?;
                    config.set_item(intern!(py, "root_document"), &self.root_document)?;
                    config.set_item(
                        intern!(py, "root_schema"),
                        attribute(&self.instance, obj, intern!(py, "_schema"))?,
                    )?;
                }
                (config, first_level)
            }
            _ => (self.flattened_config(f, kwargs)?, false),
        };
        // -- __init__ ---------------------------------------------------------------
        // first-level children share the toy handler, deeper ones get a new
        // BasicErrorHandler (their parent's configuration has none)
        let handler = if first_level {
            refs.toy_error_handler.bind(py).clone()
        } else {
            refs.basic_error_handler.bind(py).call0()?
        };
        // the property setters of allow_unknown and require_all
        for name in [intern!(py, "allow_unknown"), intern!(py, "require_all")] {
            if !config.contains(name)? {
                config.set_item(name, false)?;
            }
        }
        let schema = config.get_item(intern!(py, "schema"))?.unwrap_or_else(|| py.None().into_bound(py));
        let (document_path, schema_path) = self.child_paths(f, dcrumb, scrumb)?;

        let child = refs.object_new.bind(py).call1((self.validator.get_type(),))?;
        // SAFETY: plain call; returns a new reference or NULL with an exception.
        let instance = unsafe {
            Bound::from_owned_ptr_or_err(py, ffi::PyObject_GenericGetDict(child.as_ptr(), std::ptr::null_mut()))?
        }
        .cast_into_exact::<PyDict>()?;
        // attributes in the order `__init__` creates them, with the values
        // they have after the head of validate() / normalized()
        instance.set_item(intern!(py, "document"), document.copy()?)?;
        instance.set_item(intern!(py, "_errors"), refs.error_list.bind(py).call0()?)?;
        instance.set_item(intern!(py, "recent_error"), py.None())?;
        instance.set_item(intern!(py, "_error_log"), PyList::empty(py))?;
        instance.set_item(intern!(py, "_document_error_tree"), py.None())?;
        instance.set_item(intern!(py, "_schema_error_tree"), py.None())?;
        instance.set_item(intern!(py, "document_path"), document_path)?;
        instance.set_item(intern!(py, "schema_path"), schema_path)?;
        instance.set_item(intern!(py, "update"), validation.unwrap_or(false))?;
        instance.set_item(intern!(py, "error_handler"), handler)?;
        instance.set_item(intern!(py, "_config"), config)?;
        instance.set_item(intern!(py, "_schema"), schema)?;
        instance.set_item(intern!(py, "_remaining_rules"), PyList::empty(py))?;
        if validation.is_some() {
            instance.set_item(intern!(py, "_unrequired_by_excludes"), PySet::empty(py)?)?;
        }
        Ok(Some(child))
    }

    /// `validator(document, update=..., normalize=False)`, returning the
    /// child's errors if there are any.
    fn delegated_validate(
        &self,
        f: &Frame<'_, 'py>,
        child: &Bound<'py, PyAny>,
        document: &Bound<'py, PyAny>,
        update: bool,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = self.py;
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "update"), update)?;
        kwargs.set_item(intern!(py, "normalize"), false)?;
        child.call((document,), Some(&kwargs))?;
        let errors = child.getattr(intern!(py, "_errors"))?;
        Ok(if errors.is_truthy()? {
            // nothing is known about errors that come from Python
            f.clean.set(false);
            Some(errors)
        } else {
            None
        })
    }

    /// `validator.normalized(document, always_return_document=True)`
    fn delegated_normalize(
        &self,
        f: &Frame<'_, 'py>,
        child: &Bound<'py, PyAny>,
        document: &Bound<'py, PyAny>,
    ) -> PyResult<(Bound<'py, PyAny>, Option<Bound<'py, PyAny>>)> {
        let py = self.py;
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "always_return_document"), true)?;
        let result = child.call_method(intern!(py, "normalized"), (document,), Some(&kwargs))?;
        let errors = child.getattr(intern!(py, "_errors"))?;
        if errors.is_truthy()? {
            f.clean.set(false);
            Ok((result, Some(errors)))
        } else {
            Ok((result, None))
        }
    }

    fn name(&self, text: &'static str) -> Bound<'py, PyAny> {
        let py = self.py;
        let name = match text {
            "schema" => intern!(py, "schema"),
            "items" => intern!(py, "items"),
            "keysrules" => intern!(py, "keysrules"),
            "valuesrules" => intern!(py, "valuesrules"),
            "allow_unknown" => intern!(py, "allow_unknown"),
            "__allow_unknown__" => intern!(py, "__allow_unknown__"),
            "dependencies" => intern!(py, "dependencies"),
            "anyof" => intern!(py, "anyof"),
            "allof" => intern!(py, "allof"),
            "noneof" => intern!(py, "noneof"),
            "oneof" => intern!(py, "oneof"),
            other => return PyString::intern(py, other).into_any(),
        };
        name.clone().into_any()
    }

    // -----------------------------------------------------------------------
    // validation
    // -----------------------------------------------------------------------

    /// The body of `validate()` after normalization.
    fn validate_frame(&self, f: &Frame<'_, 'py>) -> PyResult<()> {
        match &f.doc.kind {
            DocKind::Dict(dict) if f.live => {
                // `for field in self.document:` with Python code that can
                // change the document underway
                for field in dict.as_any().try_iter()? {
                    let field = field?;
                    let Some(value) = dict.get_item(&field)? else {
                        return Err(PyKeyError::new_err((field.unbind(),)));
                    };
                    self.validate_field(f, Key::Obj(&field), &value)?;
                }
            }
            DocKind::Dict(dict) => {
                for (position, (field, value)) in dict_snapshot(dict).iter().enumerate() {
                    if f.cfg.ignore_none_values && value.is_none() {
                        continue;
                    }
                    let rules = match f.lvl {
                        // documents tend to list their fields in the order
                        // of the schema, with the very same key objects
                        Lvl::Fields(level) => match level.fields.get(position) {
                            Some((key, rules)) if key.as_ptr() == field.as_ptr() => Some(rules),
                            _ => level.get(field)?,
                        },
                        _ => self.rules_of(f, Key::Obj(field))?,
                    };
                    match rules {
                        Some(rules) => self.validate_definitions(f, rules, Key::Obj(field), value)?,
                        None => self.validate_unknown_field(f, Key::Obj(field), value)?,
                    }
                }
            }
            DocKind::Seq(items) => {
                for (i, value) in items.iter().enumerate() {
                    self.validate_field(f, Key::Index(i), value)?;
                }
            }
            DocKind::Pairs(pairs) => {
                for (field, value) in pairs {
                    self.validate_field(f, Key::Obj(field), value)?;
                }
            }
        }
        if !f.update {
            self.validate_required_fields(f)?;
        }
        Ok(())
    }

    #[inline]
    fn validate_field(&self, f: &Frame<'_, 'py>, key: Key<'_, 'py>, value: &Bound<'py, PyAny>) -> PyResult<()> {
        if f.cfg.ignore_none_values && value.is_none() {
            return Ok(());
        }
        match self.rules_of(f, key)? {
            Some(rules) => self.validate_definitions(f, rules, key, value),
            None => self.validate_unknown_field(f, key, value),
        }
    }

    /// Decides how the child validator of a rule is run.
    ///
    /// * `supported`: the engine has a compiled form of the child's schema
    ///   and configuration,
    /// * `hooks`: that form calls methods of the validator,
    /// * `native_document` / `object_document`: the document at hand suits a
    ///   frame without / with validator object.
    fn mode(
        &self,
        f: &Frame<'_, 'py>,
        supported: bool,
        hooks: bool,
        native_document: bool,
        object_document: bool,
    ) -> Mode {
        if !supported || f.depth >= MAX_NATIVE_DEPTH {
            Mode::Python
        } else if !hooks && native_document {
            Mode::Native
        } else if object_document {
            Mode::Object
        } else {
            Mode::Python
        }
    }

    /// Runs a real child validator on `document`, like
    /// `validator(document, update=update, normalize=False)`: with the native
    /// body if `body` is given, else by the reference implementation.
    /// Returns the child's errors if there are any.
    #[allow(clippy::too_many_arguments)]
    fn validate_with_object(
        &self,
        f: &Frame<'_, 'py>,
        body: Option<ObjectBody<'_, 'py>>,
        dcrumb: Option<&Bound<'py, PyAny>>,
        scrumb: &[Bound<'py, PyAny>],
        kwargs: &[(&Bound<'py, PyString>, Bound<'py, PyAny>)],
        document: &Bound<'py, PyAny>,
        update: bool,
        reason: &'static str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let assembled = match &body {
            Some(_) => self.assembled_child(f, dcrumb, scrumb, kwargs, document, Some(update))?,
            None => None,
        };
        let begun = assembled.is_some();
        let child = match assembled {
            Some(child) => child,
            None => {
                self.delegated_child(f, dcrumb, scrumb, kwargs, if body.is_some() { None } else { Some(reason) })?
            }
        };
        match body {
            None => self.delegated_validate(f, &child, document, update),
            Some(body) => {
                if !begun {
                    self.begin_validation(&child, document, update)?;
                }
                let mut frame = self.object_frame(f, child, body.lvl, body.cfg, update)?;
                frame.single = body.single;
                frame.logical_au = body.logical_au;
                self.validate_frame(&frame)?;
                Ok(self.errors_of(&frame).map(Bound::into_any))
            }
        }
    }

    /// Runs a real child validator on `document`, like
    /// `validator.normalized(document, always_return_document=True)`.
    /// Returns the normalized document and the child's errors, if any.
    #[allow(clippy::too_many_arguments)]
    fn normalize_with_object(
        &self,
        f: &Frame<'_, 'py>,
        body: Option<(&Level, Cfg<'py>)>,
        dcrumb: Option<&Bound<'py, PyAny>>,
        scrumb: &[Bound<'py, PyAny>],
        kwargs: &[(&Bound<'py, PyString>, Bound<'py, PyAny>)],
        document: &Bound<'py, PyAny>,
        reason: &'static str,
    ) -> PyResult<(Bound<'py, PyAny>, Option<Bound<'py, PyAny>>)> {
        let assembled = match &body {
            Some(_) => self.assembled_child(f, dcrumb, scrumb, kwargs, document, None)?,
            None => None,
        };
        let begun = assembled.is_some();
        let child = match assembled {
            Some(child) => child,
            None => {
                self.delegated_child(f, dcrumb, scrumb, kwargs, if body.is_some() { None } else { Some(reason) })?
            }
        };
        match body {
            None => self.delegated_normalize(f, &child, document),
            Some((level, cfg)) => {
                if !begun {
                    self.begin_normalization(&child, document)?;
                }
                let frame = self.object_frame(f, child, Lvl::Fields(level), cfg, false)?;
                self.normalize_mapping(&frame, level)?;
                let result = f_document(&frame).clone().into_any();
                Ok((result, self.errors_of(&frame).map(Bound::into_any)))
            }
        }
    }

    /// `__validate_unknown_fields`
    fn validate_unknown_field(&self, f: &Frame<'_, 'py>, key: Key<'_, 'py>, value: &Bound<'py, PyAny>) -> PyResult<()> {
        let py = self.py;
        if !f.cfg.au_truthy {
            return self.error(f, key, &self.refs.errs.unknown_field, None, Value::Given(value), PyTuple::empty(py));
        }
        if !(f.cfg.au_is_mapping || f.cfg.au_is_str) {
            return Ok(());
        }
        let field = key.object(py);
        let schema_crumb = self.name(if f.parent.is_some() { "allow_unknown" } else { "__allow_unknown__" });
        let rules = f.cfg.au_rules.as_ref();
        let mode = self.mode(f, rules.is_some(), rules.is_some_and(|rules| rules.hooks), true, true);
        let errors = match (mode, rules) {
            (Mode::Native, Some(rules)) => {
                let doc = Doc::new(DocKind::Pairs(vec![(field.clone(), value.clone())]));
                let mut child = self.child_frame(
                    f,
                    doc,
                    Lvl::Single(rules),
                    f.cfg.clone(),
                    None,
                    smallvec::smallvec![schema_crumb],
                    false,
                );
                child.single = Some(field.clone());
                self.validate_frame(&child)?;
                self.errors_of(&child).map(Bound::into_any)
            }
            (mode, rules) => {
                let schema = PyDict::new(py);
                schema.set_item(&field, &f.cfg.allow_unknown)?;
                let document = PyDict::new(py);
                document.set_item(&field, value)?;
                let body = match (mode, rules) {
                    (Mode::Object, Some(rules)) => Some(ObjectBody {
                        lvl: Lvl::Single(rules),
                        cfg: f.cfg.clone(),
                        single: Some(field.clone()),
                        logical_au: None,
                    }),
                    _ => None,
                };
                self.validate_with_object(
                    f,
                    body,
                    None,
                    &[schema_crumb],
                    &[(intern!(py, "schema"), schema.into_any())],
                    document.as_any(),
                    false,
                    "allow_unknown",
                )?
            }
        };
        if let Some(errors) = errors {
            let errors: Vec<_> = errors.try_iter()?.collect::<PyResult<_>>()?;
            self.submit(f, &errors)?;
        }
        Ok(())
    }

    /// `__validate_definitions`
    fn validate_definitions(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        if rules.queue_hooks {
            return self.validate_definitions_live(f, rules, key, value);
        }
        let mut dropped: u32 = 0;
        for (position, entry) in rules.queue.iter().enumerate() {
            if dropped & entry.bit != 0 {
                continue;
            }
            match self.run_op(f, rules, &entry.op, key, value, &mut dropped) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Stop) => break,
                Err(error) if self.is_schema_rule_type_error(&error) => break,
                Err(error) => {
                    if f.parent.is_none() {
                        // an exception leaves the queue of the field behind
                        let remaining = rules.queue[position + 1..]
                            .iter()
                            .filter(|entry| dropped & entry.bit == 0)
                            .map(|entry| entry.name.bind(self.py));
                        self.validator
                            .setattr(intern!(self.py, "_remaining_rules"), PyList::new(self.py, remaining)?)?;
                    }
                    return Err(error);
                }
            }
        }
        if f.stale_remaining_rules.get() {
            // self._drop_remaining_rules()
            f.stale_remaining_rules.set(false);
            self.validator.setattr(intern!(self.py, "_remaining_rules"), PyList::empty(self.py))?;
        }
        Ok(())
    }

    /// `__validate_definitions` for a field with rules that run Python code
    /// on the validator: the queue is the real `self._remaining_rules`,
    /// which such code may manipulate.
    fn validate_definitions_live(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        let attribute = intern!(py, "_remaining_rules");
        let validator = self.obj(f)?;
        validator.setattr(attribute, PyList::new(py, rules.queue.iter().map(|entry| entry.name.bind(py)))?)?;
        loop {
            let queue = validator.getattr(attribute)?;
            if !queue.is_truthy()? {
                break;
            }
            let rule = queue.call_method1(intern!(py, "pop"), (0,))?;
            let entry: Option<&QEntry> = rules.queue.iter().find(|entry| entry.name.as_ptr() == rule.as_ptr());
            let entry = match entry {
                Some(entry) => Some(entry),
                None => {
                    let mut found = None;
                    for entry in &rules.queue {
                        if entry.name.bind(py).as_any().eq(&rule)? {
                            found = Some(entry);
                            break;
                        }
                    }
                    found
                }
            };
            let mut dropped: u32 = 0;
            let outcome = match entry {
                Some(entry) => self.run_op(f, rules, &entry.op, key, value, &mut dropped),
                None => {
                    // a rule that Python code put into the queue
                    let name = rule.cast::<PyString>()?.to_str()?.replace(' ', "_");
                    match validator.getattr_opt(format!("_validate_{name}").as_str())? {
                        Some(handler) if !handler.is_none() => {
                            let constraint =
                                rules.src.bind(py).get_item(&rule)?.unwrap_or_else(|| py.None().into_bound(py));
                            handler
                                .call1((constraint, key.object(py), value))
                                .and_then(|result| Ok(if result.is_truthy()? { Flow::Stop } else { Flow::Continue }))
                        }
                        _ => Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "There's no handler for '{}' in the 'validate' domain.",
                            rule.str()?.extract::<PyBackedStr>()?
                        ))),
                    }
                }
            };
            // self._drop_remaining_rules(...) of native rules; they also took
            // effect if the rule went on to raise
            if dropped == DROP_ALL {
                validator.setattr(attribute, PyList::empty(py))?;
            } else if dropped != 0 {
                let queue = validator.getattr(attribute)?.cast_into::<PyList>()?;
                let mut position = 0;
                while position < queue.len() {
                    let name = queue.get_item(position)?;
                    let bit = match name.cast::<PyString>() {
                        Ok(name) => RuleId::of(name.to_str()?).bit(),
                        Err(_) => 0,
                    };
                    if bit != RuleId::Other.bit() && dropped & bit != 0 {
                        queue.del_item(position)?;
                    } else {
                        position += 1;
                    }
                }
            }
            match outcome {
                Ok(Flow::Continue) => {}
                Ok(Flow::Stop) => break,
                Err(error) if self.is_schema_rule_type_error(&error) => break,
                Err(error) => return Err(error),
            }
        }
        validator.setattr(attribute, PyList::empty(py))?;
        f.stale_remaining_rules.set(false);
        Ok(())
    }

    /// `_validate_check_with`
    fn validate_check_with(
        &self,
        validator: &Bound<'py, PyAny>,
        checks: &Bound<'py, PyAny>,
        field: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        if let Ok(name) = checks.cast::<PyString>() {
            let method = format!("_check_with_{}", name.to_str()?.replace(' ', "_"));
            return match validator.getattr_opt(method.as_str())? {
                Some(checker) if !checker.is_none() => checker.call1((field, value)).map(drop),
                // legacy `_validator_<name>` methods and the missing handler:
                // leave it to the original
                _ => validator.call_method1(intern!(py, "_validate_check_with"), (checks, field, value)).map(drop),
            };
        }
        if is_iterable(self.refs, checks)? {
            for check in checks.try_iter()? {
                self.validate_check_with(validator, &check?, field, value)?;
            }
            return Ok(());
        }
        checks.call1((field, value, validator.getattr(intern!(py, "_error"))?)).map(drop)
    }

    /// Python code with access to the validator ran: it may have filed
    /// errors, so nothing is known about the order of the error list anymore.
    fn after_hook(&self, f: &Frame<'_, 'py>) {
        f.clean.set(false);
    }

    fn run_op(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        op: &Op,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        dropped: &mut u32,
    ) -> PyResult<Flow> {
        let py = self.py;
        let refs = self.refs;
        let errs = &refs.errs;
        let no_info = || PyTuple::empty(py);
        let fail =
            |def: &ErrDef, info: Bound<'py, PyTuple>| self.error(f, key, def, Some(rules), Value::Given(value), info);
        match op {
            // _validate_nullable
            Op::Nullable => {
                if value.is_none() {
                    let nullable = match &rules.nullable {
                        Some(constraint) => constraint.bind(py).is_truthy()?,
                        None => false,
                    };
                    if !nullable {
                        fail(&errs.not_nullable, no_info())?;
                    }
                    *dropped |= NULLABLE_DROPS;
                }
            }
            // _validate_readonly
            Op::Readonly(constraint) => {
                if constraint.bind(py).is_truthy()? {
                    if !f.is_normalized.get() {
                        fail(&errs.readonly_field, no_info())?;
                    } else if self.has_error_at(f, key, &errs.readonly_field)? {
                        *dropped = DROP_ALL;
                    }
                }
            }
            // _validate_type
            Op::Type(checks) => {
                if checks.is_empty() {
                    return Ok(Flow::Continue);
                }
                for check in checks {
                    let matched = match check {
                        TypeTest::Definition(check) => check.matches(value)?,
                        // type_handler = self.__get_rule_handler('validate_type', _type)
                        TypeTest::Method(handler) => handler.resolve(self.obj(f)?)?.call1((value,))?.is_truthy()?,
                    };
                    if matched {
                        return Ok(Flow::Continue);
                    }
                }
                fail(&errs.bad_type, no_info())?;
                *dropped = DROP_ALL;
            }
            // _validate_empty
            Op::Empty(constraint) => {
                if is_sized(refs, value)? && value.len()? == 0 {
                    *dropped |= EMPTY_DROPS;
                    if !constraint.bind(py).is_truthy()? {
                        fail(&errs.empty_not_allowed, no_info())?;
                    }
                }
            }
            // _validate_allowed
            Op::Allowed(constraint) => {
                let allowed = constraint.bind(py);
                if is_iterable(refs, value)? && !is_str(value) {
                    let mut unallowed = Vec::new();
                    for item in value.try_iter()? {
                        let item = item?;
                        if !allowed.contains(&item)? {
                            unallowed.push(item);
                        }
                    }
                    if !unallowed.is_empty() {
                        fail(&errs.unallowed_values, PyTuple::new(py, [PyTuple::new(py, unallowed)?])?)?;
                    }
                } else if !allowed.contains(value)? {
                    fail(&errs.unallowed_value, PyTuple::new(py, [value])?)?;
                }
            }
            // _validate_forbidden
            Op::Forbidden(constraint) => {
                let forbidden_values = constraint.bind(py);
                if is_sequence(refs, value)? && !is_str(value) {
                    let set = py.get_type::<PySet>();
                    let forbidden = set.call1((value,))?.bitand(set.call1((forbidden_values,))?)?;
                    if forbidden.is_truthy()? {
                        let listed = py.get_type::<PyList>().call1((forbidden,))?;
                        fail(&errs.forbidden_values, PyTuple::new(py, [listed])?)?;
                    }
                } else if forbidden_values.contains(value)? {
                    fail(&errs.forbidden_value, PyTuple::new(py, [value])?)?;
                }
            }
            // _validate_contains
            Op::Contains(constraint) => {
                if !is_iterable(refs, value)? {
                    return Ok(Flow::Continue);
                }
                let expected_values = constraint.bind(py);
                let set = py.get_type::<PySet>();
                let expected = if !is_iterable(refs, expected_values)? || is_str(expected_values) {
                    set.call1((PyTuple::new(py, [expected_values])?,))?
                } else {
                    set.call1((expected_values,))?
                };
                let missing = expected.sub(set.call1((value,))?)?;
                if missing.is_truthy()? {
                    fail(&errs.missing_members, PyTuple::new(py, [missing])?)?;
                }
            }
            // _validate_min / _validate_max
            Op::Min(constraint) => {
                if compare_swallowing_type_error(value, constraint.bind(py), pyo3::basic::CompareOp::Lt)? {
                    // `self._error()` is called inside the `try`: a TypeError
                    // raised while the error is filed (sorting!) is swallowed
                    // as well.
                    match fail(&errs.min_value, no_info()) {
                        Err(error) if error.is_instance_of::<PyTypeError>(py) => {}
                        other => other?,
                    }
                }
            }
            Op::Max(constraint) => {
                if compare_swallowing_type_error(value, constraint.bind(py), pyo3::basic::CompareOp::Gt)? {
                    // `self._error()` is called inside the `try`: a TypeError
                    // raised while the error is filed (sorting!) is swallowed
                    // as well.
                    match fail(&errs.max_value, no_info()) {
                        Err(error) if error.is_instance_of::<PyTypeError>(py) => {}
                        other => other?,
                    }
                }
            }
            // _validate_minlength / _validate_maxlength
            Op::MinLength(constraint) => {
                if is_iterable(refs, value)? {
                    let length = value.len()?;
                    if length_compare(py, length, constraint.bind(py), pyo3::basic::CompareOp::Lt)? {
                        fail(&errs.min_length, PyTuple::new(py, [length])?)?;
                    }
                }
            }
            Op::MaxLength(constraint) => {
                if is_iterable(refs, value)? {
                    let length = value.len()?;
                    if length_compare(py, length, constraint.bind(py), pyo3::basic::CompareOp::Gt)? {
                        fail(&errs.max_length, PyTuple::new(py, [length])?)?;
                    }
                }
            }
            // _validate_regex
            Op::Regex(regex) => {
                if !is_str(value) {
                    return Ok(Flow::Continue);
                }
                // the `regex` crate for ASCII, where the pattern has an equivalent
                if crate::REGEX_FAST.load(Ordering::Relaxed)
                    && exact_of(value) == Exact::Str
                    && let Ok(subject) = value.cast_exact::<PyString>()?.to_str()
                    && subject.is_ascii()
                    && let Some(fast) = regex.fast.get_or_init(|| match regex.effective.bind(py).to_str() {
                        Ok(pattern) => ferberus_regex::compile(pattern),
                        Err(_) => None,
                    })
                {
                    let subject = subject.as_bytes();
                    let matched = if subject.len() >= ferberus_regex::DETACH_THRESHOLD {
                        py.detach(|| fast.is_match(subject))
                    } else {
                        fast.is_match(subject)
                    };
                    if !matched {
                        fail(&errs.regex_mismatch, no_info())?;
                    }
                    return Ok(Flow::Continue);
                }
                let compiled = regex.compiled.get_or_try_init(py, || {
                    refs.re_compile.bind(py).call1((regex.effective.bind(py),)).map(Bound::unbind)
                })?;
                if !compiled.bind(py).call_method1(intern!(py, "match"), (value,))?.is_truthy()? {
                    fail(&errs.regex_mismatch, no_info())?;
                }
            }
            Op::CheckWith(constraint) => {
                self.validate_check_with(self.obj(f)?, constraint.bind(py), &key.object(py), value)?;
                self.after_hook(f);
            }
            Op::Python(handler, constraint) => {
                let result = handler.resolve(self.obj(f)?)?.call1((constraint.bind(py), key.object(py), value))?;
                self.after_hook(f);
                // TODO remove on next breaking release (upstream): a truthy
                // result ends the processing of the field
                if result.is_truthy()? {
                    return Ok(Flow::Stop);
                }
            }
            Op::Items(items) => self.validate_items(f, rules, items, key, value)?,
            Op::Schema => self.validate_schema(f, rules, key, value)?,
            Op::KeysRules => self.validate_keysrules(f, rules, key, value)?,
            Op::ValuesRules => self.validate_valuesrules(f, rules, key, value)?,
            Op::Dependencies(constraint) => {
                return self.validate_dependencies(f, rules, constraint.bind(py), key, value);
            }
            Op::Excludes(constraint) => self.validate_excludes(f, rules, constraint.bind(py), key, value)?,
            Op::AnyOf(logical) => {
                let (valids, errors) = self.validate_logical(f, rules, "anyof", logical, key, value)?;
                if valids < 1 {
                    fail(
                        &errs.anyof,
                        PyTuple::new(
                            py,
                            [
                                errors.into_any(),
                                valids.into_pyobject(py)?.into_any(),
                                logical.defs.len().into_pyobject(py)?.into_any(),
                            ],
                        )?,
                    )?;
                }
            }
            Op::AllOf(logical) => {
                let (valids, errors) = self.validate_logical(f, rules, "allof", logical, key, value)?;
                if valids < logical.defs.len() {
                    fail(
                        &errs.allof,
                        PyTuple::new(
                            py,
                            [
                                errors.into_any(),
                                valids.into_pyobject(py)?.into_any(),
                                logical.defs.len().into_pyobject(py)?.into_any(),
                            ],
                        )?,
                    )?;
                }
            }
            Op::NoneOf(logical) => {
                let (valids, errors) = self.validate_logical(f, rules, "noneof", logical, key, value)?;
                if valids > 0 {
                    fail(
                        &errs.noneof,
                        PyTuple::new(
                            py,
                            [
                                errors.into_any(),
                                valids.into_pyobject(py)?.into_any(),
                                logical.defs.len().into_pyobject(py)?.into_any(),
                            ],
                        )?,
                    )?;
                }
            }
            Op::OneOf(logical) => {
                let (valids, errors) = self.validate_logical(f, rules, "oneof", logical, key, value)?;
                if valids != 1 {
                    fail(
                        &errs.oneof,
                        PyTuple::new(
                            py,
                            [
                                errors.into_any(),
                                valids.into_pyobject(py)?.into_any(),
                                logical.defs.len().into_pyobject(py)?.into_any(),
                            ],
                        )?,
                    )?;
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// `_validate_items`
    fn validate_items(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        items: &ItemsC,
        key: Key<'_, 'py>,
        values: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        let errs = &self.refs.errs;
        let expected = items.rules.len();
        let actual = values.len()?;
        if expected != actual {
            return self.error(
                f,
                key,
                &errs.items_length,
                Some(rules),
                Value::Given(values),
                PyTuple::new(py, [expected, actual])?,
            );
        }
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("items")];
        let supported = items.rules.iter().all(|r| r.as_ref().is_some_and(|r| r.unsupported.is_none()));
        let hooks = f.cfg.au_hooks || items.rules.iter().any(|r| r.as_ref().is_some_and(|r| r.hooks));
        let mode = self.mode(f, supported, hooks, matches!(exact_of(values), Exact::List | Exact::Tuple), true);
        let errors = match mode {
            Mode::Native => {
                let doc = Doc::new(DocKind::Seq(values.try_iter()?.collect::<PyResult<_>>()?));
                let child =
                    self.child_frame(f, doc, Lvl::Indexed(&items.rules), f.cfg.clone(), Some(field), scrumb, f.update);
                self.validate_frame(&child)?;
                self.errors_of(&child).map(Bound::into_any)
            }
            mode => {
                // schema = dict((i, definition) for i, definition in enumerate(items))
                let schema = PyDict::new(py);
                for (i, definition) in items.raw.bind(py).try_iter()?.enumerate() {
                    schema.set_item(i, definition?)?;
                }
                let document = PyDict::new(py);
                for (i, value) in values.try_iter()?.enumerate() {
                    document.set_item(i, value?)?;
                }
                let body = matches!(mode, Mode::Object).then(|| ObjectBody {
                    lvl: Lvl::Indexed(&items.rules),
                    cfg: f.cfg.clone(),
                    single: None,
                    logical_au: None,
                });
                self.validate_with_object(
                    f,
                    body,
                    Some(&field),
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any())],
                    document.as_any(),
                    f.update,
                    "items",
                )?
            }
        };
        if let Some(errors) = errors {
            self.error(f, key, &errs.bad_items, Some(rules), Value::Given(values), PyTuple::new(py, [errors])?)?;
        }
        Ok(())
    }

    /// `_validate_schema`
    fn validate_schema(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        let refs = self.refs;
        let errs = &refs.errs;
        let schema = rules.schema.as_ref().expect("schema rule");
        let raw = schema.raw.bind(py);
        if raw.is_none() {
            return Ok(());
        }
        let exact = exact_of(value);
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("schema")];

        if exact != Exact::Str && is_sequence(refs, value)? {
            // __validate_schema_sequence
            let item_rules = schema.rules.as_ref().filter(|r| r.unsupported.is_none());
            let mode = self.mode(
                f,
                item_rules.is_some(),
                f.cfg.au_hooks || item_rules.is_some_and(|r| r.hooks),
                matches!(exact, Exact::List | Exact::Tuple),
                true,
            );
            let errors = match (mode, item_rules) {
                (Mode::Native, Some(item_rules)) => {
                    let doc = Doc::new(DocKind::Seq(value.try_iter()?.collect::<PyResult<_>>()?));
                    let child = self.child_frame(
                        f,
                        doc,
                        Lvl::Uniform(item_rules),
                        f.cfg.clone(),
                        Some(field),
                        scrumb,
                        f.update,
                    );
                    self.validate_frame(&child)?;
                    self.errors_of(&child).map(Bound::into_any)
                }
                (mode, item_rules) => {
                    let child_schema = PyDict::new(py);
                    for i in 0..value.len()? {
                        child_schema.set_item(i, raw)?;
                    }
                    let document = PyDict::new(py);
                    for (i, item) in value.try_iter()?.enumerate() {
                        document.set_item(i, item?)?;
                    }
                    let body = match (mode, item_rules) {
                        (Mode::Object, Some(item_rules)) => Some(ObjectBody {
                            lvl: Lvl::Uniform(item_rules),
                            cfg: f.cfg.clone(),
                            single: None,
                            logical_au: None,
                        }),
                        _ => None,
                    };
                    self.validate_with_object(
                        f,
                        body,
                        Some(&field),
                        &scrumb,
                        &[
                            (intern!(py, "schema"), child_schema.into_any()),
                            (intern!(py, "allow_unknown"), f.cfg.allow_unknown.clone()),
                        ],
                        document.as_any(),
                        f.update,
                        "schema (sequence)",
                    )?
                }
            };
            if let Some(errors) = errors {
                drop_nodes_from_errorpaths(&errors, self.dpath(f)?.len(), self.spath(f)?.len(), &[2])?;
                self.error(
                    f,
                    key,
                    &errs.sequence_schema,
                    Some(rules),
                    Value::Given(value),
                    PyTuple::new(py, [errors])?,
                )?;
            }
        } else if is_mapping(refs, value)? {
            // __validate_schema_mapping
            let (allow_unknown, au_rules) = self.field_allow_unknown(f, rules);
            let require_all = match &rules.require_all {
                Some(value) => value.bind(py).clone(),
                None => f.cfg.require_all.clone(),
            };
            let cfg = match f.cfg.with_allow_unknown(refs, &allow_unknown, au_rules.as_ref())? {
                Some(mut cfg) => {
                    cfg.require_all = require_all.clone();
                    Some(cfg)
                }
                None => None,
            };
            let level = schema.level.as_ref().filter(|level| level.unsupported.is_none());
            let mode = match (&level, &cfg) {
                (Some(level), Some(cfg)) => {
                    self.mode(f, true, level.hooks || cfg.au_hooks, exact == Exact::Dict, exact == Exact::Dict)
                }
                _ => Mode::Python,
            };
            let outcome = match (mode, level, cfg) {
                (Mode::Native, Some(level), Some(cfg)) => {
                    let doc = Doc::new(DocKind::Dict(value.clone().cast_into()?));
                    let child = self.child_frame(f, doc, Lvl::Fields(level), cfg, Some(field), scrumb, f.update);
                    self.validate_frame(&child).map(|()| self.errors_of(&child).map(Bound::into_any))
                }
                (mode, level, cfg) => {
                    // schema = self._resolve_schema(schema)
                    let resolved = if is_str(raw) {
                        self.validator
                            .getattr(intern!(py, "schema_registry"))?
                            .call_method1(intern!(py, "get"), (raw,))?
                    } else if is_mapping(refs, raw)? {
                        raw.clone()
                    } else {
                        py.None().into_bound(py)
                    };
                    let body = match (mode, level, cfg) {
                        (Mode::Object, Some(level), Some(cfg)) => {
                            Some(ObjectBody { lvl: Lvl::Fields(level), cfg, single: None, logical_au: None })
                        }
                        _ => None,
                    };
                    self.validate_with_object(
                        f,
                        body,
                        Some(&field),
                        &scrumb,
                        &[
                            (intern!(py, "schema"), resolved),
                            (intern!(py, "allow_unknown"), allow_unknown),
                            (intern!(py, "require_all"), require_all),
                        ],
                        value,
                        f.update,
                        "schema (mapping)",
                    )
                }
            };
            match outcome {
                Ok(Some(errors)) => self.error(
                    f,
                    key,
                    &errs.mapping_schema,
                    Some(rules),
                    Value::Given(value),
                    PyTuple::new(py, [errors])?,
                )?,
                Ok(None) => {}
                Err(error) if self.is_schema_rule_type_error(&error) => {
                    self.error(
                        f,
                        key,
                        &errs.bad_type_for_schema,
                        Some(rules),
                        Value::Given(value),
                        PyTuple::empty(py),
                    )?;
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// `field_rules.get('allow_unknown', self.allow_unknown)` with its compiled form.
    fn field_allow_unknown(&self, f: &Frame<'_, 'py>, rules: &RuleSet) -> (Bound<'py, PyAny>, Option<Arc<RuleSet>>) {
        let py = self.py;
        if let Some(sub) = &rules.allow_unknown {
            return (sub.raw.bind(py).clone(), sub.rules.clone());
        }
        if rules.inherit_allow_unknown
            && let Some((value, compiled)) = &f.logical_au
        {
            return (value.clone(), compiled.clone());
        }
        (f.cfg.allow_unknown.clone(), f.cfg.au_rules.clone())
    }

    /// `_validate_keysrules`
    fn validate_keysrules(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        if !is_mapping(self.refs, value)? {
            return Ok(());
        }
        let sub = rules.keysrules.as_ref().expect("keysrules rule");
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("keysrules")];
        let key_rules = sub.rules.as_ref().filter(|r| r.unsupported.is_none());
        let mode = self.mode(
            f,
            key_rules.is_some(),
            f.cfg.au_hooks || key_rules.is_some_and(|r| r.hooks),
            is_exact_dict(value),
            true,
        );
        let errors = match (mode, key_rules) {
            (Mode::Native, Some(key_rules)) => {
                let pairs = dict_snapshot(value.cast_exact()?).into_iter().map(|(k, _)| (k.clone(), k)).collect();
                let child = self.child_frame(
                    f,
                    Doc::new(DocKind::Pairs(pairs)),
                    Lvl::Uniform(key_rules),
                    f.cfg.clone(),
                    Some(field),
                    scrumb,
                    false,
                );
                self.validate_frame(&child)?;
                self.errors_of(&child).map(Bound::into_any)
            }
            (mode, key_rules) => {
                let (schema, document) = (PyDict::new(py), PyDict::new(py));
                for k in value.call_method0(intern!(py, "keys"))?.try_iter()? {
                    schema.set_item(k?, sub.raw.bind(py))?;
                }
                for k in value.call_method0(intern!(py, "keys"))?.try_iter()? {
                    let k = k?;
                    document.set_item(&k, &k)?;
                }
                let body = match (mode, key_rules) {
                    (Mode::Object, Some(key_rules)) => Some(ObjectBody {
                        lvl: Lvl::Uniform(key_rules),
                        cfg: f.cfg.clone(),
                        single: None,
                        logical_au: None,
                    }),
                    _ => None,
                };
                self.validate_with_object(
                    f,
                    body,
                    Some(&field),
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any())],
                    document.as_any(),
                    false,
                    "keysrules",
                )?
            }
        };
        if let Some(errors) = errors {
            drop_nodes_from_errorpaths(&errors, self.dpath(f)?.len(), self.spath(f)?.len(), &[2, 4])?;
            self.error(
                f,
                key,
                &self.refs.errs.keysrules,
                Some(rules),
                Value::Given(value),
                PyTuple::new(py, [errors])?,
            )?;
        }
        Ok(())
    }

    /// `_validate_valuesrules`
    fn validate_valuesrules(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        if !is_mapping(self.refs, value)? {
            return Ok(());
        }
        let sub = rules.valuesrules.as_ref().expect("valuesrules rule");
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("valuesrules")];
        let value_rules = sub.rules.as_ref().filter(|r| r.unsupported.is_none());
        let exact = is_exact_dict(value);
        let mode =
            self.mode(f, value_rules.is_some(), f.cfg.au_hooks || value_rules.is_some_and(|r| r.hooks), exact, exact);
        let errors = match (mode, value_rules) {
            (Mode::Native, Some(value_rules)) => {
                let doc = Doc::new(DocKind::Dict(value.clone().cast_into()?));
                let child =
                    self.child_frame(f, doc, Lvl::Uniform(value_rules), f.cfg.clone(), Some(field), scrumb, f.update);
                self.validate_frame(&child)?;
                self.errors_of(&child).map(Bound::into_any)
            }
            (mode, value_rules) => {
                let schema = PyDict::new(py);
                for k in value.try_iter()? {
                    schema.set_item(k?, sub.raw.bind(py))?;
                }
                let body = match (mode, value_rules) {
                    (Mode::Object, Some(value_rules)) => Some(ObjectBody {
                        lvl: Lvl::Uniform(value_rules),
                        cfg: f.cfg.clone(),
                        single: None,
                        logical_au: None,
                    }),
                    _ => None,
                };
                self.validate_with_object(
                    f,
                    body,
                    Some(&field),
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any())],
                    value,
                    f.update,
                    "valuesrules",
                )?
            }
        };
        if let Some(errors) = errors {
            drop_nodes_from_errorpaths(&errors, self.dpath(f)?.len(), self.spath(f)?.len(), &[2])?;
            self.error(
                f,
                key,
                &self.refs.errs.valuesrules,
                Some(rules),
                Value::Given(value),
                PyTuple::new(py, [errors])?,
            )?;
        }
        Ok(())
    }

    /// `__validate_logical`
    fn validate_logical(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        operator: &'static str,
        logical: &LogicalC,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<(usize, Bound<'py, PyList>)> {
        let py = self.py;
        let refs = self.refs;
        let mut valid_counter = 0;
        let collected = refs.error_list.bind(py).call0()?.cast_into::<PyList>()?;
        let field = key.object(py);
        let true_ = pyo3::types::PyBool::new(py, true).to_owned().into_any();
        let child_cfg = f.cfg.with_allow_unknown(refs, &true_, None)?.expect("True is always usable");
        // What a definition without 'allow_unknown' gets filled in: the
        // field's own value if it has one (for a merged definition that is
        // what its evaluator filled in), else `self.allow_unknown`.
        let filled_in = match &f.logical_au {
            Some(inherited) if rules.inherit_allow_unknown => inherited.clone(),
            _ => (f.cfg.allow_unknown.clone(), f.cfg.au_rules.clone()),
        };
        for (i, definition) in logical.defs.iter().enumerate() {
            let scrumb: Crumbs<'py> =
                smallvec::smallvec![field.clone(), self.name(operator), i.into_pyobject(py)?.into_any()];
            let mode = self.mode(f, definition.merged.unsupported.is_none(), definition.merged.hooks, true, true);
            let errors = if let Mode::Native = mode {
                let mut child = self.child_frame(
                    f,
                    f.doc.clone(),
                    Lvl::Single(&definition.merged),
                    child_cfg.clone(),
                    None,
                    scrumb,
                    f.update,
                );
                child.single = Some(field.clone());
                child.logical_au = Some(filled_in.clone());
                // Only `field` has rules; every other field of the document
                // is unknown and allowed.
                self.validate_field(&child, key, value)?;
                if !child.update {
                    self.validate_required_fields(&child)?;
                }
                self.errors_of(&child).map(Bound::into_any)
            } else {
                // schema = {field: definition.copy()}
                let merged = definition.raw.bind(py).copy()?;
                for rule in [intern!(py, "allow_unknown"), intern!(py, "type")] {
                    if !merged.contains(rule)?
                        && let Some(inherited) = rules.src.bind(py).get_item(rule)?
                    {
                        merged.set_item(rule, inherited)?;
                    }
                }
                if !merged.contains(intern!(py, "allow_unknown"))? {
                    merged.set_item(intern!(py, "allow_unknown"), &filled_in.0)?;
                }
                let schema = PyDict::new(py);
                schema.set_item(&field, merged)?;
                let body = matches!(mode, Mode::Object).then(|| ObjectBody {
                    lvl: Lvl::Single(&definition.merged),
                    cfg: child_cfg.clone(),
                    single: Some(field.clone()),
                    logical_au: Some(filled_in.clone()),
                });
                self.validate_with_object(
                    f,
                    body,
                    None,
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any()), (intern!(py, "allow_unknown"), true_.clone())],
                    f.doc.as_dict(py)?.as_any(),
                    f.update,
                    "*of definition",
                )?
            };
            match errors {
                None => valid_counter += 1,
                Some(errors) => {
                    drop_nodes_from_errorpaths(&errors, self.dpath(f)?.len(), self.spath(f)?.len(), &[3])?;
                    collected.call_method1(intern!(py, "extend"), (errors,))?;
                }
            }
        }
        Ok((valid_counter, collected))
    }

    /// `_lookup_field`
    fn lookup_field(
        &self,
        f: &Frame<'_, 'py>,
        path: &Bound<'py, PyAny>,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Bound<'py, PyAny>)> {
        let py = self.py;
        let startswith = intern!(py, "startswith");
        let caret = intern!(py, "^");
        let document = || -> PyResult<Bound<'py, PyAny>> { Ok(f.doc.as_dict(py)?.clone().into_any()) };
        let mut path = path.clone();
        let mut context = if path.call_method1(startswith, (caret,))?.is_truthy()? {
            path = path.get_item(pyo3::types::PySlice::new(py, 1, isize::MAX, 1))?;
            if path.call_method1(startswith, (caret,))?.is_truthy()? { document()? } else { self.root_document.clone() }
        } else {
            document()?
        };
        let parts = path.call_method1(intern!(py, "split"), (intern!(py, "."),))?;
        for part in parts.try_iter()? {
            let part = part?;
            if !context.contains(&part)? {
                return Ok((None, py.None().into_bound(py)));
            }
            context = context.call_method1(intern!(py, "get"), (part, PyDict::new(py)))?;
        }
        Ok((Some(parts.get_item(-1)?), context))
    }

    /// `_validate_dependencies`
    fn validate_dependencies(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        constraint: &Bound<'py, PyAny>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<Flow> {
        let py = self.py;
        let refs = self.refs;
        let errs = &refs.errs;
        let mut dependencies = constraint.clone();
        if is_str(&dependencies) || !(is_iterable(refs, &dependencies)? || is_mapping(refs, &dependencies)?) {
            dependencies = PyTuple::new(py, [dependencies])?.into_any();
        }
        if is_sequence(refs, &dependencies)? {
            // __validate_dependencies_sequence
            for dependency in dependencies.try_iter()? {
                let dependency = dependency?;
                if self.lookup_field(f, &dependency)?.0.is_none() {
                    self.error(
                        f,
                        key,
                        &errs.dependencies_field,
                        Some(rules),
                        Value::Given(value),
                        PyTuple::new(py, [dependency])?,
                    )?;
                }
            }
        } else if is_mapping(refs, &dependencies)? {
            // __validate_dependencies_mapping
            let mut validated = 0usize;
            let error_info = PyDict::new(py);
            for item in dependencies.call_method0(intern!(py, "items"))?.try_iter()? {
                let (name, mut values): (Bound<'py, PyAny>, Bound<'py, PyAny>) = item?.extract()?;
                if !is_sequence(refs, &values)? || is_str(&values) {
                    values = PyList::new(py, [values])?.into_any();
                }
                let (_, wanted) = self.lookup_field(f, &name)?;
                if values.contains(&wanted)? {
                    validated += 1;
                } else {
                    error_info.set_item(name, wanted)?;
                }
            }
            if validated != dependencies.len()? {
                self.error(
                    f,
                    key,
                    &errs.dependencies_field_value,
                    Some(rules),
                    Value::Given(value),
                    PyTuple::new(py, [error_info])?,
                )?;
            }
        }

        // self.document_error_tree.fetch_node_from(self.schema_path + (field, 'dependencies')) is not None
        if let Some(log) = f.log.borrow().as_ref()
            && !log.is_empty()
        {
            let path = extend_path(self.spath(f)?, &[key.object(py), self.name("dependencies")])?;
            if log_has_node_at(log.as_any(), &path)? {
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }

    /// `self._unrequired_by_excludes`
    fn unrequired(&self, f: &Frame<'_, 'py>) -> PyResult<Bound<'py, PySet>> {
        if let Some(set) = f.unrequired.borrow().as_ref() {
            return Ok(set.clone());
        }
        let set = if let Some(obj) = &f.obj {
            obj.getattr(intern!(self.py, "_unrequired_by_excludes"))?.cast_into::<PySet>()?
        } else {
            PySet::empty(self.py)?
        };
        *f.unrequired.borrow_mut() = Some(set.clone());
        Ok(set)
    }

    /// `_validate_excludes`
    fn validate_excludes(
        &self,
        f: &Frame<'_, 'py>,
        rules: &Arc<RuleSet>,
        constraint: &Bound<'py, PyAny>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        let refs = self.refs;
        let field = key.object(py);
        let excluded_fields: Vec<Bound<'py, PyAny>> = if is_hashable(refs, constraint)? {
            vec![constraint.clone()]
        } else {
            constraint.try_iter()?.collect::<PyResult<_>>()?
        };
        // self.schema[field].get('required', self.require_all)
        let required = match &rules.required {
            Some(required) => required.bind(py).is_truthy()?,
            None => f.cfg.require_all.is_truthy()?,
        };
        if required {
            self.unrequired(f)?.add(&field)?;
        }
        for excluded in &excluded_fields {
            if self.schema_contains(f, excluded)? && required {
                self.unrequired(f)?.add(excluded)?;
            }
        }
        let document = f.doc.as_dict(py)?;
        let mut present = false;
        for excluded in &excluded_fields {
            if document.contains(excluded)? {
                present = true;
                break;
            }
        }
        if present {
            let template = intern!(py, "'{0}'");
            let mut quoted = Vec::with_capacity(excluded_fields.len());
            for excluded in &excluded_fields {
                quoted.push(template.call_method1(intern!(py, "format"), (excluded,))?);
            }
            let exclusion = intern!(py, ", ").call_method1(intern!(py, "join"), (quoted,))?;
            self.error(
                f,
                key,
                &refs.errs.excludes_field,
                Some(rules),
                Value::Given(value),
                PyTuple::new(py, [exclusion])?,
            )?;
        }
        Ok(())
    }

    /// `__validate_required_fields`
    fn validate_required_fields(&self, f: &Frame<'_, 'py>) -> PyResult<()> {
        let py = self.py;
        let unrequired = f.unrequired.borrow().clone();
        let unrequired = match unrequired {
            Some(set) => Some(set),
            // the root's set can have been filled by anybody
            None if f.obj.is_some() => Some(self.unrequired(f)?),
            None => None,
        };
        let has_unrequired = unrequired.as_ref().is_some_and(|set| !set.is_empty());
        let is_required = |rules: &RuleSet| match &rules.required {
            Some(required) => required.bind(py).as_ptr() == unsafe { ffi::Py_True() },
            None => f.cfg.require_all.as_ptr() == unsafe { ffi::Py_True() },
        };

        if !has_unrequired {
            // fast path: nothing is missing in the overwhelming majority of calls
            let anything_missing = match f.lvl {
                Lvl::Fields(level) => {
                    let mut missing = false;
                    if level.any_required || f.cfg.require_all.as_ptr() == unsafe { ffi::Py_True() } {
                        let document = f.doc.as_dict(py)?;
                        for (field, rules) in &level.fields {
                            if is_required(rules) {
                                match document.get_item(field.bind(py))? {
                                    Some(value) if !(f.cfg.ignore_none_values && value.is_none()) => {}
                                    _ => {
                                        missing = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    missing
                }
                // every field of the schema is in the document; it only counts
                // as missing if None values are ignored
                Lvl::Uniform(_) | Lvl::Indexed(_) => f.cfg.ignore_none_values,
                Lvl::Single(rules) => {
                    is_required(rules)
                        && match self.document_get(f, f.single.as_ref().expect("single field"))? {
                            Some(value) => f.cfg.ignore_none_values && value.is_none(),
                            None => true,
                        }
                }
            };
            if !anything_missing {
                return Ok(());
            }
        }

        // The reference algorithm, with real sets: the iteration order of
        // `missing` decides the order of submission.
        let items = self.schema_items(f)?;
        let required = PySet::empty(py)?;
        for (field, rules) in &items {
            if is_required(rules) {
                required.add(field)?;
            }
        }
        let required = match &unrequired {
            Some(unrequired) => required.as_any().call_method1(intern!(py, "__isub__"), (unrequired,))?,
            None => required.into_any(),
        };
        let document = f.doc.as_dict(py)?;
        let present = PySet::empty(py)?;
        for (field, value) in dict_snapshot(document) {
            if !value.is_none() || !f.cfg.ignore_none_values {
                present.add(field)?;
            }
        }
        let missing = required.sub(present)?;
        let rules_of = |field: &Bound<'py, PyAny>| -> Option<&RuleSet> {
            items.iter().find(|(candidate, _)| candidate.as_ptr() == field.as_ptr()).map(|(_, rules)| rules.as_ref())
        };
        for field in missing.try_iter()? {
            let field = field?;
            self.error(
                f,
                Key::Obj(&field),
                &self.refs.errs.required_field,
                rules_of(&field),
                Value::Lookup,
                PyTuple::empty(py),
            )?;
        }

        // At least one field from self._unrequired_by_excludes should be present.
        if let Some(unrequired) = unrequired
            && !unrequired.is_empty()
        {
            let fields = PySet::empty(py)?;
            for (field, value) in dict_snapshot(document) {
                if !value.is_none() {
                    fields.add(field)?;
                }
            }
            if unrequired.call_method1(intern!(py, "isdisjoint"), (&fields,))?.is_truthy()? {
                for field in unrequired.as_any().sub(&fields)?.try_iter()? {
                    let field = field?;
                    let rules = match rules_of(&field) {
                        Some(rules) => Some(rules),
                        None => self.rules_of(f, Key::Obj(&field))?.map(|rules| rules.as_ref()),
                    };
                    self.error(
                        f,
                        Key::Obj(&field),
                        &self.refs.errs.required_field,
                        rules,
                        Value::Lookup,
                        PyTuple::empty(py),
                    )?;
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // normalization
    // -----------------------------------------------------------------------

    /// `__normalize_mapping`; the frame's document is a real dict.
    fn normalize_mapping(&self, f: &Frame<'_, 'py>, level: &Level) -> PyResult<()> {
        let py = self.py;
        let DocKind::Dict(mapping) = &f.doc.kind else {
            unreachable!("normalization frames hold real dicts");
        };
        self.normalize_rename_fields(f, mapping, level)?;
        if f.cfg.purge_unknown_truthy()? && !f.cfg.au_truthy {
            // _normalize_purge_unknown
            for (field, _) in dict_snapshot(mapping) {
                if !level.index.bind(py).contains(&field)? {
                    mapping.call_method1(intern!(py, "pop"), (field,))?;
                }
            }
        }
        if f.cfg.purge_readonly && level.any_readonly {
            // __normalize_purge_readonly
            for (field, _) in dict_snapshot(mapping) {
                if let Some(rules) = level.get(&field)?
                    && let Some(readonly) = &rules.readonly
                    && readonly.bind(py).is_truthy()?
                {
                    mapping.call_method1(intern!(py, "pop"), (field,))?;
                }
            }
        }
        if level.any_readonly {
            // __validate_readonly_fields
            for (field, rules) in &level.fields {
                let field = field.bind(py);
                if let Some(readonly) = &rules.readonly
                    && let Some(value) = mapping.get_item(field)?
                    && readonly.bind(py).is_truthy()?
                {
                    // _validate_readonly
                    if !f.is_normalized.get() {
                        self.error(
                            f,
                            Key::Obj(field),
                            &self.refs.errs.readonly_field,
                            Some(rules),
                            Value::Given(&value),
                            PyTuple::empty(py),
                        )?;
                    }
                }
            }
        }
        if level.any_default {
            self.normalize_default_fields(f, mapping, level)?;
        }
        self.normalize_coerce(f, mapping, level)?;
        self.normalize_containers(f, mapping, level)?;
        f.is_normalized.set(true);
        if let Some(config) = &f.config {
            config.set_item(intern!(py, "_is_normalized"), true)?;
        }
        Ok(())
    }

    /// `__normalize_rename_fields`
    fn normalize_rename_fields(&self, f: &Frame<'_, 'py>, mapping: &Bound<'py, PyDict>, level: &Level) -> PyResult<()> {
        let py = self.py;
        let au_handler =
            f.cfg.au_rules.as_ref().filter(|_| f.cfg.au_is_mapping).and_then(|rules| rules.rename_handler.as_ref());
        if !level.any_rename && au_handler.is_none() {
            return Ok(());
        }
        let move_item = |from: &Bound<'py, PyAny>, to: &Bound<'py, PyAny>| -> PyResult<()> {
            // mapping[to] = mapping[from]; del mapping[from]
            let Some(value) = mapping.get_item(from)? else {
                return Err(PyKeyError::new_err((from.clone().unbind(),)));
            };
            mapping.set_item(to, value)?;
            mapping.del_item(from)
        };
        for (field, _) in dict_snapshot(mapping) {
            if let Some(rules) = level.get(&field)? {
                // _normalize_rename
                if let Some(new_name) = &rules.rename {
                    move_item(&field, new_name.bind(py))?;
                }
                // _normalize_rename_handler
                if let Some(handler) = &rules.rename_handler {
                    let new_name = self.run_processor(
                        f,
                        handler,
                        Key::Obj(&field),
                        field.clone(),
                        false,
                        &self.refs.errs.renaming_failed,
                        Some(rules),
                        Value::Lookup,
                    )?;
                    if new_name.ne(&field)? {
                        move_item(&field, &new_name)?;
                    }
                }
            } else if let Some(handler) = au_handler {
                let new_name = self.run_processor(
                    f,
                    handler,
                    Key::Obj(&field),
                    field.clone(),
                    false,
                    &self.refs.errs.renaming_failed,
                    None,
                    Value::Lookup,
                )?;
                if new_name.ne(&field)? {
                    move_item(&field, &new_name)?;
                }
            }
        }
        Ok(())
    }

    /// `__normalize_coerce`
    ///
    /// `document_value` is what `self.document.get(field)` yields while the
    /// processors run (the field is only assigned afterwards).
    #[allow(clippy::too_many_arguments)]
    fn run_processor(
        &self,
        f: &Frame<'_, 'py>,
        processor: &Processor,
        key: Key<'_, 'py>,
        value: Bound<'py, PyAny>,
        nullable: bool,
        def: &ErrDef,
        rules: Option<&RuleSet>,
        document_value: Value<'_, 'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        match processor {
            Processor::Chain(chain) => {
                let mut result = value;
                for processor in chain {
                    result = self.run_processor(f, processor, key, result, nullable, def, rules, document_value)?;
                    if self.has_error_at(f, key, &self.refs.errs.coercion_failed)? {
                        break;
                    }
                }
                Ok(result)
            }
            Processor::Callable(_) | Processor::Named(_) => {
                let callable = match processor {
                    Processor::Callable(callable) => callable.bind(py).clone(),
                    // processor = self.__get_rule_handler('normalize_coerce', processor)
                    Processor::Named(handler) => handler.resolve(self.obj(f)?)?,
                    Processor::Chain(_) => unreachable!(),
                };
                let outcome = callable.call1((&value,));
                if matches!(processor, Processor::Named(_)) {
                    self.after_hook(f);
                }
                match outcome {
                    Ok(result) => Ok(result),
                    Err(error) if self.is_exception(&error) => {
                        if !(nullable && value.is_none()) {
                            let message = error.value(py).str()?;
                            self.error(f, key, def, rules, document_value, PyTuple::new(py, [message])?)?;
                        }
                        Ok(value)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// `__normalize_default_fields`
    fn normalize_default_fields(
        &self,
        f: &Frame<'_, 'py>,
        mapping: &Bound<'py, PyDict>,
        level: &Level,
    ) -> PyResult<()> {
        let py = self.py;
        let errs = &self.refs.errs;
        // empty_fields, restricted to those that have a default (setter)
        let mut empty_fields: Vec<(&Bound<'py, PyAny>, &Arc<RuleSet>)> = Vec::new();
        for (field, rules) in &level.fields {
            if rules.default.is_none() && rules.default_setter.is_none() {
                continue;
            }
            let field = field.bind(py);
            let empty = match mapping.get_item(field)? {
                None => true,
                Some(value) => {
                    value.is_none()
                        && !match &rules.nullable {
                            Some(nullable) => nullable.bind(py).is_truthy()?,
                            None => false,
                        }
                }
            };
            if empty {
                empty_fields.push((field, rules));
            }
        }
        for (field, rules) in &empty_fields {
            // _normalize_default
            if let Some(default) = &rules.default {
                mapping.set_item(field, default.bind(py))?;
            }
        }

        let mut pending: std::collections::VecDeque<(&Bound<'py, PyAny>, &Arc<RuleSet>)> =
            empty_fields.iter().filter(|(_, rules)| rules.default_setter.is_some()).copied().collect();
        let mut known_states: HashSet<isize> = HashSet::new();
        while let Some((field, rules)) = pending.pop_front() {
            // _normalize_default_setter
            let outcome = match rules.default_setter.as_ref().expect("has setter") {
                Setter::Callable(setter) => Ok(setter.bind(py).clone()),
                // setter = self.__get_rule_handler('normalize_default_setter', setter)
                Setter::Named(handler) => handler.resolve(self.obj(f)?),
            }
            .and_then(|setter| setter.call1((mapping,)))
            .and_then(|value| mapping.set_item(field, value));
            if matches!(rules.default_setter, Some(Setter::Named(_))) {
                self.after_hook(f);
            }
            match outcome {
                Ok(()) => {}
                Err(error) if error.is_instance_of::<PyKeyError>(py) => pending.push_back((field, rules)),
                Err(error) if self.is_exception(&error) => {
                    let message = error.value(py).str()?;
                    self.error(
                        f,
                        Key::Obj(field),
                        &errs.setting_default_failed,
                        Some(rules),
                        Value::Lookup,
                        PyTuple::new(py, [message])?,
                    )?;
                }
                Err(error) => return Err(error),
            }
            let state = PyTuple::new(py, pending.iter().map(|(field, _)| *field))?.hash()?;
            if known_states.contains(&state) {
                for (field, rules) in &pending {
                    self.error(
                        f,
                        Key::Obj(field),
                        &errs.setting_default_failed,
                        Some(rules),
                        Value::Lookup,
                        PyTuple::new(py, [intern!(py, "Circular dependencies of default setters.")])?,
                    )?;
                }
                break;
            }
            known_states.insert(state);
        }
        Ok(())
    }

    /// `_normalize_coerce`
    fn normalize_coerce(&self, f: &Frame<'_, 'py>, mapping: &Bound<'py, PyDict>, level: &Level) -> PyResult<()> {
        let py = self.py;
        let au = f.cfg.au_rules.as_ref().filter(|rules| f.cfg.au_is_mapping && rules.coerce.is_some());
        if !level.any_coerce && au.is_none() {
            return Ok(());
        }
        let truthy = |value: &Option<Py<PyAny>>| -> PyResult<bool> {
            match value {
                Some(value) => value.bind(py).is_truthy(),
                None => Ok(false),
            }
        };
        // `for field in mapping:` while arbitrary callables run
        for field in mapping.as_any().try_iter()? {
            let field = field?;
            let rules = level.get(&field)?;
            let (processor, nullable, error_rules) = match (rules, au) {
                (Some(rules), _) if rules.coerce.is_some() => {
                    (rules.coerce.as_ref().expect("coerce"), truthy(&rules.nullable)?, Some(rules.as_ref()))
                }
                (rules, Some(au)) => {
                    (au.coerce.as_ref().expect("coerce"), truthy(&au.nullable)?, rules.map(|rules| rules.as_ref()))
                }
                _ => continue,
            };
            let Some(value) = mapping.get_item(&field)? else {
                return Err(PyKeyError::new_err((field.unbind(),)));
            };
            let coerced = self.run_processor(
                f,
                processor,
                Key::Obj(&field),
                value,
                nullable,
                &self.refs.errs.coercion_failed,
                error_rules,
                Value::Lookup,
            )?;
            mapping.set_item(&field, coerced)?;
        }
        Ok(())
    }

    /// `__normalize_containers`
    fn normalize_containers(&self, f: &Frame<'_, 'py>, mapping: &Bound<'py, PyDict>, level: &Level) -> PyResult<()> {
        if level.all_inert && !f.cfg.au_is_mapping {
            return Ok(());
        }
        for (field, _) in dict_snapshot(mapping) {
            let rules = level.get(&field)?;
            if rules.is_some_and(|rules| rules.inert) && !f.cfg.au_is_mapping {
                continue;
            }
            if rules.is_none() && !f.cfg.au_is_mapping {
                continue;
            }
            let Some(value) = mapping.get_item(&field)? else {
                return Err(PyKeyError::new_err((field.unbind(),)));
            };
            // mapping[field] = ..., assigned before the child's errors are filed
            self.normalize_container(f, Key::Obj(&field), rules, value, &mut |replacement| {
                mapping.set_item(&field, replacement)
            })?;
        }
        Ok(())
    }

    /// One iteration of the loop in `__normalize_containers`: `store` assigns
    /// a new value to the field.
    fn normalize_container(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        rules: Option<&Arc<RuleSet>>,
        value: Bound<'py, PyAny>,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<()> {
        let refs = self.refs;
        let exact = exact_of(&value);
        if matches!(exact, Exact::None | Exact::Bool | Exact::Int | Exact::Float | Exact::Str | Exact::Bytes) {
            // mappings: no; strings are skipped; bytes are sequences, see below
            if exact != Exact::Bytes {
                return Ok(());
            }
        }
        if is_mapping(refs, &value)? {
            let mut current = value;
            if let Some(rules) = rules {
                if let Some(sub) = &rules.keysrules {
                    self.normalize_mapping_per_keysrules(f, key, &current, sub)?;
                }
                if let Some(sub) = &rules.valuesrules {
                    current = self.normalize_mapping_per_valuesrules(f, key, &current, sub, store)?;
                }
            }
            let has_schema_rules = rules.is_some_and(|rules| {
                rules.allow_unknown.is_some() || rules.purge_unknown.is_some() || rules.schema.is_some()
            });
            if has_schema_rules || f.cfg.au_is_mapping {
                match self.normalize_mapping_per_schema(f, key, &current, rules, store) {
                    Ok(_) => {}
                    Err(error) if self.is_schema_rule_type_error(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(());
        }
        if is_str(&value) {
            return Ok(());
        }
        if is_sequence(refs, &value)?
            && let Some(rules) = rules
        {
            if rules.schema.is_some() {
                return self.normalize_sequence_per_schema(f, key, &value, rules, store).map(drop);
            } else if let Some(items) = &rules.items {
                return self.normalize_sequence_per_items(f, key, &value, items, store).map(drop);
            }
        }
        Ok(())
    }

    /// Finishes a normalization child: `self._error(validator._errors)` after
    /// dropping schema path nodes.
    fn adopt_child_errors(
        &self,
        f: &Frame<'_, 'py>,
        errors: Option<Bound<'py, PyAny>>,
        drop: &[usize],
    ) -> PyResult<()> {
        if let Some(errors) = errors {
            if !drop.is_empty() {
                drop_nodes_from_errorpaths(&errors, self.dpath(f)?.len(), self.spath(f)?.len(), drop)?;
            }
            let errors: Vec<_> = errors.try_iter()?.collect::<PyResult<_>>()?;
            self.submit(f, &errors)?;
        }
        Ok(())
    }

    /// `__normalize_mapping_per_keysrules`
    fn normalize_mapping_per_keysrules(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        sub: &Sub,
    ) -> PyResult<()> {
        let py = self.py;
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("keysrules")];
        let key_rules = sub.rules.as_ref().filter(|r| r.unsupported.is_none());
        let mode = self.mode(
            f,
            key_rules.is_some(),
            f.cfg.au_hooks || key_rules.is_some_and(|r| r.hooks),
            is_exact_dict(value),
            true,
        );
        if let (Mode::Native, Some(key_rules)) = (&mode, key_rules)
            && key_rules.inert
            && !f.cfg.au_is_mapping
        {
            // normalizing {key: key} cannot change or report anything
            return Ok(());
        }
        // document = dict((k, k) for k in mapping[field])
        let keys: Vec<Bound<'py, PyAny>> = value.try_iter()?.collect::<PyResult<_>>()?;
        let document = PyDict::new(py);
        for k in &keys {
            document.set_item(k, k)?;
        }
        let (result, errors) = match (mode, key_rules) {
            (Mode::Native, Some(key_rules)) => {
                let level = Level::uniform(py, keys.into_iter(), key_rules)?;
                let child = self.child_frame(
                    f,
                    Doc::new(DocKind::Dict(document.clone())),
                    Lvl::Fields(&level),
                    f.cfg.clone(),
                    Some(field.clone()),
                    scrumb,
                    false,
                );
                self.normalize_mapping(&child, &level)?;
                (document.into_any(), self.errors_of(&child).map(Bound::into_any))
            }
            (mode, key_rules) => {
                let schema = PyDict::new(py);
                for k in &keys {
                    schema.set_item(k, sub.raw.bind(py))?;
                }
                let level = match (mode, key_rules) {
                    (Mode::Object, Some(key_rules)) => Some(Level::uniform(py, keys.into_iter(), key_rules)?),
                    _ => None,
                };
                self.normalize_with_object(
                    f,
                    level.as_ref().map(|level| (level, f.cfg.clone())),
                    Some(&field),
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any())],
                    document.as_any(),
                    "keysrules (normalization)",
                )?
            }
        };
        self.adopt_child_errors(f, errors, &[2, 4])?;
        for k in result.try_iter()? {
            let k = k?;
            let new_key = result.get_item(&k)?;
            if k.eq(&new_key)? {
                continue;
            }
            if value.contains(&new_key)? {
                let path = extend_path(self.dpath(f)?, std::slice::from_ref(&field))?;
                let mut parts = Vec::with_capacity(path.len());
                for part in path.iter() {
                    parts.push(part.str()?.extract::<PyBackedStr>()?.to_string());
                }
                let message = format!(
                    "Normalizing keys of {}: {} already exists, its value is replaced.",
                    parts.join("."),
                    k.str()?.extract::<PyBackedStr>()?
                );
                self.refs.warn.bind(py).call1((message,))?;
                value.set_item(&new_key, value.get_item(&k)?)?;
            } else {
                value.set_item(&new_key, value.get_item(&k)?)?;
                value.del_item(&k)?;
            }
        }
        Ok(())
    }

    /// `__normalize_mapping_per_valuesrules`; returns the replacement mapping.
    fn normalize_mapping_per_valuesrules(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        sub: &Sub,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("valuesrules")];
        let value_rules = sub.rules.as_ref().filter(|r| r.unsupported.is_none());
        let exact = is_exact_dict(value);
        let mode =
            self.mode(f, value_rules.is_some(), f.cfg.au_hooks || value_rules.is_some_and(|r| r.hooks), exact, exact);
        let (result, errors) = match (mode, value_rules) {
            (Mode::Native, Some(value_rules)) => {
                // the child works on `copy(document)`
                let document = value.cast_exact::<PyDict>()?.copy()?;
                if value_rules.inert && !f.cfg.au_is_mapping {
                    (document.into_any(), None)
                } else {
                    let keys = dict_snapshot(&document).into_iter().map(|(k, _)| k);
                    let level = Level::uniform(py, keys, value_rules)?;
                    let child = self.child_frame(
                        f,
                        Doc::new(DocKind::Dict(document.clone())),
                        Lvl::Fields(&level),
                        f.cfg.clone(),
                        Some(field),
                        scrumb,
                        false,
                    );
                    self.normalize_mapping(&child, &level)?;
                    (document.into_any(), self.errors_of(&child).map(Bound::into_any))
                }
            }
            (mode, value_rules) => {
                let keys: Vec<Bound<'py, PyAny>> = value.try_iter()?.collect::<PyResult<_>>()?;
                let schema = PyDict::new(py);
                for k in &keys {
                    schema.set_item(k, sub.raw.bind(py))?;
                }
                let level = match (mode, value_rules) {
                    (Mode::Object, Some(value_rules)) => Some(Level::uniform(py, keys.into_iter(), value_rules)?),
                    _ => None,
                };
                self.normalize_with_object(
                    f,
                    level.as_ref().map(|level| (level, f.cfg.clone())),
                    Some(&field),
                    &scrumb,
                    &[(intern!(py, "schema"), schema.into_any())],
                    value,
                    "valuesrules (normalization)",
                )?
            }
        };
        store(&result)?;
        self.adopt_child_errors(f, errors, &[2])?;
        Ok(result)
    }

    /// `__normalize_mapping_per_schema`; returns the replacement mapping.
    fn normalize_mapping_per_schema(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        rules: Option<&Arc<RuleSet>>,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let refs = self.refs;
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name("schema")];
        // rules = schema.get(field, {})
        // if not rules and isinstance(self.allow_unknown, Mapping): rules = self.allow_unknown
        let own_is_empty = match rules {
            Some(rules) => rules.src.bind(py).is_empty(),
            None => true,
        };
        let effective: Option<&Arc<RuleSet>> =
            if own_is_empty && f.cfg.au_is_mapping { f.cfg.au_rules.as_ref() } else { rules };

        let (allow_unknown, au_rules) = match effective {
            Some(rules) => self.field_allow_unknown(f, rules),
            None => (f.cfg.allow_unknown.clone(), f.cfg.au_rules.clone()),
        };
        let purge_unknown = match effective.and_then(|rules| rules.purge_unknown.as_ref()) {
            Some(value) => value.bind(py).clone(),
            None => match &f.cfg.purge_unknown {
                Some(value) => value.clone(),
                None => pyo3::types::PyBool::new(py, false).to_owned().into_any(),
            },
        };
        let require_all = match effective.and_then(|rules| rules.require_all.as_ref()) {
            Some(value) => value.bind(py).clone(),
            None => f.cfg.require_all.clone(),
        };
        let schema = effective.and_then(|rules| rules.schema.as_ref());

        let empty_level;
        let level: Option<&Level> = match schema {
            Some(schema) => schema.level.as_deref().filter(|level| level.unsupported.is_none()),
            None => {
                empty_level = Level::from_fields(py, PyDict::new(py), Vec::new());
                Some(&empty_level)
            }
        };
        let cfg = f.cfg.with_allow_unknown(refs, &allow_unknown, au_rules.as_ref())?.map(|mut cfg| {
            cfg.purge_unknown = Some(purge_unknown.clone());
            cfg.require_all = require_all.clone();
            cfg
        });
        let exact = is_exact_dict(value);
        let mode = match (&level, &cfg) {
            (Some(level), Some(cfg)) => self.mode(f, true, level.hooks || cfg.au_hooks, exact, exact),
            _ => Mode::Python,
        };
        if let (Mode::Native, Some(level), Some(cfg)) = (&mode, level, &cfg) {
            // `copy(document)` in the child, `value_type(result)` here: one
            // fresh dict is indistinguishable from two.
            let document = value.cast_exact::<PyDict>()?.copy()?;
            let child = self.child_frame(
                f,
                Doc::new(DocKind::Dict(document.clone())),
                Lvl::Fields(level),
                cfg.clone(),
                Some(field),
                scrumb,
                false,
            );
            self.normalize_mapping(&child, level)?;
            store(document.as_any())?;
            self.adopt_child_errors(f, self.errors_of(&child).map(Bound::into_any), &[])?;
            return Ok(document.into_any());
        }

        let child_schema = match schema {
            Some(schema) => schema.raw.bind(py).clone(),
            None => PyDict::new(py).into_any(),
        };
        let body = match (mode, level, cfg) {
            (Mode::Object, Some(level), Some(cfg)) => Some((level, cfg)),
            _ => None,
        };
        let value_type = value.get_type();
        let (result, errors) = self.normalize_with_object(
            f,
            body,
            Some(&field),
            &scrumb,
            &[
                (intern!(py, "schema"), child_schema),
                (intern!(py, "allow_unknown"), allow_unknown),
                (intern!(py, "purge_unknown"), purge_unknown),
                (intern!(py, "require_all"), require_all),
            ],
            value,
            "schema (mapping normalization)",
        )?;
        let result = value_type.call1((result,))?;
        store(&result)?;
        self.adopt_child_errors(f, errors, &[])?;
        Ok(result)
    }

    /// `__normalize_sequence_per_schema`; returns the replacement sequence.
    fn normalize_sequence_per_schema(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        rules: &Arc<RuleSet>,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let schema = rules.schema.as_ref().expect("schema rule");
        let count = value.len()?;
        let member_rules: Option<Vec<&Arc<RuleSet>>> = schema.rules.as_ref().map(|rules| vec![rules; count]);
        // schema = dict((k, schema[field]['schema']) for k in range(len(mapping[field])))
        let child_schema = || -> PyResult<Bound<'py, PyDict>> {
            let child_schema = PyDict::new(py);
            for i in 0..count {
                child_schema.set_item(i, schema.raw.bind(py))?;
            }
            Ok(child_schema)
        };
        self.normalize_sequence(
            f,
            key,
            value,
            "schema",
            member_rules,
            child_schema,
            "schema (sequence normalization)",
            store,
        )
    }

    /// `__normalize_sequence_per_items`
    fn normalize_sequence_per_items(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        items: &ItemsC,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = self.py;
        if items.rules.len() != value.len()? {
            return Ok(None);
        }
        let member_rules: Option<Vec<&Arc<RuleSet>>> = items.rules.iter().map(Option::as_ref).collect();
        // schema = dict((k, v) for k, v in enumerate(rules))
        let child_schema = || -> PyResult<Bound<'py, PyDict>> {
            let child_schema = PyDict::new(py);
            for (i, definition) in items.raw.bind(py).try_iter()?.enumerate() {
                child_schema.set_item(i, definition?)?;
            }
            Ok(child_schema)
        };
        self.normalize_sequence(f, key, value, "items", member_rules, child_schema, "items (normalization)", store)
            .map(Some)
    }

    /// Normalizes the members of a sequence as the document `{i: member}`
    /// with the schema `{i: member_rules[i]}` and returns
    /// `value_type(result.values())`.
    #[allow(clippy::too_many_arguments)]
    fn normalize_sequence(
        &self,
        f: &Frame<'_, 'py>,
        key: Key<'_, 'py>,
        value: &Bound<'py, PyAny>,
        rule_name: &'static str,
        member_rules: Option<Vec<&Arc<RuleSet>>>,
        child_schema: impl Fn() -> PyResult<Bound<'py, PyDict>>,
        reason: &'static str,
        store: &mut dyn FnMut(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let field = key.object(py);
        let scrumb: Crumbs<'py> = smallvec::smallvec![field.clone(), self.name(rule_name)];
        let exact = exact_of(value);
        let member_rules = member_rules.filter(|all| all.iter().all(|rules| rules.unsupported.is_none()));
        let hooks = f.cfg.au_hooks || member_rules.as_ref().is_some_and(|all| all.iter().any(|rules| rules.hooks));
        let mode = self.mode(f, member_rules.is_some(), hooks, matches!(exact, Exact::List | Exact::Tuple), true);

        if let (Mode::Native, Some(member_rules)) = (&mode, &member_rules) {
            let items: Vec<Bound<'py, PyAny>> = value.try_iter()?.collect::<PyResult<_>>()?;
            let (result, errors) = self.normalize_members(f, field, scrumb, items, member_rules)?;
            let result = rebuild_sequence(py, exact, result)?;
            store(&result)?;
            self.adopt_child_errors(f, errors, &[2])?;
            return Ok(result);
        }

        let document = PyDict::new(py);
        for (i, item) in value.try_iter()?.enumerate() {
            document.set_item(i, item?)?;
        }
        let level = match (mode, &member_rules) {
            (Mode::Object, Some(member_rules)) => Some(members_level(py, member_rules)?),
            _ => None,
        };
        let value_type = value.get_type();
        let (result, errors) = self.normalize_with_object(
            f,
            level.as_ref().map(|level| (level, f.cfg.clone())),
            Some(&field),
            &scrumb,
            &[(intern!(py, "schema"), child_schema()?.into_any())],
            document.as_any(),
            reason,
        )?;
        let result = value_type.call1((result.call_method0(intern!(py, "values"))?,))?;
        store(&result)?;
        self.adopt_child_errors(f, errors, &[2])?;
        Ok(result)
    }

    /// The native part of [`Self::normalize_sequence`]: returns the resulting
    /// members and the child's errors.
    fn normalize_members(
        &self,
        f: &Frame<'_, 'py>,
        field: Bound<'py, PyAny>,
        scrumb: Crumbs<'py>,
        mut items: Vec<Bound<'py, PyAny>>,
        member_rules: &[&Arc<RuleSet>],
    ) -> PyResult<Members<'py>> {
        let py = self.py;
        let simple = member_rules.iter().all(|rules| {
            rules.rename.is_none()
                && rules.rename_handler.is_none()
                && rules.readonly.is_none()
                && rules.default.is_none()
                && rules.default_setter.is_none()
        });
        if !simple {
            // generic path on the real `{i: member}` dict
            let document = PyDict::new(py);
            for (i, item) in items.iter().enumerate() {
                document.set_item(i, item)?;
            }
            let level = members_level(py, member_rules)?;
            let child = self.child_frame(
                f,
                Doc::new(DocKind::Dict(document.clone())),
                Lvl::Fields(&level),
                f.cfg.clone(),
                Some(field),
                scrumb,
                false,
            );
            self.normalize_mapping(&child, &level)?;
            let result = dict_snapshot(&document).into_iter().map(|(_, value)| value).collect();
            return Ok((result, self.errors_of(&child).map(Bound::into_any)));
        }

        // Without rename, readonly and default rules the document keeps its
        // keys, so the members can stay in a vector.
        let inert = member_rules.iter().all(|rules| rules.inert) && !f.cfg.au_is_mapping;
        if inert {
            return Ok((items, None));
        }
        // the frame's document is only consulted for `self.document.get(field)`
        // of errors, which are given their value explicitly below
        let child = self.child_frame(
            f,
            Doc::new(DocKind::Seq(Vec::new())),
            Lvl::Indexed(&[]),
            f.cfg.clone(),
            Some(field),
            scrumb,
            false,
        );
        let au = f.cfg.au_rules.as_ref().filter(|rules| f.cfg.au_is_mapping && rules.coerce.is_some());
        let truthy = |value: &Option<Py<PyAny>>| -> PyResult<bool> {
            match value {
                Some(value) => value.bind(py).is_truthy(),
                None => Ok(false),
            }
        };
        // _normalize_coerce
        for (i, item) in items.iter_mut().enumerate() {
            let rules = member_rules[i];
            let (processor, nullable) = match (&rules.coerce, au) {
                (Some(processor), _) => (processor, truthy(&rules.nullable)?),
                (None, Some(au)) => (au.coerce.as_ref().expect("coerce"), truthy(&au.nullable)?),
                (None, None) => continue,
            };
            let original = item.clone();
            *item = self.run_processor(
                &child,
                processor,
                Key::Index(i),
                original.clone(),
                nullable,
                &self.refs.errs.coercion_failed,
                Some(rules),
                Value::Given(&original),
            )?;
        }
        // __normalize_containers
        for i in 0..items.len() {
            let rules = member_rules[i];
            if rules.inert && !f.cfg.au_is_mapping {
                continue;
            }
            let item = items[i].clone();
            self.normalize_container(&child, Key::Index(i), Some(rules), item, &mut |replacement| {
                items[i] = replacement.clone();
                Ok(())
            })?;
        }
        Ok((items, self.errors_of(&child).map(Bound::into_any)))
    }
}

/// Normalized members of a sequence and the errors of their child validator.
type Members<'py> = (Vec<Bound<'py, PyAny>>, Option<Bound<'py, PyAny>>);

/// How the child validator of a rule is run.
enum Mode {
    /// A frame without validator object.
    Native,
    /// A real child validator whose document is processed natively.
    Object,
    /// A real child validator run by the reference implementation.
    Python,
}

/// What the native body of a child validator needs besides the validator.
struct ObjectBody<'a, 'py> {
    lvl: Lvl<'a>,
    cfg: Cfg<'py>,
    single: Option<Bound<'py, PyAny>>,
    logical_au: Option<(Bound<'py, PyAny>, Option<Arc<RuleSet>>)>,
}

/// The document of a frame that holds a real dict.
fn f_document<'f, 'py>(f: &'f Frame<'_, 'py>) -> &'f Bound<'py, PyDict> {
    match &f.doc.kind {
        DocKind::Dict(dict) => dict,
        _ => unreachable!("frames with validator objects hold real dicts"),
    }
}

/// The schema `{i: member_rules[i]}`.
fn members_level(py: Python<'_>, member_rules: &[&Arc<RuleSet>]) -> PyResult<Level> {
    let index = PyDict::new(py);
    let mut fields = Vec::with_capacity(member_rules.len());
    for (i, rules) in member_rules.iter().enumerate() {
        let position = i.into_pyobject(py)?.into_any();
        index.set_item(&position, i)?;
        fields.push((position.unbind(), (*rules).clone()));
    }
    Ok(Level::from_fields(py, index, fields))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// `path + tuple(items)`
fn extend_path<'py>(path: &Bound<'py, PyTuple>, items: &[Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyTuple>> {
    let py = path.py();
    let mut all = Vec::with_capacity(path.len() + items.len());
    all.extend(path.iter());
    all.extend(items.iter().cloned());
    PyTuple::new(py, all)
}

/// `value_type(result.values())` for exact lists and tuples.
fn rebuild_sequence<'py>(py: Python<'py>, exact: Exact, items: Vec<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    Ok(match exact {
        Exact::Tuple => PyTuple::new(py, items)?.into_any(),
        _ => PyList::new(py, items)?.into_any(),
    })
}

/// `try: return value <op> constraint; except TypeError: return False`
fn compare_swallowing_type_error(
    value: &Bound<'_, PyAny>,
    constraint: &Bound<'_, PyAny>,
    op: pyo3::basic::CompareOp,
) -> PyResult<bool> {
    use pyo3::basic::CompareOp;
    let py = value.py();
    match (exact_of(value), exact_of(constraint)) {
        (Exact::Int, Exact::Int) => {
            let mut overflow = 0;
            // SAFETY: both objects are exact ints.
            let a = unsafe { ffi::PyLong_AsLongLongAndOverflow(value.as_ptr(), &mut overflow) };
            if overflow == 0 {
                let b = unsafe { ffi::PyLong_AsLongLongAndOverflow(constraint.as_ptr(), &mut overflow) };
                if overflow == 0 {
                    return Ok(match op {
                        CompareOp::Lt => a < b,
                        _ => a > b,
                    });
                }
            }
        }
        (Exact::Float, Exact::Float) => {
            // SAFETY: both objects are exact floats.
            let (a, b) =
                unsafe { (ffi::PyFloat_AS_DOUBLE(value.as_ptr()), ffi::PyFloat_AS_DOUBLE(constraint.as_ptr())) };
            return Ok(match op {
                CompareOp::Lt => a < b,
                _ => a > b,
            });
        }
        _ => {}
    }
    match value.rich_compare(constraint, op).and_then(|result| result.is_truthy()) {
        Ok(result) => Ok(result),
        Err(error) if error.is_instance_of::<PyTypeError>(py) => Ok(false),
        Err(error) => Err(error),
    }
}

/// `len(value) <op> constraint`
fn length_compare(
    py: Python<'_>,
    length: usize,
    constraint: &Bound<'_, PyAny>,
    op: pyo3::basic::CompareOp,
) -> PyResult<bool> {
    use pyo3::basic::CompareOp;
    if exact_of(constraint) == Exact::Int {
        let mut overflow = 0;
        // SAFETY: the constraint is an exact int.
        let limit = unsafe { ffi::PyLong_AsLongLongAndOverflow(constraint.as_ptr(), &mut overflow) };
        if overflow == 0 {
            let length = length as i128;
            return Ok(match op {
                CompareOp::Lt => length < limit as i128,
                _ => length > limit as i128,
            });
        }
    }
    length.into_pyobject(py)?.rich_compare(constraint, op)?.is_truthy()
}
