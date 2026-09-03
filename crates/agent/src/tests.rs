use std::fs;

use mechanics::actor::device_from_seed;
use mechanics::kinship::ProfileId;

use super::*;
use crate::store::test_support;

const AGENT_SEED: [u8; 32] = [11; 32];
const OWNER_SEED: [u8; 32] = [22; 32];

fn identities() -> (ProfileId, ProfileId) {
    (
        ProfileId::from_genesis(b"agent-test-profile"),
        ProfileId::from_genesis(b"owner-test-profile"),
    )
}

fn bond() -> OwnershipBond {
    let (agent, owner) = identities();
    let terms = OwnershipTerms::new(agent, owner, 1_700_000_000, [7; 16]);
    let agent_half = terms.sign(OwnershipRole::Agent, &AGENT_SEED).unwrap();
    let owner_half = terms.sign(OwnershipRole::Owner, &OWNER_SEED).unwrap();
    OwnershipBond::assemble(
        terms,
        agent_half,
        owner_half,
        &[device_from_seed(&AGENT_SEED)],
        &[device_from_seed(&OWNER_SEED)],
    )
    .unwrap()
}

fn item(id: &str, visibility: VisibilityOverride) -> InventoryItem {
    InventoryItem {
        id: InventoryItemId::parse(id).unwrap(),
        kind: PrimitiveKind::parse("lait.memory").unwrap(),
        label: format!("Memory {id}"),
        summary: "Durable local memory".into(),
        visibility,
        standing: PrimitiveStanding::Ready,
        public_fields: vec![InventoryField {
            key: FieldKey::parse("class").unwrap(),
            value: FieldValue::Choice("local".into()),
        }],
        owner_fields: vec![InventoryField {
            key: FieldKey::parse("limit_bytes").unwrap(),
            value: FieldValue::ByteSize(4096),
        }],
        secrets: vec![SecretBinding {
            label: "Encryption key".into(),
            reference: SecretRef::parse("secret_memory_primary").unwrap(),
            standing: SecretStanding::Connected,
        }],
    }
}

fn state() -> AgentState {
    let bond = bond();
    let record = AgentRecord::new(
        bond,
        "Adam".into(),
        "Virtual assistant for Security DVR Inc.".into(),
    )
    .unwrap();
    let owner = record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let mut inventory =
        InventoryManifest::empty(&record.ownership, Visibility::Public, &author).unwrap();
    inventory
        .apply(
            &record.ownership,
            &author,
            0,
            InventoryMutation::Add(item("memory", VisibilityOverride::Inherit)),
        )
        .unwrap();
    AgentState::new(record, inventory).unwrap()
}

#[test]
fn ownership_requires_both_signatures_and_resolved_profile_membership() {
    let held = bond();
    assert!(held
        .verify(
            &[device_from_seed(&AGENT_SEED)],
            &[device_from_seed(&OWNER_SEED)]
        )
        .is_ok());
    assert!(matches!(
        held.verify(
            &[device_from_seed(&OWNER_SEED)],
            &[device_from_seed(&OWNER_SEED)]
        ),
        Err(Error::UnrootedSigner("agent"))
    ));

    let (agent, owner) = identities();
    let terms = OwnershipTerms::new(agent, owner, 3, [1; 16]);
    let wrong_agent = terms.sign(OwnershipRole::Agent, &OWNER_SEED).unwrap();
    let owner_half = terms.sign(OwnershipRole::Owner, &OWNER_SEED).unwrap();
    assert!(matches!(
        OwnershipBond::assemble(
            terms,
            wrong_agent,
            owner_half,
            &[device_from_seed(&AGENT_SEED)],
            &[device_from_seed(&OWNER_SEED)]
        ),
        Err(Error::UnrootedSigner("agent"))
    ));
}

#[test]
fn both_signatures_bind_every_ownership_term() {
    let mut changed = bond();
    changed.terms.created_at += 1;
    assert!(matches!(
        changed.verify_signatures(),
        Err(Error::BadSignature("agent"))
    ));
}

