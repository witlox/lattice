# Lattice Examples

Practical examples for the Lattice distributed workload scheduler.

## Directory Structure

```
examples/
├── cli/                        # Command-line usage examples
│   ├── 01-basic-submit.sh      # Basic batch job submission
│   ├── 02-gpu-constraints.sh   # GPU jobs with topology constraints
│   ├── 03-task-groups.sh       # Task group (job array) submission
│   ├── 04-dag-workflow.sh      # DAG workflow submission
│   ├── 05-interactive-session.sh # Interactive sessions
│   └── 06-monitoring.sh        # Monitoring and observability
│
├── dag/                        # DAG workflow YAML definitions
│   ├── training-pipeline.yaml  # ML training pipeline (4 stages)
│   ├── checkpoint-workflow.yaml # Checkpoint-aware workflow
│   └── multi-stage-etl.yaml   # ETL pipeline with error handling
│
├── config/                     # Configuration examples
│   ├── single-node-dev.yaml    # Minimal development setup
│   ├── agent-nvidia.yaml       # NVIDIA GPU node agent config
│   └── weight-profiles/        # Cost function weight tuning
│       ├── baseline.yaml       # Default balanced weights
│       ├── fairness-tuned.yaml # Fair-share emphasis
│       └── energy-optimized.yaml # Energy-cost emphasis
│
├── python-sdk/                 # Python SDK usage examples
│   ├── 01-basic-submit.py      # Submit, poll, and print result
│   ├── 02-monitoring.py        # Watch events + stream metrics
│   ├── 03-dag-submission.py    # DAG submission via REST API
│   └── 04-agent-workflow.py    # Autonomous agent pattern
│
├── slurm-migration/            # Slurm-to-Lattice migration guide
│   ├── slurm-batch.sh          # Typical Slurm sbatch script
│   └── lattice-equivalent.sh   # Same job using Lattice CLI
│
└── rm-replay/                  # Scheduler weight tuning simulator
    ├── synthetic-trace.json    # Sample trace with 6 allocations
    └── tuning-workflow.sh      # Weight comparison workflow
```

## Getting Started

### CLI Examples

The CLI examples assume you have the `lattice` binary installed and a Lattice server
running. Set `LATTICE_SERVER` to your API endpoint, or use the default `localhost:50051`.

```bash
# Run a basic job submission
./examples/cli/01-basic-submit.sh
```

### Python SDK Examples

Install the SDK first:

```bash
pip install lattice-sdk
# or from source:
cd sdk/python && pip install -e .
```

Then run any example:

```bash
python examples/python-sdk/01-basic-submit.py
```

### DAG Workflows

Submit a DAG workflow using the CLI:

```bash
lattice dag submit examples/dag/training-pipeline.yaml
```

### RM-Replay Simulator

Test cost function weight changes before deploying:

```bash
cd tools/rm-replay
cargo run -- --input ../../examples/rm-replay/synthetic-trace.json
```

## Prerequisites

- **Lattice server** running (see `infra/docker/` for Docker Compose setup)
- **Rust toolchain** (for rm-replay): `rustup install stable`
- **Python 3.10+** (for SDK examples): with `httpx` installed
- **Environment variables**:
  - `LATTICE_SERVER` -- API server address (default: `localhost:50051`)
  - `LATTICE_TENANT` -- default tenant name
  - `LATTICE_VCLUSTER` -- default vCluster name

## Cost Function Weights

The scheduler uses a composite cost function with 9 factors:

| Factor | Description | Weight Key |
|--------|-------------|------------|
| f1 | Priority class (preemption tier) | `priority` |
| f2 | Wait time (anti-starvation, ages with queue time) | `wait_time` |
| f3 | Fair share deficit (tenant equity) | `fair_share` |
| f4 | Topology fitness (Slingshot dragonfly group packing) | `topology` |
| f5 | Data readiness (is input data on hot tier?) | `data_readiness` |
| f6 | Backlog pressure (queue depth) | `backlog` |
| f7 | Energy cost (time-varying electricity price) | `energy` |
| f8 | Checkpoint efficiency (preemption cost) | `checkpoint` |
| f9 | Conformance fitness (node config homogeneity) | `conformance` |

See `examples/config/weight-profiles/` for tuning presets, and `examples/rm-replay/`
for testing weight changes with the simulator.
