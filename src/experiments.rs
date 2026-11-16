//! Measurement helpers backing design decisions (see DESIGN.md, "Crossing
//! cost"). Compiled only with the `experiments` feature; never part of a
//! release build.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};

/// An owned Rust mirror of a JSON-like Python document.
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
}

fn convert(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        Ok(Value::None)
    } else if let Ok(b) = obj.cast_exact::<PyBool>() {
        Ok(Value::Bool(b.is_true()))
    } else if let Ok(i) = obj.cast_exact::<PyInt>() {
        Ok(Value::Int(i.extract()?))
    } else if let Ok(f) = obj.cast_exact::<PyFloat>() {
        Ok(Value::Float(f.value()))
    } else if let Ok(s) = obj.cast_exact::<PyString>() {
        Ok(Value::Str(s.to_str()?.to_owned()))
    } else if let Ok(l) = obj.cast_exact::<PyList>() {
        l.iter().map(|x| convert(&x)).collect::<PyResult<Vec<_>>>().map(Value::List)
    } else if let Ok(d) = obj.cast_exact::<PyDict>() {
        let mut map = HashMap::with_capacity(d.len());
        for (k, v) in d.iter() {
            map.insert(k.cast::<PyString>()?.to_str()?.to_owned(), convert(&v)?);
        }
        Ok(Value::Dict(map))
    } else {
        Err(pyo3::exceptions::PyTypeError::new_err("unsupported type"))
    }
}

/// A stand-in for validation work on the converted tree: type dispatch plus
/// a bounds check on every scalar.
fn walk_value(v: &Value) -> u64 {
    match v {
        Value::None => 1,
        Value::Bool(b) => *b as u64,
        Value::Int(i) => (*i >= 0) as u64,
        Value::Float(f) => (*f >= 0.0) as u64,
        Value::Str(s) => (s.len() < 1 << 20) as u64,
        Value::List(l) => l.iter().map(walk_value).sum(),
        Value::Dict(d) => d.values().map(walk_value).sum(),
    }
}

/// The same work done directly on the Python objects.
fn walk_direct(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
    if obj.is_none() {
        Ok(1)
    } else if let Ok(b) = obj.cast_exact::<PyBool>() {
        Ok(b.is_true() as u64)
    } else if let Ok(i) = obj.cast_exact::<PyInt>() {
        Ok((i.extract::<i64>()? >= 0) as u64)
    } else if let Ok(f) = obj.cast_exact::<PyFloat>() {
        Ok((f.value() >= 0.0) as u64)
    } else if let Ok(s) = obj.cast_exact::<PyString>() {
        Ok((s.len()? < 1 << 20) as u64)
    } else if let Ok(l) = obj.cast_exact::<PyList>() {
        let mut n = 0;
        for x in l.iter() {
            n += walk_direct(&x)?;
        }
        Ok(n)
    } else if let Ok(d) = obj.cast_exact::<PyDict>() {
        let mut n = 0;
        for (_, v) in d.iter() {
            n += walk_direct(&v)?;
        }
        Ok(n)
    } else {
        Err(pyo3::exceptions::PyTypeError::new_err("unsupported type"))
    }
}

/// Convert the document into Rust structures (and drop them); no validation.
#[pyfunction]
pub fn exp_convert_only(doc: &Bound<'_, PyAny>) -> PyResult<()> {
    convert(doc).map(drop)
}

/// Convert the document into Rust structures, then do the work there.
#[pyfunction]
pub fn exp_convert_walk(doc: &Bound<'_, PyAny>) -> PyResult<u64> {
    Ok(walk_value(&convert(doc)?))
}

/// Do the work reading the Python objects in place.
#[pyfunction]
pub fn exp_direct_walk(doc: &Bound<'_, PyAny>) -> PyResult<u64> {
    walk_direct(doc)
}
