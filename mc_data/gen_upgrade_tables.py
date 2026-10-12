#!/usr/bin/env python3
"""Generates the tables that upgrade block state ids and item ids saved by the Minecraft 1.20.4
version of the server to the current ones.

Usage: gen_upgrade_tables.py <old blocks.json> <old registries.json> <new blocks.json> <new registries.json> <output dir>

Output: `upgrade_blocks_1_20_4.bin` and `upgrade_items_1_20_4.bin`, arrays of little endian u16
indexed by the old id. An item that no longer exists maps to 0xFFFF.
"""
import json
import struct
import sys

BLOCK_RENAMES = {"minecraft:chain": "minecraft:iron_chain"}
ITEM_RENAMES = {"minecraft:chain": "minecraft:iron_chain", "minecraft:scute": "minecraft:turtle_scute"}


def main():
    old_blocks, old_reg, new_blocks, new_reg, out = sys.argv[1:6]
    ob, nb = json.load(open(old_blocks)), json.load(open(new_blocks))
    orr, nr = json.load(open(old_reg)), json.load(open(new_reg))

    states = {}
    defaults = {}
    for name, block in nb.items():
        defaults[name] = next(s for s in block["states"] if s.get("default"))
        for s in block["states"]:
            states[(name, tuple(sorted((s.get("properties") or {}).items())))] = s["id"]

    table = [0] * (max(s["id"] for b in ob.values() for s in b["states"]) + 1)
    for old_name, block in ob.items():
        name = BLOCK_RENAMES.get(old_name, old_name)
        for s in block["states"]:
            props = s.get("properties") or {}
            exact = states.get((name, tuple(sorted(props.items()))))
            if exact is not None:
                table[s["id"]] = exact
            elif name in defaults:
                # Properties were added or changed: keep the ones that still exist
                new_props = nb[name].get("properties") or {}
                merged = dict(defaults[name].get("properties") or {})
                merged.update({k: v for k, v in props.items() if k in new_props and v in new_props[k]})
                table[s["id"]] = states.get((name, tuple(sorted(merged.items()))), defaults[name]["id"])
    open(f"{out}/upgrade_blocks_1_20_4.bin", "wb").write(b"".join(struct.pack("<H", x) for x in table))

    new_items = nr["minecraft:item"]["entries"]
    old_items = sorted(orr["minecraft:item"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])
    items = []
    for name, _ in old_items:
        name = ITEM_RENAMES.get(name, name)
        items.append(new_items[name]["protocol_id"] if name in new_items else 0xFFFF)
    open(f"{out}/upgrade_items_1_20_4.bin", "wb").write(b"".join(struct.pack("<H", x) for x in items))


if __name__ == "__main__":
    main()
