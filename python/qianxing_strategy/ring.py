"""SPSC mmap ring compatible with qx-strategy's QXRB layout.

The ring carries already encoded QXSF frames.  It is intentionally limited to
one producer and one consumer; process supervision and strategy semantics stay
outside this transport layer.
"""

from __future__ import annotations

import mmap
import struct
import time
import zlib
from pathlib import Path

MAGIC = b"QXRB"
VERSION = 1
HEADER_BYTES = 64
SLOT_HEADER_BYTES = 16
WRITE_SEQ_OFFSET = 16
READ_SEQ_OFFSET = 24
SLOT_COMMIT_OFFSET = 0
SLOT_LEN_OFFSET = 8
SLOT_CRC_OFFSET = 12


class RingEmpty(Exception):
    pass


class RingFull(Exception):
    pass


def validate_config(capacity: int, slot_bytes: int) -> None:
    if capacity < 2 or capacity & (capacity - 1):
        raise ValueError("ring capacity must be a power of two >= 2")
    if slot_bytes <= SLOT_HEADER_BYTES or slot_bytes % 8 or slot_bytes > 64 * 1024 * 1024:
        raise ValueError("ring slot_bytes must be an 8-byte multiple in (16, 67108864]")


class SharedMemoryRing:
    def __init__(self, path: str | Path, capacity: int, slot_bytes: int):
        validate_config(capacity, slot_bytes)
        self.path = Path(path)
        self.capacity = capacity
        self.slot_bytes = slot_bytes
        self.total_bytes = HEADER_BYTES + capacity * slot_bytes
        self._file = self.path.open("r+b")
        if self._file.seek(0, 2) != self.total_bytes:
            self._file.close()
            raise ValueError("ring file size does not match configuration")
        self._mmap = mmap.mmap(self._file.fileno(), self.total_bytes, access=mmap.ACCESS_WRITE)
        if self._mmap[:4] != MAGIC:
            self.close()
            raise ValueError("ring magic mismatch")
        if self._u32(4) != VERSION or self._u32(8) != slot_bytes or self._u32(12) != capacity:
            self.close()
            raise ValueError("ring header mismatch")

    @classmethod
    def create(cls, path: str | Path, capacity: int, slot_bytes: int) -> "SharedMemoryRing":
        validate_config(capacity, slot_bytes)
        target = Path(path)
        with target.open("w+b") as file:
            file.truncate(HEADER_BYTES + capacity * slot_bytes)
            file.write(MAGIC)
            file.write(struct.pack("<III", VERSION, slot_bytes, capacity))
            file.write(b"\x00" * (HEADER_BYTES - 16))
        return cls(target, capacity, slot_bytes)

    def close(self) -> None:
        if getattr(self, "_mmap", None) is not None:
            self._mmap.flush()
            self._mmap.close()
            self._mmap = None
        if getattr(self, "_file", None) is not None:
            self._file.close()
            self._file = None

    def __enter__(self) -> "SharedMemoryRing":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def _u64(self, offset: int) -> int:
        return struct.unpack_from("<Q", self._mmap, offset)[0]

    def _u32(self, offset: int) -> int:
        return struct.unpack_from("<I", self._mmap, offset)[0]

    def _set_u64(self, offset: int, value: int) -> None:
        struct.pack_into("<Q", self._mmap, offset, value)

    def _slot(self, sequence: int) -> int:
        return HEADER_BYTES + (sequence & (self.capacity - 1)) * self.slot_bytes

    def try_push(self, payload: bytes) -> None:
        if len(payload) > self.slot_bytes - SLOT_HEADER_BYTES:
            raise ValueError("ring payload exceeds slot capacity")
        write = self._u64(WRITE_SEQ_OFFSET)
        read = self._u64(READ_SEQ_OFFSET)
        if write - read >= self.capacity:
            raise RingFull()
        slot = self._slot(write)
        struct.pack_into("<II", self._mmap, slot + SLOT_LEN_OFFSET, len(payload), zlib.crc32(payload) & 0xFFFFFFFF)
        self._mmap[slot + SLOT_HEADER_BYTES : slot + SLOT_HEADER_BYTES + len(payload)] = payload
        # Commit is published only after length, CRC and payload are visible.
        self._set_u64(slot + SLOT_COMMIT_OFFSET, write + 1)
        self._set_u64(WRITE_SEQ_OFFSET, write + 1)

    def try_pop(self) -> bytes:
        read = self._u64(READ_SEQ_OFFSET)
        write = self._u64(WRITE_SEQ_OFFSET)
        if read == write:
            raise RingEmpty()
        slot = self._slot(read)
        committed = self._u64(slot + SLOT_COMMIT_OFFSET)
        if committed != read + 1:
            raise ValueError(f"ring commit sequence mismatch: expected={read + 1} actual={committed}")
        length = self._u32(slot + SLOT_LEN_OFFSET)
        if length > self.slot_bytes - SLOT_HEADER_BYTES:
            raise ValueError("ring slot length exceeds capacity")
        expected_crc = self._u32(slot + SLOT_CRC_OFFSET)
        payload = bytes(self._mmap[slot + SLOT_HEADER_BYTES : slot + SLOT_HEADER_BYTES + length])
        if zlib.crc32(payload) & 0xFFFFFFFF != expected_crc:
            raise ValueError("ring payload CRC32 mismatch")
        self._set_u64(READ_SEQ_OFFSET, read + 1)
        return payload

    def push_wait(self, payload: bytes, deadline: float) -> None:
        while True:
            try:
                self.try_push(payload)
                return
            except RingFull:
                if time.monotonic() >= deadline:
                    raise TimeoutError("ring push timed out")
                time.sleep(0.001)
