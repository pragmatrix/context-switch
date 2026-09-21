# ADR 0006: Gemini 3.8 Live model semantics

Date: 2026-09-18
Status: Accepted

## Context

Google exposes two stable Gemini Developer API models with materially different
runtime contracts:

- `gemini-3.8-live` is the default low-latency model. It rejects
  `thinking_level`, supports blocking and non-blocking function calls, and
  supports scheduling non-blocking function responses.
- `gemini-3.8-live-extended-thinking` performs background reasoning. It accepts
  no thinking level or `low`, `medium`, or `high`; requires non-blocking
  functions; rejects function-response scheduling; and may end a model turn
  while the dialog interaction remains active.

The existing Google dialog API accepts an open model string, maps every provider
`turnComplete` to the public terminal `TurnComplete` event, and depends on a
vendored raw-WebSocket client whose tool and lifecycle types predate these
contracts. Merely accepting the new model strings would therefore advertise
support while exposing incorrect lifecycle and tool behavior.

The models were verified as stable in the Gemini Developer API. Their
availability through Gemini Enterprise Agent Platform, including EU data
residency, was not established by Google's published model-location
documentation. Paid Gemini API use is governed by Google's Data Processing
Addendum and Google states that paid prompts and responses are not used to
improve its products, but transient abuse-monitoring processing may occur in any
country where Google or its agents maintain facilities. These are dated
deployment facts, not properties guaranteed by this client.

## Decision

Support both models as production-ready direct Gemini API models. Keep
`Params.model` as an open `String`, add public `GEMINI_3_8_LIVE` and
`GEMINI_3_8_LIVE_EXTENDED_THINKING` constants, and do not add a model allowlist
or a separate extended-thinking flag. The direct API example defaults to
`GEMINI_3_8_LIVE`; Extended Thinking remains opt-in. Existing model IDs and
Agent Platform behavior remain supported. The 3.8 constants do not assert Agent
Platform availability.

The public constants have these exact declarations and remain bare model IDs:

```rust
pub const GEMINI_3_8_LIVE: &str = "gemini-3.8-live";
pub const GEMINI_3_8_LIVE_EXTENDED_THINKING: &str =
  "gemini-3.8-live-extended-thinking";
```

The integration uses the domain distinction in `CONTEXT.md`: a wire
`turnComplete` closes a **Model Turn**, while only `interactionStatus: IDLE`
closes the **Dialog Interaction**.

- Finalize the current output transcript at every wire `turnComplete`.
- Map `turnComplete` with `interactionStatus: IN_PROGRESS` to the new public
  `ServiceOutputEvent::InteractionInProgress`.
- Map `turnComplete` with `interactionStatus: IDLE` to the existing terminal
  `ServiceOutputEvent::TurnComplete`.
- Preserve legacy completion behavior when `interactionStatus` is absent.
- Preserve the status through the vendored client's wire and semantic event
  types; do not infer interaction state from model names.

The public output enum gains exactly one unit variant:

```rust
pub enum ServiceOutputEvent {
    // Existing variants remain unchanged.
    InteractionInProgress,
}
```

Its serialized form is `{"type":"interactionInProgress"}`. `TurnComplete`
retains `{"type":"turnComplete"}` and remains the only terminal interaction
event. No provider status enum is exposed: `IN_PROGRESS` and `IDLE` are wire
states whose domain meanings are represented by the two service events.

Validate model-specific setup before opening the WebSocket:

- `gemini-3.8-live` requires `thinking_level` to be absent.
- `gemini-3.8-live-extended-thinking` permits an absent level or `low`,
  `medium`, or `high`, and rejects `minimal`.
- When the level is absent for Extended Thinking, `google-dialog` sends
  `medium` itself. Google's public model documentation does not specify a
  fixed default level, so the service pins `medium` deterministically rather
  than depending on an unspecified server default.
- Legacy models retain their existing setup behavior.

For tools, declaration behavior and result scheduling are separate concepts.
The vendored protocol must model both `BLOCKING` and `NON_BLOCKING` declaration
behavior. Scheduling belongs on `FunctionResponse`, with typed values for
immediate interruption, delivery when idle, and silent context insertion; the
wire spelling must follow the current Google schema.

