//! A DNS sinkhole for inside the sandbox: every `A` question gets the same placeholder
//! address, every other question an empty answer. Nothing is forwarded anywhere, so DNS
//! cannot carry data out of the sandbox.
//!
//! Why: some CLIs look a name up themselves before they use the proxy. Grok's `web_fetch`
//! (a static musl binary: `/etc/resolv.conf` and UDP port 53, no NSS) checks the address
//! against its own SSRF rules, then connects through `GROK_WEB_FETCH_PROXY`; with no resolver
//! it fails with "DNS resolution failed". The placeholder satisfies that check; the request
//! then reaches Melissa's proxy by name, and the proxy resolves it for real and applies its
//! own rules. A program that connects to the placeholder directly gets "network
//! unreachable" (the sandbox has only loopback).
//!
//! Port 53 is privileged, and bwrap's `--dev` runs the sandbox in a nested user namespace
//! where no capability counts for the sandbox's network namespace. The lead agent, though,
//! owns the namespace that does (it started bwrap), so it may bind there. Hand-over:
//! `melissa-netbridge` creates an unbound UDP socket inside the sandbox, passes it over
//! `dns.sock` (`SCM_RIGHTS`), and the lead agent binds it to `127.0.0.1:53` and answers on it
//! ([`serve_handoff`]). The sandbox's `/etc/resolv.conf` names `127.0.0.1`.

use std::{
    io::IoSliceMut,
    mem::MaybeUninit,
    net::{Ipv4Addr, SocketAddrV4},
};

use rustix::net::{
    sockopt, AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SocketType,
};
use tokio::{
    io::{AsyncWriteExt, Interest},
    net::{UdpSocket, UnixListener, UnixStream},
};

/// The placeholder every name resolves to inside the sandbox. It must look public: CLIs'
/// own SSRF checks refuse private and documentation addresses (Grok: "SSRF blocked: ...
/// resolves to private/internal IP 203.0.113.53"). From the deprecated 6to4 relay anycast
/// prefix 192.88.99.0/24 (RFC 7526): no legitimate destination; the sandbox has no route to
/// it, and the egress proxy treats the prefix as non-public, so a CLI that connects through
/// the proxy by this address instead of the name is refused.
pub const PLACEHOLDER: Ipv4Addr = Ipv4Addr::new(192, 88, 99, 53);
/// Where the sinkhole answers inside the sandbox.
pub const DNS_ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 53);
/// The sandbox's `/etc/resolv.conf`.
pub const RESOLV_CONF_TEXT: &str = "nameserver 127.0.0.1\noptions attempts:2 timeout:2\n";
/// Hand-over answers on `dns.sock`.
pub const HANDOFF_OK: u8 = b'1';
pub const HANDOFF_FAILED: u8 = b'0';

/// Seconds a sinkhole answer may be cached.
const TTL: u32 = 5;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const RCODE_FORMERR: u8 = 1;
const RCODE_NOTIMP: u8 = 4;

/// The reply to one DNS query (UDP wire format), or `None` for something that is not a
/// query at all (a response, or too short to carry an id): those are dropped.
pub fn sinkhole_reply(query: &[u8], answer: Ipv4Addr) -> Option<Vec<u8>> {
    let header = query.get(..12)?;
    let flags_hi = header[2];
    if flags_hi & 0x80 != 0 {
        return None; // a response, not a query
    }
    let opcode = (flags_hi >> 3) & 0x0f;
    let rd = flags_hi & 0x01;
    let reply_header = |rcode: u8, qd: u16, an: u16| {
        let mut h = Vec::with_capacity(12);
        h.extend_from_slice(&header[..2]); // id
        h.push(0x80 | (opcode << 3) | 0x04 | rd); // QR, opcode, AA, RD
        h.push(0x80 | rcode); // RA, rcode
        h.extend_from_slice(&qd.to_be_bytes());
        h.extend_from_slice(&an.to_be_bytes());
        h.extend_from_slice(&[0, 0, 0, 0]); // no authority, no additional records
        h
    };
    if opcode != 0 {
        return Some(reply_header(RCODE_NOTIMP, 0, 0));
    }
    let qdcount = u16::from_be_bytes([header[4], header[5]]);
    let Some(end) = (qdcount == 1).then(|| question_end(query)).flatten() else {
        return Some(reply_header(RCODE_FORMERR, 0, 0));
    };
    let question = &query[12..end];
    let qtype = u16::from_be_bytes([query[end - 4], query[end - 3]]);
    let qclass = u16::from_be_bytes([query[end - 2], query[end - 1]]);
    let is_a = qtype == TYPE_A && qclass == CLASS_IN;
    let mut out = reply_header(0, 1, u16::from(is_a));
    out.extend_from_slice(question);
    if is_a {
        out.extend_from_slice(&[0xc0, 0x0c]); // the name: pointer to the question
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&TTL.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&answer.octets());
    }
    Some(out)
}

