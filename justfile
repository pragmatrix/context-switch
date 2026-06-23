knife:
    AUDIO_KNIFE_TRACES=/tmp cargo run -p audio-knife

knife-release:
    cargo run -p audio-knife --release

transcribe-azure-de-en:
    cargo run --example transcribe -- azure --language de-DE,en-US

transcribe-azure-es-de-en:
    cargo run --example transcribe -- azure --language es-ES,de-DE,en-US

transcribe-azure-diarization:
    cargo run --example transcribe -- azure --diarization

transcribe-azure-diarization-de:
    cargo run --example transcribe -- azure --diarization --language de-DE

transcribe-google-de-en:
    cargo run --example transcribe -- google --language de-DE,en-US --model chirp_3 --region eu

transcribe-google-latest-short:
    cargo run --example transcribe -- google --language de-DE --model latest_short --region eu

transcribe-google-latest-long:
    cargo run --example transcribe -- google --language de-DE --model latest_long --region eu

transcribe-voice-live-de:
    cargo run --example transcribe -- voice-live --language de-DE

transcribe-google-diarization:
    cargo run --example transcribe -- google --diarization --language de-DE --model chirp_3 --region eu

openai-dialog-realtime-2:
    cargo run --example openai-dialog -- --protocol openai --model gpt-realtime-2

dialog-prompt provider fifo="/tmp/context-switch-dialog.fifo":
    rm -f {{fifo}}
    mkfifo {{fifo}}
    @echo "Dialog FIFO ready at {{fifo}}"
    @echo "Send prompts with: just dialog-prompt-* fifo={{fifo}}"
    while cat {{fifo}}; do :; done | cargo run --example dialog -- {{provider}}

dialog-prompt-critic fifo="/tmp/context-switch-dialog.fifo":
    printf '%s\n' "Switch to devil's advocate mode and challenge my last statement with two concrete counterexamples." > {{fifo}}

dialog-prompt-twist fifo="/tmp/context-switch-dialog.fifo":
    printf '%s\n' "Give me a surprising analogy for this topic using a restaurant kitchen, then ask me one follow-up question." > {{fifo}}

dialog-prompt-text fifo="/tmp/context-switch-dialog.fifo":
    printf '%s\n' "Say the following words exactly in German (don't add other words): 'My confidence is at 110 percent, my plan is at 60 percent, and my coffee is doing the remaining 30.'" > {{fifo}}
