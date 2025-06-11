# Context Switch

Context Switch is a Rust-based framework for building real-time conversational applications with support for multiple modalities (audio and text). It provides a unified interface for interacting with various speech and language services like Azure Speech Services and OpenAI.

## Features

- Multi-modal conversation support (audio and text)
- Pluggable service architecture
- Integration with:
  - Azure Speech Services (transcription, translation, synthesis)
  - OpenAI dialog services
- Asynchronous processing using Tokio

## Project Structure

- `core/`: Core functionality and interfaces
- `services/`: Implementation of various service integrations
  - `azure/`: Azure Speech Services integration
  - `google-transcribe/`: Google Speech-to-Text integration (WIP)
  - `openai-dialog/`: OpenAI conversational services integration
- `audio-knife/`: WebSocket server that implements the [mod_audio_fork](https://github.com/questnet/freeswitch-modules/tree/questnet/mod_audio_fork) protocol for real-time audio streaming from telephony systems via [FreeSWITCH](https://signalwire.com/freeswitch). Provides a bridge between audio sources and the Context Switch framework.
- `examples/`: Example applications showcasing different features

## Getting Started

### Prerequisites

- Rust
- API keys for the services you intend to use:
  - OpenAI API key
  - Azure Speech Services subscription key
  - Google Cloud API key (for Google transcription)
- For Aristech services:
  - Install protoc
    - macOS: `brew install protobuf`
    - Linux: `apt-get install protobuf-compiler`

### Installation

1. Clone the repository:
   ```bash
   git clone https://github.com/pragmatrix/context-switch.git
   cd context-switch
   ```

2. Initialize submodules:
   ```bash
   git submodule update --init --recursive
   ```

3. Create a `.env` file with your API keys (see `.env.example` for reference)

### Running Examples

The project includes several examples showcasing different functionalities:

```bash
# Run OpenAI dialog example
cargo run --example openai-dialog

# Run Azure transcribe example
cargo run --example azure-transcribe

# Run Azure synthesize example
cargo run --example azure-synthesize
```

### Using Audio Knife

Audio Knife is a WebSocket server that implements the mod_audio_fork protocol, allowing real-time audio streaming from and to FreeSWITCH. It acts as a bridge between audio sources and the Context Switch framework.

To run the Audio Knife server:

```bash
cargo run -p audio-knife
```

By default, it listens on `127.0.0.1:8123`. You can customize the address by setting the `AUDIO_KNIFE_ADDRESS` environment variable.

## Configuration

Configure the services by setting the appropriate environment variables in your `.env` file:

```
# OpenAI Configuration
OPENAI_API_KEY=your_openai_key
OPENAI_REALTIME_API_MODEL=gpt-4o-mini-realtime-preview

# Azure Configuration
AZURE_SUBSCRIPTION_KEY=your_azure_key
AZURE_REGION=your_azure_region

# Audio Knife Configuration
AUDIO_KNIFE_ADDRESS=127.0.0.1:8123
```

## License

[MIT License](LICENSE) 