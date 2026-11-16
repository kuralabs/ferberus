//! `utils.mapping_to_frozenset`, the basis of the keys of
//! `Validator._valid_schemas`, built with the C-API.
//!
//! The result is the same object graph the Python implementation builds
//! (frozensets of `(key, value)` tuples, tuples for sequences, frozensets for
//! sets), so its hash is the very same number.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFrozenSet, PyList, PyTuple};

use crate::compile::dict_snapshot;
use crate::refs::Refs;
use crate::types::{Exact, exact_of};

fn convert_mapping<'py>(refs: &Refs, mapping: &Bound<'py, PyAny>, depth: usize) -> PyResult<Bound<'py, PyAny>> {
    let py = mapping.py();
    if depth > 200 {
        return Err(pyo3::exceptions::PyRecursionError::new_err("maximum recursion depth exceeded"));
    }
    // aggregation = {key: converted value}; frozenset(aggregation.items())
    let pairs = PyList::empty(py);
    match mapping.cast_exact::<PyDict>() {
        Ok(dict) => {
            for (key, value) in dict_snapshot(dict) {
                pairs.append(PyTuple::new(py, [key, convert_value(refs, value, depth)?])?)?;
            }
        }
        Err(_) => {
            // any other mapping: `for key, value in mapping.items()`, where a
            // repeated key overwrites the earlier one
            let aggregation = PyDict::new(py);
            for item in mapping.call_method0(pyo3::intern!(py, "items"))?.try_iter()? {
                let (key, value): (Bound<'py, PyAny>, Bound<'py, PyAny>) = item?.extract()?;
                aggregation.set_item(key, convert_value(refs, value, depth)?)?;
            }
            for (key, value) in dict_snapshot(&aggregation) {
                pairs.append(PyTuple::new(py, [key, value])?)?;
            }
        }
    }
    py.get_type::<PyFrozenSet>().call1((pairs,))
}

fn convert_value<'py>(refs: &Refs, value: Bound<'py, PyAny>, depth: usize) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    match exact_of(&value) {
        Exact::None | Exact::Bool | Exact::Int | Exact::Float => Ok(value),
        Exact::Dict => convert_mapping(refs, &value, depth + 1),
        Exact::List | Exact::Tuple => {
            let mut items = Vec::new();
            for item in value.try_iter()? {
                let item = item?;
                items.push(if is_mapping(refs, &item)? { convert_mapping(refs, &item, depth + 1)? } else { item });
            }
            Ok(PyTuple::new(py, items)?.into_any())
        }
        // sequences of scalars: tuple(value)
        Exact::Str | Exact::Bytes | Exact::ByteArray => py.get_type::<PyTuple>().call1((value,)),
        Exact::Set | Exact::FrozenSet => py.get_type::<PyFrozenSet>().call1((value,)),
        Exact::Other => {
            if value.is_instance(refs.mapping.bind(py))? {
                convert_mapping(refs, &value, depth + 1)
            } else if value.is_instance(refs.sequence.bind(py))? {
                // value = list(value); mappings among the items are converted
                let mut items = Vec::new();
                for item in py.get_type::<PyList>().call1((value,))?.try_iter()? {
                    let item = item?;
                    items.push(if is_mapping(refs, &item)? { convert_mapping(refs, &item, depth + 1)? } else { item });
                }
                Ok(PyTuple::new(py, items)?.into_any())
            } else if value.is_instance(refs.abc_set.bind(py))? {
                py.get_type::<PyFrozenSet>().call1((value,))
            } else {
                Ok(value)
            }
        }
    }
}

fn is_mapping(refs: &Refs, value: &Bound<'_, PyAny>) -> PyResult<bool> {
    crate::types::is_mapping(refs, value)
}

/// `cerberus.utils.mapping_to_frozenset`
#[pyfunction]
pub fn mapping_to_frozenset<'py>(mapping: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    convert_mapping(Refs::get(mapping.py())?, mapping, 0)
}

/// `cerberus.utils.mapping_hash`
#[pyfunction]
pub fn mapping_hash(mapping: &Bound<'_, PyAny>) -> PyResult<isize> {
    convert_mapping(Refs::get(mapping.py())?, mapping, 0)?.hash()
}
