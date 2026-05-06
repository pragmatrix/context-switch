knife:
    AUDIO_KNIFE_TRACES=/tmp cargo run -p audio-knife

knife-release:
    cargo run -p audio-knife --release

transcribe-azure-de-en:
    cargo run --example transcribe -- azure --language de-DE,en-US

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

transcribe-google-diarization:
    cargo run --example transcribe -- google --diarization --language de-DE --model chirp_3 --region eu

