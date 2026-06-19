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
