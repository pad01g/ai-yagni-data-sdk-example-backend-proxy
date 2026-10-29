//! backend-proxy のうち、Verus で証明する部分。注文の金額と在庫は proxy が決める（backend-rust の言う値を信じない）。
//!
//! - `valid_quantity`: 1 行の数量は 1 以上 99 以下。
//! - `reserve`: 在庫 stock から want を引き当てる。足りるときだけ残りを返す。
//! - `order_total`: 明細の単価 × 数量の和。あふれるなら None。仕様 `line_sum` とちょうど一致する。
// Verus の検証器が仕様を知っている形（比較の連なり、len、&Vec）で書くので、clippy の言い換えの提案は当てない。
#![allow(clippy::manual_range_contains, clippy::len_zero, clippy::ptr_arg)]

use vstd::prelude::*;

verus! {

pub const MAX_QUANTITY: u32 = 99;

pub fn valid_quantity(q: u32) -> (r: bool)
    ensures r == (1 <= q && q <= MAX_QUANTITY),
{
    q >= 1 && q <= MAX_QUANTITY
}

pub fn reserve(stock: u32, want: u32) -> (r: Option<u32>)
    ensures
        r matches Some(left) ==> left + want == stock,
        r is None <==> want > stock,
{
    if want > stock { None } else { Some(stock - want) }
}

/// 明細の単価 × 数量の和（先頭から）。
pub open spec fn line_sum(prices: Seq<u32>, quantities: Seq<u32>) -> int
    decreases prices.len(),
{
    if prices.len() == 0 || quantities.len() == 0 {
        0
    } else {
        line_sum(prices.drop_last(), quantities.drop_last()) + prices.last() as int * quantities.last() as int
    }
}

pub fn order_total(prices: &Vec<u32>, quantities: &Vec<u32>) -> (r: Option<u64>)
    requires prices.len() == quantities.len(),
    ensures
        r matches Some(t) ==> t == line_sum(prices@, quantities@),
        r is None ==> line_sum(prices@, quantities@) > u64::MAX,
{
    let mut acc: u64 = 0;
    let mut i: usize = 0;
    while i < prices.len()
        invariant
            prices.len() == quantities.len(),
            0 <= i <= prices.len(),
            acc == line_sum(prices@.take(i as int), quantities@.take(i as int)),
        decreases prices.len() - i,
    {
        let p = prices[i];
        let q = quantities[i];
        proof {
            assert(prices@.take(i as int + 1).drop_last() =~= prices@.take(i as int));
            assert(quantities@.take(i as int + 1).drop_last() =~= quantities@.take(i as int));
            assert(p as int * q as int <= 0xffff_ffff * 0xffff_ffff) by (nonlinear_arith)
                requires p <= 0xffff_ffff, q <= 0xffff_ffff, p >= 0, q >= 0;
        }
        let line = (p as u64) * (q as u64);
        match acc.checked_add(line) {
            Some(a) => acc = a,
            None => {
                proof {
                    lemma_sum_grows(prices@, quantities@, i as int + 1);
                }
                return None;
            }
        }
        i += 1;
    }
    proof {
        assert(prices@.take(prices.len() as int) =~= prices@);
        assert(quantities@.take(quantities.len() as int) =~= quantities@);
    }
    Some(acc)
}

/// 和は先頭から伸ばすほど減らない（単価と数量は 0 以上）。
proof fn lemma_sum_grows(prices: Seq<u32>, quantities: Seq<u32>, k: int)
    requires 0 <= k <= prices.len(), prices.len() == quantities.len(),
    ensures line_sum(prices.take(k), quantities.take(k)) <= line_sum(prices, quantities),
    decreases prices.len() - k,
{
    if k < prices.len() {
        lemma_sum_grows(prices, quantities, k + 1);
        assert(prices.take(k + 1).drop_last() =~= prices.take(k));
        assert(quantities.take(k + 1).drop_last() =~= quantities.take(k));
        assert(prices.take(k + 1).last() as int * quantities.take(k + 1).last() as int >= 0) by (nonlinear_arith)
            requires prices.take(k + 1).last() >= 0, quantities.take(k + 1).last() >= 0;
    } else {
        assert(prices.take(k) =~= prices);
        assert(quantities.take(k) =~= quantities);
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_and_stock() {
        assert!(valid_quantity(1) && valid_quantity(99) && !valid_quantity(0) && !valid_quantity(100));
        assert_eq!(reserve(3, 2), Some(1));
        assert_eq!(reserve(3, 4), None);
    }

    #[test]
    fn totals() {
        assert_eq!(order_total(&vec![2500, 1200], &vec![2, 1]), Some(6200));
        assert_eq!(order_total(&vec![], &vec![]), Some(0));
        assert_eq!(order_total(&vec![u32::MAX; 3], &vec![u32::MAX; 3]), None);
    }
}
