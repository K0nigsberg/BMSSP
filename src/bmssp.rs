use std::collections::BinaryHeap;

use crate::counters::Counters;
use crate::dijkstra::{MinState, INF};
use crate::graph::Graph;
use crate::params::params;
use crate::queue::{BlockQueue, PartialQueue, QueueOps};

const EPS: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    BTreeMap,
    Block,
}

#[derive(Debug, Clone)]
pub struct BmsspConfig {
    pub k: usize,
    pub t: usize,
    pub l: usize,
    /// ablation: set false to skip FindPivots (P = S, W = empty).
    pub use_pivots: bool,
    /// ablation: halt a level once it has completed > k*2^(l*t) vertices
    /// (Lemma 3.1 partial execution). false = run to D-empty (B' = B always).
    pub partial_execution: bool,
    /// queue implementation behind the partial-order structure.
    pub queue_impl: QueueKind,
}

impl BmsspConfig {
    pub fn from_n(n: usize) -> Self {
        let (k, t, l) = params(n);
        BmsspConfig {
            k,
            t,
            l,
            use_pivots: true,
            partial_execution: false,
            queue_impl: QueueKind::BTreeMap,
        }
    }

    /// k * 2^(l*t): the Lemma 3.1 partial-execution workload bound for a call
    /// at level `l`. Saturates to usize::MAX instead of overflowing.
    pub fn partial_limit(&self, l: usize) -> usize {
        let shift = l * self.t;
        if shift >= usize::BITS as usize {
            usize::MAX
        } else {
            self.k.saturating_mul(1usize << shift)
        }
    }

    /// Lemma 3.3 block size M = 2^((l-1)*t) for a call at level `l >= 1`.
    pub fn block_size(&self, l: usize) -> usize {
        if l == 0 {
            return 1;
        }
        let shift = (l - 1) * self.t;
        if shift >= usize::BITS as usize {
            usize::MAX
        } else {
            1usize << shift
        }
    }
}

pub struct BmsspEngine<'a> {
    g: &'a Graph,
    dist: Vec<f64>,
    cfg: BmsspConfig,
    counters: &'a mut Counters,
    /// Epoch stamp for BaseCase / FindPivots "visited in this call".
    marked: Vec<u64>,
    /// Recursion-depth stamp for U membership. Children write a *deeper*
    /// depth, so they cannot clobber a parent's U marks (Session 4 bug).
    u_depth: Vec<u32>,
    parent: Vec<u32>,
    sub: Vec<usize>,
    /// Discovery index in FindPivots' W (1-based; 0 = not in W). Used to force
    /// the pivot forest to be acyclic under zero-weight equality edges.
    w_index: Vec<u32>,
    epoch: u64,
    call_depth: u32,
    /// Recycled scratch buffers (arena) to cut per-call allocation.
    pool_u32: Vec<Vec<u32>>,
    pool_pairs: Vec<Vec<(u32, f64)>>,
    trace: bool,
}

/// Top-level driver: BMSSP(l, B = infinity, S = {src}).
pub fn barrier_breaker_sssp(g: &Graph, src: u32, counters: &mut Counters) -> Vec<f64> {
    let cfg = BmsspConfig::from_n(g.n);
    BmsspEngine::new(g, cfg, counters).run(src)
}

impl<'a> BmsspEngine<'a> {
    pub fn new(g: &'a Graph, cfg: BmsspConfig, counters: &'a mut Counters) -> Self {
        BmsspEngine {
            g,
            dist: vec![INF; g.n],
            cfg,
            counters,
            marked: vec![0; g.n],
            u_depth: vec![0; g.n],
            parent: vec![u32::MAX; g.n],
            sub: vec![0; g.n],
            w_index: vec![0; g.n],
            epoch: 0,
            call_depth: 0,
            pool_u32: Vec::new(),
            pool_pairs: Vec::new(),
            trace: std::env::var("BMSSP_TRACE").is_ok(),
        }
    }

    fn take_u32(&mut self) -> Vec<u32> {
        self.pool_u32.pop().unwrap_or_default()
    }

    fn put_u32(&mut self, mut v: Vec<u32>) {
        v.clear();
        if v.capacity() > 0 {
            self.pool_u32.push(v);
        }
    }

