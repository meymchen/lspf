# Bound outbound admission by messages and encoded bytes

Status: Accepted. Implements the outbound part of ADR 0025 and replaces ADR
0015's observational queue-depth threshold with enforced admission budgets.

Each connection admits an outbound message only when both
`ResourcePolicy::max_outbound_messages` and
`ResourcePolicy::max_outbound_bytes` have room. The byte charge is the exact
serialized JSON-RPC envelope body, excluding transport framing. A message
keeps its slot and byte charge while waiting and during the transport send.
The send-loop releases both after every transport attempt, successful or
failed. A rejected message is never charged.

The channel primitive remains unbounded because it is a runtime-neutral wake-up
mechanism shared by native and WASM targets. Admission happens under the
queue's accounting lock before a message enters that channel, so accepted work
cannot exceed either policy budget. This keeps `Client::notify` synchronous and
does not introduce transport back-pressure into handlers.

Ordinary `Client` notifications and requests return
`ClientError::OutboundOverloaded` when either budget is full. A rejected
request removes the pending broker entry allocated before enqueue. Callers can
therefore distinguish overload from a closed transport and decide whether to
retry, degrade optional output, or fail their operation.

Responses, protocol-error frames, and `$/cancelRequest` cannot be dropped as
ordinary optional work. They use the same finite budgets. If one cannot fit,
the queue signals the protocol engine's existing single close operation and
the connection ends as `Outcome::WriterFailed`. Normal connection close needs
no reserved queue entry: it rejects new work, drains messages already admitted,
and releases their accounting as the writer attempts them.

We rejected a fixed reserve for required traffic. A reserve large enough for
one response says nothing about concurrent responses or one response larger
than the reserve, while a larger reserve withholds capacity from ordinary
traffic without making delivery certain. Failure-close gives every essential
enqueue one deterministic result and preserves the absolute connection limits.
