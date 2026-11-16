//! Classification of Python values: exact built-in types get table lookups,
//! everything else goes through `isinstance` like Cerberus does.

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyTuple, PyType};

use crate::refs::Refs;

/// Exact (non-subclass) built-in types the engine knows by heart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Exact {
    None = 0,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    ByteArray,
    List,
    Tuple,
    Dict,
    Set,
    FrozenSet,
    Other,
}

pub const EXACT_KINDS: usize = Exact::Other as usize;

const fn bit(kind: Exact) -> u16 {
    1 << (kind as u16)
}

const MAPPING: u16 = bit(Exact::Dict);
const SEQUENCE: u16 =
    bit(Exact::Str) | bit(Exact::Bytes) | bit(Exact::ByteArray) | bit(Exact::List) | bit(Exact::Tuple);
const ITERABLE: u16 = SEQUENCE | bit(Exact::Dict) | bit(Exact::Set) | bit(Exact::FrozenSet);
const HASHABLE: u16 = bit(Exact::None)
    | bit(Exact::Bool)
    | bit(Exact::Int)
    | bit(Exact::Float)
    | bit(Exact::Str)
    | bit(Exact::Bytes)
    | bit(Exact::Tuple)
    | bit(Exact::FrozenSet);

#[inline]
pub fn exact_of(obj: &Bound<'_, PyAny>) -> Exact {
    // SAFETY: reading the type pointer of a live object; the statics are the
    // interpreter's own type objects.
    unsafe {
        let tp = ffi::Py_TYPE(obj.as_ptr());
        if tp == &raw mut ffi::PyUnicode_Type {
            Exact::Str
        } else if tp == &raw mut ffi::PyLong_Type {
            Exact::Int
        } else if tp == &raw mut ffi::PyDict_Type {
            Exact::Dict
        } else if tp == &raw mut ffi::PyList_Type {
            Exact::List
        } else if tp == &raw mut ffi::PyFloat_Type {
            Exact::Float
        } else if tp == &raw mut ffi::PyBool_Type {
            Exact::Bool
        } else if obj.as_ptr() == ffi::Py_None() {
            Exact::None
        } else if tp == &raw mut ffi::PyTuple_Type {
            Exact::Tuple
        } else if tp == &raw mut ffi::PyBytes_Type {
            Exact::Bytes
        } else if tp == &raw mut ffi::PySet_Type {
            Exact::Set
        } else if tp == &raw mut ffi::PyFrozenSet_Type {
            Exact::FrozenSet
        } else if tp == &raw mut ffi::PyByteArray_Type {
            Exact::ByteArray
        } else {
            Exact::Other
        }
    }
}

macro_rules! abc_predicate {
    ($name:ident, $mask:expr, $field:ident) => {
        #[inline]
        pub fn $name(refs: &Refs, obj: &Bound<'_, PyAny>) -> PyResult<bool> {
            match exact_of(obj) {
                Exact::Other => obj.is_instance(refs.$field.bind(obj.py())),
                kind => Ok($mask & bit(kind) != 0),
            }
        }
    };
}

abc_predicate!(is_mapping, MAPPING, mapping);
abc_predicate!(is_sequence, SEQUENCE, sequence);
abc_predicate!(is_iterable, ITERABLE, iterable);
abc_predicate!(is_sized, ITERABLE, sized);
abc_predicate!(is_hashable, HASHABLE, hashable);

/// `isinstance(obj, str)`
#[inline]
pub fn is_str(obj: &Bound<'_, PyAny>) -> bool {
    // SAFETY: plain flag check on a live object.
    unsafe { ffi::PyUnicode_Check(obj.as_ptr()) != 0 }
}

#[inline]
pub fn is_exact_dict(obj: &Bound<'_, PyAny>) -> bool {
    exact_of(obj) == Exact::Dict
}

type MaskEntry = (Py<PyAny>, Py<PyAny>, Option<u16>);

