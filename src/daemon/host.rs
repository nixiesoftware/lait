//! The identity-scoped Lait daemon and its single local control endpoint.
//!
//! The host owns no Space state directly. It owns one [`ControlRouter`], which
//! lazily places Stations into addressed Orbits and keeps their SpaceBridges
//! inside this process. Historical per-home control sockets remain behind those
//! bridges as compatibility adapters, but new CLI, MCP, and web requests enter
//! through this endpoint first.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    ListenerOptions,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::{acquire_daemon_lock, DaemonLock};
use crate::control::{
    self, ClientRequest, ControlRoute, Request, Response, WorldClientRequest,
    CONTROL_PROTOCOL_VERSION,
};
use crate::orbital::{WorldCall, WorldCallErrorCode, WorldPackages, WorldReply};
#[cfg(test)]
use crate::transport::TransportFactory;

use super::{ControlRouter, OrbitDirectory, OrbitDoorbell};

/// A client for the current identity's one Lait daemon.
#[derive(Debug, Clone)]
pub struct LaitDaemonClient {
    home: PathBuf,
}

impl LaitDaemonClient {
    pub fn current() -> Result<Self> {
        Ok(Self {
            home: crate::config::lait_daemon_home()?,
        })
    }

    pub fn at(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub async fn probe(&self) -> control::Probe {
        control::probe(&self.home).await
    }

    pub async fn request(
        &self,
        route: ControlRoute,
        request: &Request,
        act_as: Option<&str>,
    ) -> Result<Response> {
        control::request_as_routed(&self.home, request, Some(route), act_as).await
    }

    /// Send a product-neutral call without importing that product's request or
    /// response schema into the daemon protocol.
    pub async fn call_world(
        &self,
        route: ControlRoute,
        call: WorldCall,
        act_as: Option<&str>,
    ) -> Result<WorldReply> {
        control::call_world(&self.home, route, call, act_as).await
    }

    /// Query an already-running Station without placing a vacant Orbit.
    pub async fn request_if_running(
        &self,
        route: ControlRoute,
        request: &Request,
    ) -> Result<Response> {
        control::request_routed_if_running(&self.home, request, route).await
    }

    pub async fn subscribe_space(
        &self,
        route: ControlRoute,
        since: u64,
    ) -> Result<control::Subscription> {
        control::subscribe_routed(&self.home, since, Some(route)).await
    }

    pub async fn subscribe_catalog(&self) -> Result<OrbitSubscription> {
        let name = control::control_name(&self.home)?;
        let mut stream = LocalStream::connect(name)
            .await
            .context("connect to Lait daemon")?;
        let envelope =
            ClientRequest::routed(Request::Subscribe { since: 0 }, ControlRoute::Daemon, None);
        let mut line = serde_json::to_string(&envelope).context("encode catalog subscribe")?;
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .await
            .context("write catalog subscribe")?;
        stream.flush().await.ok();
        Ok(OrbitSubscription {
            reader: BufReader::new(stream),
        })
    }
}

/// A catalog-wide stream of Orbit-tagged invalidations.
pub struct OrbitSubscription {
    reader: BufReader<LocalStream>,
}

impl OrbitSubscription {
    pub async fn next(&mut self) -> Result<Option<OrbitDoorbell>> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .await
            .context("read Orbit doorbell")?;
        if read == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim())
            .context("decode Orbit doorbell")
            .map(Some)
    }
}

/// The process-level daemon service behind the identity-scoped endpoint.
pub struct LaitDaemon {
    router: Arc<ControlRouter>,
    stopping: tokio::sync::watch::Sender<bool>,
}

impl LaitDaemon {
    fn new(router: Arc<ControlRouter>) -> Self {
        Self {
            router,
            stopping: tokio::sync::watch::channel(false).0,
        }
    }

    fn begin_stop(&self) {
        self.stopping.send_replace(true);
    }

    async fn serve(self: Arc<Self>, home: &Path) -> Result<()> {
        let control = control::control_name(home)?;
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(home));
        let listener = ListenerOptions::new()
            .name(control)
            .create_tokio()
            .context("bind Lait daemon control channel")?;
        tracing::info!("Lait daemon online");

