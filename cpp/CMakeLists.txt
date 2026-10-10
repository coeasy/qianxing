cmake_minimum_required(VERSION 3.20)
project(qianxing_strategy_sdk VERSION 0.1.0 LANGUAGES CXX)

include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

if(POLICY CMP0135)
    cmake_policy(SET CMP0135 NEW)
endif()

add_executable(qianxing_ring_smoke tests/ring_smoke.cpp)
target_include_directories(qianxing_ring_smoke PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_ring_smoke PRIVATE cxx_std_17)

add_library(qianxing_strategy_example SHARED examples/momentum_strategy.cpp)
target_include_directories(qianxing_strategy_example PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_features(qianxing_strategy_example PRIVATE cxx_std_17)

# Installable header-only SDK surface. Consumers get the stable C ABI and
# shared-memory ring headers without depending on this repository's build tree.
add_library(qianxing_strategy_sdk INTERFACE)
add_library(Qianxing::StrategySDK ALIAS qianxing_strategy_sdk)
set_target_properties(qianxing_strategy_sdk PROPERTIES EXPORT_NAME StrategySDK)
target_include_directories(qianxing_strategy_sdk INTERFACE
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
)
target_compile_features(qianxing_strategy_sdk INTERFACE cxx_std_17)

install(DIRECTORY include/ DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(TARGETS qianxing_strategy_sdk EXPORT QianxingStrategySDKTargets)
install(EXPORT QianxingStrategySDKTargets
    FILE QianxingStrategySDKTargets.cmake
    NAMESPACE Qianxing::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/QianxingStrategySDK
)
configure_package_config_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/cmake/QianxingStrategySDKConfig.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/QianxingStrategySDKConfig.cmake
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/QianxingStrategySDK
)
write_basic_package_version_file(
    ${CMAKE_CURRENT_BINARY_DIR}/QianxingStrategySDKConfigVersion.cmake
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion
)
install(FILES
    ${CMAKE_CURRENT_BINARY_DIR}/QianxingStrategySDKConfig.cmake
    ${CMAKE_CURRENT_BINARY_DIR}/QianxingStrategySDKConfigVersion.cmake
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/QianxingStrategySDK
)

option(QX_BUILD_JSONL_EXAMPLE "Build the external JSONL strategy example" OFF)
if(QX_BUILD_JSONL_EXAMPLE)
    find_package(nlohmann_json CONFIG QUIET)
    if(NOT TARGET nlohmann_json::nlohmann_json)
        # 只有 Linux 发行版提供现成的 CONFIG 包；Windows/macOS 上退化为按 URL 拉取
        # 同一版本的 header-only 目标，让三平台跑同一条 JSONL + 共享内存环验收。
        include(FetchContent)
        set(JSON_BuildTests OFF CACHE INTERNAL "")
        set(JSON_Install OFF CACHE INTERNAL "")
        FetchContent_Declare(nlohmann_json
            URL https://github.com/nlohmann/json/releases/download/v3.11.3/json.tar.xz)
        FetchContent_MakeAvailable(nlohmann_json)
    endif()
    add_executable(qianxing_strategy_jsonl examples/jsonl_strategy.cpp)
    target_link_libraries(qianxing_strategy_jsonl PRIVATE nlohmann_json::nlohmann_json)
    target_include_directories(qianxing_strategy_jsonl PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
    target_compile_features(qianxing_strategy_jsonl PRIVATE cxx_std_17)
endif()
