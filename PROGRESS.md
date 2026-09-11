# PROGRESS — living log

## Session 1 (2026-08-05) — Phases 0–1

Goal: full scaffold + Dijkstra baseline + correct (slow) BMSSP, verified vs Dijkstra on
n <= 5000 across all graph families, with custom bench harness.

### Status

- [x] PLAN.md written
- [x] Crate scaffold (Cargo.toml, modules, stubs)
- [x] graph.rs CSR + generators
- [x] params.rs / counters.rs
- [x] dijkstra.rs baseline + oracle tests
- [x] queue.rs BTreeMap PartialQueue + model test
- [x] bmssp.rs (BaseCase, FindPivots, BMSSP, driver) — CORRECT
- [x] Property tests bmssp vs dijkstra
- [x] Bench harness
- [x] Results recorded below

### Bench snapshot (release, `BMSSP_BENCH_ITERS=3`, n=10000 default / 100000 for the big rows)

| family | n | m | dijk(ms) | bmssp(ms) | speedup | verified |
|---|---|---|---|---|---|---|
| er_c2 | 10000 | 20000 | 1.066 | 3.178 | 0.34 | true |
| er_c4 | 10000 | 40000 | 1.901 | 3.834 | 0.50 | true |
| er_c8 | 10000 | 80000 | 2.783 | 7.312 | 0.38 | true |
| er_c4_1e5 | 100000 | 400000 | 42.821 | 105.101 | 0.41 | true |
| grid_100 | 10000 | 19800 | 1.145 | 7.041 | 0.16 | true |
| grid_316 | 99856 | 199080 | 15.620 | 217.226 | 0.07 | true |
| pl_c2 | 10000 | 20000 | 0.002 | 0.010 | 0.17 | true |
| layered | 100000 | 398000 | 22.146 | 1760.904 | 0.01 | true |
| er_c4_real | 10000 | 40000 | 1.948 | 20.735 | 0.09 | true |

As expected for a Phase-1 reference (no partial-execution cap, BTreeMap queue, full recursion),
BMSSP is 2–100x slower than binary-heap Dijkstra; worst case is `layered` (long chains defeat the
interval structure). This matches the honest-benchmark literature (BMSSP loses to tuned Dijkstra at
every runnable size). Phase-2 work targets the asymptotic upgrades and a block-based queue.

### Key findings (updated as we go)

