//! TCP / TLS stream wrapper for pgwire connections.

use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

pub enum ServerIo {
    Empty,
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    Tls(TlsStream<TcpStream>),
}

impl AsyncRead for ServerIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            ServerIo::Empty => Poll::Pending,
            ServerIo::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(unix)]
            ServerIo::Unix(s) => Pin::new(s).poll_read(cx, buf),
            ServerIo::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ServerIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            ServerIo::Empty => Poll::Pending,
            ServerIo::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(unix)]
            ServerIo::Unix(s) => Pin::new(s).poll_write(cx, buf),
            ServerIo::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            ServerIo::Empty => Poll::Ready(Ok(())),
            ServerIo::Tcp(s) => Pin::new(s).poll_flush(cx),
            #[cfg(unix)]
            ServerIo::Unix(s) => Pin::new(s).poll_flush(cx),
            ServerIo::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            ServerIo::Empty => Poll::Ready(Ok(())),
            ServerIo::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(unix)]
            ServerIo::Unix(s) => Pin::new(s).poll_shutdown(cx),
            ServerIo::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl ServerIo {
    pub fn from_tcp(mut stream: TcpStream) -> Self {
        // Large Bind payloads + small CommandComplete replies interact badly with
        // Nagle/delayed-ACK on loopback; node-pg paid ~45ms/op before this.
        let _ = stream.set_nodelay(true);
        ServerIo::Tcp(stream)
    }

    #[cfg(unix)]
    pub fn from_unix(stream: UnixStream) -> Self {
        ServerIo::Unix(stream)
    }

    pub async fn upgrade_tls(
        self,
        acceptor: &TlsAcceptor,
    ) -> std::io::Result<Self> {
        match self {
            ServerIo::Tcp(tcp) => {
                let tls = acceptor.accept(tcp).await?;
                Ok(ServerIo::Tls(tls))
            }
            other => Ok(other),
        }
    }
}
