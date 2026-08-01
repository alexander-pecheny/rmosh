# 3. Safety boundary: forbid unsafe everywhere except one sys crate

Date: 2026-08-01

## Status

Accepted

## Context

The goal is safe Rust. "No unsafe anywhere" is not achievable and not meaningful: the
standard library is built on unsafe, and mosh needs a pty, termios, signal handling,
socket options that `std::net` does not expose, the locale's character-width and
multibyte-decoding functions, and the login-record database.

Two principles were adopted that turned out to collide.

The first is that every crate we write carries `#![forbid(unsafe_code)]`, so the compiler
rather than review discipline keeps unsafe out.

The second is that where mosh's C++ calls into the platform's C library, the Rust should
call the same function rather than reimplement its behaviour in Rust. This is what makes
interop with a C++ endpoint exact instead of approximate — see ADR 0002, where the two
approaches differ on 95 character widths and on whether 814,732 codepoints are printable
at all.

`nix` resolves most of this: it wraps `forkpty`, `openpty`, termios, `sigaction`,
`pselect` and socket options in safe interfaces, and its unsafe is audited far more
widely than ours would be. But `nix` has no locale module, and every `wcwidth` crate
published on crates.io is a Rust reimplementation rather than a binding. Calling glibc's
`wcwidth`, `mbrtowc`, `setlocale` and `nl_langinfo` therefore means writing the FFI
ourselves, which requires unsafe. The same is true of the login-record calls
`getutxent` and `utempter_add_record`.

## Decision

One crate, `mosh-sys`, may contain unsafe. Every other crate we write carries
`#![forbid(unsafe_code)]`.

`mosh-sys` holds only declarations nothing else wraps: the four locale functions and the
login-record functions. It exposes safe Rust signatures; callers cannot observe that FFI
happened. Each unsafe block documents the invariant that makes it sound.

Everything `nix` already covers goes through `nix`, not through `mosh-sys`. The crate is
a last resort, not a general escape hatch.

## Consequences

There is a bright line rather than a judgement call. Unsafe in any crate other than
`mosh-sys` fails to compile, so it cannot drift into the terminal or network code where
we have no reason to want it.

The audit surface is a single file of roughly a hundred lines, reviewable in one sitting
and checkable in CI by asserting unsafe appears nowhere else.

We inherit glibc's locale semantics exactly, which is the point — including its quirks
and its Unicode vintage, because the C++ endpoint we must interoperate with has the same
ones.

Writing login records ceases to be an open question: it lives here rather than being
dropped from the product.
