# Audio Test Tool

This tool helps validate whether audio files can be processed correctly by the context-switch library.
It walks a directory tree, finds audio files, and tests if they can be read and converted to one-second frames.

## Usage

```
audio-test --path <DIRECTORY> [OPTIONS]
```

### Options

- `--path`, `-p`: Path to directory containing audio files (required)
- `--sample-rate`, `-s`: Sample rate for output format (default: 16000)
- `--extensions`, `-e`: Only check files with these extensions (e.g., mp3, wav)
- `--list-only`, `-l`: List files only, don't process them

### Example

```bash
# Test all audio files in a directory and its subdirectories
cargo run -p audio-test -- --path /path/to/audio/files

# Only test WAV and MP3 files
cargo run -p audio-test -- --path /path/to/audio/files --extensions wav mp3

# List all audio files that would be processed
cargo run -p audio-test -- --path /path/to/audio/files --list-only
```

## How it works

The tool:
1. Recursively walks the specified directory tree
2. For each file with matching extensions (or all files if no extensions are specified)
3. Attempts to decode the audio file using Rodio
4. Converts the audio to one-second frames in the specified format (mono, 16-bit PCM)
5. Reports success or failure for each file
