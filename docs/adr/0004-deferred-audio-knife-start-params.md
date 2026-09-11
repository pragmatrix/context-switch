# AudioKnife assembles deferred service parameters before starting a conversation

mod_audio_fork limits its initial message to roughly 8 KiB, while Gemini and
OpenAI dialog instructions embedded in service parameters can exceed that size.
AudioKnife therefore accepts an opt-in two-message transport form and assembles
it into one complete logical Start before passing it to ContextSwitch. This keeps
the transport constraint out of the core protocol and service implementations.

## Wire contract

- An ordinary initial Start remains unchanged and contains `params`.
- A deferred initial Start contains `"deferParams": true` and omits `params`.
- Its literal next WebSocket frame must be a text message containing
  `{"type":"params","id":"<same conversation id>","params":<complete JSON value>}`.
- The deferred message supports the same plain JSON and `base64:`-prefixed JSON
  encodings as other AudioKnife client text messages.
- AudioKnife rejects mixed inline and deferred parameters, non-text or malformed
  second frames, the wrong event type, and mismatched conversation IDs through
  its existing startup error path.
- The deferred message inherits the WebSocket message-size limit. AudioKnife adds
  no separate size limit, acknowledgement, timeout, retry, or chunking protocol.
- Ping and Pong frames are not accepted between the initial Start and deferred
  params message; support can be added if this occurs in practice.

## Client implementation

A client that needs to send service parameters larger than mod_audio_fork's
initial-message limit must:

1. Serialize the complete service parameters as one JSON value.
2. Send an initial Start without `params` and with `"deferParams": true`.
3. Immediately send one text WebSocket message with `type` set to `params`, the
   Start conversation ID, and the complete serialized parameters.
4. Only send audio or other client events after the deferred params message.

For example, a client sends these two messages in order:

```json
{
  "type": "start",
  "id": "conversation-id",
  "service": "openai-dialog",
  "deferParams": true,
  "inputModality": { "type": "text" },
  "outputModalities": []
}
```

```json
{
  "type": "params",
  "id": "conversation-id",
  "params": {
    "instructions": "Complete service parameters, including long instructions"
  }
}
```

Both messages may instead use AudioKnife's `base64:<encoded-json>` text
encoding. The client must not defer only part of the parameters, combine inline
and deferred parameters, split the deferred value over multiple messages, or
send a Ping or Pong between the two messages.

Clients that do not need deferral continue to send a single Start with inline
`params`. A client opting into deferral requires an AudioKnife version that
supports this ADR; it has no negotiation or fallback after sending the deferred
Start.

## Considered Options

- **Add a two-stage lifecycle to the core protocol.** Rejected: services consume
  typed parameters when their conversation starts, and the size constraint is
  specific to the mod_audio_fork transport path.
- **Reuse a service event for deferred parameters.** Rejected: service events are
  delivered only after the service has already been constructed from Start
  parameters.
- **Support arbitrary chunks or merge inline and deferred values.** Rejected:
  neither is required, and both add ordering, completion, and conflict semantics.
- **Negotiate support with older AudioKnife servers.** Rejected: inline Starts
  remain compatible, while clients opting into deferral require a supporting
  server.
