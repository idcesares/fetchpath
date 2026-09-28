"""Delta size between related versions with zstd (FP-023).

For each OLD NEW pair: NEW compressed alone, and NEW compressed with OLD as a
raw dictionary (what `zstd --patch-from` does), with the time each takes.
Python 3.14's `compression.zstd`; prints one JSON line per pair.

    python tools/bench/delta-compare.py OLD NEW [OLD NEW ...]
"""

import json
import sys
import time
from compression import zstd

LEVEL = 19


def compress(data, dictionary=None):
    started = time.perf_counter()
    options = {
        zstd.CompressionParameter.compression_level: LEVEL,
        zstd.CompressionParameter.window_log: 27,
        zstd.CompressionParameter.enable_long_distance_matching: 1,
    }
    out = zstd.compress(data, options=options, zstd_dict=dictionary)
    return len(out), time.perf_counter() - started, out


def main(paths):
    for old_path, new_path in zip(paths[::2], paths[1::2]):
        old = open(old_path, "rb").read()
        new = open(new_path, "rb").read()
        alone, alone_seconds, _ = compress(new)
        dictionary = zstd.ZstdDict(old, is_raw=True)
        delta, delta_seconds, patch = compress(new, dictionary)
        started = time.perf_counter()
        restored = zstd.decompress(patch, zstd_dict=dictionary, options={zstd.DecompressionParameter.window_log_max: 31})
        apply_seconds = time.perf_counter() - started
        assert restored == new, "the delta does not rebuild NEW"
        print(json.dumps({
            "new": new_path, "newBytes": len(new), "compressedAlone": alone,
            "delta": delta, "deltaShareOfNew": round(delta / len(new), 4),
            "compressSeconds": round(alone_seconds, 2), "deltaSeconds": round(delta_seconds, 2),
            "applySeconds": round(apply_seconds, 2),
        }))


if __name__ == "__main__":
    main(sys.argv[1:])
