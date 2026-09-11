use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};

/// Total-order wrapper for f64 keys (no NaN; `total_cmp` gives a total order).
#[derive(Debug, Clone, Copy)]
struct K(f64);

impl PartialEq for K {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for K {}

impl Ord for K {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for K {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Phase-1 partial-order queue (Lemma 3.3 semantics, BTreeMap-backed).
///
/// Values are key/value pairs (value = current `dhat`). Operations:
/// - `insert(v, key)`: add a pair.
/// - `pull()`: remove the smallest bucket (all pairs with the smallest key) and
///   return it together with the separation bound `B_i` = smallest remaining key
///   (or the queue's upper bound `B` if empty).
/// - `batch_prepend(items)`: front-load a batch of smaller keys (via insert into
///   the map, which sorts them ahead of existing larger keys).
///
/// A vertex may appear at several keys; buckets are drained whole to preserve
/// the separation invariant. Asymptotically this is O(log Q) per op; the block
/// structure of the paper (`BlockQueue`) is the Phase-3 upgrade.
pub struct PartialQueue {
    map: BTreeMap<K, Vec<u32>>,
    bound: f64,
}

impl PartialQueue {
    pub fn new(bound: f64) -> Self {
        PartialQueue {
            map: BTreeMap::new(),
            bound,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn insert(&mut self, v: u32, key: f64) {
        debug_assert!(key.is_finite());
        self.map.entry(K(key)).or_default().push(v);
    }

    /// Returns (bucket, B_i): the smallest bucket and the bound of the rest.
    pub fn pull(&mut self) -> (Vec<u32>, f64) {
        if let Some((_, mut bucket)) = self.map.pop_first() {
            let bi = self.map.keys().next().map(|k| k.0).unwrap_or(self.bound);
            (std::mem::take(&mut bucket), bi)
        } else {
            (Vec::new(), self.bound)
        }
    }

    pub fn batch_prepend(&mut self, items: &[(u32, f64)]) {
        for &(v, k) in items {
            debug_assert!(k.is_finite());
            self.map.entry(K(k)).or_default().push(v);
        }
    }

    /// Remove and return everything still queued.
    pub fn drain(&mut self) -> Vec<(u32, f64)> {
        let mut out = Vec::new();
        for (k, vs) in std::mem::take(&mut self.map) {
            for v in vs {
                out.push((v, k.0));
            }
        }
        out
    }
}

/// Identity key for a block: `(upper bound, sequence)` so two blocks sharing an
/// upper bound remain distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UbKey {
    ub: K,
    seq: u64,
}

impl Ord for UbKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ub
            .cmp(&other.ub)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for UbKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A block: at most `m` key/value pairs, unordered inside, with `ub` an upper
/// bound on all values (we keep it exact at insertion time; it may go stale
/// high after Pull deletions, which is safe for the ordering invariant).
#[derive(Debug)]
struct Block {
    items: Vec<(u32, f64)>,
    ub: f64,
}

/// Partition an overflowing block into two blocks that keep the inter-block
/// value ordering: everything strictly below the block's max goes to the lower
/// half and the max-valued pairs to the upper half, so the upper half never
/// carries a value smaller than the max of any block that precedes it. If the
/// block's values are all equal the median split is used instead (either half
/// is valid then).
fn split_block(mut blk: Block) -> (Block, Block) {
    blk.items.sort_by(|a, b| a.1.total_cmp(&b.1));
    let below = blk.items.partition_point(|it| it.1 < blk.ub);
    let mid = if below == 0 || below == blk.items.len() {
        blk.items.len() / 2
    } else {
        below
    };
    let right = blk.items.split_off(mid);
    let ub_l = blk
        .items
        .iter()
        .map(|it| it.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let ub_r = right
        .iter()
        .map(|it| it.1)
        .fold(f64::NEG_INFINITY, f64::max);
    (
        Block {
            items: blk.items,
            ub: ub_l,
        },
        Block {
            items: right,
            ub: ub_r,
        },
    )
}

/// Block-based partial-order queue (Lemma 3.3).
///
/// - `D0` (`d0`): prepend-only front blocks for BatchPrepend when every new key
///   is strictly below the current physical minimum (paper contract).
/// - `D1` (`blocks`): BST-keyed blocks for ordinary Insert.
///
/// Vertices may appear at several keys (lazy Dijkstra-style). We deliberately
/// do **not** decrease-key / drop the prior entry when a better key arrives:
/// BMSSP can still need the older, larger key as a retry under a wider child
/// bound (see Codex review on PR #6).
///
/// Pull drains D0 completely before reading D1, then parks leftovers in D1.
/// If BatchPrepend breaks the "< min" contract, flush D0 into D1 and Insert.
#[derive(Debug)]
pub struct BlockQueue {
    d0: VecDeque<Block>,
    blocks: BTreeMap<UbKey, Block>,
    bound: f64,
    m: usize,
    seq: u64,
    pub d0_fast_path: u64,
    pub d0_fallback: u64,
}

impl BlockQueue {
    pub fn new(bound: f64, m: usize) -> Self {
        BlockQueue {
            d0: VecDeque::new(),
            blocks: BTreeMap::new(),
            bound,
            m: m.max(1),
            seq: 0,
            d0_fast_path: 0,
            d0_fallback: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.d0.is_empty() && self.blocks.is_empty()
    }

    fn item_count(&self) -> usize {
        self.d0.iter().map(|b| b.items.len()).sum::<usize>()
            + self.blocks.values().map(|b| b.items.len()).sum::<usize>()
    }

    fn d0_min(&self) -> Option<f64> {
        self.d0
            .iter()
            .flat_map(|b| b.items.iter().map(|it| it.1))
            .min_by(|a, b| a.total_cmp(b))
    }

    fn physical_min(&self) -> Option<f64> {
        let d0 = self.d0_min();
        let d1 = self
            .blocks
            .values()
            .flat_map(|b| b.items.iter().map(|it| it.1))
            .min_by(|a, b| a.total_cmp(b));
        match (d0, d1) {
            (Some(a), Some(b)) => Some(if a < b { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    pub fn insert(&mut self, v: u32, key: f64) {
        debug_assert!(key.is_finite());
        // Keep D0 <= D1: keys below the D0 minimum belong on D0.
        if let Some(d0_min) = self.d0_min() {
            if key < d0_min {
                self.prepend_d0(vec![(v, key)]);
                return;
            }
        }
        self.insert_d1(v, key);
    }

    fn insert_d1(&mut self, v: u32, key: f64) {
        let m = self.m;

        if self.blocks.is_empty() {
            self.seq += 1;
            self.blocks.insert(
                UbKey {
                    ub: K(key),
                    seq: self.seq,
                },
                Block {
                    items: vec![(v, key)],
                    ub: key,
                },
            );
            return;
        }

        let cand = self
            .blocks
            .range(UbKey { ub: K(key), seq: 0 }..)
            .next()
            .map(|(k, _)| (k.ub, k.seq));
        let Some((ub, seq)) = cand else {
            let (lub, lseq) = {
                let (k, _) = self.blocks.iter().next_back().unwrap();
                (k.ub, k.seq)
            };
            if self.blocks[&UbKey { ub: lub, seq: lseq }].items.len() < m {
                let mut blk = self.blocks.remove(&UbKey { ub: lub, seq: lseq }).unwrap();
                blk.items.push((v, key));
                blk.ub = key;
                self.blocks.insert(
                    UbKey {
                        ub: K(key),
                        seq: lseq,
                    },
                    blk,
                );
            } else {
                self.seq += 1;
                self.blocks.insert(
                    UbKey {
                        ub: K(key),
                        seq: self.seq,
                    },
                    Block {
                        items: vec![(v, key)],
                        ub: key,
                    },
                );
            }
            return;
        };

        let target = UbKey { ub, seq };
        let min = self.blocks[&target]
            .items
            .iter()
            .map(|it| it.1)
            .fold(f64::INFINITY, f64::min);

        if key < min {
            let prev = self.blocks.range(..target).next_back().map(|(k, _)| *k);
            if let Some(pk) = prev {
                if self.blocks[&pk].items.len() < m {
                    let mut blk = self.blocks.remove(&pk).unwrap();
                    blk.items.push((v, key));
                    blk.ub = key;
                    self.blocks.insert(
                        UbKey {
                            ub: K(key),
                            seq: pk.seq,
                        },
                        blk,
                    );
                    return;
                }
            }
            self.seq += 1;
            self.blocks.insert(
                UbKey {
                    ub: K(key),
                    seq: self.seq,
                },
                Block {
                    items: vec![(v, key)],
                    ub: key,
                },
            );
            return;
        }

        let blk = self.blocks.get_mut(&target).unwrap();
        blk.items.push((v, key));
        if blk.items.len() > m {
            let blk = self.blocks.remove(&target).unwrap();
            let (b1, b2) = split_block(blk);
            self.seq += 1;
            self.blocks.insert(
                UbKey {
                    ub: K(b1.ub),
                    seq: self.seq,
                },
                b1,
            );
            self.seq += 1;
            self.blocks.insert(
                UbKey {
                    ub: K(b2.ub),
                    seq: self.seq,
                },
                b2,
            );
        }
    }

    pub fn batch_prepend(&mut self, items: &[(u32, f64)]) {
        if items.is_empty() {
            return;
        }
        for &(_, k) in items {
            debug_assert!(k.is_finite());
        }

        let max_batch = items
            .iter()
            .map(|it| it.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_existing = self.physical_min();

        if min_existing.map(|mn| max_batch < mn).unwrap_or(true) {
            self.d0_fast_path += 1;
            self.prepend_d0(items.to_vec());
        } else {
            self.d0_fallback += 1;
            self.flush_d0_into_d1();
            for &(v, k) in items {
                self.insert_d1(v, k);
            }
        }
    }

    fn flush_d0_into_d1(&mut self) {
        while let Some(blk) = self.d0.pop_front() {
            for (v, k) in blk.items {
                self.insert_d1(v, k);
            }
        }
    }

    fn prepend_d0(&mut self, mut items: Vec<(u32, f64)>) {
        items.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let chunk = if items.len() <= self.m {
            self.m
        } else {
            self.m.div_ceil(2)
        }
        .max(1);
        let mut new_blocks: Vec<Block> = Vec::new();
        for piece in items.chunks(chunk) {
            let ub = piece
                .iter()
                .map(|it| it.1)
                .fold(f64::NEG_INFINITY, f64::max);
            new_blocks.push(Block {
                items: piece.to_vec(),
                ub,
            });
        }
        for blk in new_blocks.into_iter().rev() {
            self.d0.push_front(blk);
        }
    }

    pub fn pull(&mut self) -> (Vec<u32>, f64) {
        if self.is_empty() {
            return (Vec::new(), self.bound);
        }

        // Materialize every physical entry. Prefix-of-blocks Pull is a follow-up;
        // D0 still accelerates BatchPrepend between pulls. Multi-entry (no
        // decrease-key) semantics are load-bearing for BMSSP retries.
        let mut collected: Vec<(u32, f64)> = Vec::with_capacity(self.item_count());
        for blk in self.d0.drain(..) {
            collected.extend(blk.items);
        }
        for (_, blk) in std::mem::take(&mut self.blocks) {
            collected.extend(blk.items);
        }

        collected.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        let m = self.m;
        if collected.len() <= m {
            let vs = collected.iter().map(|&(v, _)| v).collect();
            return (vs, self.bound);
        }

        let vm = collected[m - 1].1;
        let take = collected.partition_point(|it| it.1 <= vm);
        let s: Vec<u32> = collected[..take].iter().map(|&(v, _)| v).collect();
        self.rebuild_blocks(&collected[take..]);
        let x = self.physical_min().unwrap_or(self.bound);
        (s, x)
    }

    fn rebuild_blocks(&mut self, items: &[(u32, f64)]) {
        for chunk in items.chunks(self.m) {
            let ub = chunk
                .iter()
                .map(|it| it.1)
                .fold(f64::NEG_INFINITY, f64::max);
            self.seq += 1;
            self.blocks.insert(
                UbKey {
                    ub: K(ub),
                    seq: self.seq,
                },
                Block {
                    items: chunk.to_vec(),
                    ub,
                },
            );
        }
    }

    pub fn drain(&mut self) -> Vec<(u32, f64)> {
        let mut out = Vec::new();
        for blk in self.d0.drain(..) {
            out.extend(blk.items);
        }
        for (_, blk) in std::mem::take(&mut self.blocks) {
            out.extend(blk.items);
        }
        out
    }
}

pub enum QueueOps {
    Map(PartialQueue),
    Block(BlockQueue),
}

impl QueueOps {
    pub fn is_empty(&self) -> bool {
        match self {
            QueueOps::Map(q) => q.is_empty(),
            QueueOps::Block(q) => q.is_empty(),
        }
    }

    pub fn insert(&mut self, v: u32, key: f64) {
        match self {
            QueueOps::Map(q) => q.insert(v, key),
            QueueOps::Block(q) => q.insert(v, key),
        }
    }

    pub fn pull(&mut self) -> (Vec<u32>, f64) {
        match self {
            QueueOps::Map(q) => q.pull(),
            QueueOps::Block(q) => q.pull(),
        }
    }

    pub fn batch_prepend(&mut self, items: &[(u32, f64)]) {
        match self {
            QueueOps::Map(q) => q.batch_prepend(items),
            QueueOps::Block(q) => q.batch_prepend(items),
        }
    }

    /// Remove and return everything still queued.
    pub fn drain(&mut self) -> Vec<(u32, f64)> {
        match self {
            QueueOps::Map(q) => q.drain(),
            QueueOps::Block(q) => q.drain(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_returns_smallest_bucket_and_bound() {
        let mut q = PartialQueue::new(100.0);
        q.insert(1, 5.0);
        q.insert(2, 5.0);
        q.insert(3, 9.0);
        q.insert(4, 1.0);
        let (b0, bi) = q.pull();
        assert_eq!(b0, vec![4]);
        assert_eq!(bi, 5.0);
        let (b1, bi) = q.pull();
        assert_eq!(b1, vec![1, 2]);
        assert_eq!(bi, 9.0);
        let (b2, bi) = q.pull();
        assert_eq!(b2, vec![3]);
        assert_eq!(bi, 100.0);
        assert!(q.is_empty());
    }

    #[test]
    fn batch_prepend_sorts_ahead() {
        let mut q = PartialQueue::new(100.0);
        q.insert(1, 9.0);
        q.batch_prepend(&[(2, 3.0), (3, 3.0)]);
        let (b, _) = q.pull();
        assert_eq!(b, vec![2, 3]);
    }

    #[test]
    fn empty_pull_returns_bound() {
        let mut q = PartialQueue::new(42.0);
        let (b, bi) = q.pull();
        assert!(b.is_empty());
        assert_eq!(bi, 42.0);
    }

    #[test]
    fn block_queue_basic() {
        let mut q = BlockQueue::new(100.0, 2);
        q.insert(1, 5.0);
        q.insert(2, 5.0);
        q.insert(3, 9.0);
        q.insert(4, 1.0);
        let (b, bi) = q.pull();
        let mut b = b;
        b.sort_unstable();
        assert_eq!(b, vec![1, 2, 4]);
        assert_eq!(bi, 9.0);
        let (b, bi) = q.pull();
        assert_eq!(b, vec![3]);
        assert_eq!(bi, 100.0);
        assert!(q.is_empty());
    }

    #[test]
    fn block_queue_d0_fast_path_prepends_below_min() {
        let mut q = BlockQueue::new(100.0, 2);
        q.insert(1, 10.0);
        q.insert(2, 12.0);
        q.batch_prepend(&[(3, 1.0), (4, 2.0), (5, 3.0)]);
        assert_eq!(q.d0_fast_path, 1);
        assert_eq!(q.d0_fallback, 0);
        assert!(!q.d0.is_empty());
        let (mut b, bi) = q.pull();
        b.sort_unstable();
        assert_eq!(b, vec![3, 4]);
        assert_eq!(bi, 3.0);
    }

    #[test]
    fn block_queue_keeps_prior_key_when_improved() {
        // Codex P1: improving a key must not erase the prior queue entry.
        let mut q = BlockQueue::new(100.0, 4);
        q.insert(2, 0.07);
        q.insert(2, 0.06);
        assert_eq!(q.item_count(), 2);
        let (b, _) = q.pull();
        // m=4 > 2, both entries returned (lazy multi-entry semantics).
        assert_eq!(b, vec![2, 2]);
        assert!(q.is_empty());
    }

    #[test]
    fn block_queue_batch_prepend_falls_back_when_contract_broken() {
        let mut q = BlockQueue::new(100.0, 2);
        q.insert(1, 5.0);
        q.batch_prepend(&[(2, 7.0)]);
        assert_eq!(q.d0_fallback, 1);
        assert!(q.d0.is_empty());
        let (mut b, _) = q.pull();
        b.sort_unstable();
        assert_eq!(b, vec![1, 2]);
    }

    /// Model-based differential test: BlockQueue vs a sorted-vector multiset
    /// (duplicate vertex keys allowed — no decrease-key).
    #[test]
    fn block_queue_matches_model() {
        use rand::Rng;
        use rand::SeedableRng;
        use rand_chacha::ChaCha8Rng;
        let mut rng = ChaCha8Rng::seed_from_u64(0xB10C);
        for &m in &[1usize, 2, 3, 8] {
            for bound in [50.0f64, 1000.0] {
                let mut q = BlockQueue::new(bound, m);
                let mut model: Vec<(u32, f64)> = Vec::new();
                let mut log: Vec<String> = Vec::new();
                for _ in 0..4000 {
                    match rng.gen_range(0..4u32) {
                        0 => {
                            let v = rng.gen_range(0..12u32);
                            let k = (rng.gen_range(0..40) as f64) * 0.25;
                            q.insert(v, k);
                            model.push((v, k));
                            log.push(format!("I {v} {k}"));
                        }
                        1 => {
                            let n = rng.gen_range(0..=5usize);
                            let items: Vec<(u32, f64)> = (0..n)
                                .map(|_| {
                                    let v = rng.gen_range(0..12u32);
                                    let k = (rng.gen_range(0..40) as f64) * 0.25;
                                    (v, k)
                                })
                                .collect();
                            q.batch_prepend(&items);
                            model.extend_from_slice(&items);
                            log.push(format!("B {items:?}"));
                        }
                        2 => {
                            let m_pre = model.clone();
                            let (sb, xb) = q.pull();
                            let (sm, xm) = model_pull(&mut model, m, bound);
                            let mut sb = sb;
                            sb.sort_unstable();
                            let mut sm = sm;
                            sm.sort_unstable();
                            assert_eq!(
                                sb,
                                sm,
                                "bucket mismatch m={m}\nmodel_pre={m_pre:?}\nops={}",
                                log.join(" ")
                            );
                            assert_eq!(xb, xm, "separation mismatch m={m}\nops={}", log.join(" "));
                            assert_eq!(q.is_empty(), model.is_empty());
                            log.push(format!("P {:?}", sm));
                        }
                        _ => {
                            assert_eq!(q.is_empty(), model.is_empty());
                        }
                    }
                    assert_eq!(
                        q.item_count(),
                        model.len(),
                        "item count mismatch m={m}\nops={}",
                        log.join(" ")
                    );
                }
            }
        }
    }

    fn model_pull(items: &mut Vec<(u32, f64)>, m: usize, bound: f64) -> (Vec<u32>, f64) {
        items.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        if items.len() <= m {
            let vs = items.iter().map(|&(v, _)| v).collect();
            items.clear();
            return (vs, bound);
        }
        let vm = items[m - 1].1;
        let take = items.partition_point(|it| it.1 <= vm);
        let s: Vec<u32> = items[..take].iter().map(|&(v, _)| v).collect();
        items.drain(..take);
        let x = if items.is_empty() { bound } else { items[0].1 };
        (s, x)
    }
}