        let mut stop = self.stopping.subscribe();
        let mut connections = tokio::task::JoinSet::new();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                accepted = listener.accept() => match accepted {
                    Ok(stream) => {
                        let daemon = self.clone();
                        connections.spawn(async move { daemon.handle_conn(stream).await });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Lait daemon accept failed");
                        break;
                    }
                },
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "Lait daemon connection task failed");
                    }
                }
            }
        }

        self.begin_stop();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "Lait daemon connection failed during shutdown");
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "Lait daemon connections did not drain during shutdown ({} remain)",
                connections.len()
            )
        })?;
        Ok(())
    }

    async fn handle_conn(self: Arc<Self>, stream: LocalStream) {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
        let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
            Ok(value) => value,
            Err(error) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err(format!("bad request: {error}")),
                )
                .await;
                return;
            }
        };

        if value.get("call").is_some() {
            let WorldClientRequest {
                route,
                act_as,
                call,
            } = match serde_json::from_value(value) {
                Ok(request) => request,
                Err(error) => {
                    let _ = write_line(
                        &mut write_half,
                        &Response::err(format!("bad World call: {error}")),
                    )
                    .await;
                    return;
                }
            };
            let reply = self
                .router
                .call_world(route, &call, act_as.as_deref())
                .await
                .unwrap_or_else(|error| {
                    WorldReply::error(&call, WorldCallErrorCode::InvalidCall, format!("{error:#}"))
                });
            let _ = write_line(&mut write_half, &reply).await;
            return;
        }

        let ClientRequest {
            route,
            if_running,
            act_as,
            request,
        } = match serde_json::from_value::<ClientRequest>(value) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err(format!("bad request: {error}")),
                )
                .await;
                return;
            }
        };

        if matches!(request, Request::Hello { .. }) {
            let _ = write_line(
                &mut write_half,
                &Response::Hello {
                    protocol_version: CONTROL_PROTOCOL_VERSION,
                },
            )
            .await;
            return;
        }

        let Some(route) = route else {
            let _ = write_line(
                &mut write_half,
                &Response::err("the Lait daemon requires an explicit control route"),
            )
            .await;
            return;
        };

        if if_running {
            let response = match (&route, &request) {
                (
                    ControlRoute::Space { .. },
                    Request::Status | Request::Id | Request::ConfigReload,
                ) => self
                    .router
                    .request_running(route, &request)
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}"))),
                _ => Response::err(
                    "passive dispatch is only available for status, id, or config reload through \
                     an explicit Space route",
                ),
            };
            let _ = write_line(&mut write_half, &response).await;
            return;
        }

        match (route, request) {
            (ControlRoute::Daemon, Request::Stop) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::Ok {
                        message: Some("stopping Lait daemon".into()),
                    },
                )
                .await;
                self.begin_stop();
            }
            (ControlRoute::Daemon, Request::Subscribe { .. }) => {
                self.stream_catalog(write_half).await;
            }
            (ControlRoute::Daemon, _) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err("request has no daemon-scoped handler"),
                )
                .await;
            }
            (route @ ControlRoute::Space { .. }, Request::Subscribe { since }) => {
                self.stream_space(write_half, route, since).await;
            }
            (ControlRoute::World { .. }, Request::Subscribe { .. }) => {
                let _ = write_line(
                    &mut write_half,
                    &Response::err("subscriptions require a Space route"),
                )
                .await;
            }
            (route, request) => {
                let response = self
                    .router
                    .request_routed(route, &request, act_as.as_deref())
                    .await
                    .unwrap_or_else(|error| Response::err(format!("{error:#}")));
                let _ = write_line(&mut write_half, &response).await;
            }
        }
    }

    async fn stream_catalog(&self, mut write_half: tokio::io::WriteHalf<LocalStream>) {
        let mut doorbells = self.router.subscribe();
        let mut stop = self.stopping.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                doorbell = doorbells.recv() => match doorbell {
                    Ok(doorbell) => {
                        if write_line(&mut write_half, &doorbell).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // OrbitDoorbell has no catalog-wide reset frame. Closing
                        // makes the web adapter emit `lagged` and reconnect,
                        // which triggers its normal authoritative rebaseline.
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    async fn stream_space(
        &self,
        mut write_half: tokio::io::WriteHalf<LocalStream>,
        route: ControlRoute,
        since: u64,
    ) {
        let ControlRoute::Space { address } = &route else {
            return;
        };
        let resolved = match self.router.place_address(address).await {
            Ok(resolved) => resolved,
            Err(error) => {
                let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                return;
            }
        };
        let mut subscription =
            match control::subscribe_routed(&resolved.home, since, Some(route)).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = write_line(&mut write_half, &Response::err(format!("{error:#}"))).await;
                    return;
                }
            };
        let mut stop = self.stopping.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                },
                next = subscription.next() => match next {
                    Ok(Some(doorbell)) => {
                        if write_line(&mut write_half, &doorbell).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "proxied Space subscription ended");
                        break;
                    }
                }
            }
        }
    }
}

