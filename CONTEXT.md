# Context

Domain glossary for mosh. Terms only — no implementation details.

## Roles

**Client** — the endpoint a person sits in front of. Owns the local terminal, renders
what it believes the server's screen looks like, and speculatively renders keystrokes
before the server confirms them.

**Server** — the endpoint that owns the real pty and the real shell process. Its screen
is the authority; the client's screen is always a guess at it.

**Wrapper** — the launcher a person actually types. It reaches the remote host over ssh,
starts a Server there, reads the handshake, and execs a Client. It is not a party to the
session itself.

## The session

**Session** — one continuous conversation between a Client and a Server, identified by a
shared secret rather than by an address. A Session outlives the network path it started
on: either endpoint may change address, disappear, and reappear without ending it.

**Datagram** — one self-contained, independently decryptable unit sent over the network.
Loss, reordering, and duplication of Datagrams are normal and expected, never errors.

**Nonce** — the never-repeating number that makes each Datagram's encryption unique.
Reusing one within a Session is a security failure, not merely a bug.

**Instruction** — what one endpoint tells the other in a Datagram: which State it is
moving from, which State it is moving to, and the last State it heard about from the
peer. Because an Instruction names the State it moves from, receiving one twice equals
receiving it once — the second is recognised as already applied and dropped.

**Diff** — the content of an Instruction: the change between two States, never the whole
State. What a Diff means depends on which direction it travels.

A Diff is a transition, not an operation: it is only meaningful applied to the exact
State it was computed from, and applying it twice does not equal applying it once. The
idempotence above is the Instruction's property, earned by naming its starting State,
and never the Diff's.

## State

**State** — a complete snapshot of one side's meaning at a point in time. Each endpoint
keeps its own State and a belief about the peer's.

**Screen** — the Server's State: the contents of the terminal, expressed as what should
be displayed rather than as the bytes that produced it. Two different byte streams that
produce the same Screen are the same State.

**User input** — the Client's State: what the person has typed.

**Frame** — one rendering of a Screen onto a real terminal, expressed as the smallest
byte sequence that turns what the terminal currently shows into what it should show.

**Cell** — one addressable position on the Screen. A Cell holds a Grapheme rather than a
character, so that a base letter and its accents occupy one position together.

**Grapheme** — what a reader perceives as a single written unit, which may be several
characters. Deciding where one Grapheme ends and the next begins is what makes a
character's Width meaningful.

**Width** — how many Cells a character claims: none (it joins the preceding Grapheme),
one, or two. A character that claims no Cells is why Graphemes exist. Width is not a
private decision — both endpoints compute it independently and must agree, or they
disagree about the Screen itself.

**Printable** — whether a character may appear on the Screen at all. Unprintable
characters are discarded rather than displayed, so the two endpoints must also agree on
which characters those are.

## Speculation

**Prediction** — a change the Client displays before the Server has confirmed it, on the
belief that the Server will echo it. Predictions are provisional: they are confirmed,
or they are wrong and withdrawn.

**Epoch** — a generation of Predictions. When the Client learns it guessed wrong, it
abandons a whole Epoch at once rather than repairing individual Predictions.

## Compatibility

**Wire Contract** — everything two endpoints must agree on to share a Session: Datagram
framing, encryption, Instruction encoding, Diff encoding, and the handshake that starts
a Session. Anything not in the Wire Contract is an endpoint's private business.

**Interop** — a Session between endpoints built from different implementations of this
project. Required in both directions: either role may be either implementation.
