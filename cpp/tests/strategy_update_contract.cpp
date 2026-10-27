#include "qianxing_strategy.h"

#include <cassert>
#include <cstddef>
#include <cstdint>
#include <type_traits>
#include <utility>

static_assert(std::is_same<
                  decltype(std::declval<qx_strategy_order_status_update_v2>().event_seq),
                  uint64_t>::value,
              "event_seq must keep its fixed-width wire type");
static_assert(std::is_same<decltype(std::declval<qx_strategy_order_update_v2>().side),
                           qx_order_side>::value,
              "side must remain a checked fixed-width code");
static_assert(QX_STRATEGY_FILL_UPDATE_SCHEMA_VERSION == 2u,
              "sequence-bound fill updates must keep their published schema version");
static_assert(QX_STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION == 1u,
              "account/position snapshots must keep their published schema version");
static_assert(QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION == 1u,
              "warmup lifecycle events must keep their published schema version");
static_assert(QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2 == 2u,
              "exact StrategyContext must keep its published schema version");
static_assert(offsetof(qx_strategy_order_status_update_v2, order)
                  > offsetof(qx_strategy_order_status_update_v2, status),
              "order facts must follow the status header");
static_assert(offsetof(qx_market_event_v2, exact_ohlcv)
                  > offsetof(qx_market_event_v2, raw_event),
              "exact market data must extend the v1 event without changing its layout");
static_assert(offsetof(qx_market_event_v3, exact_bids)
                  > offsetof(qx_market_event_v3, event_v2),
              "exact book sidecars must extend the v2 event without changing old layouts");
static_assert(offsetof(qx_strategy_warmup_complete_v1, instrument)
                  > offsetof(qx_strategy_warmup_complete_v1, schema_version),
              "warmup event identity must follow its version header");
static_assert(offsetof(qx_strategy_context_v2, context_v1)
                  > offsetof(qx_strategy_context_v2, schema_version),
              "exact StrategyContext must embed the stable v1 projection");
static_assert(offsetof(qx_strategy_context_v2, positions_exact)
                  > offsetof(qx_strategy_context_v2, context_v1),
              "exact account values must extend rather than change the v1 context layout");

int main() {
    qx_strategy_warmup_complete_v1 warmup{};
    warmup.struct_size = sizeof(warmup);
    warmup.schema_version = QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION;
    warmup.instrument = "BTCUSDT.BINANCE";
    warmup.instrument_len = 15;
    warmup.has_instrument = 1;
    warmup.observed_bars = 2;
    assert(warmup.struct_size == sizeof(qx_strategy_warmup_complete_v1));
    assert(warmup.schema_version == 1u);
    assert(warmup.instrument_len == 15);
    assert(warmup.observed_bars == 2);

    qx_strategy_context_v2 exact_context{};
    exact_context.struct_size = sizeof(exact_context);
    exact_context.schema_version = QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2;
    exact_context.positions_exact_len = 1;
    exact_context.cash_exact_len = 1;
    exact_context.has_available_margin_exact = 1;
    qx_strategy_exact_kv_v1 exact_position{};
    exact_position.key = "ALTUSDT.BINANCE";
    exact_position.key_len = 15;
    exact_position.value.coefficient = qx_raw128{1u, 0};
    exact_position.value.scale = 18;
    exact_context.positions_exact = &exact_position;
    qx_strategy_exact_kv_v1 exact_cash{};
    exact_cash.key = "USDT";
    exact_cash.key_len = 4;
    exact_context.cash_exact = &exact_cash;
    exact_context.available_margin_exact.coefficient = qx_raw128{9u, 0};
    exact_context.available_margin_exact.scale = 18;
    assert(exact_context.struct_size == sizeof(qx_strategy_context_v2));
    assert(exact_context.schema_version == 2u);
    assert(exact_context.positions_exact[0].value.coefficient.lo == 1u);
    assert(exact_context.available_margin_exact.scale == 18u);

    qx_market_event timer_event{};
    timer_event.schema_version = QX_STRATEGY_API_VERSION;
    timer_event.kind = QX_MARKET_TIMER;
    timer_event.ts = 1000;
    timer_event.timer_name = "scheduler";
    assert(timer_event.kind == QX_MARKET_TIMER);
    assert(timer_event.ts == 1000);
    assert(timer_event.timer_name[0] == 's');

    qx_market_event_v2 market_event{};
    market_event.struct_size = sizeof(market_event);
    market_event.schema_version = QX_MARKET_EVENT_SCHEMA_VERSION_V2;
    market_event.raw_event.schema_version = QX_STRATEGY_API_VERSION;
    market_event.raw_event.kind = QX_MARKET_BAR;
    market_event.has_exact_ohlcv = 1;
    market_event.exact_ohlcv.close.coefficient = qx_raw128{5u, 0};
    market_event.exact_ohlcv.close.scale = 10;
    market_event.exact_ohlcv.volume.coefficient = qx_raw128{5u, 0};
    market_event.exact_ohlcv.volume.scale = 13;

    assert(market_event.struct_size == sizeof(qx_market_event_v2));
    assert(market_event.schema_version == QX_MARKET_EVENT_SCHEMA_VERSION_V2);
    assert(market_event.has_exact_ohlcv == 1);
    assert(market_event.exact_ohlcv.close.coefficient.lo == 5u);
    assert(market_event.exact_ohlcv.close.scale == 10);

    qx_strategy_exact_book_level_v1 exact_level{};
    exact_level.has_exact = 1;
    exact_level.price.coefficient = qx_raw128{12345u, 0};
    exact_level.price.scale = 12;
    exact_level.qty.coefficient = qx_raw128{5u, 0};
    exact_level.qty.scale = 18;
    qx_market_event_v3 precise_depth{};
    precise_depth.struct_size = sizeof(precise_depth);
    precise_depth.schema_version = QX_MARKET_EVENT_SCHEMA_VERSION_V3;
    precise_depth.event_v2 = market_event;
    precise_depth.event_v2.raw_event.kind = QX_MARKET_ORDER_BOOK;
    precise_depth.exact_bids = &exact_level;
    precise_depth.exact_bids_len = 1;
    assert(precise_depth.schema_version == QX_MARKET_EVENT_SCHEMA_VERSION_V3);
    assert(precise_depth.exact_bids[0].price.coefficient.lo == 12345u);
    assert(precise_depth.exact_bids[0].price.scale == 12);
    assert(precise_depth.exact_bids[0].qty.scale == 18);

    qx_strategy_exact_order_amounts_v1 exact{};
    exact.quantity.coefficient = qx_raw128{123456789u, 0};
    exact.quantity.scale = 15;
    exact.filled.coefficient = qx_raw128{0, 0};

    qx_strategy_order_update_v2 order{};
    order.order_id = 7;
    order.side = QX_SIDE_BUY;
    order.qty_raw = qx_raw128{0, 0};
    order.exact_amounts = &exact;

    qx_strategy_order_status_update_v2 update{};
    update.schema_version = QX_STRATEGY_ORDER_STATUS_SCHEMA_VERSION;
    update.event = QX_ORDER_STATUS_EVENT_ORDER_STATUS;
    update.event_seq = 23;
    update.ts = 10;
    update.status = QX_ORDER_STATUS_ACCEPTED;
    update.order = order;

    assert(update.order.exact_amounts == &exact);
    assert(update.order.exact_amounts->quantity.coefficient.lo == 123456789u);
    assert(update.order.exact_amounts->quantity.scale == 15);
    assert(update.event_seq == 23);
    assert(update.status == QX_ORDER_STATUS_ACCEPTED);
}
