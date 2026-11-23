#!/bin/sh
# Builds Carrick's Linux package (.deb) into target/debian/: the release programs, the
# third-party licence file and the package. Needs Rust, Python 3 and
# cargo-deb (cargo install cargo-deb).
#   sh packaging/build-deb.sh
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p carrick -p carrick-gui
TARGET=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
python3 packaging/third_party.py "$TARGET/release/THIRD-PARTY-LICENSES.txt"
cargo deb -p carrick --no-build
ls -l "$TARGET"/debian/*.deb
