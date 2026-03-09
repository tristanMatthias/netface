//! Async-to-sync bridge layer.
//!
//! Manages a dedicated tokio runtime thread and provides synchronous wrappers
//! for async operations, allowing the existing std::thread-based code to
//! interact with async Nostr and WebRTC code.

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{bounded, Receiver, Sender};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use super::error::ConnError;

/// Commands that can be sent to the async runtime.
pub enum BridgeCommand {
    /// Shutdown the runtime.
    Shutdown,
    /// Execute an arbitrary async task.
    Execute(Box<dyn FnOnce() + Send + 'static>),
}

/// Response from the async runtime.
pub enum BridgeResponse {
    /// Operation completed successfully.
    Ok,
    /// Operation failed with error.
    Error(ConnError),
}

/// Handle to the async runtime thread.
pub struct AsyncBridge {
    /// Command sender to the runtime thread.
    cmd_tx: Sender<BridgeCommand>,
    /// Join handle for the runtime thread.
    thread_handle: Option<JoinHandle<()>>,
    /// Tokio mpsc sender for async tasks.
    async_tx: mpsc::UnboundedSender<Box<dyn FnOnce() + Send + 'static>>,
}

impl AsyncBridge {
    /// Spawn a new async bridge with a dedicated tokio runtime.
    pub fn spawn() -> Result<Self, ConnError> {
        let (cmd_tx, cmd_rx) = bounded::<BridgeCommand>(32);
        let (async_tx, mut async_rx) = mpsc::unbounded_channel::<Box<dyn FnOnce() + Send + 'static>>();

        let thread_handle = thread::Builder::new()
            .name("netface-async".to_string())
            .spawn(move || {
                let rt = match Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("Failed to create tokio runtime: {e}");
                        return;
                    }
                };

                rt.block_on(async move {
                    loop {
                        tokio::select! {
                            // Handle crossbeam commands
                            _ = tokio::task::spawn_blocking({
                                let cmd_rx = cmd_rx.clone();
                                move || cmd_rx.recv()
                            }) => {
                                match cmd_rx.try_recv() {
                                    Ok(BridgeCommand::Shutdown) => break,
                                    Ok(BridgeCommand::Execute(f)) => f(),
                                    Err(_) => {}
                                }
                            }

                            // Handle async tasks
                            Some(task) = async_rx.recv() => {
                                task();
                            }
                        }
                    }
                });
            })
            .map_err(|e| ConnError::RuntimeError(e.to_string()))?;

        Ok(Self {
            cmd_tx,
            thread_handle: Some(thread_handle),
            async_tx,
        })
    }

    /// Send a command to the async runtime.
    pub fn send_command(&self, cmd: BridgeCommand) -> Result<(), ConnError> {
        self.cmd_tx.send(cmd).map_err(|_| ConnError::BridgeSend)
    }

    /// Spawn an async task.
    pub fn spawn_task<F>(&self, f: F) -> Result<(), ConnError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.async_tx
            .send(Box::new(f))
            .map_err(|_| ConnError::BridgeSend)
    }

    /// Shutdown the async runtime.
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(BridgeCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AsyncBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A bridge for a specific connection that runs in the async runtime.
pub struct ConnectionBridge {
    /// Shared reference to the async bridge.
    bridge: Arc<AsyncBridge>,
    /// Channel for sending data to WebRTC.
    outbound_tx: Sender<(String, Vec<u8>)>,
    /// Channel for receiving data from WebRTC.
    inbound_rx: Receiver<(String, Vec<u8>)>,
}

impl ConnectionBridge {
    /// Create a new connection bridge.
    pub fn new(bridge: Arc<AsyncBridge>) -> (Self, Receiver<(String, Vec<u8>)>, Sender<(String, Vec<u8>)>) {
        let (outbound_tx, outbound_rx) = bounded::<(String, Vec<u8>)>(64);
        let (inbound_tx, inbound_rx) = bounded::<(String, Vec<u8>)>(64);

        let conn_bridge = Self {
            bridge,
            outbound_tx,
            inbound_rx,
        };

        (conn_bridge, outbound_rx, inbound_tx)
    }

    /// Send data on a named channel.
    pub fn send(&self, channel: &str, data: Vec<u8>) -> Result<(), ConnError> {
        self.outbound_tx
            .send((channel.to_string(), data))
            .map_err(|_| ConnError::ChannelClosed)
    }

    /// Receive data from any channel.
    pub fn recv(&self) -> Result<(String, Vec<u8>), ConnError> {
        self.inbound_rx.recv().map_err(|_| ConnError::ChannelClosed)
    }

    /// Try to receive data without blocking.
    pub fn try_recv(&self) -> Option<(String, Vec<u8>)> {
        self.inbound_rx.try_recv().ok()
    }
}

/// Utility for running async code from sync context.
pub fn block_on<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create runtime")
        .block_on(future)
}

/// Create a pair of crossbeam channels for bidirectional communication.
pub fn channel_pair<T>(capacity: usize) -> (Sender<T>, Receiver<T>, Sender<T>, Receiver<T>) {
    let (tx1, rx1) = bounded(capacity);
    let (tx2, rx2) = bounded(capacity);
    (tx1, rx1, tx2, rx2)
}
