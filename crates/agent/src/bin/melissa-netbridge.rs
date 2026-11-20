#![forbid(unsafe_code)]
//! `melissa-netbridge`: runs first inside an agent sandbox that has its own network
//! namespace (loopback only). It listens on loopback TCP ports, forwards every connection
//! to a Unix socket that the lead agent serves (the egress proxy, Melissa's MCP endpoint), then
//! runs the agent CLI as its child with the same stdio and exits with the child's status.
//!
//! Usage: `melissa-netbridge [--tcp <addr>=<unix socket>]... [--dns <unix socket>] -- <program> [args...]`
//!
//! `--dns`: before the CLI starts, create an unbound UDP socket in the sandbox and hand it
//! to the lead agent over that Unix socket; the lead agent binds it to `127.0.0.1:53` (it
//! owns the sandbox's network namespace, the sandbox cannot bind port 53) and answers every
//! name with a placeholder (`melissa_agent::egress::dns`). Failure only costs DNS.
//!
//! It enforces nothing itself: the policy lives lead-side, behind the sockets. It never
//! writes to stdout (an ACP agent speaks JSON-RPC there).

use std::{
    ffi::OsString,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use tokio::{
    net::{TcpListener, UnixStream},
    signal::unix::{signal, SignalKind},
};

const USAGE: &str = "usage: melissa-netbridge [--tcp <addr>=<unix socket>]... \
                     [--dns <unix socket>] -- <program> [args...]";
/// Exit code for the bridge's own setup failures (like `env`/`timeout`).
const SETUP_FAILED: u8 = 125;

#[derive(Debug, PartialEq)]
struct Args {
    ports: Vec<(SocketAddr, PathBuf)>,
    /// Where to hand the sandbox's DNS socket to the lead agent.
    dns: Option<PathBuf>,
    program: OsString,
    args: Vec<OsString>,
}

fn parse(argv: Vec<OsString>) -> Result<Args, String> {
    let mut ports = Vec::new();
    let mut dns = None;
    let mut it = argv.into_iter();
    loop {
        let Some(arg) = it.next() else {
            return Err("missing `-- <program>`".into());
        };
        match arg.to_str() {
            Some("--") => break,
            Some("--tcp") => {
                let spec = it
                    .next()
                    .and_then(|s| s.into_string().ok())
                    .ok_or("--tcp needs <addr>=<socket>")?;
                let (addr, path) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("bad --tcp `{spec}`"))?;
                let addr: SocketAddr = addr
                    .parse()
                    .map_err(|_| format!("bad listen address `{addr}`"))?;
                if !addr.ip().is_loopback() {
                    return Err(format!("{addr} is not a loopback address"));
                }
                ports.push((addr, PathBuf::from(path)));
            }
            Some("--dns") => {
                dns = Some(PathBuf::from(it.next().ok_or("--dns needs <unix socket>")?));
            }
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    let program = it.next().ok_or("missing program after `--`")?;
    Ok(Args {
        ports,
        dns,
        program,
        args: it.collect(),
    })
}

fn main() -> ExitCode {
    let args = match parse(std::env::args_os().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("melissa-netbridge: {e}\n{USAGE}");
            return ExitCode::from(SETUP_FAILED);
        }
    };
    if let Some(path) = &args.dns {
        if let Err(e) = lend_dns_socket(path) {
            eprintln!("melissa-netbridge: no DNS inside the sandbox: {e}");
        }
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("melissa-netbridge: cannot start: {e}");
            return ExitCode::from(SETUP_FAILED);
        }
    };
    rt.block_on(run(args))
}

/// Create an unbound UDP socket here (in the sandbox's network namespace) and pass it to the
/// lead agent over `path`, which binds it to port 53 and answers on it.
fn lend_dns_socket(path: &Path) -> std::io::Result<()> {
    use std::{
        io::{IoSlice, Read},
        mem::MaybeUninit,
        os::fd::AsFd,
    };

    use rustix::net::{AddressFamily, SendAncillaryBuffer, SendAncillaryMessage, SocketType};

    let udp = rustix::net::socket(AddressFamily::INET, SocketType::DGRAM, None)?;
    let mut unix = std::os::unix::net::UnixStream::connect(path)?;
    unix.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    let fds = [udp.as_fd()];
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(std::io::Error::other("control buffer too small"));
    }
    rustix::net::sendmsg(
        &unix,
        &[IoSlice::new(b"dns")],
        &mut control,
        rustix::net::SendFlags::empty(),
    )?;
    let mut ack = [0u8; 1];
    unix.read_exact(&mut ack)?;
    match ack[0] {
        melissa_agent::egress::dns::HANDOFF_OK => Ok(()),
        _ => Err(std::io::Error::other("the lead agent could not bind it")),
    }
}

