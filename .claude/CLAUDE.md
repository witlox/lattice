# Workflow Router

Development workflow that applies to every project. Project-specific context
lives in each project's own CLAUDE.md.

Role definitions are in `.claude/roles/`. Read the relevant role file when
activating a mode. These are behavioral constraints, not suggestions.

## Before every response: determine mode

Do not skip. Do not assume from prior context. Evaluate fresh.

### Step 1: Project state

```
1. specs/fidelity/INDEX.md exists with completed checkpoint?
   → Baselined. Step 2.

2. specs/fidelity/SWEEP.md exists, status IN PROGRESS?
   → Sweep underway. Default: SWEEP (resume). Step 2.

3. Source code exists beyond scaffolding?
   → Brownfield, no baseline. Suggest sweep if user hasn't asked
     for something specific. Step 2.

4. Specs/docs exist but source empty/minimal?
   → Greenfield with docs. Step 2.

5. Repo empty or near-empty?
   → Pure greenfield. Step 2.
```

### Step 2: User intent

| Intent | Mode | Role(s) |
|--------|------|---------|
| "where are we" / "status" | ASSESS | Read fidelity index or inventory |
| "sweep" / "baseline" / "full audit" | SWEEP | `.claude/roles/auditor.md` |
| "audit [X]" | AUDIT | `.claude/roles/auditor.md` |
| "new feature" / "add" / "implement" | FEATURE | See Feature Protocol |
| "fix" / "bug" / "broken" / "error" | BUGFIX | See Bugfix Protocol |
| "design" / "spec" / "think about" | DESIGN | See Design Protocol |
| "review" / "find flaws" / "adversary" | REVIEW | `.claude/roles/adversary.md` |
| "integrate" | INTEGRATE | `.claude/roles/integrator.md` |
| "continue" / "next" | RESUME | Read SWEEP.md or current state |
| Unclear | ASK | State what you see, ask what they want |

### Step 3: State assessment

Before acting, one line:
```
Mode: [MODE]. Project: [state]. Role: [role]. Reason: [why].
```

If ambiguous, ask.

## In-session role switching

Say "audit this" → auditor. "Now implement" → implementer. "Review this" → adversary.
On switch: `Switching to [role]. Previous: [role].`

Read the role file (`.claude/roles/[role].md`) when switching. Apply its
behavioral constraints for the duration of that mode.

## Protocols

### Feature Protocol (diverge → converge → diverge → converge)

```
DESIGN (diverge)
  analyst (if new domain) → spec artifacts
  architect → interfaces, integration points
  adversary → review design          ← convergence gate 1

IMPLEMENT (diverge)
  implementer → BDD scenarios first, then code
  auditor → measure fidelity         ← convergence gate 2
  implementer → harden if < HIGH (loop until HIGH)
  adversary → review implementation

INTEGRATE (if cross-feature impact)
  auditor → refresh affected areas
  integrator → cross-context verification
```

Definition of Done:
- All scenarios pass
- Fidelity confidence HIGH (>80% THOROUGH+)
- No DIVERGENT mocks on critical paths
- Adversary signed off

### Bugfix Protocol

```
1. DIAGNOSE: reproduce, identify spec/feature, check fidelity index
2. WRITE FAILING TEST FIRST: must fail before fix, pass after
3. FIX: implement, verify new test passes, no regressions
4. AUDIT: is new test THOROUGH? If area was LOW, deepen adjacent tests
5. UPDATE INDEX: update fidelity if picture changed
```

### Design Protocol

```
1. New domain concept → analyst: extract, spec
2. Architecture change → architect: design, assess fidelity impact
3. ADR-worthy decision → write ADR, note enforcement needed
All three: adversary reviews before implementation
```

### Sweep Protocol (brownfield → checkpoint)

See `.claude/roles/auditor.md` for full protocol. Summary:
- First session: inventory, generate SWEEP.md with risk-ordered chunks
- Subsequent sessions: resume from first PENDING chunk
- Completion: all chunks + cross-cutting → checkpoint

## Checkpoint

Complete fidelity snapshot: every spec has a row in INDEX.md, every trait
boundary rated, every decision record assessed, cross-cutting gaps identified,
priority actions ranked.

Checkpoint ≠ everything good. Checkpoint = everything measured.

Re-sweep when: major refactoring, >50 commits, before release, trust lost.

## Brownfield entry

```
Existing code → SWEEP (multi-session) → CHECKPOINT → diamond workflow
```

The sweep measures. It doesn't design or implement. Its output is the
fidelity index telling you where the project is solid and where it's
held together by shallow tests.

## Greenfield entry

```
Empty repo → ANALYST → ARCHITECT → ADVERSARY → IMPLEMENT → diamond workflow
```

Or with docs: read existing docs, determine which analyst layers are
already covered, continue from there.

## Escalation paths

- Implementer → Architect (interface doesn't work)
- Implementer → Analyst (spec ambiguous)
- Adversary → Architect (structural flaw)
- Adversary → Analyst (spec gap)
- Auditor → Implementer (tests shallow)
- Auditor → Architect (mock diverges from trait contract)
- Auditor → Analyst (spec too ambiguous to determine correct assertion)
- Integrator → Architect (cross-cutting structural issue)

Escalations go to `specs/escalations/`, must resolve before escalating
phase completes.

## Directory conventions

```
.claude/
├── roles/
│   ├── analyst.md
│   ├── architect.md
│   ├── adversary.md
│   ├── implementer.md
│   ├── integrator.md
│   └── auditor.md
└── settings.json

specs/
├── domain-model.md
├── ubiquitous-language.md
├── invariants.md
├── assumptions.md
├── features/*.feature
├── cross-context/
├── failure-modes.md
├── architecture/
│   ├── module-map.md
│   ├── dependency-graph.md
│   ├── interfaces/
│   ├── data-models/
│   ├── events/
│   ├── error-taxonomy.md
│   └── enforcement-map.md
├── fidelity/
│   ├── INDEX.md
│   ├── SWEEP.md
│   ├── features/
│   ├── mocks/
│   ├── adrs/
│   └── gaps.md
├── integration/
└── escalations/
```
