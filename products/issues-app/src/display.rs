#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "bounded raster coordinates are checked before image access"
)]

//! `issues.board.wall` — a project board on a screen.
//!
//! # Why this surface exists
//!
//! The display design says adding a World surface must require no television
//! application update and must give the receiver no product vocabulary. Signage
//! alone cannot demonstrate that: it is the product the coordinator was built
//! for, so it proves the path works and not that the path is *general*. A
//! second, unlike product going out over the same contract is the evidence.
//!
//! It is also the surface that needs no authoring. A Signage program has to be
//! written before a screen can show it; a board already exists the moment a
//! project does, which makes this the cheapest way to put a real World on a
//! real screen.
//!
//! # What the receiver learns
//!
//! Pixels, a duration and an assessment. No issue ids, no project key, no
//! workflow vocabulary — the column names are drawn *into* the frame, not sent
//! as fields. A receiver holding this program cannot name an issue, and that is
//! the property the surface contract exists to keep.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;

use font8x8::{UnicodeFonts, BASIC_FONTS};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use world_interface::display::{
    CanonicalDisplayInput, DisplayAssessment, DisplayOutputKind, DisplayPartialReason,
    DisplayProjection, DisplayRenderer, DisplayRequest, DisplaySurface, DisplaySurfaceDescriptor,
    DisplaySurfaceId, DisplayTheme, FrameMediaType, ProgramCycle, RenderedFrame, RenderedProgram,
    RenderedProgramItem, RenderedScene,
};
use world_interface::{ClientAccess, ClientInvocation, Failure};

const SURFACE_ID: &str = "issues.board.wall";
const MAX_RENDER_WIDTH: u32 = 4_096;
const MAX_RENDER_HEIGHT: u32 = 2_160;

/// How long the board holds the screen before the coordinator re-asks.
///
/// A board has no schedule of its own — nothing in it says when it next
/// changes — so this is a polling interval rather than a boundary. Sixty
/// seconds is slow enough to cost nothing and fast enough that a wall does not
/// visibly lag the room it is in.
const REFRESH_MS: u32 = 60_000;

/// Columns drawn per frame. A board with more states than this is split across
/// items rather than squeezed, because six columns of unreadable four-pixel
/// text is a worse answer than two frames of legible ones.
const COLUMNS_PER_FRAME: usize = 3;

/// Rows drawn per column. The count in the header stays truthful when rows are
/// cut, so a wall never implies a column is shorter than it is.
const ROWS_PER_COLUMN: usize = 8;

#[derive(Debug, Serialize, Deserialize)]
struct BoardInput {
    project: String,
}

pub fn board_wall_surface() -> Result<DisplaySurface, Failure> {
    let world = issues::contract::world_id();
    let mut input_digest = Sha256::new();
    input_digest.update(b"issues.board.wall.input.v1:{project:key-or-id}");
    let mut renderer_identity = Sha256::new();
    renderer_identity.update(b"issues.board.wall.renderer.v1:font8x8:png:columns");
    let mut descriptor = DisplaySurfaceDescriptor {
        id: DisplaySurfaceId::new(SURFACE_ID)?,
        title: "Issues board".into(),
        runtime_implementation: crate::lifecycle::implementation_id(),
        contract_version: 1,
        input_contract_digest: input_digest.finalize().into(),
        renderer_identity: renderer_identity.finalize().into(),
        contract_digest: [0; 32],
        // Frames only. A board has nothing to stream, and declaring Media would
        // be advertising a capability the renderer never produces.
        outputs: BTreeSet::from([DisplayOutputKind::Frame]),
    };
    descriptor.contract_digest = descriptor.expected_contract_digest(&world);
    Ok(DisplaySurface {
        descriptor,
        canonicalize_input,
        prepare,
        renderer: Arc::new(BoardRenderer),
    })
}

/// The package's own reading of its input, once.
///
/// A project selector is accepted as typed — a key like `ENG` or an id — and
/// resolved by the World, which is the only thing that knows which projects
/// exist. Validating the *shape* here and the *existence* there is the split
/// that keeps the coordinator from inventing product semantics.
fn canonicalize_input(value: Value) -> Result<CanonicalDisplayInput, Failure> {
    let input: BoardInput = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("invalid Issues board input: {error}")))?;
    let project = input.project.trim();
    if project.is_empty() || project.chars().count() > 64 {
        return Err(Failure::new(
            "an Issues board names a project key or id of 1..=64 characters",
        ));
    }
    let bytes = serde_json::to_vec(&BoardInput {
        project: project.to_owned(),
    })
    .map_err(|error| Failure::new(format!("encode Issues board input: {error}")))?;
    CanonicalDisplayInput::new(bytes)
}

