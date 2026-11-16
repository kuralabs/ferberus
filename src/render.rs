//! `BasicErrorHandler.__call__` without its deep copies.
//!
//! The Python handler deep-copies every error (including the offending value
//! and the constraint) so that it can rewrite the document paths of child
//! errors in place. Here the rewritten paths are computed on the side. That
//! is only equivalent if the deep copy could neither fail nor be observed,
//! so the fast path is limited to errors that hold plain built-in values;
//! everything else is left to the Python implementation.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};

use crate::refs::Refs;
use crate::types::{Exact, exact_of};

/// An error with the document path the handler would have rewritten it to.
struct Node<'py> {
    error: Bound<'py, PyAny>,
    code: u32,
    path: Vec<Bound<'py, PyAny>>,
    children: Vec<Node<'py>>,
    /// For logic errors: `definitions_errors`, as indices into `children`
    /// grouped by definition, in order of first appearance.
    definitions: Vec<Vec<usize>>,
}

impl Node<'_> {
    fn is_group(&self) -> bool {
        self.code & 0x80 != 0
    }

    /// `bool(self.code & LOGICAL.code - ERROR_GROUP.code)`
    fn is_logic(&self) -> bool {
        self.code & 0x10 != 0
    }
}

/// Values whose `deepcopy` cannot fail, has no side effects and yields an
/// equal object that formats the same.
fn is_plain(value: &Bound<'_, PyAny>, depth: usize) -> PyResult<bool> {
    if depth > 12 {
        return Ok(false);
    }
    Ok(match exact_of(value) {
        Exact::None | Exact::Bool | Exact::Int | Exact::Float | Exact::Str | Exact::Bytes => true,
        Exact::List | Exact::Tuple | Exact::Set | Exact::FrozenSet => {
            for item in value.try_iter()? {
                if !is_plain(&item?, depth + 1)? {
                    return Ok(false);
                }
            }
            true
        }
        Exact::Dict => {
            for (key, item) in crate::compile::dict_snapshot(value.cast_exact()?) {
                if !is_plain(&key, depth + 1)? || !is_plain(&item, depth + 1)? {
                    return Ok(false);
                }
            }
            true
        }
        Exact::ByteArray | Exact::Other => false,
    })
}

/// What `deepcopy(value)` yields for a plain value, as far as it can be told
/// apart from the original: sets are rebuilt from a list of their members,
/// which can change their iteration order (and thereby their `repr`).
fn deep_copied<'py>(value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    Ok(match exact_of(&value) {
        Exact::Set | Exact::FrozenSet => {
            let members = PyList::empty(py);
            for member in value.try_iter()? {
                members.append(deep_copied(member?)?)?;
            }
            value.get_type().call1((members,))?
        }
        Exact::List => {
            let copy = PyList::empty(py);
            for item in value.try_iter()? {
                copy.append(deep_copied(item?)?)?;
            }
            copy.into_any()
        }
        Exact::Tuple => {
            let mut items = Vec::new();
            for item in value.try_iter()? {
                items.push(deep_copied(item?)?);
            }
            PyTuple::new(py, items)?.into_any()
        }
        Exact::Dict => {
            let copy = PyDict::new(py);
            for (key, item) in crate::compile::dict_snapshot(value.cast_exact()?) {
                copy.set_item(deep_copied(key)?, deep_copied(item)?)?;
            }
            copy.into_any()
        }
        _ => value,
    })
}

/// Whether [`deep_copied`] can differ from its argument.
fn holds_set(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(match exact_of(value) {
        Exact::Set | Exact::FrozenSet => true,
        Exact::List | Exact::Tuple => {
            for item in value.try_iter()? {
                if holds_set(&item?)? {
                    return Ok(true);
                }
            }
            false
        }
        Exact::Dict => {
            for (key, item) in crate::compile::dict_snapshot(value.cast_exact()?) {
                if holds_set(&key)? || holds_set(&item)? {
                    return Ok(true);
                }
            }
            false
        }
        _ => false,
    })
}