1. **Algorithm design, phase-1 reference variant.** Implemented BMSSP(l, B, S) with the
   partial-order queue (Insert / Pull / BatchPrepend), FindPivots (bounded Bellman-Ford
   sweep + shortest-path-forest pivot rule), BaseCase (bounded multi-source mini-Dijkstra
   settling k+1 vertices), and the two-phase interval loop. Params k, t, l from `params(n)`.
   Runs to D-empty (B' = B always); no k·2^(lt) cap yet (Phase 2).

2. **Correctness bugs found & fixed (via exhaustive counterexample search + trace):**
   - `base_case` must return EVERY vertex it touches (settled AND still-in-heap), not just
     the k+1 settled ones — otherwise un-settled discovered vertices are lost.
   - The main loop must route discoveries on strict improvement into pq / batch. The first
     version checked `cand < dist[v]` AFTER relaxing, which was always false → nothing routed.
   - A level's relax loop must gate the distance update itself by `cand < b - EPS`. A child
     with bound bi=1 was "stealing" an improvement (cand=2, above its bound) without routing
     it, so the parent could never re-route it. Fix: above-bound discoveries are left for the
     parent to re-relax and route. (This replaces the reference's route-on-every-edge, which
     requires an iteration cap to terminate.)
   - Boundary/tie buckets: base_case returning all settled vertices (incl. boundary) avoids
     the infinite re-add loop and the tied-bucket loss; routing on strict improvement only
     avoids re-inserting completed vertices.
   - `find_pivots` marks/propagates vertices reached within the bound even on equal keys, so
     the W-sweep completes; pivots fall back to S on early-stop / no-qualifier.

3. **Verification.** `cargo test`: 9 lib + 7 property + 8 handcrafted = 24 green (vs
   binary-heap Dijkstra; Floyd-Warshall oracle on small). Exhaustive brute force over all
   simple digraphs n=2..4 (weights {0,1,2}, all sources) + 200k random each for n=5,6:
   no counterexample. Counterexamples that were found and fixed: n=3 triangle with a zero
   edge; n=6 graph where a child level stole an above-bound improvement.

4. **Debug aids.** `BMSSP_TRACE=1` env var prints relax events (R/B lines). Brute force in
   `examples/bruteforce.rs`; repro harness in `examples/dbg.rs`.

## Session 2 (2026-08-05) — Phase 2: constant-degree transform

### Status

- [x] `src/transform.rs`: `to_constant_degree` — splits every vertex of out-degree > 2 into a
      balanced binary tree of zero-weight aux nodes (root = original vertex). Result has max
      out-degree <= 2; original vertices keep ids `0..n`; distance-preserving. Size
      n' = n + sum_{deg>2}(2·deg − 2), m' ≈ 3m (O(n+m)).
- [x] `tests/transform.rs` (6 tests): max out-degree <= 2; distances preserved across all
      families (er/dense/grid/chain/power_law/layered, incl. zero weights); weighted-edge
      (head, weight) multiset preserved; BMSSP on transformed matches Dijkstra on original;
      exhaustive n=2..4 all simple digraphs. All green; full suite now 30 tests.
- [x] Bench harness extended with `bench_one_transformed` rows (suffix `_tr`).

### Findings

1. Transform works and is exact (verified=true everywhere, projected back to original ids).
2. Cost is real: high-degree hubs blow up the graph (~7x for er_c8: n 10k -> 150k; power-law
   ~70x: n 100k -> 700k). BMSSP-on-transformed is always slower than BMSSP-on-original and
   slower than Dijkstra on any variant (speedup 0.00–0.58). Worst case `layered_tr` (n=697k)
   is ~70s vs Dijkstra 66ms — the aux trees create long zero-weight chains that the interval
   structure cannot skip.
3. Conclusion (matches `logtwothirds`): the constant-degree transform is a theoretical
   device, not a practical speedup here; BMSSP remains behind tuned Dijkstra at every
   runnable size. Grid (`right`+`down`, out-degree <= 2) transforms to itself.

### Open / next

- [ ] **Review follow-ups (latent, not live)**
  - [ ] `base_case` marks seeds at init, so `u0` counts only *non-seed* settlements and the
        returned B' (`max dist over u0`) is not the paper's `max dhat in U0`. Harmless now
        (B' is unconsumed; `touched` still returns seeds), but must be settled before the
        Phase-3 D-empty/early-stop B' logic lands.
  - [ ] `find_pivots` forest parents are only reset for seeds; a W vertex with no incoming
        equality edge can keep a stale parent, and zero-weight ties can break the layer order
        for the reverse-order `sub` accumulation. Both only skew pivot *selection* (fallback
        `P = S` preserves correctness) — perf concern, revisit in the pivot-quality ablation.
- [x] **Phase 3: complexity-relevant upgrades (bound `k·2^(l·t)`, partial execution, D-empty
      vs early stop) + ablation matrix** — see Session 3.
- [x] **Phase 3b: Lemma 3.3 block-based queue, differentially tested vs BTreeMap model.**
- [ ] Phase 4: engineering polish (CSR locality, arena buffers, iterative recursion).
- [ ] Phase 5: scaling n = 10^4..10^7; ablation (no-pivots, k/t schedules, BTreeMap vs block,
      ±transform, recursion vs iterative); asymptotics t/(m log^(2/3) n) vs t/(m + n log n).
- [ ] Phase 6: reproducibility docs (ALGORITHM.md, AUDIT.md, auto BENCHMARKS.md).

## Session 3 (2026-08-14) — Phase 3 + 3b: partial execution, block queue, ablation

Goal: bound the per-call workload by `k·2^(l·t)` (halt + hand up unprocessed work), implement
the Lemma 3.3 block-based queue with real Pull, and expose the full ablation matrix in the bench.

### Status

- [x] `BmsspConfig { k, t, l, use_pivots, partial_execution, queue_impl }`; bench knobs
      `BMSSP_NO_PIVOTS`, `BMSSP_QUEUE=map|block`, `BMSSP_PARTIAL`, `BMSSP_BENCH_ITERS`.
- [x] Partial execution: halting at `|U| > k·2^(l·t)` (distinct vertices, epoch-deduped via
      dedicated `marked_u` array + `u_epoch` — the shared `marked` is clobbered by children's
      interior epochs); on halt the call drains its queue into U so the caller relaxes the
      dropped items' edges.
- [x] `drain()` on `PartialQueue`/`BlockQueue`/`QueueOps`; halt branch drains queue into u_total
      with a comment citing Remark 3.4.
- [x] Routing deviation kept (documented): every strict improvement is routed
      (`cand >= bi - EPS -> pq`, else `to_batch`), not the paper's interval skip; W-vertices
      discovered by FindPivots are inserted into the call's own queue (dist < b − EPS) instead
      of relying on the paper's ≤-equality re-insertion (Remark 3.4) — this is what keeps
      halting calls from losing them.
- [x] BlockQueue (Lemma 3.3): fixed-block `F = 2^((l-1)·t)`, per-block min-buckets, max-boundary
      split, gap placement; differential test `block_queue_matches_model` vs the BTreeMap model.
- [x] `tests/config_variants.rs` (8 variants: pivots ±, queue ×, partial ×) verified vs Dijkstra
      on all sized families.
- [x] Bench ablation wired: per-graph `from_n(g.n)` for k/t/l (the top level must stay
      successful: its bound `k·2^(l·t)` exceeds `|U| ≤ n`), env knobs for pivots/queue/partial.

### Bugs found & fixed (partial execution)

1. **Queue dropped on halt.** The halting call returned only `u_total` (its touched set), so
   discovered-but-unprocessed queue items vanished. Fixed by draining the queue into U.
2. **FindPivots-set dists were never queued.** `find_pivots` improves `dhat` for W vertices
   without inserting them; on halt the W-sweep excludes them (`dhat >= B'_i`), so their edges
   are never relaxed at the parent level. Case: `layered(20,15,4,5,Int{0,9})`, pivots=true,
   vertex 202 (path `0→…→172→186→202`, w=0 edge `186→202`): dist[186]=21 set by the pivot
   sweep, level-2 call halts with `B'_2=18`, vertex 202 stays unreachable. Fixed by queuing the
   W-vertices (dist < b − EPS) directly — verified via `config_variants` (3/3 green).

### Bench snapshot (release, `BMSSP_BENCH_ITERS=1`, partial execution ON, Block queue, pivots ON)

| family | n | m | dijk(ms) | bmssp(ms) | speedup | verified |
|---|---|---|---|---|---|---|
| er_c2 | 10000 | 20000 | 0.986 | 9.128 | 0.11 | true |
| er_c4 | 10000 | 40000 | 1.824 | 14.935 | 0.12 | true |
| er_c8 | 10000 | 80000 | 2.900 | 18.343 | 0.16 | true |
| er_c4_1e5 | 100000 | 400000 | 41.211 | 369.900 | 0.11 | true |
| grid_100 | 10000 | 19800 | 1.310 | 11.289 | 0.12 | true |
| grid_316 | 99856 | 199080 | 13.557 | 149.582 | 0.09 | true |
| pl_c2 | 10000 | 20000 | 0.002 | 0.019 | 0.09 | true |
| pl_c4_1e5 | 100000 | 400000 | 0.029 | 0.395 | 0.07 | true |
| layered | 100000 | 398000 | 19.989 | 190.546 | 0.10 | true |
| er_c4_real | 10000 | 40000 | 1.753 | 23.015 | 0.08 | true |

### Key findings

1. Every ablation variant is exact (all `verified=true`, incl. `_tr` rows) on the full bench
   set. Partial execution + Block queue cut `layered` 1.76s -> 0.19s (Phase-1 snapshot) while
   staying exact — the interval structure + halt bound now cap the pathological chains.
2. BMSSP remains 6–15x slower than tuned Dijkstra at every runnable size; the block queue and
   partial execution shrink the constant but don't change the asymptotic picture — consistent
   with the honest-benchmark literature.

### Open / next

- [ ] Phase 4: engineering polish (CSR locality, arena buffers, iterative recursion).
- [ ] Phase 5: scaling n = 10^4..10^7; ablation (no-pivots, k/t schedules, BTreeMap vs block,
      ±transform, recursion vs iterative); asymptotics t/(m log^(2/3) n) vs t/(m + n log n).
- [ ] Phase 6: reproducibility docs (ALGORITHM.md, AUDIT.md, auto BENCHMARKS.md).

## Session 4 (2026-08-18) — Correctness audit + paper-contract repairs

Goal: review the codebase against the paper / PLAN, catch latent bugs that distance-vs-Dijkstra
tests were masking, and restore BaseCase / leftover / FindPivots contracts so partial execution
and the k+1 workload bound are real.

### Status

- [x] Full suite green before changes (35 tests); after fixes: 40 tests green.
- [x] Deep audit of `bmssp.rs` / `queue.rs` / params / transform vs Algorithms 1–3.
- [x] **P0: `base_case` settlement accounting fixed** — seeds no longer pre-mark; settle on
      extract-min; stop at k+1; `B' = max dhat in U0` when `|U0| > k`. Still return
      settled-including-boundary + still-in-heap (documented safety vs equal-key loops).
- [x] **P0: `S_i` leftover BatchPrepend** (Alg. 3 line 25) when `B'_i < B_i`, skipping
      vertices already in `U_i` to avoid infinite re-prepend on tie buckets.
- [x] **P0: FindPivots re-expands on ≤ each round** (Alg. 1 `W_i` set semantics) so strict
      improvements propagate within the remaining k rounds.
- [x] **P0: |U| dedup bug under recursion** — shared `marked_u`/`u_epoch` was clobbered by
      child calls, inflating `|U|` past `k·2^(l·t)` and spuriously halting the top level
      (`B' << ∞`, lost vertices). Replaced with a per-call `HashSet`.
- [x] Partial halt kept as `|U| > k·2^(l·t)` (Session 3): `>=` spuriously
      top-level-halts when the bound equals n, setting `B' = last_bp << ∞`.
- [x] Regression unit tests: base_case B'/cap, FindPivots improvement, S_i leftovers,
      partial+pivots no spurious top-level halt (`er_random(400,4,seed=1)`).
- [x] `config_variants` now uses `from_n(g.n)` (was fixed `from_n(200)`).

### Bugs found & fixed

1. **`base_case` k+1 / B' were dead.** Discoveries were marked on push, so the extract path
   never incremented `settled` for non-seeds; with `|S| ≤ k` the leaf ran a full bounded
   Dijkstra and almost always returned `B' = B`. Masked the missing leftover prepend and
   made partial-execution boundaries meaningless. Fixed to settle-on-extract.
2. **Missing `S_i` leftover re-prepend** when `B'_i < B_i`. Latent until (1) made `B'` real;
   without it, tied/partial children dropped unfinished seeds. Skip-if-in-`U_i` prevents the
   equal-key infinite re-prepend Session 1 already hit.
3. **FindPivots under-propagated.** Already-marked vertices were never re-queued on ≤ /
   strict improvement, unlike Alg. 1’s per-round `W_i`. Pivot/W distances could stay
   suboptimal inside the k rounds (end-to-end SSSP still often healed via W→queue).
4. **Spurious top-level partial halt (correctness).** Child recursion overwrote `marked_u`
   epochs, so the same vertex was counted many times in `|U|`. Repro: `from_n(200)` params
   on `er_random(400, 4, seed=1)`, partial+pivots+BTreeMap → `HALT l=3 |U|=513 limit=512
   last_bp=26` with vertex 140 at Dijkstra dist 29 left at `∞`. HashSet dedup fixes it;
   top level no longer halts when `k·2^(l·t) ≥ n`.

### Key findings

1. Distance-vs-Dijkstra tests alone do **not** enforce paper local contracts (B', k+1,
   leftovers, pivot completeness). Need structural unit tests (now added).
2. Shared epoch arrays across recursive calls are a recurring hazard (`marked` was fixed in
   Session 3; `marked_u` had the same shape of bug).
3. Returning boundary + still-in-heap from BaseCase remains a necessary deviation for
   zero-weight / tie buckets under strict-`<` routing.
4. Open follow-ups from Session 2 on base_case B' / FindPivots parent reset are **closed**
   by this session (parent reset for all W was already in tree; B' now real).

### Open / next

- [x] Phase 4 — see Session 5.
- [x] Pivot-forest hardening — see Session 5.
- [x] Phase 5 (harness) + Phase 6 docs — see Session 5.
- [ ] Optional: push scaling to n=10^7 on a bigger machine; D₀ BatchPrepend; true
      decrease-key in queues.

## Session 5 (2026-08-18) — Phase 4 polish + docs + scaling harness

Goal: engineering constants (after Session 4 correctness), pivot-forest hardening,
reproducibility docs, and a scaling bench mode.

### Status

- [x] **Depth-stamped U membership** replaces per-call `HashSet`: each recursive frame
      marks with its `call_depth`; children write deeper depths and cannot clobber the
      parent’s U set (fixes the Session 4 clobber class without hashing).
- [x] **Arena buffer pool** (`pool_u32` / `pool_pairs`) recycles scratch vectors across
      BaseCase / FindPivots / BMSSP frames.
- [x] **CSR locality:** `Graph::from_edges` sorts each adjacency list by `(weight, to)`.
- [x] **Pivot forest:** equality parents only to *earlier-in-W* vertices; clear only W
      slots of `sub` / `w_index` (no full `sub.fill(0)`).
- [x] Iterative stack deferred: recursion depth is `O(l)=O(log^{1/3} n)` (tiny); not the
      bottleneck vs Dijkstra.
- [x] `ALGORITHM.md`, `AUDIT.md`, `BENCHMARKS.md` written.
- [x] Bench: `BMSSP_SCALE=1` builds n≈1e4..1e6 ER/grid/layered set and prints
      `t/(m log^{2/3} n)` vs `t/(m+n log n)` normalizers (skips transform pass).
- [x] Full suite: **41 tests** green. Release bench (partial+block): all `verified=true`.

### Bench snapshot (release, iters=1, partial+Block+pivots)

| family | n | dijk(ms) | bmssp(ms) | speedup | verified |
|---|---|---|---|---|---|
| er_c4_1e5 | 1e5 | 14.3 | 143.6 | 0.10 | true |
| grid_316 | ~1e5 | 8.1 | 61.0 | 0.13 | true |
| layered | 1e5 | 11.8 | 85.6 | 0.14 | true |

vs Session 3: layered 190→86ms, er_c4_1e5 370→144ms — constant-factor win, Dijkstra still ahead.

### Findings

1. Depth stamps are the right long-term U dedup (O(1), clobber-safe); HashSet was a
   correctness stopgap.
2. Sorting CSR by weight helps BaseCase/Dijkstra-like scans slightly; transform still
   loses on hubby graphs.
3. Docs + `BMSSP_SCALE` close the Phase 5/6 “reproducibility” checklist for agentic
   follow-up; full 1e7 runs need more wall-clock than this session budgeted.

## Session 6 (2026-09-11) — Landscape README + D₀ / decrease-key

Goal: document the broader BMSSP implementation landscape, then land the
highest-payoff DS upgrade called out across public ports (true Lemma 3.3 `D₀`
BatchPrepend + decrease-key).

### Status

- [x] `README.md` — dated (2026-09-11) landscape survey, this crate’s position,
      levers, and improvement roadmap.
- [x] `BlockQueue`: `D₀` front `VecDeque` for BatchPrepend when every new key is
      strictly below the live minimum; safe flush-into-`D₁` fallback otherwise.
- [x] Lazy decrease-key via `best: HashMap<u32, f64>` (skip worse Insert/Prepend).
- [x] Pull selects from the live set and rebuilds leftovers into `D₁` (correct;
      O(|S′|) prefix Pull still open).
- [x] BMSSP routing: Insert cut is strict `cand >= B_i` (not `B_i - EPS`) so
      real-weight keys cannot land in `D₁` behind larger `D₀` leftovers.
- [x] Regression `tests/repro_real.rs` (int + real ER, block+partial).
- [x] Full suite green; clippy `-D warnings` clean; release bench all `verified=true`.

### Findings

1. D₀ + decrease-key are the right *structural* move toward the paper bound;
   wall-clock on this Linux box is not yet better than Session 5’s macOS numbers
   because Pull still sorts the full live set each time.
2. Real-weighted `er_c4_real` is a sharp correctness probe for D₀/D₁ ordering
   under float EPS — integer graphs alone will not catch it.
3. Next: restore paper-style prefix-of-blocks Pull on top of D₀, then re-bench;
   then `t`/`k` sweep / multi-source / openSVM JSON metrics.
