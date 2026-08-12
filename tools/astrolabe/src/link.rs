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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Enter a Space from an invite. The ticket travels as the link's body.
    Invite { ticket: String },
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
            "invite" => {
                let ticket = body.trim();
                if ticket.is_empty() {
                    return Err(ClientError::invalid("this invite link carries no ticket"));
                }
                Ok(Self::Invite {
                    ticket: ticket.to_owned(),
                })
            }
            other => Err(ClientError::invalid(format!(
                "'{other}' is not something this version of Astrolabe knows how to open"
            ))),
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
        let Link::Invite { ticket } = Link::parse("lait:invite/abc123").expect("a link");
        // The whole value is the ticket. There is nowhere for an effect to hide
        // and nothing here that could have granted anything, which is the
        // property this test exists to keep true as the enum grows.
        assert_eq!(ticket, "abc123");
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