#[test]
fn only_owner_may_mutate_and_revision_conflicts_do_not_partially_apply() {
    let mut held = state();
    let original = held.clone();
    let agent = held.record.ownership.agent().clone();
    let agent_devices = [device_from_seed(&AGENT_SEED)];
    let bad_author = OwnerAuthor {
        profile: &agent,
        seed: &AGENT_SEED,
        resolved_devices: &agent_devices,
    };
    assert!(matches!(
        held.apply(
            &bad_author,
            held.head(),
            StateMutation::Record(RecordMutation::SetLifecycle(AgentLifecycle::Suspended))
        ),
        Err(Error::Unauthorized)
    ));
    assert_eq!(held, original);

    let owner = held.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let stale = StateRevision {
        record: 9,
        inventory: held.inventory.revision,
    };
    assert!(matches!(
        held.apply(
            &author,
            stale,
            StateMutation::Record(RecordMutation::SetLifecycle(AgentLifecycle::Suspended))
        ),
        Err(Error::Conflict { .. })
    ));
    assert_eq!(held, original);
}

#[test]
fn item_visibility_can_only_make_the_collection_more_restrictive() {
    let mut held = state();
    let owner = held.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let expected = held.head();
    held.apply(
        &author,
        expected,
        StateMutation::Inventory(InventoryMutation::Add(item(
            "private_memory",
            VisibilityOverride::Private,
        ))),
    )
    .unwrap();

    let public = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Public)
        .unwrap();
    let InventoryProjection::Public(public) = public else {
        panic!("public collection should be visible");
    };
    assert_eq!(public.items.len(), 1);
    assert_eq!(public.items[0].id.as_str(), "memory");

    let contact = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Contact)
        .unwrap();
    let InventoryProjection::Public(contact) = contact else {
        panic!("contact should see the collection");
    };
    assert_eq!(contact.items.len(), 1, "private override stays private");
}

#[test]
fn private_inventory_discloses_no_counts_identifiers_or_revision() {
    let mut held = state();
    let owner = held.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let revision = held.inventory.revision;
    held.inventory
        .apply(
            &held.record.ownership,
            &author,
            revision,
            InventoryMutation::SetDefaultVisibility(Visibility::Private),
        )
        .unwrap();
    assert_eq!(
        held.inventory
            .project(&held.record.ownership, InventoryReader::Public)
            .unwrap(),
        InventoryProjection::Hidden
    );
    assert_eq!(
        held.inventory
            .project(&held.record.ownership, InventoryReader::Contact)
            .unwrap(),
        InventoryProjection::Hidden
    );
}

#[test]
fn public_owner_and_secret_projections_do_not_cross_class_boundaries() {
    let held = state();
    let public = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Public)
        .unwrap();
    let InventoryProjection::Public(public) = public else {
        panic!("public projection");
    };
    assert_eq!(public.items[0].fields[0].key.as_str(), "class");

    let owner = held.record.ownership.owner().clone();
    let owner_view = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Owner(&owner))
        .unwrap();
    let InventoryProjection::Owner(owner_view) = owner_view else {
        panic!("owner projection");
    };
    assert_eq!(owner_view.items[0].fields[0].key.as_str(), "limit_bytes");
    assert_eq!(owner_view.items[0].secrets[0].label, "Encryption key");
    let owner_debug = format!("{owner_view:?}");
    assert!(!owner_debug.contains("secret_memory_primary"));

    let agent = held.record.ownership.agent().clone();
    let secret_view = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Secret(&agent))
        .unwrap();
    let InventoryProjection::Secret(secret_view) = secret_view else {
        panic!("secret projection");
    };
    assert_eq!(
        secret_view.items[0].secrets[0].reference.as_str(),
        "secret_memory_primary"
    );
    assert!(matches!(
        held.inventory
            .project(&held.record.ownership, InventoryReader::Secret(&owner)),
        Err(Error::Unauthorized)
    ));
}

