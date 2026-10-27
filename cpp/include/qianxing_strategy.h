#ifndef QIANXING_STRATEGY_H
#define QIANXING_STRATEGY_H

/*
 * Qianxing Strategy API v1, C ABI v2
 *
 * Stable C ABI for C/C++ strategies.  The strategy process never receives
 * credentials or a venue handle.  It only reads a snapshot and returns
 * OrderIntent values; the Rust runtime performs Risk/OMS/Execution checks.
 * Raw compatibility fields use signed fixed-point integers. The optional
 * market-event v2 carries exact source OHLCV; v3 also carries exact
 * order-book price/quantity sidecars as coefficient/scale decimals.
 */

#include <stdint.h>
#include <stddef.h>

#if defined(_WIN32)
#define QX_STRATEGY_EXPORT __declspec(dllexport)
#elif defined(__GNUC__) || defined(__clang__)
#define QX_STRATEGY_EXPORT __attribute__((visibility("default")))
#else
#define QX_STRATEGY_EXPORT
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define QX_STRATEGY_API_VERSION 1u
#define QX_STRATEGY_ABI_VERSION 2u
#define QX_STRATEGY_ORDER_POLICY_SCHEMA_VERSION 1u
#define QX_STRATEGY_ORDER_STATUS_SCHEMA_VERSION 1u
#define QX_STRATEGY_FILL_UPDATE_SCHEMA_VERSION 2u
#define QX_STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION 1u
#define QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION 1u
#define QX_MARKET_EVENT_SCHEMA_VERSION_V2 2u
#define QX_MARKET_EVENT_SCHEMA_VERSION_V3 3u

#define QX_ORDER_STATUS_EVENT_ORDER_STATUS 1u
#define QX_ORDER_STATUS_PENDING_SUBMIT 1u
#define QX_ORDER_STATUS_SUBMITTED 2u
#define QX_ORDER_STATUS_ACCEPTED 3u
#define QX_ORDER_STATUS_WORKING 4u
#define QX_ORDER_STATUS_PARTIALLY_FILLED 5u
#define QX_ORDER_STATUS_FILLED 6u
#define QX_ORDER_STATUS_CANCEL_PENDING 7u
#define QX_ORDER_STATUS_CANCELLED 8u
#define QX_ORDER_STATUS_REJECTED 9u
#define QX_ORDER_STATUS_EXPIRED 10u
#define QX_ORDER_STATUS_UNKNOWN 11u

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

/* Order side is a fixed-width integer, not a C enum: the Rust host has to
 * reject an invalid discriminant, but reading a C enum value that is not one
 * of the declared enumerators is undefined behavior in Rust, so there is no
 * rejection path for it. The layout is identical to the former enum
 * (4 bytes), so existing plugins need no recompilation. */
typedef uint32_t qx_order_side;
#define QX_SIDE_BUY 1u
#define QX_SIDE_SELL 2u

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

/* Versioned warmup-ready event. `instrument` is null only for the zero-Bar
   global notification; strings are borrowed for the duration of the callback. */
typedef struct qx_strategy_warmup_complete_v1 {
    uint32_t struct_size;
    uint32_t schema_version;
    const char* instrument;
    size_t instrument_len;
    uint8_t has_instrument;
    uint64_t observed_bars;
} qx_strategy_warmup_complete_v1;

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
    /* UTF-8 byte lengths exclude any optional trailing NUL terminator. */
    const char* instrument;
    size_t instrument_len;
    qx_order_side side;
    qx_raw128 qty_raw;
    qx_raw128 limit_price_raw;
    uint8_t has_limit_price;
    uint8_t reduce_only;
    uint8_t post_only;
    const char* position_side; /* net, long or short; null means runtime default */
    size_t position_side_len;
} qx_order_intent;

typedef struct qx_strategy_decision {
    uint32_t schema_version;
    /* UTF-8 byte lengths exclude any optional trailing NUL terminator. */
    const char* request_id;
    size_t request_id_len;
    const char* strategy_id;
    size_t strategy_id_len;
    uint64_t signal_id;
    qx_raw128 confidence;
    int32_t priority;
    uint64_t expires_at;
    qx_order_intent* intents;
    size_t intents_len;
    /* Plugin-declared readable element capacity of intents; must be >= len. */
    size_t intents_capacity;
} qx_strategy_decision;

/* Exact decimal: coefficient / 10^scale. */
typedef struct qx_strategy_decimal_v1 {
    qx_raw128 coefficient;
    uint32_t scale;
} qx_strategy_decimal_v1;

#define QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2 2u

typedef struct qx_strategy_exact_kv_v1 {
    const char* key;
    size_t key_len;
    qx_strategy_decimal_v1 value;
} qx_strategy_exact_kv_v1;

/* Exact account state for plugins that need sub-1e-9 positions or balances.
   context_v1 preserves the stable legacy projection; all pointers are borrowed
   and valid only during qx_strategy_on_context_v2. */
