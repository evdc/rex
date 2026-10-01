//! Generated queries. The other suites drive fixed programs with random
//! events; this one makes up the *queries* too.
//!
//! A small typed grammar produces relation expressions over a fixed schema —
//! field paths, joins through a foreign key and back, arithmetic, text,
//! comparisons, `where`/`not`/`in`, union/intersect/except, semijoin,
//! `if`/`match`, forks and projections, aggregates grouped by a field or by
//! `unit`. The grammar is looser than the type system, so the checker is the
//! filter: whatever it accepts is added to the program as a view. Then a
//! random event history runs, and after every step each generated view must
//! equal batch evaluation of its own body, with deltas to match — and the
//! whole thing must replay and restore.
//!
//! Three things are under test at once: the checker never panics on anything
//! the grammar says, lowering never panics on anything the checker accepts,
//! and incremental maintenance of every accepted query is exact.

mod common;

use common::{check_history, Op};
use proptest::prelude::*;

const SCHEMA: &str = r#"
type Tag = Red | Green | Blue
entity P { n: Int, m: Money, s: Text, b: Bool, t: Tag }
entity C { n: Int, s: Text, b: Bool, p: P }

event NewP(n: Int, m: Money, s: Text, b: Bool, t: Tag)
event NewC(n: Int, s: Text, b: Bool, p: P)
event SetPn(p: P, n: Int)
event SetPm(p: P, m: Money)
event SetPs(p: P, s: Text)
event SetPb(p: P, b: Bool)
event SetPt(p: P, t: Tag)
event SetCn(c: C, n: Int)
event SetCs(c: C, s: Text)
event SetCb(c: C, b: Bool)
event SetCp(c: C, p: P)
event DelP(p: P)
event DelC(c: C)
event BumpP()
event FlipC()
event Reparent(p: P)
event DropTagged(t: Tag)

on NewP(n, m, s, b, t) => new P { n: n, m: m, s: s, b: b, t: t }
on NewC(n, s, b, p)    => new C { n: n, s: s, b: b, p: p }
on SetPn(p, n) => p.n := n
on SetPm(p, m) => p.m := m
on SetPs(p, s) => p.s := s
on SetPb(p, b) => p.b := b
on SetPt(p, t) => p.t := t
on SetCn(c, n) => c.n := n
on SetCs(c, s) => c.s := s
on SetCb(c, b) => c.b := b
on SetCp(c, p) => c.p := p
on DelP(p) => delete p
on DelC(c) => delete c
on BumpP()        => update P where .n > 2 { n: .n + 1 }
on FlipC()        => update C where .b { b: False }
on Reparent(p)    => update C where .n < 3 { p: p }
on DropTagged(t)  => delete P where .t = t

let p0 = new P { n: 1, m: 1.50, s: "a", b: True,  t: Red }
let p1 = new P { n: 4, m: 0.25, s: "b", b: False, t: Green }
let p2 = new P { n: 4, m: 9.00, s: "a", b: True,  t: Red }
let c0 = new C { n: 2, s: "a", b: True,  p: p0 }
let c1 = new C { n: 5, s: "c", b: False, p: p0 }
let c2 = new C { n: 2, s: "",  b: True,  p: p1 }

let pn : P -> Int   = .n
let pm : P -> Money = .m
let cn : C -> Int   = .n
let cp : C -> P     = .p
"#;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ty {
    P,
    C,
    Int,
    Money,
    Text,
    Bool,
    Tag,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::P => "P",
            Ty::C => "C",
            Ty::Int => "Int",
            Ty::Money => "Money",
            Ty::Text => "Text",
            Ty::Bool => "Bool",
            Ty::Tag => "Tag",
        }
    }
}

/// Consumes a list of raw choices; out of choices means "take the first
/// alternative", so every expression ends, and shrinking a choice toward 0
/// shrinks the expression toward a leaf.
struct Gen<'a> {
    picks: &'a [u32],
    at: usize,
}

