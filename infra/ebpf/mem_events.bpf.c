// SPDX-License-Identifier: Apache-2.0
// Lattice eBPF program: memory allocation rate counters
//
// Attaches to kmem tracepoints to track page allocation and free rates.
// Counters are exposed via a per-CPU array map.

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

enum counter_idx {
    ALLOC_COUNT = 0,
    FREE_COUNT  = 1,
    ALLOC_PAGES = 2,
    FREE_PAGES  = 3,
    MAX_COUNTERS = 4,
};

// Per-CPU counters to avoid contention.
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, MAX_COUNTERS);
    __type(key, __u32);
    __type(value, __u64);
} mem_counters SEC(".maps");

SEC("tp/kmem/mm_page_alloc")
int handle_page_alloc(struct trace_event_raw_mm_page_alloc *ctx)
{
    __u32 key = ALLOC_COUNT;
    __u64 *count = bpf_map_lookup_elem(&mem_counters, &key);
    if (count)
        (*count)++;

    key = ALLOC_PAGES;
    __u64 *pages = bpf_map_lookup_elem(&mem_counters, &key);
    if (pages)
        (*pages) += (1 << ctx->order);

    return 0;
}

SEC("tp/kmem/mm_page_free")
int handle_page_free(struct trace_event_raw_mm_page_free *ctx)
{
    __u32 key = FREE_COUNT;
    __u64 *count = bpf_map_lookup_elem(&mem_counters, &key);
    if (count)
        (*count)++;

    key = FREE_PAGES;
    __u64 *pages = bpf_map_lookup_elem(&mem_counters, &key);
    if (pages)
        (*pages) += (1 << ctx->order);

    return 0;
}

char LICENSE[] SEC("license") = "Apache-2.0";
