# Context

Glossary of canonical terms for context-switch. Keep this free of implementation
detail — it is a glossary, not a spec.

## Speech provider terms

These three Microsoft offerings are distinct and must not all be called "Azure".

- **Azure Speech** — Microsoft's Azure AI Speech SDK service. In this repo it backs
  the `azure` crate (`AzureTranscribe`, `AzureSynthesize`, ...). Classic
  speech-to-text / text-to-speech.

- **Azure OpenAI Realtime** — the Azure-hosted variant of the OpenAI Realtime API.
  Reached through `openai-dialog` with `Protocol::Azure`. A realtime
  speech-to-speech dialog protocol over a single websocket.

- **Voice Live** — Microsoft Foundry's managed speech-to-speech API
  (`/voice-live/realtime`). Wire-compatible with Azure OpenAI Realtime but adds
  Azure-only capabilities (deep noise suppression, Azure semantic VAD, Azure
  speech / MAI transcription models). Backed by the `microsoft-voice-live` crate.

  Note: Voice Live and Azure OpenAI Realtime are expected to converge over time;
  the `microsoft-voice-live` boundary is kept deliberately thin so the two can be
  merged later without a rewrite.

- **Deepgram Flux** — Deepgram's turn-based conversational speech-to-text API
  (`listen` v2). Unlike classic streaming recognition, it emits per-turn events
  rather than a continuous interim/final stream. Backs the `deepgram-service`
  crate (`DeepgramTranscribe`, registered as `deepgram-transcribe`).

- **Turn** — a single contiguous span of one speaker talking, as detected by
  Flux. A turn accumulates transcript across `Update` events and is closed by an
  `EndOfTurn`.

- **End of Turn (EOT)** — Flux's decision that the speaker has finished the
  current turn. Governed by `eotThreshold` (confidence to close a turn) and
  `eotTimeoutMs` (silence after which a turn is force-closed).

- **Eager End of Turn** — an early, lower-confidence EOT signal (enabled by
  `eagerEotThreshold`) that lets a downstream agent begin preparing a reply
  before the turn is confirmed. May be retracted by a `TurnResumed` event if the
  speaker continues.

## Turn detection terms

- **Turn Detection** — the provider-neutral configuration (`context_switch_core`)
  that controls how a backend decides a turn has ended. Each backend converter
  forwards only the fields it understands. Because the two backends model
  end-of-turn aggressiveness differently, it is expressed twice rather than
  mapped between forms: `threshold` / `eagerThreshold` (confidence floats) are
  Deepgram-only, and `thresholdLevel` is Voice Live-only.

- **Threshold Level** — the categorical end-of-turn aggressiveness consumed by
  Voice Live (`low` / `medium` / `high`). Lower levels wait for stronger
  evidence before ending a turn; higher levels end turns earlier. When unset,
  Voice Live uses its built-in default behavior.