/// The value as the handler's deep copy of the error would hold it.
fn as_copied<'py>(value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    if holds_set(&value)? { deep_copied(value) } else { Ok(value) }
}

/// Reads an error (and its descendants) and computes the rewritten paths;
/// `None` if the error is not eligible for the fast path.
fn read<'py>(
    refs: &Refs,
    error: &Bound<'py, PyAny>,
    path: Vec<Bound<'py, PyAny>>,
    offset: usize,
    depth: usize,
) -> PyResult<Option<Node<'py>>> {
    let py = error.py();
    if depth > 64 || !error.get_type().is(refs.validation_error.bind(py)) {
        return Ok(None);
    }
    let code = error.getattr(intern!(py, "code"))?;
    if exact_of(&code) != Exact::Int {
        return Ok(None);
    }
    let Ok(code) = code.extract::<u32>() else {
        return Ok(None);
    };
    let info = error.getattr(intern!(py, "info"))?;
    let Ok(info) = info.cast_exact::<PyTuple>() else {
        return Ok(None);
    };
    if !is_plain(&error.getattr(intern!(py, "constraint"))?, 0)?
        || !is_plain(&error.getattr(intern!(py, "value"))?, 0)?
        || !is_plain(&error.getattr(intern!(py, "rule"))?, 0)?
    {
        return Ok(None);
    }
    let schema_path = error.getattr(intern!(py, "schema_path"))?;
    if !is_plain(&schema_path, 0)? || !path.iter().all(|item| is_plain(item, 0).unwrap_or(false)) {
        return Ok(None);
    }
    for item in &path {
        // keys of the result; a rebuilt set would be another story
        if holds_set(item)? {
            return Ok(None);
        }
    }
    let mut node = Node { error: error.clone(), code, path, children: Vec::new(), definitions: Vec::new() };
    if !node.is_group() {
        return Ok(if is_plain(info.as_any(), 0)? { Some(node) } else { None });
    }

    // a group error: info = (child_errors, ...)
    if info.is_empty() || !info.iter().skip(1).all(|item| is_plain(&item, 0).unwrap_or(false)) {
        return Ok(None);
    }
    let children = info.get_item(0)?;
    if !(children.get_type().is(refs.error_list.bind(py)) || exact_of(&children) == Exact::List) {
        return Ok(None);
    }
    let children: Vec<Bound<'py, PyAny>> = children.try_iter()?.collect::<PyResult<_>>()?;
    // child_start = len(error.document_path) - offset
    let Some(child_start) = node.path.len().checked_sub(offset) else {
        return Ok(None);
    };

    if node.is_logic() {
        // definitions_errors: children grouped by schema_path[len(self.schema_path)]
        let Ok(own_schema_path) = schema_path.cast_exact::<PyTuple>() else {
            return Ok(None);
        };
        let rule = error.getattr(intern!(py, "rule"))?;
        let mut keys: Vec<Bound<'py, PyAny>> = Vec::new();
        for child in &children {
            if !child.get_type().is(refs.validation_error.bind(py)) {
                return Ok(None);
            }
            let child_schema_path = child.getattr(intern!(py, "schema_path"))?;
            let Ok(child_schema_path) = child_schema_path.cast_exact::<PyTuple>() else {
                return Ok(None);
            };
            if child_schema_path.len() <= own_schema_path.len() {
                return Ok(None);
            }
            let definition = child_schema_path.get_item(own_schema_path.len())?;
            if !matches!(exact_of(&definition), Exact::Int | Exact::Str) {
                return Ok(None);
            }
            let mut position = None;
            for (i, key) in keys.iter().enumerate() {
                if key.eq(&definition)? {
                    position = Some(i);
                    break;
                }
            }
            let position = match position {
                Some(position) => position,
                None => {
                    keys.push(definition.clone());
                    node.definitions.push(Vec::new());
                    keys.len() - 1
                }
            };
            // nodename = '%s definition %s' % (error.rule, i)
            let nodename = PyString::new(py, &format!("{} definition {}", rule.str()?, definition.str()?));
            let Some(child_path) = rewritten(&node.path, Some(nodename.into_any()), child, child_start)? else {
                return Ok(None);
            };
            let Some(child_node) = read(refs, child, child_path, offset + 1, depth + 1)? else {
                return Ok(None);
            };
            node.definitions[position].push(node.children.len());
            node.children.push(child_node);
        }
    } else {
        for child in &children {
            let Some(child_path) = rewritten(&node.path, None, child, child_start)? else {
                return Ok(None);
            };
            let Some(child_node) = read(refs, child, child_path, offset, depth + 1)? else {
                return Ok(None);
            };
            node.children.push(child_node);
        }
    }
    Ok(Some(node))
}

