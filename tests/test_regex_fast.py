"""
The fast path of the ``regex`` rule must agree with Python's ``re`` wherever
it applies (DESIGN.md, "Regex"): for every pattern the translator accepts,
``re.compile`` succeeds without a warning and ``match()`` gives the same
answer for every ASCII subject.
"""

import re
import warnings

import pytest

hypothesis = pytest.importorskip("hypothesis")

from ferberus import _core  # noqa: E402
from hypothesis import given  # noqa: E402
from hypothesis import strategies as st

ALPHABET = "ab01_ \n\t-.@A\x1c$^"

literals = st.sampled_from(
    list("ab01_AZ") + ["\\.", "\\-", "\\$", "\\\\", " ", "@", "-", "\\n", "\\x41", "\\t"]
)
shorthands = st.sampled_from(
    ["\\d", "\\w", "\\s", "\\D", "\\W", "\\S", ".", "\\b", "\\B", "^", "\\A", "\\Z"]
)
class_items = st.sampled_from(
    [
        "a",
        "b",
        "0",
        "_",
        "a-b",
        "0-9",
        "A-Z",
        "\\d",
        "\\s",
        "\\w",
        "\\W",
        "\\-",
        "\\]",
        "-",
        "^",
        ".",
        "$",
        "\\n",
        " ",
    ]
)
classes = st.builds(
    lambda negated, items: "[" + ("^" if negated else "") + "".join(items) + "]",
    st.booleans(),
    st.lists(class_items, min_size=1, max_size=4),
)
quantifiers = st.sampled_from(
    ["", "", "", "*", "+", "?", "{2}", "{1,2}", "{,2}", "{1,}", "*?", "+?", "??"]
)
# things outside of the supported subset, to make sure they are declined
# rather than mistranslated
foreign = st.sampled_from(
    [
        "(?=a)",
        "(?!a)",
        "\\1",
        "(?i:a)",
        "a{",
        "{",
        "}",
        "]",
        "(?#x)",
        "\\0",
        "\\N{DASH}",
        "é",
        "[[a]",
        "a**",
        "$a",
        "(a$)",
    ]
)


def atoms(children):
    groups = st.builds(
        lambda kind, body: kind + body + ")",
        st.sampled_from(["(", "(?:", "(?P<g>"]),
        children,
    )
    return st.builds(
        lambda atom, quantifier: atom + quantifier,
        st.one_of(literals, literals, shorthands, classes, groups),
        quantifiers,
    )


patterns = st.recursive(
    st.builds(
        lambda atom, quantifier: atom + quantifier,
        st.one_of(literals, shorthands, classes),
        quantifiers,
    ),
    lambda children: st.one_of(
        st.lists(st.one_of(children, atoms(children)), min_size=1, max_size=4).map("".join),
        st.lists(children, min_size=2, max_size=3).map("|".join),
        st.builds(lambda a, b: a + b, children, foreign),
    ),
    max_leaves=8,
)
flags = st.sampled_from(["", "", "", "(?i)", "(?s)", "(?is)", "(?a)", "(?m)", "(?x)"])
full_patterns = st.builds(
    lambda flag, pattern, dollar: flag + pattern + dollar,
    flags,
    patterns,
    st.sampled_from(["", "$", "$"]),
)
subjects = st.text(alphabet=ALPHABET, max_size=8)


def check(pattern, subject):
    fast = _core.regex_fast_match(pattern, subject)
    if fast is None:
        return False
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        # must compile: an accepted pattern may neither be an error nor warn
        compiled = re.compile(pattern)
    expected = compiled.match(subject) is not None
    assert fast == expected, "pattern %r, subject %r: re says %s; translation %r" % (
        pattern,
        subject,
        expected,
        _core.regex_fast_translate(pattern),
    )
    return True


@given(pattern=full_patterns, subject=subjects)
def test_generated_patterns(pattern, subject):
    check(pattern, subject)


@given(
    pattern=st.text(alphabet="ab01[](){}|*+?^$\\.-dws,:PZA<>=! \n", max_size=12), subject=subjects
)
def test_arbitrary_patterns(pattern, subject):
    check(pattern, subject)


@given(subject=st.text(alphabet=st.characters(max_codepoint=127), max_size=6))
def test_character_classes_agree_on_all_of_ascii(subject):
    for pattern in (
        r"\s*$",
        r"\S*$",
        r"\w*$",
        r"\W*$",
        r"\d*$",
        r"\D*$",
        r".*$",
        r"(?s).*$",
        r"(?i)[a-z]*$",
        r"[^\W\d]*$",
    ):
        assert check(pattern, subject)


@pytest.mark.parametrize(
    "pattern,subjects",
    [
        (
            r"[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+$",
            ["x@y.zz", "x@y", "x@y.zz\n", "x@y.zz\n\n", " x@y.zz"],
        ),
        (r"\d{4}-\d{2}-\d{2}$", ["2020-01-02", "2020-01-02\n", "2020-1-02", "2020-01-02 "]),
        (r"^[a-z0-9]+(?:-[a-z0-9]+)*$", ["a-b-c", "a--b", "-a", "a-"]),
        (r"a|b$", ["a", "ab", "b", "ba", "b\n"]),
        (r"abc\$", ["abc$", "abc$def", "abc"]),
        (r"(?i)(alpha|beta|gamma)\w*$", ["Gamma1", "delta", "ALPHA"]),
        (r"$", ["", "\n", "a", "\n\n"]),
    ],
)
def test_examples(pattern, subjects):
    for subject in subjects:
        assert check(pattern, subject), (pattern, subject)


def test_unsupported_patterns_are_declined():
    for pattern in (r"(\w+) \1$", r"(?=a)\w+$", r"(?m)^a$", r"a{$", "ü+$", r"(a*)*$", r"(?i:a)$"):
        assert _core.regex_fast_translate(pattern) is None, pattern


def test_the_rule_uses_both_paths(ferberus):
    v = ferberus.Validator({"a": {"regex": "[a-z]+"}, "b": {"regex": r"(\w)\1"}})
    assert v.validate({"a": "abc", "b": "aa"})
    assert not v.validate({"a": "abc1", "b": "ab"})
    assert v.errors == {
        "a": ["value does not match regex '[a-z]+'"],
        "b": ["value does not match regex '(\\w)\\1'"],
    }
    # non-ASCII subjects always go to `re`
    assert not v.validate({"a": "äbc"})
    previous = _core.set_regex_fast(False)
    try:
        assert v.validate({"a": "abc", "b": "aa"})
        assert not v.validate({"a": "abc1"})
    finally:
        _core.set_regex_fast(previous)
