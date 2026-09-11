//! Regression: BlockQueue + partial execution on real-weighted ER graphs.
//!
//! An earlier D0/leftover placement bug only showed up here (int weights were
//! fine; `er_c4_real` in the release bench went `verified=false`).

use bmssp_rs::bmssp::{BmsspConfig, BmsspEngine, QueueKind};
use bmssp_rs::counters::Counters;
use bmssp_rs::dijkstra::dijkstra;
use bmssp_rs::graph::{er_random, WeightDist};

fn close(a: &[f64], b: &[f64]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (x, y) in a.iter().zip(b) {
        if x.is_infinite() && y.is_infinite() {
            continue;
        }
        if x.is_infinite() || y.is_infinite() {
            return false;
        }
        let scale = 1.0 + x.abs().max(y.abs());
        if (x - y).abs() > 1e-9 * scale {
            return false;
        }
    }
    true
}

fn run(wd: &WeightDist) -> bool {
    let g = er_random(10_000, 4, 0xB0555EED, wd);
    let cfg = BmsspConfig {
        use_pivots: true,
        queue_impl: QueueKind::Block,
        partial_execution: true,
        ..BmsspConfig::from_n(g.n)
    };
    let mut dc = Counters::new();
    let d = dijkstra(&g, 0, &mut dc);
    let mut bc = Counters::new();
    let b = BmsspEngine::new(&g, cfg, &mut bc).run(0);
    close(&d, &b)
}

#[test]
fn block_partial_matches_on_int_and_real_er() {
    assert!(
        run(&WeightDist::Int { min: 1, max: 100 }),
        "int-weighted er_c4 failed"
    );
    assert!(
        run(&WeightDist::Real { min: 0.0, max: 1.0 }),
        "real-weighted er_c4 failed"
    );
}
