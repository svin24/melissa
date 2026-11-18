//! Newline-delimited JSON-RPC over a byte stream pair (the agent's stdout/stdin), with an
//! optional recorder that writes both directions to a JSONL transcript ([`super::replay`]).

use std::{io, pin::Pin, sync::Arc};

use agent_client_protocol::Lines;
use futures::{Sink, Stream};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use super::replay::{Direction, Recorder};

/// Longest JSON-RPC line accepted from an agent. Big enough for session replays with file
/// contents; a longer line ends the connection instead of growing memory without bound.
pub const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;

type BoxSink = Pin<Box<dyn Sink<String, Error = io::Error> + Send>>;
type BoxStream = Pin<Box<dyn Stream<Item = io::Result<String>> + Send>>;

/// The ACP crate's line transport over `reader` (agent → us) and `writer` (us → agent).
pub fn lines<R, W>(
    reader: R,
    writer: W,
    recorder: Option<Arc<Recorder>>,
) -> Lines<BoxSink, BoxStream>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let incoming = futures::stream::unfold(
        (BufReader::new(reader), recorder.clone(), false),
        |(mut reader, recorder, failed)| async move {
            if failed {
                return None;
            }
            match read_line(&mut reader).await {
                Ok(Some(line)) => {
                    if let Some(r) = &recorder {
                        r.record(Direction::Recv, &line);
                    }
                    Some((Ok(line), (reader, recorder, false)))
                }
                Ok(None) => None,
                // Report the error once, then end the stream.
                Err(e) => Some((Err(e), (reader, recorder, true))),
            }
        },
    );
    let outgoing = futures::sink::unfold(
        (writer, recorder),
        |(mut writer, recorder), line: String| async move {
            if let Some(r) = &recorder {
                r.record(Direction::Send, &line);
            }
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            Ok::<_, io::Error>((writer, recorder))
        },
    );
    Lines::new(
        Box::pin(outgoing) as BoxSink,
        Box::pin(incoming) as BoxStream,
    )
}

/// Read one line (without the newline). `None` at end of stream. Blank lines are skipped.
pub(crate) async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<String>> {
    loop {
        let mut buf = Vec::new();
        let n = (&mut *reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buf)
            .await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() != Some(&b'\n') && n as u64 > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("agent sent a line longer than {MAX_LINE_BYTES} bytes"),
            ));
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if !line.trim().is_empty() {
            return Ok(Some(line.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_lines_and_skips_blanks() {
        let data: &[u8] = b"{\"a\":1}\r\n\n{\"b\":2}";
        let mut reader = BufReader::new(data);
        assert_eq!(read_line(&mut reader).await.unwrap().unwrap(), "{\"a\":1}");
        assert_eq!(read_line(&mut reader).await.unwrap().unwrap(), "{\"b\":2}");
        assert!(read_line(&mut reader).await.unwrap().is_none());
    }
}
