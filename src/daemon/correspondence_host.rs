//! One daemon's identity-scoped correspondence services.
//!
//! A correspondence plane owns a key, mailbox, transcript, and durable home.
//! Selecting another local identity therefore cannot be a field on the primary
//! plane: it is another [`CorrespondenceService`] rooted at another canonical
//! identity directory. This host is only the index over those independent
//! services.
//!
//! The primary identity is always present. A human-scoped daemon may explicitly
//! or lazily load provisioned agent identities from the direct children of its
//! configured `agents` directory. A self-contained agent daemon passes
//! `host_agents = false`; it can never walk sideways into a sibling. The
//! existing `act_as` selector is deliberately absent: it selects a World signer,
//! not a mailbox or owner context. Agent correspondence is reached only through
//! the explicit internal name/profile seams below.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use mechanics::kinship::ProfileId;

use super::correspondence::{CorrespondenceService, MutualIntroduction};

/// Public, non-secret identity metadata for one loaded local agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedAgent {
    pub name: String,
    pub profile: ProfileId,
}

/// One background collection result, kept attached to the profile whose
/// mailbox was asked.
#[derive(Debug)]
pub struct ProfileCollection {
    pub profile: ProfileId,
    /// `None` is the daemon's primary identity; `Some` is a local agent name.
    pub agent: Option<String>,
    pub result: Result<usize, String>,
}

/// One loaded identity's configured Post wake subscription.
///
/// This is public transport metadata, never a capability: the device id is
/// already the mailbox address and the Post still authenticates collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeRegistration {
    pub profile: ProfileId,
    pub post_url: String,
    pub device: String,
}

struct AgentService {
    profile: ProfileId,
    service: Arc<CorrespondenceService>,
}

#[derive(Default)]
struct AgentIndex {
    by_name: BTreeMap<String, AgentService>,
    by_profile: BTreeMap<ProfileId, String>,
}

/// The primary correspondence service and every explicitly loaded agent
/// service this daemon is allowed to host.
pub struct CorrespondenceHost {
    primary: Arc<CorrespondenceService>,
    agents_base: PathBuf,
    host_agents: bool,
    agents: Mutex<AgentIndex>,
    wake: OnceLock<Arc<tokio::sync::Notify>>,
    wake_started: Mutex<BTreeSet<ProfileId>>,
}

impl CorrespondenceHost {
    #[must_use]
    pub fn open(primary: &Path, agents_base: &Path, host_agents: bool) -> Self {
        Self {
            primary: Arc::new(CorrespondenceService::open(primary)),
            agents_base: agents_base.to_path_buf(),
            host_agents,
            agents: Mutex::new(AgentIndex::default()),
            wake: OnceLock::new(),
            wake_started: Mutex::new(BTreeSet::new()),
        }
    }

    /// The daemon's own correspondence identity. This is never selected by a
    /// failed agent lookup.
    #[must_use]
    pub fn primary(&self) -> Arc<CorrespondenceService> {
        Arc::clone(&self.primary)
    }

    /// A borrowed primary service for daemon infrastructure whose authority is
    /// intrinsically the host identity (fan-out, pairing, and own-device
    /// admission), never a request-selected identity.
    #[must_use]
    pub fn primary_ref(&self) -> &CorrespondenceService {
        &self.primary
    }

