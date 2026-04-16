# Slurm Comparison Harness (Phase C — Sketch)

**Status:** draft — design sketch, not yet scheduled for implementation.
**Prerequisite:** Phase A (OV suite green) + Phase B (Slurm compat + canonical workloads) complete.

## Goal

Produce defensible quantitative evidence that Lattice is a viable replacement
for Slurm by running identical workloads on both systems and comparing
scheduling behaviour, throughput, fairness, and operational characteristics.

Out of scope for this document: deciding *which* option to execute. This doc
enumerates the options and their trade-offs so a scheduling decision can be
made after Phase B.

## Two paths, non-exclusive

### Option C1 — Trace-driven comparison (RM-Replay vs Slurm simulator)

Reuses infrastructure that already exists. Fastest to execute.

```
           ┌─────────────────────┐
           │  Anonymised trace   │  (Slurm sacct export from a real site,
           │  (N allocations)    │   or synthetic trace covering canonical
           └──────────┬──────────┘   HPC patterns)
                      │
       ┌──────────────┴──────────────┐
       ▼                             ▼
┌──────────────┐             ┌──────────────────┐
│  RM-Replay   │             │ slurm-simulator  │
│  (existing)  │             │ (sim-slurm or    │
│              │             │  slurmSim)       │
└──────┬───────┘             └─────────┬────────┘
       │                               │
       └──────────────┬────────────────┘
                      ▼
           ┌─────────────────────┐
           │  Comparison report  │
           │  (metrics below)    │
           └─────────────────────┘
```

**What works already:**
- `tools/rm-replay` ingests JSON traces and scores allocations with the real
  `CostEvaluator` from `lattice-scheduler`.
- Trace format supports priority, tenant, node count, checkpoint strategy,
  data readiness, fair-share targets.

**What needs building:**
1. Slurm-trace → RM-Replay trace converter (sacct fields → TraceEntry).
2. RM-Replay output → comparable metric schema (makespan, wait p50/p95,
   throughput, utilisation, fair-share divergence).
3. Slurm-side simulator — either Micro Slurm Simulator (slurm-sim) or the
   BSC `slurm_simulator` fork. Neither is trivial to deploy; both need
   Slurm built from source with the simulator patch.
4. Workload trace corpus:
   - One real site trace (anonymised, consented)
   - Three synthetic traces: batch-heavy, mixed batch+service, burst

**Effort:** small-to-medium. ~2 weeks for harness + converters + trace corpus.

**Limitation:** RM-Replay scores allocations but does not execute them on a
live cluster. Comparisons are about *scheduler decisions* (what gets placed
when, by whom), not about runtime behaviour (service liveness, preemption
cost, agent recovery). Those must come from C2 or Phase B canonical workloads.

### Option C2 — Dual-stack live comparison (real workloads, two clusters)

Definitive but expensive.

```
┌──────────────────────────────────────────────────────────────┐
│  Shared workload generator                                   │
│  (replays same job stream against both clusters concurrently)│
└─────────────────────┬────────────────────┬───────────────────┘
                      ▼                    ▼
      ┌───────────────────────┐  ┌───────────────────────┐
      │  Slurm mini-cluster   │  │  Lattice mini-cluster │
      │  (3× control + 4×     │  │  (3× quorum + 4×      │
      │   compute, GCP)       │  │   compute, GCP)       │
      └───────────┬───────────┘  └───────────┬───────────┘
                  │                          │
                  └────────────┬─────────────┘
                               ▼
                   ┌───────────────────────┐
                   │  Prometheus / TSDB    │
                   │  side-by-side metrics │
                   └───────────────────────┘
```

**What works already:**
- GCP Terraform scaffolding (`infra/gcp/`) for lattice.
- `tests/ov/` docker-compose + GCP harness abstractions via `TestCluster`.
- lattice-server / lattice-agent systemd units.

**What needs building:**
1. Mirror Terraform module for a Slurm mini-cluster (slurmctld + slurmdbd +
   4 slurmd nodes) on the same GCP project/VPC.
