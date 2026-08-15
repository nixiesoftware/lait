//! FIND-5: reproducible structural and wall measurements for the scan paths
//! that F1 is intended to replace.
//!
//! The default suite runs a 100-Issue smoke point. Set
//! `LAIT_ISSUES_SCAN_FULL=1` for the frozen 1k/10k/50k corpus and
//! capture stdout as the raw artifact. Corpus construction is outside every
//! sample; cold means a new `IssuesWorld` at an uncached Manifest root, while
//! warm means another query against that exact root and World instance.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use fabric::{CollaborativeView, ListElement};
use replica::body::{BodyKey, SchemaId, WorldId};
use runtime::world::{BodyReader, Context, PrincipalFacts, Projection, Query, World as _};
use serde::{Deserialize, Serialize};

use crate::contract;
use crate::implementation::{scan_observation, IssuesWorld};
use crate::views::ProjectMeta;
use crate::IssueQuery;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Spec {
    version: u32,
    description: String,
    corpus: CorpusSpec,
    measurement: MeasurementSpec,
    call_paths: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusSpec {
    scales: Vec<usize>,
    smoke_scale: usize,
    projects: usize,
    edge_pattern: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MeasurementSpec {
    warm_iterations: usize,
    browser_warmups: usize,
    browser_iterations: usize,
    browser_query: String,
}

fn spec() -> Spec {
    serde_json::from_str(include_str!("../../../benchmarks/issues-scan.json"))
        .expect("the committed Issues scan corpus must parse strictly")
}

struct StoredBody {
    schema: SchemaId,
    view: CollaborativeView,
}

struct CorpusReader {
    bodies: BTreeMap<BodyKey, StoredBody>,
    collaborative_reads: AtomicU64,
    body_scan_visits: AtomicU64,
}

impl CorpusReader {
    fn reset(&self) {
        self.collaborative_reads.store(0, Ordering::Relaxed);
        self.body_scan_visits.store(0, Ordering::Relaxed);
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.collaborative_reads.load(Ordering::Relaxed),
            self.body_scan_visits.load(Ordering::Relaxed),
        )
    }
}

impl BodyReader for CorpusReader {
    fn read_body(&self, _key: &BodyKey) -> Option<Vec<u8>> {
        None
    }

    fn read_collaborative_body(
        &self,
        key: &BodyKey,
    ) -> Result<CollaborativeView, fabric::projection::Failure> {
        let stored = self
            .bodies
            .get(key)
            .ok_or(fabric::projection::Failure::NotCollaborative)?;
        self.collaborative_reads.fetch_add(1, Ordering::Relaxed);
        Ok(stored.view.clone())
    }

    fn bodies_with_schema(&self, world: &WorldId, schema: &SchemaId) -> Vec<BodyKey> {
        self.bodies
            .iter()
            .filter_map(|(key, stored)| {
                self.body_scan_visits.fetch_add(1, Ordering::Relaxed);
                (&key.world == world && &stored.schema == schema).then(|| key.clone())
            })
            .collect()
    }

    fn body_version(&self, _key: &BodyKey) -> Option<fabric::Version> {
        Some(fabric::Version::empty())
    }

    fn anchor_in_body(
        &self,
        _key: &BodyKey,
        _path: &str,
        _position: u64,
    ) -> Option<fabric::Anchor> {
        None
    }

    fn resolve_anchor(&self, _key: &BodyKey, _anchor: &fabric::Anchor) -> fabric::AnchorResolution {
        fabric::AnchorResolution::Drifted
    }

    fn content_status(
        &self,
        _content: &replica::content::ContentRef,
    ) -> Option<runtime::world::ContentStatus> {
        None
    }

    fn body_stamp(&self, _key: &BodyKey) -> Option<Vec<u8>> {
        Some(vec![1])
    }
}

struct Corpus {
    reader: CorpusReader,
    projects: Vec<String>,
    graph_doc: String,
    issues: usize,
    edges: usize,
}

fn identifier(prefix: &str, value: usize) -> String {
    format!("{prefix}_{value:026}")
}

fn corpus(issues: usize, projects: usize) -> Corpus {
    assert!(issues > 1);
    assert!(projects > 0);
    let space = space();
    let mut catalog = CollaborativeView::default();
    let project_ids: Vec<String> = (0..projects)
        .map(|index| identifier("prj", index))
        .collect();
    for (index, project) in project_ids.iter().enumerate() {
        let meta = ProjectMeta {
            name: format!("Project {index:02}"),
            key: format!("P{index:02}"),
            color: String::new(),
            ..ProjectMeta::default()
        };
        catalog.maps.entry("projects".into()).or_default().insert(
            project.clone(),
            serde_json::to_vec(&meta).expect("project JSON"),
        );
    }

    let docs: Vec<String> = (0..issues).map(|index| identifier("iss", index)).collect();
    for (index, doc) in docs.iter().enumerate() {
        catalog
            .maps
            .entry("seqs".into())
            .or_default()
            .insert(doc.clone(), (index + 1).to_string().into_bytes());
        let project = &project_ids[index % projects];
        catalog
            .lists
            .entry(format!("board/{}", project.to_ascii_lowercase()))
            .or_default()
            .push(ListElement {
                element: format!("element-{index:08}"),
                value: doc.as_bytes().to_vec(),
            });
        if let Some(next) = docs.get(index + 1) {
            catalog
                .maps
                .entry("edges".into())
                .or_default()
                .insert(format!("{doc}|relates|{next}"), b"1".to_vec());
        }
    }

    let mut bodies = BTreeMap::new();
    bodies.insert(
        contract::catalog_key(&space),
        StoredBody {
            schema: contract::catalog_schema(),
            view: catalog,
        },
    );
    for (index, doc) in docs.iter().enumerate() {
        let project = &project_ids[index % projects];
        let mut view = CollaborativeView::default();
        view.registers
            .insert("projectid".into(), project.as_bytes().to_vec());
        view.registers.insert(
            "title".into(),
            format!("Issue {index:05} deterministic baseline").into_bytes(),
        );
        view.registers.insert(
            "status".into(),
            contract::DEFAULT_STATUS.as_bytes().to_vec(),
        );
        view.registers.insert("priority".into(), b"none".to_vec());
        bodies.insert(
            contract::issue_key(doc),
            StoredBody {
                schema: contract::issue_schema(),
                view,
            },
        );
    }
    Corpus {
        reader: CorpusReader {
            bodies,
            collaborative_reads: AtomicU64::new(0),
            body_scan_visits: AtomicU64::new(0),
        },
        projects: project_ids,
        graph_doc: docs[issues / 2].clone(),
        issues,
        edges: issues - 1,
    }
}

fn space() -> mechanics::ids::SpaceId {
    mechanics::ids::SpaceId::from_digest([41u8; 16])
}

fn principal() -> PrincipalFacts {
    let device = mechanics::actor::device_from_seed(&[43u8; 32]);
    PrincipalFacts {
        actor: mechanics::ids::ActorId::from_incept_hash(&"ab".repeat(32)),
        station: mechanics::station::Key::from_device(&device).expect("station key"),
        device,
        space: space(),
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
    }
}

fn query(world: &IssuesWorld, ctx: &Context<'_>, request: IssueQuery) -> Projection {
    world
        .query(
            ctx,
            Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: request.to_json(),
            },
        )
        .expect("baseline query")
}

