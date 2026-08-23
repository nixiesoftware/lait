use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::protocol::{
    read_frame, write_frame, CallbackResponse, Operation, Ready, Reply, Request, Response,
};
use crate::PROTOCOL_VERSION;

pub trait Service: Send + Sync + 'static {
    fn descriptor(&self) -> crate::ServiceDescriptor;

    fn call(
        &self,
        operation: &str,
        payload: &[u8],
        host: Arc<dyn Host>,
    ) -> Result<Vec<u8>, String> {
        let _ = (operation, payload, host);
        Err("unsupported World operation".to_string())
    }
}

/// The only route from a World process back into its supervising host.
///
/// Operations and payloads are package-defined, while framing, correlation,
/// authentication, and bounds remain runner-owned.
pub trait Host: Send + Sync + 'static {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String>;
}

/// Serve one World process until its runner asks it to stop.
pub fn serve(
    world: impl Into<String>,
    version: impl Into<String>,
    service: impl Service,
) -> Result<()> {
    let world = world.into();
    let version = version.into();
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .context("bind World runner service")?;
    listener
        .set_nonblocking(true)
        .context("make World runner listener interruptible")?;
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token)
        .map_err(|error| anyhow::anyhow!("mint World runner credential: {error}"))?;
    let token = data_encoding::BASE64URL_NOPAD.encode(&token);
    let ready = Ready {
        protocol: PROTOCOL_VERSION,
        world,
        version,
        address: listener.local_addr().context("read World runner address")?,
        token: token.clone(),
    };
    println!(
        "{}",
        serde_json::to_string(&ready).context("encode World readiness")?
    );
    std::io::stdout()
        .flush()
        .context("publish World readiness")?;

    let service = Arc::new(service);
    let stopping = Arc::new(AtomicBool::new(false));
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("make accepted World connection blocking")?;
                let service = Arc::clone(&service);
                let stopping = Arc::clone(&stopping);
                let token = token.clone();
                thread::spawn(move || handle(stream, &token, service, stopping));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("accept World runner request"),
        }
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    token: &str,
    service: Arc<impl Service>,
    stopping: Arc<AtomicBool>,
) {
    let request = match read_frame::<Request>(&mut stream) {
        Ok(request) => request,
        Err(_) => return,
    };
    let stream = Arc::new(Mutex::new(stream));
    let channel: Arc<dyn Host> = Arc::new(CallbackChannel {
        stream: Arc::clone(&stream),
        token: token.to_string(),
        request: request.id,
        next_callback: std::sync::atomic::AtomicU64::new(1),
    });
    let mut stop_after_reply = false;
    let outcome = if request.protocol != PROTOCOL_VERSION {
        Err(format!(
            "runner protocol {} is unsupported",
            request.protocol
        ))
    } else if request.token != token {
        Err("runner credential refused".to_string())
    } else {
        match request.operation {
            Operation::Ping => Ok(Reply::Pong),
            Operation::Describe => Ok(Reply::Descriptor(service.descriptor())),
            Operation::Stop => {
                stop_after_reply = true;
                Ok(Reply::Stopping)
            }
            Operation::Call { operation, payload } => service
                .call(&operation, &payload, Arc::clone(&channel))
                .map_err(|error| format!("{operation}: {error}"))
                .map(|payload| Reply::Call { payload }),
        }
    };
    let Ok(mut stream) = stream.lock() else {
        return;
    };
    let _ = write_frame(
        &mut *stream,
        &Response::Complete {
            id: request.id,
            outcome,
        },
    );
    if stop_after_reply {
        stopping.store(true, Ordering::Release);
    }
}

struct CallbackChannel {
    stream: Arc<Mutex<TcpStream>>,
    token: String,
    request: u64,
    next_callback: std::sync::atomic::AtomicU64,
}

impl Host for CallbackChannel {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        let callback = self.next_callback.fetch_add(1, Ordering::Relaxed);
        if callback == u64::MAX {
            return Err("World callback id exhausted".to_string());
        }
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| "World callback channel was poisoned".to_string())?;
        write_frame(
            &mut *stream,
            &Response::Callback {
                id: self.request,
                callback,
                operation: operation.to_string(),
                payload: payload.to_vec(),
            },
        )
        .map_err(|error| format!("send World callback: {error}"))?;
        let response: CallbackResponse = read_frame(&mut *stream)
            .map_err(|error| format!("read World callback response: {error}"))?;
        if response.protocol != PROTOCOL_VERSION
            || response.token != self.token
            || response.id != self.request
            || response.callback != callback
        {
            return Err("World callback response did not match its authenticated request".into());
        }
        response.outcome
    }
}
