//! The Space plane.
//!
//! A Space sits *below* a World and may carry several, so its administration
//! belongs to the client rather than to any one World's settings page. A World
//! drawing the membership of the Space it happens to sit in is the layering this
//! module reverses.
//!
//! ## Listing stays passive; administering one Space does not
//!
//! Every Orbit this device serves is read from the host plane, which places
//! nothing — that is the invariant, and it is what keeps a front page from
//! costing what opening costs. Administering *one* Space is a different act: a
//! person has chosen it, and reading its membership means asking it. So the
//! calls here route with `request` rather than `request_if_running`, and the
//! surface above only makes them for the Space somebody selected.

use lait::control::{ControlRoute, OrbitAddress, Request, Response};
use lait::diagnose::DiagnosisView;
use lait::dto::{MemberDto, WhoamiDto};

use super::{Client, ClientError, ClientResult};

/// Which Space, and where its store is.
///
/// Both, because a Space id alone does not address an Orbit: the route is built
/// from the store path the registry recorded, and two identities on one machine
/// can hold the same Space at different paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRef {
    pub space: String,
    pub path: String,
}

/// One device bound to this actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceKey {
    /// The device id, when the line parsed as one.
    ///
    /// `None` when it did not. The plane answers this as human text, and a
    /// client that assumed every line was an id would offer to revoke a
    /// sentence. Revocation is offered only for a line that parsed.
    pub id: Option<String>,
    /// What the daemon actually wrote, kept verbatim so nothing is lost in the
    /// parse.
    pub line: String,
    /// Whether the daemon marked this as the device answering.
    pub is_this_device: bool,
}

/// Everything one Space says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceView {
    pub space: String,
    /// This actor's standing here. Routed per Orbit rather than per identity:
    /// one identity may hold very different standing in two Spaces, and a
    /// single answer for "who am I" would have to pick one and be wrong about
    /// the other.
    pub standing: WhoamiDto,
    pub members: Vec<MemberDto>,
    pub devices: Vec<DeviceKey>,
    /// The onboarding gates, and the one thing blocking. `None` when the Space
    /// could not be diagnosed — which is not the same as every gate passing.
    pub diagnosis: Option<DiagnosisView>,
}

/// What a person can ask a Space to do.
///
/// One enum rather than eleven actions, because every one of them is the same
/// shape — a Space, a request, and a confirmation — and spreading them across
/// the action list would put eleven near-identical arms in front of the two
/// that are genuinely different.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceOp {
    MemberAdd {
        who: String,
        admin: bool,
    },
    MemberRemove {
        who: String,
    },
    MemberSetRole {
        who: String,
        admin: bool,
    },
    /// Mint an admission-bearing link.
    Invite {
        role: String,
        reusable: bool,
        ttl_hours: u64,
    },
    DeviceRevoke {
        device: String,
    },
    CustodyExport {
        path: String,
        passphrase: String,
    },
    CustodyImport {
        path: String,
        passphrase: String,
        force: bool,
    },
    /// Sponsor a co-located agent identity by name.
    AgentProvision {
        name: String,
    },
}

impl SpaceOp {
    /// What this is, in words, for the record of what happened.
    pub fn what(&self) -> String {
        match self {
            Self::MemberAdd { who, admin: true } => format!("add {who} as an administrator"),
            Self::MemberAdd { who, .. } => format!("add {who}"),
            Self::MemberRemove { who } => format!("remove {who}"),
            Self::MemberSetRole { who, admin: true } => format!("promote {who}"),
            Self::MemberSetRole { who, .. } => format!("demote {who}"),
            Self::Invite { role, .. } => format!("mint a {role} invite"),
            Self::DeviceRevoke { device } => format!("revoke device {device}"),
            Self::CustodyExport { .. } => "export this device's recovery share".into(),
            Self::CustodyImport { .. } => "restore a recovery share".into(),
            Self::AgentProvision { name } => format!("sponsor agent '{name}'"),
        }
    }

