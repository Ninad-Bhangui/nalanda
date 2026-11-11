//! Background work that reports back to the UI thread.
//!
//! A job runs on its own thread and sends a closure that applies its result to the app state.
//! `poll` drains them before the frame; the waker makes the window run `poll` when one lands.

use std::sync::mpsc::{Receiver, Sender, channel};

use rune::Waker;

pub type Done<S> = Box<dyn FnOnce(&mut S) + Send>;

pub struct Jobs<S> {
    tx: Sender<Done<S>>,
    rx: Receiver<Done<S>>,
    waker: Option<Waker>,
}

impl<S: 'static> Jobs<S> {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            waker: None,
        }
    }

    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    /// A handle a long-lived thread keeps to report back (the device watcher).
    pub fn sender(&self) -> JobSender<S> {
        JobSender {
            tx: self.tx.clone(),
            waker: self.waker.clone(),
        }
    }

    /// Runs `work` on a thread, then `done` with its result on the UI thread.
    pub fn spawn<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut S, T) + Send + 'static,
    ) {
        let sender = self.sender();
        std::thread::Builder::new()
            .name("nalanda-job".into())
            .spawn(move || {
                let result = work();
                sender.send(move |s| done(s, result));
            })
            .expect("spawn job thread");
    }

    /// Every finished job, to apply to the state.
    pub fn finished(&self) -> Vec<Done<S>> {
        self.rx.try_iter().collect()
    }
}

pub struct JobSender<S> {
    tx: Sender<Done<S>>,
    waker: Option<Waker>,
}

impl<S> JobSender<S> {
    /// False once the app is gone.
    pub fn send(&self, done: impl FnOnce(&mut S) + Send + 'static) -> bool {
        let sent = self.tx.send(Box::new(done)).is_ok();
        if let Some(wake) = &self.waker {
            wake();
        }
        sent
    }
}
