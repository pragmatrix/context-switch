# AudioKnife assembles deferred service parameters before starting a conversation

mod_audio_fork limits its initial text message to 8191 bytes after UTF-8
encoding, while Gemini and OpenAI dialog instructions embedded in service
parameters can exceed that size.
AudioKnife therefore accepts an opt-in deferred-parameter exchange and assembles
it into one complete logical Start before passing it to ContextSwitch. This keeps
the transport constraint out of the core protocol and service implementations.

## Wire contract

- An ordinary initial Start remains unchanged and contains `params`.
- A deferred initial Start contains `"deferParams": true` and omits `params`.
- Before requesting the parameters, AudioKnife validates only the transport
  fields needed for the deferred exchange: the message is a JSON object, its
  type is `start`, it has a valid conversation ID, and it opts into deferral
  without inline `params`. Validation of ContextSwitch Start fields remains with
  ContextSwitch after the Logical Start has been assembled.
- After accepting the deferred initial Start, AudioKnife sends this Deferred
  Params Request and waits for the send to complete:
  `{"type":"json","data":{"type":"sendParams","id":"<same conversation id>"}}`.
- The Deferred Params Request acknowledges only that AudioKnife is ready to
  receive parameters; it does not mean that the conversation has started.
- The Deferred Params Request is an AudioKnife/mod_audio_fork transport message,
  not a ContextSwitch `ServerEvent`. ContextSwitch observes only the assembled
  Logical Start.
- Only after sending the Deferred Params Request does AudioKnife begin receiving
  the deferred params message. A params frame already buffered by the WebSocket
  is accepted; AudioKnife does not attempt to detect whether the client sent it
  before receiving the request.
- Its first subsequent text WebSocket frame must contain
  `{"type":"params","id":"<same conversation id>","params":<complete JSON value>}`.
- The deferred message supports the same plain JSON and `base64:`-prefixed JSON
  encodings as other AudioKnife client text messages.
- AudioKnife ignores Binary, Ping, and Pong frames while waiting for deferred
  parameters. It rejects Close frames, mixed inline and deferred parameters,
  malformed first text frames, the wrong event type, and mismatched conversation
  IDs through its existing startup error path.
- The deferred message inherits the WebSocket message-size limit. AudioKnife adds
  no separate size limit, completion acknowledgement, timeout, retry, or
  chunking protocol. The normal conversation `Started` or startup error follows
  processing of the assembled Logical Start.

## Client implementation

A client that needs to send service parameters larger than mod_audio_fork's
initial-message limit must:

1. Serialize the complete service parameters as one JSON value.
2. Send an initial Start without `params` and with `"deferParams": true`.
3. Wait until AudioKnife tells the client to send the parameters.
4. Send one text WebSocket message with `type` set to `params`, the Start
  conversation ID, and the complete serialized parameters.
5. Only send audio or other client events after the deferred params message.

For example, the exchange starts with this client message:

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

AudioKnife responds:

```json
{
  "type": "json",
  "data": {
    "type": "sendParams",
    "id": "conversation-id"
  }
}
```

After receiving this Deferred Params Request, the client responds:

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
and deferred parameters, or split the deferred value over multiple messages.
Binary, Ping, and Pong frames sent before the deferred params message are
discarded. A Close frame ends startup with an error.

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
