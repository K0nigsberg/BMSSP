# AUDIT — correctness history and invariants

Living record of bugs found against the paper / Dijkstra oracle, and the
invariants the test suite is meant to protect.

## Invariants (must hold)

1. **Distances:** for every supported config, `BMSSP` distances match Dijkstra
   within `0` (integer weights) or `1e-9` relative (reals), including unreachables.
2. **Top-level success:** with `params(n)`, `k·2^(l·t) ≥ n`, so a full run with
   `partial_execution` must not lose reachable vertices.
3. **BaseCase cap:** a leaf settles at most `k+1` vertices and returns a real
   `B' < B` when the cap is hit (`bmssp::tests::base_case_caps_*`).
4. **Leftovers:** when a child returns `B'_i < B_i`, unfinished `S_i` seeds with
   `d̂ ∈ [B'_i, B_i)` re-enter `D` unless already in `U_i`.
5. **U dedup:** `|U|` counts distinct vertices per call; recursive children must
   not inflate the parent’s `|U|` (depth stamps; regression on `er_random(400,…)`).
6. **FindPivots propagation:** within `k` rounds, strict improvements re-expand
   (`find_pivots_propagates_improvement_across_rounds`).
7. **Pivot forest acyclicity:** equality parents only to earlier-in-W vertices
   (`find_pivots_forest_rejects_zero_weight_cycles`).

## Critical bugs fixed (by session)

| Session | Bug | Symptom | Fix |
|---------|-----|---------|-----|
| 1 | BaseCase returned only settled, lost heap verts | Wrong dists on tiny digraphs | Return touched |
| 1 | Route-after-relax always false | Nothing entered `D` | Route on strict improvement before/with update |
| 1 | Child stole above-bound improvements | Parent never saw them | Gate updates by child bound |
| 3 | Halt dropped queue | Lost discoveries | Drain `D` into `U` |
| 3 | FindPivots W never queued | Unreachable under partial | Insert `W` into `D` |
| 4 | BaseCase `settled` never counted non-seeds | `B'` always `B`; full Dijkstra leaf | Settle on extract |
| 4 | Missing `S_i` leftover prepend | Dropped seeds once `B'` real | Alg. 3 line 25 + skip-if-in-`U` |
| 4 | FindPivots no re-expand on ≤ | Stale dists inside `k` rounds | Per-round `W_i` set semantics |
| 4 | Shared `marked_u` epoch clobber | Spurious top halt, lost verts | Depth-stamped U (then arena) |
| 4 | Halt at `\|U\| ≥ limit` when limit=`n` | Top-level `B'<<∞` | Keep `\|U\| > limit` |
| 5 | Zero-weight pivot forest cycles | Wrong/`P=S` pivots | W-order parents only |
| 6 | D₀ leftovers + `cand >= B_i - EPS` Insert | Real-weight `er_c4_real` wrong dists under block+partial | Strict `cand >= B_i`; Pull from live `best`; D₀ drain / D₁ leftovers |

## Test map

| File | Role |
|------|------|
| `src/dijkstra.rs` | Floyd oracle on tiny graphs |
| `src/queue.rs` | BlockQueue ↔ decrease-key model fuzz + D₀ path tests |
| `src/params.rs` | `k·2^(l·t) ≥ n` |
| `src/bmssp.rs` tests | BaseCase B', leftovers, FindPivots, forest, partial regression |
| `tests/bmssp_vs_dijkstra.rs` | Property tests across families |
| `tests/config_variants.rs` | pivots × queue × partial vs Dijkstra |
| `tests/handcrafted.rs` | Chains, zeros, parallels, stars |
| `tests/transform.rs` | Degree ≤ 2, distance preservation |
| `tests/repro_real.rs` | Block+partial on int/real ER (D₀ float regression) |

## How to re-audit

```bash
cargo test
BMSSP_PARTIAL=1 BMSSP_QUEUE=block BMSSP_BENCH_ITERS=1 cargo run --release --bin bench_sssp
# optional: BMSSP_TRACE=1 on a failing seed via examples/dbg.rs
```
