# Performance

The goal for the Rust port was to stay within roughly 5% of the C++.

## What is measured

`third_party/mosh/src/examples/benchmark.cc` and `crates/mosh-client/examples/benchmark.rs` do the same
thing, one iteration per keystroke: predict the character, take the server's screen, lay
the predictions over it, and compute the frame that turns the previous screen into the
new one. That is the client's hot path — the work done between a key being pressed and
something appearing on screen.

Nothing else is measured. Crypto, compression and the terminal parser are all off this
path and are exercised only by the test suite.

## Result

Twelve runs each, 100000 iterations at 80x24, same machine, both optimised:

|      | min    | median | mean   | peak RSS |
|------|--------|--------|--------|----------|
| C++  | 5.48 s | 5.61 s | 5.65 s | 470 MB   |
| Rust | 3.39 s | 3.48 s | 3.47 s | 328 MB   |

Rust is about 38% faster and uses about 30% less memory.

## Why it is reported this way

Timings are min and median over repeated runs, not a single measurement. The noise on
this machine is 5.6% standard deviation and 16.5% spread between fastest and slowest
run — larger than the 5% tolerance being tested, so any single timing could have shown
almost anything.

Memory is reported because it is the check that the two are doing comparable work. Both
accumulate unconfirmed predictions over the run, which is what drives the footprint into
hundreds of megabytes. A speedup won by quietly skipping work would show up as a much
smaller footprint, not a modestly smaller one.

## Where the difference plausibly comes from

Not investigated, because the result is comfortably on the right side of the target.
Two things were expected to cost us and evidently did not dominate: `Renditions` lost the
bitfield packing it had in C++ (three fields rather than three bit-ranges in a `uint64_t`,
across 80x24 cells in two framebuffers), and `Cell` holds a `String` where the C++ held a
tuned `std::string`.

If a future change regresses this, those two are the first places to look.