    /// Load or return one provisioned agent's independent correspondence
    /// service.
    pub fn agent(&self, name: &str, now: u64) -> Result<Arc<CorrespondenceService>, String> {
        let home = self.resolve_agent_home(name)?;
        let mut agents = self
            .agents
            .lock()
            .map_err(|_| "the agent correspondence index is poisoned".to_string())?;
        if let Some(loaded) = agents.by_name.get(name) {
            return Ok(Arc::clone(&loaded.service));
        }

        // Resolve the actual self-certifying profile from this home. The helper
        // carries forward an existing genesis or founds and durably stores the
        // first one; no profile is derived from `name` or faked to fill an index.
        let expected = crate::config::identity_profile(&home).map_err(|error| {
            format!("resolve correspondence profile for agent '{name}': {error:#}")
        })?;
        let service = Arc::new(CorrespondenceService::open(&home));
        service
            .restore(now)
            .map_err(|error| format!("restore correspondence for agent '{name}': {error}"))?;
        let profile = service
            .profile()
            .ok_or_else(|| format!("agent '{name}' restored without a correspondence profile"))?;
        if profile != expected {
            return Err(format!(
                "agent '{name}' restored as a different correspondence profile"
            ));
        }
        if let Some(other) = agents.by_profile.get(&profile) {
            return Err(format!(
                "agent '{name}' resolves to the correspondence profile already loaded as '{other}'"
            ));
        }

        // Names and faces are identity state too. If the agent's own book is
        // present, hook that book—not the owner's and not a sibling's.
        if let Ok(book) = crate::daemon::address_book::AddressBookService::open(&home) {
            service.hook_book(Arc::new(book));
        }
        if let Some(contractor) = super::correspondence::configured_carrier(&home) {
            service
                .carry_over(contractor, now)
                .map_err(|error| format!("carry correspondence for agent '{name}': {error}"))?;
        }

        agents.by_profile.insert(profile.clone(), name.to_string());
        agents.by_name.insert(
            name.to_string(),
            AgentService {
                profile,
                service: Arc::clone(&service),
            },
        );
        drop(agents);
        self.start_pending_wakes();
        Ok(service)
    }

    /// A loaded agent by its resolved profile. This never searches disk or
    /// guesses from a name.
    #[must_use]
    pub fn profile(&self, profile: &ProfileId) -> Option<Arc<CorrespondenceService>> {
        let agents = self.agents.lock().ok()?;
        let name = agents.by_profile.get(profile)?;
        agents
            .by_name
            .get(name)
            .map(|loaded| Arc::clone(&loaded.service))
    }

    /// Safe provisioned-name enumeration. Only valid direct child directories
    /// with an identity key are returned; symlinks and paths resolving outside
    /// the configured agent base are omitted.
    #[must_use]
    pub fn available_agents(&self) -> Vec<String> {
        if !self.host_agents {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&self.agents_base) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| self.resolve_agent_home(name).is_ok())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Public metadata for loaded agents, sorted by their local names.
    #[must_use]
    pub fn loaded_agents(&self) -> Vec<LoadedAgent> {
        self.agents.lock().map_or_else(
            |_| Vec::new(),
            |agents| {
                agents
                    .by_name
                    .iter()
                    .map(|(name, loaded)| LoadedAgent {
                        name: name.clone(),
                        profile: loaded.profile.clone(),
                    })
                    .collect()
            },
        )
    }

    /// Eagerly restore every safely enumerable provisioned agent.
    ///
    /// Each result is independent so one damaged agent home can be reported
    /// without preventing the primary identity or any sibling from standing.
    pub fn load_available(&self, now: u64) -> Vec<(String, Result<ProfileId, String>)> {
        self.available_agents()
            .into_iter()
            .map(|name| {
                let loaded = self.agent(&name, now).and_then(|service| {
                    service.profile().ok_or_else(|| {
                        format!("agent '{name}' restored without a correspondence profile")
                    })
                });
                (name, loaded)
            })
            .collect()
    }

    /// Make the primary owner and one provisioned agent immediately reachable
    /// to each other using normal signed Reach announcements.
    ///
    /// Loading an agent does not itself grant or imply owner authority. This
    /// explicit supervisor operation establishes correspondence only, and the
    /// two independent Reach stores are durable before it reports success.
    pub fn introduce_agent(&self, name: &str, now: u64) -> Result<MutualIntroduction, String> {
        let agent = self.agent(name, now)?;
        self.primary.mutually_introduce(&agent)
    }

