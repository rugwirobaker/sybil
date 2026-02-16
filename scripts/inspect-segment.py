#!/usr/bin/env python3
"""Inspect a Sybil L0 segment file.

Usage:
    ./scripts/inspect-segment.py <segment.seg>              # full dump
    ./scripts/inspect-segment.py <segment.seg> --footer     # footer only
    ./scripts/inspect-segment.py <segment.seg> --dict       # dictionary only
    ./scripts/inspect-segment.py <segment.seg> --blooms     # bloom filters only
    ./scripts/inspect-segment.py <segment.seg> --entries    # decoded log entries
    ./scripts/inspect-segment.py <segment.seg> --map        # section byte map
    ./scripts/inspect-segment.py <segment.seg> --hex-footer # raw footer hex

Requires: pip install zstandard  (for entry decompression)

L0 segment layout:
    [Compressed Entries][Dictionary][Bloom Filters][Footer (40B)]
"""

import argparse
import datetime
import struct
import sys

MAGIC = 0x5359424C  # "SYBL"
FOOTER_SIZE = 40

AUTO_INDEX_FIELDS = [
    "service", "app", "source", "component", "namespace",
    "environment", "env", "host", "level", "severity", "trace_id",
]


def read_footer(data):
    if len(data) < FOOTER_SIZE:
        sys.exit(f"File too small ({len(data)} bytes) for a segment footer")

    f = data[-FOOTER_SIZE:]
    magic = struct.unpack_from("<I", f, 36)[0]
    if magic != MAGIC:
        sys.exit(f"Bad magic: 0x{magic:08x} (expected 0x{MAGIC:08x})")

    return {
        "log_count":    struct.unpack_from("<I", f, 0)[0],
        "min_ts":       struct.unpack_from("<q", f, 4)[0],
        "max_ts":       struct.unpack_from("<q", f, 12)[0],
        "entries_size": struct.unpack_from("<I", f, 20)[0],
        "dict_offset":  struct.unpack_from("<I", f, 24)[0],
        "bloom_offset": struct.unpack_from("<I", f, 28)[0],
        "checksum":     struct.unpack_from("<I", f, 32)[0],
        "magic":        magic,
    }


def ts_str(epoch_ms):
    return datetime.datetime.fromtimestamp(
        epoch_ms / 1000, tz=datetime.timezone.utc
    ).isoformat()


def print_footer(footer, file_size):
    print("=== Footer (40 bytes) ===")
    print(f"  log_count:       {footer['log_count']}")
    print(f"  min_timestamp:   {footer['min_ts']}  ({ts_str(footer['min_ts'])})")
    print(f"  max_timestamp:   {footer['max_ts']}  ({ts_str(footer['max_ts'])})")
    print(f"  entries_size:    {footer['entries_size']} bytes")
    print(f"  dict_offset:     {footer['dict_offset']}")
    print(f"  bloom_offset:    {footer['bloom_offset']}")
    print(f"  checksum:        0x{footer['checksum']:08x}")
    print(f"  magic:           0x{footer['magic']:08x} (SYBL)")
    dur_ms = footer["max_ts"] - footer["min_ts"]
    print(f"  time span:       {dur_ms}ms ({dur_ms / 1000:.1f}s)")


def print_map(footer, file_size):
    es = footer["entries_size"]
    do = footer["dict_offset"]
    bo = footer["bloom_offset"]
    fs = file_size - FOOTER_SIZE
    print("=== Section map ===")
    print(f"  [0x{0:04x} .. 0x{es:04x})  compressed entries  ({es} bytes)")
    print(f"  [0x{do:04x} .. 0x{bo:04x})  dictionary          ({bo - do} bytes)")
    print(f"  [0x{bo:04x} .. 0x{fs:04x})  bloom filters       ({fs - bo} bytes)")
    print(f"  [0x{fs:04x} .. 0x{file_size:04x})  footer              (40 bytes)")
    print(f"  total: {file_size} bytes")


def print_hex_footer(data):
    print("=== Footer hex ===")
    footer_bytes = data[-FOOTER_SIZE:]
    offset = len(data) - FOOTER_SIZE
    for i in range(0, FOOTER_SIZE, 16):
        chunk = footer_bytes[i : i + 16]
        hex_part = " ".join(f"{b:02x}" for b in chunk)
        ascii_part = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        print(f"  {offset + i:08x}: {hex_part:<48s} {ascii_part}")


def parse_dictionary(data, footer):
    dict_bytes = data[footer["dict_offset"] : footer["bloom_offset"]]
    pos = 0
    entry_count = struct.unpack_from("<I", dict_bytes, pos)[0]
    pos += 4
    entries = []
    for _ in range(entry_count):
        slen = struct.unpack_from("<H", dict_bytes, pos)[0]
        pos += 2
        entries.append(dict_bytes[pos : pos + slen].decode("utf-8"))
        pos += slen
    return entries


def print_dictionary(dictionary):
    print(f"=== Dictionary ({len(dictionary)} entries) ===")
    for i, val in enumerate(dictionary):
        print(f"  {i}: \"{val}\"")


