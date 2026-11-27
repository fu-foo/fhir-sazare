#!/bin/sh
# Run sazare-server in the foreground (`run`) as the unprivileged `sazare` user.
#
# Started as root (the default), this first hands the data directory to
# `sazare` — volumes written by images before 0.13.0, which ran as root, are
# root-owned — and then drops privileges. Started with `--user`, it runs as
# that user directly and leaves ownership alone.
set -e

if [ "$(id -u)" = "0" ]; then
    data_dir="${SAZARE_DATA_DIR:-/data}"
    mkdir -p "$data_dir"
    # Only what isn't sazare's yet (cheap on every start; a no-op after the first).
    find "$data_dir" \! -user sazare -exec chown sazare:sazare {} +
    exec setpriv --reuid=sazare --regid=sazare --init-groups sazare-server run "$@"
fi

exec sazare-server run "$@"
