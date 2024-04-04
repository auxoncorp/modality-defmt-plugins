#!/usr/bin/env bash

set -euo pipefail

cat /tmp/rtt_log.bin | defmt-print -e target/thumbv7em-none-eabihf/release/atsamd-rtic-firmware

exit 0
