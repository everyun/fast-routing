# Parallel Connection Routing (T3 — Optimistic Concurrency with Deterministic In-Order Commit)

This document specifies the design, conflict detection model, transactional commit protocol, and correctness guarantees of **T3 concurrent connection routing** in `fast-routing`.

This is a **declared divergence** from upstream Java Freerouting 2.3.0 (`docs/PARITY-DIVERGENCES.md`), not a transcription of upstream source code. Upstream's route pass is strictly sequential.

---

## 1. Background & Problem Statement

In upstream Freerouting, the autoroute pass (`autoroute_pass`) accounts for ~73% of total runtime across the benchmark suite. Prior parallelisation tiers (T1 portfolio pass racing, T1 optimizer candidate evaluation, T2 referee scanning) do not accelerate the inner sequential route pass itself:
- **T1 Portfolio**: Races independent route passes with different item shuffles on board clones. It improves routing quality at equal wall clock on budget-starved boards, but does not reduce the critical path of a single pass.
- **T1 Optimizer**: Evaluates reroute candidates in parallel windows, but kept items are re-routed on master.
- **T2 Read-only Fan-out**: Parallelises DRC violation scanning and ratsnest updates, but Amdahl's law bounds overall speedup while the route pass remains single-threaded.

On multi-core architectures (e.g., Apple M3 Ultra with 28 cores), a single autoroute pass presents a work-list of dozens to hundreds of unconnected connection items (`WorkItem`). The majority of these connections are **spatially disjoint** across the board surface and do not interact in maze room expansion or copper placement.

**T3 Goal**: Achieve $\ge 3\times$ speedup on the route pass on 8 worker threads for medium/large boards (Issue039, Issue269, Issue157, Issue102, Issue230) without violating design rules or regressing incomplete connection counts.

---

## 2. Optimistic Concurrency Model

### 2.1 Batching and Windowing
Given $N$ parallel connection workers (`--parallel-connections N`, default 1):
1. The work-list is divided into batches of size $K = \min(2N, \text{remaining items})$.
2. At the beginning of each batch, the master board state is snapshotted as $B_0$.
3. All $K$ candidate connections $[w_0, w_1, \dots, w_{K-1}]$ are evaluated concurrently on worker clones of $B_0$.

### 2.2 Worker Execution on Clones
Each worker thread receives an independent clone of $B_0$ (or a reset worker board) and runs the complete autoroute pipeline for one `WorkItem`:
- Component connectivity lookup (`connected_set`, `unconnected_set`).
- Autoroute control construction (`AutorouteControl`).
- Maze graph search (`MazeSearch::find_connection`).
- Path location & pin-snapping (`locate`).
- Trial insertion and pull-tight optimization (`insert_connection` + `PullTightAlgo::opt_changed_area`).

The worker produces a `ParallelCandidate` containing:
- The attempt state (`AttemptState`: `Routed`, `NotRouted`, `NoUnconnectedNets`, `ConnectedToPlane`, `InsertError`).
- The located geometry (`LocatedConnection`, `AutorouteControl`, `bounds`).
- **Read / Search Footprint ($R_i$)**: The 2D bounding box covering the search bounds, start and destination items, and located trace/via coordinates, enlarged by the maximum clearance requirement.
- **Write Footprint ($W_i$)**: The 2D bounding box covering all newly placed traces and vias, shoved items, and the pull-tight `ChangedArea`, enlarged by the clearance class radius.
- **Ripped / Modified Set ($M_i$)**: Whether any items were ripped up (`ripped_items`), and the set of existing item IDs that were modified.
- Clock and resource usage metrics.

---

## 3. Deterministic In-Order Commit Protocol

The master thread reviews worker candidates in **strict original work-list order** $i = 0, 1, \dots, K-1$.

Let $\mathcal{W}_{<i} = \bigcup_{j < i, \text{ committed}} W_j$ be the cumulative write footprint of all previously committed items in the current batch, and $\mathcal{M}_{<i}$ be the set of all modified/ripped item IDs.

For candidate $i$:

### 3.1 Skip States
If candidate $i$ returned `NoUnconnectedNets` or `ConnectedToPlane`:
- Master verifies if the net is still fully connected on the live board.
- If verified, master marks the item skipped and proceeds to $i+1$.

### 3.2 NotRouted / InsertError
If candidate $i$ failed to find a route on $B_0$:
- If $R_i \cap \mathcal{W}_{<i} = \emptyset$: No committed copper was added in candidate $i$'s search region. The board in candidate $i$'s search envelope is identical to $B_0$, so running sequentially on the live board would produce the identical failure. The failure is recorded immediately without re-search.
- If $R_i \cap \mathcal{W}_{<i} \ne \emptyset$: The committed copper altered the local obstacle field. Master discards the parallel verdict and executes a serial attempt on the live board.

### 3.3 Routed Candidates (Commit vs Conflict)
For a candidate $i$ with `AttemptState::Routed`:

1. **Conflict Detection**:
   - **Ripup Conflict**: If candidate $i$ ripped up existing copper ($|M_i| > 0$) and $\mathcal{W}_{<i} \ne \emptyset$, it is flagged as conflicting (ripup alters shared net structures and obstacle graphs).
   - **Spatial Overlap**: If $R_i \cap \mathcal{W}_{<i} \ne \emptyset$ or $W_i \cap \mathcal{W}_{<i} \ne \emptyset$, candidate $i$ overlaps with prior committed copper $\to$ conflict.
   - **Modified Item Conflict**: If candidate $i$ touches or connects to any item in $\mathcal{M}_{<i}$ $\to$ conflict.

2. **On Conflict**:
   - The parallel result is discarded.
   - Master executes candidate $i$ **serially** on the live master board (`autoroute_item`).
   - If the serial run succeeds, its write footprint is added to $\mathcal{W}_{<i}$ and $\mathcal{M}_{<i}$.

3. **On No Conflict**:
   - Candidate $i$ is spatially disjoint from all previous commits in this batch.
   - Master opens a board transaction (`let mark = board.begin()`).
   - Master executes `insert_connection(board, rules, &ctrl, &located, bounds)` followed by `PullTightAlgo::opt_changed_area`.
   - The symmetric DRC clearance oracle verifies the insertion. If insertion succeeds and leaves live copper:
     - Master commits the transaction (`board.commit(mark)`).
     - Master updates $\mathcal{W}_{<i} \gets \mathcal{W}_{<i} \cup W_i$ and $\mathcal{M}_{<i} \gets \mathcal{M}_{<i} \cup M_i$.
   - If insertion fails or rejects:
     - Master rolls back the transaction (`board.rollback_to(mark)`).
     - Master executes candidate $i$ serially on the master board.

---

## 4. Correctness Invariants & Safety Guarantees

1. **Zero Added Violations**: Every commit (whether parallel or serial fallback) passes through `insert_connection` and `PullTightAlgo::check_trace_shape` under the DRC clearance oracle with exact integer and symmetric clearance tests.
2. **Deterministic Output**: For any fixed thread count $N$, batch partitioning, candidate execution, and commit decisions are deterministic pure functions of the input geometry. Running twice with the same $N$ produces bit-identical board copper and SES files.
3. **No Orphan Copper**: Failed insertions or conflicted attempts execute within transactional `BoardMark` scopes that roll back completely on failure.
4. **Graceful Fallback**: Any conflict, ripup ambiguity, or insertion error falls back immediately to sequential routing on the live board, guaranteeing that routing capability is monotonic with respect to single-threaded execution.
