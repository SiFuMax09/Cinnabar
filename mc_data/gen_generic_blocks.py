#!/usr/bin/env python3
"""Generates the compact data files used for blocks and items that have no dedicated enum variant.

Usage: gen_generic_blocks.py <blocks.json> <registries.json> <attrs.txt> <output dir>

`attrs.txt` is produced by `mc_data/probe/Probe.java`, which queries the Minecraft server for the
properties of every block state:
    javac -cp "<server.jar>:<libraries>" -d out probe/Probe.java
    java -cp "out:<server.jar>:<libraries>" Probe attrs.txt

Outputs:
  generic_blocks.txt  one line per block: `name first_id default_id prop=v1,v2;prop=v1,v2`
  block_attrs.bin     two bytes per block state: flags (1 = redstone conductor, 2 = sturdy top face,
                      4 = full collision cube) and the note block instrument index
  block_collision.bin six bytes per block state: the bounding box of the collision shape as
                      min x, y, z and max x, y, z in sixteenths of a block plus 16, or all 0xFF without
                      collision
  block_resistance.bin the explosion resistance of every block state in tenths, as little endian
                      u16 clamped to 65535
  block_light.bin     two bytes per block state: the light emission in the low nibble and the light
                      dampening in the high nibble of the first byte, and a mask of the faces (down, up,
                      north, south, west, east) whose occlusion shape covers the whole face
  item_names.txt      the names of all items in protocol id order
"""
import json
import sys
from itertools import product

# Collision bounds can be slightly negative, so they are stored with this offset
COLLISION_OFFSET = 16

# Must match the `Instrument` enum in crates/blocks/src/blocks/props.rs
INSTRUMENTS = [
    "harp", "basedrum", "snare", "hat", "bass", "flute", "bell", "guitar", "chime", "xylophone",
    "iron_xylophone", "cow_bell", "didgeridoo", "bit", "banjo", "pling", "trumpet",
    "trumpet_exposed", "trumpet_oxidized", "trumpet_weathered", "zombie", "skeleton", "creeper",
    "dragon", "wither_skeleton", "piglin", "custom_head",
]


def main():
    blocks_path, registries_path, attrs_path, out_dir = sys.argv[1:5]
    blocks = json.load(open(blocks_path))
    registries = json.load(open(registries_path))

    lines = []
    num_states = 0
    for name, block in sorted(blocks.items(), key=lambda kv: min(s["id"] for s in kv[1]["states"])):
        states = block["states"]
        first = min(s["id"] for s in states)
        default = next(s["id"] for s in states if s.get("default"))
        by_id = {st["id"]: (st.get("properties") or {}) for st in states}
        names = list((block.get("properties") or {}).keys())
        # The order of the properties and their values is only implied by the state ids, which
        # enumerate them like an odometer: derive the stride of every property from the ids.
        strides = {}
        for prop in names:
            strides[prop] = next(i - first for i in sorted(by_id) if by_id[i][prop] != by_id[first][prop])
        order = sorted(names, key=lambda prop: -strides[prop])
        props = {}
        for prop in order:
            props[prop] = [by_id[first + k * strides[prop]][prop] for k in range(len(set(st[prop] for st in by_id.values())))]
        expected = {}
        for offset, combo in enumerate(product(*props.values())):
            expected[first + offset] = dict(zip(props.keys(), combo))
        assert len(expected) == len(states), name
        for state_id, state_props in by_id.items():
            assert expected[state_id] == state_props, name
        num_states += len(states)
        spec = ";".join(f"{k}={','.join(v)}" for k, v in props.items())
        lines.append(f"{name} {first} {default} {spec}")
    open(f"{out_dir}/generic_blocks.txt", "w").write("\n".join(lines) + "\n")

    attrs = bytearray(num_states * 2)
    collision = bytearray(b"\xff" * (num_states * 6))
    resistances = bytearray(num_states * 2)
    light = bytearray(num_states * 2)
    seen = 0
    for line in open(attrs_path):
        state_id, solid, sturdy, full, instrument, bounds, resistance, emission, dampening, faces = line.split()
        res = int(round(float(resistance) * 10)) if float(resistance) < 6000 else 65535
        resistances[int(state_id) * 2:int(state_id) * 2 + 2] = res.to_bytes(2, "little")
        if bounds != "none":
            collision[int(state_id) * 6:int(state_id) * 6 + 6] = bytes(int(v) + COLLISION_OFFSET for v in bounds.split(","))
        state_id = int(state_id)
        light[state_id * 2] = int(emission) | int(dampening) << 4
        light[state_id * 2 + 1] = int(faces)
        attrs[state_id * 2] = int(solid) | int(sturdy) << 1 | int(full) << 2
        attrs[state_id * 2 + 1] = INSTRUMENTS.index(instrument)
        seen += 1
    assert seen == num_states, (seen, num_states)
    open(f"{out_dir}/block_attrs.bin", "wb").write(attrs)
    open(f"{out_dir}/block_collision.bin", "wb").write(collision)
    open(f"{out_dir}/block_resistance.bin", "wb").write(resistances)
    open(f"{out_dir}/block_light.bin", "wb").write(light)

    items = sorted(registries["minecraft:item"]["entries"].items(), key=lambda kv: kv[1]["protocol_id"])
    assert [e["protocol_id"] for _, e in items] == list(range(len(items)))
    open(f"{out_dir}/item_names.txt", "w").write("\n".join(k for k, _ in items) + "\n")
    print(f"{len(lines)} blocks, {num_states} states, {len(items)} items")


if __name__ == "__main__":
    main()