async fn run(args: Args) -> ExitCode {
    // Listen before the CLI starts, so its first connection finds the port open.
    for (addr, socket) in args.ports {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("melissa-netbridge: cannot listen on {addr}: {e}");
                return ExitCode::from(SETUP_FAILED);
            }
        };
        tokio::spawn(forward(listener, socket));
    }
    let mut child = match tokio::process::Command::new(&args.program)
        .args(&args.args)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("melissa-netbridge: cannot run {:?}: {e}", args.program);
            return ExitCode::from(if e.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    let signals = [
        SignalKind::terminate(),
        SignalKind::interrupt(),
        SignalKind::hangup(),
    ];
    let mut streams = Vec::new();
    for kind in signals {
        match signal(kind) {
            Ok(s) => streams.push(s),
            Err(e) => eprintln!("melissa-netbridge: cannot watch signals: {e}"),
        }
    }
    let any_signal = async {
        let waits = streams.iter_mut().map(|s| Box::pin(s.recv()));
        futures_select_all(waits.collect()).await;
    };
    let status = tokio::select! {
        status = child.wait() => status,
        () = any_signal => {
            // The sandbox is going away; take the CLI with us.
            let _ = child.kill().await;
            child.wait().await
        }
    };
    match status {
        Ok(status) => exit_code(status),
        Err(e) => {
            eprintln!("melissa-netbridge: waiting for the program failed: {e}");
            ExitCode::from(SETUP_FAILED)
        }
    }
}

/// Resolve when any of the futures does.
async fn futures_select_all<F: std::future::Future + Unpin>(futs: Vec<F>) {
    if futs.is_empty() {
        std::future::pending::<()>().await;
    }
    let _ = futures::future::select_all(futs).await;
}

/// The child's exit code, or 128 + signal number like a shell.
fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

async fn forward(listener: TcpListener, socket: PathBuf) {
    loop {
        let Ok((mut tcp, _)) = listener.accept().await else {
            continue;
        };
        let socket = socket.clone();
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            // A refused socket (the lead agent closed this sandbox's sockets) just drops the
            // connection: the client sees a reset, never a way around the proxy.
            if let Ok(mut unix) = UnixStream::connect(&socket).await {
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_ports_and_program() {
        let a = parse(os(&[
            "--tcp",
            "127.0.0.1:3128=/run/melissa-net/egress.sock",
            "--tcp",
            "[::1]:8080=/run/melissa-net/mcp.sock",
            "--",
            "/usr/bin/cli",
            "-p",
            "--tcp",
        ]))
        .unwrap();
        assert_eq!(a.ports.len(), 2);
        assert_eq!(a.ports[1].0, "[::1]:8080".parse().unwrap());
        assert_eq!(a.program, "/usr/bin/cli");
        // Everything after `--` belongs to the program, flags included.
        assert_eq!(a.args, os(&["-p", "--tcp"]));
        let none = parse(os(&["--", "x"])).unwrap();
        assert!(none.ports.is_empty());
        assert_eq!(none.dns, None);
        let dns = parse(os(&["--dns", "/run/melissa-net/dns.sock", "--", "x"])).unwrap();
        assert_eq!(dns.dns, Some(PathBuf::from("/run/melissa-net/dns.sock")));
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            &[][..],
            &["x"],
            &["--"],
            &["--tcp"],
            &["--tcp", "127.0.0.1:1", "--", "x"],
            &["--tcp", "0.0.0.0:1=/s", "--", "x"],
            &["--tcp", "10.0.0.1:1=/s", "--", "x"],
            &["--tcp", "nope=/s", "--", "x"],
            &["--dns"],
        ] {
            assert!(parse(os(bad)).is_err(), "{bad:?}");
        }
    }
}
