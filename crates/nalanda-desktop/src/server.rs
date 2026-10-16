//! Runs the HTTP/OPDS server inside the desktop process, so a phone can
//! browse and download from the same library the desktop is managing.

use std::net::SocketAddr;
use std::sync::Arc;

use nalanda_convert::Converters;
use nalanda_library::Library;

/// Address the embedded server listens on. Defaults to loopback because the
/// API has no authentication yet; set `NALANDA_BIND=0.0.0.0:8083` to expose it
/// on the local network.
pub fn bind_addr() -> SocketAddr {
    std::env::var("NALANDA_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8083)))
}

/// A running embedded server. Dropping it also stops the server.
pub struct Server {
    addr: SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    stopped: std::sync::mpsc::Receiver<()>,
}

impl Server {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops accepting connections and returns once the port is free again.
    /// Transfers still in progress are cut off.
    pub fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.stopped.recv();
    }
}

/// Starts the server on its own thread and Tokio runtime. Returns once the
/// socket is bound, or with the bind error.
pub fn start(library: Arc<Library>, converters: Arc<Converters>) -> anyhow::Result<Server> {
    let addr = bind_addr();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("nalanda-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow::Error::from(e)));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(addr).await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.into()));
                        return;
                    }
                };
                let local = listener.local_addr().unwrap_or(addr);
                let _ = ready_tx.send(Ok(local));
                let app = nalanda_server::router(nalanda_server::AppState {
                    library,
                    converters,
                });
                tokio::select! {
                    result = nalanda_server::serve_listener(listener, app) => {
                        if let Err(e) = result {
                            tracing::error!("server stopped: {e}");
                        }
                    }
                    _ = stop_rx => {}
                }
            });
            // The listener was dropped with the serve future, so the port is free.
            let _ = stopped_tx.send(());
            runtime.shutdown_background();
        })?;
    let addr = ready_rx.recv()??;
    Ok(Server {
        addr,
        stop: stop_tx,
        stopped: stopped_rx,
    })
}
