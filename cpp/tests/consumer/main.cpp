#include <qianxing_ring.hpp>
#include <qianxing_app.hpp>
#include <qianxing_strategy.h>

#include <string>

int main() {
    qianxing::AppClient app([](std::string_view method, std::string_view path,
                              std::string_view body) {
        // The route comes from the header's declaration surface, never from a
        // copy of the literal: this file is the template downstream users copy.
        if (method != "POST" || path != qianxing::routes::kCompareRuns || body.empty()) {
            return qianxing::AppResponse{400, "bad request"};
        }
        return qianxing::AppResponse{200, "{}"};
    });
    const auto response = app.compare_runs("{\"schema_version\":1,\"runs\":[]}");
    return QX_STRATEGY_API_VERSION == 1u && QX_STRATEGY_ABI_VERSION == 2u && response.ok() ? 0 : 1;
}
