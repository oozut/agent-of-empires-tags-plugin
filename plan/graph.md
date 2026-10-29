# Plan graph

Mermaid views of the plan: the ladder, one dependency graph per milestone, the M0 lane
swimlanes, and a relative-effort schedule. Source of truth is the task tables in `mN.md` and the
priority/lane rules in `README.md` — if a diagram here disagrees with them, the diagram is wrong.

**Legend**

| Mark | Meaning |
|---|---|
| `P0` red / `P1` amber / `P2` grey | Priority: P0 blocks the milestone's acceptance, P1 needed to use it for real, P2 may slip |
| node text `M0-T07 · wire · S` | Task ID, lane, size (S ≤2 h, M ≤½ day, L ≈1 day) |
| solid arrow | Hard dependency: the target task cannot start first |
| dashed arrow | Cross-milestone entry: the gate, not a task |
| `wave N` | Topological level — everything in a wave is unblocked at the same time. Same-lane tasks inside one wave are sequential in practice. |

## 1. The ladder

```mermaid
flowchart LR
    M0["M0 · prove a header arrives<br/>9 P0 · gate"]:::p0
    M1["M1 · settings + attribution UI<br/>4 P0"]:::p0
    M2["M2 · discovery + wiring<br/>5 P0"]:::p0
    M3["M3 · usage views<br/>3 P0"]:::p0
    M4["M4 · harnesses + distribution<br/>2 P0"]:::p0

    M0 -->|acceptance| M1
    M1 -->|acceptance| M2
    M2 -->|acceptance| M3
    M0 -.->|rows + publish path run in parallel| M4

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
```

Priority never reorders the ladder. It only orders work inside one box — and it lets the M4 lane
start from M0 while M1–M3 march in sequence.

## 2. M0 — prove a header arrives

```mermaid
flowchart LR
    subgraph W0["wave 0 · ready now"]
        direction TB
        T01["M0-T01 · core · S<br/>scaffold"]:::p0
    end
    subgraph W1["wave 1"]
        direction TB
        T02["M0-T02 · core · M<br/>CLI core: resolve → render"]:::p0
    end
    subgraph W2["wave 2 · split into lanes"]
        direction TB
        T03["M0-T03 · core · M<br/>resolver tests"]:::p0
        T05["M0-T05 · core · S<br/>setup --print"]:::p0
        T11["M0-T11 · core · M<br/>doctor"]:::p0
    end
    subgraph W3["wave 3"]
        direction TB
        T06["M0-T06 · wire · S<br/>install both hook keys"]:::p0
        T07["M0-T07 · wire · S<br/>static harness mapping"]:::p0
        T04["M0-T04 · core · S<br/>doctor tests"]:::p1
        T12["M0-T12 · docs · S<br/>install page"]:::p1
    end
    subgraph W4["wave 4 · the two proofs"]
        direction TB
        T08["M0-T08 · proof · S<br/>sink run"]:::p0
        T09["M0-T09 · proof · S<br/>consumer run"]:::p0
        T10["M0-T10 · wire · S<br/>second header"]:::p1
    end

    T01 --> T02
    T02 --> T03
    T02 --> T05
    T02 --> T11
    T05 --> T06
    T05 --> T07
    T05 --> T12
    T06 --> T08
    T07 --> T08
    T06 --> T09
    T07 --> T09
    T07 --> T10
    T11 --> T04

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
```

**Critical path:** `T01 → T02 → T05 → T06/T07 → T08/T09`. Everything else hangs off `T02` or off
the host preconditions. `T08` and `T09` are two independent proofs of the same wiring — the sink
(mechanism) and the real consumer (delivery) — and must not be collapsed into one task.

## 3. M1 — settings and attribution UI