    /// A stable key, so two different asks on one Space are two things in
    /// flight rather than one.
    pub fn key(&self) -> String {
        match self {
            Self::MemberAdd { who, .. } => format!("member.add:{who}"),
            Self::MemberRemove { who } => format!("member.remove:{who}"),
            Self::MemberSetRole { who, .. } => format!("member.role:{who}"),
            Self::Invite { .. } => "invite".into(),
            Self::DeviceRevoke { device } => format!("device.revoke:{device}"),
            Self::CustodyExport { .. } => "custody.export".into(),
            Self::CustodyImport { .. } => "custody.import".into(),
            Self::AgentProvision { name } => format!("agent.provision:{name}"),
        }
    }

    fn into_request(self) -> Request {
        match self {
            Self::MemberAdd { who, admin } => Request::MemberAdd {
                who,
                admin,
                as_name: None,
            },
            Self::MemberRemove { who } => Request::MemberRemove { who },
            Self::MemberSetRole { who, admin } => Request::MemberSetRole { who, admin },
            Self::Invite {
                role,
                reusable,
                ttl_hours,
            } => Request::Invite {
                world: None,
                role: Some(role),
                reusable,
                ttl_hours: Some(ttl_hours),
            },
            Self::DeviceRevoke { device } => Request::DeviceRevoke { device },
            Self::CustodyExport { path, passphrase } => {
                Request::SpaceCustodyExport { path, passphrase }
            }
            Self::CustodyImport {
                path,
                passphrase,
                force,
            } => Request::SpaceCustodyImport {
                path,
                passphrase,
                force,
            },
            Self::AgentProvision { name } => Request::AgentProvision { name },
        }
    }
}

impl Client {
    /// Everything one Space says about itself, in one read.
    ///
    /// One call rather than four, because a surface drawn from four independent
    /// answers can show a roster from one moment beside a standing from another
    /// — and the moment that matters is the one where somebody's role just
    /// changed.
    pub async fn space_view(&self, at: &SpaceRef) -> ClientResult<SpaceView> {
        let standing = match self.space_request(at, Request::Whoami).await? {
            Response::Whoami(standing) => standing,
            other => {
                return Err(ClientError::internal(format!(
                    "unexpected standing reply: {other:?}"
                )))
            }
        };
        let members = match self.space_request(at, Request::Members).await? {
            Response::Members { members } => members,
            other => {
                return Err(ClientError::internal(format!(
                    "unexpected member roster reply: {other:?}"
                )))
            }
        };
        let devices = match self.space_request(at, Request::DeviceList).await? {
            Response::Text { text } => parse_devices(&text),
            other => {
                return Err(ClientError::internal(format!(
                    "unexpected device list reply: {other:?}"
                )))
            }
        };
        // A diagnosis that could not be taken is absent, not "every gate
        // passes". Those are the two answers this whole codebase spends its
        // effort keeping apart.
        let diagnosis = match self
            .space_request(
                at,
                Request::Diagnose {
                    expected_space: None,
                },
            )
            .await
        {
            Ok(Response::Diagnosis(view)) => Some(*view),
            _ => None,
        };

        Ok(SpaceView {
            space: at.space.clone(),
            standing,
            members,
            devices,
            diagnosis,
        })
    }

    /// Ask a Space to do something, and hand back whatever it said.
    ///
    /// The reply is carried as text rather than swallowed because three of
    /// these — an invite link, a device-enrolment token, a consent blob — *are*
    /// their reply. A verb that returned only "ok" would have produced the thing
    /// a person came for and thrown it away.
    pub async fn space_do(&self, at: &SpaceRef, operation: SpaceOp) -> ClientResult<String> {
        let described = operation.what();
        match self.space_request(at, operation.into_request()).await? {
            Response::Ok { message } => Ok(message.unwrap_or(described)),
            Response::Text { text } | Response::Ref { reff: text } => Ok(text),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected reply to {described}: {other:?}"
            ))),
        }
    }

    async fn space_request(&self, at: &SpaceRef, request: Request) -> ClientResult<Response> {
        let Some(space) = mechanics::ids::SpaceId::parse(&at.space) else {
            return Err(ClientError::invalid(format!(
                "'{}' is not a Space id",
                at.space
            )));
        };
        let route = ControlRoute::Orbit {
            address: OrbitAddress::for_store(std::path::Path::new(&at.path), space),
        };
        let reply = self
            .daemon()?
            .request(route, &request, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("reach the Space: {error:#}")))?;
        match reply {
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Ok(other),
        }
    }
}

