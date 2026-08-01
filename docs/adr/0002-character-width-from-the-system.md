# 2. Take character width from the system, not from a Rust crate

Date: 2026-08-01

## Status

Accepted

## Context

Both endpoints of a session run a terminal emulator and each computes screen layout
independently. The Server decides which cell a character occupies when it updates the
Screen; the Client decides the same thing again when it renders and when it predicts.
Character width is therefore not an internal implementation detail — it is a shared
assumption. If the two ends disagree about how wide a character is, they disagree about
what the screen looks like, and the session desynchronises.

The C++ implementation asks the platform, via `wcwidth()` under the session's locale. The
obvious Rust choice is the `unicode-width` crate: pure Rust, no libc, and tracking a far
more recent Unicode than a typical glibc.

We measured the two against each other across all 1,112,064 codepoints on this host.

95 codepoints are printable to both but disagree on width. These are not exotic: Indic
vowel signs (U+09BE, U+0BBE, U+0D3E and similar), the halfwidth katakana voiced sound
marks U+FF9E and U+FF9F, musical notation from U+1D165, and SOFT HYPHEN. glibc calls them
width 1, the crate calls them width 0 — and width 0 is exactly the signal that means
"combine into the previous cell". The same character therefore lands in a different cell
on each endpoint. One codepoint, U+17D8, is width 3 to the crate, a value the emulator's
model has no representation for and asserts on.

814,732 codepoints are worse. glibc reports them unassigned; the crate reports them
printable. Unprintable characters are dropped outright, so a Rust endpoint would display
text that a C++ endpoint silently discards — most recently-assigned scripts and emoji.

## Decision

Take width from the system `wcwidth()`, bound through `libc` behind a safe wrapper.

## Consequences

Parity with a C++ endpoint on the same host is exact by construction, on every codepoint,
rather than exact on the codepoints we thought to test.

Behaviour varies with the host's glibc version. This is not a regression: it is precisely
the behaviour C++ mosh already has, and inheriting it is the point. Two hosts with
different glibc versions could disagree — but they disagree today, between two C++
endpoints, and we are not making that worse.

The terminal layer depends on the process locale being set before any width query. mosh
already requires and asserts a UTF-8 locale at startup, so this adds an ordering
constraint rather than a new requirement.

This is a deliberate vote for compatibility over standards-correctness. If interop with
C++ endpoints is ever dropped as a requirement, `unicode-width` becomes the better choice
and this decision should be revisited.
