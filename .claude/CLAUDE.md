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
| "where are we" / "status" | ASSESS | Read fidelity/findings indexes or inventory |
| "sweep" / "baseline" / "full audit" | SWEEP | `.claude/roles/auditor.md` |
| "adversary sweep" / "security review" / "full review" | ADV-SWEEP | `.claude/roles/adversary.md` |
| "audit [X]" | AUDIT | `.claude/roles/auditor.md` |
| "new feature" / "add" / "implement" | FEATURE | See Feature Protocol |
| "fix" / "bug" / "broken" / "error" | BUGFIX | See Bugfix Protocol |
| "design" / "spec" / "think about" | DESIGN | See Design Protocol |
| "review" / "find flaws" / "adversary" | REVIEW | `.claude/roles/adversary.md` |
| "integrate" | INTEGRATE | `.claude/roles/integrator.md` |
| "continue" / "next" | RESUME | Read SWEEP.md / ADVERSARY-SWEEP.md or current state |
| Unclear | ASK | State what you see, ask what they want |

### Step 3: State assessment

Before acting, one line:
```
Mode: [MODE]. Project: [state]. Role: [role]. Reason: [why].
```

If ambiguous, ask.

## In-session role switching

Say "audit this" → auditor. "Implement" → implementer. "Review" → adversary.
On switch: `Switching to [role]. Previous: [role].`
Read `.claude/roles/[role].md` when switching. Apply its constraints.

## Protocols

### Feature Protocol (diverge → converge → diverge → converge)

```
DESIGN: analyst → spec | architect → interfaces | adversary → gate 1
IMPLEMENT: implementer → BDD+code | auditor → gate 2 | harden until HIGH
REVIEW: adversary → findings | INTEGRATE (if cross-feature): integrator
```

Done = scenarios pass + fidelity HIGH + no DIVERGENT mocks + adversary signed off.

### Bugfix Protocol

```
1. DIAGNOSE: reproduce, check fidelity (was this area LOW?)
2. WRITE FAILING TEST FIRST: must fail before fix, pass after
3. FIX: implement, no regressions
4. AUDIT: new test THOROUGH? Deepen adjacent if area was LOW
5. UPDATE INDEX
```

### Design Protocol

```
1. New domain → analyst | 2. Architecture change → architect | 3. ADR → write it
All: adversary reviews before implementation
```

### Sweep Protocols

**Fidelity sweep** (`.claude/roles/auditor.md`): inventory → chunked assessment → checkpoint.
**Adversary sweep** (`.claude/roles/adversary.md`): attack surface → chunked review → findings index.

Run fidelity first when possible — LOW areas get higher adversary priority.
Both can run in parallel. Together: where tests are shallow AND where things are broken.

## Checkpoint

Complete fidelity snapshot: every spec has a row in INDEX.md, every trait
boundary rated, every decision record assessed, cross-cutting gaps identified,
priority actions ranked.

Checkpoint ≠ everything good. Checkpoint = everything measured.

Re-sweep when: major refactoring, >50 commits, before release, trust lost.

## Brownfield entry

```
Existing code → FIDELITY SWEEP → CHECKPOINT
             → ADVERSARY SWEEP (can overlap or follow)
             → diamond workflow
```

The fidelity sweep measures test depth. The adversary sweep finds actual
problems. Together they tell you: where tests are shallow AND where things
are broken. Highest priority fixes: areas that are both LOW confidence
and have critical/high adversary findings.

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
├── findings/
│   ├── INDEX.md
│   ├── ADVERSARY-SWEEP.md
│   └── [chunk].md
├── integration/
└── escalations/
```