#[test]
fn structural_and_text_bounds_are_enforced_before_commit() {
    let mut held = state();
    held.record.name = "x".repeat(MAX_NAME_BYTES + 1);
    assert!(matches!(held.validate(), Err(Error::Bound("agent name"))));

    let mut too_many = state();
    too_many.inventory.items = (0..=MAX_ITEMS)
        .map(|index| item(&format!("memory_{index}"), VisibilityOverride::Inherit))
        .collect();
    assert!(matches!(
        too_many.validate(),
        Err(Error::Bound("inventory items"))
    ));
}

#[test]
fn the_private_store_round_trips_and_enforces_expected_heads() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::at(dir.path());
    assert!(store.load().unwrap().is_none());
    let initial = state();
    store
        .create(
            &initial,
            &[device_from_seed(&AGENT_SEED)],
            &[device_from_seed(&OWNER_SEED)],
        )
        .unwrap();
    assert_eq!(store.load().unwrap(), Some(initial.clone()));

    let owner = initial.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let changed = store
        .mutate(
            &author,
            initial.head(),
            StateMutation::Record(RecordMutation::SetLifecycle(AgentLifecycle::Suspended)),
        )
        .unwrap();
    assert_eq!(changed.record.lifecycle, AgentLifecycle::Suspended);
    assert!(matches!(
        store.mutate(
            &author,
            initial.head(),
            StateMutation::Record(RecordMutation::SetLifecycle(AgentLifecycle::Active))
        ),
        Err(Error::Conflict { .. })
    ));
}

#[test]
fn interrupted_atomic_write_recovers_only_a_valid_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::at(dir.path());
    let expected = state();
    fs::create_dir_all(dir.path().join("agent")).unwrap();
    fs::write(
        dir.path().join("agent/state.tmp"),
        test_support::encode(&expected).unwrap(),
    )
    .unwrap();
    assert_eq!(store.load().unwrap(), Some(expected));
    assert!(dir.path().join("agent/state.bin").exists());
}

#[test]
fn envelope_corruption_length_and_versions_fail_closed() {
    let valid = test_support::encode(&state()).unwrap();

    let mut corrupt = valid.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert!(matches!(
        test_support::decode(&corrupt),
        Err(Error::Corrupt("envelope digest"))
    ));

    let mut future_envelope = valid.clone();
    future_envelope[8] = test_support::ENVELOPE_VERSION + 1;
    assert!(matches!(
        test_support::decode(&future_envelope),
        Err(Error::UnsupportedVersion {
            artifact: "agent state envelope",
            ..
        })
    ));

    let mut wrong_length = valid;
    wrong_length[9..13].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        test_support::decode(&wrong_length),
        Err(Error::Corrupt("envelope length disagrees with file"))
    ));
}

#[test]
fn nested_record_and_inventory_versions_are_rejected_even_with_a_valid_digest() {
    let mut future_record = state();
    future_record.record.version += 1;
    assert!(matches!(
        future_record.validate(),
        Err(Error::UnsupportedVersion {
            artifact: "agent record",
            ..
        })
    ));

    let mut future_inventory = state();
    future_inventory.inventory.version += 1;
    assert!(matches!(
        future_inventory.validate(),
        Err(Error::UnsupportedVersion {
            artifact: "inventory manifest",
            ..
        })
    ));
}

#[test]
fn oversized_store_file_is_a_bound_failure_not_absence() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::at(dir.path());
    fs::create_dir_all(dir.path().join("agent")).unwrap();
    fs::write(
        dir.path().join("agent/state.bin"),
        vec![0u8; crate::store::MAX_STATE_BYTES + 1],
    )
    .unwrap();
    assert!(matches!(
        store.load(),
        Err(Error::Bound("agent state envelope"))
    ));
}

