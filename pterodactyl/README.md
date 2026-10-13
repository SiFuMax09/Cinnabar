# Cinnabar Pterodactyl egg

`egg-cinnabar.json` is a PTDL_v2 egg for Pterodactyl panel 1.x. It runs the prebuilt Cinnabar container image (Minecraft 26.2); no install script work is needed.

## Import

1. Admin area -> Nests -> Import Egg -> select `egg-cinnabar.json` (or pick/create a nest first).
2. Create a server with that egg and allocate a port. Players need a **Minecraft 26.2** client.

### Image placeholder

The egg points at `ghcr.io/sifumax09/cinnabar:latest` (and `:26.2`), derived from this repository's git remote. If you use another fork, replace it with `ghcr.io/<owner>/<repo>:latest` (lowercase) in the egg's "Docker Images" section. The image is published by `.github/workflows/docker.yml`; make the GHCR package public or configure registry credentials in Wings.

## How it works

- Startup command: `./cinnabar`. The image entrypoint links the binary into `/home/container`, applies the variables below to `Config.toml`, then starts the server.
- "Done" detection: `Done! Start took`.
- Stop: `^C` (SIGINT). Cinnabar has no console command input, only a Ctrl+C handler which saves and shuts down.
- The listen port is the allocated `SERVER_PORT` (`bind_address = 0.0.0.0:<port>`).
- `Config.toml` is created by the server on first start; variables are applied by the entrypoint on every start (it also has `find`/`replace` entries in the egg for `bind_address`, `motd`, `max_players`, `view_distance`). Editing these values in `Config.toml` by hand is overwritten on the next start; use the panel variables instead. Other options (`whitelist`, `luckperms`, ...) can be edited in `Config.toml` freely.

## Variables

| Variable | Default | Config.toml key | Notes |
| --- | --- | --- | --- |
| `CINNABAR_LOG` | `info` | n/a | Tracing filter, e.g. `debug` |
| `CINNABAR_MOTD` | `Minecraft High Performance Redstone Server` | `motd` | |
| `MAX_PLAYERS` | `100` | `max_players` | integer 1-99999 |
| `VIEW_DISTANCE` | `8` | `view_distance` | integer 2-32 |
| `VELOCITY_ENABLED` | `false` | `[velocity] enabled` | `true`/`false` |
| `VELOCITY_SECRET` | empty | `[velocity] secret` | contents of the proxy's forwarding secret, not a path; required when enabled |
| `SERVER_PORT` | provided by panel | `bind_address` | not a variable you set |

## Validation

```shell
python3 pterodactyl/validate_egg.py pterodactyl/egg-cinnabar.json
```

This checks that the JSON parses, the required top-level keys exist, and the embedded config JSON strings are valid. It also runs in CI.
