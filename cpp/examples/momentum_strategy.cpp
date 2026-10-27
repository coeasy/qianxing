#include "qianxing_strategy.h"

#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <cctype>
#include <new>
#include <string>
#include <vector>

namespace {

struct State {
    uint64_t next_intent_id = 1;
    uint64_t exact_context_updates = 0;
    uint64_t exact_position_coefficient = 0;
    uint64_t exact_cash_coefficient = 0;
    uint64_t exact_margin_coefficient = 0;
    uint32_t exact_position_scale = 0;
    uint32_t exact_cash_scale = 0;
    uint32_t exact_margin_scale = 0;
    uint64_t order_status_events = 0;
    uint64_t warmup_complete_events = 0;
    uint64_t strategy_error_events = 0;
    uint64_t last_warmup_observed_bars = 0;
    uint64_t order_status_v1_events = 0;
    uint64_t order_status_v2_events = 0;
    uint64_t fill_update_v2_events = 0;
    uint64_t account_update_events = 0;
    uint64_t exact_market_events = 0;
    uint64_t exact_orderbook_events = 0;
    uint64_t last_exact_book_price_coefficient = 0;
    uint32_t last_exact_book_price_scale = 0;
    uint64_t last_exact_book_qty_coefficient = 0;
    uint32_t last_exact_book_qty_scale = 0;
    uint64_t last_order_status_event_seq = 0;
    uint64_t last_fill_event_seq = 0;
    uint64_t last_account_update_event_seq = 0;
    uint64_t last_exact_close_coefficient = 0;
    uint32_t last_exact_close_scale = 0;
    std::string request_id;
    std::string instrument = "BTCUSDT.BINANCE";
    std::string last_warmup_instrument;
    std::vector<std::string> callback_order;
    uint64_t warmup_bar_callbacks = 0;
    bool started = false;
    bool stopped = false;
    bool stop_context_seen = false;
};

void record_callback(State* state, const std::string& callback) {
    if (state && state->callback_order.size() < 128) {
        state->callback_order.push_back(callback);
    }
}

std::string callback_order_json(const State& state) {
    std::string result = "[";
    for (size_t index = 0; index < state.callback_order.size(); ++index) {
        if (index != 0) {
            result += ",";
        }
        // The sample records only fixed labels, instrument IDs and integer timestamps.
        result += "\"" + state.callback_order[index] + "\"";
    }
    result += "]";
    return result;
}

bool has_non_whitespace(const char* value) {
    if (!value) {
        return false;
    }
    for (const auto* cursor = reinterpret_cast<const unsigned char*>(value); *cursor; ++cursor) {
        if (!std::isspace(*cursor)) {
            return true;
        }
    }
    return false;
}

constexpr qx_raw128 raw128_from_i64(int64_t value) {
    return qx_raw128{
        static_cast<uint64_t>(value),
        value < 0 ? -1 : 0,
    };
}

qx_strategy_handle create_strategy(const char*) {
    return new State{};
}

int on_init(qx_strategy_handle raw, const qx_strategy_context*, char*, size_t) {
    record_callback(static_cast<State*>(raw), "on_init");
    return 0;
}

int on_event(qx_strategy_handle raw,
             const qx_strategy_context* context,
             const qx_market_event* event,
             qx_strategy_decision* decision,
             char* error,
             size_t error_len) {
    const bool is_timer = event && event->kind == QX_MARKET_TIMER;
    const bool is_order_book = event && event->kind == QX_MARKET_ORDER_BOOK;
    if (!raw || !context || !event || !decision
        || (!is_timer && !is_order_book && event->kind != QX_MARKET_BAR)
        || (is_timer && !has_non_whitespace(event->timer_name))) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid strategy callback input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }

    auto* state = static_cast<State*>(raw);
    if (is_timer) {
        record_callback(state, "market:Timer:" + std::to_string(event->ts));
    } else if (is_order_book) {
        record_callback(state, "market:OrderBook:" + std::to_string(event->ts));
    } else if (state->warmup_bar_callbacks < 2) {
        record_callback(state, "market:Bar:" + std::to_string(event->ts));
        ++state->warmup_bar_callbacks;
    }
    state->request_id =
        std::string(context->strategy_id) + ":" + std::to_string(event->ts);
    if (is_timer) {
        state->request_id += ":" + std::string(event->timer_name);
    }
    decision->schema_version = QX_STRATEGY_API_VERSION;
    decision->request_id = state->request_id.c_str();
    decision->request_id_len = state->request_id.size();
    decision->strategy_id = context->strategy_id;
    decision->strategy_id_len = std::strlen(context->strategy_id);
    decision->signal_id = event->ts;
    decision->confidence = raw128_from_i64(500);
    decision->priority = 0;
    decision->expires_at = event->ts;
    if (is_timer || is_order_book) {
        decision->intents = nullptr;
        decision->intents_len = 0;
        decision->intents_capacity = 0;
        return 0;
    }
    auto* intents = new (std::nothrow) qx_order_intent[1]{};
    if (!intents) {
        if (error && error_len > 0) {
            std::strncpy(error, "failed to allocate strategy intent", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 2;
    }
    intents[0].intent_id = state->next_intent_id++;
    intents[0].instrument = state->instrument.c_str();
    intents[0].instrument_len = state->instrument.size();
    intents[0].side = QX_SIDE_BUY;
    intents[0].qty_raw = raw128_from_i64(1'000'000'000);
    intents[0].has_limit_price = 0;
    intents[0].reduce_only = 0;
    intents[0].post_only = 0;
    intents[0].position_side = "long";
    intents[0].position_side_len = 4;
    decision->intents = intents;
    decision->intents_len = 1;
    decision->intents_capacity = 1;
    return 0;
}

void free_decision(qx_strategy_handle, qx_strategy_decision* decision) {
    if (decision) {
        delete[] decision->intents;
        decision->intents = nullptr;
        decision->intents_len = 0;
        decision->intents_capacity = 0;
    }
}

void destroy_strategy(qx_strategy_handle raw) {
    delete static_cast<State*>(raw);
}

bool read_json_u64(const char* json, const char* field, uint64_t* output) {
    if (!json || !field || !output) {
        return false;
    }
    const std::string key = std::string("\"") + field + "\"";
    const char* match = std::strstr(json, key.c_str());
    if (!match) {
        return false;
    }
    const char* colon = std::strchr(match + key.size(), ':');
    if (!colon) {
        return false;
    }
    const char* value = colon + 1;
    while (*value && std::isspace(static_cast<unsigned char>(*value))) {
        ++value;
    }
    char* end = nullptr;
    const auto parsed = std::strtoull(value, &end, 10);
    if (end == value) {
        return false;
    }
    *output = static_cast<uint64_t>(parsed);
    return true;
}

int on_fill_update_v2(qx_strategy_handle raw,
                      const char* update_json,
                      qx_strategy_decision* decision,
                      char* error,
                      size_t error_len) {
    uint64_t schema_version = 0;
    uint64_t event_seq = 0;
    uint64_t ts = 0;
    if (!raw || !update_json || !decision
        || !read_json_u64(update_json, "schema_version", &schema_version)
        || schema_version != QX_STRATEGY_FILL_UPDATE_SCHEMA_VERSION
        || !std::strstr(update_json, "\"event\":\"fill\"")
        || !read_json_u64(update_json, "event_seq", &event_seq)
        || !read_json_u64(update_json, "ts", &ts) || ts == 0) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid sequence-bound fill update", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->fill_update_v2_events;
    state->last_fill_event_seq = event_seq;
    record_callback(state, "fill:" + std::to_string(event_seq));
    return 0;
}

int on_order_status(qx_strategy_handle raw,
                    const char* update_json,
                    qx_strategy_decision* decision,
                    char* error,
                    size_t error_len) {
    if (!raw || !update_json || !decision || update_json[0] == '\0') {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid order status input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->order_status_events;
    ++state->order_status_v1_events;
    return 0;
}

const qx_strategy_vtable VTABLE{
    QX_STRATEGY_ABI_VERSION,
    &create_strategy,
    &on_init,
    &on_event,
    nullptr,
    &free_decision,
    &destroy_strategy,
};

} // namespace

extern "C" const qx_strategy_vtable* qx_strategy_get_vtable(void) {
    return &VTABLE;
}

extern "C" int qx_strategy_on_market_event_v2(
    qx_strategy_handle raw,
    const qx_strategy_context* context,
    const qx_market_event_v2* event,
    qx_strategy_decision* decision,
    char* error,
    size_t error_len) {
    if (!event || event->struct_size < sizeof(qx_market_event_v2)
        || event->schema_version != QX_MARKET_EVENT_SCHEMA_VERSION_V2
        || event->has_exact_ohlcv > 1) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid market event v2 envelope", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    if (event->has_exact_ohlcv
        && (event->raw_event.kind != QX_MARKET_BAR
            || event->exact_ohlcv.open.scale > 18
            || event->exact_ohlcv.high.scale > 18
            || event->exact_ohlcv.low.scale > 18
            || event->exact_ohlcv.close.scale > 18
            || event->exact_ohlcv.volume.scale > 18)) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid exact OHLCV market event", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 2;
    }
    if (event->has_exact_ohlcv) {
        auto* state = static_cast<State*>(raw);
        ++state->exact_market_events;
        state->last_exact_close_coefficient = event->exact_ohlcv.close.coefficient.lo;
        state->last_exact_close_scale = event->exact_ohlcv.close.scale;
    }
    return on_event(raw, context, &event->raw_event, decision, error, error_len);
}

extern "C" int qx_strategy_on_market_event_v3(
    qx_strategy_handle raw,
    const qx_strategy_context* context,
    const qx_market_event_v3* event,
    qx_strategy_decision* decision,
    char* error,
    size_t error_len) {
    if (!raw || !event || event->struct_size < sizeof(qx_market_event_v3)
        || event->schema_version != QX_MARKET_EVENT_SCHEMA_VERSION_V3
        || event->exact_bids_len != event->event_v2.raw_event.bids_len
        || event->exact_asks_len != event->event_v2.raw_event.asks_len
        || (event->exact_bids_len && !event->exact_bids)
        || (event->exact_asks_len && !event->exact_asks)) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid market event v3 envelope", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    const auto check_levels = [](const qx_strategy_exact_book_level_v1* levels,
                                 size_t count) {
        for (size_t i = 0; i < count; ++i) {
            if (levels[i].has_exact > 1
                || (levels[i].has_exact
                    && (levels[i].price.scale > 18 || levels[i].qty.scale > 18))) {
                return false;
            }
        }
        return true;
    };
    if (!check_levels(event->exact_bids, event->exact_bids_len)
        || !check_levels(event->exact_asks, event->exact_asks_len)) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid exact order-book level", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 2;
    }
    if (event->event_v2.raw_event.kind == QX_MARKET_ORDER_BOOK) {
        auto* state = static_cast<State*>(raw);
        bool recorded = false;
        const auto record_first_exact = [&](const qx_strategy_exact_book_level_v1* levels,
                                            size_t count) {
            for (size_t i = 0; i < count; ++i) {
                if (levels[i].has_exact) {
                    ++state->exact_orderbook_events;
                    state->last_exact_book_price_coefficient = levels[i].price.coefficient.lo;
                    state->last_exact_book_price_scale = levels[i].price.scale;
                    state->last_exact_book_qty_coefficient = levels[i].qty.coefficient.lo;
                    state->last_exact_book_qty_scale = levels[i].qty.scale;
                    return true;
                }
            }
            return false;
        };
        recorded = record_first_exact(event->exact_bids, event->exact_bids_len);
        if (!recorded) {
            record_first_exact(event->exact_asks, event->exact_asks_len);
        }
    }
    return qx_strategy_on_market_event_v2(
        raw, context, &event->event_v2, decision, error, error_len);
}