/// Joinable ownership of the process endpoint and its process-wide lock.
pub(crate) struct LaitDaemonRunner {
    home: PathBuf,
    daemon: Arc<LaitDaemon>,
    _lock: DaemonLock,
}

#[derive(Clone)]
pub(crate) struct LaitDaemonStop {
    daemon: Weak<LaitDaemon>,
}

impl LaitDaemonStop {
    pub(crate) fn stop(&self) {
        if let Some(daemon) = self.daemon.upgrade() {
            daemon.begin_stop();
        }
    }
}

impl LaitDaemonRunner {
    pub(crate) fn start(home: PathBuf, router: Arc<ControlRouter>) -> Result<Self> {
        let lock = acquire_daemon_lock(&home)?;
        Ok(Self {
            home,
            daemon: Arc::new(LaitDaemon::new(router)),
            _lock: lock,
        })
    }

    pub(crate) fn stop_handle(&self) -> LaitDaemonStop {
        LaitDaemonStop {
            daemon: Arc::downgrade(&self.daemon),
        }
    }

    pub(crate) async fn run(self) -> Result<()> {
        let serve_result = self.daemon.clone().serve(&self.home).await;
        let shutdown_result = self
            .daemon
            .router
            .shutdown()
            .await
            .context("drain Station placements");
        #[cfg(unix)]
        let _ = std::fs::remove_file(crate::config::socket_path(&self.home));
        serve_result?;
        shutdown_result
    }
}

/// Run the current identity's always-on Lait daemon.
pub async fn run_lait_daemon(packages: WorldPackages) -> Result<()> {
    let identity = crate::config::identity_dir()?;
    let config_root = crate::config::config_root()?;
    let self_contained = std::env::var_os("LAIT_HOME").is_some();
    let agents_base = crate::registry::agents_base(&config_root);
    let home = crate::config::lait_daemon_home()?;
    let router = Arc::new(ControlRouter::new(
        OrbitDirectory::new(identity, agents_base, self_contained),
        packages,
    ));
    let runner = LaitDaemonRunner::start(home, router)?;
    let stop = runner.stop_handle();
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        stop.stop();
    });
    let result = runner.run().await;
    signal.abort();
    let _ = signal.await;
    result
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

async fn write_line<T: serde::Serialize>(
    write_half: &mut tokio::io::WriteHalf<LocalStream>,
    value: &T,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)
        .unwrap_or_else(|_| "{\"kind\":\"error\",\"message\":\"encode failure\"}".into());
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await
}

