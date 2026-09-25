//! The typing environment: sorts (one per entity), entity field tables, and
//! `let` bindings.

use super::ty::{RelTy, SortId, ValueTy};
use std::collections::HashMap;

/// A `let` binding is either a view (a relation) or a bound value (an entity id
/// introduced by `new`, §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Rel(RelTy),
    Value(ValueTy),
}

#[derive(Debug, Default)]
pub struct Env {
    /// SortId index -> sort name (e.g. "CustomerID").
    sort_names: Vec<String>,
    /// Entity name (e.g. "Customer") -> its minted sort.
    entity_sort: HashMap<String, SortId>,
    /// Sort name (e.g. "CustomerID") -> sort. Used to resolve type annotations.
    sort_of_name: HashMap<String, SortId>,
    /// (sort index, field name) -> the field's value type.
    fields: HashMap<(usize, String), ValueTy>,
    /// Ordered field names per sort (for completeness checks / diagnostics).
    field_order: HashMap<usize, Vec<String>>,
    /// `let` name -> binding.
    bindings: HashMap<String, Binding>,
    /// `type Name = A | B` -> its constructors, in declaration order (S-50).
    type_ctors: HashMap<String, Vec<String>>,
    /// Constructor name -> the type that declared it. A constructor is an
    /// ordinary atom whose name is the constructor's, written verbatim, so
    /// `type Filter = All | ...` makes `All` mean `@All`. This table is what
    /// lets a bare `All` in expression position resolve.
    ctor_type: HashMap<String, String>,
}

impl Env {
    pub fn new() -> Env {
        let mut env = Env::default();
        // `Bool` is sugar for a two-constructor type (MVP-PLAN §5 decision 3),
        // predeclared so `not` and `class.x=` have a type to work against.
        env.declare_type("Bool", &["True".to_string(), "False".to_string()]);
        env
    }

    /// Declare `type name = ctors...`. Returns the constructor that clashes
    /// with an already-declared one, if any (the caller reports it: a shared
    /// constructor would make a bare `All` ambiguous).
    pub fn declare_type(&mut self, name: &str, ctors: &[String]) -> Option<(String, String)> {
        for c in ctors {
            if let Some(owner) = self.ctor_type.get(c)
                && owner != name
            {
                return Some((c.clone(), owner.clone()));
            }
        }
        for c in ctors {
            self.ctor_type.insert(c.clone(), name.to_string());
        }
        self.type_ctors.insert(name.to_string(), ctors.to_vec());
        None
    }

    /// The constructors of a declared `type`, if `name` is one.
    pub fn type_ctors(&self, name: &str) -> Option<&[String]> {
        self.type_ctors.get(name).map(|v| v.as_slice())
    }

    /// The type that declared constructor `ctor`, if any.
    pub fn ctor_owner(&self, ctor: &str) -> Option<&str> {
        self.ctor_type.get(ctor).map(|s| s.as_str())
    }

    pub fn is_type_name(&self, name: &str) -> bool {
        self.type_ctors.contains_key(name)
    }

    /// Mint a fresh ID-sort for `entity`. Its sort is named `<Entity>ID`.
    pub fn mint_sort(&mut self, entity: &str) -> SortId {
        let id = SortId(self.sort_names.len());
        let sort_name = format!("{entity}ID");
        self.sort_names.push(sort_name.clone());
        self.entity_sort.insert(entity.to_string(), id);
        self.sort_of_name.insert(sort_name, id);
        id
    }

    pub fn sort_name(&self, sort: SortId) -> &str {
        &self.sort_names[sort.0]
    }

    /// Resolve an entity name (`Customer`) or a sort name (`CustomerID`) to its sort.
    pub fn resolve_sort(&self, name: &str) -> Option<SortId> {
        self.entity_sort
            .get(name)
            .or_else(|| self.sort_of_name.get(name))
            .copied()
    }

    pub fn is_entity(&self, name: &str) -> bool {
        self.entity_sort.contains_key(name)
    }

    pub fn entity_sort(&self, name: &str) -> Option<SortId> {
        self.entity_sort.get(name).copied()
    }

    pub fn add_field(&mut self, sort: SortId, field: &str, ty: ValueTy) {
        self.fields.insert((sort.0, field.to_string()), ty);
        self.field_order
            .entry(sort.0)
            .or_default()
            .push(field.to_string());
    }

    pub fn field_ty(&self, sort: SortId, field: &str) -> Option<&ValueTy> {
        self.fields.get(&(sort.0, field.to_string()))
    }

    pub fn bind(&mut self, name: &str, binding: Binding) {
        self.bindings.insert(name.to_string(), binding);
    }

    pub fn binding(&self, name: &str) -> Option<&Binding> {
        self.bindings.get(name)
    }

    /// All `let`-bound names and their bindings (for REPL introspection).
    pub fn bindings(&self) -> impl Iterator<Item = (&String, &Binding)> {
        self.bindings.iter()
    }

    /// All declared entity names (for REPL introspection).
    pub fn entities(&self) -> impl Iterator<Item = &String> {
        self.entity_sort.keys()
    }

    /// Human-readable rendering of a value type for diagnostics.
    pub fn show(&self, ty: &ValueTy) -> String {
        match ty {
            ValueTy::Unit => "Unit".into(),
            ValueTy::Int => "Int".into(),
            ValueTy::Money => "Money".into(),
            ValueTy::Text => "Text".into(),
            ValueTy::Date => "Date".into(),
            ValueTy::Id(s) => self.sort_name(*s).to_string(),
            ValueTy::Atom(a) => format!("@{a}"),
            ValueTy::Product(a, b) => format!("({} * {})", self.show(a), self.show(b)),
            ValueTy::Coproduct(elems) => {
                let inner = elems
                    .iter()
                    .map(|e| self.show(e))
                    .collect::<Vec<_>>()
                    .join(" + ");
                format!("{{{inner}}}")
            }
        }
    }

    pub fn show_rel(&self, rt: &RelTy) -> String {
        format!("{} -> {}", self.show(&rt.from), self.show(&rt.to))
    }
}