/// Index just past the first question (name, type, class), if it is well formed. Queries
/// carry no compression pointers.
fn question_end(msg: &[u8]) -> Option<usize> {
    let mut i = 12;
    let mut name_len = 0usize;
    loop {
        let len = usize::from(*msg.get(i)?);
        i += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None;
        }
        name_len += len + 1;
        if name_len > 255 {
            return None;
        }
        i += len;
    }
    let end = i + 4;
    (end <= msg.len()).then_some(end)
}

/// Lead side of `dns.sock`: take the first socket handed over, bind it to [`DNS_ADDR`] and
/// answer queries on it until dropped. Only one hand-over per sandbox (the bridge's, made
/// before the CLI starts); the listener closes after it.
pub async fn serve_handoff(listener: UnixListener, bind_to: SocketAddrV4) {
    let Ok((mut stream, _)) = listener.accept().await else {
        return;
    };
    drop(listener);
    let socket = match take_socket(&stream, bind_to).await {
        Ok(s) => {
            let _ = stream.write_all(&[HANDOFF_OK]).await;
            s
        }
        Err(e) => {
            tracing::warn!("sandbox DNS not available: {e}");
            let _ = stream.write_all(&[HANDOFF_FAILED]).await;
            return;
        }
    };
    drop(stream);
    sinkhole(socket, PLACEHOLDER).await;
}

/// Receive one UDP/IPv4 socket over `stream` and bind it to `bind_to`. Binding a port below
/// 1024 works because the lead agent owns the sandbox's network namespace.
async fn take_socket(stream: &UnixStream, bind_to: SocketAddrV4) -> std::io::Result<UdpSocket> {
    let fd = loop {
        stream.readable().await?;
        let got = stream.try_io(Interest::READABLE, || {
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let mut buf = [0u8; 16];
            rustix::net::recvmsg(
                stream,
                &mut [IoSliceMut::new(&mut buf)],
                &mut control,
                RecvFlags::CMSG_CLOEXEC,
            )?;
            let mut fd = None;
            for msg in control.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = msg {
                    for f in fds {
                        fd.get_or_insert(f); // further descriptors are closed
                    }
                }
            }
            Ok(fd)
        });
        match got {
            Ok(fd) => break fd,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    };
    let fd = fd.ok_or_else(|| std::io::Error::other("no socket was handed over"))?;
    if sockopt::socket_type(&fd)? != SocketType::DGRAM
        || sockopt::socket_domain(&fd)? != AddressFamily::INET
    {
        return Err(std::io::Error::other("not a UDP/IPv4 socket"));
    }
    rustix::net::bind(&fd, &bind_to)?;
    let socket = std::net::UdpSocket::from(fd);
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket)
}

