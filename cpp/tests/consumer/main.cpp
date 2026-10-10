#include <qianxing_ring.hpp>
#include <qianxing_app.hpp>
#include <qianxing_strategy.h>

#include <string>

int main() {
    qianxing::AppClient app([](std::string_view method, std::string_view path,
                              std::string_view body) {
        if (method != "POST" || path != "/app/compare-runs" || body.empty()) {
            return qianxing::AppResponse{400, "bad request"};
        }
        return qianxing::AppResponse{200, "{}"};
    });
    const auto response = app.compare_runs("{\"schema_version\":1,\"runs\":[]}");
    return QX_STRATEGY_API_VERSION == 1u && response.ok() ? 0 : 1;
}
