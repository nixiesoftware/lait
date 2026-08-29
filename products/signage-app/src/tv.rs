//! The television that shows a screen, as Signage speaks of it.
//!
//! Signage has one word for hardware, and it is "screen". A screen is a real
//! screen; the only question about it is whether a television is showing it
//! yet. So the acts here are a screen's: get a code for its TV, withdraw it,
//! say which screen a TV that connected by words is, disconnect the TV. Each
//! lands on the host's display family, which scopes it to this World: the
//! surface is always this product's, the input is always a screen, and the
//! host names the World. Nothing here can reach a receiver another World
//! holds, and nothing here can take over the machine's own screen — that
//! stays with the linked-devices manager.
//!
//! A TV has no name of its own here. The label the host keeps for it is the
//! screen's name, so the linked-devices manager lists it the way Signage
//! does.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use world_interface::display::DisplaySurfaceId;
use world_interface::{ClientAccess, ClientHost, Failure, HostControlRequest};

/// The local operation these ride on, for the host's `execute`.
pub const LOCAL_TV: &str = "signage.tv";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum TvRequest {
    /// This World's TVs, the ones nobody holds, and the codes and word-pairings
    /// waiting.
    TvList,
    /// A code for the TV that will show `screen` the moment it connects.
    /// `label` is the screen's name — the TV is known by it everywhere else.
    TvCodeMint {
        screen: String,
        label: String,
    },
    TvCodeRevoke {
        rendezvous: String,
    },
    /// A TV that shows no screen, or a screen that is gone, is this one.
    TvAssign {
        device: String,
        screen: String,
    },
    /// Disconnect a TV: it asks for a code again.
    TvForget {
        device: String,
    },
    /// Trust a TV asking to connect by words: it is `screen`.
    TvPairingApprove {
        pairing: String,
        label: String,
        screen: String,
    },
    TvPairingReject {
        pairing: String,
    },
}

impl TvRequest {
    pub fn is_tv_command(command: &str) -> bool {
        command.starts_with("tv_")
    }

    pub fn access(&self) -> ClientAccess {
        match self {
            Self::TvList => ClientAccess::Query,
            _ => ClientAccess::Command,
        }
    }
}

/// The surface every Signage TV shows, and the input a screen is.
fn surface() -> Result<DisplaySurfaceId, Failure> {
    DisplaySurfaceId::new(crate::display::SURFACE_ID)
        .map_err(|error| Failure::new(format!("{error}")))
}

fn screen_input(screen: &str) -> Result<Value, Failure> {
    if replica::body::BodyId::parse(screen).is_none() {
        return Err(Failure::new(
            "a TV shows a screen, named by the screen's id",
        ));
    }
    Ok(serde_json::json!({ "screen": screen }))
}

pub async fn run(host: &dyn ClientHost, request: TvRequest) -> Result<Value, Failure> {
    match request {
        TvRequest::TvList => {
            host.call_control(HostControlRequest::DisplayReceivers)
                .await
        }
        TvRequest::TvCodeMint { screen, label } => {
            if label.trim().is_empty() {
                return Err(Failure::new("a code is labelled by the screen's name"));
            }
            host.call_control(HostControlRequest::DisplayCodeMint {
                label,
                surface: surface()?,
                input: screen_input(&screen)?,
                sync: None,
            })
            .await
        }
        TvRequest::TvCodeRevoke { rendezvous } => {
            host.call_control(HostControlRequest::DisplayCodeRevoke { rendezvous })
                .await
        }
        TvRequest::TvAssign { device, screen } => assign(host, device, &screen).await,
        TvRequest::TvForget { device } => {
            host.call_control(HostControlRequest::DisplayForget { device })
                .await
        }
        TvRequest::TvPairingApprove {
            pairing,
            label,
            screen,
        } => {
            // The screen is checked before the trust act, so a bad screen id
            // refuses cleanly rather than enrolling a TV that then shows nothing.
            screen_input(&screen)?;
            let approved = host
                .call_control(HostControlRequest::DisplayPairingApprove { pairing, label })
                .await?;
            let device = approved
                .get("device")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("the host approved the TV but did not name it"))?
                .to_string();
            assign(host, device.clone(), &screen).await?;
            Ok(serde_json::json!({ "kind": "display_device", "device": device }))
        }
        TvRequest::TvPairingReject { pairing } => {
            host.call_control(HostControlRequest::DisplayPairingReject { pairing })
                .await
        }
    }
}

async fn assign(host: &dyn ClientHost, device: String, screen: &str) -> Result<Value, Failure> {
    host.call_control(HostControlRequest::DisplayAssign {
        device,
        surface: surface()?,
        input: screen_input(screen)?,
        sync: None,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tv_request_is_read_by_its_command_and_classified_by_what_it_does() {
        let list: TvRequest =
            serde_json::from_value(serde_json::json!({ "cmd": "tv_list" })).unwrap();
        assert_eq!(list, TvRequest::TvList);
        assert_eq!(list.access(), ClientAccess::Query);
        let mint: TvRequest = serde_json::from_value(serde_json::json!({
            "cmd": "tv_code_mint", "screen": "z6d2vyxarb6rqqnhtm2iy6i7mq", "label": "Lobby"
        }))
        .unwrap();
        assert_eq!(mint.access(), ClientAccess::Command);
        assert!(TvRequest::is_tv_command("tv_assign"));
        assert!(!TvRequest::is_tv_command("screen_list"));
        // A TV is never left showing nothing on purpose: there is no such act.
        assert!(serde_json::from_value::<TvRequest>(
            serde_json::json!({ "cmd": "tv_unassign", "device": "dev" })
        )
        .is_err());
    }

    #[test]
    fn a_screen_is_the_input_and_the_surface_is_this_products() {
        let input = screen_input("z6d2vyxarb6rqqnhtm2iy6i7mq").unwrap();
        assert_eq!(
            input,
            serde_json::json!({ "screen": "z6d2vyxarb6rqqnhtm2iy6i7mq" })
        );
        assert!(screen_input("not a screen").is_err());
        assert_eq!(surface().unwrap().as_str(), "signage.program");
    }
}
