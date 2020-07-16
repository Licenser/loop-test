use async_channel::{bounded, Sender, TrySendError};
use async_std::stream::Fuse;
use async_std::stream::Stream;
use async_std::stream::StreamExt;
use async_std::task;
use core::pin::Pin;
use core::task::{Context, Poll};
use pin_project_lite::pin_project;
use std::fmt;

pub struct TrySender<M: Send> {
    addr: Sender<M>,
    pending: Vec<M>,
    pending2: Vec<M>,
}

impl<M: Send> std::fmt::Debug for TrySender<M>
where
    Sender<M>: std::fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.addr)
    }
}

impl<M: Send> Clone for TrySender<M> {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr.clone(),
            pending: Vec::new(),
            pending2: Vec::new(),
        }
    }
}

impl<M: Send> From<Sender<M>> for TrySender<M> {
    fn from(addr: Sender<M>) -> Self {
        Self {
            addr,
            pending: Vec::new(),
            pending2: Vec::new(),
        }
    }
}

impl<M: Send> TrySender<M> {
    pub(crate) fn try_send_safe(&mut self, msg: M) {
        match self.addr.try_send(msg) {
            Err(TrySendError::Full(msg)) => {
                self.pending.push(msg);
            }
            Ok(_) | Err(_) => (),
        }
    }
    pub(crate) fn ready(&self) -> bool {
        let not_full = !self.addr.is_full();
        let no_pending = self.pending.is_empty();
        not_full && no_pending
    }

    pub(crate) fn drain_ready(&mut self) -> bool {
        let some_pending = !self.pending.is_empty();
        let not_full = !self.addr.is_full();
        if some_pending && not_full {
            std::mem::swap(&mut self.pending, &mut self.pending2);
            let mut bad = false;
            for msg in self.pending2.drain(..) {
                if bad {
                    self.pending.push(msg)
                } else {
                    match self.addr.try_send(msg) {
                        Err(TrySendError::Full(msg)) => {
                            self.pending.push(msg);
                            bad = true
                        }
                        Ok(()) | Err(_) => (),
                    }
                }
            }
        }
        let ready = self.ready();
        ready
    }

    pub(crate) async fn send(&self, msg: M) {
        self.addr.send(msg).await;
    }

    pub(crate) fn maybe_send(&self, msg: M) -> bool {
        self.addr.try_send(msg).is_ok()
    }
}

pin_project! {
    /// A stream that merges two other streams into a single stream.
    ///
    /// This `struct` is created by the [`merge`] method on [`Stream`]. See its
    /// documentation for more.
    ///
    /// [`merge`]: trait.Stream.html#method.merge
    /// [`Stream`]: trait.Stream.html
    #[cfg_attr(feature = "docs", doc(cfg(unstable)))]
    #[derive(Debug)]
    pub struct PriorityMerge<High, Low> {
        #[pin]
        high: Fuse<High>,
        #[pin]
        low: Fuse<Low>,
    }
}

impl<High: Stream, Low: Stream> PriorityMerge<High, Low> {
    pub(crate) fn new(high: High, low: Low) -> Self {
        Self {
            high: high.fuse(),
            low: low.fuse(),
        }
    }
}

impl<High, Low, T> Stream for PriorityMerge<High, Low>
where
    High: Stream<Item = T>,
    Low: Stream<Item = T>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        match this.high.poll_next(cx) {
            Poll::Ready(None) => this.low.poll_next(cx),
            Poll::Ready(item) => Poll::Ready(item),
            Poll::Pending => match this.low.poll_next(cx) {
                Poll::Ready(None) | Poll::Pending => Poll::Pending,
                Poll::Ready(item) => Poll::Ready(item),
            },
        }
    }
}

#[async_std::main]
async fn main() {
    let (f_tx, f_rx) = bounded(8);
    let mut f_tx = TrySender::from(f_tx).clone();
    let (r_tx, r_rx) = bounded(128);
    let (on_tx, on_rx) = bounded(128);
    let (off_tx, off_rx) = bounded(128);
    task::spawn(async move {
        loop {
            if let Ok(m) = off_rx.try_recv() {
                r_tx.send(m).await;
            }
        }
    });

    task::spawn(async move {
        enum M {
            F(u64),
            R(u64),
        }

        let mut s = PriorityMerge::new(r_rx.map(M::R), f_rx.map(M::F));
        // let mut s = r_rx.map(M::R).merge(f_rx.map(M::F));

        while let Some(m) = s.next().await {
            match m {
                M::F(m) => off_tx.send(m).await,
                M::R(m) => on_tx.send(m).await,
            };
        }
    });

    let mut i: u64 = 0;
    loop {
        while let Ok(r) = on_rx.try_recv() {
            if r % 10 == 0 {
                println!("< {}", r)
            }
        }

        // we can simulate this w/o the try_sender but
        // there is a potential deadlock doing that
        // so we avoid it

        if !f_tx.drain_ready() {
            continue;
        }

        if i % 10 == 0 {
            println!("> {}", i);
        }
        f_tx.try_send_safe(i);

        i += 1;
    }
}
