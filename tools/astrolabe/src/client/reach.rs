//! Correspondence, as this client reaches it: over the daemon, like everything
//! else the identity owns.
//!
//! The plane is substrate and lives in [`correspondence::plane`]; the mailbox it
//! holds is the daemon's. This client used to be both, which is why a World
//! could not send at all and why mail arrived only while a window was open. It
//! is a caller now and holds nothing.
//!
//! Every request answers with the whole view, so there is one model of what has
//! been said and this side never keeps a second — the same rule the rest of this
//! client follows, and the case a second copy would have broken is exactly the
//! one that matters: a letter landing while nobody was looking.

use lait::control::{
    ControlRoute, MarkerView, OriginView, OwnDeviceView, ReachView, Request, Response,
};

use crate::client::{Client, ClientError, ClientResult};
use crate::model::{ChatMessage, Contact, Conversation, Correspondence};

pub use correspondence::plane::{Collected, Failure, Opened, ReachPlane};

/// The profile this device belongs to, and every device that holds it.
///
/// Read from the same [`ReachView`] the mailbox comes from — one request, two
/// readings — because they are one identity's facts and a second request
/// would be a second moment. Absence keeps its kind here: an empty device set
/// with `device_set_unknown` is a daemon that has not restored the set, which
/// is not a profile with no devices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSnapshot {
    /// This identity's own address on the plane.
    pub profile: String,
    /// This machine's device in the profile — the join key every row uses.
    pub me: Option<String>,
    /// Founded here, or adopted from a device that already held it.
    pub origin: Option<OriginView>,
    /// The device set, `me` included.
    pub devices: Vec<OwnDeviceView>,
    /// The set was not held. Never folded into `devices` being empty.
    pub device_set_unknown: bool,
    /// The markers this identity weighs, in its own order. Carried beside
    /// the devices because a device's certification is only readable against
    /// the list — "no marker says so" and "the marker could not be asked"
    /// are different answers and the second one lives here.
    pub markers: Vec<MarkerView>,
}

impl Client {
    /// The profile's devices, as the daemon holds them.
    pub async fn profile(&self) -> ClientResult<ProfileSnapshot> {
        let view = self.reach_raw(Request::ReachView).await?;
        Ok(ProfileSnapshot {
            profile: view.profile,
            me: view.me,
            origin: view.origin,
            devices: view.devices,
            device_set_unknown: view.device_set_unknown,
            markers: view.markers,
        })
    }

    /// What this identity's correspondence looks like now.
    pub async fn reach_view(&self) -> ClientResult<Correspondence> {
        self.reach_request(Request::ReachView).await
    }

    /// Publish this identity's reach, so it can be handed to somebody.
    pub async fn reach_share(&self) -> ClientResult<Correspondence> {
        self.reach_request(Request::ReachShare).await
    }

    /// Take a correspondent in, by the announcement they handed over.
    pub async fn reach_learn(&self, announcement: String) -> ClientResult<Correspondence> {
        self.reach_request(Request::ReachLearn { announcement })
            .await
    }

    /// Seal a message to a learned correspondent and deposit it.
    pub async fn correspond_send(&self, to: String, body: String) -> ClientResult<Correspondence> {
        self.reach_request(Request::CorrespondSend { to, body })
            .await
    }

    /// Ask the carrier for anything waiting.
    pub async fn correspond_collect(&self) -> ClientResult<Correspondence> {
        self.reach_request(Request::CorrespondCollect).await
    }

    /// Carry an invitation this identity already holds to a correspondent.
    pub async fn correspond_invite(
        &self,
        to: String,
        link: String,
    ) -> ClientResult<Correspondence> {
        self.reach_request(Request::CorrespondInvite { to, link })
            .await
    }

    async fn reach_request(&self, request: Request) -> ClientResult<Correspondence> {
        Ok(from_view(self.reach_raw(request).await?))
    }

    async fn reach_raw(&self, request: Request) -> ClientResult<ReachView> {
        let daemon = self.daemon()?;
        let reply = daemon
            .request(ControlRoute::Daemon, &request, None)
            .await
            .map_err(|error| ClientError::unreachable(format!("{error:#}")))?;
        match reply {
            Response::Reach(view) => Ok(*view),
            Response::Error { message, .. } => Err(ClientError::refused(message)),
            other => Err(ClientError::internal(format!(
                "unexpected correspondence reply: {other:?}"
            ))),
        }
    }
}

/// The daemon's view, as the model draws it.
///
/// A correspondent's name is their address, until a Card can name one — a
/// truthful placeholder rather than an invented one.
fn from_view(view: ReachView) -> Correspondence {
    let me = view.profile.clone();
    let contact = |id: String, name: String| Contact {
        id,
        name,
        devices: Vec::new(),
        added: true,
        is_agent: false,
        parent_id: None,
        parent_name: None,
        unread: 0,
    };

    let mut contacts = vec![contact(me.clone(), "You".to_owned())];
    contacts.extend(
        view.correspondents
            .iter()
            .map(|address| contact(address.clone(), address.clone())),
    );

    let open_tabs: Vec<String> = view
        .conversations
        .iter()
        .map(|conversation| conversation.peer.clone())
        .collect();

    Correspondence {
        my_device: None,
        my_reach: view.announcement,
        me: Some(me),
        contacts,
        conversations: view
            .conversations
            .into_iter()
            .map(|conversation| Conversation {
                peer_name: conversation.peer.clone(),
                peer_id: conversation.peer,
                messages: conversation
                    .letters
                    .into_iter()
                    .map(|letter| ChatMessage {
                        id: letter.id,
                        invitation: letter.invitation,
                        mine: letter.mine,
                        kind: letter.kind,
                        body: letter.body,
                        sent_at: letter.sent_at,
                        from_device: letter.from_device,
                        provenance_agrees: letter.provenance_agrees,
                    })
                    .collect(),
            })
            .collect(),
        active_tab: open_tabs.first().cloned(),
        open_tabs,
    }
}
