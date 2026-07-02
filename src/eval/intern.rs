//! A global string interner for `Text` and `Atom` values and field names.
//!
//! Interning makes `Value` cheap to clone, compare, and hash everywhere the
//! Z-set machinery touches it: a [`Sym`] is a `Copy` `u32`, so BTreeMap keying
//! and equality are integer operations instead of `String` traversals.
//!
//! `Sym`'s derived `Ord` is *interning order* — a total order, deterministic
//! for a given program, which is all Z-set storage needs. It is NOT
//! lexicographic: anything user-visible (surface-language `<`/`>` on text,
//! display) must resolve through [`Sym::as_str`].
//!
//! Interned strings are leaked and live for the process lifetime; the table
//! only grows. That is the intended trade for a long-lived engine whose
//! strings are field names, atoms, and stored text values.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// An interned string. Equal ids ⇔ equal strings.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(u32);

struct Interner {
    map: HashMap<&'static str, u32>,
    strings: Vec<&'static str>,
}

static INTERNER: LazyLock<Mutex<Interner>> =
    LazyLock::new(|| Mutex::new(Interner { map: HashMap::new(), strings: Vec::new() }));

/// Intern a string, returning its symbol. Idempotent.
pub fn intern(s: &str) -> Sym {
    let mut i = INTERNER.lock().unwrap();
    if let Some(&id) = i.map.get(s) {
        return Sym(id);
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    let id = u32::try_from(i.strings.len()).expect("interner overflow");
    i.strings.push(leaked);
    i.map.insert(leaked, id);
    Sym(id)
}

impl Sym {
    /// The interned string. `'static` because the interner leaks.
    pub fn as_str(self) -> &'static str {
        INTERNER.lock().unwrap().strings[self.0 as usize]
    }
}

impl std::fmt::Display for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Debug for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_idempotent_and_distinguishing() {
        assert_eq!(intern("alpha"), intern("alpha"));
        assert_ne!(intern("alpha"), intern("beta"));
        assert_eq!(intern("alpha").as_str(), "alpha");
    }
}
