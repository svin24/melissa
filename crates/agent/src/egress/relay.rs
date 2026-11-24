//! Copy bytes both ways between a sandbox's socket and the upstream host, with an idle
//! timeout (no bytes either way), a byte cap for the whole tunnel and, for web reads, caps
//! per direction and a lifetime.

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

/// How long a tunnel stays open after one side has closed its direction.
const HALF_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Limits of one tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayLimits {
    /// Closed after this long with no bytes either way.
    pub idle: Duration,
    /// Bytes both directions together.
    pub max_bytes: u64,
    /// Bytes client → upstream.
    pub max_up: u64,
    /// Bytes upstream → client.
    pub max_down: u64,
    /// Closed after this long in total.
    pub lifetime: Option<Duration>,
}

/// Why a tunnel ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEnd {
    /// Both sides closed (or one did and the other followed within the grace period).
    Closed,
    Idle,
    ByteCap,
    UploadCap,
    DownloadCap,
    Lifetime,
}

impl RelayEnd {
    /// Words for the audit, `None` for a normal close.
    pub fn reason(self, limits: &RelayLimits) -> Option<String> {
        match self {
            Self::Closed => None,
            Self::Idle => Some(format!("idle for {} s", limits.idle.as_secs())),
            Self::ByteCap => Some(format!("over {} bytes", limits.max_bytes)),
            Self::UploadCap => Some(format!(
                "upload limit of {} bytes reached (web reading is read-only)",
                limits.max_up
            )),
            Self::DownloadCap => Some(format!(
                "download limit of {} bytes reached",
                limits.max_down
            )),
            Self::Lifetime => Some(format!(
                "open longer than {} s",
                limits.lifetime.unwrap_or_default().as_secs()
            )),
        }
    }
}

/// Bytes sent (client → upstream), received, and why the tunnel ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayStats {
    pub up: u64,
    pub down: u64,
    pub end: RelayEnd,
}

/// Relay until both sides are done, the tunnel is idle for `idle`, or `max_bytes` have
/// passed. Returns the bytes sent (client → upstream) and received.
#[cfg(test)]
pub async fn relay<A, B>(client: A, upstream: B, idle: Duration, max_bytes: u64) -> (u64, u64)
where
    A: AsyncRead + AsyncWrite + Send + 'static,
    B: AsyncRead + AsyncWrite + Send + 'static,
{
    let limits = RelayLimits {
        idle,
        max_bytes,
        max_up: u64::MAX,
        max_down: u64::MAX,
        lifetime: None,
    };
    let s = relay_with(client, upstream, limits).await;
    (s.up, s.down)
}

/// Relay until both sides are done or a limit in `limits` is reached.
pub async fn relay_with<A, B>(client: A, upstream: B, limits: RelayLimits) -> RelayStats
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
    let idle = limits.idle;
    let end_at = limits.lifetime.map(|l| start + l);

    let shared = Shared {
        start,
        last: last.clone(),
        total: total.clone(),
        max_bytes: limits.max_bytes,
    };
    let a = Box::pin(pump(
        cr,
        uw,
        up.clone(),
        limits.max_up,
        RelayEnd::UploadCap,
        shared.clone(),
    ));
    let b = Box::pin(pump(
        ur,
        cw,
        down.clone(),
        limits.max_down,
        RelayEnd::DownloadCap,
        shared,
    ));
    tokio::pin!(a, b);
    let (mut a_done, mut b_done) = (false, false);
    // When one side has finished, the other gets a short grace period, not the idle
    // timeout: some servers never answer a half-close, which would pin the tunnel.
    let mut half_closed: Option<Instant> = None;
    let end = loop {
        let since = Duration::from_millis(last.load(Ordering::Relaxed));
        let mut deadline = start + since + idle;
        if let Some(at) = half_closed {
            deadline = deadline.min(at + HALF_CLOSE_GRACE);
        }
        if let Some(at) = end_at {
            deadline = deadline.min(at);
        }
        tokio::select! {
            r = &mut a, if !a_done => { a_done = true; if let Err(cap) = r { break cap; } }
            r = &mut b, if !b_done => { b_done = true; if let Err(cap) = r { break cap; } }
            () = tokio::time::sleep_until(deadline) => {
                let now = Instant::now();
                if end_at.is_some_and(|at| now >= at) {
                    tracing::debug!("egress tunnel closed: lifetime over");
                    break RelayEnd::Lifetime;
                }
                if half_closed.is_some_and(|at| now >= at + HALF_CLOSE_GRACE) {
                    break RelayEnd::Closed;
                }
                // Woken by the deadline computed before the last activity: re-check.
                let since = Duration::from_millis(last.load(Ordering::Relaxed));
                if now >= start + since + idle {
                    tracing::debug!("egress tunnel closed: idle for {idle:?}");
                    break RelayEnd::Idle;
                }
            }
        }
        if a_done && b_done {
            break RelayEnd::Closed;
        }
        if (a_done || b_done) && half_closed.is_none() {
            half_closed = Some(Instant::now());
        }
    };
    RelayStats {
        up: up.load(Ordering::Relaxed),
        down: down.load(Ordering::Relaxed),
        end,
    }
}

