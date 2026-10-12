#!/bin/sh
# MCHPRS container entrypoint.
#
# Applies optional environment variables to Config.toml (only the ones that are
# set; everything else is left to the server defaults / the existing file), then
# starts the server. Works for plain Docker (cwd /data) and for Pterodactyl
# (STARTUP is set, cwd /home/container).
#
# Variables (aliases in parentheses):
#   MCHPRS_LOG              tracing filter, default "info"
#   MCHPRS_BIND_ADDRESS     bind_address            (BIND_PORT or SERVER_PORT -> 0.0.0.0:<port>)
#   MCHPRS_MOTD             motd
#   MCHPRS_MAX_PLAYERS      max_players             (MAX_PLAYERS)
#   MCHPRS_VIEW_DISTANCE    view_distance           (VIEW_DISTANCE)
#   MCHPRS_VELOCITY_SECRET  [velocity] secret       (VELOCITY_SECRET)
#   MCHPRS_VELOCITY_ENABLED [velocity] enabled      (VELOCITY_ENABLED); defaults
#                           to true when a secret is given
set -eu

export MCHPRS_LOG="${MCHPRS_LOG:-info}"

CONFIG=Config.toml

# Pterodactyl: run in the server volume and expose the binary as ./mchprs.
if [ -n "${STARTUP:-}" ]; then
    cd /home/container
    ln -sf /usr/local/bin/mchprs ./mchprs
fi

toml_str() { # escape for a TOML basic string
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# set_top KEY TOML_VALUE : set a top-level key, creating the file if needed.
set_top() {
    [ -f "$CONFIG" ] || : > "$CONFIG"
    tmp="$CONFIG.tmp"
    {
        printf '%s = %s\n' "$1" "$2"
        K="$1" awk '
            /^[ \t]*\[/ { intable = 1 }
            !intable && $0 ~ "^[ \t]*" ENVIRON["K"] "[ \t]*=" { next }
            { print }
        ' "$CONFIG"
    } > "$tmp"
    mv "$tmp" "$CONFIG"
}

set_int() {
    case "$2" in
        ''|*[!0-9]*) echo "entrypoint: ignoring non-numeric value for $1: $2" >&2 ;;
        *) set_top "$1" "$2" ;;
    esac
}

MAX_PLAYERS_V="${MCHPRS_MAX_PLAYERS:-${MAX_PLAYERS:-}}"
VIEW_DISTANCE_V="${MCHPRS_VIEW_DISTANCE:-${VIEW_DISTANCE:-}}"
SECRET_V="${MCHPRS_VELOCITY_SECRET:-${VELOCITY_SECRET:-}}"
ENABLED_V="${MCHPRS_VELOCITY_ENABLED:-${VELOCITY_ENABLED:-}}"

if [ -n "${MCHPRS_BIND_ADDRESS:-}" ]; then
    set_top bind_address "\"$(toml_str "$MCHPRS_BIND_ADDRESS")\""
elif [ -n "${BIND_PORT:-${SERVER_PORT:-}}" ]; then
    # SERVER_PORT is injected by Pterodactyl with the allocated port.
    set_top bind_address "\"0.0.0.0:$(toml_str "${BIND_PORT:-$SERVER_PORT}")\""
fi
[ -z "${MCHPRS_MOTD:-}" ] || set_top motd "\"$(toml_str "$MCHPRS_MOTD")\""
[ -z "$MAX_PLAYERS_V" ] || set_int max_players "$MAX_PLAYERS_V"
[ -z "$VIEW_DISTANCE_V" ] || set_int view_distance "$VIEW_DISTANCE_V"

if [ -n "$SECRET_V" ] || [ -n "$ENABLED_V" ]; then
    [ -n "$ENABLED_V" ] || ENABLED_V=true
    case "$ENABLED_V" in
        true|1|yes|on) ENABLED_V=true ;;
        *) ENABLED_V=false ;;
    esac
    if [ "$ENABLED_V" = true ] && [ -z "$SECRET_V" ]; then
        echo "entrypoint: Velocity enabled but no secret given; leaving [velocity] untouched" >&2
    else
        [ -f "$CONFIG" ] || : > "$CONFIG"
        tmp="$CONFIG.tmp"
        {
            # drop any existing [velocity] table, keep everything else
            awk '
                /^[ \t]*\[velocity\][ \t]*(#.*)?$/ { skip = 1; next }
                /^[ \t]*\[/ { skip = 0 }
                !skip { print }
            ' "$CONFIG"
            printf '\n[velocity]\nenabled = %s\nsecret = "%s"\n' "$ENABLED_V" "$(toml_str "$SECRET_V")"
        } > "$tmp"
        mv "$tmp" "$CONFIG"
    fi
fi

if [ -n "${STARTUP:-}" ]; then
    # Pterodactyl passes e.g. "./mchprs" with optional {{VAR}} placeholders.
    MODIFIED_STARTUP=$(printf '%s' "$STARTUP" | sed -e 's/{{/${/g' -e 's/}}/}/g')
    echo ":/home/container$ ${MODIFIED_STARTUP}"
    eval "exec ${MODIFIED_STARTUP}"
fi

exec /usr/local/bin/mchprs "$@"
