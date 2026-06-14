# Position encoding: negotiate UTF-8, default UTF-16

LSP `Position.character` is not a character index and not a byte offset —
its meaning is the *negotiated* `PositionEncodingKind` (LSP 3.18,
*Position*). The only mandatory encoding is UTF-16; a client may also
offer UTF-8 and/or UTF-32 through the `general.positionEncodings` client
capability, and the server echoes its pick back in
`InitializeResult.capabilities.positionEncoding`. If the server omits it,
the encoding is UTF-16.

lspf negotiates **UTF-8 when the client offers it, falling back to
UTF-16** otherwise. The negotiated kind is stored as runtime state on the
[[Documents]] store and governs every later `position ↔ offset`
conversion. This is rust-analyzer's strategy.

We chose UTF-8-preferred for performance (ADR 0005). The [[Document]]'s
`ropey::Rope` and every Rust `&str` index natively in **bytes**; with
UTF-8 negotiated, `Position.character` *is* the byte offset within the
line, so the hot path — the conversion done on every hover, completion,
diagnostic range, and incremental `didChange` — is a direct rope byte
index with no transcoding. This matters most over the WebSocket
transport, where ADR 0005 already counts redundant work as latency.

We rejected **UTF-16 only** (advertise nothing, default to UTF-16). It is
the smaller surface — one conversion path, correct for every client — but
`ropey` indexes in chars and bytes, never UTF-16 code units, so *every*
position would pay a `char ↔ utf16` transcode even when talking to a
client (rust-analyzer's own ecosystem, most modern editors) that would
happily speak UTF-8. We keep the UTF-16 path as the mandatory fallback,
not the default.

We rejected **treating `Position.character` as a `ropey` char index**
(the naive reading). It is wrong under *both* encodings: a line
containing any non-ASCII text (an emoji, a CJK character, a combining
mark) has a different char count than its UTF-16 code-unit count and its
UTF-8 byte count. This is the most common silent correctness bug in LSP
servers, and naming it here is half the reason this ADR exists.

This is the one capability that escapes ADR 0004's static-const
auto-derivation. Every other capability is a compile-time `const` on the
trait; `positionEncoding` cannot be, because the server's choice depends
on the *client's* offered list, which only arrives in `InitializeParams`.
Two consequences follow:

1. The `InitializeResult` builder must **see the client capabilities** —
   the capability-assembly step takes `InitializeParams`, not just
   `&self`, so it can intersect the client's `general.positionEncodings`
   with lspf's preference order (`[utf-8, utf-16]`).
2. The negotiated kind is **runtime state**, not a const. The
   [[Documents]] store holds it and every conversion method reads it; a
   handler never has to thread the encoding through itself.

The cost we accept: two conversion code paths (UTF-8 native, UTF-16
transcoded) plus the negotiation logic, and a `positionEncoding` that
sits outside the otherwise-uniform const-derived capability model. We
treat the asymmetry as honest — position encoding genuinely *is*
negotiated rather than declared, and pretending otherwise would either
break correctness or forfeit the UTF-8 fast path.