fn prepare(request: &DisplayRequest) -> Result<ClientInvocation, Failure> {
    request.validate()?;
    if request.surface.as_str() != SURFACE_ID {
        return Err(Failure::new(
            "Issues board renderer received another surface",
        ));
    }
    let input: BoardInput = serde_json::from_slice(request.input.as_bytes())
        .map_err(|error| Failure::new(format!("decode Issues board input: {error}")))?;
    let call = crate::encode_call(&crate::IssuesRequest::Board {
        project: Some(input.project),
        project_hint: None,
        // A wall draws what fits on it; the page bound is the wall's, not
        // the board's.
        page: issues::contract::PageRequest::default(),
    })
    .map_err(|error| Failure::new(error.to_string()))?;
    // Query, and stated here as well as classified by the runtime. A board is a
    // read; a surface that could ask for anything else would be the hole the
    // required-Query boundary exists to close.
    Ok(ClientInvocation::world(call, ClientAccess::Query, None))
}

struct BoardRenderer;

impl DisplayRenderer for BoardRenderer {
    fn project<'a>(
        &'a self,
        value: Value,
        request: &'a DisplayRequest,
    ) -> world_interface::display::DisplayProjectFuture<'a> {
        // The trait is async because some renderer somewhere will need to be.
        // Nothing here awaits, so the work is a plain function and the future
        // is a wrapper — which is also what makes the properties below
        // testable without standing up a runtime to assert them.
        Box::pin(async move { render(value, request) })
    }
}

fn render(value: Value, request: &DisplayRequest) -> Result<DisplayProjection, Failure> {
    if request.width > MAX_RENDER_WIDTH || request.height > MAX_RENDER_HEIGHT {
        return Err(Failure::new(
            "Issues board render dimensions exceed the frame bound",
        ));
    }
    let response: crate::IssuesResponse = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("decode Issues projection: {error}")))?;
    let crate::IssuesResponse::Board(board) = response else {
        return Err(Failure::new("Issues did not answer with a board"));
    };

    // The board arrives as one ordered page of rows plus the workflow it is
    // read against, rather than pre-grouped columns: grouping is presentation
    // and the wire does not pay for it. This surface draws columns, so it
    // groups here, in the workflow's own order.
    let columns: Vec<issues::dto::BoardColumn> = board
        .workflow
        .iter()
        .map(|state| issues::dto::BoardColumn {
            state: state.clone(),
            rows: board
                .rows
                .items
                .iter()
                .filter(|row| row.status == state.id)
                .cloned()
                .collect(),
        })
        .collect();

    let palette = Palette::for_theme(request.theme);
    let mut reasons = BTreeSet::new();
    // A provisional row is a row whose catalog entry is not yet settled.
    // It is drawn, and it makes the whole projection Partial — a wall
    // that showed provisional work as settled would be the more
    // confident and less true answer.
    if columns
        .iter()
        .any(|column| column.rows.iter().any(|row| row.provisional))
    {
        reasons.insert(DisplayPartialReason::ProvisionalData);
    }

    let chunks: Vec<&[issues::dto::BoardColumn]> = if columns.is_empty() {
        Vec::new()
    } else {
        columns.chunks(COLUMNS_PER_FRAME).collect()
    };

    let items = if chunks.is_empty() {
        // A project with no workflow states is not a failure and not an
        // unavailable source: it is a board with nothing on it, and it
        // says so rather than blanking.
        vec![RenderedProgramItem {
            id: "empty".into(),
            duration_ms: Some(REFRESH_MS),
            scene: RenderedScene::Frame(frame(
                request,
                &palette,
                &board.project.name,
                &[],
                "This project has no columns",
            )?),
            assessment: DisplayAssessment::Current,
            spoken_summary: Some(format!("{} has no columns", board.project.name)),
        }]
    } else {
        let mut items = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            items.push(RenderedProgramItem {
                id: format!("columns-{index}"),
                duration_ms: Some(REFRESH_MS / u32::try_from(chunks.len()).unwrap_or(1)),
                scene: RenderedScene::Frame(frame(
                    request,
                    &palette,
                    &board.project.name,
                    chunk,
                    "",
                )?),
                assessment: DisplayAssessment::Current,
                spoken_summary: Some(spoken(&board.project.name, chunk)),
            });
        }
        items
    };

    Ok(DisplayProjection {
        program: RenderedProgram {
            items,
            // Loop, because a wall is never finished. `HoldLast` would
            // freeze a multi-frame board on whichever columns happened
            // to be last.
            cycle: ProgramCycle::Loop,
            refresh_after_ms: Some(REFRESH_MS.min(request.window_horizon_ms.max(1))),
        },
        assessment: if reasons.is_empty() {
            DisplayAssessment::Current
        } else {
            DisplayAssessment::Partial(reasons)
        },
        spoken_summary: Some(format!("{} board", board.project.name)),
    })
}

