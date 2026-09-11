# ALGORITHM — BMSSP as implemented

Faithful-but-pragmatic port of Duan–Mao–Mao–Shu–Yin, *Breaking the Sorting Barrier
for Directed Single-Source Shortest Paths* (arXiv:2504.17033, STOC 2025).

## Parameters

From `params(n)` (base-2 logs so the top-level workload bound dominates `n`):

- `k = ⌊(log₂ n)^{1/3}⌋`
- `t = ⌊(log₂ n)^{2/3}⌋`
- `l = ⌈log₂ n / t⌉`

each at least 1. Top level runs `BMSSP(l, B=∞, S={src})` with `d̂[src]=0`.

## Subroutines

### FindPivots(B, S) — Algorithm 1 / Lemma 3.2

Up to `k` rounds of bounded Bellman–Ford from the previous frontier. Each round
rebuilds `W_i` from ≤-relaxations with `cand < B` (set semantics: a vertex may
reappear so improvements keep propagating). Early stop when `|W| > k|S|` ⇒
`P = S`. Otherwise build a predecessor forest `F` on equality edges among `W`,
**restricted to parents that appear earlier in W** (BF discovery order) so
zero-weight cycles cannot corrupt subtree sizes. `P` = seeds that are roots of
subtrees of size `≥ k` (fallback `P = S` if empty).

### BaseCase(B, S) — Algorithm 2

Multi-source mini-Dijkstra (paper assumes singleton `S`; we accept a Pull bucket).
Settle on extract-min; stop after `k+1` settlements.  
`B' = B` if `|U₀| ≤ k`, else `max d̂ in U₀`.  
**Deviation:** return all settled vertices (including the boundary) plus
still-in-heap discoveries, so callers never lose out-edges under strict-`<`
routing / tie buckets.

### BMSSP(l, B, S) — Algorithm 3

- `l = 0` → BaseCase.
- Else FindPivots → seed partial-order queue `D` with pivots **and** all of `W`
  (deviation vs Remark 3.4 equality re-insertion; required under our routing).
- Loop: `Pull → recurse(l-1) →` BatchPrepend `S_i` leftovers with
  `d̂ ∈ [B'_i, B_i)` (skip if already in `U`) `→` relax `U_i`, route strict
  improvements to batch (`< B_i`) or insert (`≥ B_i`).
- Partial execution (optional): halt when `|U| > k·2^(l·t)`, drain `D` into `U`,
  return `B' = last child B'_i`. Successful completion returns `B' = B`.

### Queues

- `PartialQueue`: `BTreeMap` by exact key; Pull = one smallest key-bucket.
- `BlockQueue`: Lemma 3.3-style blocks of size `M = 2^((l-1)t)`:
  - **`D₀`**: front `VecDeque` of blocks that only receive BatchPrepend when
    every new key is strictly below the current live minimum (paper contract).
    Otherwise falls back to Insert into `D₁` (keeps random fuzz / odd routing safe).
  - **`D₁`**: BST-keyed blocks for ordinary Insert.
  - **No decrease-key:** a vertex may appear at several keys (lazy Dijkstra-style).
    Dropping the prior larger key when a better one arrives breaks BMSSP retries
    under a wider child bound (Codex P1 on PR #6).
  - Pull = `M` smallest values with ties taken whole. Currently materializes the
    physical multiset then rebuilds leftovers into `D₁` (correct; paper-style
    O(|S′|) prefix Pull is next). Insert uses a strict `≥ B_i` cut in `bmssp.rs`
    (not `B_i - EPS`) so real weights cannot hide keys behind D₀.

## Documented deviations from the paper

1. Strict-`<` distance updates (lazy heaps + zero-weight safety) instead of ≤
   everywhere; completeness patched by W→queue, touched returns, leftover prepend.
2. Route every strict improvement (no `cand < B'_i` skip); Insert cut is
   exact `cand >= B_i` (not `B_i - EPS`) for BlockQueue D₀/D₁ safety.
3. BaseCase returns boundary + still-in-heap, not paper `U = {v : d̂ < B'}`.
4. Depth-stamped U membership (not a shared epoch array) across recursion.
5. Constant-degree transform is **out-degree only** (`transform.rs`).

## Complexity note

The paper’s `O(m log^{2/3} n)` claim assumes constant degree, the Lemma 3.3
queue with real `D₀` BatchPrepend, and the local contracts above. This crate
targets **correct distances first**, then constant-factor engineering. At
runnable sizes, tuned binary-heap Dijkstra remains faster (see `BENCHMARKS.md`).
