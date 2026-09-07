# Remote Benchmarking Hosts

To prevent local CPU contention and ensure repeatable, hermetic testing, test sweeps (24-board unique, 46-board main suite, and 9-board OOS) can be offloaded to dedicated remote Debian servers.

## Available Hosts

| Host Alias | Address | CPU Cores | RAM | Default Concurrency ($W$) | Role / Workload Policy |
|---|---|---|---|---|---|
| **orca** | per ops notes | 80 cores (`x86_64`) | 125 GiB | 8 | Dedicated primary test runner / GPU experiments |
| **salmon** | per ops notes | 64 cores (`x86_64`) | 125 GiB | 8 | Secondary test runner (also runs k8s / VMs) |
| **whale** | per ops notes | 8 cores (`x86_64`) | 251 GiB | 4 | Tertiary test runner (also runs NAS / ZFS / k8s) |

> **Critical Host Safety Rule**: Never kill or interfere with existing non-fast-routing services or Docker containers on remote hosts. All fast-routing operations must remain inside `~/fr/` (local NVMe), and remote jobs must run with `nice -n 10`.

---

## Toolchain & Remote Workflow

All scripts are located in `bench/tools/remote/`:

- `push.sh <worktree_or_commit> [--host orca|salmon|whale|<ip>]`
  Pushes code to the target remote bare repository (`~/fr/fast-routing.git`), preserving the branch name.
- `quality.sh <worktree_or_commit> [--host orca|salmon|whale|<ip>] [--suite 24|46|oos] [--mp 20] [--job-timeout 285] [--doors on|off|default] [-w <workers>]`
  Pushes code, builds `fr-cli` and `g21-referee` in isolated remote per-branch target directories (`~/fr/targets/<branch>`), executes the suite with $W$ workers, runs `g21_judge.py` against reference sessions, and rsyncs `gate.json`, `sweep.json`, `summary.txt`, and per-board `*.log` back to the Mac (`~/fr-work/logs/remote/<tag>/`).
- `java_ref.sh [--host orca|salmon|whale|<ip>] [--suite 24|46|oos] [--mp 20] [--job-timeout 285] [-w <workers>]`
  Runs the pinned upstream `freerouting-2.3.0.jar` on the remote host with a fresh, clean `--user_data_path` per board to prevent settings leakage.

---

## Single-Core Relative Performance

Measured under single-thread execution (`-mp 1 --no-optimizer`, in-tool routing duration):

| Host | Issue026-J2 (in-tool s) | Issue155-CH376 (in-tool s) | Single-Core Relative Speed |
|---|---|---|---|
| **salmon** | 0.77 s | 3.06 s | **1.35x** |
| **orca** | 1.07 s | 4.12 s | **1.00x** (baseline) |
| **whale** | 1.42 s | 5.58 s | **0.74x** |

### Cross-Host 285 s Deadline Incomparability

> **Important**: Under fixed wall-clock deadlines (`--job-timeout 285`), the number of completed unrouted connections ($u$) on unfinished timeout boards is **strictly hardware- and frequency-dependent** and cannot be compared across different host architectures.
> 
> Only converged boards (boards finishing before the deadline) are deterministic and byte-identical across machines. For timeout boards, comparisons must strictly be evaluated against baselines taken on the *same machine*.
