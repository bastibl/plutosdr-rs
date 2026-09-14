//! Compose native blocking and asynchronous implementations without block_on.
use nusb::MaybeFuture;
use std::future::{Future, IntoFuture};

#[cfg(not(target_arch = "wasm32"))]
pub use std::marker::Send as PlatformSend;
#[cfg(target_arch = "wasm32")]
pub trait PlatformSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> PlatformSend for T {}

pub(crate) struct Operation<I, B, A> {
    input: I,
    blocking: B,
    asynchronous: A,
}

impl<I, B, A, F: Future> IntoFuture for Operation<I, B, A>
where
    A: FnOnce(I) -> F,
{
    type Output = F::Output;
    type IntoFuture = F;
    fn into_future(self) -> F {
        drop(self.blocking);
        (self.asynchronous)(self.input)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<I, B, A, F> MaybeFuture for Operation<I, B, A>
where
    I: Send,
    B: FnOnce(I) -> F::Output + Send,
    A: FnOnce(I) -> F + Send,
    F: Future + Send,
{
    fn wait(self) -> F::Output {
        (self.blocking)(self.input)
    }
}

#[cfg(target_arch = "wasm32")]
impl<I, A, F: Future> MaybeFuture for Operation<I, (), A> where A: FnOnce(I) -> F {}

pub(crate) fn operation<I, B, A, F: Future>(
    input: I,
    blocking: B,
    asynchronous: A,
) -> Operation<I, B, A>
where
    A: FnOnce(I) -> F,
{
    Operation {
        input,
        blocking,
        asynchronous,
    }
}

macro_rules! dual {
    ($input:expr, $blocking:expr, $asynchronous:expr) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            $crate::maybe_future::operation($input, $blocking, $asynchronous)
        }
        #[cfg(target_arch = "wasm32")]
        {
            $crate::maybe_future::operation($input, (), $asynchronous)
        }
    }};
}
pub(crate) use dual;