typedef struct qx_strategy_context_v2 {
    uint32_t struct_size;
    uint32_t schema_version;
    qx_strategy_context context_v1;
    const qx_strategy_exact_kv_v1* positions_exact;
    size_t positions_exact_len;
    const qx_strategy_exact_kv_v1* cash_exact;
    size_t cash_exact_len;
    qx_strategy_decimal_v1 available_margin_exact;
    uint8_t has_available_margin_exact;
} qx_strategy_context_v2;

typedef struct qx_strategy_exact_ohlcv_v1 {
    qx_strategy_decimal_v1 open;
    qx_strategy_decimal_v1 high;
    qx_strategy_decimal_v1 low;
    qx_strategy_decimal_v1 close;
    qx_strategy_decimal_v1 volume;
} qx_strategy_exact_ohlcv_v1;

/* Versioned market event for the optional on_market_event_v2 symbol. The
   embedded raw_event retains the complete v1 market-event payload. Exact
   OHLCV is valid only when has_exact_ohlcv is non-zero; decimals are
   coefficient / 10^scale and are never converted through floating point. */
typedef struct qx_market_event_v2 {
    uint32_t struct_size;
    uint32_t schema_version;
    qx_market_event raw_event;
    uint8_t has_exact_ohlcv;
    qx_strategy_exact_ohlcv_v1 exact_ohlcv;
} qx_market_event_v2;

/* Exact source values for one order-book level. The sidecar array is parallel
   to the raw qx_book_level array; has_exact is zero for legacy raw levels. */
typedef struct qx_strategy_exact_book_level_v1 {
    uint8_t has_exact;
    qx_strategy_decimal_v1 price;
    qx_strategy_decimal_v1 qty;
} qx_strategy_exact_book_level_v1;

/* Versioned market event with exact order-book sidecars. The sidecar arrays
   must match the embedded raw bid/ask lengths and ordering. */
typedef struct qx_market_event_v3 {
    uint32_t struct_size;
    uint32_t schema_version;
    qx_market_event_v2 event_v2;
    const qx_strategy_exact_book_level_v1* exact_bids;
    size_t exact_bids_len;
    const qx_strategy_exact_book_level_v1* exact_asks;
    size_t exact_asks_len;
} qx_market_event_v3;

typedef struct qx_strategy_exact_order_amounts_v1 {
    qx_strategy_decimal_v1 quantity;
    qx_strategy_decimal_v1 limit_price;
    uint8_t has_limit_price;
    qx_strategy_decimal_v1 filled;
    qx_strategy_decimal_v1 quote_budget;
    uint8_t has_quote_budget;
    qx_strategy_decimal_v1 quote_filled;
    qx_strategy_decimal_v1 market_reference_price;
    uint8_t has_market_reference_price;
} qx_strategy_exact_order_amounts_v1;

typedef struct qx_strategy_order_update_v2 {
    uint64_t order_id;
    const char* account_id;
    size_t account_id_len;
    const char* instrument;
    size_t instrument_len;
    qx_order_side side;
    qx_raw128 qty_raw;
    qx_raw128 filled_raw;
    const char* strategy_id;
    size_t strategy_id_len;
    const qx_strategy_exact_order_amounts_v1* exact_amounts;
} qx_strategy_order_update_v2;

/* All pointed-to bytes are host-owned and valid only until this callback returns.
   event/status are fixed-width codes, not C enums, to avoid invalid-enum UB. */
typedef struct qx_strategy_order_status_update_v2 {
    uint32_t schema_version;
    uint32_t event;
    uint64_t event_seq;
    uint64_t ts;
    uint32_t status;
    qx_strategy_order_update_v2 order;
    const char* reason;
    size_t reason_len;
    uint8_t has_reason;
} qx_strategy_order_status_update_v2;

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
QX_STRATEGY_EXPORT const qx_strategy_vtable* qx_strategy_get_vtable(void);

/* Optional precise market-event callback. If a source Bar carries exact OHLCV,
   the host requires this symbol and will not silently project it to raw v1. */
