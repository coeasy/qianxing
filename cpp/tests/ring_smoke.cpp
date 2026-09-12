#include "qianxing_ring.hpp"

#include <cassert>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <string>
#include <vector>

namespace {
void put_u32(std::vector<std::uint8_t>& bytes, std::size_t offset, std::uint32_t value) {
    for (int index = 0; index < 4; ++index) {
        bytes[offset + index] = static_cast<std::uint8_t>(value >> (8u * index));
    }
}
}

int main() {
    const std::string path = "qianxing-ring-smoke.bin";
    constexpr std::uint32_t capacity = 2;
    constexpr std::uint32_t slot_bytes = 128;
    const auto total = qianxing::QxrbRing::kHeaderBytes + capacity * slot_bytes;
    std::vector<std::uint8_t> bytes(total, 0);
    bytes[0] = 'Q';
    bytes[1] = 'X';
    bytes[2] = 'R';
    bytes[3] = 'B';
    put_u32(bytes, 4, 1);
    put_u32(bytes, 8, slot_bytes);
    put_u32(bytes, 12, capacity);
    {
        std::ofstream file(path, std::ios::binary | std::ios::trunc);
        file.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    }

    qianxing::QxrbRing writer(path, capacity, slot_bytes);
    qianxing::QxrbRing reader(path, capacity, slot_bytes);
    std::string payload;
    assert(!reader.try_pop(payload));
    assert(writer.try_push("one"));
    assert(writer.try_push("two"));
    assert(!writer.try_push("three"));
    assert(reader.try_pop(payload) && payload == "one");
    assert(writer.try_push("three"));
    assert(reader.try_pop(payload) && payload == "two");
    assert(reader.try_pop(payload) && payload == "three");
    assert(!reader.try_pop(payload));
    std::remove(path.c_str());
    return 0;
}
