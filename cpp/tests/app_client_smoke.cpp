#include <qianxing_app.hpp>

#include <cassert>
#include <string>

int main() {
    std::string observed_path;
    std::string observed_body;
    qianxing::AppClient client([&](std::string_view method, std::string_view path,
                                  std::string_view body) {
        assert(method == "POST");
        observed_path.assign(path);
        observed_body.assign(body);
        return qianxing::AppResponse{200, R"({"runs":[]})"};
    });

    const std::string spec = R"({"schema_version":1,"runs":[]})";
    const auto response = client.compare_runs(spec);
    assert(response.ok());
    assert(response.body == R"({"runs":[]})");
    assert(observed_path == "/app/compare-runs");
    assert(observed_body == spec);

    qianxing::AppClient unavailable({});
    assert(unavailable.verify_run("{}").status == 0);
}
