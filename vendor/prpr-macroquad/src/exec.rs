use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// Returns Pending as long as its inner bool is false.
#[derive(Default)]
pub struct FrameFuture {
    done: bool,
}

impl Future for FrameFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context) -> Poll<Self::Output> {
        if self.done {
            // We were told to step, meaning this future gets destroyed and we run
            // the main future until we call next_frame again and end up in this poll
            // function again.
            Poll::Ready(())
        } else {
            self.done = true;
            Poll::Pending
        }
    }
}

type FileResult<T> = Result<T, crate::file::FileError>;

pub struct FileLoadingFuture {
    pub contents: Arc<Mutex<Option<FileResult<Vec<u8>>>>>,
}

impl Future for FileLoadingFuture {
    type Output = FileResult<Vec<u8>>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context) -> Poll<Self::Output> {
        let mut contents = self.contents.lock().unwrap();
        if let Some(contents) = contents.take() {
            Poll::Ready(contents)
        } else {
            Poll::Pending
        }
    }
}

fn waker() -> Waker {
    unsafe fn clone(data: *const ()) -> RawWaker {
        RawWaker::new(data, &VTABLE)
    }
    unsafe fn wake(_data: *const ()) {
        /* 【话梅 2026-11-17】这里原本直接 panic：
             "macroquad does not support waking futures, please use coroutines,
              otherwise your pending future will block until the next frame"

           改成空操作的原因：
           · macroquad 的主循环**每帧无条件 poll**（见本文件 resume()），
             丢弃一次唤醒最多让 future 晚一帧推进，不会永远卡住。
             这正是原提示里 "will block until the next frame" 的含义。
           · 而 panic 的代价是 tokio worker 线程直接死掉，那个线程上
             正在进行的上报/请求会静默丢失（实测 3 次启动触发 2 次，
             且功能上看不出症状）。
           两害相权：宁可晚一帧，也不要死线程。 */
    }
    unsafe fn wake_by_ref(data: *const ()) {
        wake(data)
    }
    unsafe fn drop(_data: *const ()) {
        // Nothing to do
    }
    const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    let raw_waker = RawWaker::new(std::ptr::null(), &VTABLE);
    unsafe { Waker::from_raw(raw_waker) }
}

/// returns Some(T) if future is done, None if it would block
pub(crate) fn resume<T>(future: &mut Pin<Box<dyn Future<Output = T>>>) -> Option<T> {
    let waker = waker();
    let mut futures_context = std::task::Context::from_waker(&waker);
    match future.as_mut().poll(&mut futures_context) {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}
