# bmssp-rs

**Rust implementation of Bounded Multi-Source Shortest Path (BMSSP)** — the core
subroutine of Duan–Mao–Mao–Shu–Yin, *Breaking the Sorting Barrier for Directed
Single-Source Shortest Paths* (arXiv:[2504.17033](https://arxiv.org/abs/2504.17033),
STOC 2025).

This crate is a **correctness-first** port with a Dijkstra oracle, ablation knobs
(pivots / queue / partial execution), CSR graphs, a Lemma‑3.3‑style block queue,
operation counters, and a custom release bench harness. At every size we have
measured so far, a tuned binary-heap Dijkstra still wins on wall-clock time —
matching the honest public literature.

| | |
|---|---|
| **Crate** | `bmssp-rs` 0.1.0 (Rust 2021) |
| **Paper** | arXiv 2504.17033 (v1 Apr 2025; check v2 Jul 2025 revisions) |
| **Theory** | Deterministic \(O(m \cdot \log^{2/3} n)\) directed SSSP, non‑negative weights |
| **This repo’s measured gap** | BMSSP ≈ **6–12× slower** than Dijkstra on n ≤ 10⁵ (Session 5) |
| **Survey date** | **2026-09-11** (landscape + this crate’s status) |

Living docs elsewhere in the tree:

- [`ALGORITHM.md`](ALGORITHM.md) — what we implement (and documented deviations)
- [`AUDIT.md`](AUDIT.md) — invariants + bug history vs Dijkstra / paper contracts
- [`BENCHMARKS.md`](BENCHMARKS.md) — pinned-seed wall-clock tables
- [`PLAN.md`](PLAN.md) / [`PROGRESS.md`](PROGRESS.md) — roadmap and session log

---

## Why BMSSP exists (one paragraph)

Dijkstra’s comparison-based bound is \(O(m + n\log n)\). The 2025 result breaks
that *sorting barrier* with a deterministic directed SSSP algorithm whose
dominant subroutine is **BMSSP**: recursively solve bounded multi-source
subproblems, using **FindPivots** (limited Bellman–Ford + shortest-path forest)
to shrink the frontier and a specialized partial-order structure **D**
(`Insert` / `Pull` / `BatchPrepend`) so total work scales like
\(m\cdot\log^{2/3}n\) rather than a full sort of the frontier. A Feb 2026
follow-up sharpens the bound further to
\(O(m\cdot\sqrt{\log n\cdot\log\log n})\) (“beyond Dijkstra”); almost nobody
has shipped that yet.

---

## Landscape (as of 2026-09-11)

Since the paper appeared (April 2025), roughly a dozen independent ports have
shown up across Rust, Python, Go, C/C++, JavaScript, Java, and multi-language
benchmark harnesses. The **consistent empirical finding** is sobering and
important:

> Every carefully measured implementation so far **loses to a plain optimized
> Dijkstra** on wall-clock time. The asymptotic win is real; constant factors
> and D-structure overhead currently dominate.

### Implementation matrix

| Implementation | Lang | D structure | Notes / reported result | Status |
|---|---|---|---|---|
| **`bmssp-rs` (this repo)** | Rust | `PartialQueue` (BTreeMap) + `BlockQueue` (Lemma 3.3-style) | Correct vs Dijkstra; **~6–12× slower** at n≤10⁵; CSR, counters, ablation | Active |
| `bmssp` (crates.io / lib.rs) | Rust | Specialized frontier | Claims paper asymptotics; little public wall-clock vs Dijkstra | Single release, Aug 2025 |
| `notJoon/bmssp` | Rust | Simple heap | Textbook port; no published benches | Dormant |
| `quantumrag/BMSSP` (`duan_sssp`) | Rust | Partial-order queue (not block-based) | Correctness / educational; author flags block queue as the big missing piece | Reference |
| `madaffrager/Bounded-Multi-Source-…` | Rust | — | Explicit theory-vs-practice analysis | Analysis |
| `pathfinding-indexed` | Rust | Experimental BMSSP | Own benches: **does not yet outperform Dijkstra** | Active, 2026 |
| `rap2363/ssps` (arXiv 2509.13448) | Rust | — | Lightning Network topology: **no significant practical speedup** | Paper-backed |
| `openSVM/bmssp-benchmark-game` | Many | Per-language contract | Cross-lang JSON metrics (`popped`, `edges_scanned`, `heap_pushes`, `B′`); README: *“BMSSP is the simplest dependable hammer; small-integer weights → buckets win”* | Active harness |
| `primaryaesthetics/logtwothirds` | Rust+Py | Faithful + engineered `bmssp-fast` | **Faithful 26–128× slower**; engineered **1.1–2× slower** (matrix up to ~5×) on m=4n, n=10⁴…10⁷; found completeness-lemma counterexample + repair | Most rigorous study |
| `beyond-dijkstra` (same authors) | Rust | 2026 sharpened bound | Bit-for-bit vs audited Python; first port of the follow-up paper | Newest |
| `bzantium/bmssp-python` | Python | Educational + `BmsspSolverV2` | Occasional wins (~25% on Google web graph); tied/lose elsewhere | Active |
| `hparreao/BMSSP-Python`, `BMSSPy` | Python | Faithful | Correctness / packaged comparison studies | Reference |
| `mfreeman451/bmssp-go` | Go | — | Optimistic speedup tables; not independently reproduced | Active |
| `Sirivasv/bmssp-js` | JS | Block list + recursion | Oracle-checked up to **2M nodes** | Active |
| `PatrickDiallo23/BMSSP-Java` | Java 21 | Full D + recursion | Instrumented harness vs Dijkstra | Active |
| `rvcgeeks/bmssp_c` | C++11 | Single-file | Reference / `.dot` graphs | Reference |

Apples-to-apples studies (same Dijkstra baseline they also ship): **logtwothirds**,
the LN paper, **openSVM**, **pathfinding-indexed**, and **this crate**.

### What the numbers actually say

1. **Theory–practice gap is large and universal.** logtwothirds (2026): faithful
   BMSSP is 26–128× slower than its own Dijkstra; after heavy engineering still
   ~1.1–2× slower on random graphs up to n=10⁷. LN paper: no statistically
   significant win on real Lightning topology.
2. **Wins are narrow.** Optimized Python V2 is competitive / ~25% faster on some
   large web graphs (Google 916K/5.1M), tied on Stanford, worse elsewhere —
   typically when the graph is large, reasonably dense, and bound `B` cuts off a
   large fraction of the frontier.
3. **Time goes into D, not edge relax.** openSVM’s counters (`popped`,
   `edges_scanned`, `heap_pushes`, `B′`) show D ops dominate. Flat heaps /
   naive buckets silently restore the sorting barrier the paper broke.

### Concrete levers (industry consensus)

| Priority | Lever | Why |
|---|---|---|
| 1 | **Block-based D** (paper Lemma 3.1 / 3.3) | Total Insert+BatchPrepend must be \(O(m\log^{2/3}n)\), not \(O(m\log n)\) |
| 2 | **CSR adjacency** | 1.5–3× typical for any SSSP from cache locality |
| 3 | **Sweep `t` / `k`** | Paper constants are proof artifacts; almost nobody profiles ±2× |
| 4 | **Honest instrumentation** | Attribute every change to pulls / pushes / relaxes / `B′` |
| 5 | **Dijkstra-oracle differential tests** | logtwothirds found a completeness-lemma counterexample in v1 pseudocode |
| 6 | **Still unshipped features** | Parallel BMSSP, incremental/dynamic queries, 2026 beyond-Dijkstra bound, multi-source benches (`|S|∈{1,8,64,512}`), real SNAP / LN graphs, WASM |

---

## This crate — what we have

```
src/
  graph.rs       CSR Graph {offsets, to, weight} + ER / grid / layered / power-law generators
  dijkstra.rs    Binary-heap Dijkstra baseline + counters
  params.rs      k = ⌊(log₂ n)^{1/3}⌋, t = ⌊(log₂ n)^{2/3}⌋, l = ⌈log₂ n / t⌉
  queue.rs       PartialQueue (BTreeMap) + BlockQueue (fixed blocks, differential tests)
  bmssp.rs       FindPivots / BaseCase / BMSSP recursion / BmsspEngine / driver
  transform.rs   Out-degree ≤ 2 vertex-split (theory device; usually slows practice)
  counters.rs    relax / heap / queue / pivots / partial-halt counts
  bin/bench_sssp.rs   release CLI → markdown table (+ BMSSP_SCALE asymptotics)
tests/           Dijkstra property tests, config ablation, handcrafted, transform
```

**Already in place relative to the landscape table:**

- CSR graphs (adjacency sorted by `(weight, to)`)
- Block queue *and* BTreeMap queue behind `QueueKind` / `BMSSP_QUEUE`
- Partial execution (`|U| > k·2^(l·t)`), leftover `BatchPrepend`, depth-stamped U
- Arena scratch buffers, FindPivots forest hardening (no zero-weight cycles)
- Dijkstra-oracle suite + config matrix (pivots × queue × partial)
- Operation counters suitable for openSVM-style attribution

**Documented deviations** (see `ALGORITHM.md`): strict-`<` updates with completeness
patched via W→queue / touched BaseCase returns / leftover prepend; every strict
improvement is routed (no paper interval skip); BaseCase returns boundary +
still-in-heap; constant-degree transform is out-degree only.

### Local bench snapshot (Session 6, 2026-09-11)

Ablation: pivots on, `BlockQueue` (**D₀ + decrease-key**), partial execution on,
release `lto=thin` (Linux).

| family | n | m | dijk (ms) | bmssp (ms) | speedup | verified |
|---|---|---|---|---|---|---|
| er_c2 | 10⁴ | 2·10⁴ | 0.92 | 12.4 | 0.07 | ✓ |
| er_c4 | 10⁴ | 4·10⁴ | 1.54 | 17.8 | 0.09 | ✓ |
| er_c8 | 10⁴ | 8·10⁴ | 2.23 | 18.3 | 0.12 | ✓ |
| er_c4_1e5 | 10⁵ | 4·10⁵ | 34.4 | 313 | 0.11 | ✓ |
| grid_316 | ~10⁵ | ~2·10⁵ | 12.6 | 173 | 0.07 | ✓ |
| layered | 10⁵ | ~4·10⁵ | 20.5 | 444 | 0.05 | ✓ |
| er_c4_real | 10⁴ | 4·10⁴ | 1.59 | 33.4 | 0.05 | ✓ |

**Takeaway:** correctness is solid (`verified=true`, including real weights).
D₀ BatchPrepend is in; Pull still rebuilds from the live set, so Dijkstra
remains ahead. Next DS win is O(|S′|) prefix Pull on top of D₀.

---

## Quick start

```bash
# tests (oracle + ablation + handcrafted + transform)
cargo test --locked

# release bench vs Dijkstra (pinned seeds in src/bin/bench_sssp.rs)
BMSSP_BENCH_ITERS=1 BMSSP_PARTIAL=1 BMSSP_QUEUE=block \
  cargo run --release --bin bench_sssp

# optional knobs
#   BMSSP_NO_PIVOTS=1      disable FindPivots
#   BMSSP_QUEUE=map       BTreeMap PartialQueue instead of BlockQueue
#   BMSSP_SCALE=1         n≈1e4..1e6 + asymptotic normalizers
#   BMSSP_TRACE=1         relax trace (debug)
```

Library entry points:

```rust
use bmssp_rs::bmssp::{barrier_breaker_sssp, BmsspConfig, BmsspEngine};
use bmssp_rs::dijkstra::dijkstra;
use bmssp_rs::counters::Counters;
use bmssp_rs::graph::er_random;
```

CI (`.github/workflows/ci.yml`): `fmt` + `clippy -D warnings` + `cargo test --locked`.

---

## Improvement roadmap (what we want to try next)

Ordered by expected payoff for *this* Rust crate, given what peers already did:

1. ~~**True `D₀` BatchPrepend + decrease-key**~~ — **done (2026-09-11).**
   `BlockQueue` now has a real `D₀` front list (fast path when prepends sit
   below the live min) plus lazy decrease-key via `best`. See `ALGORITHM.md`.
2. **`t` / `k` parameter sweep** — novel data point almost nobody publishes;
   wire into `bench_sssp` / `BmsspConfig`.
3. **Multi-source microbench** — BMSSP’s real API is bounded *multi*-source;
   measure `|S| ∈ {1, 8, 64, 512}` instead of only SSSP.
4. **openSVM-shaped JSON metrics** — emit `popped`, `edges_scanned`, `heap_pushes`,
   `B′` for cross-repo comparison.
5. **Real-graph suite** — SNAP (Stanford / Google / Pokec), LN topology from
   arXiv 2509.13448; leave synthetic-only behind.
6. **Parallel top-of-tree** — `rayon` over independent subproblems at the first
   1–2 recursion levels (nobody has shipped this cleanly).
7. **Port / gate the 2026 beyond-Dijkstra sharpening** — only `beyond-dijkstra`
   has it today.
8. **Scale to n=10⁷** on a bigger machine with `BMSSP_SCALE` and record whether
   the gap shrinks with \(n\).
9. **O(|S′|) prefix Pull** — current Pull selects from the full live set (correct,
   simple); restoring paper-style prefix-of-blocks Pull is the next DS polish.

Guardrail (unchanged): **distances must match Dijkstra**; report wins and losses
honestly; never tune tables to hide a loss.

---

## References

- Duan, Mao, Mao, Shu, Yin — *Breaking the Sorting Barrier…*, arXiv:2504.17033
- Lightning Network BMSSP study — arXiv:2509.13448 (`rap2363/ssps`)
- `primaryaesthetics/logtwothirds` — most careful theory/practice + lemma repair
- `openSVM/bmssp-benchmark-game` — cross-language instrumented harness
- Follow-up sharpening — `beyond-dijkstra` (Feb 2026)

---

*README survey timestamp: 2026-09-11. Bench numbers above are from Session 5
(2026-08-18); re-run `bench_sssp` to refresh `BENCHMARKS.md` on your machine.*
