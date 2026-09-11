use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};

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
///   is strictly below the current live minimum (paper contract).
/// - `D1` (`blocks`): BST-keyed blocks for ordinary Insert.
/// - `best`: lazy decrease-key — keep the smallest key per vertex; skip worse
///   Insert/BatchPrepend; Pull ignores stale physical copies.
///
/// Pull always drains D0 completely before reading D1, then parks leftovers in
/// D1 only. That keeps "D0 before D1" aligned with value order even after
/// BatchPrepend + Insert under real-weight EPS noise. If BatchPrepend breaks
/// the contract, we flush D0 into D1 and Insert (safe fallback).
#[derive(Debug)]
pub struct BlockQueue {
    d0: VecDeque<Block>,
    blocks: BTreeMap<UbKey, Block>,
    bound: f64,
    m: usize,
    seq: u64,
    best: HashMap<u32, f64>,
    pub d0_fast_path: u64,
    pub d0_fallback: u64,
    pub decrease_key_skipped: u64,
}

impl BlockQueue {
    pub fn new(bound: f64, m: usize) -> Self {
        BlockQueue {
            d0: VecDeque::new(),
            blocks: BTreeMap::new(),
            bound,
            m: m.max(1),
            seq: 0,
            best: HashMap::new(),
            d0_fast_path: 0,
            d0_fallback: 0,
            decrease_key_skipped: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.best.is_empty()
    }

    pub fn live_len(&self) -> usize {
        self.best.len()
    }

    fn note_key(&mut self, v: u32, key: f64) -> bool {
        if let Some(&prev) = self.best.get(&v) {
            if prev <= key {
                self.decrease_key_skipped += 1;
                return false;
            }
        }
        self.best.insert(v, key);
        true
    }

    fn is_live(&self, v: u32, key: f64) -> bool {
        self.best
            .get(&v)
            .is_some_and(|&b| b.to_bits() == key.to_bits())
    }

    fn d0_min_live(&self) -> Option<f64> {
        self.d0
            .iter()
            .flat_map(|b| b.items.iter())
            .filter(|(v, k)| self.is_live(*v, *k))
            .map(|(_, k)| *k)
            .min_by(|a, b| a.total_cmp(b))
    }

    pub fn insert(&mut self, v: u32, key: f64) {
        debug_assert!(key.is_finite());
        if !self.note_key(v, key) {
            return;
        }
        if let Some(d0_min) = self.d0_min_live() {
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
        let min_existing = self
            .best
            .values()
            .copied()
            .min_by(|a, b| a.total_cmp(b));

        let mut filtered: Vec<(u32, f64)> = Vec::with_capacity(items.len());
        for &(v, k) in items {
            debug_assert!(k.is_finite());
            if !self.note_key(v, k) {
                continue;
            }
            filtered.push((v, k));
        }
        if filtered.is_empty() {
            return;
        }

        let max_batch = filtered
            .iter()
            .map(|it| it.1)
            .fold(f64::NEG_INFINITY, f64::max);

        if min_existing.map(|mn| max_batch < mn).unwrap_or(true) {
            self.d0_fast_path += 1;
            self.prepend_d0(filtered);
        } else {
            self.d0_fallback += 1;
            self.flush_d0_into_d1();
            for (v, k) in filtered {
                self.insert_d1(v, k);
            }
        }
    }

    fn flush_d0_into_d1(&mut self) {
        while let Some(blk) = self.d0.pop_front() {
            for (v, k) in blk.items {
                if self.is_live(v, k) {
                    self.insert_d1(v, k);
                }
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
        if self.best.is_empty() {
            self.d0.clear();
            self.blocks.clear();
            return (Vec::new(), self.bound);
        }

        // Authoritative selection from the live set. Physical D0/D1 storage is
        // compacted around leftovers afterward. BatchPrepend still benefits
        // from the D0 fast path between pulls; restoring paper-style O(|S'|)
        // prefix Pull is a follow-up.
        let mut live: Vec<(u32, f64)> = self.best.iter().map(|(&v, &k)| (v, k)).collect();
        live.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        self.d0.clear();
        self.blocks.clear();
        self.select_and_rebuild(live, self.m)
    }

    fn select_and_rebuild(&mut self, deduped: Vec<(u32, f64)>, m: usize) -> (Vec<u32>, f64) {
        if deduped.is_empty() {
            return (Vec::new(), self.bound);
        }
        if deduped.len() <= m {
            for &(v, _) in &deduped {
                self.best.remove(&v);
            }
            let vs = deduped.iter().map(|&(v, _)| v).collect();
            return (vs, self.min_value_live().unwrap_or(self.bound));
        }
        let vm = deduped[m - 1].1;
        let take = deduped.partition_point(|it| it.1 <= vm);
        let s: Vec<u32> = deduped[..take].iter().map(|&(v, _)| v).collect();
        for &v in &s {
            self.best.remove(&v);
        }
        self.rebuild_blocks(&deduped[take..]);
        (s, self.min_value_live().unwrap_or(self.bound))
    }

    fn min_value_live(&self) -> Option<f64> {
        self.best.values().copied().min_by(|a, b| a.total_cmp(b))
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
        let mut out: Vec<(u32, f64)> = self.best.iter().map(|(&v, &k)| (v, k)).collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        self.d0.clear();
        self.blocks.clear();
        self.best.clear();
        out
    }
}

/// Uniform interface used by the BMSSP engine for both queue backends.
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
    fn block_queue_decrease_key_keeps_best() {
        let mut q = BlockQueue::new(100.0, 4);
        q.insert(1, 9.0);
        q.insert(1, 9.0);
        assert_eq!(q.decrease_key_skipped, 1);
        q.insert(1, 3.0);
        q.insert(1, 5.0);
        assert_eq!(q.live_len(), 1);
        let (b, bi) = q.pull();
        assert_eq!(b, vec![1]);
        assert_eq!(bi, 100.0);
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

    #[test]
    fn block_queue_matches_model() {
        use rand::Rng;
        use rand::SeedableRng;
        use rand_chacha::ChaCha8Rng;
        let mut rng = ChaCha8Rng::seed_from_u64(0xB10C);
        for &m in &[1usize, 2, 3, 8] {
            for bound in [50.0f64, 1000.0] {
                let mut q = BlockQueue::new(bound, m);
                let mut model: HashMap<u32, f64> = HashMap::new();
                let mut log: Vec<String> = Vec::new();
                for _ in 0..4000 {
                    match rng.gen_range(0..4u32) {
                        0 => {
                            let v = rng.gen_range(0..12u32);
                            let k = (rng.gen_range(0..40) as f64) * 0.25;
                            q.insert(v, k);
                            model_insert(&mut model, v, k);
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
                            for &(v, k) in &items {
                                model_insert(&mut model, v, k);
                            }
                            log.push(format!("B {items:?}"));
                        }
                        2 => {
                            let m_pre: Vec<(u32, f64)> =
                                model.iter().map(|(&v, &k)| (v, k)).collect();
                            let (sb, xb) = q.pull();
                            let (sm, xm) = model_pull(&mut model, m, bound);
                            let mut sb = sb;
                            sb.sort_unstable();
                            let mut sm = sm;
                            sm.sort_unstable();
                            assert_eq!(
                                sb, sm,
                                "bucket mismatch m={m}\nmodel_pre={m_pre:?}\nops={}",
                                log.join(" ")
                            );
                            assert_eq!(
                                xb, xm,
                                "separation mismatch m={m}\nops={}",
                                log.join(" ")
                            );
                            assert_eq!(q.is_empty(), model.is_empty());
                            log.push(format!("P {:?}", sm));
                        }
                        _ => {
                            assert_eq!(q.is_empty(), model.is_empty());
                        }
                    }
                    assert_eq!(
                        q.live_len(),
                        model.len(),
                        "live count mismatch m={m}\nops={}",
                        log.join(" ")
                    );
                }
            }
        }
    }

    fn model_insert(model: &mut HashMap<u32, f64>, v: u32, k: f64) {
        match model.get(&v) {
            Some(&prev) if prev <= k => {}
            _ => {
                model.insert(v, k);
            }
        }
    }

    fn model_pull(model: &mut HashMap<u32, f64>, m: usize, bound: f64) -> (Vec<u32>, f64) {
        let mut items: Vec<(u32, f64)> = model.iter().map(|(&v, &k)| (v, k)).collect();
        items.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        if items.len() <= m {
            let vs = items.iter().map(|&(v, _)| v).collect();
            model.clear();
            return (vs, bound);
        }
        let vm = items[m - 1].1;
        let take = items.partition_point(|it| it.1 <= vm);
        let s: Vec<u32> = items[..take].iter().map(|&(v, _)| v).collect();
        for &v in &s {
            model.remove(&v);
        }
        let x = items[take..]
            .iter()
            .map(|it| it.1)
            .min_by(|a, b| a.total_cmp(b))
            .unwrap_or(bound);
        (s, x)
    }
}
