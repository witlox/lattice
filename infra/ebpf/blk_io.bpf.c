// SPDX-License-Identifier: Apache-2.0
// Lattice eBPF program: block I/O latency histogram
//
// Attaches to block_rq_issue and block_rq_complete tracepoints to measure
// per-device I/O latency. Latencies are accumulated in a log2 histogram.

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define MAX_SLOTS 32

struct io_event {
    __u64 timestamp_ns;
    __u32 dev_major;
    __u32 dev_minor;
    __u64 latency_ns;
    __u32 data_len;
    __u8  is_write;
};

// Track request issue times keyed by request pointer.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, __u64);   // request pointer
    __type(value, __u64); // issue timestamp
} req_start SEC(".maps");

// Latency histogram: slot i counts requests with latency in [2^i, 2^(i+1)) ns.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_SLOTS);
    __type(key, __u32);
    __type(value, __u64);
} latency_hist SEC(".maps");

// Output ring buffer for per-request events.
struct {
    __uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
    __uint(key_size, sizeof(__u32));
    __uint(value_size, sizeof(__u32));
} events SEC(".maps");

static __always_inline __u32 log2l(__u64 v) {
    __u32 r = 0;
    while (v > 1 && r < MAX_SLOTS - 1) {
        v >>= 1;
        r++;
    }
    return r;
}

SEC("tp/block/block_rq_issue")
int handle_block_rq_issue(struct trace_event_raw_block_rq *ctx)
{
    __u64 key = (__u64)ctx->__data_loc_rwbs; // use as request identifier
    __u64 ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&req_start, &key, &ts, BPF_ANY);
    return 0;
}

SEC("tp/block/block_rq_complete")
int handle_block_rq_complete(struct trace_event_raw_block_rq *ctx)
{
    __u64 key = (__u64)ctx->__data_loc_rwbs;
    __u64 *start = bpf_map_lookup_elem(&req_start, &key);
    if (!start)
        return 0;

    __u64 latency = bpf_ktime_get_ns() - *start;
    bpf_map_delete_elem(&req_start, &key);

    // Update histogram
    __u32 slot = log2l(latency);
    __u64 *count = bpf_map_lookup_elem(&latency_hist, &slot);
    if (count)
        __sync_fetch_and_add(count, 1);

    struct io_event evt = {
        .timestamp_ns = bpf_ktime_get_ns(),
        .dev_major = ctx->dev >> 20,
        .dev_minor = ctx->dev & 0xFFFFF,
        .latency_ns = latency,
        .data_len = ctx->bytes,
        .is_write = (ctx->rwbs[0] == 'W') ? 1 : 0,
    };

    bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU,
                          &evt, sizeof(evt));
    return 0;
}

char LICENSE[] SEC("license") = "Apache-2.0";
