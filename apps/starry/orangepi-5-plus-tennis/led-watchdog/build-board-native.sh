#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cc=${CC:-cc}

"$cc" -O2 -Wall -Wextra -o "$script_dir/led-watchdog" "$script_dir/led_watchdog.c"
echo "built: $script_dir/led-watchdog"
