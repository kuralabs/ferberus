# Public surface of Cerberus 1.3.x (`83f5dad`)

Everything a user can touch, and therefore everything ferberus must
reproduce. "Observable" below means: can be seen from user code without
reading private name-mangled attributes. Quirks that look like bugs are part
of the surface and are listed under [Quirks](#quirks-that-must-be-preserved).

Modules: `cerberus` (`__init__`), `cerberus.validator`, `cerberus.schema`,
`cerberus.errors`, `cerberus.utils`, `cerberus.platform`.

## 1. Package namespace

`cerberus.__all__ = [DocumentError, SchemaError, TypeDefinition, Validator,
schema_registry, rules_set_registry, __version__]`.
`__version__` comes from `importlib.metadata.version("Cerberus")`, falling
back to `"unknown"`; the module is reloadable (upstream tests `reload()` it).
`cerberus.platform` raises `RuntimeError` at import under `python -OO`
(docstrings carry the rule schemas).

## 2. `Validator`

`Validator = InspectedValidator('Validator', (BareValidator,), {})`;
`Validator.__mro__ == (Validator, BareValidator, object)`.

### 2.1 Construction

`__init__(self, *args, **kwargs)` with the effective signature
`(schema=None, ignore_none_values=False, allow_unknown=False,
require_all=False, purge_unknown=False, purge_readonly=False,
error_handler=BasicErrorHandler)`.

- Positional arguments are mapped onto that signature; a duplicate raises
  `TypeError("__init__ got multiple values for argument '<p>'")`.
- **All keyword arguments are kept in `self._config`** (except
  `error_handler`), including unknown ones; they are passed on to every child
  validator. This is the documented way to carry context
  (`Validator(schema, my_ctx=...)` + `self._config['my_ctx']`).
- `error_handler`: a `BaseErrorHandler` subclass, an instance, or a
  `(class, kwargs-dict)` tuple; anything else → `RuntimeError('Invalid
  error_handler.')`.
- Assigning `schema` validates it (`SchemaError` on failure); assigning a
  non-bool, non-`DefinitionSchema` `allow_unknown` validates it as a rules set.
- Subclasses commonly override `__init__` and call `super().__init__`.

### 2.2 Instance attributes and properties

| Name | Kind | Notes |
| --- | --- | --- |
| `document` | attr | `copy(document)` of the last processed document (shallow!), normalized in place. `None` initially. |
| `_errors` | attr | `errors.ErrorList` of `ValidationError`, **sorted** after every submission. New object per run. |
| `recent_error` | attr | last submitted `ValidationError` or `None`. |
| `document_error_tree`, `schema_error_tree` | attr | `DocumentErrorTree` / `SchemaErrorTree`; new objects per run. |
| `document_path`, `schema_path` | attr | tuples; `()` on a root validator. |
| `update` | attr | the `update` flag of the last `validate` call. |
| `error_handler` | attr | handler instance. Children get the shared `ToyErrorHandler`. |
| `_config` | attr | dict of constructor kwargs (+ `is_child`, `root_*`, `_is_normalized`, …). |
| `_remaining_rules` | attr | queue of rules still to run for the current field. |
| `_unrequired_by_excludes` | attr | set, exists only after the first `validate()`. |
| `schema` | property | `DefinitionSchema` (root) / plain mapping (child) / `None`. Setter validates. Overridable (upstream test replaces the setter). |
| `allow_unknown` | property | bool, mapping (rules set) or registry name. Setter validates. |
| `require_all`, `ignore_none_values`, `purge_unknown`, `purge_readonly` | properties | stored in `_config`. |
| `errors` | property | `self.error_handler(self._errors)`. |
| `is_child`, `_is_normalized` | properties | `_config` backed. |
| `root_allow_unknown`, `root_require_all`, `root_document`, `root_schema` | properties | values of the first-level ancestor. |
| `schema_registry`, `rules_set_registry` | properties | per-instance override through `_config`, else the module globals. |

### 2.3 Class attributes

`types_mapping` (dict of `TypeDefinition`; mutable, overridable per class and
commonly mutated in place), `mandatory_validations = ('nullable',)`,
`priority_validations = ('nullable', 'readonly', 'type', 'empty')`,
`_valid_schemas` (set of schema hashes, **shared by all classes that do not
redefine it**), and the introspection results set by the metaclass:
`validation_rules`, `normalization_rules`, `rules`, `checkers`, `coercers`,
`default_setters`, `_types_from_methods`, plus the read-only class property
`types`.

### 2.4 Methods

Public: `validate(document, schema=None, update=False, normalize=True)`,
`__call__` (same function object), `validated(*args, always_return_document=False, **kwargs)`,
`normalized(document, schema=None, always_return_document=False)`,
`clear_caches()` (classmethod).

Protected and used by extensions: `_error(*args)` (three signatures),
`_get_child_validator(document_crumb=None, schema_crumb=None, **kwargs)`,
`_drop_remaining_rules(*rules)`, `_drop_nodes_from_errorpaths`,
`_lookup_field(path)`, `_resolve_rules_set`, `_resolve_schema`, every
`_validate_<rule>`, `_normalize_coerce`, `_normalize_default`,
`_normalize_default_setter`, `_normalize_purge_unknown`, `_normalize_rename`,
`_normalize_rename_handler`.

Raised exceptions: `DocumentError` (`"document is missing"`,
`"'{0}' is not a document, must be a dict"`), `SchemaError`
(`"validation schema missing"`, `"schema definition for field '{0}' must be a
dict"`, or the formatted schema errors dict), `RuntimeError("There's no
handler for '<rule>' in the '<domain>' domain.")`, and whatever user callables
raise outside `coerce` / `default_setter` / `rename_handler`.

### 2.5 Processing order (observable through errors and hooks)

`validate`: reset state → `copy(document)` → optional schema replacement →
`DocumentError` checks → `error_handler.start` → normalization (if
`normalize`) → per field in document order: rules or unknown-field handling →
required fields (unless `update`) → `error_handler.end` → `not bool(_errors)`.

Normalization of one mapping: rename / rename_handler → purge_unknown (only if
`allow_unknown` is falsy) → purge_readonly → readonly check → defaults →
default setters (with dependency resolution) → coerce → containers
(keysrules, valuesrules, schema/allow_unknown/purge_unknown, sequence schema,
items).

Rules of one field: `nullable`, `readonly`, `type`, `empty` (if present;
`nullable` always) and then the remaining rules **in schema order**, skipping
normalization rules and `allow_unknown`, `require_all`, `meta`, `required`.
A rule method returning a truthy value stops the field's processing.
`nullable`, `empty`, `type`, `readonly` drop queued rules.

## 3. Rules

### 3.1 Validation rules

| Rule | Constraint schema (docstring) | Behavior summary |
| --- | --- | --- |
| `allof`, `anyof`, `noneof`, `oneof` | `{'type': 'list', 'logical': '<op>'}` | each definition validated by a child with `allow_unknown=True`, inheriting `type`/`allow_unknown`; group errors 0x94/0x93/0x91/0x92 with `info=(child_errors, valids, n)`. |
| `allow_unknown` | bool or rules set (dict/registry name) | field-level override for `schema` children; never run as a rule. |
| `allowed` | `{'type': 'container'}` | non-str iterables: every member; else the value. 0x45 / 0x44. |
| `check_with` (`validator`) | callable, name, or list of both | `_check_with_<name>(field, value)` / `f(field, value, error)`; legacy `_validator_<name>` with `DeprecationWarning`. |
| `contains` | `{'empty': False}` | missing members → 0x48 with a `set`. |
| `dependencies` | hashable, list or dict | field presence (0x04, one error per missing field) or values (0x05); dotted paths, `^` root prefix. |
| `empty` | bool | sized and `len == 0`; drops `allowed`, `forbidden`, `items`, `minlength`, `maxlength`, `regex`, `check_with`. 0x22. |
| `excludes` | hashable or list | 0x06; interacts with `required`. |
| `forbidden` | list | 0x47 / 0x46. |
| `items` | list of rules sets | positional validation; 0x26 length mismatch, 0x8F group. |
| `keysrules` (`keyschema`) | rules set | validates keys; group 0x83. |
| `valuesrules` (`valueschema`) | rules set | validates values; group 0x84. |
| `max`, `min` | `{'nullable': False}` | `value > max` / `value < min`; `TypeError` is swallowed. 0x43 / 0x42. |
| `maxlength`, `minlength` | integer | `Iterable` and `len`. 0x28 / 0x27 with `info=(len,)`. |
| `meta` | anything | ignored. |
| `nullable` | bool | 0x23; drops most rules on `None`. Mandatory. |
| `readonly` | bool | 0x63; also evaluated during normalization. |
| `regex` | string | `re.compile(pattern + '$' unless it ends with '$').match(value)` for strings. 0x41. |
| `required` | bool | checked after all fields. 0x02. |
| `require_all` | bool | field-level override for `schema` children. |
| `schema` | rules set (sequence) or schema (mapping), or registry name | group 0x82 / 0x81; 0x25 if a mapping schema meets a sequence definition. |
| `type` | name or list of names | `types_mapping` lookup, else legacy `_validate_type_<name>(value)`. 0x24, drops all remaining rules. |

Shortcuts: `anyof_<rule>`, `allof_<rule>`, `noneof_<rule>`, `oneof_<rule>`
("type savers", e.g. `anyof_type`) are expanded in place to the `*of` form.
Rule names may contain spaces instead of underscores (`'default setter'`).

Types: `binary`, `boolean`, `container`, `date`, `datetime`, `dict`, `float`,
`integer`, `list`, `number`, `set`, `string` (see `types_mapping` for the
included/excluded types; e.g. `bool` is an `integer` but not a `number`,
strings are neither `list` nor `container`).

### 3.2 Normalization rules

`coerce` (callable, name → `_normalize_coerce_<name>`, or a chain),
`default`, `default_setter` (callable or name →
`_normalize_default_setter_<name>`), `purge_unknown` (validator- and
field-level), `rename`, `rename_handler` (like `coerce`), plus the
validator-level `purge_readonly`. Errors: 0x61, 0x62, 0x64 (incl. `'Circular
dependencies of default setters.'`).

## 4. Extension points

- **Custom rules:** `_validate_<rule>(self, constraint, field, value)`; the
  docstring holds the constraint's schema as a Python literal, optionally
  after the sentence `The rule's arguments are validated against this
  schema:`. A missing/unparseable schema warns (`UserWarning`).
- **Checks:** `_check_with_<name>(self, field, value)`.
- **Coercers / setters:** `_normalize_coerce_<name>(self, value)`,
  `_normalize_default_setter_<name>(self, document)`.
- **Types:** `types_mapping` + `TypeDefinition(name, included, excluded)`;
  deprecated `_validate_type_<name>(self, value)` (class creation emits
  `DeprecationWarning`).
- **Overriding built-ins:** any `_validate_*` / `_normalize_*` method and the
  protected helpers of 2.4, including the `schema` property.
- **Metaclass `InspectedValidator`:** collects the above on class creation
  (also for classes built with `validator_factory(name, bases, namespace)`,
  which merges docstrings of mixins).
- **Child validators:** created through `_get_child_validator`; they are
  instances of the same class, receive the full `_config` and are what
  `self` is inside hooks at nested levels (`self.document`,
  `self.root_document`, `self.document_path`, …).
- **Error handlers:** subclass `BaseErrorHandler` (`__call__`, `add`, `emit`,
  `start`, `end`, `extend`) or `BasicErrorHandler` (override `messages`,
  `_format_message`, …).

## 5. Schemas and registries

- `DefinitionSchema(validator, schema)` — `MutableMapping`; expands shortcuts
  and renames deprecated rule names **in the user's own dict**, validates
  against the rules' docstring schemas with a `SchemaValidator`
  (`validator_factory('SchemaValidator', SchemaValidatorMixin)`), caches
  validity in `validator._valid_schemas` by
  `(mapping_hash(schema), mapping_hash(types_mapping))`. Methods: mapping
  protocol, `copy`, `update`, `validate`, `regenerate_validation_schema`,
  `expand` (classmethod), `get`, `items`, `__str__`/`__repr__`.
  The wrapped dict is `DefinitionSchema.schema` (not a copy if a mapping was
  given).
- `UnvalidatedSchema(schema={})`, `SchemaValidationSchema(validator)`.
- `SchemaError`.
- `Registry(definitions={})`: `add`, `all`, `clear`, `extend`, `get`,
  `remove`; `SchemaRegistry`, `RulesSetRegistry`; module-level
  `schema_registry`, `rules_set_registry`. References are strings used in
  place of a schema / rules set (also as `allow_unknown`), resolved at
  validation time; recursion through references is legal.

## 6. Errors

- `ErrorDefinition(code, rule)` namedtuple and the constants of
  `cerberus.errors` (codes 0x00–0x94).
- `ValidationError(document_path, schema_path, code, rule, constraint, value,
  info)`: plain attributes, `__eq__`/`__hash__` on paths and code, `__lt__`
  via `compare_paths_lt`, `__repr__`, properties `child_errors`,
  `definitions_errors`, `field`, `is_group_error`, `is_logic_error`,
  `is_normalization_error`.
- `ErrorList` (`in` accepts only `ErrorDefinition`).
- `ErrorTreeNode`, `ErrorTree`, `DocumentErrorTree`, `SchemaErrorTree`
  (`add`, `fetch_errors_from`, `fetch_node_from`, mapping protocol).
- `BaseErrorHandler`, `ToyErrorHandler`, `BasicErrorHandler` (`messages`,
  `tree`, `pretty_tree`, `__str__`), `SchemaErrorHandler`.
- Message strings are part of the surface (see `BasicErrorHandler.messages`).

## 7. Utilities

`TypeDefinition`, `validator_factory`, `compare_paths_lt`,
`drop_item_from_tuple`, `get_Validator_class`, `mapping_hash`,
`mapping_to_frozenset`, `quote_string`, `readonly_classproperty`.

## 8. Python-level behaviors

- **Documents:** any `Mapping`; `copy.copy` decides how it is copied. Nested
  mappings are rebuilt with `type(value)(normalized)` when a `schema` rule
  normalizes them; nested sequences with `type(value)(values)`.
- **Type tests** use `isinstance` against ABCs (`Mapping`, `Sequence`,
  `Container`, `Iterable`, `Sized`, `Hashable`), so `ABC.register` and
  subclasses count.
- **Values** are compared with Python semantics (`==`, `<`, `in`, hashing for
  `contains` / `forbidden` on sequences), e.g. `1 == 1.0 == True`,
  `Decimal`, `datetime`, user classes.
- **Reuse:** one validator instance validates any number of documents; all
  per-run state is reset by each call. A validator is **not** thread-safe
  (state lives on the instance); separate instances are independent except
  for the shared `_valid_schemas` cache and the registries.
- **Copy / pickle:** validators, schemas, errors and trees are ordinary
  Python objects.
- **Warnings:** deprecated rule names, legacy type methods, missing rule
  schemas, key collisions on `keysrules` renaming.

## Quirks that must be preserved

1. `_validate_dependencies` queries the *document* error tree with a *schema*
   path to decide whether to stop processing the field.
2. `ValidationError.__lt__` is not a consistent order (`compare_paths_lt`
   returns `True` for equal paths and for mixed `int`/`str` in both
   directions, and can raise `TypeError`); the resulting order of `_errors`
   is whatever `list.sort` makes of it.
3. `required` errors for `require_all` carry the *string*
   `"__require_all__"` as `schema_path`.
4. `allowed` on a non-string iterable value reports the offending members;
   `forbidden` on a sequence reports `list(set(value) & set(forbidden))`
   (set iteration order).
5. Normalizing `keysrules` mutates the *caller's* nested mapping (only the
   top level of the document is copied).
6. `normalized()`/`validate(normalize=True)` re-wrap and re-validate the
   schema (`schema.copy()` + per-field `__setitem__`) on every call — the
   dominant cost in the baseline — and thereby populate `_valid_schemas`.
7. `BasicErrorHandler` deep-copies every error (including the offending
   value) before formatting.
8. A `coerce`/`rename_handler`/`default_setter` exception becomes an error; a
   `check_with` or custom-rule exception propagates.
9. `regex` appends `$` unless the pattern already ends with `$`, and uses
   `re.match` (anchored at the start, `$` also matches before a trailing
   newline).
10. With `purge_unknown=True` and a truthy `allow_unknown`, nothing is purged.
11. `empty`, `minlength`, `maxlength` call `len()` on anything `Sized` /
    `Iterable` (generators raise `TypeError`).
12. Schema validation errors use `SchemaErrorHandler` (`"unknown rule"`).
