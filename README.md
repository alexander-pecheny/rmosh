rmosh
=====

A port of [mosh](https://github.com/mobile-shell/mosh) to Rust: the terminal, the state
sync, the network layer, `mosh-server`, `mosh-client`, and a launcher that replaces the
Perl wrapper. It was written module by module against the C++ and its tests rather than
run through a transpiler; [ADR 0001](docs/adr/0001-hand-port-rather-than-transpile.md)
says why.

Usage and options are upstream's; see the mosh(1) man page and [mosh.org](https://mosh.org).
The launcher needs no Perl, so `mosh-server` can be packaged without it.

Building
--------

A Rust toolchain and `protoc`, from `brew install protobuf` on MacOS or
`apt install protobuf-compiler libprotobuf-dev` on Debian.

```
$ cargo build --release
$ cargo test
```

Testing against upstream
------------------------

Upstream sits at `third_party/mosh` as a submodule, and nothing in it is ours to change.
The tests compare screens, frames, parser states and whole datagrams between the two
trees. They skip until upstream has been built, which is what `tests/cpp/build.sh` does.

```
$ git submodule update --init
$ ./tests/cpp/build.sh
$ cargo test
```

That build needs mosh's own deps (autotools, protobuf, ncurses, openssl, zlib, and on
Linux libutempter). Upstream's [README](https://github.com/mobile-shell/mosh#notes-for-developers)
lists the package names per distro.

Upstream's own shell tests can drive our binaries, so a session runs with one end from
each tree. Their paths are hardcoded, so this is the one time the submodule gets patched.
Revert it afterwards.

```
$ cd third_party/mosh
$ git apply ../../tests/cpp/endpoint-override.patch
$ MOSH_CLIENT_OVERRIDE=$PWD/../../target/release/mosh-client \
  MOSH_SERVER_OVERRIDE=$PWD/../../target/release/mosh-server make check
$ git checkout -- src/tests
```

Two behaviours diverge from upstream on purpose. Colour and OSC 52 clipboard queries go
to the client's terminal here, which is the pair of patches this fork started as. And
upstream drops SGR 2 and SGR 9, which this implements. The tests allow exactly those and
compare everything else strictly.