impl Gen<'_> {
    fn pick(&mut self, n: usize) -> usize {
        let p = self.picks.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        p as usize % n
    }

    fn one<'s>(&mut self, of: &[&'s str]) -> &'s str {
        of[self.pick(of.len())]
    }

    /// A relation `e -> to`, written with `e`'s row as the ambient domain.
    fn value(&mut self, e: Ty, to: Ty, depth: u32) -> String {
        let leaf = depth == 0;
        let d = depth.saturating_sub(1);
        match to {
            Ty::Int => match (if leaf { self.pick(3) } else { self.pick(11) }, e) {
                (0, _) => ".n".into(),
                (1, _) => self.one(&["0", "1", "2", "3", "7", "-1", "100"]).into(),
                (2, Ty::C) => ".p.n".into(),
                (2, _) => "count(C by .p)".into(),
                (3, _) => format!("({} {} {})", self.value(e, Ty::Int, d), self.one(&["+", "-", "*"]), self.value(e, Ty::Int, d)),
                (4, _) => format!("({} {} {})", self.value(e, Ty::Int, d), self.one(&["/", "%"]), self.value(e, Ty::Int, d)),
                (5, _) => format!("(if {} then {} else {})", self.pred(e, d), self.value(e, Ty::Int, d), self.value(e, Ty::Int, d)),
                (6, Ty::P) => format!("{}(cn by .p)", self.one(&["sum", "min", "max", "count"])),
                (6, _) => format!("(.p . {})", self.value(Ty::P, Ty::Int, d)),
                (7, Ty::P) => format!("(match .t {{ Red => {}, _ => {} }})", self.value(e, Ty::Int, d), self.value(e, Ty::Int, d)),
                (7, _) => "(.p . pn)".into(),
                (8, _) => format!("fst ({} , {})", self.value(e, Ty::Int, d), self.value(e, Ty::Text, d)),
                (9, _) => format!("({} . {})", self.keyset(e, d), self.value(e, Ty::Int, d)),
                _ => format!("snd ({} , {})", self.value(e, Ty::Bool, d), self.value(e, Ty::Int, d)),
            },
            Ty::Money => match (if leaf { self.pick(2) } else { self.pick(7) }, e) {
                (0, Ty::P) => ".m".into(),
                (0, _) => ".p.m".into(),
                (1, _) => self.one(&["0.00", "2.50", "10.00"]).into(),
                (2, _) => format!("({} {} {})", self.value(e, Ty::Money, d), self.one(&["+", "-"]), self.value(e, Ty::Money, d)),
                (3, _) => format!("({} * {})", self.value(e, Ty::Money, d), self.value(e, Ty::Int, d)),
                (4, _) => format!("({} * {})", self.value(e, Ty::Int, d), self.value(e, Ty::Money, d)),
                (5, Ty::P) => "avg(cn by .p)".into(),
                (5, _) => format!("(.p . {})", self.value(Ty::P, Ty::Money, d)),
                _ => format!("(if {} then {} else {})", self.pred(e, d), self.value(e, Ty::Money, d), self.value(e, Ty::Money, d)),
            },
            Ty::Text => match (if leaf { self.pick(2) } else { self.pick(6) }, e) {
                (0, _) => ".s".into(),
                (1, _) => self.one(&["\"\"", "\"a\"", "\"x,y\"", "\"(\""]).into(),
                (2, _) => format!("({} ++ {})", self.value(e, Ty::Text, d), self.value(e, Ty::Text, d)),
                (3, Ty::C) => ".p.s".into(),
                (3, _) => format!("(match .t {{ Red => {}, Green => \"g\", _ => {} }})", self.value(e, Ty::Text, d), self.value(e, Ty::Text, d)),
                (4, _) => format!("(if {} then {} else {})", self.pred(e, d), self.value(e, Ty::Text, d), self.value(e, Ty::Text, d)),
                _ => format!("({} . {})", self.keyset(e, d), self.value(e, Ty::Text, d)),
            },
            Ty::Bool => match (self.pick(2), e) {
                (0, _) => ".b".into(),
                (_, Ty::C) => ".p.b".into(),
                _ => ".b".into(),
            },
            Ty::Tag => (if e == Ty::P { ".t" } else { ".p.t" }).into(),
            Ty::P if e == Ty::C => match self.pick(2) {
                0 => ".p".into(),
                _ => format!("(.p . {})", self.keyset(Ty::P, d)),
            },
            Ty::C if e == Ty::P => match self.pick(3) {
                0 => "~cp".into(),
                1 => format!("~({} . .p)", self.keyset(Ty::C, d)),
                _ => format!("(~cp . {})", self.keyset(Ty::C, d)),
            },
            // `e -> e`: a keyset.
            _ => self.keyset(e, depth),
        }
    }

    /// A predicate on `e`'s rows, for filter position.
    fn pred(&mut self, e: Ty, depth: u32) -> String {
        let d = depth.saturating_sub(1);
        let cmp = ["=", "!=", "<", "<=", ">", ">="];
        match (if depth == 0 { self.pick(4) } else { self.pick(10) }, e) {
            (0, _) => ".b".into(),
            (1, _) => format!(".n {} {}", self.one(&cmp), self.one(&["0", "2", "4", "-1"])),
            (2, _) => format!(".s {} {}", self.one(&["=", "!=", "<", ">"]), self.one(&["\"a\"", "\"\"", "\"b\""])),
            (3, Ty::P) => format!(".t {}", self.one(&["= Red", "!= Green", "in (Red | Blue)"])),
            (3, _) => format!(".p.t {}", self.one(&["= Red", "in (Green | Blue)"])),
            (4, _) => format!("not {}", self.pred(e, d)),
            (5, _) => format!("{} {} {}", self.value(e, Ty::Int, d), self.one(&cmp), self.value(e, Ty::Int, d)),
            (6, _) => format!("({} {} {})", self.pred(e, d), self.one(&["|", "&"]), self.pred(e, d)),
            (7, _) => format!(".n in ({})", self.one(&["1 | 2", "4", "2 | 4 | 5"])),
            (8, Ty::P) => format!("{} {} {}", self.value(e, Ty::Money, d), self.one(&cmp), self.one(&["1.00", "2", ".m"])),
            (8, _) => format!("{} = {}", self.value(e, Ty::Text, d), self.value(e, Ty::Text, d)),
            _ => format!("{} > 0", self.value(e, Ty::Int, d)),
        }
    }

    /// A coreflexive `e -> e`: a set of `e`'s rows.
    fn keyset(&mut self, e: Ty, depth: u32) -> String {
        let name = e.name();
        let d = depth.saturating_sub(1);
        match (if depth == 0 { self.pick(2) } else { self.pick(11) }, e) {
            (0, _) => name.into(),
            (1, _) => format!("({name} where {})", self.pred(e, d)),
            (2, _) => format!("({} {} {})", self.keyset(e, d), self.one(&["|", "&", "except"]), self.keyset(e, d)),
            (3, _) => format!("{name}[{}]", self.pred(e, d)),
            (4, _) => format!("(id where {})", self.pred(e, d)),
            (5, _) => format!("distinct ({} | {})", self.keyset(e, d), self.keyset(e, d)),
            (6, Ty::C) => format!("C[(.p) . {}]", self.keyset(Ty::P, d)),
            (6, _) => format!("P[~cp . {}]", self.keyset(Ty::C, d)),
            (7, Ty::C) => format!("C[(.p) except {}]", self.keyset(Ty::P, d)),
            (7, _) => format!("(P except ({} . ~cp . cp))", self.keyset(Ty::P, d)),
            (8, _) => format!("({name} where not {})", self.pred(e, d)),
            (9, _) => format!("(match .b {{ True => {}, _ => {} }})", self.keyset(e, d), self.keyset(e, d)),
            _ => format!("({} . {})", self.keyset(e, d), self.keyset(e, d)),
        }
    }

    /// One whole `let`, with its annotation.
    fn query(&mut self, name: &str, depth: u32) -> String {
        let e = if self.pick(2) == 0 { Ty::P } else { Ty::C };
        let other = if e == Ty::P { Ty::C } else { Ty::P };
        let scalar = [Ty::Int, Ty::Money, Ty::Text, Ty::Bool, Ty::Tag];
        match self.pick(9) {
            0 => format!("let {name} : {} = {}", e.name(), self.keyset(e, depth)),
            1 | 2 => {
                let to = scalar[self.pick(scalar.len())];
                format!("let {name} : {} -> {} = {}", e.name(), to.name(), self.value(e, to, depth))
            }
            3 => format!("let {name} : {} -> {} = {}", e.name(), other.name(), self.value(e, other, depth)),
            // Regroup by a field, or under the one `Unit` point.
            4 => {
                let agg = self.one(&["count", "sum", "min", "max", "avg"]);
                let to = if agg == "avg" { "Money" } else { "Int" };
                let image = if e == Ty::P { "pn" } else { "cn" };
                format!("let {name} : Unit -> {to} = {agg}({image} by unit)")
            }
            5 => format!("let {name} : Unit -> Int = count({} by unit)", self.keyset(e, depth)),
            6 => {
                let by = self.one(&[".t", ".s", ".n", ".b"]);
                let key = match by {
                    ".t" => "Tag",
                    ".s" => "Text",
                    ".n" => "Int",
                    _ => "Bool",
                };
                let by = if e == Ty::C && by == ".t" { ".p.t" } else { by };
                format!("let {name} : {key} -> Int = count({} by {by})", self.keyset(e, depth))
            }
            7 => format!(
                "let {name} : {} -> Int * Text = ({} , {})",
                e.name(),
                self.value(e, Ty::Int, depth),
                self.value(e, Ty::Text, depth)
            ),
            // No annotation: the checker infers from the leading entity.
            _ => format!("let {name} = {} . {}", self.keyset(e, depth), self.value(e, Ty::Int, depth)),
        }
    }
}