extern "C" int qx_strategy_on_start_v1(qx_strategy_handle raw,
                                        const qx_strategy_context* context,
                                        char* error,
                                        size_t error_len) {
    if (!raw || !context) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid strategy start input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    static_cast<State*>(raw)->started = true;
    record_callback(static_cast<State*>(raw), "on_start");
    return 0;
}

extern "C" int qx_strategy_on_stop_v1(qx_strategy_handle raw,
                                       char* error,
                                       size_t error_len) {
    if (!raw) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid strategy stop input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    static_cast<State*>(raw)->stopped = true;
    return 0;
}

extern "C" int qx_strategy_on_stop_v2(qx_strategy_handle raw,
                                       const char* stop_context_json,
                                       char* error,
                                       size_t error_len) {
    if (!raw || !stop_context_json
        || !std::strstr(stop_context_json, "\"schema_version\":1")
        || !std::strstr(stop_context_json, "\"reason\":\"host_stop\"")
        || !std::strstr(stop_context_json, "\"context\":{")) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid StrategyStopContextV1 JSON", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    state->stopped = true;
    state->stop_context_seen = true;
    record_callback(state, "on_stop:host_stop");
    return 0;
}

extern "C" int qx_strategy_on_error_v1(qx_strategy_handle raw,
                                        const char* strategy_error_json,
                                        char* error,
                                        size_t error_len) {
    if (!raw || !strategy_error_json
        || !std::strstr(strategy_error_json, "\"schema_version\":1")
        || !std::strstr(strategy_error_json, "\"stage\":")
        || !std::strstr(strategy_error_json, "\"strategy_id\":")
        || !std::strstr(strategy_error_json, "\"message\":")) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid StrategyErrorV1 JSON", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->strategy_error_events;
    record_callback(state, "on_error_v1:market_event");
    return 0;
}