The exact types are:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionBehavior {
  Blocking,
  NonBlocking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionResponseScheduling {
  Silent,
  WhenIdle,
  Interrupt,
}
```

These types are defined by `gemini-live` because they directly represent wire
fields, and are re-exported by `google-dialog` so callers do not need to name
the vendored crate. `FunctionDeclaration::behavior` remains an
`Option<FunctionBehavior>`. Its existing declaration-level `scheduling` field
remains only for legacy deserialization and is not re-exported as the response
scheduling type.

`ServiceInputEvent::FunctionCallResult` changes to this exact shape:

```rust
FunctionCallResult {
  call_id: String,
  output: serde_json::Value,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  scheduling: Option<FunctionResponseScheduling>,
},
```

Omitting `scheduling` means no field is sent and preserves Google's original
function-response handling for backward compatibility. The serialized forms
are:

```json
{"type":"functionCallResult","callId":"call-1","output":{"ok":true}}
```

```json
{"type":"functionCallResult","callId":"call-1","output":{"ok":true},"scheduling":"INTERRUPT"}
```

- Standard 3.8 permits blocking and non-blocking declarations and optional
  scheduling on non-blocking function responses.
- Extended Thinking defaults omitted declaration behavior to `NON_BLOCKING`,
  preserves explicit `NON_BLOCKING`, and rejects `BLOCKING`.
- Extended Thinking rejects scheduling both in declarations and function
  responses rather than silently dropping it.
- Declaration-level scheduling remains readable for legacy compatibility but is
  not used to represent the current 3.8 response contract.
- Function call IDs remain the correlation key, allowing multiple non-blocking
  calls to be outstanding and results to arrive out of order.

For standard 3.8, an explicit response schedule paired with a blocking
declaration is rejected rather than sent as an ignored field. For Extended
Thinking, any explicit response schedule is rejected. These checks happen when
the service event is handled because the call ID identifies the corresponding
declaration. Omitted scheduling is valid for every model. Unknown and legacy
models retain pass-through behavior, and response scheduling is supported on
both direct Gemini API and Agent Platform connections.

Expose full-session incremental context as a provider input event named
`ClientContent` (the Google wire envelope is `clientContent`). It carries an
explicit `user` or `model` role, text content, and `turn_complete`. Keep the
existing realtime `Prompt` input unchanged. Sending client content with
`turn_complete: true` interrupts active generation; sending it without that flag
appends content and waits for further input.

Callers pick between these text input paths by intent:

- Realtime user input (the user speaks or types now): `Prompt`.
- An immediate, ordered response with interruption of active generation:
  `ClientContent` with `user` role and `turn_complete: true`.
- Silent context the model uses only when generation is next triggered:
  `ClientContent` with `user` role and `turn_complete: false`.
- Restoring or fabricating an earlier assistant turn: `ClientContent` with
  `model` role.

The exact public API is:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientContentRole {
  User,
  Model,
}

pub enum ServiceInputEvent {
  // Existing variants remain, with FunctionCallResult extended as above.
  ClientContent {
    role: ClientContentRole,
    text: String,
    #[serde(default)]
    turn_complete: bool,
  },
}
```

The members have this contract:

- `role: ClientContentRole` identifies the author of the history entry. `User`
  serializes as `user` and `Model` as `model`. No other roles are accepted;
  this prevents arbitrary role strings from reaching the Gemini wire protocol.
- `text: String` is the text of exactly one content part. It is required, is
  sent as `clientContent.turns[0].parts[0].text`, and is not interpreted as
  realtime input. Empty text is allowed because the event represents content,
  not a prompt-validation policy.
- `turn_complete: bool` controls generation after the content is appended. The
  default is `false`, which appends the content and leaves generation pending.
  `true` asks Gemini to start generation immediately and intentionally interrupts
  active generation. It serializes as `turnComplete`.

The roles have distinct conversation semantics:

- `User` means that `text` is user-authored context supplied by the client. It
  represents something the end user said or wrote and is eligible to be used as
  user input when Gemini generates the next response.
- `Model` means that `text` is model-authored context supplied by the client. It
  represents an earlier assistant/model response that the client is restoring
  or appending to conversation history; it does not claim that Gemini generated
  this text during the current connection.

These roles describe conversation authorship, not caller authorization, audio
direction, or the event source. A client must not label newly supplied user
input as `Model`, and the service must not infer a `Model` entry from a
realtime `Prompt`. `ClientContentRole` is provider-owned and restricts callers
to the two roles accepted by Google. One event maps to one
`clientContent.turns` entry with `role`, one text part, and the requested
completion flag. The service sends `turnComplete` explicitly, including when
it is `false`, so the resulting wire intent is unambiguous. Input-event JSON
uses the service tag and field names; the client converts it to the Gemini
envelope rather than forwarding this JSON directly:

```json
{"type":"clientContent","role":"user","text":"Remember this.","turnComplete":false}
```

```json
{"type":"clientContent","role":"model","text":"Earlier answer.","turnComplete":true}
```

The corresponding Gemini wire message for the first example is:

```json
{"clientContent":{"turns":[{"role":"user","parts":[{"text":"Remember this."}]}],"turnComplete":false}}
```

`ClientContent` is valid for both `User` and `Model` history entries, but it is
not a replacement for ordinary audio or realtime text input. The event is
provider-specific and is rejected by the service when the selected routing
cannot send direct Gemini Live `clientContent` messages.

The existing `Prompt { text }` variant and its JSON remain unchanged. It maps to
realtime text input, is always user input, and cannot insert model history.

Input transcription language hints are exposed on `Params`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TranscriptionMode {
  Verbatim,
  Smart,
}

#[serde(default)]
pub input_audio_transcription_language_codes: Vec<String>,
#[serde(default = "default_transcription_mode")]
/// Defaults to `VERBATIM`.
pub input_audio_transcription_mode: TranscriptionMode,
#[serde(default = "default_transcription_mode")]
/// Defaults to `VERBATIM`.
pub output_audio_transcription_mode: TranscriptionMode,
```

The field serializes as `inputAudioTranscriptionLanguageCodes` and maps to
Google's `inputAudioTranscription.languageCodes`. Values are BCP-47 language
codes used as input ASR hints, not a request to force the model's native audio
response language. The explicit `input_audio_transcription: bool` controls
whether input transcription is enabled; language codes and mode only configure
it when that flag is `true`. The language-code list defaults to empty, and an
empty list means automatic language detection. Both transcription mode fields
default to `VERBATIM`; callers select `SMART` explicitly. The output
transcription boolean likewise controls whether output transcription is
enabled; its mode only configures it when that flag is `true`.
The transcription mode fields map to the corresponding configuration's `mode`:
`inputAudioTranscription.mode` and `outputAudioTranscription.mode`. The public
default is `VERBATIM`; `SMART` removes disfluencies, performs light
grammatical cleanup, applies automatic formatting, and makes minor inline
corrections. `SMART` cannot be combined with word timestamps or diarization;
those controls remain unexposed. Output transcription has no corresponding
language-code parameter because native-audio output language selection is
automatic for these models. `google-dialog` therefore sends
`outputAudioTranscription.languageCodes` as absent and derives the output
transcript language from the generated audio; callers cannot use output
transcription configuration to select or override that language.

`google-dialog` re-exports the complete new surface from its crate root:

```rust
pub use types::{
  ClientContentRole, FunctionBehavior, FunctionResponseScheduling,
  GEMINI_3_8_LIVE, GEMINI_3_8_LIVE_EXTENDED_THINKING, Params,
  ServiceInputEvent, ServiceOutputEvent, VOICES, parse_voice_value,
};
```

## 3.8 Live feature overview

Beyond the wiring documented above, the two 3.8 Live models provide these
capabilities (verified against Google's model and Live API documentation on
2026-09-18 and 2026-09-21, with the model-specific pages treated as
authoritative):

- **Async function calling.** Non-blocking tool declarations (`behavior:
  NON_BLOCKING`) let longer-running functions execute in the
  background while the conversation continues. Omitted `behavior` defaults to
  blocking (Google's tool-use guide: a declaration without `behavior` "will
  still pause all interactions with the model"); the agent can provide updates
  and returns results when ready, controlled by the `FunctionResponseScheduling`
  values `SILENT`, `WHEN_IDLE`, and `INTERRUPT` that `FunctionCallResult`
  forwards. Extended Thinking runs async-only and rejects scheduling. Google's
  capabilities table spells the last value `INTERRUPTED`; the tool-use guide
  and this ADR use `INTERRUPT`.
- **Proactive audio.** The agent speaks only when relevant and can remain
  quiet unless directly addressed, making conversations less interruptive. This
  is enabled permanently on both 3.8 models (see the fixed-choice entry in the
  next section).
- **Client content for context injection.** The `clientContent` channel lets
  callers add context without forcing a turn (`turn_complete: false`),
  backchanneling information silently so the model stays informed without a
  spoken response. With `turn_complete: true`, the same channel instead asks
  the model to respond immediately, interrupting any active generation.
- **High/background reasoning.** Extended Thinking performs frontier-level
  reasoning in the background and supports more complex tasks while remaining
  responsive during the ongoing conversation. Standard 3.8 keeps interleaved
  reasoning with fixed, non-configurable depth.
- **Native audio experience.** The background reasoning runs inside the native
  audio pipeline, so deep processing and real-time voice interaction coexist
  instead of trading one for the other.
- **Responsive multitasking.** The model can work on slower complex outputs
  while still chatting, balancing deep reasoning with fast conversational
  response.
- **Model capability comparison.** Standard `gemini-3.8-live` already carries
  strong reasoning; `gemini-3.8-live-extended-thinking` favors deeper
  performance on complex or creative tasks over minimal latency.
- **Google Search grounding.** The only supported grounding tool on both
  models; exposed through `Tool::GoogleSearch` (its metadata is not forwarded;
  see the exclusion list below).
- **Bidirectional audio transcription.** Input and output transcripts in
  `VERBATIM` or `SMART` mode are configured through the transcription `Params`
  fields documented above; native audio language selection stays automatic.

## Features not exposed by `google-dialog`

The public service API is intentionally narrower than the direct Gemini Live
wire API. As verified against Google's model and Live API documentation on
2026-09-18, this integration does not expose the following supported features:

- Image or video input. Gemini accepts JPEG or PNG video frames at up to one
  frame per second, but the core conversation input supports only audio, text,
  and service events. `google-dialog` continues to require audio input.
- Arbitrary multimodal or batched `clientContent`. `ClientContent` sends one
  role-qualified text part per service event; callers cannot submit multiple
  turns, inline media, function parts, or arbitrary Gemini `Part` values.
- Manual and hybrid VAD signals. `Params.realtime_input_config` exposes setup
  configuration, but callers cannot directly send `activityStart`,
  `activityEnd`, or `audioStreamEnd` service events. The service sends
  `audioStreamEnd` itself when its audio input closes.
- Media-resolution selection for image or video input.
- Generation controls other than the existing temperature and model-specific
  thinking level. In particular, there are no public controls for `topP`,
  `topK`, `maxOutputTokens`, or `seed`.
- Detailed transcription configuration beyond language hints and mode. Input
  and output transcription can be enabled as booleans, input ASR language
  hints are configurable, and both directions support `VERBATIM` or `SMART`
  mode, but custom vocabulary, word timestamps, diarization, speaker labels,
  and word timing are not configurable or emitted as service events.
- Thought summaries. `includeThoughts` is never enabled and thought parts are
  not emitted. Exposing them requires a distinct core output contract so
  reasoning summaries cannot be mistaken for ordinary model output.
- Dedicated Live Translation configuration, including target-language and
  echo-target-language controls. A caller may still request language behavior
  through instructions, but that is not equivalent to exposing Google's typed
  translation configuration.
- Rich function responses. A service event returns one JSON result for one call
  ID; it cannot return multiple function responses in one message, media parts,
  or streaming/generator results through `willContinue`.
- Grounding metadata and citations produced by Google Search. Search grounding
  itself remains available through `Tool::GoogleSearch`, but its structured
  metadata is not forwarded to consumers.
- Raw lifecycle and transport events. `generationComplete`, interruption,
  `GoAway`, and session-resumption updates are handled or ignored internally;
  only the domain-level completion events and tool-call cancellation are public.
- Raw token-usage metadata. Usage is converted into billing records and is not
  also emitted as a provider service event.
- Client-managed context compression and session resumption. Callers cannot set
  compression thresholds, choose the sliding-window target, supply or retrieve
  resume handles, request transparent resumption, or control reconnect timing.
  The service enables and manages these mechanisms internally.
- Client-to-server authentication with ephemeral tokens. `google-dialog` is a
  server-side service and authenticates direct Gemini API connections with its
  configured API key.

The following are fixed model or integration choices rather than missing
controls:

- Response modality is audio. Text output is obtained through output-audio
  transcription; callers cannot select a text-only Gemini response modality.
- Proactive audio is permanently enabled by both 3.8 models. The service does
  not send a `proactivity` override; Google documents that setting
  `proactive_audio: false` as an error.
- Native-audio language selection is automatic. Callers can guide language in
  the system instructions, but Google does not accept an explicit language code
  for these models.

The following Gemini capabilities are not exposed because both 3.8 Live model
pages mark them unsupported, not because this client withholds a supported
feature: context caching, code execution, file search, Google Maps grounding,
image generation, structured output, and URL context. The models also do not
support the Batch API. Affective dialog is removed from both 3.8 Live model
contracts and is therefore not configurable. The general Live capabilities
guide currently describes affective dialog and proactive audio differently;
the model-specific 3.8 pages are treated as authoritative here. Google Search
grounding and function calling are supported and remain exposed; they are not
part of this exclusion list.

Keep context-window compression enabled with Google's defaults and retain
automatic session resumption. Compression manages context growth; resumption
preserves the logical session across finite WebSocket lifetimes. Resumption must
be checkpoint-safe:

- Treat `GoAway` as advance notice and continue consuming the current socket.
- Track the current `resumable` state and newest resumable handle.
- Reconnect before the deadline only from the newest valid checkpoint.
- On an unexpected disconnect, resume only when a currently valid checkpoint
  exists.
- If the socket is lost while state is not resumable, fail with an explicit
  possible-state-loss error instead of restoring stale state or silently
  starting a new session.
- If `GoAway.timeLeft` expires before a valid checkpoint arrives, emit an API
  error, close the socket, and mark the session closed. Do not attempt either a
  resumed or fresh reconnect; callers must establish a new session explicitly.
- Defer ordinary outbound commands after `GoAway` so they cannot be sent to a
  session that is about to terminate. Tool responses and an explicit close
  remain allowed while waiting for the checkpoint.

## Raw WebSocket client requirements

The vendored `gemini-live-rs` implementation must follow the JSON wire contract,
independently of any Google SDK convenience behavior:

1. Accept a bare model ID from `Params`, but send it in direct API setup as the
  resource name `models/{model}`.
2. Omit unsupported setup fields instead of serializing null or default values.
   In particular, omit the entire thinking configuration for standard 3.8.
3. Use audio as the response modality. Enable output-audio transcription when a
  text transcript is required, and when input transcription is enabled, map
  input language hints to `inputAudioTranscription.languageCodes`.
4. Deserialize `interactionStatus` from `serverContent`, alongside
   `turnComplete`, and retain both values when decomposing a server message into
   semantic events.
5. Continue reading after `turnComplete(IN_PROGRESS)`; later tool calls, audio,
   or another completion may belong to the same dialog interaction.
6. Serialize function behavior on each function declaration. Serialize optional
   scheduling on each function response, never as the current 3.8 declaration
   contract.
7. Send each function result with the original function call ID. Do not assume
   non-blocking results arrive in call order.
8. Serialize incremental context as `clientContent` with explicit part roles.
   Treat `turnComplete: true` as an intentional interruption request.
9. Process session-resumption updates as checkpoint state, including
   `resumable: false`; possession of an older handle is not proof that current
   in-flight work can be restored.
10. Treat `GoAway.timeLeft` as a reconnect deadline while continuing to receive
  updates needed to obtain a safe checkpoint. Expiry without a checkpoint is
  a terminal client-visible failure, not permission to start a fresh session.

## Consequences

The service gains one intermediate output event and provider-specific input and
function-result fields. Exhaustive downstream matches must handle the new event.
Consumers may receive multiple finalized transcript segments before one terminal
`TurnComplete`, but they will no longer be told that an interaction ended while
reasoning or tools remain active.

Existing serialized `FunctionCallResult` and `Prompt` inputs remain valid.
Existing serialized output events retain their spellings. Rust source
compatibility is intentionally broken in two places: constructors and patterns
for `FunctionCallResult` must add or ignore `scheduling`, and exhaustive matches
on `ServiceOutputEvent` must handle `InteractionInProgress`. A caller preserving
the prior function-result behavior migrates by setting `scheduling: None`.
Introducing a second scheduled-result variant was rejected because it would
preserve a duplicated public concept indefinitely while the output enum already
requires a compatibility release boundary.

The implementation deliberately performs validation only for the two known 3.8
contracts. Unknown and legacy model strings continue to pass through, preserving
forward compatibility and existing behavior.

Prices and data-processing terms must be rechecked before deployment. As of
2026-09-18, Google listed both 3.8 Live models at the same paid rates per one
million tokens: text input $0.75, audio input $3.00, image/video input $1.00,
text output $4.50, and audio output $12.00. This ADR does not make those rates or
any GDPR deployment conclusion part of the software contract.

## Sources verified on 2026-09-18

- <https://ai.google.dev/gemini-api/docs/models/gemini-3.8-live>
- <https://ai.google.dev/gemini-api/docs/models/gemini-3.8-live-extended-thinking>
- <https://ai.google.dev/gemini-api/docs/live-api/capabilities>
- <https://ai.google.dev/gemini-api/docs/live-api/live-translate>
- <https://ai.google.dev/gemini-api/docs/live-api/tools>
- <https://ai.google.dev/gemini-api/docs/live-api/session-management>
- <https://ai.google.dev/gemini-api/docs/live-api/ephemeral-tokens>
- <https://ai.google.dev/api/live>
- <https://ai.google.dev/gemini-api/pricing>
- <https://ai.google.dev/gemini-api/terms>
- <https://ai.google.dev/gemini-api/docs/changelog> — 3.8 Live GA announcement,
  2026-09-15
- <https://github.com/googleapis/js-genai/blob/main/src/types.ts>
