#pragma once

// QXRB fixed-slot SPSC mmap ring. The file is created and sized by the Rust
// host; this header only opens it and exchanges already encoded QXSF frames.
// One process must own the writer side and one process the reader side.

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <stdexcept>
#include <string>
#include <utility>

#ifdef _WIN32
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace qianxing {

inline std::uint32_t qxrb_crc32(const std::string& bytes) {
    std::uint32_t crc = 0xffffffffu;
    for (const auto byte : bytes) {
        crc ^= static_cast<std::uint8_t>(byte);
        for (int bit = 0; bit < 8; ++bit) {
            const auto mask = 0u - (crc & 1u);
            crc = (crc >> 1u) ^ (0xedb88320u & mask);
        }
    }
    return ~crc;
}

class QxrbMapping {
public:
    QxrbMapping() = default;
    QxrbMapping(const QxrbMapping&) = delete;
    QxrbMapping& operator=(const QxrbMapping&) = delete;

    QxrbMapping(QxrbMapping&& other) noexcept { move_from(std::move(other)); }
    QxrbMapping& operator=(QxrbMapping&& other) noexcept {
        if (this != &other) {
            close();
            move_from(std::move(other));
        }
        return *this;
    }

    ~QxrbMapping() { close(); }

    static QxrbMapping open(const std::string& path, std::size_t expected_bytes) {
        QxrbMapping result;
        result.size_ = expected_bytes;
#ifdef _WIN32
        result.file_ = CreateFileA(path.c_str(), GENERIC_READ | GENERIC_WRITE,
                                   FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING,
                                   FILE_ATTRIBUTE_NORMAL, nullptr);
        if (result.file_ == INVALID_HANDLE_VALUE) {
            result.file_ = nullptr;
            throw std::runtime_error("cannot open QXRB file");
        }
        LARGE_INTEGER actual{};
        if (!GetFileSizeEx(result.file_, &actual) ||
            static_cast<std::uint64_t>(actual.QuadPart) != expected_bytes) {
            result.close();
            throw std::runtime_error("QXRB file size mismatch");
        }
        const auto high = static_cast<DWORD>(static_cast<std::uint64_t>(expected_bytes) >> 32u);
        const auto low = static_cast<DWORD>(expected_bytes & 0xffffffffu);
        result.mapping_ = CreateFileMappingA(result.file_, nullptr, PAGE_READWRITE, high, low, nullptr);
        if (result.mapping_ == nullptr) {
            result.close();
            throw std::runtime_error("cannot create QXRB file mapping");
        }
        result.data_ = static_cast<std::uint8_t*>(MapViewOfFile(result.mapping_, FILE_MAP_ALL_ACCESS, 0, 0, expected_bytes));
        if (result.data_ == nullptr) {
            result.close();
            throw std::runtime_error("cannot map QXRB file");
        }
#else
        result.fd_ = ::open(path.c_str(), O_RDWR);
        if (result.fd_ < 0) {
            throw std::runtime_error("cannot open QXRB file");
        }
        struct stat metadata{};
        if (::fstat(result.fd_, &metadata) != 0 ||
            static_cast<std::uint64_t>(metadata.st_size) != expected_bytes) {
            result.close();
            throw std::runtime_error("QXRB file size mismatch");
        }
        result.data_ = static_cast<std::uint8_t*>(
            ::mmap(nullptr, expected_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, result.fd_, 0));
        if (result.data_ == MAP_FAILED) {
            result.data_ = nullptr;
            result.close();
            throw std::runtime_error("cannot map QXRB file");
        }
#endif
        return result;
    }

    std::uint8_t* data() const { return data_; }

private:
    void close() noexcept {
#ifdef _WIN32
        if (data_ != nullptr) UnmapViewOfFile(data_);
        if (mapping_ != nullptr) CloseHandle(mapping_);
        if (file_ != nullptr) CloseHandle(file_);
        data_ = nullptr;
        mapping_ = nullptr;
        file_ = nullptr;
#else
        if (data_ != nullptr) ::munmap(data_, size_);
        if (fd_ >= 0) ::close(fd_);
        data_ = nullptr;
        fd_ = -1;
#endif
    }

    void move_from(QxrbMapping&& other) noexcept {
        data_ = other.data_;
        size_ = other.size_;
#ifdef _WIN32
        file_ = other.file_;
        mapping_ = other.mapping_;
        other.file_ = nullptr;
        other.mapping_ = nullptr;
#else
        fd_ = other.fd_;
        other.fd_ = -1;
#endif
        other.data_ = nullptr;
        other.size_ = 0;
    }

    std::uint8_t* data_ = nullptr;
    std::size_t size_ = 0;
#ifdef _WIN32
    HANDLE file_ = nullptr;
    HANDLE mapping_ = nullptr;
#else
    int fd_ = -1;
#endif
};

class QxrbRing {
public:
    static constexpr std::size_t kHeaderBytes = 64;
    static constexpr std::size_t kSlotHeaderBytes = 16;

    QxrbRing(const std::string& path, std::uint32_t capacity, std::uint32_t slot_bytes)
        : capacity_(capacity), slot_bytes_(slot_bytes) {
        if (capacity < 2 || (capacity & (capacity - 1u)) != 0 ||
            slot_bytes <= kSlotHeaderBytes || (slot_bytes % 8u) != 0 ||
            slot_bytes > 64u * 1024u * 1024u) {
            throw std::invalid_argument("invalid QXRB ring configuration");
        }
        const auto total = kHeaderBytes + static_cast<std::size_t>(capacity) * slot_bytes;
        mapping_ = QxrbMapping::open(path, total);
        if (std::memcmp(mapping_.data(), "QXRB", 4) != 0 || load_u32(4) != 1 ||
            load_u32(8) != slot_bytes || load_u32(12) != capacity) {
            throw std::runtime_error("QXRB header mismatch");
        }
    }

    bool try_push(const std::string& payload) {
        if (payload.size() > slot_bytes_ - kSlotHeaderBytes) {
            throw std::invalid_argument("QXRB payload exceeds slot capacity");
        }
        const auto write = load_u64(16);
        const auto read = load_u64(24);
        if (write - read >= capacity_) return false;
        const auto slot = slot_offset(write);
        store_u32(slot + 8, static_cast<std::uint32_t>(payload.size()));
        store_u32(slot + 12, qxrb_crc32(payload));
        std::memcpy(mapping_.data() + slot + kSlotHeaderBytes, payload.data(), payload.size());
        store_u64(slot, write + 1);
        store_u64(16, write + 1);
        return true;
    }

    bool try_pop(std::string& payload) {
        const auto read = load_u64(24);
        const auto write = load_u64(16);
        if (read == write) return false;
        const auto slot = slot_offset(read);
        const auto committed = load_u64(slot);
        if (committed != read + 1) throw std::runtime_error("QXRB commit sequence mismatch");
        const auto length = load_u32(slot + 8);
        if (length > slot_bytes_ - kSlotHeaderBytes) throw std::runtime_error("QXRB slot length exceeds capacity");
        payload.assign(reinterpret_cast<const char*>(mapping_.data() + slot + kSlotHeaderBytes), length);
        if (qxrb_crc32(payload) != load_u32(slot + 12)) throw std::runtime_error("QXRB payload CRC mismatch");
        store_u64(24, read + 1);
        return true;
    }

private:
    std::size_t slot_offset(std::uint64_t sequence) const {
        return kHeaderBytes + (static_cast<std::size_t>(sequence) & (capacity_ - 1u)) * slot_bytes_;
    }

    std::uint32_t load_u32(std::size_t offset) const {
        std::uint32_t value = 0;
        std::memcpy(&value, mapping_.data() + offset, sizeof(value));
        return value;
    }

    std::uint64_t load_u64(std::size_t offset) const {
#ifdef _MSC_VER
        return static_cast<std::uint64_t>(InterlockedCompareExchange64(
            reinterpret_cast<volatile LONG64*>(mapping_.data() + offset), 0, 0));
#else
        return __atomic_load_n(reinterpret_cast<const std::uint64_t*>(mapping_.data() + offset),
                                __ATOMIC_ACQUIRE);
#endif
    }

    void store_u32(std::size_t offset, std::uint32_t value) {
        std::memcpy(mapping_.data() + offset, &value, sizeof(value));
    }

    void store_u64(std::size_t offset, std::uint64_t value) {
#ifdef _MSC_VER
        InterlockedExchange64(reinterpret_cast<volatile LONG64*>(mapping_.data() + offset),
                              static_cast<LONG64>(value));
#else
        __atomic_store_n(reinterpret_cast<std::uint64_t*>(mapping_.data() + offset), value,
                          __ATOMIC_RELEASE);
#endif
    }

    std::uint32_t capacity_;
    std::uint32_t slot_bytes_;
    QxrbMapping mapping_;
};

} // namespace qianxing
