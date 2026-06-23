# Provider-specific turn-detection fields instead of a mapped neutral value

The shared `TurnDetection` abstraction in `context_switch_core` is used by both Deepgram Flux
and Voice Live, but the two backends model end-of-turn aggressiveness incompatibly: Deepgram
takes confidence floats (`eot_threshold` 0.5–0.9, `eager_eot_threshold` 0.3–0.9) while Voice Live
takes a categorical `threshold_level` (low/medium/high). Rather than invent a single neutral
value and translate it per backend, we expose both shapes side by side — `threshold` /
`eager_threshold` (Deepgram-only) and `threshold_level` (Voice Live-only) — and let each
converter forward only the fields it understands.

## Considered Options

- **Bucket a neutral float into Voice Live levels** (e.g. split Deepgram's 0.5–0.9 range into
  thirds, inverted). Rejected: the cutoffs are arbitrary magic numbers, the direction is
  inverted and confusing, and the mapping is lossy in both directions.
- **A richer enum that can express both shapes.** Rejected: heavier and couples core to both
  providers' models without removing the underlying conceptual mismatch.

## Consequences

- Callers must know which field applies to their chosen backend; a field meant for the other
  backend is silently ignored.
- New turn-based backends add their own field(s) here rather than reinterpreting existing ones.
