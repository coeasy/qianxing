#ifndef QIANXING_STRATEGY_H
#define QIANXING_STRATEGY_H

/*
 * Qianxing Strategy API v1
 *
 * Stable C ABI for C/C++ strategies.  The strategy process never receives
 * credentials or a venue handle.  It only reads a snapshot and returns
 * OrderIntent values; the Rust runtime performs Risk/OMS/Execution checks.
 * All prices, quantities and balances are signed fixed-point raw integers.
 */

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define QX_STRATEGY_API_VERSION 1u

/* Portable representation of Rust i128.  The numeric value is
 * (hi << 64) | lo in two's-complement form. */
typedef struct qx_raw128 {
    uint64_t lo;
    int64_t hi;
} qx_raw128;

typedef enum qx_market_event_kind {
    QX_MARKET_BAR = 1,
    QX_MARKET_TICK = 2,
    QX_MARKET_TIMER = 3,
    QX_MARKET_ORDER_BOOK = 4
} qx_market_event_kind;

typedef enum qx_order_side {
    QX_SIDE_BUY = 1,
    QX_SIDE_SELL = 2
} qx_order_side;

typedef struct qx_strategy_kv {
    const char* key;
    qx_raw128 value_raw;
} qx_strategy_kv;

typedef struct qx_book_level {
    qx_raw128 price_raw;
    qx_raw128 qty_raw;
} qx_book_level;

typedef struct qx_strategy_context {
    uint32_t schema_version;
    const char* strategy_id;
    const char* strategy_version;
    const char* account_id;
    const char* venue_id;
    const char* data_fingerprint;
    uint64_t as_of;
    const qx_strategy_kv* positions;
    size_t positions_len;
    const qx_strategy_kv* cash;
    size_t cash_len;
    qx_raw128 available_margin_raw;
    uint8_t has_available_margin;
    const char* risk_state;
} qx_strategy_context;

typedef struct qx_market_event {
    uint32_t schema_version;
    qx_market_event_kind kind;
    const char* instrument;
    uint64_t ts;
    qx_raw128 open_raw;
    qx_raw128 high_raw;
    qx_raw128 low_raw;
    qx_raw128 close_raw;
    qx_raw128 volume_raw;
    qx_raw128 bid_raw;
    qx_raw128 ask_raw;
    qx_raw128 last_raw;
    uint8_t has_last;
    uint64_t sequence;
    const qx_book_level* bids;
    size_t bids_len;
    const qx_book_level* asks;
    size_t asks_len;
    const char* timer_name;
} qx_market_event;

typedef struct qx_order_intent {
    uint64_t intent_id;
    const char* instrument;
    qx_order_side side;
    qx_raw128 qty_raw;
    qx_raw128 limit_price_raw;
    uint8_t has_limit_price;
    uint8_t reduce_only;
    uint8_t post_only;
    const char* position_side; /* net, long or short; null means runtime default */
} qx_order_intent;

typedef struct qx_strategy_decision {
    uint32_t schema_version;
    const char* request_id;
    const char* strategy_id;
    uint64_t signal_id;
    qx_raw128 confidence;
    int32_t priority;
    uint64_t expires_at;
    qx_order_intent* intents;
    size_t intents_len;
} qx_strategy_decision;

typedef void* qx_strategy_handle;

typedef struct qx_strategy_vtable {
    uint32_t abi_version;
    qx_strategy_handle (*create)(const char* config_json);
    int (*on_init)(qx_strategy_handle,
                   const qx_strategy_context*,
                   char* error_buffer,
                   size_t error_buffer_len);
    int (*on_event)(qx_strategy_handle,
                    const qx_strategy_context*,
                    const qx_market_event*,
                    qx_strategy_decision*,
                    char* error_buffer,
                    size_t error_buffer_len);
    int (*on_order_update)(qx_strategy_handle,
                           const char* update_json,
                           qx_strategy_decision*,
                           char* error_buffer,
                           size_t error_buffer_len);
    void (*free_decision)(qx_strategy_handle, qx_strategy_decision*);
    void (*destroy)(qx_strategy_handle);
} qx_strategy_vtable;

/* Plugin entry point exported by a C++ strategy shared library. */
const qx_strategy_vtable* qx_strategy_get_vtable(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* QIANXING_STRATEGY_H */
