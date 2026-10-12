#!/usr/bin/env python3
"""Generates block_piston.bin, the piston related attributes of every block state.

Usage: gen_piston_data.py <piston.txt> <output dir>

`piston.txt` is produced by `mc_data/probe/PistonProbe.java` (see gen_generic_blocks.py for how to
run a probe against the server jar). One byte per block state:
  bits 0-1  push reaction: 0 normal, 1 destroy, 2 block, 3 push only
  bit 2     the block can't be broken (destroy speed of -1)
  bit 3     the block has a block entity
"""
import sys

REACTIONS = {"NORMAL": 0, "DESTROY": 1, "BLOCK": 2, "PUSH_ONLY": 3}


def main():
    attrs_path, out_dir = sys.argv[1:3]
    rows = [line.split() for line in open(attrs_path)]
    data = bytearray(len(rows))
    for state_id, reaction, indestructible, block_entity in rows:
        data[int(state_id)] = REACTIONS[reaction] | int(indestructible) << 2 | int(block_entity) << 3
    open(f"{out_dir}/block_piston.bin", "wb").write(data)
    print(f"{len(data)} states")


if __name__ == "__main__":
    main()