extern "C" int qx_strategy_on_context_v2(qx_strategy_handle raw,
                                          const qx_strategy_context_v2* context,
                                          char* error,
                                          size_t error_len) {
    constexpr char expected_position[] = "ALTUSDT.BINANCE";
    constexpr char expected_cash[] = "USDT";
    const bool valid = raw && context
        && context->struct_size == sizeof(qx_strategy_context_v2)
        && context->schema_version == QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2
        && context->context_v1.schema_version == QX_STRATEGY_API_VERSION
        && context->positions_exact_len == 1 && context->positions_exact
        && context->cash_exact_len == 1 && context->cash_exact
        && context->has_available_margin_exact == 1
        && context->context_v1.positions_len == 1 && context->context_v1.positions
        && context->context_v1.cash_len == 1 && context->context_v1.cash
        && context->positions_exact[0].key
        && context->positions_exact[0].key_len == sizeof(expected_position) - 1
        && std::memcmp(context->positions_exact[0].key, expected_position,
                       sizeof(expected_position) - 1) == 0
        && context->cash_exact[0].key
        && context->cash_exact[0].key_len == sizeof(expected_cash) - 1
        && std::memcmp(context->cash_exact[0].key, expected_cash,
                       sizeof(expected_cash) - 1) == 0
        && context->positions_exact[0].value.coefficient.lo == 1
        && context->positions_exact[0].value.coefficient.hi == 0
        && context->positions_exact[0].value.scale == 18
        && context->cash_exact[0].value.coefficient.lo == 7
        && context->cash_exact[0].value.coefficient.hi == 0
        && context->cash_exact[0].value.scale == 18
        && context->available_margin_exact.coefficient.lo == 9
        && context->available_margin_exact.coefficient.hi == 0
        && context->available_margin_exact.scale == 18;
    if (!valid) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid exact StrategyContextV2 input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->exact_context_updates;
    state->exact_position_coefficient = context->positions_exact[0].value.coefficient.lo;
    state->exact_position_scale = context->positions_exact[0].value.scale;
    state->exact_cash_coefficient = context->cash_exact[0].value.coefficient.lo;
    state->exact_cash_scale = context->cash_exact[0].value.scale;
    state->exact_margin_coefficient = context->available_margin_exact.coefficient.lo;
    state->exact_margin_scale = context->available_margin_exact.scale;
    return 0;
}

