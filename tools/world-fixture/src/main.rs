use world_runner::{Service, ServiceDescriptor};

struct Fixture;

impl Service for Fixture {
    fn descriptor(&self) -> ServiceDescriptor {
        ServiceDescriptor {
            world: "com.lait.fixture".to_string(),
            implementation: [0x42; 32],
            implementation_version: 1,
        }
    }

    fn call(
        &self,
        operation: &str,
        payload: &[u8],
        host: std::sync::Arc<dyn world_runner::Host>,
    ) -> Result<Vec<u8>, String> {
        match operation {
            "echo" => Ok(payload.to_vec()),
            "roundtrip" => host.call("uppercase", payload),
            _ => Err(format!("unsupported fixture operation {operation}")),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let version = std::env::var("LAIT_WORLD_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    world_runner::serve("com.lait.fixture", version, Fixture)
}