#[test]
fn every_inventory_revision_and_publication_is_owner_signed() {
    let mut held = state();
    let owner = held.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    assert!(held
        .inventory
        .verify(&held.record.ownership, &owner_devices)
        .is_ok());
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let revision = held.inventory.revision;
    held.inventory
        .apply(
            &held.record.ownership,
            &author,
            revision,
            InventoryMutation::SetDefaultVisibility(Visibility::Contacts),
        )
        .unwrap();
    assert!(held
        .inventory
        .verify(&held.record.ownership, &owner_devices)
        .is_ok());
    let publication = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Contact)
        .unwrap();
    let InventoryProjection::Public(publication) = publication else {
        panic!("contacts publication");
    };
    assert_eq!(publication.revision, held.inventory.revision);
    assert_eq!(publication.audience, PublicationAudience::Contacts);
    assert!(publication
        .verify(&held.record.ownership, &owner_devices)
        .is_ok());

    held.inventory.items[0].summary.push_str(" tampered");
    assert!(matches!(
        held.inventory.validate(),
        Err(Error::BadSignature("owner"))
    ));
}

#[test]
fn unresolved_owner_device_cannot_author_a_manifest_revision() {
    let mut held = state();
    let owner = held.record.ownership.owner().clone();
    let unrelated_devices = [device_from_seed(&AGENT_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &unrelated_devices,
    };
    let before = held.inventory.clone();
    assert!(matches!(
        held.inventory.apply(
            &held.record.ownership,
            &author,
            before.revision,
            InventoryMutation::SetDefaultVisibility(Visibility::Private)
        ),
        Err(Error::UnrootedSigner("owner"))
    ));
    assert_eq!(held.inventory, before);
}

#[test]
fn duplicate_and_cross_class_field_keys_are_rejected() {
    let mut duplicate = item("duplicate", VisibilityOverride::Inherit);
    duplicate
        .public_fields
        .push(duplicate.public_fields[0].clone());
    assert!(matches!(
        duplicate.validate(),
        Err(Error::Invalid("duplicate inventory field key"))
    ));

    let mut overlap = item("overlap", VisibilityOverride::Inherit);
    overlap.owner_fields[0].key = overlap.public_fields[0].key.clone();
    assert!(matches!(
        overlap.validate(),
        Err(Error::Invalid("public and owner field keys overlap"))
    ));
}

#[test]
fn opaque_future_items_survive_unrelated_mutations_byte_for_byte() {
    let mut held = state();
    let owner = held.record.ownership.owner().clone();
    let owner_devices = [device_from_seed(&OWNER_SEED)];
    let author = OwnerAuthor {
        profile: &owner,
        seed: &OWNER_SEED,
        resolved_devices: &owner_devices,
    };
    let opaque = OpaqueInventoryItem {
        id: InventoryItemId::parse("future_brain").unwrap(),
        kind: PrimitiveKind::parse("future.brain").unwrap(),
        label: "Future brain".into(),
        summary: "A newer primitive revision".into(),
        visibility: VisibilityOverride::Contacts,
        body_version: 9,
        body: vec![0, 1, 2, 254, 255],
    };
    let first = held.inventory.revision;
    held.inventory
        .apply(
            &held.record.ownership,
            &author,
            first,
            InventoryMutation::AddOpaque(opaque.clone()),
        )
        .unwrap();
    let second = held.inventory.revision;
    held.inventory
        .apply(
            &held.record.ownership,
            &author,
            second,
            InventoryMutation::Add(item("scratch", VisibilityOverride::Inherit)),
        )
        .unwrap();
    assert_eq!(held.inventory.opaque_items, vec![opaque]);

    let contact = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Contact)
        .unwrap();
    let InventoryProjection::Public(contact) = contact else {
        panic!("contact projection");
    };
    let future = contact
        .items
        .iter()
        .find(|entry| entry.id.as_str() == "future_brain")
        .unwrap();
    assert_eq!(future.standing, None, "opaque entries render generically");

    let owner_view = held
        .inventory
        .project(&held.record.ownership, InventoryReader::Owner(&owner))
        .unwrap();
    let InventoryProjection::Owner(owner_view) = owner_view else {
        panic!("owner projection");
    };
    assert!(
        !owner_view
            .items
            .iter()
            .find(|entry| entry.public.id.as_str() == "future_brain")
            .unwrap()
            .editable
    );
}