/// Copy upstream → client only (a forwarded `GET`: the request went up already), within
/// `max_down`, `idle` and `lifetime`. The client is never read. `up` is 0 in the result.
pub async fn relay_down<R, W>(mut upstream: R, mut client: W, limits: RelayLimits) -> RelayStats
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let start = Instant::now();
    let end_at = limits.lifetime.map(|l| start + l);
    let mut down = 0u64;
    let mut buf = vec![0u8; 16 * 1024];
    let end = loop {
        let mut deadline = Instant::now() + limits.idle;
        if let Some(at) = end_at {
            deadline = deadline.min(at);
        }
        let n = match tokio::time::timeout_at(deadline, upstream.read(&mut buf)).await {
            Err(_) if end_at.is_some_and(|at| Instant::now() >= at) => break RelayEnd::Lifetime,
            Err(_) => break RelayEnd::Idle,
            Ok(Ok(0) | Err(_)) => break RelayEnd::Closed,
            Ok(Ok(n)) => n,
        };
        if down + n as u64 > limits.max_down.min(limits.max_bytes) {
            break RelayEnd::DownloadCap;
        }
        if client.write_all(&buf[..n]).await.is_err() {
            break RelayEnd::Closed;
        }
        down += n as u64;
    };
    let _ = client.shutdown().await;
    RelayStats { up: 0, down, end }
}

#[derive(Clone)]
struct Shared {
    start: Instant,
    last: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    max_bytes: u64,
}

/// One direction, at most `max` bytes. `Err` if a cap was hit (the whole tunnel must close).
async fn pump<R, W>(
    mut r: R,
    mut w: W,
    count: Arc<AtomicU64>,
    max: u64,
    over: RelayEnd,
    s: Shared,
) -> Result<(), RelayEnd>
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
            return Err(RelayEnd::ByteCap);
        }
        // Checked before writing: nothing beyond the cap is ever passed on.
        if count.load(Ordering::Relaxed) + n as u64 > max {
            tracing::info!("egress tunnel closed: {over:?} ({max} bytes)");
            return Err(over);
        }
        if w.write_all(&buf[..n]).await.is_err() {
            break;
        }
        count.fetch_add(n as u64, Ordering::Relaxed);
        let ms = u64::try_from(s.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        s.last.store(ms, Ordering::Relaxed);
    }
    let _ = w.shutdown().await;
    Ok(())
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

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
    async fn a_half_closed_tunnel_ends_after_a_short_grace() {
        let (client, client_peer) = tokio::io::duplex(64);
        let (upstream, _upstream_peer) = tokio::io::duplex(64);
        // The client is gone; the upstream never closes its side.
        drop(client_peer);
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            relay(client, upstream, Duration::from_secs(600), 1 << 20),
        )
        .await
        .unwrap();
        assert_eq!(result, (0, 0));
        assert!(started.elapsed() >= HALF_CLOSE_GRACE);
        assert!(started.elapsed() < Duration::from_secs(5));
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
    fn web_limits() -> RelayLimits {
        RelayLimits {
            idle: Duration::from_secs(5),
            max_bytes: 1 << 20,
            max_up: 100,
            max_down: 1000,
            lifetime: Some(Duration::from_millis(400)),
        }
    }

    #[tokio::test]
    async fn upload_cap_closes_the_tunnel_before_forwarding_the_excess() {
        let (client, mut client_peer) = tokio::io::duplex(64 * 1024);
        let (upstream, mut upstream_peer) = tokio::io::duplex(64 * 1024);
        let relay = tokio::spawn(relay_with(client, upstream, web_limits()));
        client_peer.write_all(&[1u8; 60]).await.unwrap();
        let mut got = [0u8; 60];
        upstream_peer.read_exact(&mut got).await.unwrap();
        client_peer.write_all(&[2u8; 60]).await.unwrap();
        let stats = tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.end, RelayEnd::UploadCap);
        assert_eq!(stats.up, 60, "the chunk over the cap is not forwarded");
        let mut rest = Vec::new();
        upstream_peer.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        assert!(stats
            .end
            .reason(&web_limits())
            .unwrap()
            .contains("read-only"));
    }

    #[tokio::test(start_paused = true)]
    async fn download_cap_and_lifetime_close_the_tunnel() {
        let (client, _client_peer) = tokio::io::duplex(64 * 1024);
        let (upstream, mut upstream_peer) = tokio::io::duplex(64 * 1024);
        let relay = tokio::spawn(relay_with(client, upstream, web_limits()));
        upstream_peer.write_all(&[3u8; 2000]).await.unwrap();
        let stats = relay.await.unwrap();
        assert_eq!(stats.end, RelayEnd::DownloadCap);

        let (client, _c) = tokio::io::duplex(64);
        let (upstream, _u) = tokio::io::duplex(64);
        let started = Instant::now();
        let stats = relay_with(client, upstream, web_limits()).await;
        assert_eq!(stats.end, RelayEnd::Lifetime);
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
