#!/usr/bin/env python3
"""Sanity-check a Pterodactyl PTDL_v2 egg: JSON parses and required keys exist."""
import json
import sys

TOP = ["meta", "name", "description", "docker_images", "startup", "config", "scripts", "variables"]
VAR = ["name", "description", "env_variable", "default_value", "user_viewable", "user_editable", "rules"]


def main(path):
    errors = []
    with open(path, encoding="utf-8") as f:
        egg = json.load(f)

    for key in TOP:
        if key not in egg:
            errors.append(f"missing top-level key: {key}")
    if egg.get("meta", {}).get("version") != "PTDL_v2":
        errors.append("meta.version must be PTDL_v2")
    if not egg.get("docker_images"):
        errors.append("docker_images must not be empty")

    cfg = egg.get("config", {})
    for key in ("files", "startup", "logs", "stop"):
        if key not in cfg:
            errors.append(f"config.{key} missing")
    # These are JSON documents stored as strings.
    for key in ("files", "startup", "logs"):
        try:
            json.loads(cfg.get(key, "{}"))
        except ValueError as e:
            errors.append(f"config.{key} is not valid JSON: {e}")

    if "installation" not in egg.get("scripts", {}):
        errors.append("scripts.installation missing")

    seen = set()
    for i, var in enumerate(egg.get("variables", [])):
        for key in VAR:
            if key not in var:
                errors.append(f"variables[{i}] missing {key}")
        env = var.get("env_variable")
        if env in seen:
            errors.append(f"duplicate env_variable {env}")
        seen.add(env)

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"{path}: OK ({len(seen)} variables)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "pterodactyl/egg-mchprs.json"))
