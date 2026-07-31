//! Range-attached comments — the durable half of the anchor algebra.
//!
//! A caret rides a datagram and is forgotten. A comment's span is written into
//! the Body and has to survive every edit made after it, which is why the
//! record stores the ANCHOR and the projection computes the POSITION on every
//! read. These tests drive a real Runtime so the anchors are minted and
//! resolved by the convergence engine rather than by a stub, and they assert
//! what goes wrong if the rule is relaxed: a stale position, a comment dropped
//! because its text was edited away, an anchor stored into a field the algebra
//! cannot move, and a span reported as lost because something beside it was
//! edited.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lait::dto::{CommentAnchorState, IssueView};
use lait::ids::{ActorId, DeviceId, DocId, ProjectId, SystemUlidSource};
use lait::world::contract::{self, IssueIntent, IssueQuery};
use lait::world::IssuesWorld;
use mechanics::crypto::AuthorizedBodyKey;
use replica::frontier::AuthorityFrontier;
use runtime::{
    ActivationOptions, Authority, CommsOptions, Intent, LocalIdentity, Query, RequestId, Runtime,
    RuntimeBuilder, Session, Station, WorldError,
};

const FOUNDER_SEED: [u8; 32] = [37u8; 32];
const RECOVERY_SEED: [u8; 32] = [38u8; 32];
const STATION_A_SEED: [u8; 32] = [39u8; 32];
const STATION_B_SEED: [u8; 32] = [40u8; 32];
const WRITER_SEED: [u8; 32] = [41u8; 32];
const EPOCH: [u8; 16] = [42u8; 16];
const EPOCH_KEY: [u8; 32] = [43u8; 32];

/// The description every test attaches to. Ascii and unremarkable: the span
/// arithmetic is the subject, not the text.
const DESCRIPTION: &str = "the quick brown fox";
/// `quick` — chars 4 through 9 of [`DESCRIPTION`].
const SPAN: (u64, u64) = (4, 9);

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-anchor-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn my_actor() -> ActorId {
    ActorId::from_incept_hash(&"c".repeat(64))
}

fn my_device() -> DeviceId {
    mechanics::crypto::device_from_seed(&WRITER_SEED)
}

struct WriterAuthority;
impl runtime::AuthorityView for WriterAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::PrincipalResolution> {
        Some(runtime::PrincipalResolution {
            actor: my_actor(),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![8]),
        })
    }
}

