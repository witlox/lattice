#!/usr/bin/env python3
"""02-monitoring.py — Watch allocation events and stream metrics.

Demonstrates Lattice's observability features via the Python SDK:
  - watch(): Stream lifecycle events (state transitions) via SSE
  - stream_metrics(): Stream resource utilization metrics via SSE
  - logs(): Stream or fetch allocation logs

Prerequisites:
  pip install lattice-sdk
"""

import asyncio
import os
import sys

from lattice_sdk import LatticeClient


async def watch_events(client: LatticeClient, alloc_id: str):
    """Stream lifecycle events until the allocation reaches a terminal state."""
    print(f"\n--- Watching events for {alloc_id} ---")
    terminal_states = {"completed", "failed", "cancelled"}

    async for event in client.watch(alloc_id):
        state = event.allocation.state.value
        print(f"[{event.timestamp}] {event.event_type}: state={state}")

        if state in terminal_states:
            print(f"Allocation reached terminal state: {state}")
            break


async def stream_metrics(client: LatticeClient, alloc_id: str, duration_seconds: int = 60):
    """Stream resource utilization metrics for a fixed duration."""
    print(f"\n--- Streaming metrics for {alloc_id} ({duration_seconds}s) ---")

    # Use asyncio.wait_for to limit the streaming duration.
    try:
        async with asyncio.timeout(duration_seconds):
            async for metrics in client.stream_metrics(alloc_id):
                print(
                    f"[{metrics.timestamp}] "
                    f"CPU: {metrics.cpu_utilization:.1f}% | "
                    f"GPU: {metrics.gpu_utilization:.1f}% | "
                    f"Mem: {metrics.memory_utilization_gb:.1f}GB | "
                    f"Net: {metrics.network_in_mbps:.0f}/{metrics.network_out_mbps:.0f} Mbps"
                )
    except TimeoutError:
        print(f"Metric streaming stopped after {duration_seconds}s")


async def tail_logs(client: LatticeClient, alloc_id: str, num_lines: int = 20):
    """Fetch recent logs (non-streaming)."""
    print(f"\n--- Last {num_lines} log lines for {alloc_id} ---")

    async for entry in client.logs(alloc_id, follow=False, lines=num_lines):
        print(f"[{entry.timestamp}] [{entry.level}] {entry.message}")


async def main():
    host = os.environ.get("LATTICE_HOST", "localhost")
    port = int(os.environ.get("LATTICE_PORT", "8080"))

    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <allocation-id> [watch|metrics|logs]")
        print("  watch   - Stream lifecycle events (default)")
        print("  metrics - Stream resource utilization for 60 seconds")
        print("  logs    - Show recent log entries")
        sys.exit(1)

    alloc_id = sys.argv[1]
    mode = sys.argv[2] if len(sys.argv) > 2 else "watch"

    async with LatticeClient(host=host, port=port) as client:
        # First, check that the allocation exists.
        alloc = await client.status(alloc_id)
        print(f"Allocation {alloc.id}: {alloc.state.value}")

        if mode == "watch":
            await watch_events(client, alloc_id)
        elif mode == "metrics":
            await stream_metrics(client, alloc_id, duration_seconds=60)
        elif mode == "logs":
            await tail_logs(client, alloc_id)
        else:
            print(f"Unknown mode: {mode}")
            sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
