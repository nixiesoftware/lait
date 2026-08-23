//! `lait:` links, and what arriving on one may and may not do.
//!
//! An invite is a blob a person copies out of one place and pastes into
//! another, and every hop is somewhere it can be truncated, mangled, or lost. An
//! installed client can register a URL scheme, so the invite becomes a link.
//!
//! The installer registers the handler and passes the URL as `%1`; this is the
//! other half.

use crate::client::{ClientError, ClientResult};

/// What a `lait:` link asks for.
///
/// The first segment is a **namespace**, not a World. `client` is already taken
/// by a raise (`serve::shell`'s `RAISE_BASE`) and `join` by an invite, so a World
/// is addressed under `world/` rather than at the root — a World that mounted
/// itself as `join` would otherwise silently shadow invitations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Enter a Space from an invite. The ticket travels as the link's body.
    Invite { ticket: String },
    /// Open one World: `lait://world/<mount>`.
    ///
    /// The mount, because that is already the World's published name — it
    /// prefixes its MCP tools and its route segment, and `WorldClientPackage`'s
    /// own docs call it machine input that must never change. A second slug
    /// declared beside it would be a second name for one thing, and every
    /// rename would become a compatibility event in two places instead of none.
    World { mount: String },
}

/// The scheme the installer registers.
pub const SCHEME: &str = "lait:";

impl Link {
    /// Parse one command-line argument as a link.
    ///
    /// Deliberately strict. This value arrives from the shell, which got it
    /// from a web page, a chat client, or a file — so it is untrusted input
    /// reaching a program that is about to act on it, and the parse is the only
    /// place that is true in one spot.
    pub fn parse(argument: &str) -> ClientResult<Self> {
        let rest = argument
            .strip_prefix(SCHEME)
            .ok_or_else(|| ClientError::invalid(format!("'{argument}' is not a {SCHEME} link")))?;
        // Both `lait:invite/…` and `lait://invite/…` reach here: a shell, a
        // browser and a chat client do not agree about the slashes, and
        // refusing one spelling would make the link work in some places and
        // not others for reasons nobody could see.
        let rest = rest.trim_start_matches('/');
        let (kind, body) = rest
            .split_once('/')
            .ok_or_else(|| ClientError::invalid(format!("'{argument}' names nothing to open")))?;
        match kind {
            // `join` is the canonical spelling and `invite` is accepted beside
            // it. Not a convenience: every place that *renders* an invitation
            // emits `lait://join/…` — the viewer's Members panel, the iOS
            // client, and `runtime::coordinates::parse_link`, which accepts
            // `lait://join/` and the bare form — while this parser accepted only
            // `invite`. So the one link the product produces was refused by the
            // product's own handler, with "'join' is not something this version
            // of Astrolabe knows how to open".
            //
            // Reconciled by widening the parser rather than by changing what is
            // rendered, because the rendered form is the one already pasted into
            // other people's chat histories, and those links have to keep
            // working. `invite` stays because an installer may already have
            // registered it.
            "join" | "invite" => {
                let ticket = body.trim();
                if ticket.is_empty() {
                    return Err(ClientError::invalid("this invite link carries no ticket"));
                }
                Ok(Self::Invite {
                    ticket: ticket.to_owned(),
                })
            }
            "world" => {
                let mount = body.trim().trim_end_matches('/');
                // Validated here, not at the callsite: this is the one place the
                // value is known to be untrusted, and a mount reaches a Host
                // header and a registry lookup. Anything but the grammar a mount
                // is already held to would be a path or a hostname smuggled in.
                let usable = !mount.is_empty()
                    && mount.len() <= 32
                    && mount
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
                if !usable {
                    return Err(ClientError::invalid(format!(
                        "'{mount}' is not a World mount"
                    )));
                }
                Ok(Self::World {
                    mount: mount.to_owned(),
                })
            }
            other => Err(ClientError::invalid(format!(
                "'{other}' is not something this version of Astrolabe knows how to open"
            ))),
        }
    }

    /// The canonical spelling of this link.
    ///
    /// The inverse of [`Link::parse`], so a link recognized at a launch can be
    /// carried onward as one value rather than as the argument it arrived in.
    pub fn to_url(&self) -> String {
        match self {
            Self::Invite { ticket } => format!("{SCHEME}//join/{ticket}"),
            Self::World { mount } => format!("{SCHEME}//world/{mount}"),
        }
    }

