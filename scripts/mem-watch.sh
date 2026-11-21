#!/usr/bin/env bash
# Probe only: runs a command while printing memory use to the live log.
a=/sys/fs/cgroup/system.slice/hosted-compute-agent.service
(
  while true; do
    echo "MEM $(date -u +%T) $(free -m | awk 'NR==2{printf "used=%s avail=%s ", $3, $7} NR==3{printf "swap=%s ", $3}')psi=$(awk '/^full/{print $2}' /proc/pressure/memory) agent=$(($(cat $a/memory.current 2>/dev/null || echo 0)/1048576))M agentmin=$(($(cat $a/memory.min 2>/dev/null || echo 0)/1048576))M agentswap=$(($(cat $a/memory.swap.current 2>/dev/null || echo 0)/1048576))M"
    sleep 20
  done
) &
monitor=$!
set +e
"$@"
status=$?
kill "$monitor"
exit "$status"
