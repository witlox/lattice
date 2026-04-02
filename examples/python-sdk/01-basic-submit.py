#!/usr/bin/env python3
"""01-basic-submit.py — Submit a job, poll for completion, print result.

Demonstrates the basic LatticeClient workflow:
  1. Connect to the Lattice REST API
  2. Submit an allocation with resource requirements
  3. Poll status until the job completes
  4. Print the final result

Prerequisites:
  pip install lattice-sdk
"""

import asyncio
import os

from lattice_sdk import LatticeClient, AllocationSpec


async def main():
    host = os.environ.get("LATTICE_HOST", "localhost")
    port = int(os.environ.get("LATTICE_PORT", "8080"))

    # Connect using async context manager (auto-connects and auto-closes).
    async with LatticeClient(host=host, port=port) as client:
        # Verify the server is reachable.
        health = await client.health()
        print(f"Server health: {health}")

        # Define the allocation specification.
        spec = AllocationSpec(
            entrypoint="python train.py --epochs 50 --batch-size 128",
            tenant="ml-team",
            nodes=4,
            walltime_hours=2.0,
            image="nvcr.io/nvidia/pytorch:24.01-py3",
        )

        # Submit the allocation.
        alloc_id = await client.submit(spec)
        print(f"Submitted allocation: {alloc_id}")

        # Poll until the allocation reaches a terminal state.
        terminal_states = {"completed", "failed", "cancelled"}
        alloc = await client.status(alloc_id)
        while alloc.state.value not in terminal_states:
            await asyncio.sleep(5)
            alloc = await client.status(alloc_id)
            print(f"  State: {alloc.state.value}", end="")
            if alloc.nodes_allocated:
                print(f" (nodes: {', '.join(alloc.nodes_allocated)})", end="")
            print()

        # Print final result.
        print(f"\nAllocation {alloc_id} finished:")
        print(f"  Final state: {alloc.state.value}")
        print(f"  Exit code: {alloc.exit_code}")
        if alloc.message:
            print(f"  Message: {alloc.message}")
        print(f"  Started: {alloc.started_at}")
        print(f"  Completed: {alloc.completed_at}")


if __name__ == "__main__":
    asyncio.run(main())
