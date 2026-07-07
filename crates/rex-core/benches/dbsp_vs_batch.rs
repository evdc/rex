//! Benchmarks the DBSP incremental engine (`rex::dbsp`) against the batch
//! interpreter (`rex::eval`) on a synthetic TPC-H-shaped dataset (see
//! `dataset.rs`), for two scenarios:
//!
//! - **cold_start**: computing a full query from scratch over N rows of
//!   data — batch's only mode, vs. DBSP's full load+backfill, vs. DBSP's
//!   backfill-over-preexisting-data alone.
//! - **incremental_delta**: applying K new rows once the views are already
//!   live over a fixed base dataset — DBSP's per-transaction delta cost vs.
//!   batch's only option, a full recompute of base+delta.

mod dataset;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use dataset::{Scale, LARGE, MEDIUM, SMALL};
use rex::dbsp::Engine;
use rex::eval::interp::lit_value;
use rex::eval::{self, Value};
use rex::types::typed::{TProgram, TStmt, TValue};
use std::collections::HashMap;
use std::time::Duration;

const SEED: u64 = 0xC0FFEE;

fn elaborate(src: &str) -> TProgram {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    checked.elaborated.expect("clean check produces the elaborated program")
}

/// Mirrors `tests/incremental.rs`'s `apply_stmt`: apply one typed statement
/// to a live `Engine`, resolving `new`-bound value references as we go.
fn apply_stmt(engine: &mut Engine, stmt: &TStmt, values: &mut HashMap<String, Value>) {
    match stmt {
        TStmt::New { name, sort, fields } => {
            let resolved: Vec<(String, Value)> = fields
                .iter()
                .map(|(f, tv)| {
                    let v = match tv {
                        TValue::Lit(lit) => lit_value(lit),
                        TValue::Ref(n) => values[n].clone(),
                    };
                    (f.clone(), v)
                })
                .collect();
            let (id, _) = engine.apply_new(*sort, &resolved);
            if let Some(name) = name {
                values.insert(name.clone(), id);
            }
        }
        TStmt::Let { name, body } => {
            if let Some(name) = name {
                engine.add_view(name, body, values);
            }
        }
        TStmt::LetRec { bindings } => {
            engine.add_view_group(bindings, values);
        }
    }
}

fn apply_all(engine: &mut Engine, stmts: &[TStmt], values: &mut HashMap<String, Value>) {
    for stmt in stmts {
        apply_stmt(engine, stmt, values);
    }
}

/// `tuning` is `(sample_size, measurement_time_secs)` — larger scales need
/// fewer samples and a longer measurement window than Criterion's defaults
/// (100 samples / 5s), or every run prints "unable to complete N samples"
/// warnings and silently truncates the sample count anyway.
fn bench_cold_start(c: &mut Criterion, label: &str, scale: Scale, tuning: Option<(usize, u64)>) {
    let (src, counts) = dataset::generate(scale, SEED);
    let typed = elaborate(&src);
    let news_end = counts.total_news();

    let mut group = c.benchmark_group(format!("cold_start/{label}"));
    if let Some((n, secs)) = tuning {
        group.sample_size(n);
        group.measurement_time(Duration::from_secs(secs));
    }

    // Batch has no way to separate loading data from computing views — this
    // is its one, natural full-pipeline number.
    group.bench_function("batch/full_eval", |b| {
        b.iter_with_large_drop(|| eval::run_typed_values(&typed))
    });

    // Apples-to-apples against batch/full_eval: fresh engine, load everything,
    // define every view (which backfills it over the data just loaded).
    group.bench_function("dbsp/full_load_and_backfill", |b| {
        b.iter_batched(
            || (Engine::new(), HashMap::new()),
            |(mut engine, mut values)| {
                apply_all(&mut engine, &typed.stmts, &mut values);
                (engine, values)
            },
            BatchSize::LargeInput,
        )
    });

    // Isolates just the backfill cost (`Circuit::backfill`, the DBSP term for
    // "compute a new view over data already sitting in the circuit"), with
    // data loading pushed into unmeasured setup.
    group.bench_function("dbsp/backfill_only", |b| {
        b.iter_batched(
            || {
                let mut engine = Engine::new();
                let mut values = HashMap::new();
                apply_all(&mut engine, &typed.stmts[..news_end], &mut values);
                (engine, values)
            },
            |(mut engine, mut values)| {
                apply_all(&mut engine, &typed.stmts[news_end..], &mut values);
                (engine, values)
            },
            BatchSize::LargeInput,
        )
    });

    group.finish();
}

fn cold_start_benches(c: &mut Criterion) {
    bench_cold_start(c, "small", SMALL, None); // ~ms-scale ops: 100 samples fit in the default 5s fine
    bench_cold_start(c, "medium", MEDIUM, Some((30, 10)));
    bench_cold_start(c, "large", LARGE, Some((10, 25)));
}

fn incremental_benches(c: &mut Criterion) {
    let base = MEDIUM;
    let mut group = c.benchmark_group("incremental_delta");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for k in [1usize, 10, 100, 1_000] {
        let extended = Scale { lines: base.lines + k, ..base };
        let (src, counts) = dataset::generate(extended, SEED);
        let typed = elaborate(&src);

        let base_news_end = counts.customers + counts.products + counts.orders + base.lines;
        let delta_news_end = base_news_end + k;
        debug_assert_eq!(counts.lines, base.lines + k);
        debug_assert_eq!(typed.stmts.len(), delta_news_end + counts.views);

        // Pure IVM cost: views are already live over the base dataset;
        // measure only applying the K new Line transactions.
        group.bench_with_input(BenchmarkId::new("dbsp/apply_delta", k), &k, |b, _| {
            b.iter_batched(
                || {
                    let mut engine = Engine::new();
                    let mut values = HashMap::new();
                    apply_all(&mut engine, &typed.stmts[..base_news_end], &mut values);
                    apply_all(&mut engine, &typed.stmts[delta_news_end..], &mut values);
                    (engine, values)
                },
                // Return the engine so criterion drops it *outside* the timed
                // region — dropping ~30 populated integrals costs ~20ms and
                // otherwise swamps the per-delta cost being measured.
                |(mut engine, mut values)| {
                    apply_all(&mut engine, &typed.stmts[base_news_end..delta_news_end], &mut values);
                    (engine, values)
                },
                BatchSize::LargeInput,
            )
        });

        // Batch's only option: recompute everything (base + delta) from
        // scratch every time.
        group.bench_with_input(BenchmarkId::new("batch/full_recompute", k), &k, |b, _| {
            b.iter_with_large_drop(|| eval::run_typed_values(&typed))
        });
    }

    group.finish();
}

// Small/fast benchmarks (a few ms) are noisy enough on a dev laptop that the
// default 1% noise threshold flags normal jitter as "improved"/"regressed"
// between otherwise-identical runs; widen it so only real changes get flagged.
fn config() -> Criterion {
    Criterion::default().noise_threshold(0.05)
}

criterion_group! {
    name = benches;
    config = config();
    targets = cold_start_benches, incremental_benches
}
criterion_main!(benches);
