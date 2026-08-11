//! S1a golden fixtures freezing the collaborative operation algebra's canonical
//! postcard encoding. Positive vectors pin each variant's tag and layout;
//! negative vectors prove out-of-range tags and trailing bytes are rejected.

use replica::body::Op;

/// postcard encodes an enum as `varint(variant_index) || fields...`.
#[test]
fn body_op_variant_tags_are_frozen() {
    // Nullary variants encode as a single tag byte.
    assert_eq!(postcard::to_stdvec(&Op::Create).unwrap(), vec![12]);
    assert_eq!(postcard::to_stdvec(&Op::Tombstone).unwrap(), vec![13]);

    // ReplaceAtomic is tag 0, then a length-prefixed byte vector.
    assert_eq!(
        postcard::to_stdvec(&Op::ReplaceAtomic { value: vec![0xAB] }).unwrap(),
        vec![0, 1, 0xAB]
    );

    // CounterAdd is tag 11, then a zigzag varint delta (-1 -> 0x01).
    assert_eq!(
        postcard::to_stdvec(&Op::CounterAdd {
            path: String::new(),
            delta: -1
        })
        .unwrap(),
        vec![11, 0, 0x01]
    );

    // The tree operations were appended after the frozen twelve, so every tag
    // above holds its original value. TreeInsert is tag 14: path, then two
    // Options (0 = None), then a length-prefixed value.
    assert_eq!(
        postcard::to_stdvec(&Op::TreeInsert {
            path: String::new(),
            parent: None,
            after: None,
            value: vec![0xAB],
        })
        .unwrap(),
        vec![14, 0, 0, 0, 1, 0xAB]
    );
    assert_eq!(
        postcard::to_stdvec(&Op::TreeUnset {
            path: String::new(),
            node: String::new(),
            key: String::new(),
        })
        .unwrap(),
        vec![18, 0, 0, 0]
    );
    // TreeAnchor is tag 19: path, anchor, then an Option (0 = None).
    assert_eq!(
        postcard::to_stdvec(&Op::TreeAnchor {
            path: String::new(),
            anchor: String::new(),
            parent: None,
        })
        .unwrap(),
        vec![19, 0, 0, 0]
    );
    // LogAppend is tag 20: path, length-prefixed value, then a varint retain
    // (512 -> 0x80 0x04).
    assert_eq!(
        postcard::to_stdvec(&Op::LogAppend {
            path: String::new(),
            value: vec![0xAB],
            retain: 512,
        })
        .unwrap(),
        vec![20, 0, 1, 0xAB, 0x80, 0x04]
    );
}

#[test]
fn every_variant_roundtrips() {
    let ops = vec![
        Op::ReplaceAtomic { value: vec![1, 2] },
        Op::RegisterSet {
            path: "title".into(),
            value: vec![9],
        },
        Op::RegisterClear {
            path: "title".into(),
        },
        Op::MapSet {
            path: "labels".into(),
            key: "bug".into(),
            value: vec![],
        },
        Op::MapRemove {
            path: "labels".into(),
            key: "bug".into(),
        },
        Op::ListInsert {
            path: "items".into(),
            index: 0,
            value: vec![7],
        },
        Op::ListRemove {
            path: "items".into(),
            element: "e_1".into(),
        },
        Op::ListMove {
            path: "items".into(),
            element: "e_1".into(),
            index: 3,
        },
        Op::TextSplice {
            path: "body".into(),
            index: 2,
            delete: 1,
            insert: "x".into(),
        },
        Op::SetAdd {
            path: "tags".into(),
            value: vec![1],
        },
        Op::SetRemove {
            path: "tags".into(),
            value: vec![1],
        },
        Op::CounterAdd {
            path: "votes".into(),
            delta: 5,
        },
        Op::Create,
        Op::Tombstone,
        Op::TreeInsert {
            path: "comments".into(),
            parent: Some("3@7".into()),
            after: None,
            value: vec![7],
        },
        Op::TreeMove {
            path: "comments".into(),
            node: "5@7".into(),
            parent: None,
            after: Some("3@7".into()),
        },
        Op::TreeRemove {
            path: "comments".into(),
            node: "5@7".into(),
        },
        Op::TreeSet {
            path: "comments".into(),
            node: "5@7".into(),
            key: "pinned".into(),
            value: vec![1],
        },
        Op::TreeUnset {
            path: "comments".into(),
            node: "5@7".into(),
            key: "pinned".into(),
        },
        Op::TreeAnchor {
            path: "hierarchy".into(),
            anchor: "iss_child".into(),
            parent: Some("iss_parent".into()),
        },
        Op::LogAppend {
            path: "events".into(),
            value: vec![3],
            retain: 512,
        },
    ];
    for op in ops {
        let bytes = postcard::to_stdvec(&op).unwrap();
        let back: Op = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(op, back, "roundtrip for {op:?}");
    }
}

#[test]
fn unknown_variant_tag_is_rejected() {
    // Tag 99 is out of range for the frozen 21-variant algebra.
    let bytes = vec![99u8];
    assert!(postcard::from_bytes::<Op>(&bytes).is_err());
}

#[test]
fn trailing_bytes_break_canonical_reencoding() {
    // `postcard::from_bytes` tolerates trailing bytes, so canonicality is proven
    // the way the envelope decoders do it: decode, then require re-encode to be
    // byte-exact. A trailing byte fails that check.
    let mut bytes = postcard::to_stdvec(&Op::Create).unwrap();
    bytes.push(0xFF);
    let decoded: Op = postcard::from_bytes(&bytes).unwrap();
    assert_ne!(
        postcard::to_stdvec(&decoded).unwrap(),
        bytes,
        "re-encode is shorter than the trailing-padded input"
    );
}