/// Colours, taken from the assignment's declared theme.
///
/// A wall in a lit room and a wall in a dark one are different requests, and the
/// theme is the operator saying which. Nothing here reads a system preference —
/// the screen has no person at it to have one.
struct Palette {
    ground: Rgba<u8>,
    ink: Rgba<u8>,
    quiet: Rgba<u8>,
    rule: Rgba<u8>,
}

impl Palette {
    fn for_theme(theme: DisplayTheme) -> Self {
        match theme {
            DisplayTheme::Light => Self {
                ground: Rgba([250, 250, 250, 255]),
                ink: Rgba([17, 17, 17, 255]),
                quiet: Rgba([110, 110, 110, 255]),
                rule: Rgba([210, 210, 210, 255]),
            },
            // High contrast is not dark with more saturation: it is the same
            // layout with every mid-tone removed, because the failure it
            // addresses is discrimination between near values.
            DisplayTheme::HighContrast => Self {
                ground: Rgba([0, 0, 0, 255]),
                ink: Rgba([255, 255, 255, 255]),
                quiet: Rgba([255, 255, 255, 255]),
                rule: Rgba([255, 255, 255, 255]),
            },
            DisplayTheme::Dark => Self {
                ground: Rgba([14, 14, 16, 255]),
                ink: Rgba([238, 238, 240, 255]),
                quiet: Rgba([150, 150, 158, 255]),
                rule: Rgba([48, 48, 54, 255]),
            },
        }
    }
}

fn spoken(project: &str, columns: &[issues::dto::BoardColumn]) -> String {
    let parts: Vec<String> = columns
        .iter()
        .map(|column| format!("{} {}", column.rows.len(), column.state.name))
        .collect();
    format!("{project}: {}", parts.join(", "))
}

fn frame(
    request: &DisplayRequest,
    palette: &Palette,
    project: &str,
    columns: &[issues::dto::BoardColumn],
    said: &str,
) -> Result<RenderedFrame, Failure> {
    let width = request.width.max(1);
    let height = request.height.max(1);
    let mut image = RgbaImage::from_pixel(width, height, palette.ground);

    // Scale with the glass. A wall is read from across a room, so the type is
    // sized from the frame rather than pinned to pixels.
    let scale = (width / 640).clamp(1, 6);
    let inset = scale.saturating_mul(16);
    let cell = scale.saturating_mul(9).max(1);

    draw_text(&mut image, project, inset, inset, scale + 1, palette.ink);
    let rule_y = inset.saturating_add((scale + 1).saturating_mul(12));
    draw_rule(
        &mut image,
        inset,
        rule_y,
        width.saturating_sub(inset * 2),
        palette.rule,
    );

    if !said.is_empty() {
        draw_text(
            &mut image,
            said,
            inset,
            rule_y.saturating_add(cell * 2),
            scale,
            palette.quiet,
        );
    }

    let top = rule_y.saturating_add(cell * 2);
    let usable = width.saturating_sub(inset.saturating_mul(2));
    let column_width = if columns.is_empty() {
        usable
    } else {
        usable / u32::try_from(columns.len()).unwrap_or(1).max(1)
    };

    for (index, column) in columns.iter().enumerate() {
        let x = inset.saturating_add(
            u32::try_from(index)
                .unwrap_or(0)
                .saturating_mul(column_width),
        );
        // The count is the column's real length, always. Rows below are capped
        // for legibility; the header is what keeps that from becoming a lie.
        let heading = format!(
            "{}  {}",
            column.state.name.to_uppercase(),
            column.rows.len()
        );
        draw_text(&mut image, &heading, x, top, scale, palette.quiet);

        let mut y = top.saturating_add(cell.saturating_mul(2));
        for row in column.rows.iter().take(ROWS_PER_COLUMN) {
            let label = match &row.key_alias {
                Some(alias) => format!("{alias}  {}", row.title),
                None => format!("{}  {}", row.reff, row.title),
            };
            let ink = if row.provisional {
                palette.quiet
            } else {
                palette.ink
            };
            draw_clipped(
                &mut image,
                &label,
                x,
                y,
                column_width.saturating_sub(cell),
                scale,
                ink,
            );
            y = y.saturating_add(cell.saturating_add(scale.saturating_mul(3)));
            if y.saturating_add(cell) >= height {
                break;
            }
        }
        if column.rows.len() > ROWS_PER_COLUMN {
            let more = column.rows.len().saturating_sub(ROWS_PER_COLUMN);
            draw_clipped(
                &mut image,
                &format!("+{more} more"),
                x,
                y,
                column_width.saturating_sub(cell),
                scale,
                palette.quiet,
            );
        }
    }

    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| Failure::new(format!("encode Issues board frame: {error}")))?;
    Ok(RenderedFrame {
        media_type: FrameMediaType::Png,
        width,
        height,
        bytes: cursor.into_inner(),
    })
}

