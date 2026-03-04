# Lattice eBPF Programs

Kernel-level telemetry programs for the Lattice node agent. These attach to
tracepoints and kprobes to collect scheduling, I/O, memory, and network
metrics with minimal overhead (<0.3% on compute-bound workloads).

## Programs

| Program | Tracepoints/Probes | Metrics |
|---------|-------------------|---------|
| `sched_events.bpf.c` | `tp/sched/sched_switch`, `tp/sched/sched_wakeup` | Per-CPU scheduling latency |
| `blk_io.bpf.c` | `tp/block/block_rq_issue`, `tp/block/block_rq_complete` | I/O latency histogram |
| `mem_events.bpf.c` | `tp/kmem/mm_page_alloc`, `tp/kmem/mm_page_free` | Page allocation rate counters |
| `net_flow.bpf.c` | `kprobe/tcp_sendmsg`, `kprobe/tcp_recvmsg` | Per-flow byte counters |

## Building

Prerequisites:
- `clang` >= 12 (for BPF target support)
- `libbpf-dev` (BPF helper headers)
- `bpftool` (to generate `vmlinux.h` from kernel BTF)

```bash
# Generate vmlinux.h from running kernel
make vmlinux.h

# Build all programs
make

# Install to /opt/lattice/ebpf/
sudo make install
```

## Deployment

The compiled `.bpf.o` files are loaded by the `lattice-agent` when the `ebpf`
feature flag is enabled. The agent reads programs from the path configured in
`TelemetryConfig::ebpf_programs_path` (default: `/opt/lattice/ebpf/`).

On systems without BPF support, the agent falls back to the `/proc`-based
`ProcSysCollector` which provides equivalent metrics from procfs.

## Architecture

Each program outputs events via `BPF_MAP_TYPE_PERF_EVENT_ARRAY` or exposes
counters via `BPF_MAP_TYPE_PERCPU_ARRAY`. The node agent's `AyaEbpfCollector`
polls these maps and converts events into the `EbpfEvent` format consumed by
the telemetry pipeline.