```mermaid
flowchart LR
    subgraph W0["wave 0"]
        T01["M1-T01 · ui · S<br/>manifest: grants, settings, slots"]:::p0
        T02["M1-T02 · core · M<br/>worker: JSON-RPC + status"]:::p0
    end
    subgraph W1["wave 1"]
        T03["M1-T03 · ui · M<br/>settings UI"]:::p0
        T06["M1-T06 · core · S<br/>status over sessions.list"]:::p1
    end
    subgraph W2["wave 2"]
        T04["M1-T04 · ui · S<br/>row badge"]:::p0
        T05["M1-T05 · ui · S<br/>pane"]:::p1
        T07["M1-T07 · core · S<br/>hook == status == value"]:::p1
        T08["M1-T08 · ui · S<br/>lifecycle invalidation"]:::p2
    end

    M0g["M0 accepted"]:::gate -.-> T01
    M0g -.-> T02
    T01 --> T03
    T02 --> T03
    T02 --> T06
    T03 --> T04
    T03 --> T05
    T02 --> T07
    T06 --> T07
    T03 --> T08

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
    classDef p2 fill:#eeeeee,stroke:#888,color:#111
    classDef gate fill:#ffffff,stroke:#c0392b,stroke-dasharray:4 3,color:#c0392b
```

## 4. M2 — discovery and wiring

```mermaid
flowchart LR
    T01["M2-T01 · core · S<br/>per-adapter path resolution"]:::p0
    T02["M2-T02 · core · M<br/>classification, 5 verdicts"]:::p0
    T03["M2-T03 · core · M<br/>diff → apply"]:::p0
    T04["M2-T04 · core · S<br/>undo"]:::p0
    T05["M2-T05 · proof · M<br/>verify"]:::p0
    T06["M2-T06 · ui · S<br/>pane actions"]:::p1
    T07["M2-T07 · adapters · M<br/>two more mechanisms"]:::p1
    T08["M2-T08 · adapters · S<br/>capability probe"]:::p2

    M1g["M1 accepted"]:::gate -.-> T01
    M0T07["M0-T07 mapping"]:::gate -.-> T07
    T01 --> T02
    T01 --> T03
    T03 --> T04
    T03 --> T05
    T02 --> T05
    T04 --> T06
    T05 --> T08

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
    classDef p2 fill:#eeeeee,stroke:#888,color:#111
    classDef gate fill:#ffffff,stroke:#888,stroke-dasharray:4 3,color:#444
```

`T02` (does it look wired?) and `T03` (write it) both build on `T01` but touch different code, so
they can be split across two workers. `T04` is P0 even though it looks like cleanup: acceptance #2
is byte-identical `undo`, and that is the check that keeps the writer honest.

## 5. M3 — usage views

```mermaid
flowchart LR
    T01["M3-T01 · core · L<br/>consumer adapter interface"]:::p0
    T02["M3-T02 · ui · M<br/>page board"]:::p0
    T03["M3-T03 · ui · S<br/>honest degradation"]:::p0
    T04["M3-T04 · ui · S<br/>pane cost"]:::p1
    T05["M3-T05 · ui · S<br/>blocks"]:::p1
    T06["M3-T06 · ui · M<br/>richer rules"]:::p2

    M2g["M2 accepted"]:::gate -.-> T01
    M1T03["M1-T03 settings UI"]:::gate -.-> T06
    T01 --> T02
    T01 --> T03
    T02 --> T04
    T02 --> T05

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
    classDef p2 fill:#eeeeee,stroke:#888,color:#111
    classDef gate fill:#ffffff,stroke:#888,stroke-dasharray:4 3,color:#444
```

## 6. M4 — harnesses and distribution

```mermaid
flowchart LR
    T01["M4-T01 · adapters · S<br/>honest status per row"]:::p0
    T02["M4-T02 · release · S<br/>install from published source"]:::p0
    T03["M4-T03 · adapters · L<br/>verify rows"]:::p1
    T04["M4-T04 · release · M<br/>worker binary"]:::p1
    T05["M4-T05 · release · S<br/>versioning + aoe_version"]:::p1
    T06["M4-T06 · release · S<br/>featured index"]:::p2

    M0T01["M0-T01 scaffold"]:::gate -.-> T02
    M1T02["M1-T02 worker"]:::gate -.-> T04
    T01 --> T03
    T02 --> T05
    T02 --> T06
    T05 --> T06

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
    classDef p2 fill:#eeeeee,stroke:#888,color:#111
    classDef gate fill:#ffffff,stroke:#888,stroke-dasharray:4 3,color:#444
```

`T01` and `T02` have no dependency on M1–M3: the table can be made honest and the install path can
be proven today, which is why M4 is the one milestone that genuinely overlaps M0.

