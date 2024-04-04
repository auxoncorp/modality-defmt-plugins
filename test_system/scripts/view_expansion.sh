#!/usr/bin/env bash

set -euo pipefail

rustfmt --emit stdout target/rtic-expansion.rs | vim +':setlocal buftype=nofile filetype=rust' -

exit 0
