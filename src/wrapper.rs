use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use russh::{ChannelStream, server::Msg};

use crate::stats::Stats;

pin_project! {
    pub struct Wrapper {
        #[pin]
        inner: ChannelStream<Msg>,
        stats: Arc<Stats>,
    }
}

impl Wrapper {
    pub fn new(inner: ChannelStream<Msg>, stats: Arc<Stats>) -> Self {
        Self { inner, stats }
    }
}

impl hyper::rt::Read for Wrapper {
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

impl hyper::rt::Write for Wrapper {
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
