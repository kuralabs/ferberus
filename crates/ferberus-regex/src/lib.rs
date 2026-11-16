//! Fast path for the `regex` rule of ferberus.
//!
//! Cerberus evaluates `re.compile(pattern).match(value)`. For ASCII patterns
//! from a conservative subset of Python's syntax, and ASCII subjects, the
//! same question is answered by the `regex` crate. This module translates
//! such a pattern; whatever it does not accept stays with Python's `re`
//! (see DESIGN.md, "Regex").
//!
//! The translator parses the Python syntax itself and emits an explicit
//! pattern for `regex::bytes` with Unicode disabled:
//!
//! * every literal becomes a hex escape, every class an explicit list of
//!   byte ranges (computed here, including case folding and negation), so
//!   no syntax difference between the two engines can change the meaning;
//! * `$` is only accepted as the last element of a top-level alternative and
//!   becomes `\n?\z`; `^`/`\A` become `\A`, `\Z` becomes `\z`;
//! * rejected: back-references, look-around, conditionals, atomic groups,
//!   possessive quantifiers, comments, flags other than leading `i`/`s`/`a`/
//!   `u`, octal and Unicode escapes, `{` that is not a quantifier, constructs
//!   for which Python warns, and repetition of groups that can match the
//!   empty string (where `sre`'s loop guards are not obviously regular).
//!
//! The question asked is boolean and anchored at the start, so leftmost-first
//! versus backtracking priority cannot make a difference for this subset.

pub use regex::Error as BuildError;
use regex::bytes::{Regex, RegexBuilder};

/// A set of ASCII characters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Set(u128);

impl Set {
    const EMPTY: Set = Set(0);

    fn single(c: u8) -> Set {
        Set(1 << c)
    }

    fn range(lo: u8, hi: u8) -> Set {
        let mut set = 0u128;
        for c in lo..=hi {
            set |= 1 << c;
        }
        Set(set)
    }

    fn union(self, other: Set) -> Set {
        Set(self.0 | other.0)
    }

    fn negate(self) -> Set {
        Set(!self.0)
    }

    fn contains(self, c: u8) -> bool {
        self.0 & (1 << c) != 0
    }

    /// Adds the other case of every letter (`re.IGNORECASE` on ASCII).
    fn fold_case(self) -> Set {
        let mut set = self;
        for c in b'a'..=b'z' {
            let upper = c.to_ascii_uppercase();
            if self.contains(c) || self.contains(upper) {
                set = set.union(Set::single(c)).union(Set::single(upper));
            }
        }
        set
    }

    fn digits() -> Set {
        Set::range(b'0', b'9')
    }

    fn word() -> Set {
        Set::digits().union(Set::range(b'a', b'z')).union(Set::range(b'A', b'Z')).union(Set::single(b'_'))
    }

    /// `str.isspace()` for ASCII: includes the separators 0x1c-0x1f.
    fn space() -> Set {
        Set::range(b'\t', b'\r').union(Set::single(b' ')).union(Set::range(0x1c, 0x1f))
    }

    /// The set as a `regex` class of explicit byte ranges.
    fn emit(self, out: &mut String) {
        if self == Set::EMPTY {
            // matches nothing
            out.push_str("[^\\x00-\\xFF]");
            return;
        }
        out.push('[');
        let mut c = 0u16;
        while c < 128 {
            if self.contains(c as u8) {
                let start = c;
                while c + 1 < 128 && self.contains((c + 1) as u8) {
                    c += 1;
                }
                if start == c {
                    out.push_str(&format!("\\x{start:02X}"));
                } else {
                    out.push_str(&format!("\\x{start:02X}-\\x{c:02X}"));
                }
            }
            c += 1;
        }
        out.push(']');
    }
}

struct Translator<'a> {
    input: &'a [u8],
    pos: usize,
    out: String,
    ignore_case: bool,
    dot_all: bool,
    group_names: Vec<&'a [u8]>,
    depth: usize,
}

/// Why a pattern is left to Python's `re`.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsupported;

type Translation<T> = Result<T, Unsupported>;

