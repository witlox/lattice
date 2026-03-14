#!/usr/bin/env python3
"""04-agent-workflow.py — Autonomous agent pattern.

Demonstrates an AI agent workflow that:
  1. Submits a training job
  2. Watches for the allocation to reach RUNNING state
  3. Streams metrics and monitors GPU utilization
  4. Cancels the job if GPU utilization drops below a threshold
     (indicating the training has stalled or diverged)

This pattern is designed for the Intent API where agents declare
what they need and react to observed conditions.

Prerequisites:
  pip install lattice-sdk
"""

import asyncio
import os

from lattice_sdk import LatticeClient, AllocationSpec


# Configuration for the agent's decision-making.
GPU_UTIL_THRESHOLD = 10.0  # Cancel if GPU util drops below 10%
STALL_WINDOW = 5           # Number of consecutive low-util samples before cancelling
METRIC_TIMEOUT = 300       # Stop monitoring after 5 minutes (for this demo)


async def wait_for_running(client: LatticeClient, alloc_id: str) -> bool:
    """Watch events until the allocation is RUNNING or reaches a terminal state."""
    print(f"Waiting for {alloc_id} to start running...")

    async for event in client.watch(alloc_id):
        state = event.allocation.state.value
        print(f"  Event: {event.event_type} -> {state}")

        if state == "running":
            print(f"  Allocation is now running on: {event.allocation.nodes_allocated}")
            return True
        if state in ("completed", "failed", "cancelled"):
            print(f"  Allocation reached terminal state: {state}")
            return False

    return False


async def monitor_and_react(client: LatticeClient, alloc_id: str) -> str:
    """Stream metrics and decide whether to cancel based on GPU utilization.

    Returns the reason for stopping: "stalled", "timeout", or "completed".
    """
    print(f"\nMonitoring GPU utilization (threshold: {GPU_UTIL_THRESHOLD}%)...")
    low_util_count = 0

    try:
        async with asyncio.timeout(METRIC_TIMEOUT):
            async for metrics in client.stream_metrics(alloc_id):
                gpu_util = metrics.gpu_utilization

                # Log current metrics.
                status = "OK" if gpu_util >= GPU_UTIL_THRESHOLD else "LOW"
                print(
                    f"  [{metrics.timestamp}] GPU: {gpu_util:.1f}% [{status}] | "
                    f"CPU: {metrics.cpu_utilization:.1f}% | "
                    f"Mem: {metrics.memory_utilization_gb:.1f}GB"
                )

                # Check for stall condition.
                if gpu_util < GPU_UTIL_THRESHOLD:
                    low_util_count += 1
                    if low_util_count >= STALL_WINDOW:
                        print(
                            f"\n  GPU utilization below {GPU_UTIL_THRESHOLD}% for "
                            f"{STALL_WINDOW} consecutive samples. Training appears stalled."
                        )
                        return "stalled"
                else:
                    low_util_count = 0

    except TimeoutError:
        return "timeout"

    return "completed"


async def main():
    host = os.environ.get("LATTICE_HOST", "localhost")
    port = int(os.environ.get("LATTICE_PORT", "8080"))

    async with LatticeClient(host=host, port=port) as client:
        # Step 1: Submit a training job.
        spec = AllocationSpec(
            entrypoint="python train.py --model resnet50 --epochs 100",
            nodes=4,
            cpus=64.0,
            memory_gb=256.0,
            gpus=4,
            gpu_memory_gb=80.0,
            priority_class="normal",
            tenant_id="ml-team",
            uenv="pytorch:2.3",
        )

        alloc = await client.submit(spec)
        alloc_id = alloc.id
        print(f"Submitted allocation: {alloc_id}")

        # Step 2: Wait for the job to start running.
        is_running = await wait_for_running(client, alloc_id)
        if not is_running:
            print("Job did not start running. Exiting.")
            return

        # Step 3: Monitor metrics and react.
        reason = await monitor_and_react(client, alloc_id)

        # Step 4: Take action based on the monitoring result.
        if reason == "stalled":
            print(f"\nCancelling stalled allocation {alloc_id}...")
            cancelled = await client.cancel(alloc_id)
            if cancelled:
                print("Allocation cancelled successfully.")
                print("Consider: reduce batch size, check data pipeline, or inspect logs.")
            else:
                print("Failed to cancel allocation.")
        elif reason == "timeout":
            print(f"\nMonitoring timeout reached. Allocation {alloc_id} still running.")
            print("The job will continue running until walltime expires.")
        else:
            alloc = await client.status(alloc_id)
            print(f"\nAllocation {alloc_id} completed: {alloc.state.value}")


if __name__ == "__main__":
    asyncio.run(main())