/// `SCHEMA` plus every generated `let` the checker accepts, in order — a
/// later query may name an earlier one only by accident, so each is checked
/// against the program so far. Returns the program and how many were kept.
fn program(picks: &[u32], want: usize) -> (String, usize, Vec<String>) {
    let mut g = Gen { picks, at: 0 };
    let mut src = SCHEMA.to_string();
    let (mut kept, mut rejected) = (0, Vec::new());
    for i in 0..want {
        let depth = 1 + g.pick(3) as u32;
        let q = g.query(&format!("q{i}"), depth);
        let candidate = format!("{src}{q}\n");
        match common::check(&candidate) {
            Ok(_) => {
                src = candidate;
                kept += 1;
            }
            Err(e) => rejected.push(format!("{q}\n    {}", e.lines().next().unwrap_or(""))),
        }
    }
    (src, kept, rejected)
}

fn op() -> impl Strategy<Value = Op> {
    (any::<usize>(), prop::collection::vec(any::<usize>(), 8)).prop_map(|(event, picks)| Op { event, picks })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(160), ..ProptestConfig::default() })]

    #[test]
    fn accepted_queries_are_maintained_exactly(
        picks in prop::collection::vec(any::<u32>(), 60..240),
        ops in prop::collection::vec(op(), 1..18),
        cut in 0usize..18,
    ) {
        let (src, _, _) = program(&picks, 6);
        check_history(&src, &ops, cut.min(ops.len())).map_err(|e| TestCaseError::fail(format!("{e}\n--- program ---\n{}", &src[SCHEMA.len()..])))?;
    }
}

