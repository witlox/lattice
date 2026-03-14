#!/usr/bin/env python3
"""03-dag-submission.py — Submit a DAG workflow via the REST API.

Demonstrates DAG submission using the Python SDK's httpx client.
The DAG is defined as a Python dictionary and submitted via POST /api/v1/dags.
This approach is useful when DAGs are generated programmatically
(e.g., from experiment configs or agent decisions).

Prerequisites:
  pip install lattice-sdk
"""

import asyncio
import os

import httpx

from lattice_sdk import LatticeClient


async def main():
    host = os.environ.get("LATTICE_HOST", "localhost")
    port = int(os.environ.get("LATTICE_PORT", "8080"))
    base_url = f"http://{host}:{port}"

    # Define a DAG programmatically.
    # This is the same structure as the YAML files in examples/dag/,
    # but constructed in Python for dynamic workflow generation.
    dag_spec = {
        "allocations": [
            {
                "id": "preprocess",
                "tenant": "ml-team",
                "project": "dynamic-pipeline",
                "entrypoint": "python preprocess.py --input s3://data/raw",
                "resources": {
                    "min_nodes": 2,
                    "max_nodes": 2,
                },
                "lifecycle": {
                    "type": 0,  # BOUNDED
                    "walltime": {"seconds": 3600, "nanos": 0},
                },
                "depends_on": [],
            },
            {
                "id": "train",
                "tenant": "ml-team",
                "project": "dynamic-pipeline",
                "entrypoint": "torchrun --nproc_per_node=4 train.py",
                "resources": {
                    "min_nodes": 8,
                    "max_nodes": 8,
                },
                "lifecycle": {
                    "type": 0,
                    "walltime": {"seconds": 86400, "nanos": 0},
                },
                "depends_on": [
                    {"ref_id": "preprocess", "condition": "success"},
                ],
            },
            {
                "id": "evaluate",
                "tenant": "ml-team",
                "project": "dynamic-pipeline",
                "entrypoint": "python evaluate.py --model /scratch/model.pt",
                "resources": {
                    "min_nodes": 1,
                    "max_nodes": 1,
                },
                "lifecycle": {
                    "type": 0,
                    "walltime": {"seconds": 1800, "nanos": 0},
                },
                "depends_on": [
                    {"ref_id": "train", "condition": "success"},
                ],
            },
        ],
    }

    # Submit the DAG via REST API.
    async with httpx.AsyncClient(base_url=base_url, timeout=30.0) as http_client:
        response = await http_client.post(
            "/api/v1/dags",
            json=dag_spec,
        )
        response.raise_for_status()
        result = response.json()
        print(f"DAG submitted successfully:")
        print(f"  DAG ID: {result.get('dag_id', 'N/A')}")
        print(f"  Allocations: {result.get('allocation_ids', [])}")

    # Now use the SDK to monitor the DAG's progress.
    dag_id = result.get("dag_id")
    if dag_id:
        async with LatticeClient(host=host, port=port) as client:
            # Poll each allocation in the DAG.
            for alloc_id in result.get("allocation_ids", []):
                alloc = await client.status(alloc_id)
                print(f"  {alloc_id}: {alloc.state.value}")


if __name__ == "__main__":
    asyncio.run(main())