fn draw_rule(image: &mut RgbaImage, x: u32, y: u32, width: u32, colour: Rgba<u8>) {
    for offset in 0..width {
        let px = x.saturating_add(offset);
        if px < image.width() && y < image.height() {
            image.put_pixel(px, y, colour);
        }
    }
}

/// Draw one line, cut to the width it is given.
///
/// Cut rather than wrapped: a board column is a list, and a title that flowed
/// onto a second line would push the row below it out of alignment with the
/// column beside it.
fn draw_clipped(
    image: &mut RgbaImage,
    text: &str,
    x: u32,
    y: u32,
    width: u32,
    scale: u32,
    colour: Rgba<u8>,
) {
    let cell = scale.saturating_mul(9).max(1);
    let columns = usize::try_from(width / cell).unwrap_or(0);
    if columns == 0 {
        return;
    }
    let mut line: String = text.chars().take(columns).collect();
    if text.chars().count() > columns && columns > 1 {
        line = text.chars().take(columns - 1).collect();
        line.push('…');
    }
    draw_text(image, &line, x, y, scale, colour);
}

fn draw_text(image: &mut RgbaImage, text: &str, x: u32, y: u32, scale: u32, colour: Rgba<u8>) {
    let cell = scale.saturating_mul(9).max(1);
    for (column, character) in text.chars().enumerate() {
        let cx = x.saturating_add(
            u32::try_from(column)
                .unwrap_or(u32::MAX)
                .saturating_mul(cell),
        );
        if cx >= image.width() {
            break;
        }
        draw_character(image, character, cx, y, scale, colour);
    }
}

