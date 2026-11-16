//! Native core of ferberus.
//!
//! The Python package (`python/ferberus`) defines every user-visible class;
//! this crate accelerates document processing for it. See `DESIGN.md`.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;

mod compile;
mod engine;
mod errors;
#[cfg(feature = "experiments")]
mod experiments;
mod hashing;
mod refs;
mod render;
mod stats;
mod types;

pub(crate) static ENABLED: AtomicBool = AtomicBool::new(true);
pub(crate) static REGEX_FAST: AtomicBool = AtomicBool::new(true);
/// Subjects of the `regex` rule at least this long (bytes) are matched with
/// the interpreter released.
pub(crate) static REGEX_DETACH_THRESHOLD: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(ferberus_regex::DETACH_THRESHOLD);

#[pymodule(gil_used = false)]
mod _core {
    use super::*;

    #[cfg(feature = "experiments")]
    #[pymodule_export]
    use super::experiments::{exp_convert_only, exp_convert_walk, exp_direct_walk};
    #[pymodule_export]
    use super::hashing::{mapping_hash, mapping_to_frozenset};
    #[pymodule_export]
    use super::render::render_errors;
    #[pymodule_export]
    use super::stats::{reset_stats, stats};

    /// Version of the native core.
    #[pyfunction]
    fn core_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// A complete `Validator.validate()` call without schema argument.
    ///
    /// Returns the result of the validation, `None` if nothing was done (the
    /// reference implementation has to run the whole call) or `...` if only
    /// the head of the call was done (it has to continue with the body).
    #[pyfunction]
    fn validate<'py>(
        validator: &Bound<'py, PyAny>,
        document: &Bound<'py, PyAny>,
        update: &Bound<'py, PyAny>,
        normalize: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = validator.py();
        if !ENABLED.load(Ordering::Relaxed) {
            crate::stats::count(crate::stats::Stat::DeclinedDisabled);
            return Ok(py.None().into_bound(py));
        }
        let normalize = normalize.is_truthy()?;
        Ok(match engine::process(validator, document, Some((update, normalize)))? {
            engine::Outcome::NotStarted => py.None().into_bound(py),
            engine::Outcome::PreludeDone => py.Ellipsis().into_bound(py),
            // return not bool(self._errors)
            engine::Outcome::Done => {
                let errors = validator.getattr(pyo3::intern!(py, "_errors"))?;
                pyo3::types::PyBool::new(py, !errors.is_truthy()?).to_owned().into_any()
            }
        })
    }

    /// A complete `Validator.normalized()` call without schema argument, up
    /// to and including `error_handler.end()`. Returns `True` if all was
    /// done, else `None` or `...` like `validate`.
    #[pyfunction]
    fn normalize<'py>(validator: &Bound<'py, PyAny>, document: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = validator.py();
        if !ENABLED.load(Ordering::Relaxed) {
            crate::stats::count(crate::stats::Stat::DeclinedDisabled);
            return Ok(py.None().into_bound(py));
        }
        Ok(match engine::process(validator, document, None)? {
            engine::Outcome::NotStarted => py.None().into_bound(py),
            engine::Outcome::PreludeDone => py.Ellipsis().into_bound(py),
            engine::Outcome::Done => pyo3::types::PyBool::new(py, true).to_owned().into_any(),
        })
    }

    /// Runs the body of `Validator.validate()` natively for a validator that
    /// was reset and handed its document by Python code. Returns `False` if
    /// the reference implementation has to do it.
    #[pyfunction]
    fn validate_body(
        validator: &Bound<'_, PyAny>,
        normalize: &Bound<'_, PyAny>,
        update: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        if !ENABLED.load(Ordering::Relaxed) {
            crate::stats::count(crate::stats::Stat::DeclinedDisabled);
            return Ok(false);
        }
        engine::validate_body(validator, normalize.is_truthy()?, update.is_truthy()?)
    }

    /// Runs the body of `Validator.normalized()` natively, see `validate_body`.
    #[pyfunction]
    fn normalize_body(validator: &Bound<'_, PyAny>) -> PyResult<bool> {
        if !ENABLED.load(Ordering::Relaxed) {
            crate::stats::count(crate::stats::Stat::DeclinedDisabled);
            return Ok(false);
        }
        engine::normalize_body(validator)
    }

    /// A validator class was modified: cached class analyses are void.
    #[pyfunction]
    fn class_changed() {
        compile::CLASS_EPOCH.fetch_add(1, Ordering::AcqRel);
    }

    /// `Validator.clear_caches()` was called.
    #[pyfunction]
    fn caches_cleared() {
        compile::CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
        compile::Program::clear_cache();
    }

    /// Switches the `regex`-crate fast path of the `regex` rule on or off;
    /// returns the previous state.
    #[pyfunction]
    fn set_regex_fast(enabled: bool) -> bool {
        REGEX_FAST.swap(enabled, Ordering::AcqRel)
    }

    /// Sets the subject length from which the `regex` rule releases the
    /// interpreter while matching; returns the previous value. For
    /// experiments, see `benches/exp_gil_release.py`.
    #[pyfunction]
    fn set_regex_detach_threshold(length: usize) -> usize {
        REGEX_DETACH_THRESHOLD.swap(length, Ordering::AcqRel)
    }

    /// What the fast path of the `regex` rule answers for
    /// `re.compile(pattern).match(subject)`: a bool, or `None` if it does not
    /// apply to the pattern or the subject. For tests.
    #[pyfunction]
    fn regex_fast_match(pattern: &str, subject: &str) -> Option<bool> {
        if !subject.is_ascii() {
            return None;
        }
        ferberus_regex::compile(pattern).map(|regex| regex.is_match(subject.as_bytes()))
    }

    /// The translation of a Python pattern for the `regex` crate, or `None`.
    #[pyfunction]
    fn regex_fast_translate(pattern: &str) -> Option<String> {
        ferberus_regex::translate(pattern).ok()
    }

    /// Switches the native engine on or off; returns the previous state.
    #[pyfunction]
    fn set_enabled(enabled: bool) -> bool {
        ENABLED.swap(enabled, Ordering::AcqRel)
    }
}
