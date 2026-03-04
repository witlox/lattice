// SPDX-License-Identifier: Apache-2.0
// Lattice eBPF program: per-flow network byte counters
//
// Attaches to tcp_sendmsg and tcp_recvmsg kprobes to track bytes
// sent and received per TCP flow (4-tuple). Flows are aggregated
// in a hash map keyed by (saddr, daddr, sport, dport).

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

struct flow_key {
    __u32 saddr;
    __u32 daddr;
    __u16 sport;
    __u16 dport;
};

struct flow_val {
    __u64 bytes_sent;
    __u64 bytes_recv;
    __u64 packets_sent;
    __u64 packets_recv;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, struct flow_key);
    __type(value, struct flow_val);
} flows SEC(".maps");

static __always_inline void get_flow_key(struct sock *sk, struct flow_key *key)
{
    BPF_CORE_READ_INTO(&key->saddr, sk, __sk_common.skc_rcv_saddr);
    BPF_CORE_READ_INTO(&key->daddr, sk, __sk_common.skc_daddr);
    BPF_CORE_READ_INTO(&key->sport, sk, __sk_common.skc_num);
    BPF_CORE_READ_INTO(&key->dport, sk, __sk_common.skc_dport);
}

SEC("kprobe/tcp_sendmsg")
int handle_tcp_sendmsg(struct pt_regs *ctx)
{
    struct sock *sk = (struct sock *)PT_REGS_PARM1(ctx);
    int size = (int)PT_REGS_PARM3(ctx);

    struct flow_key key = {};
    get_flow_key(sk, &key);

    struct flow_val *val = bpf_map_lookup_elem(&flows, &key);
    if (val) {
        __sync_fetch_and_add(&val->bytes_sent, size);
        __sync_fetch_and_add(&val->packets_sent, 1);
    } else {
        struct flow_val new_val = {
            .bytes_sent = size,
            .packets_sent = 1,
        };
        bpf_map_update_elem(&flows, &key, &new_val, BPF_NOEXIST);
    }
    return 0;
}

SEC("kprobe/tcp_recvmsg")
int handle_tcp_recvmsg(struct pt_regs *ctx)
{
    struct sock *sk = (struct sock *)PT_REGS_PARM1(ctx);
    int size = (int)PT_REGS_PARM3(ctx);

    struct flow_key key = {};
    get_flow_key(sk, &key);

    struct flow_val *val = bpf_map_lookup_elem(&flows, &key);
    if (val) {
        __sync_fetch_and_add(&val->bytes_recv, size);
        __sync_fetch_and_add(&val->packets_recv, 1);
    } else {
        struct flow_val new_val = {
            .bytes_recv = size,
            .packets_recv = 1,
        };
        bpf_map_update_elem(&flows, &key, &new_val, BPF_NOEXIST);
    }
    return 0;
}

char LICENSE[] SEC("license") = "Apache-2.0";
