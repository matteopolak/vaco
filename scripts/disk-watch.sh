#!/usr/bin/env bash
# Watch free disk space while a wave of agents builds, and reclaim the cheap
# space automatically before it becomes a problem.
#
# Usage: scripts/disk-watch.sh [minutes] [floor_gib]
#
# Why this exists as a script rather than an inline command: the inline version
# printed "never dropped below 9GiB", which is ambiguous between a low-water
# mark and a maximum drop, and an unreadable number is the same as no number.
# This one labels both.
#
# What it will delete on its own, and what it will not
# ----------------------------------------------------
# Automatic, when free space falls under the floor: build outputs, which are by
# definition reproducible — cargo `target/` directories belonging to *other*
# projects (never this one; agents are compiling into it) and this repo's own
# scratch directories under /tmp.
#
# Reported but never deleted automatically: anything a human would have to
# re-download or re-create — Steam games, container images, node_modules.
# Reclaiming those is a decision, and a background loop is the wrong place to
# make one unattended.
set -uo pipefail

MINUTES="${1:-45}"
FLOOR_GIB="${2:-25}"
INTERVAL=60
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

free_gib() { df -g /System/Volumes/Data | awk 'NR==2{print $4}'; }

reclaim() {
  local before after
  before=$(free_gib)
  # Other projects' build outputs. `-prune` so we never descend into one, and
  # an explicit skip of this repo so a running build is never pulled out from
  # under an agent.
  # Other projects' debug targets first: they are the biggest and the cheapest
  # to rebuild. Measured 2026-08-23 — one of them had grown to 57GB and was the
  # entire reason free space fell from 77GiB to 20GiB with six agents running.
  # `-mtime +0` rather than `+2`: under this load a day-old target is stale
  # enough, and waiting two days is how the floor gets breached.
  find "$HOME/projects" -maxdepth 4 -type d -name debug -path "*/target/*" \
       -not -path "$REPO/*" -mtime +0 -prune -print -exec rm -rf {} + 2>/dev/null
  find "$HOME/projects" -maxdepth 3 -type d -name target \
       -not -path "$REPO/*" -mtime +2 -prune -print -exec rm -rf {} + 2>/dev/null
  rm -rf /tmp/vaco-p0 /tmp/vaco-fresh /tmp/vaco-fuzzlog 2>/dev/null
  find /tmp -maxdepth 1 -name 'vaco-*' -type d -mtime +1 -prune \
       -exec rm -rf {} + 2>/dev/null
  after=$(free_gib)
  echo "reclaimed $((after - before))GiB (${before} -> ${after})"
}

start=$(free_gib)
low=$start
cleans=0
for ((i = 0; i < MINUTES; i++)); do
  now=$(free_gib)
  ((now < low)) && low=$now
  if ((now < FLOOR_GIB)); then
    echo "free ${now}GiB is under the ${FLOOR_GIB}GiB floor; reclaiming"
    reclaim
    cleans=$((cleans + 1))
  fi
  sleep "$INTERVAL"
done

end=$(free_gib)
echo "disk watch over ${MINUTES}m: started ${start}GiB, low-water ${low}GiB, now ${end}GiB; ${cleans} reclaim(s)"
if ((end < FLOOR_GIB * 2)); then
  echo "candidates a human should decide on (nothing deleted):"
  du -sh "$HOME/Library/Application Support/Steam/steamapps/common" 2>/dev/null
  docker system df 2>/dev/null | sed -n '1,5p'
  find "$HOME/projects" -maxdepth 3 -type d -name node_modules -prune \
       -exec du -sh {} + 2>/dev/null | sort -rh | head -5
fi
