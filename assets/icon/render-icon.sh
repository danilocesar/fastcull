#!/usr/bin/env bash
# The only producer of FastCull's rendered icon files (specs/modules/app-icon.md,
# brief 013). Reads the two SVG sources beside this script and writes the PNG set
# and the Windows .ico, every output stripped and then audited metadata-free: a
# PNG may hold only IHDR, IDAT and IEND chunks, and no output may contain a
# C2PA, JUMBF, XMP or EXIF marker ("Make sure it's c2pa free" — the user,
# 2026-10-06). Exit status is non-zero when the audit fails.
#
#   render-icon.sh [out-dir]
#
# Sources (this directory):
#   fastcull.svg         the master drawing, 512x512 viewBox, renders 48 px and up
#   fastcull-small.svg   the 16-32 px drawing (may be identical to the master)
# Outputs (<out-dir>, default: this directory):
#   png/fastcull-<N>.png   N in 16 22 24 32 48 64 128 256 512 (the hicolor sizes)
#   fastcull.ico           members 16 20 24 32 48 64 256 (20 is rendered for the
#                          .ico only and is not kept as a PNG)
#
# Needs ImageMagick 7 (`magick`) with the librsvg delegate, and python3 for the
# chunk walk. The committed renders were made with the tool versions the
# reproduction test records (crates/fastcull-app/tests/app_icon.rs); the output
# is byte-deterministic on one seat and tool version, and a different
# ImageMagick or librsvg renders different bytes without any regression — so a
# tool upgrade means: re-run, and if the bytes changed, commit the renders and
# the test's recorded versions together, saying so in the commit.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
master="$here/fastcull.svg"
small="$here/fastcull-small.svg"
out="${1:-$here}"

for f in "$master" "$small"; do
  [ -f "$f" ] || { echo "render-icon.sh: missing source $f" >&2; exit 2; }
done
command -v magick >/dev/null || { echo "render-icon.sh: ImageMagick 7 (magick) not found" >&2; exit 2; }
# The whole format list is read before it is searched. Piped straight into
# `grep -q`, grep exits at the first match while magick may still be
# writing; magick then dies of SIGPIPE (exit 141) and `pipefail` turns a
# seat that HAS librsvg into a refusal (2026-10-06: 955 of 3200 runs under
# 8-way load, 1 of 300 idle).
formats="$(magick -list format)" || formats=""
grep -q 'RSVG' <<<"$formats" || { echo "render-icon.sh: ImageMagick has no librsvg delegate" >&2; exit 2; }
command -v python3 >/dev/null || { echo "render-icon.sh: python3 not found (the chunk audit needs it)" >&2; exit 2; }

echo "tools: $(magick -version | head -1 | sed 's/^Version: //'); $(magick -list format | grep -o 'RSVG [0-9.]*' | head -1)"
mkdir -p "$out/png"

render() { # render <svg> <size> <file>
  magick -background none -density 384 "$1" -resize 512x512 \
    -filter Lanczos -resize "${2}x${2}" -strip "PNG32:$3"
}
for n in 16 22 24 32; do render "$small" "$n" "$out/png/fastcull-$n.png"; done
for n in 48 64 128 256 512; do render "$master" "$n" "$out/png/fastcull-$n.png"; done

# The .ico: the small drawing up to 32 (20 rendered here and discarded), the
# master from 48. Member order is part of the contract (app-icon.md).
render "$small" 20 "$out/png/_ico-20.png"
magick "$out/png/fastcull-16.png" "$out/png/_ico-20.png" "$out/png/fastcull-24.png" \
  "$out/png/fastcull-32.png" "$out/png/fastcull-48.png" "$out/png/fastcull-64.png" \
  "$out/png/fastcull-256.png" -strip "$out/fastcull.ico"
rm -f "$out/png/_ico-20.png"

# Metadata audit — the same two rules the repository test asserts over the
# committed files. Markers are case-sensitive on purpose: a case-insensitive
# scan of compressed pixel data false-positives at roughly 1e-4 per file, and
# every manifest writer emits the canonical spellings.
bad=0
for f in "$out"/png/*.png "$out"/fastcull.ico; do
  if grep -a -q -E 'c2pa|jumb|jumd|urn:uuid|<x:xmpmeta|adobe:ns:meta|Exif' "$f"; then
    echo "METADATA MARKER FOUND in $f" >&2; bad=1
  fi
done
python3 - "$out"/png/*.png "$out"/fastcull.ico <<'PY' || bad=1
import struct, sys

def png_chunks(d):
    pos, names = 8, []
    while pos + 8 <= len(d):
        n = struct.unpack('>I', d[pos:pos + 4])[0]
        names.append(d[pos + 4:pos + 8].decode('latin1'))
        pos += 12 + n
    return names

ok = True
for p in sys.argv[1:]:
    d = open(p, 'rb').read()
    if d[:8] == b'\x89PNG\r\n\x1a\n':
        names = png_chunks(d)
        if [c for c in names if c not in ('IHDR', 'IDAT', 'IEND')] or names[0] != 'IHDR' or names[-1] != 'IEND':
            print(f"EXTRA CHUNKS in {p}: {names}", file=sys.stderr); ok = False
    elif p.endswith('.ico'):
        n = struct.unpack('<H', d[4:6])[0]
        for i in range(n):
            size, off = struct.unpack('<II', d[6 + 16 * i + 8:6 + 16 * i + 16])
            m = d[off:off + size]
            if m[:8] == b'\x89PNG\r\n\x1a\n':
                names = png_chunks(m)
                if [c for c in names if c not in ('IHDR', 'IDAT', 'IEND')]:
                    print(f"EXTRA CHUNKS in {p} member {i}: {names}", file=sys.stderr); ok = False
sys.exit(0 if ok else 1)
PY

if [ "$bad" -eq 0 ]; then
  echo "metadata audit: clean (PNG chunks IHDR/IDAT/IEND only; no C2PA, JUMBF, XMP or EXIF markers)"
else
  echo "metadata audit: FAILED — do not commit these renders" >&2
fi
magick identify "$out/fastcull.ico" | awk '{print $1, $3}' | sed 's/^/ico: /'
ls -l "$out/png" | awk 'NR>1{print $5, $9}' | sed 's/^/png: /'
exit "$bad"
