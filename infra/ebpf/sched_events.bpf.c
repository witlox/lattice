// SPDX-License-Identifier: Apache-2.0
// Lattice eBPF program: scheduling latency events
//
// Attaches to sched_switch and sched_wakeup tracepoints to measure
// per-CPU scheduling latency. Events are emitted to a perf event array
// consumed by the lattice-node-agent.

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

struct sched_event {
    __u64 timestamp_ns;
    __u32 cpu;
    __u32 prev_pid;
    __u32 next_pid;
    __u64 latency_ns;
};

// Per-CPU map to store wakeup timestamps for latency calculation.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, __u32);   // pid
    __type(value, __u64); // wakeup timestamp
} wakeup_ts SEC(".maps");

// Output ring buffer for events.
struct {
    __uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
    __uint(key_size, sizeof(__u32));
    __uint(value_size, sizeof(__u32));
} events SEC(".maps");

SEC("tp/sched/sched_wakeup")
int handle_sched_wakeup(struct trace_event_raw_sched_wakeup_template *ctx)
{
    __u32 pid = ctx->pid;
    __u64 ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&wakeup_ts, &pid, &ts, BPF_ANY);
    return 0;
}

SEC("tp/sched/sched_switch")
int handle_sched_switch(struct trace_event_raw_sched_switch *ctx)
{
    __u32 next_pid = ctx->next_pid;
    __u64 *wakeup = bpf_map_lookup_elem(&wakeup_ts, &next_pid);
    if (!wakeup)
        return 0;

    __u64 now = bpf_ktime_get_ns();
    __u64 latency = now - *wakeup;
    bpf_map_delete_elem(&wakeup_ts, &next_pid);

    struct sched_event evt = {
        .timestamp_ns = now,
        .cpu = bpf_get_smp_processor_id(),
        .prev_pid = ctx->prev_pid,
        .next_pid = next_pid,
        .latency_ns = latency,
    };

    bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU,
                          &evt, sizeof(evt));
    return 0;
}

char LICENSE[] SEC("license") = "Apache-2.0";