struct AnyKnownSigner;
impl replica::AuthoritySource for AnyKnownSigner {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [WRITER_SEED, STATION_A_SEED, STATION_B_SEED]
            .iter()
            .any(|seed| mechanics::crypto::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

struct AcceptingIncorporator;
impl replica::AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::AuthorityBatchReceipt, String> {
        Ok(replica::AuthorityBatchReceipt {
            space: coordinates().verify().unwrap().space.clone(),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![8]),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn coordinates() -> runtime::SignedCoordinates {
    use runtime::coordinates::{ApproachRoute, CoordinatesAdmission, CoordinatesPayload};
    let rc = mechanics::space::recovery_commit(&mechanics::space::recovery_pub_of(&RECOVERY_SEED))
        .unwrap();
    let device = mechanics::space::recovery_pub_of(&FOUNDER_SEED);
    let ws = mechanics::space::derive_space_id(&device, &[11u8; 16], &rc);
    let (incept, _actor) =
        mechanics::actor::incept_single(&FOUNDER_SEED, &ws, [1u8; 16], [2u8; 16], None);
    let payload = CoordinatesPayload {
        space: <[u8; 29]>::try_from(ws.as_str().as_bytes()).unwrap(),
        salt: [11u8; 16],
        recovery_root: rc,
        founder_inception: postcard::to_stdvec(&incept).unwrap(),
        display_name_hint: "Anchor Space".into(),
        approach_station: mechanics::crypto::device_from_seed(&STATION_A_SEED)
            .key_bytes()
            .unwrap(),
        approach_nick_hint: "a".into(),
        approach_routes: vec![ApproachRoute::DirectIpv4 {
            ip: [127, 0, 0, 1],
            port: 4242,
        }],
        admission: CoordinatesAdmission::None,
    };
    runtime::SignedCoordinates::sign(payload, &STATION_A_SEED)
}

fn product_runtime(root: &std::path::Path) -> Runtime {
    let registry = RuntimeBuilder::new()
        .register(Arc::new(IssuesWorld::new()))
        .build()
        .unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(WriterAuthority),
        Arc::new(replica::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
        )),
    )
}

/// The daemon-side driver: docks a session and adapts intents/queries.
struct Driver {
    session: Session,
    writer: LocalIdentity,
    now: u64,
}

impl Driver {
    fn dock(station: &Station) -> Self {
        let writer = Runtime::identity_from_seed(&WRITER_SEED);
        let session = station.dock(&contract::world_id(), &writer).unwrap();
        Self {
            session,
            writer,
            now: 1_700_000_000,
        }
    }

    fn ts(&mut self) -> u64 {
        self.now += 1;
        self.now
    }

    fn submit(&self, intent: &IssueIntent) -> Result<contract::IssueEffect, WorldError> {
        let signed = self
            .writer
            .sign_action(
                &self.session,
                RequestId::mint(),
                Intent {
                    schema: contract::issue_schema(),
                    schema_version: contract::ISSUE_SCHEMA_VERSION,
                    payload: intent.to_json(),
                },
            )
            .unwrap();
        let committed = self.session.submit(signed)?;
        Ok(contract::IssueEffect::from_json(&committed.effect).unwrap())
    }

    fn view(&self, doc: &str) -> IssueView {
        let bytes = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: IssueQuery::View {
                    doc: doc.to_string(),
                    me: None,
                }
                .to_json(),
            })
            .unwrap()
            .bytes;
        serde_json::from_slice(&bytes).unwrap()
    }