/// `parent_path (+ (nodename,)) + child.document_path[child_start:]`
fn rewritten<'py>(
    parent: &[Bound<'py, PyAny>],
    nodename: Option<Bound<'py, PyAny>>,
    child: &Bound<'py, PyAny>,
    child_start: usize,
) -> PyResult<Option<Vec<Bound<'py, PyAny>>>> {
    let child_path = child.getattr(intern!(child.py(), "document_path"))?;
    let Ok(child_path) = child_path.cast_exact::<PyTuple>() else {
        return Ok(None);
    };
    let mut path = parent.to_vec();
    path.extend(nodename);
    path.extend(child_path.iter().skip(child_start));
    Ok(Some(path))
}

struct Renderer<'py> {
    py: Python<'py>,
    messages: Bound<'py, PyAny>,
    tree: Bound<'py, PyDict>,
}

impl<'py> Renderer<'py> {
    /// `self.messages[error.code].format(*error.info, constraint=..., field=..., value=...)`
    fn format(&self, field: &Bound<'py, PyAny>, node: &Node<'py>) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let template = self.messages.get_item(node.code)?;
        if let Ok(text) = template.cast_exact::<PyString>()
            && !text.to_str()?.contains(['{', '}'])
        {
            return Ok(template);
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "constraint"), as_copied(node.error.getattr(intern!(py, "constraint"))?)?)?;
        kwargs.set_item(intern!(py, "field"), field)?;
        kwargs.set_item(intern!(py, "value"), as_copied(node.error.getattr(intern!(py, "value"))?)?)?;
        // (group errors have no placeholders for their info in the stock
        // messages; their child lists are passed as they are)
        let info = node.error.getattr(intern!(py, "info"))?;
        let info = if node.is_group() { info } else { as_copied(info)? };
        template.getattr(intern!(py, "format"))?.call(info.cast_into::<PyTuple>()?, Some(&kwargs))
    }

    /// `error.field`
    fn field(&self, node: &Node<'py>) -> Bound<'py, PyAny> {
        match node.path.last() {
            Some(field) => field.clone(),
            None => self.py.None().into_bound(self.py),
        }
    }

    /// `_insert_error`
    fn insert(
        &self,
        tree: &Bound<'py, PyDict>,
        path: &[Bound<'py, PyAny>],
        message: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = self.py;
        let Some(field) = path.first() else {
            return Err(pyo3::exceptions::PyIndexError::new_err("tuple index out of range"));
        };
        if path.len() == 1 {
            match tree.get_item(field)? {
                Some(list) => {
                    // subtree = self.tree[field].pop(); self.tree[field] += [node, subtree]
                    let list = list.cast_into::<PyList>()?;
                    let subtree = list.call_method0(intern!(py, "pop"))?;
                    list.append(message)?;
                    list.append(subtree)?;
                }
                None => tree.set_item(field, PyList::new(py, [message.clone(), PyDict::new(py).into_any()])?)?,
            }
        } else {
            let list = match tree.get_item(field)? {
                Some(list) => list.cast_into::<PyList>()?,
                None => {
                    let list = PyList::new(py, [PyDict::new(py)])?;
                    tree.set_item(field, &list)?;
                    list
                }
            };
            let subtree = list.get_item(list.len() - 1)?.cast_into::<PyDict>()?;
            self.insert(&subtree, &path[1..], message)?;
        }
        Ok(())
    }

    /// `add`, after the error was copied and its paths rewritten.
    fn add(&self, node: &Node<'py>) -> PyResult<()> {
        if node.is_logic() {
            self.insert_logic_error(node)
        } else if node.is_group() {
            self.insert_group_error(node)
        } else if self.messages.contains(node.code)? {
            self.insert(&self.tree, &node.path, &self.format(&self.field(node), node)?)
        } else {
            Ok(())
        }
    }

    /// `_insert_group_error`
    fn insert_group_error(&self, node: &Node<'py>) -> PyResult<()> {
        for child in &node.children {
            if child.is_logic() {
                self.insert_logic_error(child)?;
            } else if child.is_group() {
                self.insert_group_error(child)?;
            } else {
                self.insert(&self.tree, &child.path, &self.format(&self.field(child), child)?)?;
            }
        }
        Ok(())
    }

    /// `_insert_logic_error`
    fn insert_logic_error(&self, node: &Node<'py>) -> PyResult<()> {
        let field = self.field(node);
        self.insert(&self.tree, &node.path, &self.format(&field, node)?)?;
        for definition in &node.definitions {
            for &index in definition {
                let child = &node.children[index];
                if child.is_logic() {
                    self.insert_logic_error(child)?;
                } else if child.is_group() {
                    self.insert_group_error(child)?;
                } else {
                    self.insert(&self.tree, &child.path, &self.format(&field, child)?)?;
                }
            }
        }
        Ok(())
    }
}

/// `deepcopy(tree)` followed by `_purge_empty_dicts` on every list.
fn pretty<'py>(py: Python<'py>, tree: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyDict>> {
    let copy = PyDict::new(py);
    for (field, list) in crate::compile::dict_snapshot(tree) {
        let list = list.cast_into::<PyList>()?;
        let pretty_list = PyList::empty(py);
        let last = list.len() - 1;
        for (i, item) in list.iter().enumerate() {
            if i < last {
                pretty_list.append(item)?;
            } else {
                let subtree = item.cast_into::<PyDict>()?;
                if !subtree.is_empty() {
                    pretty_list.append(pretty(py, &subtree)?)?;
                }
            }
        }
        copy.set_item(field, pretty_list)?;
    }
    Ok(copy)
}

/// `handler(errors)` for an unmodified `BasicErrorHandler` or
/// `SchemaErrorHandler`; `NotImplemented` if the Python implementation has
/// to do it.
#[pyfunction]
pub fn render_errors<'py>(handler: &Bound<'py, PyAny>, errors: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = handler.py();
    let refs = Refs::get(py)?;
    let not_implemented = || Ok(py.NotImplemented().into_bound(py));
    if !crate::ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return not_implemented();
    }
    let handler_type = handler.get_type();
    if !(handler_type.is(refs.basic_error_handler.bind(py)) || handler_type.is(refs.schema_error_handler.bind(py))) {
        return not_implemented();
    }
    if !(errors.get_type().is(refs.error_list.bind(py)) || exact_of(errors) == Exact::List) {
        return not_implemented();
    }
    let messages = handler.getattr(intern!(py, "messages"))?;
    if exact_of(&messages) != Exact::Dict {
        return not_implemented();
    }

    let mut nodes = Vec::new();
    for error in errors.try_iter()? {
        let error = error?;
        let document_path = error.getattr(intern!(py, "document_path"))?;
        let Ok(document_path) = document_path.cast_exact::<PyTuple>() else {
            return not_implemented();
        };
        if document_path.is_empty() {
            return not_implemented();
        }
        match read(refs, &error, document_path.iter().collect(), 0, 0)? {
            Some(node) => nodes.push(node),
            None => return not_implemented(),
        }
    }

    // self.clear(); self.extend(errors); return self.pretty_tree
    let renderer = Renderer { py, messages, tree: PyDict::new(py) };
    handler.setattr(intern!(py, "tree"), &renderer.tree)?;
    for node in &nodes {
        renderer.add(node)?;
    }
    Ok(pretty(py, &renderer.tree)?.into_any())
}
