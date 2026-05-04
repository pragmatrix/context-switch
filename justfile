knife:
    AUDIO_KNIFE_TRACES=/tmp cargo run -p audio-knife

knife-release:
    cargo run -p audio-knife --release

transcribe-azure-de-en:
    cargo run --example transcribe -- azure --language de-DE,en-US

transcribe-google-de-en:
    cargo run --example transcribe -- google --language de-DE,en-US
