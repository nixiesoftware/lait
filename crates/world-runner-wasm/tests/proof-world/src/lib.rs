//! A minimal World runner, compiled to wasm to prove the ABI end to end.
//!
//! It is real trait code — a [`world_runner::Service`] answering named
//! operations and calling its [`world_runner::Host`] — so the exercise proves
//! the boundary carries the actual contract, not a bespoke test shim.

use std::sync::Arc;

use world_runner::{Host, Service, ServiceDescriptor};

struct ProofWorld {
    descriptor: ServiceDescriptor,
}

impl Service for ProofWorld {
    fn descriptor(&self) -> ServiceDescriptor {
        self.descriptor.clone()
    }

    fn call(&self, operation: &str, payload: &[u8], host: Arc<dyn Host>) -> Result<Vec<u8>, String> {
        match operation {
            // Proves a synchronous host callback round-trips: ask the host to
            // "ping", then return the host's answer with the payload appended.
            "echo" => {
                let mut answer = host.call("ping", payload)?;
                answer.extend_from_slice(payload);
                Ok(answer)
            }
            // Proves trap recovery: a guest trap must surface as an error the
            // host maps to unavailability, after which it re-instantiates.
            "trap" => {
                unreachable!("proof-world was asked to trap");
            }
            // Proves a large payload crosses linear memory intact.
            "len" => Ok((payload.len() as u64).to_le_bytes().to_vec()),
            // Proves the request deadline bites: loop until the host's epoch
            // interruption traps this guest.
            "spin" => {
                #[allow(clippy::empty_loop)]
                loop {}
            }
            // Proves the memory ceiling bites: allocate past the host's limit
            // so the guest's grow is denied and the allocator aborts (a trap).
            "hog" => {
                let mut chunks: Vec<Vec<u8>> = Vec::new();
                loop {
                    chunks.push(vec![0xff_u8; 8 * 1024 * 1024]);
                    std::hint::black_box(&chunks);
                }
            }
            other => Err(format!("proof-world has no operation {other:?}")),
        }
    }
}

world_runner::export_world_runner!(|init| {
    Arc::new(ProofWorld {
        descriptor: ServiceDescriptor {
            world: init.world,
            implementation: [7; 32],
            implementation_version: 1,
        },
    })
});