/// Masks by the addresses of `(included_types, excluded_types)`.
static MASKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(usize, usize), MaskEntry>>> =
    std::sync::LazyLock::new(Default::default);

/// One entry of `types_mapping`, prepared for fast matching.
pub struct TypeCheck {
    pub included: Py<PyAny>,
    pub excluded: Py<PyAny>,
    /// Bit per [`Exact`] kind: does a value of exactly that type match?
    /// `None` if that cannot be decided ahead of time.
    pub mask: Option<u16>,
}

impl TypeCheck {
    pub fn new(refs: &Refs, definition: &Bound<'_, PyAny>) -> PyResult<TypeCheck> {
        let py = definition.py();
        let included = definition.getattr("included_types")?;
        let excluded = definition.getattr("excluded_types")?;
        // computing the mask means two dozen `issubclass` calls against
        // ABCs: remembered per pair of tuples
        let key = (included.as_ptr() as usize, excluded.as_ptr() as usize);
        let cached = MASKS.lock().ok().and_then(|masks| masks.get(&key).map(|entry| entry.2));
        let mask = match cached {
            Some(mask) => mask,
            None => {
                let mask = if plain_type_tuple(refs, &included) && plain_type_tuple(refs, &excluded) {
                    let mut mask = 0u16;
                    for (i, tp) in exact_types(py, refs).iter().enumerate() {
                        let tp = tp.cast::<PyType>()?;
                        if tp.is_subclass(&included)? && !tp.is_subclass(&excluded)? {
                            mask |= 1 << i;
                        }
                    }
                    Some(mask)
                } else {
                    None
                };
                if let Ok(mut masks) = MASKS.lock() {
                    if masks.len() >= 1024 {
                        masks.clear();
                    }
                    // the tuples are kept alive, so their addresses stay theirs
                    masks.insert(key, (included.clone().unbind(), excluded.clone().unbind(), mask));
                }
                mask
            }
        };
        Ok(TypeCheck { included: included.unbind(), excluded: excluded.unbind(), mask })
    }

    /// `isinstance(value, included_types) and not isinstance(value, excluded_types)`
    #[inline]
    pub fn matches(&self, value: &Bound<'_, PyAny>) -> PyResult<bool> {
        if let Some(mask) = self.mask {
            let kind = exact_of(value);
            if kind != Exact::Other {
                return Ok(mask & bit(kind) != 0);
            }
        }
        let py = value.py();
        Ok(value.is_instance(self.included.bind(py))? && !value.is_instance(self.excluded.bind(py))?)
    }
}

/// A tuple of classes whose `isinstance` is the standard, class-based one
/// (metaclass `type` or `ABCMeta`), so results for exact built-in types can
/// be precomputed with `issubclass`.
fn plain_type_tuple(refs: &Refs, obj: &Bound<'_, PyAny>) -> bool {
    let py = obj.py();
    let Ok(tuple) = obj.cast_exact::<PyTuple>() else {
        return false;
    };
    tuple.iter().all(|item| {
        item.cast::<PyType>().is_ok() && {
            let meta = item.get_type();
            meta.is(py.get_type::<PyType>()) || meta.is(refs.abc_meta.bind(py))
        }
    })
}

/// The type objects of the [`Exact`] kinds, in discriminant order.
fn exact_types<'py>(py: Python<'py>, refs: &Refs) -> [Bound<'py, PyAny>; EXACT_KINDS] {
    use pyo3::types::*;
    [
        refs.none_type.bind(py).clone(),
        py.get_type::<PyBool>().into_any(),
        py.get_type::<PyInt>().into_any(),
        py.get_type::<PyFloat>().into_any(),
        py.get_type::<PyString>().into_any(),
        py.get_type::<PyBytes>().into_any(),
        py.get_type::<PyByteArray>().into_any(),
        py.get_type::<PyList>().into_any(),
        py.get_type::<PyTuple>().into_any(),
        py.get_type::<PyDict>().into_any(),
        py.get_type::<PySet>().into_any(),
        py.get_type::<PyFrozenSet>().into_any(),
    ]
}