/// The device list, which the Space plane answers as human text.
///
/// Parsed defensively and never trusted: a line that does not resolve to a
/// device id keeps its text and offers no revocation. The alternative — assuming
/// every line is an id — would put "no devices" in a list beside a Revoke
/// button.
fn parse_devices(text: &str) -> Vec<DeviceKey> {
    const HERE: &str = " (this device)";
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let is_this_device = line.ends_with(HERE);
            let candidate = line.strip_suffix(HERE).unwrap_or(line).trim();
            DeviceKey {
                id: mechanics::ids::DeviceId::parse(candidate).map(|id| id.as_str().to_owned()),
                line: line.to_owned(),
                is_this_device,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plane answers this as prose, and the parse must not turn prose into
    /// something a Revoke button can be pointed at.
    #[test]
    fn a_device_line_that_is_not_a_device_offers_nothing_to_revoke() {
        let parsed = parse_devices("no devices");
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed
                .first()
                .and_then(|device| device.id.clone())
                .is_none(),
            "a sentence was accepted as a device id"
        );
        assert_eq!(
            parsed.first().map(|device| device.line.clone()).as_deref(),
            Some("no devices")
        );
    }

    /// The marker the daemon adds is read, and it is read off the end rather
    /// than searched for anywhere in the line.
    #[test]
    fn the_device_answering_is_marked_and_still_parses_as_a_device() {
        let id = "0".repeat(64);
        let parsed = parse_devices(&format!("{id} (this device)\n{id}"));
        assert_eq!(parsed.len(), 2);
        let first = parsed.first().expect("a first device");
        assert!(first.is_this_device);
        assert_eq!(first.id.as_deref(), Some(id.as_str()));
        let second = parsed.get(1).expect("a second device");
        assert!(!second.is_this_device);
    }

    /// Each ask is its own thing in flight. A key shared between two members'
    /// buttons would disable both rows because one of them was busy.
    #[test]
    fn two_asks_about_two_members_are_two_things_in_flight() {
        assert_ne!(
            SpaceOp::MemberRemove { who: "a".into() }.key(),
            SpaceOp::MemberRemove { who: "b".into() }.key()
        );
        assert_ne!(
            SpaceOp::MemberRemove { who: "a".into() }.key(),
            SpaceOp::MemberSetRole {
                who: "a".into(),
                admin: true
            }
            .key()
        );
    }

    /// Promotion and demotion are different acts, and the record of what
    /// happened has to say which.
    #[test]
    fn a_role_change_says_which_direction_it_went() {
        let up = SpaceOp::MemberSetRole {
            who: "a".into(),
            admin: true,
        };
        let down = SpaceOp::MemberSetRole {
            who: "a".into(),
            admin: false,
        };
        assert!(up.what().contains("promote"), "{}", up.what());
        assert!(down.what().contains("demote"), "{}", down.what());
    }

    #[test]
    fn sponsoring_two_agents_are_two_things_in_flight() {
        assert_ne!(
            SpaceOp::AgentProvision {
                name: "grok".into()
            }
            .key(),
            SpaceOp::AgentProvision {
                name: "claude".into()
            }
            .key()
        );
        assert!(
            SpaceOp::AgentProvision {
                name: "grok".into()
            }
            .what()
            .contains("grok"),
            "{}",
            SpaceOp::AgentProvision {
                name: "grok".into()
            }
            .what()
        );
    }
}
