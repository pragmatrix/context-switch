# Partial text input via an `isFinal` flag on the existing text event

Streaming text-to-speech backends (ElevenLabs realtime TTS) generate audio from
text as it arrives, so the protocol needs to express a synthesis request that is
streamed across several text fragments. We extend the existing text input event
rather than adding a dedicated streaming event family: `ClientEvent::Text` /
`Input::Text` gain an `isFinal` flag (defaulting to `true`). Fragments arrive in
order and are not interleaved with other requests, so they need no correlation
id: a backend accumulates fragments until one with `isFinal: true` completes the
request and yields `RequestCompleted`.

## Considered Options

- **Dedicated streaming variants** (e.g. `TextChunk` / `TextEnd`). Rejected: a
  separate event family adds wire surface for what is conceptually one event with
  a finality flag.
- **A correlation `request_id` on text fragments.** Rejected (YAGNI): fragments
  are delivered in order without interleaving, so grouping them needs no id.

## Consequences

- Omitting `isFinal` preserves today's behavior exactly (each text event is one
  complete request), so existing clients and the non-streaming synthesize
  backends (azure, aristech) need no changes — they always observe `isFinal: true`.
- Interleaving fragments of different requests on one conversation is unsupported
  by design; a future need would reintroduce an explicit correlation id.
