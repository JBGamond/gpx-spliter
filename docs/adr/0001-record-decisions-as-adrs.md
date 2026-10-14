# ADR-0001: Record decisions as ADRs

[← Back to index](../README.md)

- **Status:** Accepted
- **Date:** 2026-10-14
- **Decider:** jbgamond

## Context
AI assistants work on this repo. Choices made without the owner's input or a written record are hard to audit later.

## Options considered
1. No formal record (decisions live in commits/chat).
2. ADRs in `docs/`: index file plus one file per ADR.

## Decision
Option 2. The assistant consults jbgamond before any decision with alternatives, then records it as `docs/adr/NNNN-kebab-title.md` and adds a row to [docs/README.md](../README.md). Each ADR has: Status, Date, Decider, Context, Options considered, Decision, Consequences. Numbers are sequential and never reused; superseded ADRs stay and link to their replacement.

## Consequences
- Every notable change carries a traceable rationale.
- Small overhead per decision: one file plus one index row.
