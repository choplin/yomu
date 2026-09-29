# Documentation Policy

Everything under `docs/` is written for developers changing Raden. User-facing
installation, configuration, and operation belong in the top-level
[`README`](../README.md), not here.

This file defines how `docs/` is organized. It does not list or summarize
individual documents; adding or removing a document must not require editing
this file.

## Top level: the mental model

Top-level documents explain the architecture developers need to hold in mind:
the important parts, their responsibilities, their relationships, and the
constraints between them. [`architecture.md`](architecture.md) is the entry
point. Every other top-level document covers one coherent architectural unit
and is reachable by following links from that entry point.

A top-level document is deliberately not exhaustive. It contains enough detail
to predict where behavior belongs and how a change crosses boundaries. Exact
procedures, edge cases, rejected alternatives, and implementation-level
contracts belong under `design/` once the larger shape is understandable.

Top-level documents describe established current architecture. Proposed work,
migration status, and temporary gaps belong in the work tracker rather than in
this directory.

## `design/`: one design question per file

Each file under [`design/`](design/) answers one question that could reasonably
have been answered another way. It owns the precise current rule, its rationale,
rejected alternatives, and only the history needed to explain that rule.

Split files by design question, not by source module or work item. One design
topic may constrain several modules, and one architectural unit may link to
several design topics. When a decision changes, rewrite the design document so
it continues to describe the present rule.

## `decision-log.md`: history

[`decision-log.md`](decision-log.md) records decisions newest first. Each entry
states what changed and why, then links to the document that owns the current
rule. It does not duplicate implementation details or the complete rationale.

## Single source of truth

Give every settled claim one canonical home. A top-level document may summarize
a design rule at the depth needed for the mental model and link to its design
document. The design document owns the exact contract. The decision log owns
chronology. Source comments own details that are useful only while changing one
implementation.
