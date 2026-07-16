#!/usr/bin/env bash
#
# Re-fetches the README demo sticker GIFs from KLIPY (https://klipy.com), the
# project's sticker provider. This is provenance + reproducibility for the
# committed docs/stickers/*.gif — the README "Bring your own stickers" demos
# composite these through kine's image-asset feature.
#
# The stickers are KLIPY content, used under KLIPY's API terms of use
# (https://klipy.com/support/api-terms). The API key is a secret and is NOT
# committed — pass it in the environment:
#
#   KLIPY_API_KEY=<your key> scripts/fetch-stickers.sh
#
# We take the "sm" (small) rendition so decoded frames stay under kine's 32MB
# per-asset budget (frames x w x h x 4). The CDN blocks default user agents, so
# a browser UA + referer is sent.
#
set -euo pipefail
: "${KLIPY_API_KEY:?set KLIPY_API_KEY (get one at https://klipy.com/developers)}"
cd "$(dirname "$0")/../docs/stickers"

UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15'
API="https://api.klipy.com/api/v1/${KLIPY_API_KEY}/stickers/search"

fetch() { # query  sticker_id  outfile
  local url
  url=$(curl -s "$API?q=$1&per_page=25&customer_id=kine-readme" \
    | python3 -c "import sys,json; d=json.load(sys.stdin); \
print(next(it['file']['sm']['gif']['url'] for it in d['data']['data'] if str(it['id'])=='$2'))")
  curl -s -A "$UA" -e "https://klipy.com/" "$url" -o "$3"
  echo "  $3  <=  klipy sticker $2"
}

# id, query and one-line description of each committed sticker:
fetch "gold+star" 9946759279709363 stars.gif   # "Star Estrella" — three twinkling gold stars
fetch "follow+me" 5566941337520777 follow.gif  # "Follow Now" — Instagram-style peel sticker
echo "done."
