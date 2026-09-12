/*
 * Optional external-process example.
 *
 * Build with a C++17 compiler and nlohmann/json, then configure the binary as
 * strategy.external_executable. The default protocol is one JSON request and
 * one JSON response per line. Passing --protocol framed_json enables the same
 * bounded QXSF frame used by the Rust/Python workers. Passing
 * --protocol shared_memory_json enables the QXRB SPSC mmap rings created by
 * the Rust host; stdout is unused in that mode.
 */
#include <array>
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <iostream>
#include <nlohmann/json.hpp>
#include <stdexcept>
#include <string>
#include <thread>

#include "qianxing_ring.hpp"

using json = nlohmann::json;

namespace {
constexpr std::size_t kHeaderSize = 24;
constexpr std::uint32_t kMaxFrameBytes = 16u * 1024u * 1024u;
constexpr std::uint16_t kVersion = 1;
constexpr std::uint8_t kRequest = 1;
constexpr std::uint8_t kResponse = 2;

std::uint32_t crc32(const std::string& payload) {
    std::uint32_t crc = 0xffffffffu;
    for (const auto byte : payload) {
        crc ^= static_cast<std::uint8_t>(byte);
        for (int bit = 0; bit < 8; ++bit) {
            const auto mask = 0u - (crc & 1u);
            crc = (crc >> 1u) ^ (0xedb88320u & mask);
        }
    }
    return ~crc;
}

void put_u16(std::array<char, kHeaderSize>& header, std::size_t offset,
             std::uint16_t value) {
    header[offset] = static_cast<char>(value & 0xffu);
    header[offset + 1] = static_cast<char>((value >> 8u) & 0xffu);
}

void put_u32(std::array<char, kHeaderSize>& header, std::size_t offset,
             std::uint32_t value) {
    for (int index = 0; index < 4; ++index) {
        header[offset + index] = static_cast<char>((value >> (8u * index)) & 0xffu);
    }
}

void put_u64(std::array<char, kHeaderSize>& header, std::size_t offset,
             std::uint64_t value) {
    for (int index = 0; index < 8; ++index) {
        header[offset + index] = static_cast<char>((value >> (8u * index)) & 0xffu);
    }
}

std::uint16_t get_u16(const std::array<char, kHeaderSize>& header, std::size_t offset) {
    return static_cast<std::uint16_t>(static_cast<std::uint8_t>(header[offset])) |
           (static_cast<std::uint16_t>(static_cast<std::uint8_t>(header[offset + 1])) << 8u);
}

std::uint32_t get_u32(const std::array<char, kHeaderSize>& header, std::size_t offset) {
    std::uint32_t value = 0;
    for (int index = 0; index < 4; ++index) {
        value |= static_cast<std::uint32_t>(static_cast<std::uint8_t>(header[offset + index]))
                 << (8u * index);
    }
    return value;
}

std::uint64_t get_u64(const std::array<char, kHeaderSize>& header, std::size_t offset) {
    std::uint64_t value = 0;
    for (int index = 0; index < 8; ++index) {
        value |= static_cast<std::uint64_t>(static_cast<std::uint8_t>(header[offset + index]))
                 << (8u * index);
    }
    return value;
}

void decode_frame(const std::string& encoded, std::uint64_t& sequence, std::string& payload,
                  std::uint8_t expected_kind) {
    if (encoded.size() < kHeaderSize) throw std::runtime_error("truncated strategy frame header");
    std::array<char, kHeaderSize> header{};
    std::copy_n(encoded.data(), kHeaderSize, header.data());
    if (std::string(header.data(), 4) != "QXSF" || get_u16(header, 4) != kVersion ||
        header[7] != 0 || static_cast<std::uint8_t>(header[6]) != expected_kind) {
        throw std::runtime_error("invalid strategy frame header");
    }
    sequence = get_u64(header, 8);
    const auto payload_size = get_u32(header, 16);
    if (payload_size > kMaxFrameBytes - kHeaderSize || encoded.size() != kHeaderSize + payload_size) {
        throw std::runtime_error("strategy frame exceeds size or is truncated");
    }
    payload.assign(encoded.data() + kHeaderSize, payload_size);
    if (crc32(payload) != get_u32(header, 20)) {
        throw std::runtime_error("invalid strategy frame payload CRC");
    }
}

std::string encode_frame(std::uint64_t sequence, const std::string& payload) {
    if (payload.size() > kMaxFrameBytes - kHeaderSize) {
        throw std::runtime_error("strategy response frame exceeds size limit");
    }
    std::array<char, kHeaderSize> header{};
    header[0] = 'Q';
    header[1] = 'X';
    header[2] = 'S';
    header[3] = 'F';
    put_u16(header, 4, kVersion);
    header[6] = static_cast<char>(kResponse);
    put_u64(header, 8, sequence);
    put_u32(header, 16, static_cast<std::uint32_t>(payload.size()));
    put_u32(header, 20, crc32(payload));
    return std::string(header.data(), header.size()) + payload;
}

std::uint16_t columnar_u16(const std::string& bytes, std::size_t offset) {
    if (offset + 2 > bytes.size()) throw std::runtime_error("truncated columnar u16");
    return static_cast<std::uint16_t>(static_cast<std::uint8_t>(bytes[offset])) |
           (static_cast<std::uint16_t>(static_cast<std::uint8_t>(bytes[offset + 1])) << 8u);
}

std::uint32_t columnar_u32(const std::string& bytes, std::size_t offset) {
    if (offset + 4 > bytes.size()) throw std::runtime_error("truncated columnar u32");
    std::uint32_t value = 0;
    for (int index = 0; index < 4; ++index) {
        value |= static_cast<std::uint32_t>(static_cast<std::uint8_t>(bytes[offset + index]))
                 << (8u * index);
    }
    return value;
}

std::uint64_t columnar_u64(const std::string& bytes, std::size_t offset) {
    if (offset + 8 > bytes.size()) throw std::runtime_error("truncated columnar u64");
    std::uint64_t value = 0;
    for (int index = 0; index < 8; ++index) {
        value |= static_cast<std::uint64_t>(static_cast<std::uint8_t>(bytes[offset + index]))
                 << (8u * index);
    }
    return value;
}

std::int64_t columnar_i128_as_i64(const std::string& bytes, std::size_t offset) {
    if (offset + 16 > bytes.size()) throw std::runtime_error("truncated columnar i128");
    const auto sign_extension = static_cast<std::uint8_t>(bytes[offset + 15]) & 0x80u
                                    ? 0xffu
                                    : 0u;
    if ((sign_extension == 0u && (static_cast<std::uint8_t>(bytes[offset + 7]) & 0x80u) != 0u) ||
        (sign_extension == 0xffu && (static_cast<std::uint8_t>(bytes[offset + 7]) & 0x80u) == 0u)) {
        throw std::runtime_error("columnar i128 exceeds C++ int64 strategy boundary");
    }
    for (std::size_t index = 8; index < 16; ++index) {
        if (static_cast<std::uint8_t>(bytes[offset + index]) != sign_extension) {
            throw std::runtime_error("columnar i128 exceeds C++ int64 strategy boundary");
        }
    }
    std::uint64_t raw = 0;
    for (int index = 0; index < 8; ++index) {
        raw |= static_cast<std::uint64_t>(static_cast<std::uint8_t>(bytes[offset + index]))
               << (8u * index);
    }
    std::int64_t value = 0;
    std::memcpy(&value, &raw, sizeof(value));
    return value;
}

std::string decode_columnar_request(const std::string& encoded) {
    constexpr std::size_t kColumnarHeaderSize = 16;
    if (encoded.size() < kColumnarHeaderSize || encoded.compare(0, 4, "QXCB") != 0 ||
        columnar_u16(encoded, 4) != 1 || columnar_u16(encoded, 6) != 0) {
        throw std::runtime_error("invalid QXCB columnar header");
    }
    const auto metadata_size = columnar_u32(encoded, 8);
    const auto rows = columnar_u32(encoded, 12);
    if (rows == 0) throw std::runtime_error("QXCB payload must contain bars");
    const auto metadata_start = kColumnarHeaderSize;
    const auto metadata_end = metadata_start + metadata_size;
    const auto expected = metadata_end + static_cast<std::size_t>(rows) * (8 + 5 * 16);
    if (metadata_end > encoded.size() || expected != encoded.size()) {
        throw std::runtime_error("QXCB column lengths do not match payload");
    }
    auto value = json::parse(encoded.substr(metadata_start, metadata_size));
    if (!value["bars"].is_null()) throw std::runtime_error("QXCB metadata unexpectedly contains bars");
    const auto source = value.value("__qx_bars_source", std::string("shared-columnar-v1"));
    value.erase("__qx_bars_source");
    json bars = {
        {"source", source},
        {"ts", json::array()},
        {"open_raw", json::array()},
        {"high_raw", json::array()},
        {"low_raw", json::array()},
        {"close_raw", json::array()},
        {"volume_raw", json::array()},
    };
    std::size_t offset = metadata_end;
    for (std::uint32_t index = 0; index < rows; ++index) {
        bars["ts"].push_back(columnar_u64(encoded, offset));
        offset += 8;
    }
    const char* columns[] = {"open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"};
    for (const auto* column : columns) {
        for (std::uint32_t index = 0; index < rows; ++index) {
            bars[column].push_back(columnar_i128_as_i64(encoded, offset));
            offset += 16;
        }
    }
    value["bars"] = bars;
    return value.dump();
}

bool read_frame(std::uint64_t& sequence, std::string& payload) {
    std::array<char, kHeaderSize> header{};
    std::cin.read(header.data(), static_cast<std::streamsize>(header.size()));
    if (std::cin.gcount() == 0 && std::cin.eof()) {
        return false;
    }
    if (std::cin.gcount() != static_cast<std::streamsize>(header.size())) {
        throw std::runtime_error("truncated strategy frame header");
    }
    if (std::string(header.data(), 4) != "QXSF" || get_u16(header, 4) != kVersion ||
        header[7] != 0 || static_cast<std::uint8_t>(header[6]) != kRequest) {
        throw std::runtime_error("invalid strategy request frame header");
    }
    sequence = get_u64(header, 8);
    const auto payload_size = get_u32(header, 16);
    if (payload_size > kMaxFrameBytes - kHeaderSize) {
        throw std::runtime_error("strategy request frame exceeds size limit");
    }
    const auto expected_crc = get_u32(header, 20);
    payload.assign(payload_size, '\0');
    std::cin.read(payload.data(), static_cast<std::streamsize>(payload.size()));
    if (std::cin.gcount() != static_cast<std::streamsize>(payload.size()) ||
        crc32(payload) != expected_crc) {
        throw std::runtime_error("invalid strategy request frame payload");
    }
    return true;
}

void write_frame(std::uint64_t sequence, const std::string& payload) {
    const auto encoded = encode_frame(sequence, payload);
    std::cout.write(encoded.data(), static_cast<std::streamsize>(encoded.size()));
    std::cout.flush();
}

std::string handle(const std::string& line) {
    try {
        const auto input = json::parse(line);
        const auto instrument = input.at("instrument").get<std::string>();
        const auto targets = input.value("research_targets", json::object());
        const auto target = targets.value(instrument, json(0));
        json output = {
            {"schema_version", input.at("schema_version")},
            {"request_id", input.at("request_id")},
            {"strategy_id", input.at("strategy_id")},
            {"signal_id", input.at("as_of")},
            {"instrument", instrument},
            {"target_qty", target},
            {"confidence", 0},
            {"priority", 0},
            {"expires_at", input.at("as_of")},
            {"intents", json::array()},
        };
        return json{{"ok", true}, {"output", output}}.dump();
    } catch (const std::exception& error) {
        return json{{"ok", false}, {"error", error.what()}}.dump();
    }
}
} // namespace

