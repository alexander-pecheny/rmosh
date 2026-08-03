# Agents

## Upstream is a read-only submodule

Development happens in `crates/`. Upstream mosh sits unmodified at `third_party/mosh`,
tracking <https://github.com/mobile-shell/mosh>, and exists so the port can be tested
against the implementation it has to stay compatible with. Bump it with
`git submodule update --remote` and expect the differential tests to say what changed.

Never commit a change inside the submodule, and never carry a patch for it in this
repository. What we do keep is the test scaffolding upstream has no reason to hold: the
dump helpers in `tests/cpp`, and `endpoint-override.patch`, which lets upstream's own
shell tests be pointed at the Rust binaries.

Where the two implementations differ on purpose, the difference belongs in the
differential test as an explicit exception, so that everything else stays strict:

- Colour queries (`OSC 4;n;?`, `OSC 10..19;?`) and clipboard queries are forwarded to
  the client's terminal here. Upstream drops them.
- SGR 2 (faint) and SGR 9 (strikethrough) are implemented here. Upstream parses neither.
