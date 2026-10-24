"""Default index cache directory names, matching `cache_path` in src/cli.rs.

The CLI names a root's default cache `blake3(root path bytes)` in hex. This is a
single-chunk BLAKE3 (inputs up to 1024 bytes, which covers practical paths); a
longer path returns None so callers fail open.
"""
from __future__ import annotations

import os
from pathlib import Path

IV = (0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19)
PERMUTATION = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)
CHUNK_START, CHUNK_END, ROOT = 1, 2, 8
CHUNK_BYTES = 1024
MASK = 0xFFFFFFFF


def mix(state: list[int], a: int, b: int, c: int, d: int, x: int, y: int) -> None:
    state[a] = (state[a] + state[b] + x) & MASK
    state[d] = ((state[d] ^ state[a]) >> 16 | (state[d] ^ state[a]) << 16) & MASK
    state[c] = (state[c] + state[d]) & MASK
    state[b] = ((state[b] ^ state[c]) >> 12 | (state[b] ^ state[c]) << 20) & MASK
    state[a] = (state[a] + state[b] + y) & MASK
    state[d] = ((state[d] ^ state[a]) >> 8 | (state[d] ^ state[a]) << 24) & MASK
    state[c] = (state[c] + state[d]) & MASK
    state[b] = ((state[b] ^ state[c]) >> 7 | (state[b] ^ state[c]) << 25) & MASK


def compress(chaining: tuple[int, ...], block: bytes, length: int, flags: int) -> list[int]:
    words = [int.from_bytes(block[i:i + 4], "little") for i in range(0, 64, 4)]
    state = [*chaining, *IV[:4], 0, 0, length, flags]
    for _ in range(7):
        mix(state, 0, 4, 8, 12, words[0], words[1])
        mix(state, 1, 5, 9, 13, words[2], words[3])
        mix(state, 2, 6, 10, 14, words[4], words[5])
        mix(state, 3, 7, 11, 15, words[6], words[7])
        mix(state, 0, 5, 10, 15, words[8], words[9])
        mix(state, 1, 6, 11, 12, words[10], words[11])
        mix(state, 2, 7, 8, 13, words[12], words[13])
        mix(state, 3, 4, 9, 14, words[14], words[15])
        words = [words[index] for index in PERMUTATION]
    return [state[i] ^ state[i + 8] for i in range(8)]


def blake3_hex(data: bytes) -> str | None:
    """BLAKE3-256 of `data` in hex, or None when it exceeds one chunk."""
    if len(data) > CHUNK_BYTES:
        return None
    blocks = [data[i:i + 64] for i in range(0, len(data), 64)] or [b""]
    chaining = IV
    for index, block in enumerate(blocks):
        flags = (CHUNK_START if index == 0 else 0) | (CHUNK_END | ROOT if index == len(blocks) - 1 else 0)
        chaining = tuple(compress(chaining, block.ljust(64, b"\0"), len(block), flags))
    return b"".join(word.to_bytes(4, "little") for word in chaining).hex()


def default_cache_dir(root: Path) -> Path | None:
    """The CLI's default cache directory for canonical `root`, or None if unknown."""
    base = os.environ.get("XDG_CACHE_HOME") or (os.environ.get("HOME") and os.path.join(os.environ["HOME"], ".cache"))
    key = blake3_hex(os.fsencode(str(root)))
    return Path(base) / "trufflepig" / key if base and key else None
