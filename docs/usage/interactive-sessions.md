# Interactive Sessions

Interactive sessions give you a terminal attached to allocated compute nodes — similar to `salloc` + `srun --pty` in Slurm.

## Creating a Session

```bash
# Basic interactive session (1 node, 4 hours)
lattice session --walltime=4h

# With GPU and software environment
lattice session --nodes=1 --constraint="gpu_type=GH200" --uenv=prgenv-gnu/24.11:v1

# Specify vCluster
lattice session --vcluster=interactive --walltime=2h
```

The session enters the queue like any other allocation. Once scheduled, your terminal automatically attaches to the first node.

## Attaching to Running Allocations

You can attach a terminal to any running allocation (not just sessions):

```bash
# Attach to a running job
lattice attach a1b2c3d4

# Attach to a specific node in a multi-node allocation
lattice attach a1b2c3d4 --node=nid001234

# Run a specific command instead of a shell
lattice attach a1b2c3d4 -- htop
```

## Multiple Terminals

You can open multiple terminals to the same allocation:

```bash
# Terminal 1
lattice attach a1b2c3d4

# Terminal 2 (different shell window)
lattice attach a1b2c3d4
```

## Session Lifecycle

- **Pending** — waiting in the queue for resources
- **Running** — terminal is attached, you're working
- **Disconnected** — if you lose connection, the session keeps running (use `tmux`/`screen` inside for persistence)
- **Completed** — walltime expired or you exited

## Tips

- Use `tmux` or `screen` inside your session for disconnect resilience
- Sessions respect the same preemption rules as batch jobs — use `--preemption-class=7` for important interactive work
- If preempted, you'll see checkpoint progress in your terminal before disconnection
- The `--walltime` flag is mandatory for sessions (prevents runaway resource usage)
