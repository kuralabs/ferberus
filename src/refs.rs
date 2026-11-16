//! Python objects the engine needs, resolved once per interpreter.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyString;

/// An `errors.ErrorDefinition` with its parts ready for use.
pub struct ErrDef {
    pub code: u8,
    pub code_obj: Py<PyAny>,
    pub rule: Option<Py<PyString>>,
}

impl ErrDef {
    fn load(errors: &Bound<'_, PyAny>, name: &str) -> PyResult<Self> {
        let def = errors.getattr(name)?;
        let code_obj = def.getattr("code")?;
        let rule = def.getattr("rule")?;
        Ok(ErrDef {
            code: code_obj.extract()?,
            code_obj: code_obj.unbind(),
            rule: if rule.is_none() {
                None
            } else {
                Some(PyString::intern(errors.py(), rule.cast::<PyString>()?.to_str()?).unbind())
            },
        })
    }
}

macro_rules! err_defs {
    ($($field:ident => $name:literal),* $(,)?) => {
        /// The error definitions of `ferberus.errors` used by native rules.
        pub struct ErrDefs { $(pub $field: ErrDef,)* }

        impl ErrDefs {
            fn load(errors: &Bound<'_, PyAny>) -> PyResult<Self> {
                Ok(ErrDefs { $($field: ErrDef::load(errors, $name)?,)* })
            }
        }
    };
}

err_defs! {
    required_field => "REQUIRED_FIELD",
    unknown_field => "UNKNOWN_FIELD",
    dependencies_field => "DEPENDENCIES_FIELD",
    dependencies_field_value => "DEPENDENCIES_FIELD_VALUE",
    excludes_field => "EXCLUDES_FIELD",
    empty_not_allowed => "EMPTY_NOT_ALLOWED",
    not_nullable => "NOT_NULLABLE",
    bad_type => "BAD_TYPE",
    bad_type_for_schema => "BAD_TYPE_FOR_SCHEMA",
    items_length => "ITEMS_LENGTH",
    min_length => "MIN_LENGTH",
    max_length => "MAX_LENGTH",
    regex_mismatch => "REGEX_MISMATCH",
    min_value => "MIN_VALUE",
    max_value => "MAX_VALUE",
    unallowed_value => "UNALLOWED_VALUE",
    unallowed_values => "UNALLOWED_VALUES",
    forbidden_value => "FORBIDDEN_VALUE",
    forbidden_values => "FORBIDDEN_VALUES",
    missing_members => "MISSING_MEMBERS",
    coercion_failed => "COERCION_FAILED",
    renaming_failed => "RENAMING_FAILED",
    readonly_field => "READONLY_FIELD",
    setting_default_failed => "SETTING_DEFAULT_FAILED",
    mapping_schema => "MAPPING_SCHEMA",
    sequence_schema => "SEQUENCE_SCHEMA",
    keysrules => "KEYSRULES",
    valuesrules => "VALUESRULES",
    bad_items => "BAD_ITEMS",
    noneof => "NONEOF",
    oneof => "ONEOF",
    anyof => "ANYOF",
    allof => "ALLOF",
}

pub struct Refs {
    pub validation_error: Py<PyAny>,
    pub error_list: Py<PyAny>,
    pub schema_rule_type_error: Py<PyAny>,
    pub definition_schema: Py<PyAny>,
    pub basic_error_handler: Py<PyAny>,
    pub schema_error_handler: Py<PyAny>,
    pub base_error_handler: Py<PyAny>,
    pub analyze_class: Py<PyAny>,
    pub schema_copy_dance: Py<PyAny>,
    pub begin_validation: Py<PyAny>,
    pub begin_normalization: Py<PyAny>,
    pub toy_error_handler: Py<PyAny>,
    pub object_new: Py<PyAny>,
    pub type_setattr: Py<PyAny>,
    pub re_compile: Py<PyAny>,
    pub warn: Py<PyAny>,
    pub abc_meta: Py<PyAny>,
    pub mapping: Py<PyAny>,
    pub sequence: Py<PyAny>,
    pub iterable: Py<PyAny>,
    pub sized: Py<PyAny>,
    pub hashable: Py<PyAny>,
    pub abc_set: Py<PyAny>,
    pub rules_set_registry: Py<PyAny>,
    pub schema_registry: Py<PyAny>,
    pub rules_set_registry_class: Py<PyAny>,
    pub schema_registry_class: Py<PyAny>,
    pub none_type: Py<PyAny>,
    pub errs: ErrDefs,
}

static REFS: PyOnceLock<Refs> = PyOnceLock::new();

impl Refs {
    pub fn get(py: Python<'_>) -> PyResult<&'static Refs> {
        REFS.get_or_try_init(py, || Refs::load(py))
    }

    fn load(py: Python<'_>) -> PyResult<Refs> {
        let errors = py.import("ferberus.errors")?;
        let validator = py.import("ferberus.validator")?;
        let schema = py.import("ferberus.schema")?;
        let abc = py.import("collections.abc")?;
        let builtins = py.import("builtins")?;
        Ok(Refs {
            validation_error: errors.getattr("ValidationError")?.unbind(),
            error_list: errors.getattr("ErrorList")?.unbind(),
            schema_rule_type_error: validator.getattr("_SchemaRuleTypeError")?.unbind(),
            definition_schema: schema.getattr("DefinitionSchema")?.unbind(),
            basic_error_handler: errors.getattr("BasicErrorHandler")?.unbind(),
            schema_error_handler: errors.getattr("SchemaErrorHandler")?.unbind(),
            base_error_handler: errors.getattr("BaseErrorHandler")?.unbind(),
            analyze_class: validator.getattr("_analyze_class")?.unbind(),
            schema_copy_dance: validator.getattr("_schema_copy_dance")?.unbind(),
            begin_validation: validator.getattr("_begin_validation")?.unbind(),
            begin_normalization: validator.getattr("_begin_normalization")?.unbind(),
            toy_error_handler: validator.getattr("toy_error_handler")?.unbind(),
            object_new: builtins.getattr("object")?.getattr("__new__")?.unbind(),
            type_setattr: builtins.getattr("type")?.getattr("__setattr__")?.unbind(),
            re_compile: py.import("re")?.getattr("compile")?.unbind(),
            warn: py.import("warnings")?.getattr("warn")?.unbind(),
            abc_meta: py.import("abc")?.getattr("ABCMeta")?.unbind(),
            mapping: abc.getattr("Mapping")?.unbind(),
            sequence: abc.getattr("Sequence")?.unbind(),
            iterable: abc.getattr("Iterable")?.unbind(),
            sized: abc.getattr("Sized")?.unbind(),
            hashable: abc.getattr("Hashable")?.unbind(),
            abc_set: abc.getattr("Set")?.unbind(),
            // the registries `ferberus.validator` falls back to
            rules_set_registry: validator.getattr("rules_set_registry")?.unbind(),
            schema_registry: validator.getattr("schema_registry")?.unbind(),
            rules_set_registry_class: schema.getattr("RulesSetRegistry")?.unbind(),
            schema_registry_class: schema.getattr("SchemaRegistry")?.unbind(),
            none_type: py.None().bind(py).get_type().into_any().unbind(),
            errs: ErrDefs::load(errors.as_any())?,
        })
    }
}
