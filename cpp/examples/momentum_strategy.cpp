#include "qianxing_strategy.h"

#include <cstring>
#include <string>

namespace {

struct State {
    uint64_t next_intent_id = 1;
    std::string request_id;
};

qx_strategy_handle create_strategy(const char*) {
    return new State{};
}

int on_init(qx_strategy_handle, const qx_strategy_context*, char*, size_t) {
    return 0;
}

int on_event(qx_strategy_handle raw,
             const qx_strategy_context* context,
             const qx_market_event* event,
             qx_strategy_decision* decision,
             char* error,
             size_t error_len) {
    if (!raw || !context || !event || !decision || event->kind != QX_MARKET_BAR) {
        if (error && error_len > 0) {
            std::strncpy(error, "invalid strategy callback input", error_len - 1);
            error[error_len - 1] = '\0';
        }
        return 1;
    }

    auto* state = static_cast<State*>(raw);
    state->request_id =
        std::string(context->strategy_id) + ":" + std::to_string(event->ts);
    decision->schema_version = QX_STRATEGY_API_VERSION;
    decision->request_id = state->request_id.c_str();
    decision->strategy_id = context->strategy_id;
    decision->signal_id = event->ts;
    decision->confidence = qx_raw128_from_i64(500);
    decision->priority = 0;
    decision->expires_at = event->ts;
    decision->intents = nullptr;
    decision->intents_len = 0;

    /* The example intentionally emits no order. Production code allocates the
       intent array through its plugin allocator and implements free_decision. */
    (void)state;
    return 0;
}

void free_decision(qx_strategy_handle, qx_strategy_decision* decision) {
    if (decision) {
        decision->intents = nullptr;
        decision->intents_len = 0;
    }
}

void destroy_strategy(qx_strategy_handle raw) {
    delete static_cast<State*>(raw);
}

const qx_strategy_vtable VTABLE{
    QX_STRATEGY_API_VERSION,
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
