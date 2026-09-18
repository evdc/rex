//! Synthetic TPC-H-shaped dataset generator, reusing the SPEC §12 fixture's
//! schema and queries (Customer/Product/Order/Line, revenue-by-region) at
//! larger scale. Produces Rex *source text*, not `TStmt`s directly, so the
//! generated program goes through the exact same parse/check/elaborate path
//! as any real `.rex` file.

/// A scale tier: row counts for each entity.
#[derive(Clone, Copy)]
pub struct Scale {
    pub customers: usize,
    pub products: usize,
    pub orders: usize,
    pub lines: usize,
}

pub const SMALL: Scale = Scale { customers: 100, products: 50, orders: 300, lines: 1_000 };
pub const MEDIUM: Scale = Scale { customers: 1_000, products: 300, orders: 3_000, lines: 10_000 };
pub const LARGE: Scale = Scale { customers: 5_000, products: 1_000, orders: 15_000, lines: 50_000 };

/// A tiny xorshift64* PRNG — deterministic, no external `rand` dependency.
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Lcg {
        Lcg(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn next_range(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

const REGIONS: [&str; 4] = ["@north", "@south", "@east", "@west"];

const ENTITIES: &str = "\
entity Customer { name: Text, region: {@north | @south | @east | @west} }
entity Product  { name: Text, price: Money }
entity Order    { customer: CustomerID, placed: Date }
entity Line     { order: OrderID, product: ProductID, qty: Int }
";

const QUERIES: &str = "\
let lineprice : Line -> Money     = .qty * .product.price
let custspend : Customer -> Money = sum(lineprice by .order.customer)
let inregion  : Customer          = id where .region in (@west | @east)
let result    : Customer -> Money = inregion . (custspend where > 30)
";

/// Generate a full program: entities, `scale.customers` Customer news,
/// `scale.products` Product news, `scale.orders` Order news, `scale.lines`
/// Line news, then the 4 view `let`s — in that order, matching spec12.
///
/// Returns the source text plus the exact statement-count breakdown so
/// callers can slice the elaborated `TProgram.stmts` without re-parsing.
///
/// For a fixed seed and unchanged `customers`/`products`/`orders`, growing
/// `lines` reuses the identical RNG prefix — so `generate(Scale { lines: n +
/// k, ..base }, seed)` produces `base`'s program with `k` extra `Line` rows
/// appended, letting callers slice out just the new deltas.
pub fn generate(scale: Scale, seed: u64) -> (String, Counts) {
    let mut rng = Lcg::new(seed);
    let mut src = String::with_capacity(
        ENTITIES.len() + QUERIES.len() + 64 * (scale.customers + scale.products + scale.orders + scale.lines),
    );
    src.push_str(ENTITIES);
    src.push('\n');

    for i in 0..scale.customers {
        let region = REGIONS[rng.next_range(REGIONS.len())];
        src.push_str(&format!(
            "let c{i} = new Customer {{ name: \"Cust{i}\", region: {region} }}\n"
        ));
    }
    for i in 0..scale.products {
        let cents = 100 + rng.next_range(99_900); // $1.00..$1000.00
        src.push_str(&format!(
            "let p{i} = new Product {{ name: \"Prod{i}\", price: {}.{:02} }}\n",
            cents / 100,
            cents % 100
        ));
    }
    for i in 0..scale.orders {
        let c = rng.next_range(scale.customers);
        let day = 1 + rng.next_range(28);
        let month = 1 + rng.next_range(12);
        src.push_str(&format!(
            "let o{i} = new Order {{ customer: c{c}, placed: 2026-{month:02}-{day:02} }}\n"
        ));
    }
    for _ in 0..scale.lines {
        let o = rng.next_range(scale.orders);
        let p = rng.next_range(scale.products);
        let qty = 1 + rng.next_range(10);
        src.push_str(&format!(
            "let _ = new Line {{ order: o{o}, product: p{p}, qty: {qty} }}\n"
        ));
    }
    src.push('\n');
    src.push_str(QUERIES);

    let counts = Counts {
        customers: scale.customers,
        products: scale.products,
        orders: scale.orders,
        lines: scale.lines,
        views: 4,
    };
    (src, counts)
}

/// Exact statement-count breakdown of a generated program, in emission order:
/// customers, products, orders, lines, then views.
#[derive(Clone, Copy)]
pub struct Counts {
    pub customers: usize,
    pub products: usize,
    pub orders: usize,
    pub lines: usize,
    pub views: usize,
}

impl Counts {
    pub fn total_news(&self) -> usize {
        self.customers + self.products + self.orders + self.lines
    }
}
