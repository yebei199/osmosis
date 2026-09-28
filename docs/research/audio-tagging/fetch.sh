#!/usr/bin/env bash
# 按 sample.json 经 GET /play 取播放源并下载;音频只在打标期间留在 audio/,打完删
set -euo pipefail
cd ~/audiotag; mkdir -p audio
B=https://music.cryptorust.uk; T=$(cat token)
for id in $(jq -r '.[].id' sample.json); do
  [ -s audio/$id ] && continue
  src=$(curl -s --max-time 30 $B/play/$id -H "Authorization: Bearer $T")
  url=$(jq -r .url <<<"$src"); trial=$(jq -r .trial <<<"$src")
  curl -s --max-time 120 -L -o audio/$id "$url" || echo "FAIL $id"
  echo "$id trial=$trial fmt=$(jq -r .format <<<"$src") size=$(stat -c %s audio/$id)"
done
