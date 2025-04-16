use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use russh::{ChannelStream, server::Msg};

use crate::helper::Unit;

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub struct Stats {
    connections: AtomicUsize,
    rx: AtomicUsize,
    tx: AtomicUsize,
}

impl Stats {
    pub fn add_connection(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_rx_bytes(&self, n: usize) {
        self.rx.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_tx_bytes(&self, n: usize) {
        self.tx.fetch_add(n, Ordering::Relaxed);
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn rx(&self) -> Unit {
        Unit::new(self.rx.load(Ordering::Relaxed), "B")
    }

    pub fn tx(&self) -> Unit {
        Unit::new(self.tx.load(Ordering::Relaxed), "B")
    }
}

pin_project! {
    pub struct TrackStats {
        #[pin]
        inner: ChannelStream<Msg>,
        stats: Arc<Stats>,
    }
}

impl TrackStats {
    pub fn new(inner: ChannelStream<Msg>, stats: Arc<Stats>) -> Self {
        Self { inner, stats }
    }
}

impl hyper::rt::Read for TrackStats {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let project = self.project();
        let n = unsafe {
            let mut tbuf = tokio::io::ReadBuf::uninit(buf.as_mut());
            match tokio::io::AsyncRead::poll_read(project.inner, cx, &mut tbuf) {
                Poll::Ready(Ok(())) => tbuf.filled().len(),
                other => return other,
            }
        };

        project.stats.add_tx_bytes(n);

        unsafe {
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for TrackStats {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let project = self.project();
        tokio::io::AsyncWrite::poll_write(project.inner, cx, buf).map(|res| {
            res.inspect(|n| {
                project.stats.add_rx_bytes(*n);
            })
        })
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        tokio::io::AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        tokio::io::AsyncWrite::poll_shutdown(self.project().inner, cx)
    }

    fn is_write_vectored(&self) -> bool {
        tokio::io::AsyncWrite::is_write_vectored(&self.inner)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        let project = self.project();
        tokio::io::AsyncWrite::poll_write_vectored(project.inner, cx, bufs).map(|res| {
            res.inspect(|n| {
                project.stats.add_rx_bytes(*n);
            })
        })
    }
}