extern "C" int qx_strategy_history_requirements_v1(qx_strategy_handle raw,
                                                       uint64_t* warmup_bars,
                                                       char* error,
                                                       size_t error_len) {
    if (!raw || !warmup_bars) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid strategy history requirements input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    *warmup_bars = 2;
    return 0;
}

extern "C" int qx_strategy_on_warmup_complete_v1(
    qx_strategy_handle raw,
    const qx_strategy_warmup_complete_v1* event,
    char* error,
    size_t error_len) {
    constexpr char expected_instrument[] = "BTCUSDT.BINANCE";
    const bool valid = raw && event
        && event->struct_size == sizeof(qx_strategy_warmup_complete_v1)
        && event->schema_version == QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION
        && event->has_instrument == 1
        && event->instrument
        && event->instrument_len == sizeof(expected_instrument) - 1
        && std::memcmp(event->instrument, expected_instrument, sizeof(expected_instrument) - 1) == 0
        && event->observed_bars == 2;
    if (!valid) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid warmup-complete input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->warmup_complete_events;
    state->last_warmup_observed_bars = event->observed_bars;
    state->last_warmup_instrument.assign(event->instrument, event->instrument_len);
    record_callback(
        state,
        "warmup_complete:" + state->last_warmup_instrument + ":"
            + std::to_string(event->observed_bars));
    return 0;
}

extern "C" int qx_strategy_on_order_status_v1(qx_strategy_handle raw,
                                                const char* update_json,
                                                qx_strategy_decision* decision,
                                                char* error,
                                                size_t error_len) {
    return on_order_status(raw, update_json, decision, error, error_len);
}

extern "C" int qx_strategy_on_order_status_v2(
    qx_strategy_handle raw,
    const qx_strategy_order_status_update_v2* update,
    qx_strategy_decision* decision,
    char* error,
    size_t error_len) {
    if (!raw || !update || !decision
        || update->schema_version != QX_STRATEGY_ORDER_STATUS_SCHEMA_VERSION
        || update->event != QX_ORDER_STATUS_EVENT_ORDER_STATUS
        || update->event_seq == 0 || update->ts == 0
        || !update->order.account_id || update->order.account_id_len == 0
        || !update->order.instrument || update->order.instrument_len == 0
        || !update->order.strategy_id || update->order.strategy_id_len == 0
        || update->order.order_id == 0
        || (update->order.side != QX_SIDE_BUY && update->order.side != QX_SIDE_SELL)
        || update->status < QX_ORDER_STATUS_PENDING_SUBMIT
        || update->status > QX_ORDER_STATUS_UNKNOWN) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid typed order status input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    const bool reason_is_valid = update->has_reason <= 1
        && (update->has_reason
            ? update->reason && update->reason_len <= 2048
            : !update->reason && update->reason_len == 0);
    if (!reason_is_valid) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid order status reason", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 2;
    }
    if (update->order.exact_amounts) {
        const auto* exact = update->order.exact_amounts;
        if (exact->quantity.scale > 18 || exact->limit_price.scale > 18
            || exact->filled.scale > 18 || exact->quote_budget.scale > 18
            || exact->quote_filled.scale > 18 || exact->market_reference_price.scale > 18) {
            if (error && error_len > 0) {
                std::strncpy(error, "invalid exact order amount scale", error_len - 1);
                error[error_len - 1] = '\0';
            }
            return 3;
        }
    }
    auto* state = static_cast<State*>(raw);
    ++state->order_status_events;
    ++state->order_status_v2_events;
    state->last_order_status_event_seq = update->event_seq;
    record_callback(state, "order_status:" + std::to_string(update->event_seq));
    return 0;
}

extern "C" int qx_strategy_on_fill_update_v2(qx_strategy_handle raw,
                                               const char* update_json,
                                               qx_strategy_decision* decision,
                                               char* error,
                                               size_t error_len) {
    return on_fill_update_v2(raw, update_json, decision, error, error_len);
}