    /// Wake subscriptions for the primary identity and every loaded agent that
    /// is actually carried by a configured Post.
    ///
    /// This never scans or loads: eager registry restoration defines the
    /// finite set before listener launch. A damaged or unconfigured identity is
    /// simply absent and cannot prevent a healthy sibling's registration.
    #[must_use]
    pub fn wake_registrations(&self) -> Vec<WakeRegistration> {
        let mut services = vec![self.primary()];
        if let Ok(agents) = self.agents.lock() {
            services.extend(
                agents
                    .by_name
                    .values()
                    .map(|loaded| Arc::clone(&loaded.service)),
            );
        }
        services
            .into_iter()
            .filter(|service| service.carrying())
            .filter_map(|service| {
                Some(WakeRegistration {
                    profile: service.profile()?,
                    post_url: super::correspondence::configured_post_url(service.home())?,
                    device: service.my_wire_device()?,
                })
            })
            .collect()
    }

    /// Activate Post doorbells for every currently and subsequently loaded
    /// identity. Calling this again is harmless: at most one listener is
    /// started for each resolved profile in this host generation.
    pub fn activate_wakes(&self, woken: Arc<tokio::sync::Notify>) {
        let _ = self.wake.set(woken);
        self.start_pending_wakes();
    }

    fn start_pending_wakes(&self) {
        let Some(woken) = self.wake.get() else {
            return;
        };
        let Ok(mut started) = self.wake_started.lock() else {
            return;
        };
        for wake in self.wake_registrations() {
            if started.insert(wake.profile) {
                super::correspondence::serve_wake(wake.post_url, wake.device, Arc::clone(woken));
            }
        }
    }

    /// Collect the primary mailbox and every loaded agent mailbox independently.
    ///
    /// Every result retains its profile, and every service owns its own lock and
    /// durable store. One unavailable carrier therefore does not prevent the
    /// other identities from being asked during this pass.
    pub fn collect_loaded(&self, now: u64) -> Result<Vec<ProfileCollection>, String> {
        // There is no honest key for an unrestored primary. Omit it rather than
        // inventing one, but continue to collect every restored agent.
        let mut services = self
            .primary
            .profile()
            .map(|profile| vec![(profile, None, self.primary())])
            .unwrap_or_default();
        {
            let agents = self
                .agents
                .lock()
                .map_err(|_| "the agent correspondence index is poisoned".to_string())?;
            services.extend(agents.by_name.iter().map(|(name, loaded)| {
                (
                    loaded.profile.clone(),
                    Some(name.clone()),
                    Arc::clone(&loaded.service),
                )
            }));
        }
        Ok(services
            .into_iter()
            .map(|(profile, agent, service)| ProfileCollection {
                profile,
                agent,
                result: service.collect_standing(now),
            })
            .collect())
    }