fn draw_character(
    image: &mut RgbaImage,
    character: char,
    x: u32,
    y: u32,
    scale: u32,
    colour: Rgba<u8>,
) {
    // A glyph the basic font does not carry is drawn as nothing rather than as
    // a substitute: a wall showing the wrong character is worse than one
    // showing a gap, because only the gap is legible as missing.
    let Some(glyph) = BASIC_FONTS.get(character) else {
        return;
    };
    for (row, bits) in glyph.iter().enumerate() {
        for bit in 0..8u32 {
            if bits & (1 << bit) == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x
                        .saturating_add(bit.saturating_mul(scale))
                        .saturating_add(dx);
                    let py = y
                        .saturating_add(u32::try_from(row).unwrap_or(0).saturating_mul(scale))
                        .saturating_add(dy);
                    if px < image.width() && py < image.height() {
                        image.put_pixel(px, py, colour);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(width: u32, height: u32) -> DisplayRequest {
        DisplayRequest {
            surface: DisplaySurfaceId::new(SURFACE_ID).unwrap(),
            width,
            height,
            scale_milli: 1000,
            theme: DisplayTheme::Dark,
            locale: "en".into(),
            window_start_unix: 1_700_000_000,
            window_horizon_ms: 300_000,
            input: CanonicalDisplayInput::new(br#"{"project":"ENG"}"#.to_vec()).unwrap(),
        }
    }

    fn row(reff: &str, alias: &str, title: &str, provisional: bool) -> Value {
        row_in("backlog", reff, alias, title, provisional)
    }

    /// A row in one named workflow state. The state is on the row now: the
    /// board is delivered as one ordered page plus the workflow it is read
    /// against, and `status` is what puts a row in a column.
    fn row_in(state: &str, reff: &str, alias: &str, title: &str, provisional: bool) -> Value {
        serde_json::json!({
            "reff": reff,
            "doc_id": "iss_01JV9VBLGTK0BME1R9KS2H5GIC",
            "project_id": "prj_01JV9VB8C96C5A4HCML1REEL5L",
            "key_alias": alias,
            "title": title,
            "status": state,
            "priority": "high",
            "assignee_summary": "",
            "assignees": [],
            "tombstone": false,
            "provisional": provisional,
        })
    }

    /// `IssuesResponse::Board` is an internally-tagged *newtype* variant, so
    /// the `BoardPage` fields sit beside `kind` rather than under a key.
    ///
    /// These fixtures are still written as columns, because columns are what
    /// the surface draws and what each test is about. The wire no longer
    /// carries them: it carries the workflow and one ordered page of rows,
    /// and the surface groups. So this takes the readable form and flattens
    /// it into the shape that actually arrives — which also keeps the tests
    /// honest about the grouping being the surface's own work.
    fn board(columns: Value) -> Value {
        let columns = columns.as_array().cloned().unwrap_or_default();
        let workflow: Vec<Value> = columns
            .iter()
            .map(|column| column["state"].clone())
            .collect();
        let rows: Vec<Value> = columns
            .iter()
            .flat_map(|column| {
                column["rows"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
            })
            .collect();
        serde_json::json!({
            "kind": "board",
            "schema_version": 5,
            "project": {
                "id": "prj_01JV9VB8C96C5A4HCML1REEL5L",
                "name": "Engineering",
                "key": "ENG",
                "color": "blue",
            },
            "workflow": workflow,
            "rows": {
                "publication": {
                    "materialization": 0,
                    "publication": {
                        "extractor_schema_digest": vec![0u8; 32],
                        "implementation_digest": vec![0u8; 32],
                        "manifest_root": vec![0u8; 32],
                    },
                },
                "items": rows,
                "next_cursor": null,
                "exact_total": null,
            },
        })
    }

    /// One column, named. The state id is derived from the name so that two
    /// columns in one fixture are two columns after grouping rather than one.
    fn column(name: &str, rows: Vec<Value>) -> Value {
        let id = name.to_ascii_lowercase().replace(' ', "-");
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|mut row| {
                row["status"] = Value::String(id.clone());
                row
            })
            .collect();
        serde_json::json!({
            "state": { "id": id, "name": name, "category": "backlog", "color": "gray" },
            "rows": rows,
        })
    }

    #[test]
    fn a_board_reaches_a_screen_as_frames_and_nothing_it_could_name() {
        let value = board(serde_json::json!([column(
            "Backlog",
            vec![row("iss_01JV9", "ENG-1", "Widen LocalNet", false)]
        )]));
        let projection = render(value, &request(1280, 720)).unwrap();

        let RenderedScene::Frame(frame) = &projection.program.items[0].scene else {
            panic!("a board did not project as a frame");
        };
        assert_eq!(frame.bytes.get(..8), Some(b"\x89PNG\r\n\x1a\n".as_slice()));

        // The claim the surface contract exists to keep: a receiver holding this
        // program cannot name an issue. Column names and titles are drawn *into*
        // the pixels; none of the product's identifiers ride beside them.
        let wire = serde_json::to_string(&serde_json::json!({
            "items": projection
                .program
                .items
                .iter()
                .map(|item| serde_json::json!({
                    "id": item.id,
                    "duration_ms": item.duration_ms,
                }))
                .collect::<Vec<_>>(),
        }))
        .unwrap();
        assert!(
            !wire.contains("iss_01JV9"),
            "an issue id reached the receiver"
        );
        assert!(!wire.contains("ENG-1"), "an issue key reached the receiver");
        assert!(!wire.contains("prj_"), "a project id reached the receiver");
    }

    #[test]
    fn a_provisional_row_makes_the_whole_projection_partial() {
        let value = board(serde_json::json!([
            column("Backlog", vec![row("iss_a", "ENG-1", "Settled", false)]),
            column("Doing", vec![row("iss_b", "ENG-2", "Not settled", true)]),
        ]));
        let projection = render(value, &request(1280, 720)).unwrap();

        // Drawn, and declared. A wall that showed provisional work as settled
        // would be the more confident and less true answer.
        let DisplayAssessment::Partial(reasons) = &projection.assessment else {
            panic!("a provisional row did not make the projection partial");
        };
        assert!(reasons.contains(&DisplayPartialReason::ProvisionalData));
    }

    #[test]
    fn a_settled_board_is_current_rather_than_cautious() {
        let value = board(serde_json::json!([column(
            "Backlog",
            vec![row("iss_a", "ENG-1", "Settled", false)]
        )]));
        let projection = render(value, &request(1280, 720)).unwrap();
        assert!(matches!(projection.assessment, DisplayAssessment::Current));
    }

    #[test]
    fn more_columns_than_fit_become_more_frames_rather_than_smaller_ones() {
        let columns: Vec<Value> = (0..7)
            .map(|index| column(&format!("State {index}"), vec![]))
            .collect();
        let projection = render(board(serde_json::json!(columns)), &request(1280, 720)).unwrap();

        // Seven columns at three per frame is three items, not one frame of
        // unreadable slivers.
        assert_eq!(projection.program.items.len(), 3);
        assert!(matches!(projection.program.cycle, ProgramCycle::Loop));
    }

    #[test]
    fn a_column_reports_its_real_length_even_when_it_draws_fewer_rows() {
        let rows: Vec<Value> = (0..20)
            .map(|index| {
                row(
                    &format!("iss_{index}"),
                    &format!("ENG-{index}"),
                    "Work",
                    false,
                )
            })
            .collect();
        let projection = render(
            board(serde_json::json!([column("Backlog", rows)])),
            &request(1280, 720),
        )
        .unwrap();

        // Twenty, not the eight that fit. The cap is a legibility decision and
        // must never become a claim about how much work there is.
        let spoken = projection.program.items[0]
            .spoken_summary
            .as_deref()
            .unwrap();
        assert!(
            spoken.contains("20 Backlog"),
            "spoken summary said: {spoken}"
        );
    }

    #[test]
    fn a_board_with_no_columns_says_so_rather_than_blanking() {
        let projection = render(board(serde_json::json!([])), &request(1280, 720)).unwrap();

        // Not a blank and not a failure: a project with no workflow states is a
        // board with nothing on it, which is a fact worth drawing.
        assert_eq!(projection.program.items.len(), 1);
        assert!(matches!(
            projection.program.items[0].scene,
            RenderedScene::Frame(_)
        ));
        assert!(matches!(projection.assessment, DisplayAssessment::Current));
    }

    #[test]
    fn the_surface_prepares_a_read_and_nothing_else() {
        let invocation = prepare(&request(1280, 720)).unwrap();
        assert_eq!(invocation.access(), ClientAccess::Query);
    }

    #[test]
    fn a_shapeless_input_is_refused_by_the_package_that_owns_it() {
        assert!(canonicalize_input(serde_json::json!({})).is_err());
        assert!(canonicalize_input(serde_json::json!({ "project": "   " })).is_err());
        assert!(canonicalize_input(serde_json::json!({ "project": "E".repeat(65) })).is_err());
        // Trimmed once, here, so the coordinator stores exact bytes it never had
        // to interpret.
        let canonical = canonicalize_input(serde_json::json!({ "project": " ENG " })).unwrap();
        assert_eq!(canonical.as_bytes(), br#"{"project":"ENG"}"#);
    }

    #[test]
    fn a_render_larger_than_the_frame_bound_is_refused() {
        let refused = render(
            board(serde_json::json!([])),
            &request(MAX_RENDER_WIDTH + 1, 720),
        );
        assert!(refused.is_err());
    }

    #[test]
    fn the_descriptor_commits_to_its_own_world() {
        let surface = board_wall_surface().unwrap();
        surface
            .descriptor
            .validate(&issues::contract::world_id())
            .expect("the board surface does not validate against Issues");
        assert!(surface
            .descriptor
            .outputs
            .contains(&DisplayOutputKind::Frame));
        // Frames only: declaring Media would advertise a capability this
        // renderer never produces.
        assert!(!surface
            .descriptor
            .outputs
            .contains(&DisplayOutputKind::Media));
    }
}
