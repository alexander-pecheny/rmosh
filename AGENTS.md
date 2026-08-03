# Agents

## The C++ tree is frozen

Development happens in `crates/`. The C++ tree under `src/` is kept only so a session
can be run with a C++ endpoint on one side and a Rust endpoint on the other, which is
how the port is checked.

Do not port fixes or features back into `src/`. A change there is warranted only when
the reference behaviour a test compares against is itself wrong.