extern "C" int qx_strategy_on_account_update_v1(qx_strategy_handle raw,
                                                const char* update_json,
                                                qx_strategy_decision* decision,
                                                char* error,
                                                size_t error_len) {
    uint64_t schema_version = 0;
    uint64_t event_seq = 0;
    uint64_t ts = 0;
    const bool event_kind = update_json
        && (std::strstr(update_json, "\"event\":\"account_snapshot\"")
            || std::strstr(update_json, "\"event\":\"position_snapshot\""));
    if (!raw || !update_json || !decision || !event_kind
        || !read_json_u64(update_json, "schema_version", &schema_version)
        || schema_version != QX_STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION
        || !read_json_u64(update_json, "event_seq", &event_seq)
        || !read_json_u64(update_json, "ts", &ts) || ts == 0) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid account/position snapshot update", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    auto* state = static_cast<State*>(raw);
    ++state->account_update_events;
    state->last_account_update_event_seq = event_seq;
    if (std::strstr(update_json, "\"event\":\"account_snapshot\"")) {
        record_callback(state, "account_snapshot:" + std::to_string(event_seq));
    } else {
        record_callback(state, "position_snapshot:" + std::to_string(event_seq));
    }
    return 0;
}

extern "C" int qx_strategy_get_order_policy_v1(qx_strategy_handle raw,
                                                 uint64_t intent_id,
                                                 char* output,
                                                 size_t output_capacity,
                                                 size_t* output_len,
                                                 char* error,
                                                 size_t error_len) {
    if (!raw || !output || !output_len) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid order policy input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }
    (void)intent_id;
    constexpr char payload[] =
        "{\"schema_version\":1,\"margin_mode\":\"cross\","
        "\"position_mode\":\"hedge\",\"leverage\":3}";
    constexpr size_t payload_len = sizeof(payload) - 1;
    if (output_capacity < payload_len) {
        if (error && error_len > 0) {
            std::strncpy(error, "order policy output buffer too small", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 2;
    }
    std::memcpy(output, payload, payload_len);
    *output_len = payload_len;
    return 0;
}

extern "C" int qx_strategy_diagnostic_state(qx_strategy_handle raw,
                                             char* output,
                                             size_t output_capacity,
                                             size_t* output_len) {
    if (!raw || !output || output_capacity == 0 || !output_len) {
        return 1;
    }
    const auto* state = static_cast<const State*>(raw);
    const auto callback_json = callback_order_json(*state);
    const int written = std::snprintf(
        output,
        output_capacity,
        "{\"next_intent_id\":%llu,\"exact_context_updates\":%llu,\"exact_position_coefficient\":%llu,\"exact_position_scale\":%u,\"exact_cash_coefficient\":%llu,\"exact_cash_scale\":%u,\"exact_margin_coefficient\":%llu,\"exact_margin_scale\":%u,\"order_status_events\":%llu,\"order_status_v1_events\":%llu,\"order_status_v2_events\":%llu,\"last_order_status_event_seq\":%llu,\"account_update_events\":%llu,\"last_account_update_event_seq\":%llu,\"fill_update_v2_events\":%llu,\"last_fill_event_seq\":%llu,\"exact_market_events\":%llu,\"last_exact_close_coefficient\":%llu,\"last_exact_close_scale\":%u,\"exact_orderbook_events\":%llu,\"last_exact_book_price_coefficient\":%llu,\"last_exact_book_price_scale\":%u,\"last_exact_book_qty_coefficient\":%llu,\"last_exact_book_qty_scale\":%u,\"warmup_complete_events\":%llu,\"last_warmup_instrument\":\"%s\",\"last_warmup_observed_bars\":%llu,\"has_request\":%s,\"started\":%s,\"stopped\":%s,\"stop_context_seen\":%s,\"strategy_error_events\":%llu,\"callback_order\":%s}",
        static_cast<unsigned long long>(state->next_intent_id),
        static_cast<unsigned long long>(state->exact_context_updates),
        static_cast<unsigned long long>(state->exact_position_coefficient),
        state->exact_position_scale,
        static_cast<unsigned long long>(state->exact_cash_coefficient),
        state->exact_cash_scale,
        static_cast<unsigned long long>(state->exact_margin_coefficient),
        state->exact_margin_scale,
        static_cast<unsigned long long>(state->order_status_events),
        static_cast<unsigned long long>(state->order_status_v1_events),
        static_cast<unsigned long long>(state->order_status_v2_events),
        static_cast<unsigned long long>(state->last_order_status_event_seq),
        static_cast<unsigned long long>(state->account_update_events),
        static_cast<unsigned long long>(state->last_account_update_event_seq),
        static_cast<unsigned long long>(state->fill_update_v2_events),
        static_cast<unsigned long long>(state->last_fill_event_seq),
        static_cast<unsigned long long>(state->exact_market_events),
        static_cast<unsigned long long>(state->last_exact_close_coefficient),
        state->last_exact_close_scale,
        static_cast<unsigned long long>(state->exact_orderbook_events),
        static_cast<unsigned long long>(state->last_exact_book_price_coefficient),
        state->last_exact_book_price_scale,
        static_cast<unsigned long long>(state->last_exact_book_qty_coefficient),
        state->last_exact_book_qty_scale,
        static_cast<unsigned long long>(state->warmup_complete_events),
        state->last_warmup_instrument.c_str(),
        static_cast<unsigned long long>(state->last_warmup_observed_bars),
        state->request_id.empty() ? "false" : "true",
        state->started ? "true" : "false",
        state->stopped ? "true" : "false",
        state->stop_context_seen ? "true" : "false",
        static_cast<unsigned long long>(state->strategy_error_events),
        callback_json.c_str());
    if (written < 0 || static_cast<size_t>(written) >= output_capacity) {
        return 2;
    }
    *output_len = static_cast<size_t>(written);
    return 0;
}

extern "C" int qx_strategy_checkpoint_snapshot_v1(qx_strategy_handle raw,
                                                    char* output,
                                                    size_t output_capacity,
                                                    size_t* output_len) {
    if (!raw || !output || !output_len) {
        return 1;
    }
    const auto* state = static_cast<const State*>(raw);
    const int written = std::snprintf(output,
                                      output_capacity,
                                      "{\"next_intent_id\":%llu}",
                                      static_cast<unsigned long long>(state->next_intent_id));
    if (written < 0 || static_cast<size_t>(written) >= output_capacity) {
        return 2;
    }
    *output_len = static_cast<size_t>(written);
    return 0;
}

extern "C" int qx_strategy_checkpoint_restore_v1(qx_strategy_handle raw,
                                                   const char* input,
                                                   size_t input_len,
                                                   char* error_buffer,
                                                   size_t error_buffer_len) {
    if (!raw || !input || input_len == 0 || input_len > 65536) {
        if (error_buffer && error_buffer_len > 0) {
            std::snprintf(error_buffer, error_buffer_len, "invalid checkpoint input");
        }
        return 1;
    }
    const std::string payload(input, input_len);
    const auto key = payload.find("\"next_intent_id\"");
    const auto colon = key == std::string::npos ? std::string::npos : payload.find(':', key);
    if (colon == std::string::npos) {
        if (error_buffer && error_buffer_len > 0) {
            std::snprintf(error_buffer, error_buffer_len, "checkpoint is missing next_intent_id");
        }
        return 2;
    }
    const char* value_start = payload.c_str() + colon + 1;
    char* value_end = nullptr;
    const auto next_intent_id = std::strtoull(value_start, &value_end, 10);
    if (value_end == value_start || next_intent_id == 0) {
        if (error_buffer && error_buffer_len > 0) {
            std::snprintf(error_buffer, error_buffer_len, "checkpoint next_intent_id is invalid");
        }
        return 3;
    }
    static_cast<State*>(raw)->next_intent_id = static_cast<uint64_t>(next_intent_id);
    return 0;
}