    fn take_pairs(&mut self) -> Vec<(u32, f64)> {
        self.pool_pairs.pop().unwrap_or_default()
    }

    fn put_pairs(&mut self, mut v: Vec<(u32, f64)>) {
        v.clear();
        if v.capacity() > 0 {
            self.pool_pairs.push(v);
        }
    }

    #[inline]
    fn u_add(&mut self, u_total: &mut Vec<u32>, depth: u32, v: u32) -> bool {
        if self.u_depth[v as usize] != depth {
            self.u_depth[v as usize] = depth;
            u_total.push(v);
            true
        } else {
            false
        }
    }

    pub fn run(&mut self, src: u32) -> Vec<f64> {
        assert!(src < self.g.n as u32, "source out of range");
        self.dist.fill(INF);
        self.dist[src as usize] = 0.0;
        self.call_depth = 0;
        let l = self.cfg.l;
        let (_, u) = self.bmssp(l, INF, &[src]);
        self.put_u32(u);
        self.dist.clone()
    }

    /// BMSSP(l, B, S) -> (B', U). With `partial_execution` off (default) it runs
    /// to D-empty and B' = B. With it on, a level halts once it has completed
    /// more than k*2^(l*t) vertices and returns B' = the last child's boundary.
    ///
    /// Recursion depth is O(l) = O(log^{1/3} n), so an explicit iterative stack
    /// is unnecessary; Phase 4 instead removes HashSet allocs via depth-stamped
    /// U membership and recycles scratch buffers through an arena pool.
    fn bmssp(&mut self, l: usize, b: f64, s: &[u32]) -> (f64, Vec<u32>) {
        debug_assert!(!s.is_empty());
        self.counters.recursive_calls += 1;
        if l == 0 {
            return self.base_case(b, s);
        }

        self.call_depth += 1;
        let depth = self.call_depth;

        let (pivots, wset) = if self.cfg.use_pivots {
            self.find_pivots(b, s)
        } else {
            (s.to_vec(), Vec::new())
        };

        let mut pq = self.make_queue(l, b);
        let initial_items = if self.cfg.use_pivots { &wset } else { &pivots };
        for &x in initial_items {
            if self.dist[x as usize] < b - EPS {
                pq.insert(x, self.dist[x as usize]);
                self.counters.queue_insert += 1;
            }
        }

        let partial_limit = self.cfg.partial_limit(l);
        let mut u_total = self.take_u32();
        let mut halted = false;
        let mut last_bp = b;
        while !pq.is_empty() {
            // Halt once |U| has exceeded k·2^(l·t). Using `>` (not `>=`) is
            // load-bearing at the top level where the bound equals n.
            if self.cfg.partial_execution && u_total.len() > partial_limit {
                halted = true;
                if self.trace {
                    eprintln!(
                        "HALT l={l} b={b} |U|={} limit={partial_limit} last_bp={last_bp}",
                        u_total.len()
                    );
                }
                for (v, _) in pq.drain() {
                    self.u_add(&mut u_total, depth, v);
                }
                break;
            }
            let (si, bi) = pq.pull();
            self.counters.queue_pull += 1;
            if si.is_empty() {
                continue;
            }
            let (bp_i, mut ui) = self.bmssp(l - 1, bi, &si);
            last_bp = bp_i;
            for &x in &ui {
                self.u_add(&mut u_total, depth, x);
            }

            // Route every strict improvement: [0, B_i) -> batch, [B_i, B) -> insert
            // (exact B_i cut; see Insert branch below). BatchPrepend S_i leftovers
            // with dhat in [B'_i, B_i) (Alg. 3 line 25).
            let mut to_batch = self.take_pairs();
            if bp_i < bi - EPS {
                for &x in &si {
                    let dx = self.dist[x as usize];
                    if dx >= bp_i - EPS && dx < bi - EPS && self.u_depth[x as usize] != depth {
                        to_batch.push((x, dx));
                    }
                }
            }
            for &u in &ui {
                let du = self.dist[u as usize];
                for i in self.g.edge_range(u as usize) {
                    let v = self.g.to[i];
                    let w = self.g.weight[i];
                    self.counters.relaxations += 1;
                    let cand = du + w;
                    if cand < b - EPS && cand < self.dist[v as usize] - EPS {
                        self.dist[v as usize] = cand;
                        self.parent[v as usize] = u;
                        if self.trace {
                            eprintln!("R: {u} -> {v} cand={cand} b={b} bi={bi} bp={bp_i}");
                        }
                        // Strict `>= B_i` (not `B_i - EPS`): a key in
                        // `[B_i - EPS, B_i)` must BatchPrepend into D0. Sending
                        // it through Insert with BlockQueue's D0/D1 split can
                        // hide it behind larger D0 leftovers under real weights.
                        if cand >= bi {
                            pq.insert(v, cand);
                            self.counters.queue_insert += 1;
                        } else {
                            to_batch.push((v, cand));
                        }
                    }
                }
            }
            self.put_u32(std::mem::take(&mut ui));
            if !to_batch.is_empty() {
                pq.batch_prepend(&to_batch);
                self.counters.queue_batch_prepend += 1;
            }
            self.put_pairs(to_batch);
        }

        let b_prime = if halted {
            self.counters.partial_executions += 1;
            last_bp
        } else {
            b
        };
        for &x in &wset {
            if self.dist[x as usize] < b_prime - EPS {
                self.u_add(&mut u_total, depth, x);
            }
        }
        self.put_u32(wset);
        self.put_u32(pivots);
        self.call_depth -= 1;
        (b_prime, u_total)
    }