int main(int argc, char** argv) {
    std::ios::sync_with_stdio(false);
    std::cin.tie(nullptr);

    std::string protocol = "jsonl";
    std::string input_ring;
    std::string output_ring;
    std::uint32_t ring_capacity = 1024;
    std::uint32_t ring_slot_bytes = 64u * 1024u;
    for (int index = 1; index + 1 < argc; index += 2) {
        const std::string key = argv[index];
        const std::string value = argv[index + 1];
        if (key == "--protocol") protocol = value;
        else if (key == "--input-ring") input_ring = value;
        else if (key == "--output-ring") output_ring = value;
        else if (key == "--ring-capacity") ring_capacity = static_cast<std::uint32_t>(std::stoul(value));
        else if (key == "--ring-slot-bytes") ring_slot_bytes = static_cast<std::uint32_t>(std::stoul(value));
    }

    const bool framed = protocol == "framed_json";
    const bool shared = protocol == "shared_memory_json" || protocol == "shared_memory_columnar";
    const bool columnar = protocol == "shared_memory_columnar";
    if (framed) {
        try {
            std::uint64_t sequence = 0;
            std::string payload;
            while (read_frame(sequence, payload)) {
                write_frame(sequence, handle(payload));
            }
            return 0;
        } catch (const std::exception& error) {
            std::cerr << "strategy framed worker failed: " << error.what() << '\n';
            return 2;
        }
    }

    if (shared) {
        if (input_ring.empty() || output_ring.empty()) {
            std::cerr << "shared_memory_json requires input/output ring paths\n";
            return 2;
        }
        try {
            qianxing::QxrbRing input(input_ring, ring_capacity, ring_slot_bytes);
            qianxing::QxrbRing output(output_ring, ring_capacity, ring_slot_bytes);
            for (;;) {
                std::string encoded;
                if (!input.try_pop(encoded)) {
                    std::this_thread::sleep_for(std::chrono::milliseconds(1));
                    continue;
                }
                std::uint64_t sequence = 0;
                std::string payload;
                std::string frame_payload;
                decode_frame(encoded, sequence, frame_payload, kRequest);
                payload = columnar ? decode_columnar_request(frame_payload) : frame_payload;
                const auto response = encode_frame(sequence, handle(payload));
                const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(30);
                while (!output.try_push(response)) {
                    if (std::chrono::steady_clock::now() >= deadline) {
                        throw std::runtime_error("QXRB response ring remained full");
                    }
                    std::this_thread::sleep_for(std::chrono::milliseconds(1));
                }
            }
        } catch (const std::exception& error) {
            std::cerr << "strategy shared-memory worker failed: " << error.what() << '\n';
            return 2;
        }
    }

    std::string line;
    while (std::getline(std::cin, line)) {
        std::cout << handle(line) << '\n';
        std::cout.flush();
    }
    return 0;
}