    fn resolve_agent_home(&self, name: &str) -> Result<PathBuf, String> {
        if !self.host_agents {
            return Err("this self-contained identity cannot host sibling agents".into());
        }
        crate::agent_token::plain_agent_name(name).map_err(|error| error.to_string())?;
        let base = crate::config::resolved(&self.agents_base)
            .ok_or_else(|| "the configured agent directory does not exist".to_string())?;
        let spelled = self.agents_base.join(name);
        let home = crate::config::resolved(&spelled)
            .ok_or_else(|| format!("no provisioned local agent named '{name}'"))?;
        if home.parent() != Some(base.as_path()) || !home.join("secret.key").is_file() {
            return Err(format!("no provisioned local agent named '{name}'"));
        }
        Ok(home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Request, Response};

    fn found(home: &Path) {
        std::fs::create_dir_all(home).expect("identity directory");
        crate::config::load_or_create_identity(home).expect("identity key");
        crate::config::identity_profile(home).expect("identity profile");
    }

    fn stand(home: &Path) -> Arc<CorrespondenceService> {
        found(home);
        let service = Arc::new(CorrespondenceService::open(home));
        service
            .restore(super::super::correspondence::now_secs())
            .expect("restore correspondence");
        service
    }

    fn reach(response: &Response) -> &crate::control::ReachView {
        match response {
            Response::Reach(view) => view,
            other => panic!("expected a reach view, got {other:?}"),
        }
    }

    async fn leave_for(
        sender: &CorrespondenceService,
        recipient: &CorrespondenceService,
        body: &str,
    ) {
        let recipient = reach(&recipient.handle(Request::ReachShare).await).clone();
        let announcement = recipient.announcement.expect("recipient card");
        let learned = sender.handle(Request::ReachLearn { announcement }).await;
        assert!(matches!(learned, Response::Reach(_)));
        let sent = sender
            .handle(Request::CorrespondSend {
                to: recipient.profile,
                body: body.into(),
            })
            .await;
        assert!(matches!(sent, Response::Reach(_)));
    }

    fn incoming_bodies(view: &crate::control::ReachView) -> Vec<String> {
        view.conversations
            .iter()
            .flat_map(|conversation| conversation.letters.iter())
            .filter(|letter| !letter.mine)
            .filter_map(|letter| letter.body.clone())
            .collect()
    }

    #[tokio::test]
    async fn primary_and_agent_collect_into_separate_durable_mailboxes_without_a_world() {
        let root = tempfile::tempdir().expect("config root");
        let primary_home = root.path().join("primary");
        let agents_base = crate::registry::agents_base(root.path());
        let agent_home = agents_base.join("adam");
        let outsider_home = root.path().join("outsider");
        found(&primary_home);
        found(&agent_home);
        let outsider = stand(&outsider_home);

        let host = CorrespondenceHost::open(&primary_home, &agents_base, true);
        let primary = host.primary();
        primary.restore(1).expect("primary restore");
        let eagerly_loaded = host.load_available(1);
        assert_eq!(eagerly_loaded.len(), 1);
        assert_eq!(eagerly_loaded[0].0, "adam");
        let adam_profile = eagerly_loaded[0]
            .1
            .as_ref()
            .expect("eagerly load Adam")
            .clone();
        let adam = host.profile(&adam_profile).expect("Adam by profile");
        assert_ne!(primary.profile(), Some(adam_profile.clone()));
        assert!(Arc::ptr_eq(
            &adam,
            &host
                .profile(&adam_profile)
                .expect("lookup Adam by resolved profile")
        ));
        assert_eq!(host.available_agents(), vec!["adam"]);
        assert_eq!(
            host.loaded_agents(),
            vec![LoadedAgent {
                name: "adam".into(),
                profile: adam_profile.clone(),
            }]
        );

        // The supervisor establishes direct correspondence with ordinary
        // signed Reach artifacts. It needs neither a directory nor a World,
        // and both independent stores know the other before success returns.
        let introduced = host
            .introduce_agent("adam", 1)
            .expect("mutually introduce Adam and owner");
        let primary_profile = primary.profile().expect("primary profile");
        assert_eq!(introduced.primary, primary_profile);
        assert_eq!(introduced.agent, adam_profile);
        assert_eq!(
            addressbook::Announcement::parse(&introduced.primary_announcement)
                .expect("primary announcement")
                .profile,
            primary_profile
        );
        assert_eq!(
            addressbook::Announcement::parse(&introduced.agent_announcement)
                .expect("agent announcement")
                .profile,
            adam_profile
        );
        let primary_after_introduction = reach(&primary.handle(Request::ReachView).await).clone();
        let adam_after_introduction = reach(&adam.handle(Request::ReachView).await).clone();
        assert!(primary_after_introduction
            .correspondents
            .contains(&adam_profile.as_str().to_owned()));
        assert!(adam_after_introduction
            .correspondents
            .contains(&primary_profile.as_str().to_owned()));

        let carrier = correspondence::SharedMem::new();
        outsider
            .carry_over_with(Box::new(carrier.clone()), None, 1)
            .expect("outsider carrier");
        primary
            .carry_over_with(Box::new(carrier.clone()), None, 1)
            .expect("primary carrier");
        adam.carry_over_with(Box::new(carrier.clone()), None, 1)
            .expect("Adam carrier");
        for home in [&primary_home, &agent_home] {
            let mut settings = crate::config::ConfigMap::default();
            settings.set("post.url", "https://post.example.test");
            settings
                .save(&crate::config::store_config_path(home))
                .expect("store Post setting");
        }
        let wakes = host.wake_registrations();
        assert_eq!(wakes.len(), 2);
        assert!(wakes
            .iter()
            .all(|wake| wake.post_url == "https://post.example.test"));
        assert_ne!(wakes[0].profile, wakes[1].profile);
        assert_ne!(wakes[0].device, wakes[1].device);

        leave_for(&outsider, &primary, "only primary").await;
        leave_for(&outsider, &adam, "only Adam").await;
        let collected = host.collect_loaded(2).expect("collect every profile");
        assert_eq!(collected.len(), 2);
        assert!(collected.iter().all(|row| row.result.as_ref() == Ok(&1)));
        assert!(collected.iter().any(|row| row.agent.is_none()));
        assert!(collected
            .iter()
            .any(|row| { row.agent.as_deref() == Some("adam") && row.profile == adam_profile }));

        let primary_view = reach(&primary.handle(Request::ReachView).await).clone();
        let adam_view = reach(&adam.handle(Request::ReachView).await).clone();
        assert_eq!(incoming_bodies(&primary_view), vec!["only primary"]);
        assert_eq!(incoming_bodies(&adam_view), vec!["only Adam"]);

        // Reloading the host reconstructs a new service from Adam's own home,
        // retaining Adam's profile and transcript without borrowing primary's.
        drop(adam);
        drop(primary);
        drop(host);
        let host = CorrespondenceHost::open(&primary_home, &agents_base, true);
        host.primary().restore(3).expect("primary reload");
        let adam = host.agent("adam", 3).expect("Adam reload");
        assert_eq!(adam.profile(), Some(adam_profile));
        let adam_view = reach(&adam.handle(Request::ReachView).await).clone();
        assert_eq!(incoming_bodies(&adam_view), vec!["only Adam"]);
        assert!(adam_view
            .correspondents
            .contains(&primary_profile.as_str().to_owned()));
        let primary_view = reach(&host.primary().handle(Request::ReachView).await).clone();
        assert!(primary_view.correspondents.contains(&adam_view.profile));
    }

    #[test]
    fn invalid_unknown_and_sideways_agent_names_never_select_an_identity() {
        let root = tempfile::tempdir().expect("config root");
        let primary_home = root.path().join("primary");
        let agents_base = crate::registry::agents_base(root.path());
        let agent_home = agents_base.join("adam");
        found(&primary_home);
        found(&agent_home);
        let host = CorrespondenceHost::open(&primary_home, &agents_base, true);
        host.primary().restore(1).expect("primary restore");

        for invalid in ["", ".", "..", "../adam", "adam/other", " adam", "CON"] {
            assert!(host.agent(invalid, 1).is_err(), "{invalid:?}");
        }
        assert!(host.agent("unknown", 1).is_err());
        assert!(host.loaded_agents().is_empty());
        assert_eq!(host.available_agents(), vec!["adam"]);

        // A self-contained agent daemon can select only its own primary
        // identity. `act_as` never turns into authority over a sibling.
        let self_contained = CorrespondenceHost::open(&agent_home, &agents_base, false);
        self_contained
            .primary()
            .restore(1)
            .expect("self-contained primary");
        assert!(self_contained.agent("adam", 1).is_err());
        assert!(self_contained.available_agents().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_directory_symlink_cannot_escape_the_canonical_base() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("config root");
        let primary_home = root.path().join("primary");
        let agents_base = crate::registry::agents_base(root.path());
        let outside = root.path().join("outside");
        found(&primary_home);
        found(&outside);
        std::fs::create_dir_all(&agents_base).expect("agents base");
        symlink(&outside, agents_base.join("escape")).expect("agent symlink");

        let host = CorrespondenceHost::open(&primary_home, &agents_base, true);
        host.primary().restore(1).expect("primary restore");
        assert!(host.agent("escape", 1).is_err());
        assert!(!host.available_agents().iter().any(|name| name == "escape"));
    }
}