    fn seed(&mut self) -> String {
        let ts = self.ts();
        let project = ProjectId::mint(&SystemUlidSource).as_str().to_string();
        self.submit(&contract::initialize_tracker_intent(
            "Anchor Space",
            ts,
            &project,
            "Engineering",
            "eng",
            my_device().as_str(),
        ))
        .unwrap();
        let doc = DocId::mint(&SystemUlidSource).as_str().to_string();
        let ts = self.ts();
        self.submit(&IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: doc.clone(),
            project,
            title: "First issue".into(),
            priority: "high".into(),
            assignees: vec![],
            labels: vec![],
            new_labels: vec![],
            body: Some(DESCRIPTION.into()),
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
        doc
    }

    /// Attach `body` to `field[start..end]`, returning the comment's id.
    fn comment_at(
        &mut self,
        doc: &str,
        body: &str,
        field: &str,
        start: u64,
        end: Option<u64>,
    ) -> Result<String, WorldError> {
        let id = lait::ids::mint_comment_id(&SystemUlidSource);
        let ts = self.ts();
        self.submit(&IssueIntent::CommentAt {
            doc: doc.to_string(),
            body: body.into(),
            field: field.into(),
            start,
            end,
            id: id.clone(),
            parent: None,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })?;
        Ok(id)
    }

    fn describe(&mut self, doc: &str, description: &str) {
        let ts = self.ts();
        self.submit(&IssueIntent::IssueEdit {
            doc: doc.to_string(),
            title: None,
            status: None,
            priority: None,
            description: Some(description.to_string()),
            duedate: None,
            estimate: None,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    }
}

fn offline(root: &std::path::Path) -> Station {
    product_runtime(root)
        .create()
        .unwrap()
        .open(ActivationOptions::offline())
        .unwrap()
}

/// The anchor state one comment reports in the view as it stands now.
fn state_of(view: &IssueView, id: &str) -> CommentAnchorState {
    let comment = view
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("comment {id} is missing from the view"));
    let anchor = comment
        .anchor
        .as_ref()
        .unwrap_or_else(|| panic!("comment {id} carries no anchor"));
    assert_eq!(anchor.field, "description");
    anchor.state
}

/// The text the comment's span names, read out of the description with the
/// offsets the projection resolved — never with the offsets the test asked for.
///
/// Slicing with the constants the assertion above already pinned proves the
/// constants agree with themselves. Slicing with the resolved span is what
/// says the comment still marks the word its author selected.
fn marked_text(view: &IssueView, id: &str) -> String {
    let CommentAnchorState::At { start, end } = state_of(view, id) else {
        panic!("comment {id} has no position to read");
    };
    view.description
        .chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect()
}

/// The span reads back where it was put, and the comment is an ordinary
/// comment in every other respect.
#[test]
fn a_span_comment_reads_back_the_position_it_was_taken_at() {
    let root = temp_root("mint");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(
            &doc,
            "this word is wrong",
            "description",
            SPAN.0,
            Some(SPAN.1),
        )
        .unwrap();

    let view = driver.view(&doc);
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].body, "this word is wrong");
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At {
            start: SPAN.0,
            end: SPAN.1
        }
    );
    assert_eq!(marked_text(&view, &id), "quick");

    // The wire shape a client draws from, pinned.
    let json = serde_json::to_value(&view.comments[0]).unwrap();
    assert_eq!(
        json["anchor"],
        serde_json::json!({
            "field": "description",
            "state": {"kind": "at", "start": 4, "end": 9},
        })
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// **The central test.** An insertion before the span moves the position, and
/// the NEXT read reports the new one.
///
/// The comment record is untouched by that edit — no intent rewrote it — so the
/// only way the second read can be right is by resolving the stored anchor
/// again. A cached resolution, or a position stored at submit time, still
/// answers 4..9 here and points at "the q".
#[test]
fn an_insertion_before_the_span_moves_it_on_the_next_read() {
    let root = temp_root("move");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(
            &doc,
            "this word is wrong",
            "description",
            SPAN.0,
            Some(SPAN.1),
        )
        .unwrap();
    assert_eq!(
        state_of(&driver.view(&doc), &id),
        CommentAnchorState::At {
            start: SPAN.0,
            end: SPAN.1
        }
    );

    // Four characters in front of everything.
    driver.describe(&doc, "PRE the quick brown fox");

    let view = driver.view(&doc);
    assert_eq!(view.description, "PRE the quick brown fox");
    assert_eq!(view.comments.len(), 1, "the edit rewrote no comment");
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At {
            start: SPAN.0 + 4,
            end: SPAN.1 + 4
        },
        "the span must be resolved against the Body as it stands, not as it stood"
    );
    assert_eq!(
        marked_text(&view, &id),
        "quick",
        "the span still names the same word"
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// Deleting the marked text drifts the span and keeps the comment. A comment
/// whose text was edited away is still a comment somebody wrote.
#[test]
fn deleting_the_marked_text_drifts_the_span_and_keeps_the_comment() {
    let root = temp_root("drift");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(
            &doc,
            "this word is wrong",
            "description",
            SPAN.0,
            Some(SPAN.1),
        )
        .unwrap();
    driver.describe(&doc, "the brown fox");

    let view = driver.view(&doc);
    assert_eq!(state_of(&view, &id), CommentAnchorState::Drifted);
    let comment = view
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(id.as_str()))
        .expect("a drifted comment is still a comment");
    assert_eq!(comment.body, "this word is wrong");
    assert_eq!(comment.author, my_actor());

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// An atomic field has no positions inside it for the algebra to move, so the
/// attachment is refused rather than stored.
///
/// `title` is a register: replaced whole on every edit. `anchor_in_body` would
/// mint an anchor for it anyway — it validates no path — and that anchor would
/// answer position zero forever without ever reporting drift.
#[test]
fn an_atomic_field_cannot_carry_a_span() {
    let root = temp_root("atomic");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let refused = driver.comment_at(&doc, "the title is wrong", "title", 0, Some(3));
    assert!(matches!(refused, Err(WorldError::InvalidRequest)));

    // A field no operation writes at all is refused for the same reason.
    let refused = driver.comment_at(&doc, "nowhere", "notes", 0, Some(1));
    assert!(matches!(refused, Err(WorldError::InvalidRequest)));

    // Refused means nothing was written: no comment, no anchor.
    assert!(driver.view(&doc).comments.is_empty());

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A span outside the material, or inside material that does not exist yet, is
/// refused at the seam.
#[test]
fn a_span_outside_the_material_is_refused() {
    let root = temp_root("bounds");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();
    let length = DESCRIPTION.chars().count() as u64;

    for (start, end) in [
        (length + 1, Some(length + 2)),
        (0, Some(length + 1)),
        (5, Some(2)),
    ] {
        let refused = driver.comment_at(&doc, "out of range", "description", start, end);
        assert!(
            matches!(refused, Err(WorldError::InvalidRequest)),
            "span {start}..{end:?} of a {length}-character text must be refused"
        );
    }

    // An empty description is material that is not there. A span of it names
    // nothing, and the anchor the algebra returns binds nothing.
    driver.describe(&doc, "");
    let refused = driver.comment_at(&doc, "on nothing", "description", 0, None);
    assert!(matches!(refused, Err(WorldError::InvalidRequest)));

    assert!(driver.view(&doc).comments.is_empty());

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A comment with no span carries no anchor, and the field stays off the wire.
#[test]
fn an_ordinary_comment_carries_no_anchor() {
    let root = temp_root("plain");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Comment {
            doc: doc.clone(),
            body: "just a comment".into(),
            id: Some(lait::ids::mint_comment_id(&SystemUlidSource)),
            parent: None,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();

    let view = driver.view(&doc);
    assert_eq!(view.comments.len(), 1);
    assert!(view.comments[0].anchor.is_none());
    let json = serde_json::to_value(&view.comments[0]).unwrap();
    assert!(
        json.get("anchor").is_none(),
        "an unattached comment must not grow an `anchor` key: {json}"
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A point attachment resolves to a zero-width span rather than to nothing.
#[test]
fn a_point_attachment_resolves_to_a_zero_width_span() {
    let root = temp_root("point");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(&doc, "insert a word here", "description", 4, None)
        .unwrap();
    assert_eq!(
        state_of(&driver.view(&doc), &id),
        CommentAnchorState::At { start: 4, end: 4 }
    );

    driver.describe(&doc, "PRE the quick brown fox");
    assert_eq!(
        state_of(&driver.view(&doc), &id),
        CommentAnchorState::At { start: 8, end: 8 }
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A span that starts at offset 0 moves like any other.
///
/// The head of a span binds to the span's own first character, so the one
/// position the algebra cannot bind — offset 0, which has no character in
/// front of it — is not where a span's head sits.
#[test]
fn a_span_starting_at_zero_moves_with_the_word_it_marks() {
    let root = temp_root("head");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(&doc, "wrong article", "description", 0, Some(3))
        .unwrap();
    driver.describe(&doc, "Xthe quick brown fox");

    let view = driver.view(&doc);
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At { start: 1, end: 4 }
    );
    assert_eq!(marked_text(&view, &id), "the");

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A caret at offset 0 stays at the start of the text instead of being pushed
/// along by an insertion in front of it.
///
/// Pinned because it is a real edge and not a bug in this World, and because it
/// is the only one left: a caret has no material of its own, so its anchor is
/// the character in front of it, and at offset 0 there is none. The algebra
/// hands back an offset from the start, which never drifts and never moves.
#[test]
fn a_caret_at_the_start_of_the_text_stays_at_the_start() {
    let root = temp_root("caret-head");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(&doc, "add a word here", "description", 0, None)
        .unwrap();
    driver.describe(&doc, "PRE the quick brown fox");

    assert_eq!(
        state_of(&driver.view(&doc), &id),
        CommentAnchorState::At { start: 0, end: 0 }
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// **The adjacent-deletion test.** Deleting text in FRONT of the span leaves
/// the span on the word it marks.
///
/// `anchor_in_body` binds a position to whatever wrote the character before
/// it, so a head minted at the span's start would be tied to a character
/// outside the span — and deleting the space in front of a marked word, or the
/// sentence in front of it, would report the word as gone while it is still on
/// screen. Both edits here leave "quick" untouched.
#[test]
fn deleting_the_text_in_front_of_the_span_leaves_it_on_its_word() {
    let root = temp_root("before");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(
            &doc,
            "this word is wrong",
            "description",
            SPAN.0,
            Some(SPAN.1),
        )
        .unwrap();

    // Exactly one character, the space immediately in front of the span.
    driver.describe(&doc, "thequick brown fox");
    let view = driver.view(&doc);
    assert_eq!(view.description, "thequick brown fox");
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At { start: 3, end: 8 }
    );
    assert_eq!(marked_text(&view, &id), "quick");

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// The same rule at the scale a reader meets it: deleting the sentence in front
/// of an anchored word.
#[test]
fn deleting_the_sentence_in_front_of_the_span_leaves_it_on_its_word() {
    let root = temp_root("before-sentence");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();
    driver.describe(&doc, "AAA. BBB. quick brown fox");

    let id = driver
        .comment_at(&doc, "this word is wrong", "description", 10, Some(15))
        .unwrap();
    assert_eq!(marked_text(&driver.view(&doc), &id), "quick");

    driver.describe(&doc, "AAA. quick brown fox");
    let view = driver.view(&doc);
    assert_eq!(view.description, "AAA. quick brown fox");
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At { start: 5, end: 10 }
    );
    assert_eq!(marked_text(&view, &id), "quick");

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// Emptying the field a caret sits in drifts it.
///
/// A caret at offset 0 binds to no operation, so the algebra keeps answering
/// zero for it after the last character is deleted. Zero is a position, and an
/// empty text has none — the same rule the write seam refuses `CommentAt` on,
/// applied to the material as it stands.
#[test]
fn emptying_the_field_drifts_the_caret_it_carried() {
    let root = temp_root("emptied");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();

    let id = driver
        .comment_at(&doc, "add a word here", "description", 0, None)
        .unwrap();
    assert_eq!(
        state_of(&driver.view(&doc), &id),
        CommentAnchorState::At { start: 0, end: 0 }
    );

    driver.describe(&doc, "");
    let view = driver.view(&doc);
    assert_eq!(view.description, "");
    assert_eq!(state_of(&view, &id), CommentAnchorState::Drifted);
    assert_eq!(
        view.comments.len(),
        1,
        "a drifted comment is still a comment"
    );

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// Spans are counted in Unicode scalars, not in UTF-8 bytes.
///
/// The coordinate system is documented on the intent, on the wire request and
/// on the DTO, and every other test in this file uses ASCII — where the two
/// counts agree and a length check written in bytes passes anyway. `héllo
/// wörld` is 11 scalars and 13 bytes, so 12..13 is past the end of the text in
/// the only coordinates that name a place in it.
#[test]
fn spans_are_counted_in_unicode_scalars_and_not_in_bytes() {
    let root = temp_root("unicode");
    let station = offline(&root);
    let mut driver = Driver::dock(&station);
    let doc = driver.seed();
    driver.describe(&doc, "héllo wörld");

    let refused = driver.comment_at(&doc, "past the end", "description", 12, Some(13));
    assert!(
        matches!(refused, Err(WorldError::InvalidRequest)),
        "12..13 is inside the byte length and past the scalar length"
    );

    let id = driver
        .comment_at(&doc, "this word is wrong", "description", 6, Some(11))
        .unwrap();
    let view = driver.view(&doc);
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At { start: 6, end: 11 }
    );
    assert_eq!(marked_text(&view, &id), "wörld");

    // Three scalars in front, five bytes: the span moves by three.
    driver.describe(&doc, "sö héllo wörld");
    let view = driver.view(&doc);
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At { start: 9, end: 14 }
    );
    assert_eq!(marked_text(&view, &id), "wörld");

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// A peer that converged later edits the text the span is on.
///
/// The answer the anchor gives is the whole point of storing an anchor rather
/// than an offset: B's insertion moves A's span, B's deletion drifts it, and
/// neither produces a number that points somewhere nobody commented on. The
/// history the anchor needs travels with the comment — both are operations on
/// the same Body, and the comment causally follows the text it marks — so a
/// replica holding the comment holds what it takes to resolve it.
#[test]
fn a_later_peers_edit_moves_the_span_and_never_invents_a_position() {
    let coords = coordinates();
    let net = comms::mem::MemNet::new();
    let ta: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::crypto::device_from_seed(&STATION_A_SEED)));
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::crypto::device_from_seed(&STATION_B_SEED)));
    let comms_options = |transport: Arc<dyn comms::Transport>, seed: [u8; 32]| CommsOptions {
        transport,
        station_seed: seed,
        authority: Authority {
            source: Arc::new(AnyKnownSigner),
            incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
            export: Arc::new(Vec::new),
            frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![8])),
        },
        gossip: None,
        whole_deadline: Duration::from_secs(20),
        progress_deadline: Duration::from_secs(5),
        route_lease: Duration::from_secs(60),
    };
    let activation = |transport: Arc<dyn comms::Transport>, seed: [u8; 32]| ActivationOptions {
        planes: Default::default(),
        content: Default::default(),
        drain_deadline: Duration::from_secs(5),
        comms: Some(comms_options(transport, seed)),
        observation_capacity: 0,
    };

    let root_a = temp_root("peer-a");
    let root_b = temp_root("peer-b");
    let station_a = product_runtime(&root_a)
        .materialize(&coords)
        .unwrap()
        .open(activation(ta, STATION_A_SEED))
        .unwrap();
    let mut driver_a = Driver::dock(&station_a);
    let doc = driver_a.seed();
    let id = driver_a
        .comment_at(
            &doc,
            "this word is wrong",
            "description",
            SPAN.0,
            Some(SPAN.1),
        )
        .unwrap();

    let station_b = product_runtime(&root_b)
        .materialize(&coords)
        .unwrap()
        .open(activation(tb, STATION_B_SEED))
        .unwrap();
    let a_station =
        mechanics::station::Key::from_device(&mechanics::crypto::device_from_seed(&STATION_A_SEED))
            .unwrap();
    let b_station =
        mechanics::station::Key::from_device(&mechanics::crypto::device_from_seed(&STATION_B_SEED))
            .unwrap();
    assert!(station_b.contact(&a_station).unwrap().convergence.accepted >= 1);

    // B holds A's comment, and resolves A's span against its own replica.
    let mut driver_b = Driver::dock(&station_b);
    driver_b.now = 1_700_500_000;
    assert_eq!(
        state_of(&driver_b.view(&doc), &id),
        CommentAnchorState::At {
            start: SPAN.0,
            end: SPAN.1
        },
        "a peer that converged later resolves the span it received"
    );

    // B inserts in front of the span and A converges: A's span moved.
    driver_b.describe(&doc, "PRE the quick brown fox");
    assert!(station_a.contact(&b_station).unwrap().convergence.accepted >= 1);
    let view = driver_a.view(&doc);
    assert_eq!(view.description, "PRE the quick brown fox");
    assert_eq!(
        state_of(&view, &id),
        CommentAnchorState::At {
            start: SPAN.0 + 4,
            end: SPAN.1 + 4
        },
        "B's edit must move A's span, not leave it pointing at the old offset"
    );

    // B deletes the marked word and A converges: A's span drifts, and A still
    // has the comment.
    driver_b.describe(&doc, "PRE the brown fox");
    assert!(station_a.contact(&b_station).unwrap().convergence.accepted >= 1);
    let view = driver_a.view(&doc);
    assert_eq!(view.description, "PRE the brown fox");
    assert_eq!(state_of(&view, &id), CommentAnchorState::Drifted);
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].body, "this word is wrong");

    let _ = station_a.vacate();
    let _ = station_b.vacate();
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
}
