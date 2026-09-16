# ADR 0005: Google transcribe `numerals` as a class-token adaptation hint

Date: 2026-09-16
Status: Accepted

## Context

Deepgram's transcriber exposes a `numerals` option that formats recognized
numbers as digits. Google Cloud Speech-to-Text V2 has no equivalent
formatting toggle: `RecognitionConfig`/`RecognitionFeatures` contain no
numerals field, and Google already emits recognized numbers as digits by
default (built-in ITN).

What Google does offer is *recognition biasing* via `SpeechAdaptation`:
an inline `PhraseSet` whose phrases may contain class tokens such as
`$OOV_CLASS_DIGIT_SEQUENCE` ("nine four one two" → `9412`). This changes
what the recognizer hears, not how the transcript is formatted, but the
observable contract for callers is the same as Deepgram's `numerals`:
numbers come out as digits.

## Decision

- `google_transcribe::Params` gains a `numerals: bool`. When set, the
  service sends an inline `SpeechAdaptation` containing a single phrase
  with the `$OOV_CLASS_DIGIT_SEQUENCE` class token.
- The full class-token vocabulary (27 strings across the `$OOV_CLASS_*`
  and bare `$*` families) is documented as public constants in
  `services/google-transcribe/src/class_tokens.rs`. Only
  `$OOV_CLASS_DIGIT_SEQUENCE` is wired up; the rest exist as reference
  documentation.
- The hint is sent unconditionally when `numerals` is requested. There is
  no model/locale filter: Google publishes no (model × locale) support
  matrix for class tokens (the class-tokens page is locale-only), and
  Google silently ignores tokens unsupported for the request's locale.
- The phrase carries a `boost` of 20.0 (the maximum), taken over from an
  internal project that parameterized Google via FreeSWITCH.
- The example CLI accepts `--numerals` for the Google provider.

## Consequences

- `--numerals` now works for both Deepgram and Google with the same
  observable effect, though the underlying mechanisms differ (formatting
  vs. recognition bias). On Google the effect covers digit sequences
  specifically, not every numeric phrase.
- Token availability depends on the selected model and locale; see
  <https://docs.cloud.google.com/speech-to-text/docs/class-tokens>.
- If a support filter is ever needed, per-locale availability data must be
  sourced fresh from Google's class-tokens page; it is not derivable from
  the API reference or the proto crate.

## Rejected alternative: post-hoc alternative re-ranking

An earlier iteration added a `digitBoost` parameter that added a
confidence bonus to final alternatives whose transcript was digit-only,
plus a `maxAlternatives` parameter (default 4) so Google would return
lower alternatives to boost. It was removed because it cannot work
reliably:

- Google populates `confidence` only on the top alternative of a final
  streaming result; every lower alternative carries `0.0`, which is the
  documented sentinel for "not set", not a real score. Re-ranking by
  confidence therefore compares unknown values against one known value.
- The only reliable ranking signal is Google's own ordering ("alternatives
  are ordered in terms of accuracy, with the top (first) alternative being
  the most probable, as ranked by the recognizer"), which the service now
  follows directly: final results take the first alternative, and
  `max_alternatives` is hardcoded to 1 in `client.rs`.
- Lower alternatives' confidences are still logged for observability, but
  never used for selection.
