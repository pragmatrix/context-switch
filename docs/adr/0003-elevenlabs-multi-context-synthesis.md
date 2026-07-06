# ElevenLabs synthesis uses the multi-context websocket for request boundaries

ElevenLabs realtime TTS must keep one websocket open across many sequential
synthesis requests (with idle gaps in between), and emit `RequestCompleted` once
per request — the same lifecycle as the non-streaming backends (azure, aristech).
The single-context `stream-input` endpoint cannot do this: it only emits the
`isFinal` completion marker when the input stream is *closed* (`{"text": ""}`),
which also tears down the connection; a `flush` generates audio but never signals
completion. We therefore use the multi-context `multi-stream-input` endpoint,
where each request is a `context_id` and `{"close_context": true}` completes that
context (server replies `{"isFinal": true, "contextId": ...}`) while the socket
stays open for the next request.

## Considered Options

- **Single-context `stream-input` + close on `isFinal: true`.** Rejected: closing
  to obtain `isFinal` ends the whole connection, so each conversation could serve
  only one request — no idle-then-next-request flow.
- **Single-context + reconnect per request.** Rejected: a new TCP+TLS+websocket
  handshake per utterance adds latency and defeats the point of a persistent
  connection.

## Decision details

- **One context per request, strictly sequential.** Requests are never
  interleaved (per ADR 0002), so at most one context is live at a time. We use
  multi-context purely for its per-request boundary (`close_context` → `isFinal`),
  not for the concurrency it was designed for.
- **`context_id` is internal.** It is a monotonic per-connection counter and never
  surfaces in our protocol or glossary. Inbound `audio` / `isFinal` are matched
  against the active `context_id`; messages for any other context are ignored as
  stale.
- **`voice_settings` is per-context.** It is sent on the opening (first) fragment
  of each request, which the multi-context API scopes to that context — unlike the
  single-context endpoint where settings were frozen for the whole connection.

## Consequences

- The `eleven_v3` model is unsupported on multi-context websockets; the default
  `eleven_flash_v2_5` and other realtime models are unaffected.
- Connection teardown sends `{"close_socket": true}` and drains until the server
  closes, so any in-flight audio is still delivered.