    /// BaseCase (Algorithm 2), generalised to a multi-source seed set. Bounded
    /// mini-Dijkstra that settles at most k+1 vertices (seeds included).
    ///
    /// Returns `(B', U)` where B' is B when `|U0| <= k`, else `max dhat in U0`.
    /// Paper U is `{v in U0 : dhat[v] < B'}`; we also append still-in-heap
    /// discoveries (a documented safety deviation) so a caller never skips
    /// out-edges of vertices whose distances this call already improved.
    fn base_case(&mut self, b: f64, s: &[u32]) -> (f64, Vec<u32>) {
        self.counters.base_case_calls += 1;
        self.epoch += 1;
        let e = self.epoch;

        let mut u0 = self.take_u32();
        let mut heap: BinaryHeap<MinState> = BinaryHeap::new();
        // Seeds go on the heap only; they are counted as settled on extract so
        // the k+1 cap and B' track real Dijkstra settlements (not just |S|).
        for &x in s {
            if self.dist[x as usize] < b - EPS {
                heap.push(MinState {
                    cost: self.dist[x as usize],
                    node: x,
                });
                self.counters.heap_insert += 1;
            }
        }

        while let Some(MinState { cost, node }) = heap.pop() {
            self.counters.heap_extract_min += 1;
            if cost > self.dist[node as usize] + EPS {
                continue;
            }
            if cost >= b - EPS {
                break;
            }
            if self.marked[node as usize] == e {
                continue; // already settled in this call
            }
            self.marked[node as usize] = e;
            u0.push(node);

            // Relax from this settlement (including the (k+1)-th / boundary).
            for i in self.g.edge_range(node as usize) {
                let v = self.g.to[i];
                let w = self.g.weight[i];
                self.counters.relaxations += 1;
                let cand = cost + w;
                // Strict < (not paper's ≤) avoids zero-weight churn on the lazy
                // heap; Remark 3.4 equality re-use is handled by upper-level
                // routing deviations instead.
                if cand < self.dist[v as usize] - EPS && cand < b - EPS {
                    self.dist[v as usize] = cand;
                    self.parent[v as usize] = node;
                    if self.trace {
                        eprintln!("B: {node} -> {v} cand={cand} b={b}");
                    }
                    heap.push(MinState {
                        cost: cand,
                        node: v,
                    });
                    self.counters.heap_insert += 1;
                }
            }
            if u0.len() > self.cfg.k {
                break;
            }
        }

        let bprime = if u0.len() <= self.cfg.k {
            b
        } else {
            u0.iter()
                .map(|&v| self.dist[v as usize])
                .fold(f64::NEG_INFINITY, f64::max)
        };

        // Paper returns {v in U0 : dhat < B'}, excluding the boundary. We
        // return all settled vertices (including boundary) plus still-in-heap
        // discoveries: excluding the boundary re-introduces an infinite
        // re-prepend loop on equal-key / zero-weight tie buckets (PROGRESS
        // Session 1), and still-in-heap vertices must not lose their out-edges.
        let mut result = u0;
        while let Some(MinState { cost, node }) = heap.pop() {
            if cost > self.dist[node as usize] + EPS {
                continue;
            }
            if self.marked[node as usize] != e && self.dist[node as usize] < b - EPS {
                self.marked[node as usize] = e;
                result.push(node);
            }
        }
        (bprime, result)
    }