## 7. M0 by lane — what can actually run at once

```mermaid
flowchart TB
    subgraph core["lane core — src/, resolver, CLI verbs"]
        direction TB
        T01["M0-T01 · S"]:::p0 --> T02["M0-T02 · M"]:::p0
        T02 --> T03["M0-T03 · M"]:::p0
        T02 --> T11["M0-T11 · M"]:::p0
        T11 --> T04["M0-T04 · S"]:::p1
    end
    subgraph wire["lane wire — aoe + harness config"]
        direction TB
        T05["M0-T05 · S"]:::p0 --> T06["M0-T06 · S"]:::p0
        T05 --> T07["M0-T07 · S"]:::p0
        T07 --> T10["M0-T10 · S"]:::p1
    end
    subgraph proof["lane proof — runs and records only"]
        direction TB
        T08["M0-T08 · sink · S"]:::p0
        T09["M0-T09 · consumer · S"]:::p0
    end
    subgraph docs["lane docs"]
        direction TB
        T12["M0-T12 · S"]:::p1
    end

    T02 --> T05
    T06 --> T08
    T06 --> T09
    T07 --> T08
    T07 --> T09
    T05 --> T12

    classDef p0 fill:#ffe0e0,stroke:#c0392b,color:#111
    classDef p1 fill:#fff3cd,stroke:#b8860b,color:#111
```

Two workers: one in `core` (T02 → T03/T11) and one in `wire` (T05 → T06/T07 → T08/T09). They meet
only at the single arrow `T02 → T05`, which is the hook line's command string — so the `wire`
worker can start as soon as `setup --print` exists, and never has to rebase onto `bin/`.

## 8. Relative-effort schedule

Units are relative effort (S = 1, M = 2, L = 4), not calendar days. The ladder is sequential; the
overlap between M0 and M4's release lane is the only cross-milestone concurrency.

```mermaid
gantt
    title P0-first schedule (relative units)
    dateFormat YYYY-MM-DD
    axisFormat %d
    todayMarker off

    section M0 core
    T01 scaffold            :t01, 2026-01-01, 1d
    T02 CLI core            :t02, after t01, 2d
    T03 resolver tests      :t03, after t02, 2d
    T11 doctor              :t11, after t02, 2d
    T04 doctor tests (P1)   :t04, after t11, 1d

    section M0 wire
    T05 setup --print       :t05, after t02, 1d
    T06 hook keys           :t06, after t05, 1d
    T07 harness mapping     :t07, after t05, 1d
    T10 second header (P1)  :t10, after t07, 1d

    section M0 proof
    T08 sink run            :t08, after t07, 1d
    T09 consumer run        :t09, after t08, 1d
    T12 install page (P1)   :t12, after t07, 1d

    section M4 (parallel)
    T01 row statuses        :m4a, 2026-01-01, 1d
    T02 publish path        :m4b, after t01, 1d
    T03 verify rows (P1)    :m4c, after m4a, 4d

    section M1
    T01 manifest            :m11, after t09, 1d
    T02 worker              :m12, after t09, 2d
    T03 settings UI         :m13, after m12, 2d
    T04 badge               :m14, after m13, 1d

    section M2
    T01 path resolution     :m21, after m14, 1d
    T02 classification      :m22, after m21, 2d
    T03 diff and apply      :m23, after m21, 2d
    T04 undo                :m24, after m23, 1d
    T05 verify              :m25, after m24, 2d

    section M3
    T01 adapter interface   :m31, after m25, 4d
    T02 page board          :m32, after m31, 2d
```

## What to check in these diagrams

1. **No P0 depends on a P1.** Follow every arrow into a red node: the source must be red. (The one
   amber arrow into red — `T11 → T04` — is a P1 depending on a P0, which is the allowed direction.)
2. **Every wave-0 node has no arrow in** except a dashed milestone gate. If a task has no arrow in
   but is not in wave 0, the table's `Depends` cell is missing something.
3. **The two proofs are separate.** `T08` and `T09` share all their inputs and prove different
   things; merging them hides which leg failed.
4. **The lane crossing is a single edge.** In M0 the only `core → wire` arrow is `T02 → T05`. If a
   change adds a second one, the split has broken down and the tasks should be re-cut.
