#!/usr/bin/env bash
# Fetch sample RAW files used by integration tests and benchmarks.
# Files land in testdata/raws/ (gitignored). Skips files already present
# with the expected size, so it is safe to re-run and CI-cacheable.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p raws

# name|url|expected_bytes  (sizes pin the exact upstream revisions we test against)
FILES='
A1_full_compressed.ARW|https://raw.pixls.us/data/Sony/ILCE-1/A1_full_compressed.ARW|63216896
A1_full_lossless_compressed.ARW|https://raw.pixls.us/data/Sony/ILCE-1/A1_full_lossless_compressed.ARW|81575280
A1_full_uncompressed.ARW|https://raw.pixls.us/data/Sony/ILCE-1/A1_full_uncompressed.ARW|113224192
'

status=0
for line in $FILES; do
    name="${line%%|*}"; rest="${line#*|}"
    url="${rest%%|*}"; size="${rest#*|}"
    dest="raws/$name"
    if [ -f "$dest" ] && [ "$(wc -c < "$dest")" -eq "$size" ]; then
        echo "ok       $name"
        continue
    fi
    echo "fetching $name"
    if ! curl -fSL --retry 3 -o "$dest.part" "$url"; then
        echo "FAILED   $name" >&2; status=1; rm -f "$dest.part"; continue
    fi
    actual="$(wc -c < "$dest.part")"
    if [ "$actual" -ne "$size" ]; then
        echo "SIZE MISMATCH $name: got $actual want $size" >&2
        rm -f "$dest.part"; status=1; continue
    fi
    mv "$dest.part" "$dest"
done
exit $status