def parse_blooms(data, footer, file_size):
    bloom_bytes = data[footer["bloom_offset"] : file_size - FOOTER_SIZE]
    pos = 0
    count = bloom_bytes[pos]
    pos += 1
    filters = []
    for _ in range(count):
        field_id = bloom_bytes[pos]
        pos += 1
        byte_len = struct.unpack_from("<H", bloom_bytes, pos)[0]
        pos += 2
        bits = bloom_bytes[pos : pos + byte_len]
        pos += byte_len
        num_bits = byte_len * 8
        set_bits = sum(bin(b).count("1") for b in bits)
        name = AUTO_INDEX_FIELDS[field_id] if field_id < len(AUTO_INDEX_FIELDS) else f"field_{field_id}"
        filters.append({
            "field_id": field_id, "name": name,
            "byte_len": byte_len, "num_bits": num_bits, "set_bits": set_bits,
        })
    return filters


def print_blooms(filters):
    print(f"=== Bloom filters ({len(filters)}) ===")
    for f in filters:
        fill = f["set_bits"] / f["num_bits"] if f["num_bits"] else 0
        print(f"  {f['name']} (id={f['field_id']}): "
              f"{f['byte_len']} bytes, {f['num_bits']} bits, "
              f"{f['set_bits']} set ({fill:.1%} fill)")


def decompress_entries(data, footer):
    try:
        import zstandard
    except ImportError:
        sys.exit("zstandard not installed. Run: uv pip install zstandard")

    compressed = data[: footer["entries_size"]]
    dctx = zstandard.ZstdDecompressor()
    return dctx.decompress(compressed, max_output_size=64 * 1024 * 1024)


def parse_entries(raw, footer, dictionary):
    entries = []
    pos = 0
    base_ts = footer["min_ts"]

    for _ in range(footer["log_count"]):
        ts_delta = struct.unpack_from("<i", raw, pos)[0]
        pos += 4
        presence = struct.unpack_from("<H", raw, pos)[0]
        pos += 2

        indexed = {}
        for bit in range(16):
            if presence & (1 << bit):
                dict_id = struct.unpack_from("<I", raw, pos)[0]
                pos += 4
                name = AUTO_INDEX_FIELDS[bit] if bit < len(AUTO_INDEX_FIELDS) else f"field_{bit}"
                indexed[name] = dictionary[dict_id] if dict_id < len(dictionary) else f"<id={dict_id}>"

        payload_len = struct.unpack_from("<I", raw, pos)[0]
        pos += 4
        payload = raw[pos : pos + payload_len].decode("utf-8")
        pos += payload_len

        entries.append({
            "timestamp": base_ts + ts_delta,
            "presence": presence,
            "indexed": indexed,
            "payload": payload,
        })
    return entries


def print_entries(entries, compressed_size, decompressed_size):
    ratio = decompressed_size / compressed_size if compressed_size else 0
    print(f"=== Entries ({len(entries)}) ===")
    print(f"  compressed: {compressed_size} -> decompressed: {decompressed_size} bytes ({ratio:.1f}x)")
    print()
    for i, e in enumerate(entries):
        print(f"  [{i}] {ts_str(e['timestamp'])}")
        if e["indexed"]:
            fields = ", ".join(f"{k}={v}" for k, v in e["indexed"].items())
            print(f"      indexed: {fields}")
        print(f"      payload: {e['payload']}")


def main():
    parser = argparse.ArgumentParser(description="Inspect a Sybil L0 segment file")
    parser.add_argument("file", help="Path to .seg file")
    parser.add_argument("--footer", action="store_true", help="Show footer only")
    parser.add_argument("--dict", action="store_true", help="Show dictionary only")
    parser.add_argument("--blooms", action="store_true", help="Show bloom filters only")
    parser.add_argument("--entries", action="store_true", help="Show decoded entries only")
    parser.add_argument("--map", action="store_true", help="Show section byte map only")
    parser.add_argument("--hex-footer", action="store_true", help="Show raw footer hex")
    args = parser.parse_args()

    data = open(args.file, "rb").read()
    file_size = len(data)
    footer = read_footer(data)

    show_all = not any([args.footer, args.dict, args.blooms, args.entries, args.map, args.hex_footer])

    if show_all or args.footer:
        print_footer(footer, file_size)
        print()

    if show_all or args.map:
        print_map(footer, file_size)
        print()

    if show_all or args.hex_footer:
        print_hex_footer(data)
        print()

    if show_all or args.dict:
        dictionary = parse_dictionary(data, footer)
        print_dictionary(dictionary)
        print()

    if show_all or args.blooms:
        filters = parse_blooms(data, footer, file_size)
        print_blooms(filters)
        print()

    if show_all or args.entries:
        dictionary = parse_dictionary(data, footer)
        raw = decompress_entries(data, footer)
        entries = parse_entries(raw, footer, dictionary)
        print_entries(entries, footer["entries_size"], len(raw))
        print()


if __name__ == "__main__":
    main()
