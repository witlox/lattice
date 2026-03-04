# Lattice eBPF programs — build with clang targeting BPF
#
# Prerequisites:
#   - clang >= 12
#   - libbpf-dev (for headers)
#   - bpftool (for generating vmlinux.h)
#
# Usage:
#   make              # build all .o files
#   make vmlinux.h    # generate kernel type header
#   make clean        # remove build artifacts
#   make install      # copy to /opt/lattice/ebpf/

CLANG ?= clang
BPFTOOL ?= bpftool
INSTALL_DIR ?= /opt/lattice/ebpf

CFLAGS := -O2 -g -target bpf -D__TARGET_ARCH_x86 \
          -Wall -Werror \
          -I.

PROGS := sched_events blk_io mem_events net_flow
OBJS  := $(addsuffix .bpf.o, $(PROGS))

.PHONY: all clean install

all: $(OBJS)

%.bpf.o: %.bpf.c vmlinux.h
	$(CLANG) $(CFLAGS) -c $< -o $@

vmlinux.h:
	$(BPFTOOL) btf dump file /sys/kernel/btf/vmlinux format c > $@

clean:
	rm -f $(OBJS)

install: $(OBJS)
	install -d $(INSTALL_DIR)
	install -m 644 $(OBJS) $(INSTALL_DIR)/