QX_STRATEGY_EXPORT int qx_strategy_on_market_event_v2(
    qx_strategy_handle strategy,
    const qx_strategy_context* context,
    const qx_market_event_v2* event,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional v3 callback supersedes v2 when present. Exact OHLCV remains in
   event_v2; exact book levels are carried in parallel sidecar arrays. The
   host rejects exact order books when this symbol is unavailable. */
QX_STRATEGY_EXPORT int qx_strategy_on_market_event_v3(
    qx_strategy_handle strategy,
    const qx_strategy_context* context,
    const qx_market_event_v3* event,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional lifecycle exports detected by symbol name; they do not change the vtable ABI. */
QX_STRATEGY_EXPORT int qx_strategy_on_start_v1(qx_strategy_handle strategy,
                                               const qx_strategy_context* context,
                                               char* error_buffer,
                                               size_t error_buffer_len);
QX_STRATEGY_EXPORT int qx_strategy_on_stop_v1(qx_strategy_handle strategy,
                                              char* error_buffer,
                                              size_t error_buffer_len);
/* Optional v2 stop callback receives StrategyStopContextV1 JSON. When absent,
   the host calls the legacy no-context v1 symbol if it is present. */
QX_STRATEGY_EXPORT int qx_strategy_on_stop_v2(qx_strategy_handle strategy,
                                              const char* stop_context_json,
                                              char* error_buffer,
                                              size_t error_buffer_len);
/* Optional diagnostic callback receiving bounded StrategyErrorV1 JSON. Its
   failure never replaces the original strategy callback failure. */
QX_STRATEGY_EXPORT int qx_strategy_on_error_v1(qx_strategy_handle strategy,
                                               const char* strategy_error_json,
                                               char* error_buffer,
                                               size_t error_buffer_len);

/* Optional context refresh. The host calls it before each legacy callback that
   receives qx_strategy_context. If an exact account value cannot be represented
   by the v1 1e-9 projection, the host requires this export and fails closed
   rather than sending a lossy context. */
QX_STRATEGY_EXPORT int qx_strategy_on_context_v2(
    qx_strategy_handle strategy,
    const qx_strategy_context_v2* context,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional lifecycle exports; absent history declaration means zero warmup.
   The host calls on_warmup_complete_v1 after warmup Bars for one instrument. */
QX_STRATEGY_EXPORT int qx_strategy_history_requirements_v1(
    qx_strategy_handle strategy,
    uint64_t* warmup_bars,
    char* error_buffer,
    size_t error_buffer_len);
QX_STRATEGY_EXPORT int qx_strategy_on_warmup_complete_v1(
    qx_strategy_handle strategy,
    const qx_strategy_warmup_complete_v1* event,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional committed order-state callback. The payload is versioned JSON;
   this symbol is resolved independently so the vtable ABI and fill callback
   retain their existing layout and semantics. */
QX_STRATEGY_EXPORT int qx_strategy_on_order_status_v1(
    qx_strategy_handle strategy,
    const char* update_json,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Typed order-status callback. The host prefers this optional v2 symbol when
   present and falls back to the v1 JSON callback for existing plugins. */
QX_STRATEGY_EXPORT int qx_strategy_on_order_status_v2(
    qx_strategy_handle strategy,
    const qx_strategy_order_status_update_v2* update,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional committed fill callback. The host passes a versioned JSON
   StrategyFillUpdateV2 payload containing event_seq and ts. If this symbol is
   absent, the host projects the payload to the legacy fill-only vtable callback. */
QX_STRATEGY_EXPORT int qx_strategy_on_fill_update_v2(
    qx_strategy_handle strategy,
    const char* update_json,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional account/position snapshot callback. The payload is a versioned
   StrategyAccountUpdateV1 JSON object with account_id, venue_id, event_seq and
   either balances (account_snapshot) or positions (position_snapshot). It is
   an observation from the account EventLog; it does not mutate the Rust ledger. */
QX_STRATEGY_EXPORT int qx_strategy_on_account_update_v1(
    qx_strategy_handle strategy,
    const char* update_json,
    qx_strategy_decision* decision,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional per-intent derivative policy export. The host calls it after an
   intent is returned, using the intent_id as the correlation key. The plugin
   writes one bounded UTF-8 JSON object (output_len excludes an optional NUL):
   {"schema_version":1,"margin_mode":"cross",
    "position_mode":"hedge","leverage":3}. Provide all three policy fields
   together, or only schema_version to preserve runtime defaults. */
QX_STRATEGY_EXPORT int qx_strategy_get_order_policy_v1(
    qx_strategy_handle strategy,
    uint64_t intent_id,
    char* output,
    size_t output_capacity,
    size_t* output_len,
    char* error_buffer,
    size_t error_buffer_len);

/* Optional diagnostics-only export. The host supplies the bounded output buffer;
   the function returns compact UTF-8 JSON through output_len and transfers no memory. */
QX_STRATEGY_EXPORT int qx_strategy_diagnostic_state(qx_strategy_handle strategy,
                                                    char* output,
                                                    size_t output_capacity,
                                                    size_t* output_len);

/* Optional durable-state exports. Implement both or neither. Snapshot writes a
   UTF-8 JSON object into the host-owned output buffer and sets output_len;
   restore receives that object as borrowed UTF-8 bytes and reports failures
   through the bounded error buffer. The host limits checkpoint JSON to 64 KiB. */
QX_STRATEGY_EXPORT int qx_strategy_checkpoint_snapshot_v1(qx_strategy_handle strategy,
                                                          char* output,
                                                          size_t output_capacity,
                                                          size_t* output_len);
QX_STRATEGY_EXPORT int qx_strategy_checkpoint_restore_v1(qx_strategy_handle strategy,
                                                         const char* input,
                                                         size_t input_len,
                                                         char* error_buffer,
                                                         size_t error_buffer_len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* QIANXING_STRATEGY_H */