/// Repetition counts above this are left to Python (the automaton would be
/// large; `regex` may also refuse to build it).
const MAX_REPEAT: u32 = 255;
const MAX_DEPTH: usize = 32;

impl<'a> Translator<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn eat(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.input[self.pos..].starts_with(prefix)
    }

    /// Leading global flags: `(?i)`, `(?s)`, `(?is)`, ...
    fn flags(&mut self) -> Translation<()> {
        while self.starts_with(b"(?") {
            let Some(end) = self.input[self.pos..].iter().position(|&c| c == b')') else {
                return Err(Unsupported);
            };
            let flags = &self.input[self.pos + 2..self.pos + end];
            if flags.is_empty() || !flags.iter().all(|c| b"isau".contains(c)) {
                // not a flag group (or flags that change the semantics)
                return if flags.iter().all(|c| c.is_ascii_alphabetic()) && !flags.is_empty() {
                    Err(Unsupported)
                } else {
                    Ok(())
                };
            }
            // 'a' and 'u' are mutually exclusive in Python
            if flags.contains(&b'a') && flags.contains(&b'u') {
                return Err(Unsupported);
            }
            self.ignore_case |= flags.contains(&b'i');
            self.dot_all |= flags.contains(&b's');
            self.pos += end + 1;
        }
        Ok(())
    }

    /// `branch ('|' branch)*`; returns whether the alternation can match the
    /// empty string.
    fn alternation(&mut self, top_level: bool) -> Translation<bool> {
        let mut nullable = false;
        loop {
            nullable |= self.branch(top_level)?;
            if self.eat(b'|') {
                self.out.push('|');
            } else {
                return Ok(nullable);
            }
        }
    }

    fn branch(&mut self, top_level: bool) -> Translation<bool> {
        let mut nullable = true;
        loop {
            match self.peek() {
                None | Some(b'|') => return Ok(nullable),
                Some(b')') => return if top_level { Err(Unsupported) } else { Ok(nullable) },
                Some(b'$') => {
                    self.pos += 1;
                    // only as the very last element of a top-level alternative
                    if !top_level || !matches!(self.peek(), None | Some(b'|')) {
                        return Err(Unsupported);
                    }
                    self.out.push_str("(?:\\x0A?\\z)");
                }
                Some(_) => {
                    let start = self.out.len();
                    let (atom_nullable, repeatable) = self.atom()?;
                    let repeated_nullable = self.quantifier(start, atom_nullable, repeatable)?;
                    nullable &= repeated_nullable;
                }
            }
        }
    }

    /// Parses one atom into `self.out`; returns (can match empty, may be
    /// quantified).
    fn atom(&mut self) -> Translation<(bool, bool)> {
        let c = self.peek().ok_or(Unsupported)?;
        self.pos += 1;
        match c {
            b'.' => {
                if self.dot_all {
                    self.out.push_str("[\\x00-\\x7F]");
                } else {
                    Set::single(b'\n').negate().emit(&mut self.out);
                }
                Ok((false, true))
            }
            b'^' => {
                self.out.push_str("\\A");
                Ok((true, false))
            }
            b'[' => {
                let set = self.class()?;
                set.emit(&mut self.out);
                Ok((false, true))
            }
            b'(' => self.group(),
            b'\\' => self.escape(),
            // quantifiers without operand, unbalanced or special characters
            b'*' | b'+' | b'?' | b'{' | b')' | b'|' | b'$' => Err(Unsupported),
            literal => {
                self.literal(literal);
                Ok((false, true))
            }
        }
    }

    fn literal(&mut self, c: u8) {
        let mut set = Set::single(c);
        if self.ignore_case {
            set = set.fold_case();
        }
        if set == Set::single(c) {
            self.out.push_str(&format!("\\x{c:02X}"));
        } else {
            set.emit(&mut self.out);
        }
    }

    fn group(&mut self) -> Translation<(bool, bool)> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Unsupported);
        }
        if self.eat(b'?') {
            if self.eat(b':') {
                // non-capturing
            } else if self.starts_with(b"P<") {
                self.pos += 2;
                let start = self.pos;
                while self.peek().is_some_and(|c| c != b'>') {
                    self.pos += 1;
                }
                let name = &self.input[start..self.pos];
                if !self.eat(b'>') {
                    return Err(Unsupported);
                }
                let valid = name.first().is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                    && name.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_');
                if !valid || self.group_names.contains(&name) {
                    return Err(Unsupported);
                }
                self.group_names.push(name);
            } else {
                // look-around, conditionals, comments, scoped flags, ...
                return Err(Unsupported);
            }
        }
        self.out.push_str("(?:");
        let nullable = self.alternation(false)?;
        if !self.eat(b')') {
            return Err(Unsupported);
        }
        self.out.push(')');
        self.depth -= 1;
        Ok((nullable, true))
    }

    /// An escape outside of a class.
    fn escape(&mut self) -> Translation<(bool, bool)> {
        let c = self.peek().ok_or(Unsupported)?;
        self.pos += 1;
        let set = match c {
            b'A' => {
                self.out.push_str("\\A");
                return Ok((true, false));
            }
            b'Z' => {
                self.out.push_str("\\z");
                return Ok((true, false));
            }
            b'b' => {
                self.out.push_str("\\b");
                return Ok((true, false));
            }
            b'B' => {
                self.out.push_str("\\B");
                return Ok((true, false));
            }
            b'd' => Set::digits(),
            b'D' => Set::digits().negate(),
            b'w' => Set::word(),
            b'W' => Set::word().negate(),
            b's' => Set::space(),
            b'S' => Set::space().negate(),
            other => {
                let literal = self.escaped_literal(other, false)?;
                self.literal(literal);
                return Ok((false, true));
            }
        };
        set.emit(&mut self.out);
        Ok((false, true))
    }

    /// The character an escape sequence stands for.
    fn escaped_literal(&mut self, c: u8, in_class: bool) -> Translation<u8> {
        Ok(match c {
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'f' => 0x0c,
            b'v' => 0x0b,
            b'a' => 0x07,
            b'b' if in_class => 0x08,
            b'x' => {
                let digits = self.input.get(self.pos..self.pos + 2).ok_or(Unsupported)?;
                let text = std::str::from_utf8(digits).map_err(|_| Unsupported)?;
                if !digits.iter().all(u8::is_ascii_hexdigit) {
                    return Err(Unsupported);
                }
                let value = u8::from_str_radix(text, 16).map_err(|_| Unsupported)?;
                if value >= 0x80 {
                    return Err(Unsupported);
                }
                self.pos += 2;
                value
            }
            // other letters are errors or Unicode features, digits are
            // back-references or octal escapes
            c if c.is_ascii_alphanumeric() => return Err(Unsupported),
            c if c.is_ascii() => c,
            _ => return Err(Unsupported),
        })
    }

    /// `[...]`, the opening bracket is consumed.
    fn class(&mut self) -> Translation<Set> {
        let negated = self.eat(b'^');
        let mut set = Set::EMPTY;
        let mut first = true;
        loop {
            let c = self.peek().ok_or(Unsupported)?;
            self.pos += 1;
            if c == b']' && !first {
                break;
            }
            first = false;
            // constructs Python warns about ("possible nested set", "possible
            // set difference", ...)
            if c == b'[' || (matches!(c, b'-' | b'&' | b'~' | b'|') && self.peek() == Some(c)) {
                return Err(Unsupported);
            }
            let lo = if c == b'\\' {
                let escaped = self.peek().ok_or(Unsupported)?;
                self.pos += 1;
                let shorthand = match escaped {
                    b'd' => Some(Set::digits()),
                    b'D' => Some(Set::digits().negate()),
                    b'w' => Some(Set::word()),
                    b'W' => Some(Set::word().negate()),
                    b's' => Some(Set::space()),
                    b'S' => Some(Set::space().negate()),
                    _ => None,
                };
                if let Some(shorthand) = shorthand {
                    // as the start of a range it is an error in Python
                    if self.peek() == Some(b'-') && self.input.get(self.pos + 1) != Some(&b']') {
                        return Err(Unsupported);
                    }
                    set = set.union(shorthand);
                    continue;
                }
                self.escaped_literal(escaped, true)?
            } else {
                c
            };
            if !lo.is_ascii() {
                return Err(Unsupported);
            }
            // a range?
            if self.peek() == Some(b'-') && self.input.get(self.pos + 1).is_some_and(|&next| next != b']') {
                self.pos += 1;
                let c = self.peek().ok_or(Unsupported)?;
                self.pos += 1;
                if c == b'[' {
                    return Err(Unsupported);
                }
                // Python warns about a "possible set difference"
                if c == b'-' {
                    return Err(Unsupported);
                }
                let hi = if c == b'\\' {
                    let escaped = self.peek().ok_or(Unsupported)?;
                    self.pos += 1;
                    if matches!(escaped, b'd' | b'D' | b'w' | b'W' | b's' | b'S') {
                        return Err(Unsupported);
                    }
                    self.escaped_literal(escaped, true)?
                } else {
                    c
                };
                if !hi.is_ascii() || hi < lo {
                    return Err(Unsupported);
                }
                set = set.union(Set::range(lo, hi));
            } else {
                set = set.union(Set::single(lo));
            }
        }
        if self.ignore_case {
            set = set.fold_case();
        }
        Ok(if negated { set.negate() } else { set })
    }

    /// An optional quantifier for the atom emitted at `out[start..]`.
    fn quantifier(&mut self, start: usize, nullable: bool, repeatable: bool) -> Translation<bool> {
        let (min, max): (u32, Option<u32>) = match self.peek() {
            Some(b'*') => (0, None),
            Some(b'+') => (1, None),
            Some(b'?') => (0, Some(1)),
            Some(b'{') => {
                // {m}, {m,}, {,n}, {m,n}; anything else is a literal brace in
                // Python and not supported here
                let rest = &self.input[self.pos + 1..];
                let close = rest.iter().position(|&c| c == b'}').ok_or(Unsupported)?;
                let body = std::str::from_utf8(&rest[..close]).map_err(|_| Unsupported)?;
                let number = |text: &str| -> Translation<u32> {
                    if text.is_empty() || text.len() > 4 || !text.bytes().all(|c| c.is_ascii_digit()) {
                        return Err(Unsupported);
                    }
                    text.parse().map_err(|_| Unsupported)
                };
                let bounds = match body.split_once(',') {
                    None => {
                        let n = number(body)?;
                        (n, Some(n))
                    }
                    Some((lo, hi)) => (
                        if lo.is_empty() { 0 } else { number(lo)? },
                        if hi.is_empty() { None } else { Some(number(hi)?) },
                    ),
                };
                if body == "," {
                    return Err(Unsupported);
                }
                // onto the closing brace, which the common tail consumes
                self.pos += close + 1;
                bounds
            }
            _ => return Ok(nullable),
        };
        self.pos += 1;
        if !repeatable {
            return Err(Unsupported);
        }
        if min > MAX_REPEAT || max.is_some_and(|max| max > MAX_REPEAT || max < min) {
            return Err(Unsupported);
        }
        // repeating something that can match the empty string
        if nullable && max != Some(1) {
            return Err(Unsupported);
        }
        // lazy quantifiers match the same strings
        self.eat(b'?');
        // possessive quantifiers and stacked quantifiers
        if matches!(self.peek(), Some(b'+' | b'*' | b'?' | b'{')) {
            return Err(Unsupported);
        }
        let atom = self.out.split_off(start);
        self.out.push_str("(?:");
        self.out.push_str(&atom);
        self.out.push(')');
        match (min, max) {
            (0, None) => self.out.push('*'),
            (1, None) => self.out.push('+'),
            (0, Some(1)) => self.out.push('?'),
            (min, None) => self.out.push_str(&format!("{{{min},}}")),
            (min, Some(max)) => self.out.push_str(&format!("{{{min},{max}}}")),
        }
        Ok(nullable || min == 0)
    }
}