/// Answer every query on `socket` with [`sinkhole_reply`].
pub async fn sinkhole(socket: UdpSocket, answer: Ipv4Addr) {
    let mut buf = [0u8; 1500];
    loop {
        let Ok((n, from)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(reply) = sinkhole_reply(&buf[..n], answer) {
            let _ = socket.send_to(&reply, from).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        let mut q = id.to_be_bytes().to_vec();
        q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1]); // RD, 1 question, 1 additional
        for label in name.split('.') {
            q.push(u8::try_from(label.len()).unwrap());
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        // An EDNS OPT record (glibc sends one), which the reply drops.
        q.extend_from_slice(&[0, 0, 41, 4, 0xd0, 0, 0, 0, 0, 0, 0]);
        q
    }

    #[test]
    fn a_questions_get_the_placeholder() {
        let q = query(0xbeef, "Example.COM", TYPE_A);
        let r = sinkhole_reply(&q, PLACEHOLDER).unwrap();
        assert_eq!(&r[..2], &[0xbe, 0xef]);
        assert_eq!(r[2], 0x85, "QR, AA, RD");
        assert_eq!(r[3], 0x80, "RA, no error");
        assert_eq!(&r[4..12], &[0, 1, 0, 1, 0, 0, 0, 0]);
        let qlen = 1 + 7 + 1 + 3 + 1 + 4;
        assert_eq!(&r[12..12 + qlen], &q[12..12 + qlen], "question echoed");
        let ans = &r[12 + qlen..];
        assert_eq!(
            ans,
            &[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 5, 0, 4, 192, 88, 99, 53]
        );
    }

    #[test]
    fn other_types_get_an_empty_answer() {
        let r = sinkhole_reply(&query(1, "example.com", 28), PLACEHOLDER).unwrap();
        assert_eq!(r[3] & 0x0f, 0);
        assert_eq!(&r[4..12], &[0, 1, 0, 0, 0, 0, 0, 0], "AAAA: no records");
    }

    #[test]
    fn junk_is_refused_or_dropped() {
        assert_eq!(sinkhole_reply(&[0; 5], PLACEHOLDER), None);
        let mut resp = query(1, "a.b", TYPE_A);
        resp[2] |= 0x80;
        assert_eq!(
            sinkhole_reply(&resp, PLACEHOLDER),
            None,
            "responses dropped"
        );
        // Truncated question, two questions, a label over 63 bytes: FORMERR.
        let q = query(2, "example.com", TYPE_A);
        let mut two = q.clone();
        two[5] = 2;
        let long = query(3, &"a".repeat(64), TYPE_A);
        for bad in [&q[..16], &two[..], &long[..]] {
            let r = sinkhole_reply(bad, PLACEHOLDER).unwrap();
            assert_eq!(r[3] & 0x0f, RCODE_FORMERR, "{bad:?}");
            assert_eq!(&r[4..12], &[0; 8]);
        }
        // A non-query opcode (e.g. UPDATE): NOTIMP.
        let mut update = q.clone();
        update[2] = 5 << 3;
        assert_eq!(
            sinkhole_reply(&update, PLACEHOLDER).unwrap()[3] & 0x0f,
            RCODE_NOTIMP
        );
    }

    /// The hand-over as the bridge does it, on an unprivileged port; then a real query.
    #[tokio::test]
    async fn a_handed_over_socket_is_bound_and_answers() {
        use std::os::fd::AsFd;
        let dir = std::env::temp_dir().join(format!("melissa-dns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dns.sock");
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let bind_to = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        tokio::spawn(serve_handoff(UnixListener::bind(&path).unwrap(), bind_to));

        let path2 = path.clone();
        let ack = tokio::task::spawn_blocking(move || {
            use std::io::{IoSlice, Read};
            let udp = rustix::net::socket(AddressFamily::INET, SocketType::DGRAM, None).unwrap();
            let mut unix = std::os::unix::net::UnixStream::connect(&path2).unwrap();
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
            let fds = [udp.as_fd()];
            assert!(control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
            rustix::net::sendmsg(
                &unix,
                &[IoSlice::new(b"dns")],
                &mut control,
                rustix::net::SendFlags::empty(),
            )
            .unwrap();
            let mut ack = [0u8; 1];
            unix.read_exact(&mut ack).unwrap();
            ack[0]
        })
        .await
        .unwrap();
        assert_eq!(ack, HANDOFF_OK);

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(bind_to).await.unwrap();
        client.send(&query(9, "example.com", TYPE_A)).await.unwrap();
        let mut buf = [0u8; 512];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[n - 4..n], &PLACEHOLDER.octets());
        // The hand-over listener is gone: no second socket can be handed over.
        assert!(std::os::unix::net::UnixStream::connect(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
