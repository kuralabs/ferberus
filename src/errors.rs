//! `ValidationError` handling: ordering, path surgery and the queries the
//! engine would otherwise ask the error trees.

use std::cmp::Ordering;

use pyo3::ffi;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};

use crate::types::{Exact, exact_of};

/// Result of comparing two error paths natively.
enum PathOrder {
    /// A strict, unambiguous order.
    Strict(Ordering),
    /// Equal paths, mixed element types or elements that are neither exact
    /// `str` nor exact `int`: `compare_paths_lt` is not a consistent order
    /// there, so only Python's own `list.sort()` gives the upstream result.
    Unclear,
}

fn compare_paths(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<PathOrder> {
    let (Ok(a), Ok(b)) = (a.cast_exact::<PyTuple>(), b.cast_exact::<PyTuple>()) else {
        return Ok(PathOrder::Unclear);
    };
    let (len_a, len_b) = (a.len(), b.len());
    for i in 0..len_a.min(len_b) {
        // SAFETY: `i` is within the bounds of both tuples.
        let (x, y) = unsafe { (a.get_borrowed_item_unchecked(i), b.get_borrowed_item_unchecked(i)) };
        match (exact_of(&x), exact_of(&y)) {
            (Exact::Str, Exact::Str) => {
                if x.as_ptr() == y.as_ptr() {
                    continue;
                }
                // code point order equals UTF-8 byte order
                let (x, y) = (x.cast_exact::<pyo3::types::PyString>()?, y.cast_exact::<pyo3::types::PyString>()?);
                match x.to_str()?.cmp(y.to_str()?) {
                    Ordering::Equal => continue,
                    order => return Ok(PathOrder::Strict(order)),
                }
            }
            (Exact::Int, Exact::Int) => {
                let mut overflow = 0;
                // SAFETY: both are exact ints.
                let (x, y) = unsafe {
                    let x = ffi::PyLong_AsLongLongAndOverflow(x.as_ptr(), &mut overflow);
                    if overflow != 0 {
                        return Ok(PathOrder::Unclear);
                    }
                    let y = ffi::PyLong_AsLongLongAndOverflow(y.as_ptr(), &mut overflow);
                    if overflow != 0 {
                        return Ok(PathOrder::Unclear);
                    }
                    (x, y)
                };
                match x.cmp(&y) {
                    Ordering::Equal => continue,
                    order => return Ok(PathOrder::Strict(order)),
                }
            }
            _ => return Ok(PathOrder::Unclear),
        }
    }
    Ok(match len_a.cmp(&len_b) {
        Ordering::Equal => PathOrder::Unclear,
        order => PathOrder::Strict(order),
    })
}

/// `ValidationError.__lt__`, where it is a strict order.
fn compare_errors(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<PathOrder> {
    let py = a.py();
    let (dp, sp) = (intern!(py, "document_path"), intern!(py, "schema_path"));
    let (dp_a, dp_b) = (a.getattr(dp)?, b.getattr(dp)?);
    match compare_paths(&dp_a, &dp_b)? {
        PathOrder::Strict(order) => Ok(PathOrder::Strict(order)),
        PathOrder::Unclear => {
            // only identical document paths lead to the schema paths
            let same = dp_a.cast_exact::<PyTuple>().is_ok()
                && dp_b.cast_exact::<PyTuple>().is_ok()
                && plain_and_equal(&dp_a, &dp_b)?;
            if !same {
                return Ok(PathOrder::Unclear);
            }
            compare_paths(&a.getattr(sp)?, &b.getattr(sp)?)
        }
    }
}

/// Both are tuples of exact `str` / `int` and compare equal.
fn plain_and_equal(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<bool> {
    let (a, b) = (a.cast_exact::<PyTuple>()?, b.cast_exact::<PyTuple>()?);
    if a.len() != b.len() {
        return Ok(false);
    }
    for (x, y) in a.iter().zip(b.iter()) {
        match (exact_of(&x), exact_of(&y)) {
            (Exact::Str, Exact::Str) | (Exact::Int, Exact::Int) => {
                if x.as_ptr() != y.as_ptr() && !x.eq(&y)? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Sorts `errors`, whose first `sorted` items are already in order, the way
/// `list.sort()` would.
///
/// `*clean` tells whether the list's order has been unambiguous so far; once
/// it is not, every later sort is done by Python.
pub fn sort_errors(errors: &Bound<'_, PyList>, sorted: usize, clean: &mut bool) -> PyResult<()> {
    let total = errors.len();
    if total <= 1 {
        return Ok(());
    }
    if *clean {
        // binary insertion on a shadow copy; the list itself is only touched
        // if every comparison was unambiguous
        let mut shadow: Vec<Bound<'_, PyAny>> = errors.iter().collect();
        // (the list can only have changed meanwhile if it is misused from
        // several threads; Python's sort copes with that)
        let mut unambiguous = shadow.len() == total && sorted <= total;
        let total = if unambiguous { total } else { 0 };
        'insertion: for i in sorted..total {
            let candidate = shadow[i].clone();
            let (mut lo, mut hi) = (0, i);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                match compare_errors(&candidate, &shadow[mid])? {
                    PathOrder::Strict(Ordering::Less) => hi = mid,
                    PathOrder::Strict(_) => lo = mid + 1,
                    PathOrder::Unclear => {
                        unambiguous = false;
                        break 'insertion;
                    }
                }
            }
            shadow[lo..=i].rotate_right(1);
        }
        if unambiguous {
            for (i, error) in shadow.into_iter().enumerate() {
                errors.set_item(i, error)?;
            }
            return Ok(());
        }
        *clean = false;
    }
    errors.call_method0(intern!(errors.py(), "sort"))?;
    Ok(())
}

/// `ValidationError.is_group_error` and its child errors.
pub fn child_errors<'py>(error: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = error.py();
    let code: u32 = error.getattr(intern!(py, "code"))?.extract()?;
    if code & 0x80 == 0 {
        return Ok(None);
    }
    Ok(Some(error.getattr(intern!(py, "info"))?.get_item(0)?))
}

/// `Validator._drop_nodes_from_errorpaths`, relative to the given base depths.
pub fn drop_nodes_from_errorpaths(
    errors: &Bound<'_, PyAny>,
    dp_base: usize,
    sp_base: usize,
    sp_items: &[usize],
) -> PyResult<()> {
    let py = errors.py();
    let sp_name = intern!(py, "schema_path");
    let _ = dp_base;
    for error in errors.try_iter()? {
        let error = error?;
        for i in sp_items.iter().rev() {
            // t[:i] + t[i + 1:]
            let path = error.getattr(sp_name)?;
            let index = sp_base + i;
            let head = path.get_item(pyo3::types::PySlice::new(py, 0, index as isize, 1))?;
            let tail = path.get_item(pyo3::types::PySlice::new(py, index as isize + 1, isize::MAX, 1))?;
            error.setattr(sp_name, head.add(tail)?)?;
        }
        // `if error.child_errors:`
        if let Some(children) = child_errors(&error)?
            && children.is_truthy()?
        {
            drop_nodes_from_errorpaths(&children, dp_base, sp_base, sp_items)?;
        }
    }
    Ok(())
}

/// Does the document error tree, built from `log`, hold an error with `code`
/// at exactly `path`?
pub fn log_has_error_at(log: &Bound<'_, PyAny>, path: &Bound<'_, PyTuple>, code: u8) -> PyResult<bool> {
    let py = log.py();
    for error in log.try_iter()? {
        let error = error?;
        let error_code: u32 = error.getattr(intern!(py, "code"))?.extract()?;
        if error_code == code as u32 && error.getattr(intern!(py, "document_path"))?.eq(path)? {
            return Ok(true);
        }
        if error_code & 0x80 != 0 {
            let children = error.getattr(intern!(py, "info"))?.get_item(0)?;
            if log_has_error_at(&children, path, code)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Does the document error tree, built from `log`, have a node at `path`?
pub fn log_has_node_at(log: &Bound<'_, PyAny>, path: &Bound<'_, PyTuple>) -> PyResult<bool> {
    let py = log.py();
    let len = path.len();
    for error in log.try_iter()? {
        let error = error?;
        let document_path = error.getattr(intern!(py, "document_path"))?;
        if document_path.len()? >= len
            && document_path.get_item(pyo3::types::PySlice::new(py, 0, len as isize, 1))?.eq(path)?
        {
            return Ok(true);
        }
        let error_code: u32 = error.getattr(intern!(py, "code"))?.extract()?;
        if error_code & 0x80 != 0 {
            let children = error.getattr(intern!(py, "info"))?.get_item(0)?;
            if log_has_node_at(&children, path)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Both paths of the error are tuples of exact `str` / `int` elements, for
/// which `compare_paths_lt` cannot raise.
pub fn has_plain_paths(error: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = error.py();
    for name in [intern!(py, "document_path"), intern!(py, "schema_path")] {
        let path = error.getattr(name)?;
        let Ok(path) = path.cast_exact::<PyTuple>() else {
            return Ok(false);
        };
        if !path.iter().all(|item| matches!(exact_of(&item), Exact::Str | Exact::Int)) {
            return Ok(false);
        }
    }
    Ok(true)
}