/// Translates a Python pattern into an equivalent pattern for
/// `regex::bytes` that is anchored at the start.
pub fn translate(pattern: &str) -> Translation<String> {
    if !pattern.is_ascii() || pattern.len() > 2048 {
        return Err(Unsupported);
    }
    let mut translator = Translator {
        input: pattern.as_bytes(),
        pos: 0,
        out: String::with_capacity(pattern.len() * 4 + 16),
        ignore_case: false,
        dot_all: false,
        group_names: Vec::new(),
        depth: 0,
    };
    translator.flags()?;
    translator.out.push_str("\\A(?:");
    translator.alternation(true)?;
    if translator.pos != translator.input.len() {
        return Err(Unsupported);
    }
    translator.out.push(')');
    Ok(translator.out)
}

/// Compiles the output of [`translate`].
pub fn build(translated: &str) -> Result<Regex, BuildError> {
    RegexBuilder::new(translated).unicode(false).size_limit(1 << 20).dfa_size_limit(1 << 20).build()
}

/// The compiled fast path for `pattern`, if it has one.
pub fn compile(pattern: &str) -> Option<Regex> {
    build(&translate(pattern).ok()?).ok()
}

/// Subjects at least this long are matched with the interpreter released
/// (see BENCHMARKS.md for the measurement behind the number).
pub const DETACH_THRESHOLD: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, subject: &str) -> Option<bool> {
        compile(pattern).map(|regex| regex.is_match(subject.as_bytes()))
    }

    #[test]
    fn literals_and_anchors() {
        assert_eq!(matches("abc$", "abc"), Some(true));
        assert_eq!(matches("abc$", "abc\n"), Some(true));
        assert_eq!(matches("abc$", "abc\n\n"), Some(false));
        assert_eq!(matches("abc$", "xabc"), Some(false));
        assert_eq!(matches("abc", "abcdef"), Some(true));
        assert_eq!(matches("a|b$", "axyz"), Some(true));
        assert_eq!(matches("a|b$", "bxyz"), Some(false));
        assert_eq!(matches("^a$|^b$", "b"), Some(true));
        assert_eq!(matches(r"a\Z", "a\n"), Some(false));
    }

    #[test]
    fn classes() {
        assert_eq!(matches(r"\s$", "\x1c"), Some(true));
        assert_eq!(matches(r"[^\s]+$", "a b"), Some(false));
        assert_eq!(matches(r"[]a]+$", "]a]"), Some(true));
        assert_eq!(matches(r"[a\-z]+$", "a-z"), Some(true));
        assert_eq!(matches(r"[a-c-]+$", "ab-"), Some(true));
        assert_eq!(matches(r"(?i)[^a]$", "A"), Some(false));
        assert_eq!(matches(r"(?i)straSSe$", "STRASSE"), Some(true));
        assert_eq!(matches(r".$", "\n"), Some(false));
        assert_eq!(matches(r"(?s).$", "\n"), Some(true));
        assert_eq!(matches(r"\w+@\w+\.\w{2,}$", "x@y.zz"), Some(true));
    }

    #[test]
    fn rejected() {
        for pattern in [
            r"(a)\1",
            r"(?=a)b",
            r"(?<!a)b",
            "a$b",
            "(a$)",
            "(?m)^a$",
            "(?x) a",
            "a{",
            "a{x}",
            "a**",
            "a*+",
            "(a*)*",
            "(?:a|)+",
            "[[a]",
            "[a--b]",
            r"\0",
            r"\N{DASH}",
            "é",
            "(?i:a)",
            "*a",
            "a)",
            "(a",
            "[a",
            r"\q",
            "[z-a]",
            r"(?P<n>a)(?P<n>b)",
            "(?#comment)a",
        ] {
            assert_eq!(translate(pattern), Err(Unsupported), "{pattern}");
        }
    }

    #[test]
    fn quantifiers() {
        assert_eq!(matches(r"\d{4}-\d{2}-\d{2}$", "2020-01-02"), Some(true));
        assert_eq!(matches(r"a{,2}$", "aaa"), Some(false));
        assert_eq!(matches(r"a{2,}?$", "aaa"), Some(true));
        assert_eq!(matches(r"(?:ab)+$", "abab"), Some(true));
        assert_eq!(matches(r"(?P<x>ab)?c$", "c"), Some(true));
    }
}