2. Workload generator (Python) that:
   - Takes a job-stream spec (YAML: `{interarrival, runtime, nodes, priority, tenant}`)
   - Submits concurrently to both Slurm (`sbatch`) and Lattice (`lattice submit`)
   - Records per-job submit/start/end timestamps on both sides
3. Metric exporter for Slurm (sacct → Prometheus) — Lattice already exports.
4. Comparison dashboards (Grafana) + a static report generator.
5. Canonical job-stream specs (same as C1 synthetic traces, made runnable).

**Effort:** large. ~4–6 weeks including Slurm Terraform, workload generator,
metric reconciliation, and 1–2 weeks of run-tune-rerun.

**Cost:** non-trivial. Keeping two 7-node GCP clusters up during comparison
runs; use preemptible VMs where possible.

**Limitation:** mini-cluster ≠ production. Results are directionally useful
but a production site will still want its own validation before cutover.
This is the "Phase 1 — Dual-Stack" in `docs/architecture/slurm-migration.md`
executed as an engineering harness rather than a production pilot.

## Metrics to compare

Regardless of path, the comparison must produce these metrics side-by-side:

| Metric | Definition | Why it matters |
|---|---|---|
| Throughput | allocations completed per unit time | Core productivity measure |
| Wait time p50 / p95 | queue-time distribution | User experience |
| Makespan | total time to drain a workload | Batch-science KPI |
| Utilisation | Σ(node-seconds used) / Σ(node-seconds available) | Operational efficiency |
| Fair-share divergence | Σ\|actual − target\|² across tenants | Multi-tenant fairness |
| Backfill efficiency | small-job insertions into reservation gaps | Slurm's headline feature |
| Preemption cost | CPU-seconds lost to preemption events | Checkpoint design validation |
| Scheduling latency | submit → first scheduling decision | Responsiveness |
| Sensitive-workload isolation | ownership violations (must be 0) | Compliance gate |
| Service uptime | for Unbounded allocations: availability % | Feature Slurm lacks |
| DAG latency | queue-to-completion for dependent chains | Workflow-system comparison |

The last three are Slurm-unsupported features. For C1 they are annotated
as "Lattice-only". For C2 they are measured on Lattice alone but reported
alongside as a capability delta.

## Workload corpus

Same corpus for C1 and C2. Four canonical job streams:

1. **batch-heavy** — 1000 jobs, 95% bounded batch, random walltimes
   5 min–8 h, Zipfian priority, single tenant. Models ML-training cluster.
2. **mixed** — 500 bounded + 20 unbounded services + 5 DAGs of 10–50 tasks.
   Three tenants with fair-share quotas 50/30/20. Models shared HPC site.
3. **burst** — steady background load + sudden 300-job burst from one tenant
   at t=30 min. Measures backlog response.
4. **sensitive** — 50 sensitive-workload claims interleaved with 200 regular
   batch jobs across two tenants. Measures isolation + audit.

Stored in `tests/comparison/corpus/*.yaml`. The same spec is consumed by
both C1 (rendered to RM-Replay JSON + Slurm trace) and C2 (rendered to live
submission scripts).

## Deliverables

- `specs/architecture/slurm-comparison.md` (this doc)
- `tools/comparison/` crate: trace converters + workload generator
- `tests/comparison/corpus/*.yaml`
- `tests/comparison/reports/` — versioned output from each run
- Optional: Grafana dashboards in `infra/grafana/comparison.json`

## Decision gates

Before starting C1:
- Phase A green (OV suite proves basic lifecycle works)
- Phase B green (canonical workloads demonstrate feature parity)
- Trace corpus agreed with stakeholders

Before starting C2:
- C1 complete — if C1 shows Lattice is far behind on any core metric,
  fix in-place before spending GCP budget on C2
- Budget approval for ~4 weeks of dual GCP clusters
- Identification of at least one real Slurm site willing to share an
  anonymised trace for validation (reduces "synthetic-only" critique)

## Cross-references

- `docs/architecture/slurm-migration.md` — migration phases (C2 is Phase 1 engineered)
- `docs/architecture/scheduling-algorithm.md` — cost-function definition
- `tools/rm-replay/` — existing trace replay tool
- `tests/ov/` — Phase A/B operational-validation harness