    /// Construct the partial-order queue for a call at level `l` (Lemma 3.3
    /// block size M = 2^((l-1)t); ignored by the BTreeMap implementation).
    fn make_queue(&self, l: usize, b: f64) -> QueueOps {
        match self.cfg.queue_impl {
            QueueKind::BTreeMap => QueueOps::Map(PartialQueue::new(b)),
            QueueKind::Block => QueueOps::Block(BlockQueue::new(b, self.cfg.block_size(l))),
        }
    }

    /// FindPivots (Algorithm 1 / Lemma 3.2): k rounds of bounded Bellman-Ford
    /// from seeds S building a predecessor forest F. W = visited set; P = roots
    /// in S whose F-subtree has >= k vertices (or P = S on early stop / fallback).
    fn find_pivots(&mut self, b: f64, s: &[u32]) -> (Vec<u32>, Vec<u32>) {
        self.counters.find_pivots_calls += 1;
        self.epoch += 1;
        let e = self.epoch;

        let mut w = self.take_u32();
        let mut frontier = self.take_u32();
        for &x in s {
            if self.dist[x as usize] < b - EPS {
                if self.marked[x as usize] != e {
                    self.marked[x as usize] = e;
                    w.push(x);
                }
                frontier.push(x);
            }
        }

        let mut early_stop = false;
        for _ in 0..self.cfg.k {
            if frontier.is_empty() {
                break;
            }
            // Paper Alg. 1: W_i is rebuilt each round from ≤-relaxations out of
            // W_{i-1} (set semantics). A vertex may reappear so an improved (or
            // equal) distance keeps propagating for the remaining rounds.
            let mut next = self.take_u32();
            for &u in &frontier {
                let du = self.dist[u as usize];
                for i in self.g.edge_range(u as usize) {
                    let v = self.g.to[i];
                    let wt = self.g.weight[i];
                    self.counters.relaxations += 1;
                    let cand = du + wt;
                    if cand < b - EPS && cand <= self.dist[v as usize] + EPS {
                        if cand < self.dist[v as usize] - EPS {
                            self.dist[v as usize] = cand;
                        }
                        if self.marked[v as usize] != e {
                            self.marked[v as usize] = e;
                            w.push(v);
                        }
                        next.push(v);
                    }
                }
            }
            next.sort_unstable();
            next.dedup();
            self.put_u32(frontier);
            frontier = next;
            if w.len() > self.cfg.k * s.len().max(1) {
                early_stop = true;
                break;
            }
        }
        self.put_u32(frontier);

        let pivots = if early_stop || w.is_empty() {
            s.to_vec()
        } else {
            // Forest F: equality edges among W, but only parent←earlier-in-W so
            // zero-weight cycles cannot form (W is built in BF discovery order).
            for (i, &x) in w.iter().enumerate() {
                self.parent[x as usize] = u32::MAX;
                self.w_index[x as usize] = (i + 1) as u32; // 1-based
                self.sub[x as usize] = 1; // clear+init only W, not O(n) fill
            }
            for &u in &w {
                let du = self.dist[u as usize];
                let u_ord = self.w_index[u as usize];
                for i in self.g.edge_range(u as usize) {
                    let v = self.g.to[i];
                    let wt = self.g.weight[i];
                    let cand = du + wt;
                    if self.marked[v as usize] == e
                        && self.w_index[v as usize] > u_ord
                        && (cand - self.dist[v as usize]).abs() <= EPS
                        && (self.parent[v as usize] == u32::MAX || u < self.parent[v as usize])
                    {
                        self.parent[v as usize] = u;
                    }
                }
            }
            // Reverse W order is a valid postorder when edges only go forward.
            for &v in w.iter().rev() {
                let p = self.parent[v as usize];
                if p != u32::MAX {
                    self.sub[p as usize] += self.sub[v as usize];
                }
            }
            let mut pivots: Vec<u32> = Vec::new();
            for &x in s {
                if self.marked[x as usize] == e
                    && self.parent[x as usize] == u32::MAX
                    && self.sub[x as usize] >= self.cfg.k
                {
                    pivots.push(x);
                }
            }
            // Clear w_index for W so a later FindPivots cannot see stale ranks.
            for &x in &w {
                self.w_index[x as usize] = 0;
            }
            if pivots.is_empty() {
                s.to_vec()
            } else {
                pivots
            }
        };
        self.counters.pivots_found += pivots.len() as u64;
        (pivots, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    fn chain(n: usize) -> Graph {
        let mut edges = Vec::with_capacity(n.saturating_sub(1));
        for i in 0..n.saturating_sub(1) as u32 {
            edges.push((i, i + 1, 1.0));
        }
        Graph::from_edges(n, &edges)
    }

    #[test]
    fn base_case_caps_settlements_and_returns_bprime() {
        // Long chain, k=1: BaseCase must settle at most k+1=2 vertices and
        // return B' = max dhat in U0 (< B), not run a full Dijkstra to B.
        let g = chain(20);
        let cfg = BmsspConfig {
            k: 1,
            t: 1,
            l: 0,
            use_pivots: false,
            partial_execution: false,
            queue_impl: QueueKind::BTreeMap,
        };
        let mut c = Counters::new();
        let mut eng = BmsspEngine::new(&g, cfg, &mut c);
        eng.dist[0] = 0.0;
        let (bp, u) = eng.base_case(INF, &[0]);
        assert!(
            bp.is_finite() && bp < INF,
            "expected B' < B after hitting k+1 settlements, got {bp}"
        );
        assert!(
            (bp - 1.0).abs() < 1e-12,
            "with k=1 on a unit chain, B' should be dist of the 2nd settlement (=1), got {bp}"
        );
        // We return all settled vertices (incl. boundary) plus still-in-heap.
        assert!(
            u.contains(&0) && u.contains(&1),
            "both settlements must be in U; got {u:?}"
        );
        // Vertices far down the chain must remain untouched by this leaf call.
        assert!(
            eng.dist[10].is_infinite(),
            "base_case must not settle the whole chain; dist[10]={}",
            eng.dist[10]
        );
    }

    #[test]
    fn base_case_small_k_matches_dijkstra_via_recursion() {
        // With l>=1 the parent re-queues work via leftovers / routing; end-to-end
        // distances on a chain must still match Dijkstra. Top-level partial bound
        // must dominate n (here k·2^(l·t) = 1·2^6 = 64 >= 50).
        let g = chain(50);
        let cfg = BmsspConfig {
            k: 1,
            t: 1,
            l: 6,
            use_pivots: true,
            partial_execution: true,
            queue_impl: QueueKind::BTreeMap,
        };
        let mut c = Counters::new();
        let got = BmsspEngine::new(&g, cfg, &mut c).run(0);
        let exp: Vec<f64> = (0..50).map(|i| i as f64).collect();
        assert_eq!(got, exp);
    }

    #[test]
    fn find_pivots_propagates_improvement_across_rounds() {
        // 0 -> 1 (10), 0 -> 2 (1), 2 -> 1 (1): after round 1, dist[1]=10 via
        // the direct edge; a later round must re-expand 2 and improve dist[1]
        // to 2. Use k=3 so |W|=3 does not trip the early-stop |W| > k|S|.
        let g = Graph::from_edges(3, &[(0, 1, 10.0), (0, 2, 1.0), (2, 1, 1.0)]);
        let cfg = BmsspConfig {
            k: 3,
            t: 1,
            l: 1,
            use_pivots: true,
            partial_execution: false,
            queue_impl: QueueKind::BTreeMap,
        };
        let mut c = Counters::new();
        let mut eng = BmsspEngine::new(&g, cfg, &mut c);
        eng.dist[0] = 0.0;
        let (_p, w) = eng.find_pivots(INF, &[0]);
        assert!(
            w.contains(&1) && w.contains(&2),
            "W should contain both 1 and 2"
        );
        assert!(
            (eng.dist[1] - 2.0).abs() < 1e-12,
            "FindPivots must improve 0→1 via 2 within k rounds; dist[1]={}",
            eng.dist[1]
        );
    }

    #[test]
    fn si_leftovers_requeued_when_child_returns_bprime() {
        // Aggressive k=1 with partial execution on a longer chain: children hit
        // the k+1 base-case boundary (B' < B_i) and must re-prepend S_i
        // leftovers; otherwise the tail of the chain stays unreachable.
        let g = chain(30);
        let cfg = BmsspConfig {
            k: 1,
            t: 1,
            l: 5, // k·2^(l·t) = 32 >= 30
            use_pivots: false,
            partial_execution: true,
            queue_impl: QueueKind::Block,
        };
        let mut c = Counters::new();
        let got = BmsspEngine::new(&g, cfg, &mut c).run(0);
        let exp: Vec<f64> = (0..30).map(|i| i as f64).collect();
        assert_eq!(got, exp);
    }

    #[test]
    fn partial_with_pivots_no_spurious_top_level_halt() {
        // Regression: shared marked_u/u_epoch was clobbered by recursive
        // children, inflating |U| past k·2^(l·t) and halting the top level with
        // B' << ∞ (lost vertices). from_n(200) on n=400 previously failed for
        // partial+pivots+BTreeMap (vertex reachable by Dijkstra, INF in BMSSP).
        // Lower-level partials are allowed; distances must still match.
        use crate::dijkstra::dijkstra;
        use crate::graph::{er_random, WeightDist};
        let g = er_random(400, 4, 1, &WeightDist::Int { min: 1, max: 10 });
        let cfg = BmsspConfig {
            partial_execution: true,
            use_pivots: true,
            queue_impl: QueueKind::BTreeMap,
            ..BmsspConfig::from_n(200)
        };
        let mut c = Counters::new();
        let got = BmsspEngine::new(&g, cfg, &mut c).run(0);
        let exp = dijkstra(&g, 0, &mut Counters::new());
        let reachable_d = exp.iter().filter(|d| d.is_finite()).count();
        let reachable_b = got.iter().filter(|d| d.is_finite()).count();
        assert_eq!(
            reachable_b, reachable_d,
            "lost reachability under partial+pivots (d={reachable_d} b={reachable_b}, partials={})",
            c.partial_executions
        );
        for i in 0..g.n {
            assert_eq!(
                got[i], exp[i],
                "dist mismatch at {i}: bmssp={} dijk={}",
                got[i], exp[i]
            );
        }
    }

    #[test]
    fn find_pivots_forest_rejects_zero_weight_cycles() {
        // 0→1 (0), 1→0 (0), 0→2 (1): equality edges 0↔1 must not form a cycle
        // in F; with W-order parents, 1's parent is 0 and 0 stays a root.
        // k=3 so |W|=3 does not trip early-stop (|W| > k|S|).
        let g = Graph::from_edges(3, &[(0, 1, 0.0), (1, 0, 0.0), (0, 2, 1.0)]);
        let cfg = BmsspConfig {
            k: 3,
            t: 1,
            l: 1,
            use_pivots: true,
            partial_execution: false,
            queue_impl: QueueKind::BTreeMap,
        };
        let mut c = Counters::new();
        let mut eng = BmsspEngine::new(&g, cfg, &mut c);
        eng.dist[0] = 0.0;
        let (_pivots, w) = eng.find_pivots(INF, &[0]);
        assert!(w.contains(&0) && w.contains(&1) && w.contains(&2));
        assert_eq!(eng.parent[0], u32::MAX, "seed must remain a forest root");
        assert_eq!(eng.parent[1], 0, "1 should parent to earlier W vertex 0");
        // Even after later rounds re-see 1→0, W-order forbids parent[0]=1.
        assert_ne!(eng.parent[0], 1);
    }

    #[test]
    fn limits_saturate_gracefully_on_large_shifts() {
        let cfg = BmsspConfig {
            k: 10,
            t: 10,
            l: 10,
            use_pivots: true,
            partial_execution: true,
            queue_impl: QueueKind::BTreeMap,
        };
        // l * t = 100 >= usize::BITS
        assert_eq!(cfg.partial_limit(10), usize::MAX);
        assert_eq!(cfg.block_size(10), usize::MAX);
        assert_eq!(cfg.block_size(0), 1);
    }
}
