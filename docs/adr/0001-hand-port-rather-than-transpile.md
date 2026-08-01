# 1. Hand port to Rust rather than transpile

Date: 2026-08-01

## Status

Accepted

## Context

We are moving mosh from C++ to safe Rust, keeping the existing test suite green and
staying within roughly 5% of the C++ performance baseline.

An automated path exists and looks attractive at first glance. Cpp2Rust (PLDI 2026,
`github.com/Cpp2Rust/cpp2rust`) is the first tool that translates C++ into *fully safe*
Rust automatically, and it is publicly released. Cp2SRust (ISEC 2026) targets the same
problem but promises only "safer" Rust. Not using a released tool that does exactly what
the task asks for is the kind of decision a future reader will question.

Three facts decided it against us.

Cpp2Rust cannot ingest mosh as it stands. Its documented unsupported list includes
bitfields, exceptions, user-defined copy constructors, and base classes carrying fields
or non-virtual methods. Each of those is not incidental in mosh but load-bearing:
`Renditions` is three bitfields and is the per-cell attribute type; there are 83
throw/catch sites across 19 files; `Framebuffer`'s user-defined copy constructor is how
copy-on-write works; `Parser::State` has both a field and non-virtual methods. Neither
`std::shared_ptr` nor `std::list` is in its supported subset, and `shared_ptr<Row>` *is*
the framebuffer's row-sharing design. Reaching the supported subset means substantially
rewriting the C++ first — which is most of the work of a port, done twice, in the
language we are trying to leave.

The performance envelope is incompatible with our budget. Cpp2Rust attains safety by
mapping every C++ pointer onto a reference-counted, dynamically-checked smart pointer.
Their own evaluation reports 2% overhead on WOFF2 but 6x on Brunsli, with the difference
tracking how pointer- and aliasing-heavy the code is. mosh's hot loop — diffing an 80x24
grid of cells across two framebuffers that deliberately share rows — sits at the bad end
of that spectrum. A tool whose demonstrated range spans 2% to 600% is not a way to hit a
5% gate.

The output would not be the artifact we want. The paper explicitly leaves replacing its
smart-pointer type with idiomatic constructs to future work. A second, idiomatising pass
would therefore start from reference-counted pointer soup rather than from the original,
readable C++ — a worse starting point than the one we already have.

## Decision

Port by hand, in two passes per layer.

Pass one transliterates: same structure, same names, same control flow as the C++, no
redesign. It lands when the tests are green. Pass two makes that layer idiomatic, and the
tests must stay green across it.

The two passes never share a commit. A behaviour change and a design change are never in
flight at the same time, so any regression bisects to one or the other rather than to
both at once.

## Consequences

All ~17k lines are written by hand. This is the dominant cost of the project and it is
accepted deliberately.

Correctness rests on the test suite rather than on a tool's semantics-preservation
guarantee. This is why interop is required in both directions: a session between a Rust
endpoint and a C++ endpoint is an independent oracle that catches porting mistakes two
Rust endpoints would happily agree on.

We are free to choose data representations that suit Rust, so the 5% performance gate is
reachable rather than fought against.

Should Cpp2Rust's supported subset grow to cover exceptions and bitfields, this decision
is worth revisiting only for code not yet ported. Ported code stays hand-written.
