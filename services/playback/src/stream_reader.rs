use std::{io, pin::Pin};

use futures_util::Stream;

/// A wrapper that converts a reqwest bytes stream into a Read + Seek implementation
/// Note: Seek operations will return errors since streams are not seekable
pub struct StreamReader {
    stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + Sync>>,
    current_chunk: Option<bytes::Bytes>,
    chunk_offset: usize,
    runtime: tokio::runtime::Handle,
}

impl StreamReader {
    pub fn new(
        stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            stream: Box::pin(stream),
            current_chunk: None,
            chunk_offset: 0,
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl io::Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // If we have a current chunk with remaining data, use it
        if let Some(ref chunk) = self.current_chunk {
            if self.chunk_offset < chunk.len() {
                let remaining_in_chunk = chunk.len() - self.chunk_offset;
                let to_copy = std::cmp::min(buf.len(), remaining_in_chunk);

                buf[..to_copy]
                    .copy_from_slice(&chunk[self.chunk_offset..self.chunk_offset + to_copy]);
                self.chunk_offset += to_copy;

                return Ok(to_copy);
            }
        }

        // Need to get the next chunk from the stream
        let next_chunk = self
            .runtime
            .block_on(async { futures_util::StreamExt::next(&mut self.stream).await });

        match next_chunk {
            Some(Ok(chunk)) => {
                if chunk.is_empty() {
                    return Ok(0);
                }

                let to_copy = std::cmp::min(buf.len(), chunk.len());
                buf[..to_copy].copy_from_slice(&chunk[..to_copy]);

                if to_copy < chunk.len() {
                    // Store the remainder for the next read
                    self.current_chunk = Some(chunk);
                    self.chunk_offset = to_copy;
                } else {
                    // Used the entire chunk
                    self.current_chunk = None;
                    self.chunk_offset = 0;
                }

                Ok(to_copy)
            }
            Some(Err(e)) => Err(io::Error::other(e)),
            None => Ok(0), // End of stream
        }
    }
}

impl io::Seek for StreamReader {
    fn seek(&mut self, _pos: io::SeekFrom) -> io::Result<u64> {
        // Streams are not seekable, return an error
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Seek not supported on streaming data",
        ))
    }
}