#[derive(Debug, Clone, Serialize)]
struct Observation {
    parsed_bodies: u64,
    body_scan_visits: u64,
    graph_edge_visits: u64,
    allocation_calls: u64,
    allocated_bytes: u64,
    returned_bytes: usize,
    wall_micros: u128,
}

fn observe(reader: &CorpusReader, operation: impl FnOnce() -> Vec<Projection>) -> Observation {
    reader.reset();
    let ((projections, graph_edge_visits), allocations) =
        crate::test_allocation::measure(|| scan_observation::measure(operation));
    let (parsed_bodies, body_scan_visits) = reader.counts();
    Observation {
        parsed_bodies,
        body_scan_visits,
        graph_edge_visits,
        allocation_calls: allocations.calls,
        allocated_bytes: allocations.bytes,
        returned_bytes: projections
            .iter()
            .map(|projection| projection.bytes.len())
            .sum(),
        wall_micros: allocations.wall_micros,
    }
}

#[derive(Debug, Serialize)]
struct Family {
    cold: Observation,
    warm: Vec<Observation>,
}

#[derive(Debug, Serialize)]
struct ScaleReport {
    issues: usize,
    projects: usize,
    edges: usize,
    list: Family,
    graph: Family,
    browser_board_fanout: Family,
}

fn repeated(count: usize, mut operation: impl FnMut() -> Observation) -> Vec<Observation> {
    (0..count).map(|_| operation()).collect()
}

