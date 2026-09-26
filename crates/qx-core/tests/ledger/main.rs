//! 账簿内核集成用例：只用公开 API 断言事实归约与重放一致性。

use qx_core::*;
use std::collections::BTreeMap;

fn order(side: Side) -> Order {
    Order {
        client_id: 7,
        instrument: InstrumentId::parse("BTC-USDT.BINANCE").unwrap(),
        side,
        qty: Quantity::from_i64(2),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

mod core_facts;
mod corporate_actions;
mod rights_entitlements;
