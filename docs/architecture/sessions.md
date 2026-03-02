# Interactive Sessions

## Design Principle

Interactive sessions are allocations with a terminal. They reuse the standard allocation lifecycle with additional terminal protocol handling. Sessions are not a separate concept — they are bounded or unbounded allocations with an attached PTY as the primary interaction mode.

## Session Creation

A session is created via `POST /v1/sessions` (or `lattice session`):

```yaml
session:
  tenant: "ml-team"
  vcluster: "interactive"         # typically the interactive FIFO vCluster
  resources:
    nodes: 1                      # default: 1 node
    constraints:
      gpu_type: "GH200"
  lifecycle:
    type: "bounded"
    walltime: "4h"                # interactive sessions have walltime
  environment:
    uenv: "prgenv-gnu/24.11:v1"
```

Internally, the API server creates a standard `Allocation` with:
- `lifecycle.type = Bounded { walltime }`
- A flag indicating terminal should auto-attach on scheduling
- Allocation state follows the normal lifecycle (Pending → Running → Completed)

## Terminal Protocol

### Connection Setup

```
1. Client connects: POST /v1/sessions → returns session_id + allocation_id
2. Allocation is scheduled (may wait in queue)
3. Once Running, client opens terminal: GET /v1/sessions/{id}/terminal (WebSocket upgrade)
4. WebSocket connection established to lattice-api
5. lattice-api opens gRPC bidirectional stream to the node agent
6. Node agent spawns PTY + user shell in allocation's mount/network namespace
```

### Wire Protocol

The gRPC bidirectional stream carries framed messages:

**Client → Server:**

| Message Type | Content |
|-------------|---------|
| `StdinData` | Raw bytes from client terminal |
| `Resize` | Terminal dimensions (rows, cols) |
| `Signal` | SIGINT, SIGTSTP, SIGHUP, SIGQUIT |
| `Keepalive` | Heartbeat (every 30s) |

**Server → Client:**

| Message Type | Content |
|-------------|---------|
| `StdoutData` | Raw bytes from PTY (stdout + stderr merged) |
| `ExitCode` | Process exit code (terminal message) |
| `Error` | Error description (e.g., "allocation not running") |

### Initial Terminal Size

The client sends a `Resize` message as the first message after connection. The node agent configures the PTY with these dimensions. If no `Resize` is sent, defaults to 80x24.

### Signal Handling

| Signal | Client Action | Server Action |
|--------|--------------|---------------|
| SIGINT (Ctrl+C) | Send `Signal(SIGINT)` | Node agent sends SIGINT to foreground process group |
| SIGTSTP (Ctrl+Z) | Send `Signal(SIGTSTP)` | Node agent sends SIGTSTP to foreground process group |
| SIGHUP | Connection close | Node agent sends SIGHUP to session process group |
| SIGQUIT (Ctrl+\\) | Send `Signal(SIGQUIT)` | Node agent sends SIGQUIT to foreground process group |
| SIGWINCH | Send `Resize(rows, cols)` | Node agent calls `ioctl(TIOCSWINSZ)` on PTY |

## Session Lifecycle

### Active Session

While the terminal is connected:
- PTY output streams to client in real-time
- Client input streams to PTY stdin
- Keepalive every 30s to detect stale connections
- Session remains active as long as the WebSocket is open AND the shell process is alive

### Disconnect and Reconnect

**Client disconnect (network drop, laptop close):**

1. WebSocket closes (or keepalive timeout: 90s)
2. Node agent sends SIGHUP to the session's process group
3. Default behavior: processes receive SIGHUP and exit
4. If the user's shell ignores SIGHUP (e.g., `tmux`, `screen`):
   - Processes continue running in the background
   - User can reconnect: `lattice attach <alloc_id>`
   - Allocation walltime continues counting

**Deliberate detach:**

Users who want background sessions should use `tmux` or `screen` inside the session. Lattice does not implement a detach/reattach protocol — it delegates to proven tools.

### Session Timeout

| Timeout | Default | Description |
|---------|---------|-------------|
| `idle_timeout` | 30 minutes | If no stdin for this duration, warn user. No auto-kill. |
| `walltime` | User-specified | Hard deadline. SIGTERM → SIGKILL → release. |
| `keepalive_timeout` | 90s | WebSocket keepalive. Missed → treat as disconnect. |

**Idle warning:** After `idle_timeout`, the terminal displays:
```
[lattice] Warning: session idle for 30 minutes. Walltime remaining: 3h 12m.
```

No automatic termination on idle — the user may be running a long computation.

### Cleanup

When the session's allocation reaches a terminal state (Completed, Failed, Cancelled):

1. SIGTERM to all remaining processes
2. Grace period (30s)
3. SIGKILL
4. Unmount uenv, release scratch, release nodes
5. Session terminal sends `ExitCode` and closes WebSocket

### Preemption During Active Session

When a session's allocation is preempted while a terminal is connected:

1. The checkpoint sequence begins (if `checkpoint != None`)
2. The terminal remains connected during checkpointing — user sees normal output
3. When checkpoint completes and the allocation transitions to `Suspended`:
   - Server sends a terminal message: `[lattice] Allocation preempted. Session suspended. Use 'lattice attach <id>' to reconnect after rescheduling.`
   - Server sends `ExitCode(-1)` and closes the stream
4. When the allocation is rescheduled and resumes:
   - The user must manually reconnect: `lattice attach <id>`
   - The session starts a fresh shell (PTY state is not checkpointed)
   - Application state is restored from checkpoint (if the application supports it)

## Multi-Node Sessions

For sessions requesting multiple nodes:

- The terminal connects to the first node (node 0)
- The user's shell runs on node 0
- Other nodes are accessible via `ssh` (intra-allocation, uses the network domain)
- Or via `lattice attach <alloc_id> --node=<node_id>` (opens a second terminal to a specific node)

## Concurrent Attach

| Scenario | Allowed | Notes |
|----------|---------|-------|
| Same user, multiple terminals | Yes | Multiple attach sessions to the same allocation |
| Different users (non-medical) | No | Only the allocation owner can attach |
| Different users (medical) | No | Only the claiming user; one session at a time |
| Same user, different nodes | Yes | Each attach targets a specific node |

## Slurm Compatibility

| Slurm | Lattice | Notes |
|-------|---------|-------|
| `salloc -N2` | `lattice session --nodes=2` | Creates session allocation |
| `srun --jobid=123 --pty bash` | `lattice attach 123` | Attach to existing allocation |
| `salloc` then `srun` | `lattice session` then `lattice launch` | Session + task within allocation |

## CLI Usage

```bash
# Create a session (waits for scheduling, then opens terminal)
lattice session --nodes=1 --walltime=4h --uenv=prgenv-gnu/24.11:v1

# Create with specific constraints
lattice session --nodes=2 --constraint=gpu_type:GH200 --walltime=8h

# Create in a specific vCluster
lattice session --vcluster=interactive --walltime=2h

# Attach to an existing session's allocation
lattice attach 12345

# Attach to a specific node
lattice attach 12345 --node=x1000c0s0b0n3

# Attach with a specific command (not the default shell)
lattice attach 12345 --command="nvidia-smi -l 1"
```

## Cross-References

- [observability.md](observability.md) — Attach architecture, authorization model, rate limiting
- [api-design.md](api-design.md) — Session API endpoints
- [sensitive-workloads.md](sensitive-workloads.md) — Medical session constraints (one session, recording, signed uenv)
- [cli-design.md](cli-design.md) — Full CLI command reference
