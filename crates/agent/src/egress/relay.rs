//! Copy bytes both ways between a sandbox's socket and the upstream host, with an idle
//! timeout (no bytes either way) and a byte cap for the whole tunnel.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::Instant,
};

/// Relay until both sides are done, the tunnel is idle for `idle`, or `max_bytes` have
/// passed. Returns the bytes sent (client → upstream) and received.
pub async fn relay<A, B>(client: A, upstream: B, idle: Duration, max_bytes: u64) -> (u64, u64)
where
    A: AsyncRead + AsyncWrite + Send + 'static,
    B: AsyncRead + AsyncWrite + Send + 'static,
{
    let (cr, cw) = tokio::io::split(client);
    let (ur, uw) = tokio::io::split(upstream);
    let start = Instant::now();
    // Milliseconds since `start` of the last byte, either direction.
    let last = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let up = Arc::new(AtomicU64::new(0));
    let down = Arc::new(AtomicU64::new(0));

    let shared = Shared {
        start,
        last: last.clone(),
        total: total.clone(),
        max_bytes,
    };
    let a = Box::pin(pump(cr, uw, up.clone(), shared.clone()));
    let b = Box::pin(pump(ur, cw, down.clone(), shared));
    tokio::pin!(a, b);
    let (mut a_done, mut b_done) = (false, false);
    loop {
        let since = Duration::from_millis(last.load(Ordering::Relaxed));
        let deadline = start + since + idle;
        tokio::select! {
            ok = &mut a, if !a_done => { a_done = true; if !ok { break; } }
            ok = &mut b, if !b_done => { b_done = true; if !ok { break; } }
            () = tokio::time::sleep_until(deadline) => {
                // Woken by the deadline computed before the last activity: re-check.
                let since = Duration::from_millis(last.load(Ordering::Relaxed));
                if Instant::now() >= start + since + idle {
                    tracing::debug!("egress tunnel closed: idle for {idle:?}");
                    break;
                }
            }
        }
        if a_done && b_done {
            break;
        }
    }
    (up.load(Ordering::Relaxed), down.load(Ordering::Relaxed))
}

#[derive(Clone)]
struct Shared {
    start: Instant,
    last: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    max_bytes: u64,
}

/// One direction. Returns false if the byte cap was hit (the whole tunnel must close).
async fn pump<R, W>(mut r: R, mut w: W, count: Arc<AtomicU64>, s: Shared) -> bool
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let used = s.total.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        if used > s.max_bytes {
            tracing::info!("egress tunnel closed: over {} bytes", s.max_bytes);
            return false;
        }
        if w.write_all(&buf[..n]).await.is_err() {
            break;
        }
        count.fetch_add(n as u64, Ordering::Relaxed);
        let ms = u64::try_from(s.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        s.last.store(ms, Ordering::Relaxed);
    }
    let _ = w.shutdown().await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copies_both_ways_and_reports_bytes() {
        let (client, mut client_peer) = tokio::io::duplex(1024);
        let (upstream, mut upstream_peer) = tokio::io::duplex(1024);
        let relay = tokio::spawn(relay(client, upstream, Duration::from_secs(5), 1 << 20));
        client_peer.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        upstream_peer.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        upstream_peer.write_all(b"world!").await.unwrap();
        let mut buf = [0u8; 6];
        client_peer.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"world!");
        drop(client_peer);
        drop(upstream_peer);
        assert_eq!(relay.await.unwrap(), (5, 6));
    }

    #[tokio::test]
    async fn idle_tunnels_are_closed() {
        let (client, mut client_peer) = tokio::io::duplex(64);
        let (upstream, mut upstream_peer) = tokio::io::duplex(64);
        let started = Instant::now();
        let done = tokio::spawn(relay(client, upstream, Duration::from_millis(300), 1 << 20));
        // Activity pushes the deadline out.
        tokio::time::sleep(Duration::from_millis(200)).await;
        client_peer.write_all(b"x").await.unwrap();
        let mut b = [0u8; 1];
        upstream_peer.read_exact(&mut b).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, (1, 0));
        assert!(started.elapsed() >= Duration::from_millis(450));
    }

    #[tokio::test]
    async fn byte_cap_closes_the_tunnel() {
        let (client, mut client_peer) = tokio::io::duplex(64 * 1024);
        let (upstream, _upstream_peer) = tokio::io::duplex(64 * 1024);
        let relay = tokio::spawn(relay(client, upstream, Duration::from_secs(5), 1000));
        client_peer.write_all(&[7u8; 4000]).await.unwrap();
        let (up, _) = tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .unwrap()
            .unwrap();
        assert!(up <= 1000, "{up}");
    }
}