/// Factory-injected constructor used by lifecycle tests.
#[cfg(test)]
pub(crate) fn runner_with_factory(
    home: PathBuf,
    directory: OrbitDirectory,
    factory: Arc<dyn TransportFactory>,
) -> Result<LaitDaemonRunner> {
    LaitDaemonRunner::start(
        home,
        Arc::new(ControlRouter::with_factory(
            directory,
            factory,
            crate::world::packages(),
        )),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::net::Network;
    use crate::spaces::{Origin, SpaceEntry};
    use crate::transport::mem::MemNet;
    use crate::transport::Transport;

    static HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct MemFactory(MemNet);

    #[async_trait]
    impl TransportFactory for MemFactory {
        async fn build(
            &self,
            identity_seed: &[u8; 32],
            _network: &Network,
            _protocols: comms::Protocols<'_>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(
                self.0.peer(crate::crypto::device_from_seed(identity_seed)),
            ))
        }
    }

    fn formed_directory(
        tag: &str,
        seed: &[u8; 32],
    ) -> (
        PathBuf,
        PathBuf,
        OrbitDirectory,
        super::super::ResolvedOrbit,
    ) {
        let n = HOME_COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("lait-host-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("orbit");
        let daemon_home = base.join("daemon");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&daemon_home).unwrap();
        let (mechanics, _) = crate::orbital::form_space(&home, seed, "Host Test").unwrap();
        std::fs::write(
            home.join("secret.key"),
            data_encoding::HEXLOWER.encode(seed),
        )
        .unwrap();
        let id = super::super::LocalOrbitId::for_store(&home);
        let directory = OrbitDirectory::with_entries(
            home.clone(),
            home.join("agents"),
            false,
            vec![SpaceEntry {
                space: mechanics.space().as_str().to_string(),
                name: "Host Test".into(),
                path: home.to_string_lossy().to_string(),
                origin: Origin::Founded,
                host_nick: String::new(),
                last_opened: 1,
                projects: Vec::new(),
            }],
        );
        let resolved = directory.resolve(id.as_str()).unwrap();
        (base, daemon_home, directory, resolved)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_host_endpoint_places_routes_streams_and_drains_an_orbit() {
        let seed = [211; 32];
        let (base, daemon_home, directory, resolved) = formed_directory("route", &seed);
        let orbit_home = resolved.home.clone();
        let runner = runner_with_factory(
            daemon_home.clone(),
            directory,
            Arc::new(MemFactory(MemNet::new())),
        )
        .unwrap();
        let completion = tokio::spawn(runner.run());
        let client = LaitDaemonClient::at(daemon_home.clone());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !matches!(client.probe().await, control::Probe::Healthy) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "host endpoint did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut catalog = client.subscribe_catalog().await.unwrap();
        let route = control::station_route(resolved.address.clone());
        assert!(matches!(
            client
                .request_if_running(route.clone(), &Request::Status)
                .await
                .unwrap(),
            Response::Error { .. }
        ));
        drop(
            acquire_daemon_lock(&orbit_home)
                .expect("a passive status probe must not activate the Orbit"),
        );
        assert!(matches!(
            client.request(route, &Request::Status, None).await.unwrap(),
            Response::Status(_)
        ));
        let call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList).unwrap();
        let world_route = ControlRoute::World {
            address: resolved.address,
            world: call.world().as_str().to_string(),
        };
        let reply = client
            .call_world(world_route, call.clone(), None)
            .await
            .unwrap();
        let value = issues_app::decode_reply(&call, reply).unwrap();
        assert!(matches!(
            serde_json::from_value::<issues_app::IssuesResponse>(value).unwrap(),
            issues_app::IssuesResponse::Projects { .. }
        ));
        let ring = tokio::time::timeout(Duration::from_secs(2), catalog.next())
            .await
            .expect("catalog doorbell")
            .unwrap()
            .expect("catalog stream remains open");
        assert_eq!(
            ring.orbit,
            super::super::LocalOrbitId::for_store(&orbit_home)
        );
        assert!(
            acquire_daemon_lock(&daemon_home).is_err(),
            "host process lease remains held"
        );
        assert!(
            acquire_daemon_lock(&orbit_home).is_err(),
            "placed Station retains its Orbit lease"
        );

        client
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await
            .unwrap();
        completion.await.unwrap().unwrap();
        drop(acquire_daemon_lock(&daemon_home).expect("host lease released"));
        drop(acquire_daemon_lock(&orbit_home).expect("Orbit returned to vacancy"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_rejects_a_stale_space_expectation_before_placement() {
        let seed = [212; 32];
        let (base, daemon_home, directory, mut resolved) = formed_directory("scope", &seed);
        let orbit_home = resolved.home.clone();
        resolved.address.space = crate::ids::SpaceId::from_digest([99; 16]);
        let runner = runner_with_factory(
            daemon_home.clone(),
            directory,
            Arc::new(MemFactory(MemNet::new())),
        )
        .unwrap();
        let completion = tokio::spawn(runner.run());
        let client = LaitDaemonClient::at(daemon_home);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !matches!(client.probe().await, control::Probe::Healthy) {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let response = client
            .request(
                ControlRoute::Space {
                    address: resolved.address,
                },
                &Request::Status,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(response, Response::Error { .. }));
        drop(
            acquire_daemon_lock(&orbit_home)
                .expect("a rejected address must not activate the Orbit"),
        );

        client
            .request(ControlRoute::Daemon, &Request::Stop, None)
            .await
            .unwrap();
        completion.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(base);
    }
}
