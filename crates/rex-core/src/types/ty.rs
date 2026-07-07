//! The value algebra (§2) and relation types (§3).

/// Index into the environment's sort table. Each `entity` mints a fresh sort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SortId(pub usize);

/// A value-algebra type: the domain elements range over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueTy {
    Unit,
    Int,
    Money,
    Text,
    Date,
    /// An ID-sort minted by an `entity` (e.g. `CustomerID`).
    Id(SortId),
    /// A singleton atom type `{@foo}`.
    Atom(String),
    Product(Box<ValueTy>, Box<ValueTy>),
    /// A coproduct `{@a + @b + ...}` (or `(V + Unit)`), kept in a canonical
    /// de-duplicated order.
    Coproduct(Vec<ValueTy>),
}

impl ValueTy {
    pub fn is_numeric(&self) -> bool {
        matches!(self, ValueTy::Int | ValueTy::Money)
    }

    /// The atoms this type ranges over, if it is an atom or a coproduct of atoms.
    pub fn atoms(&self) -> Option<Vec<&str>> {
        match self {
            ValueTy::Atom(a) => Some(vec![a.as_str()]),
            ValueTy::Coproduct(elems) => {
                let mut out = Vec::new();
                for e in elems {
                    match e {
                        ValueTy::Atom(a) => out.push(a.as_str()),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }
}

/// Every Rex expression denotes a binary relation `from -> to` (§3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelTy {
    pub from: ValueTy,
    pub to: ValueTy,
}

impl RelTy {
    pub fn new(from: ValueTy, to: ValueTy) -> RelTy {
        RelTy { from, to }
    }

    /// A coreflexive (sub-identity) relation on `v`: `v -> v`.
    pub fn coreflexive(v: ValueTy) -> RelTy {
        RelTy {
            from: v.clone(),
            to: v,
        }
    }
}