    /// The link this launch was asked to open, if any.
    ///
    /// The first argument that parses, rather than the first argument: a
    /// process may be handed all sorts of things, and scanning for the one that
    /// is a link is more robust than insisting it be `argv[1]`.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Option<Self> {
        args.into_iter()
            .skip(1)
            .find_map(|argument| Self::parse(&argument).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every link this build recognizes survives being rendered and reparsed,
    /// which is what lets one arrive as an argument and travel on as a value.
    #[test]
    fn a_link_round_trips_through_its_canonical_spelling() {
        for link in [
            Link::Invite {
                ticket: "abc123".into(),
            },
            Link::World {
                mount: "issues".into(),
            },
        ] {
            assert_eq!(
                Link::parse(&link.to_url()).expect("its own spelling parses"),
                link
            );
        }
    }

    #[test]
    fn an_invite_link_carries_its_ticket_through_either_spelling() {
        let expected = Link::Invite {
            ticket: "abc123".into(),
        };
        assert_eq!(Link::parse("lait:invite/abc123").expect("bare"), expected);
        assert_eq!(
            Link::parse("lait://invite/abc123").expect("slashed"),
            expected,
            "a spelling difference no person controls changed the outcome"
        );
    }

    /// The spelling the product actually renders must be the spelling it opens.
    ///
    /// Regression, and the reason it was invisible: the parser was internally
    /// consistent and every renderer was internally consistent, and they
    /// disagreed. Anything that emits an invitation belongs in this list.
    #[test]
    fn the_link_the_product_renders_is_the_link_it_opens() {
        let expected = Link::Invite {
            ticket: "abc123".into(),
        };
        for rendered in [
            // viewer/src/ui/Members.tsx, tools/astrolabe-ios/src/node.rs, and
            // the form runtime::coordinates::parse_link advertises.
            "lait://join/abc123",
            "lait:join/abc123",
            // Kept working because an installer may already have registered it.
            "lait://invite/abc123",
            "lait:invite/abc123",
        ] {
            assert_eq!(
                Link::parse(rendered).unwrap_or_else(|error| panic!("{rendered}: {error}")),
                expected,
                "{rendered} is a spelling the product emits"
            );
        }
    }

    #[test]
    fn a_world_link_names_a_mount_and_refuses_anything_that_is_not_one() {
        assert_eq!(
            Link::parse("lait://world/issues").expect("a mount"),
            Link::World {
                mount: "issues".into()
            }
        );
        // A trailing slash is a spelling difference nobody controls, like the
        // leading ones.
        assert_eq!(
            Link::parse("lait://world/signage/").expect("trailing slash"),
            Link::World {
                mount: "signage".into()
            }
        );

        // A mount reaches a Host header and a registry lookup, so the grammar is
        // narrow on purpose: no dots, no slashes, no case, nothing that could be
        // a hostname or a path smuggled through.
        for hostile in [
            "lait://world/",
            "lait://world/../etc",
            "lait://world/a/b",
            "lait://world/evil.example.com",
            "lait://world/Issues",
            "lait://world/has_underscore",
            "lait://world/x:8080",
        ] {
            assert!(
                Link::parse(hostile).is_err(),
                "{hostile} must not parse as a World"
            );
        }
    }

    #[test]
    fn something_that_is_not_our_link_is_refused_rather_than_guessed_at() {
        for hostile in [
            "https://example.com/invite/abc",
            "lait:",
            "lait:invite",
            "lait:invite/",
            "lait:something-else/abc",
            "",
        ] {
            assert!(
                Link::parse(hostile).is_err(),
                "'{hostile}' was accepted as a link"
            );
        }
    }

    /// A link is an *input*, not an authority. The parse produces a ticket and
    /// nothing else — no side effect, no acceptance, no membership. Opening the
    /// client is not accepting an invite, and the person still confirms.
    #[test]
    fn parsing_a_link_performs_no_action() {
        // Matched exhaustively rather than bound irrefutably. The `let` this
        // replaced compiled only while the enum had one variant, so adding one
        // broke it — which is the compiler asking the question this test is
        // about: does the new variant carry anything but a name? It does not.
        match Link::parse("lait:invite/abc123").expect("a link") {
            // The whole value is the ticket. There is nowhere for an effect to
            // hide and nothing here that could have granted anything, which is
            // the property this test exists to keep true as the enum grows.
            Link::Invite { ticket } => assert_eq!(ticket, "abc123"),
            other => panic!("an invite link parsed as {other:?}"),
        }
        match Link::parse("lait://world/issues").expect("a link") {
            // Likewise a mount and nothing else: opening a World is a
            // navigation, and this parse neither starts a head nor reaches one.
            Link::World { mount } => assert_eq!(mount, "issues"),
            other => panic!("a World link parsed as {other:?}"),
        }
    }

    #[test]
    fn the_link_is_found_wherever_the_shell_put_it() {
        let args = vec![
            "astrolabe.exe".to_owned(),
            "--some-flag".to_owned(),
            "lait:invite/abc123".to_owned(),
        ];
        assert_eq!(
            Link::from_args(args),
            Some(Link::Invite {
                ticket: "abc123".into()
            })
        );
        // The executable's own path is never a link, however it is spelled.
        assert_eq!(
            Link::from_args(vec!["lait:invite/nope".to_owned()]),
            None,
            "argv[0] was parsed as a link"
        );
        assert_eq!(Link::from_args(vec!["astrolabe.exe".to_owned()]), None);
    }
}