fn measure_scale(spec: &Spec, issues: usize) -> ScaleReport {
    let fixture = corpus(issues, spec.corpus.projects);
    let facts = principal();
    let ctx = Context::with_reads(&facts, &fixture.reader, [51u8; 32]);

    let list_world = IssuesWorld::new();
    let list_request = || IssueQuery::List {
        project: None,
        label: None,
        status: None,
        milestone: None,
        mine: None,
        all: true,
        me: None,
    };
    let list_cold = observe(&fixture.reader, || {
        vec![query(&list_world, &ctx, list_request())]
    });
    let list_warm = repeated(spec.measurement.warm_iterations, || {
        observe(&fixture.reader, || {
            vec![query(&list_world, &ctx, list_request())]
        })
    });

    let graph_world = IssuesWorld::new();
    let graph_request = || IssueQuery::Graph {
        doc: fixture.graph_doc.clone(),
        me: None,
    };
    let graph_cold = observe(&fixture.reader, || {
        vec![query(&graph_world, &ctx, graph_request())]
    });
    let graph_warm = repeated(spec.measurement.warm_iterations, || {
        observe(&fixture.reader, || {
            vec![query(&graph_world, &ctx, graph_request())]
        })
    });

    let search_world = IssuesWorld::new();
    let board_fanout = || {
        fixture
            .projects
            .iter()
            .map(|project| {
                query(
                    &search_world,
                    &ctx,
                    IssueQuery::Board {
                        project: project.clone(),
                        me: None,
                    },
                )
            })
            .collect()
    };
    let browser_cold = observe(&fixture.reader, &board_fanout);
    let browser_warm = repeated(spec.measurement.warm_iterations, || {
        observe(&fixture.reader, &board_fanout)
    });

    let expected_bodies = u64::try_from(issues + 1).expect("baseline scale fits u64");
    let expected_scans = expected_bodies * 2;
    for cold in [&list_cold, &graph_cold, &browser_cold] {
        assert_eq!(cold.parsed_bodies, expected_bodies);
        assert_eq!(cold.body_scan_visits, expected_scans);
    }
    for warm in list_warm.iter().chain(&graph_warm).chain(&browser_warm) {
        assert_eq!(warm.parsed_bodies, 0, "a warm root reparsed Bodies");
        assert_eq!(warm.body_scan_visits, 0, "a warm root rescanned Bodies");
    }
    let expected_edges = u64::try_from(fixture.edges * 2).expect("edge count fits u64");
    assert_eq!(graph_cold.graph_edge_visits, expected_edges);
    assert!(graph_warm
        .iter()
        .all(|sample| sample.graph_edge_visits == expected_edges));
    assert_eq!(list_cold.graph_edge_visits, 0);
    assert_eq!(browser_cold.graph_edge_visits, 0);

    ScaleReport {
        issues: fixture.issues,
        projects: fixture.projects.len(),
        edges: fixture.edges,
        list: Family {
            cold: list_cold,
            warm: list_warm,
        },
        graph: Family {
            cold: graph_cold,
            warm: graph_warm,
        },
        browser_board_fanout: Family {
            cold: browser_cold,
            warm: browser_warm,
        },
    }
}

#[test]
fn current_issue_scan_work_is_measured_at_cold_and_warm_roots() {
    let spec = spec();
    assert_eq!(spec.version, 1);
    assert!(!spec.description.is_empty());
    assert_eq!(spec.corpus.edge_pattern, "forward-relates-chain");
    assert_eq!(spec.call_paths.len(), 3);
    assert!(spec.measurement.browser_warmups > 0);
    assert!(spec.measurement.browser_iterations > 0);
    assert!(!spec.measurement.browser_query.is_empty());

    let scales = if std::env::var_os("LAIT_ISSUES_SCAN_FULL").is_some() {
        spec.corpus.scales.clone()
    } else {
        vec![spec.corpus.smoke_scale]
    };
    let reports: Vec<ScaleReport> = scales
        .into_iter()
        .map(|scale| measure_scale(&spec, scale))
        .collect();
    for report in &reports {
        println!(
            "issues={} list(cold={}us warm={}us) graph(cold={}us warm={}us edges={}) search-fanout(cold={}us warm={}us bytes={})",
            report.issues,
            report.list.cold.wall_micros,
            report.list.warm[0].wall_micros,
            report.graph.cold.wall_micros,
            report.graph.warm[0].wall_micros,
            report.graph.warm[0].graph_edge_visits,
            report.browser_board_fanout.cold.wall_micros,
            report.browser_board_fanout.warm[0].wall_micros,
            report.browser_board_fanout.warm[0].returned_bytes,
        );
    }

    if std::env::var_os("LAIT_ISSUES_SCAN_FULL").is_some() {
        let report = serde_json::json!({
            "version": 1,
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "corpus": {
                "projects": spec.corpus.projects,
                "edgePattern": spec.corpus.edge_pattern,
            },
            "callPaths": spec.call_paths,
            "scales": reports,
        });
        println!(
            "ISSUES_SCAN_REPORT_JSON={}",
            serde_json::to_string(&report).expect("report JSON")
        );
    }
}