/// The grammar earns its keep only if the checker accepts a fair share of
/// what it writes, across every production. This pins a floor, and with
/// `--nocapture` prints the rate and the commonest rejections — the thing to
/// look at when the grammar or the checker changes.
#[test]
fn the_grammar_mostly_checks() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u32
    };
    let (mut kept, mut total) = (0, 0);
    let mut reasons: std::collections::HashMap<String, (usize, String)> = Default::default();
    for _ in 0..400 {
        let picks: Vec<u32> = (0..200).map(|_| next()).collect();
        let (_, k, rejected) = program(&picks, 6);
        kept += k;
        total += 6;
        for r in rejected {
            let (q, why) = r.split_once("\n    ").unwrap();
            // Bucket by message with the specifics (backticked names) removed.
            let bucket: String = why.split('`').step_by(2).collect::<Vec<_>>().join("…");
            let e = reasons.entry(bucket).or_insert((0, q.to_string()));
            e.0 += 1;
            if q.len() < e.1.len() {
                e.1 = q.to_string();
            }
        }
    }
    let mut top: Vec<_> = reasons.into_iter().collect();
    top.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    println!("accepted {kept} of {total} generated queries ({}%)", kept * 100 / total);
    for (why, (n, example)) in top.iter().take(25) {
        println!("{n:5}  {why}\n         e.g. {example}");
    }
    assert!(kept * 100 / total >= 50, "only {kept} of {total} generated queries check");
}
