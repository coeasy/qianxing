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

    const std::string spec = R"({"schema_version":1,"experiment_id":"smoke"})";
    const auto response = client.run_experiment(spec);
    assert(response.ok());
    assert(response.body == R"({"runs":[]})");
    assert(observed_path == "/app/run-experiment");
    assert(observed_body == spec);

    qianxing::AppClient unavailable({});
    assert(unavailable.run_experiment("{}").status == 0);
    assert(unavailable.verify_run("{}").status == 0);
}
