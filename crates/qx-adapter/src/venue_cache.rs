//! venue 侧本地订单缓存的驻留上限。
//!
//! 每个柜台适配器都自己维护一份 `client_id -> Order` 缓存，用来把柜台回报认回本地
//! 订单、把重复回报按幂等吞掉。这份缓存在进程启动时由 `restore_orders(pipeline
//! .orders())` 灌成账户的**全部历史订单**，运行期只继续变长：一个跑几十天的用户流
//! worker 会一路持有该账户自开户以来的每一笔订单和每一笔成交身份，而这批数据在
//! EventLog 里本来就有权威副本。
//!
//! 退场规则刻意保守：只退终态订单、按 `client_id` 升序退（本地发号顺序，不是挂钟
//! 年龄），未终态订单永不退场——它们还要接后续的成交与撤单回报。被退场的订单若真
//! 有回报进来，会落到各适配器既有的「未知本地订单」分支并进入 `ReconcileRequired`：
//! 分歧必须显式升级，不能被缓存淘汰策略静默吞掉。

use qx_core::Order;
use std::collections::{BTreeMap, BTreeSet};

/// 单个 venue 进程常驻的本地订单条数上限。
pub(crate) const MAX_CACHED_ORDERS: usize = 8_192;

/// 迟滞窗口：越限后一次退到 `上限 - 该值`，把整表扫描摊到每 1024 条新订单上，
/// 而不是启动期灌进来的每一条历史订单都扫一遍全表。
const ORDER_EVICT_HYSTERESIS: usize = 1_024;

/// 就地把订单缓存压回上限，返回被退场的 `client_id`，由调用方级联自己的派生索引。
pub(crate) fn evict_stale_terminal_orders(orders: &mut BTreeMap<u64, Order>) -> BTreeSet<u64> {
    if orders.len() <= MAX_CACHED_ORDERS {
        return BTreeSet::new();
    }
    let overflow = orders.len() - (MAX_CACHED_ORDERS - ORDER_EVICT_HYSTERESIS);
    let evicted: BTreeSet<u64> = orders
        .iter()
        .filter(|(_, order)| order.status.is_terminal())
        .map(|(client_id, _)| *client_id)
        .take(overflow)
        .collect();
    orders.retain(|client_id, _| !evicted.contains(client_id));
    evicted
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, OrderStatus, Price, Quantity, Side};

    fn order(client_id: u64, status: OrderStatus) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(1)),
            status,
            filled: if status == OrderStatus::Filled {
                Quantity::from_i64(1)
            } else {
                Quantity::ZERO
            },
            account_id: "acct".into(),
            trace: None,
            policy: None,
        }
    }

    fn fill_book(count: u64, status: OrderStatus) -> BTreeMap<u64, Order> {
        (1..=count)
            .map(|client_id| (client_id, order(client_id, status)))
            .collect()
    }

    #[test]
    fn below_the_cap_nothing_is_touched() {
        let mut orders = fill_book(MAX_CACHED_ORDERS as u64, OrderStatus::Filled);
        assert!(evict_stale_terminal_orders(&mut orders).is_empty());
        assert_eq!(orders.len(), MAX_CACHED_ORDERS);
    }

    #[test]
    fn eviction_drops_the_oldest_terminal_orders_down_to_the_hysteresis_line() {
        let mut orders = fill_book(MAX_CACHED_ORDERS as u64 + 10, OrderStatus::Cancelled);
        let evicted = evict_stale_terminal_orders(&mut orders);
        let kept = MAX_CACHED_ORDERS - ORDER_EVICT_HYSTERESIS;
        assert_eq!(orders.len(), kept);
        assert_eq!(evicted.len(), 1_024 + 10);
        // 退的是发号最早的一批，最新那笔终态订单仍然认得回来。
        assert!(evicted.contains(&1));
        assert!(orders.contains_key(&(MAX_CACHED_ORDERS as u64 + 10)));
    }

    /// 正向对照：缓存里全是活跃订单时，宁可继续带着它们也不能退——退掉活跃订单等于
    /// 把一笔还会收到成交的订单变成「未知订单」，那是丢事实而不是省内存。
    #[test]
    fn live_orders_are_never_evicted_even_over_the_cap() {
        let mut orders = fill_book(MAX_CACHED_ORDERS as u64 + 10, OrderStatus::Working);
        assert!(evict_stale_terminal_orders(&mut orders).is_empty());
        assert_eq!(orders.len(), MAX_CACHED_ORDERS + 10);
    }

    #[test]
    fn mixed_book_evicts_only_terminal_orders() {
        let mut orders: BTreeMap<u64, Order> = (1..=MAX_CACHED_ORDERS as u64 + 5)
            .map(|client_id| {
                let status = if client_id % 2 == 0 {
                    OrderStatus::Filled
                } else {
                    OrderStatus::Working
                };
                (client_id, order(client_id, status))
            })
            .collect();
        let evicted = evict_stale_terminal_orders(&mut orders);
        assert_eq!(orders.len(), MAX_CACHED_ORDERS - ORDER_EVICT_HYSTERESIS);
        assert_eq!(evicted.len(), 1_029);
        assert!(evicted.iter().all(|client_id| client_id % 2 == 0));
        // 奇数号一笔都没丢，偶数号里也只有最早的那批被退。
        for client_id in 1..=2_059u64 {
            assert_eq!(
                orders.contains_key(&client_id),
                client_id % 2 == 1 || client_id > 2_058,
                "订单 {client_id} 的退留不符合『终态、按发号顺序、退到迟滞线』"
            );
        }
    }
}
