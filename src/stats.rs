//! Engine statistics: how much work ran natively, was delegated to the
//! reference implementation, or was declined — and why.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pyo3::prelude::*;
use pyo3::types::PyDict;

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Stat {
    NativeCalls,
    NativeFrames,
    ObjectFrames,
    DelegatedFrames,
    Compilations,
    DeclinedDisabled,
    DeclinedExoticClass,
    DeclinedInstanceOverride,
    DeclinedChild,
    DeclinedDocumentType,
    DeclinedSchemaType,
    DeclinedUnsupportedSchema,
}

const NAMES: [&str; 12] = [
    "native_calls",
    "native_frames",
    "object_frames",
    "delegated_frames",
    "compilations",
    "declined_disabled",
    "declined_exotic_class",
    "declined_instance_override",
    "declined_child",
    "declined_document_type",
    "declined_schema_type",
    "declined_unsupported_schema",
];

static COUNTERS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
static REASONS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());
/// Reasons are only collected on request: the map is behind a lock.
static DETAILED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn count(stat: Stat) {
    COUNTERS[stat as usize].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn reason(reason: &'static str) {
    if DETAILED.load(Ordering::Relaxed)
        && let Ok(mut reasons) = REASONS.lock()
    {
        *reasons.entry(reason).or_insert(0) += 1;
    }
}

/// Counters of the native engine since the last `reset_stats()`.
#[pyfunction]
pub fn stats(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let result = PyDict::new(py);
    for (name, counter) in NAMES.iter().zip(&COUNTERS) {
        result.set_item(name, counter.load(Ordering::Relaxed))?;
    }
    let reasons = PyDict::new(py);
    if let Ok(collected) = REASONS.lock() {
        for (reason, count) in collected.iter() {
            reasons.set_item(reason, count)?;
        }
    }
    result.set_item("reasons", reasons)?;
    Ok(result)
}

/// Resets the counters; `detailed=True` also collects the reasons for
/// declined calls and delegated frames.
#[pyfunction]
#[pyo3(signature = (detailed = false))]
pub fn reset_stats(detailed: bool) {
    for counter in &COUNTERS {
        counter.store(0, Ordering::Relaxed);
    }
    if let Ok(mut reasons) = REASONS.lock() {
        reasons.clear();
    }
    DETAILED.store(detailed, Ordering::Relaxed);
}
