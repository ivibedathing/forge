//! Validating a tileset file (M47).
//!
//! A tileset is a file kind of its own, recognised by shape — a top-level
//! `tiles`, no `entities`, no `tracks` — the way `engine validate` already
//! routes clips and materials. The field walk is the same schema-driven one
//! every component gets, so unknown fields, closed vocabularies, numeric ranges
//! and `did_you_mean` all arrive without bespoke code.
//!
//! What is left is what a schema cannot say: that a socket string parses, that
//! a part names a palette entry that exists, and — the check the format is
//! hardest to author without — that every socket has somebody to mate.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::codes;
use crate::error::EngineError;
use crate::lineindex::LineIndex;
use crate::tileset::{self, Compat, Face, Tileset, MAX_PARTS_PER_TILE};

use super::{kind_of, walk_component, ComponentSchemas, Cx};

/// Every diagnostic a tileset file has, all at once.
pub fn validate_tileset_source(source: &str, path: &str) -> Vec<EngineError> {
    let root: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            return vec![EngineError::new(codes::INVALID_JSON, e.to_string())
                .file(path)
                .line(e.line() as u32)
                .column(e.column() as u32)];
        }
    };

    let cx = Cx {
        file: path,
        index: LineIndex::new(source),
    };
    let mut errors = Vec::new();

    let Some(object) = root.as_object() else {
        errors.push(cx.err(
            codes::TILESET_MALFORMED,
            format!(
                "a tileset file must be a JSON object, found {}",
                kind_of(&root)
            ),
            "",
        ));
        return errors;
    };

    let schemas = ComponentSchemas::from_schema(crate::schema::tileset_schema());
    let clean = walk_component(
        &cx,
        &schemas,
        schemas.root(),
        object,
        "Tileset",
        "",
        "",
        &mut errors,
    );
    if !clean {
        // The shape is wrong, so serde would reject it and every check below
        // would be reading fields that are not there. Report what the walk
        // found and stop — the all-at-once contract is per *pass*, not a
        // promise to guess past a structural break.
        return errors;
    }

    let tileset: Tileset = match serde_json::from_value(root.clone()) {
        Ok(parsed) => parsed,
        Err(e) => {
            // The walk passed and serde refused: that is the engine
            // disagreeing with itself, and it has its own code for exactly
            // this reason (M5 §7).
            errors.push(
                cx.err(
                    codes::SCENE_PARSE_DESYNC,
                    format!("tileset passed the field walk but failed to parse: {e}"),
                    "",
                )
                .file(path),
            );
            return errors;
        }
    };

    check_cell(&cx, &tileset, &mut errors);
    check_tiles(&cx, &tileset, &mut errors);
    check_sockets(&cx, &tileset, &mut errors);
    check_partners(&cx, &tileset, &mut errors);
    errors
}

fn check_cell(cx: &Cx<'_>, tileset: &Tileset, errors: &mut Vec<EngineError>) {
    for (axis, name) in [(tileset.cell.x, "x"), (tileset.cell.y, "y"), (tileset.cell.z, "z")] {
        // Negated so a NaN fails. `axis <= 0.0` is a different function — it
        // is *false* for NaN — and a cell of NaN metres would place every tile
        // nowhere while validating clean.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(axis > 0.0) {
            errors.push(
                cx.err(
                    codes::INVALID_SHAPE_DIMENSION,
                    format!("a tileset's cell must be positive on every axis; {name} is {axis}"),
                    "/cell",
                )
                .component("Tileset")
                .field("cell"),
            );
        }
    }
}

fn check_tiles(cx: &Cx<'_>, tileset: &Tileset, errors: &mut Vec<EngineError>) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let palette: Vec<&str> = tileset.palette.keys().map(String::as_str).collect();

    for (index, tile) in tileset.tiles.iter().enumerate() {
        let at = format!("/tiles/{index}");
        if tile.name.is_empty() {
            errors.push(
                cx.err(
                    codes::TILESET_MALFORMED,
                    "a tile's name is the empty string; a layout addresses tiles by name".into(),
                    &format!("{at}/name"),
                )
                .component("Tileset")
                .field("name"),
            );
        }
        if tile.name.contains(['@', ' ', '!']) {
            errors.push(
                cx.err(
                    codes::TILESET_MALFORMED,
                    format!(
                        "tile name {:?} holds a character a layout row reserves: \
                         '@' separates the rotation, ' ' separates cells, '!' locks one",
                        tile.name
                    ),
                    &format!("{at}/name"),
                )
                .component("Tileset")
                .field("name"),
            );
        }
        if !seen.insert(tile.name.as_str()) {
            errors.push(
                cx.err(
                    codes::TILESET_MALFORMED,
                    format!("two tiles are named {:?}; a layout could mean either", tile.name),
                    &format!("{at}/name"),
                )
                .component("Tileset")
                .field("name"),
            );
        }
        if !matches!(tile.rotations, 1 | 2 | 4) {
            errors.push(
                cx.err(
                    codes::TILESET_MALFORMED,
                    format!(
                        "tile {:?} asks for {} rotations; a quarter-turn set is 1, 2 or 4",
                        tile.name, tile.rotations
                    ),
                    &format!("{at}/rotations"),
                )
                .component("Tileset")
                .field("rotations"),
            );
        }
        if tile.parts.len() > MAX_PARTS_PER_TILE {
            errors.push(
                cx.err(
                    codes::TILESET_TOO_COMPLEX,
                    format!(
                        "tile {:?} carries {} parts, more than the {MAX_PARTS_PER_TILE} a tile grows",
                        tile.name,
                        tile.parts.len()
                    ),
                    &format!("{at}/parts"),
                )
                .component("Tileset")
                .field("parts"),
            );
        }

        for (part_index, part) in tile.parts.iter().enumerate() {
            let part_at = format!("{at}/parts/{part_index}");
            if !tileset.palette.contains_key(&part.material) {
                errors.push(
                    cx.err(
                        codes::UNKNOWN_PALETTE_KEY,
                        format!(
                            "tile {:?} paints a part with {:?}, which is not in the palette",
                            tile.name, part.material
                        ),
                        &format!("{part_at}/material"),
                    )
                    .component("Tileset")
                    .field("material")
                    .suggest_from(&part.material, palette.iter().copied()),
                );
            }
            for (axis, name) in [(part.size.x, "x"), (part.size.y, "y"), (part.size.z, "z")] {
                // Negated for the NaN reason above.
                #[allow(clippy::neg_cmp_op_on_partial_ord)]
                if !(axis > 0.0) {
                    errors.push(
                        cx.err(
                            codes::INVALID_SHAPE_DIMENSION,
                            format!(
                                "tile {:?} has a part of size {axis} on {name}; a part with no \
                                 extent grows geometry nothing can see",
                                tile.name
                            ),
                            &format!("{part_at}/size"),
                        )
                        .component("Tileset")
                        .field("size"),
                    );
                }
            }
        }
    }
}

fn check_sockets(cx: &Cx<'_>, tileset: &Tileset, errors: &mut Vec<EngineError>) {
    for (index, tile) in tileset.tiles.iter().enumerate() {
        for face in Face::ALL {
            let key = Face::KEYS[face.index()];
            let text = tile.faces.get(face);
            let at = format!("/tiles/{index}/faces/{key}");
            match tileset::parse_socket(text) {
                Ok((_, invariant)) if invariant && !face.is_vertical() => errors.push(
                    cx.err(
                        codes::UNKNOWN_SOCKET_FORM,
                        format!(
                            "tile {:?} writes {text:?} on {key}, but _i drops the rotation \
                             index and only a vertical face carries one",
                            tile.name
                        ),
                        &at,
                    )
                    .component("Tileset")
                    .field(key),
                ),
                Ok(_) => {}
                Err(message) => errors.push(
                    cx.err(
                        codes::UNKNOWN_SOCKET_FORM,
                        format!("tile {:?}: {message}", tile.name),
                        &at,
                    )
                    .component("Tileset")
                    .field(key),
                ),
            }
        }
    }
}

/// The check that makes the format writable from a prompt.
///
/// A socket graph is a constraint problem rather than a description, so it is
/// the part of a tileset an author gets wrong first — and a tile with an
/// orphaned socket is not an error, it is a tile that silently never appears.
/// Reporting it by *name* is what turns "the village came out empty" into "the
/// `eave` socket on `roof_slope.pz` has nobody to mate".
fn check_partners(cx: &Cx<'_>, tileset: &Tileset, errors: &mut Vec<EngineError>) {
    let Ok(expanded) = tileset::expand(tileset) else {
        // `expand` only fails on the tile budget, which `check_tiles` reports
        // against the same file. Nothing to add.
        return;
    };
    let compat = Compat::build(&expanded);

    // One report per **authored** (tile, face), not per rotation. The face a
    // socket lands on moves with the turn, so the same authored mistake shows
    // up on `px` at one rotation and `nz` at the next; keying on the expanded
    // face would report it four times and point at three faces the author
    // never wrote. Walking the permutation back is what names the line to fix.
    let mut orphaned: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (index, tile) in expanded.iter().enumerate() {
        for face in Face::ALL {
            if compat.partners(face.index(), index) == 0 {
                let mut authored = face;
                for _ in 0..tile.rotation {
                    authored = authored.source_after_turn();
                }
                let socket = tile.faces[face.index()].socket.name().to_string();
                orphaned.insert((tile.tile, authored.index()), socket);
            }
        }
    }

    for ((tile_index, face_index), socket) in orphaned {
        let tile = &tileset.tiles[tile_index];
        let key = Face::KEYS[face_index];
        errors.push(
            cx.err(
                codes::TILE_SOCKET_ORPHANED,
                format!(
                    "no tile mates the socket {socket:?} that {:?} carries on {key}, so \
                     nothing can ever sit that side of it — check for a missing \
                     _l/_r pair, or a name spelled two ways",
                    tile.name
                ),
                &format!("/tiles/{tile_index}/faces/{key}"),
            )
            .component("Tileset")
            .field(key)
            .warning(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
      "cell": [2.0, 2.5, 2.0],
      "palette": { "stone": { "albedo": [0.5, 0.5, 0.5] } },
      "tiles": [
        { "name": "air", "faces": {} },
        { "name": "floor", "rotations": 1,
          "faces": { "px": "gnd", "nx": "gnd", "pz": "gnd", "nz": "gnd" },
          "parts": [ { "kind": "box", "at": [0, 0.1, 0], "size": [2, 0.2, 2], "material": "stone" } ] }
      ]
    }"#;

    fn codes_of(source: &str) -> Vec<&'static str> {
        validate_tileset_source(source, "t.json")
            .into_iter()
            .map(|e| e.error)
            .collect()
    }

    #[test]
    fn a_well_formed_tileset_reports_nothing() {
        assert_eq!(codes_of(GOOD), Vec::<&str>::new());
    }

    #[test]
    fn the_field_walk_drives_the_ordinary_checks() {
        assert!(codes_of(r#"{"tiles": [], "cel": [1,1,1]}"#).contains(&codes::UNKNOWN_FIELD));
        assert!(codes_of(r#"{"tiles": [{"nam": "a"}]}"#).contains(&codes::UNKNOWN_FIELD));
        assert!(codes_of(r#"{"tiles": [{}]}"#).contains(&codes::MISSING_FIELD));
        assert!(
            codes_of(
                r#"{"palette": {"s": {}}, "tiles": [{"name": "a", "parts":
                   [{"kind": "sphere", "size": [1,1,1], "material": "s"}]}]}"#
            )
            .contains(&codes::INVALID_FIELD_TYPE),
            "a part kind is a closed vocabulary"
        );
    }

    /// A `BTreeMap` field publishes `additionalProperties` and no
    /// `properties`, so before the walk learned that arm every palette key
    /// reported as an unknown field.
    #[test]
    fn palette_keys_are_data_rather_than_unknown_fields() {
        assert_eq!(
            codes_of(r#"{"palette": {"anything_at_all": {"roughness": 0.5}}, "tiles": []}"#),
            Vec::<&str>::new()
        );
        assert!(
            codes_of(r#"{"palette": {"s": {"roughnes": 0.5}}, "tiles": []}"#)
                .contains(&codes::UNKNOWN_FIELD),
            "but their values are still checked"
        );
    }

    #[test]
    fn a_part_must_name_a_palette_entry() {
        let errors = validate_tileset_source(
            r#"{"palette": {"stone": {}}, "tiles": [{"name": "a", "parts":
               [{"kind": "box", "size": [1,1,1], "material": "ston"}]}]}"#,
            "t.json",
        );
        let hit = errors
            .iter()
            .find(|e| e.error == codes::UNKNOWN_PALETTE_KEY)
            .expect("reported");
        assert_eq!(
            hit.context().and_then(|c| c.did_you_mean.as_deref()),
            Some("stone"),
            "a near miss gets the suggestion the wire contract promises"
        );
    }

    #[test]
    fn socket_forms_are_checked_and_located() {
        assert!(codes_of(
            r#"{"tiles": [{"name": "a", "faces": {"px": "x_l_r"}}]}"#
        )
        .contains(&codes::UNKNOWN_SOCKET_FORM));
        assert!(
            codes_of(r#"{"tiles": [{"name": "a", "faces": {"px": "x_i"}}]}"#)
                .contains(&codes::UNKNOWN_SOCKET_FORM),
            "_i on a horizontal face names a rotation index that face never had"
        );
        assert!(
            !codes_of(r#"{"tiles": [{"name": "a", "faces": {"py": "x_i"}}]}"#)
                .contains(&codes::UNKNOWN_SOCKET_FORM),
            "on a vertical face _i is the whole point of the suffix"
        );
    }

    #[test]
    fn an_orphaned_socket_is_a_warning_that_names_the_socket() {
        let errors = validate_tileset_source(
            r#"{"tiles": [
                 {"name": "a", "faces": {"px": "join"}},
                 {"name": "b", "faces": {"px": "jion"}}
               ]}"#,
            "t.json",
        );
        let orphans: Vec<&EngineError> = errors
            .iter()
            .filter(|e| e.error == codes::TILE_SOCKET_ORPHANED)
            .collect();
        assert!(orphans.iter().all(|e| e.is_warning()));
        // Both halves of the typo, each named, so the fix is a diff of one
        // character rather than a hunt.
        assert!(orphans.iter().any(|e| e.message.contains("\"join\"")));
        assert!(orphans.iter().any(|e| e.message.contains("\"jion\"")));
    }

    /// A rotationally expanded tile is usually its own partner — `px` at
    /// rotation 0 meets `nx` at rotation 2 — so the orphan case that survives
    /// expansion is a vertical face, which does not turn.
    #[test]
    fn an_orphan_on_a_rotated_tile_reports_once() {
        let errors = validate_tileset_source(
            r#"{"tiles": [{"name": "a", "rotations": 4, "faces": {"px": "0", "nx": "0",
                 "pz": "0", "nz": "0", "py": "lonely", "ny": "0"}}]}"#,
            "t.json",
        );
        let named: Vec<&EngineError> = errors
            .iter()
            .filter(|e| e.error == codes::TILE_SOCKET_ORPHANED && e.message.contains("lonely"))
            .collect();
        assert_eq!(named.len(), 1, "four rotations, one authoring mistake");
        assert_eq!(
            named[0].context().and_then(|c| c.field.as_deref()),
            Some("py"),
            "and it points at the face the author actually wrote"
        );
    }

    /// A tile whose faces all rotate does find its own partners, which is why
    /// the check above had to reach for a vertical face.
    #[test]
    fn a_four_way_tile_mates_its_own_turns() {
        assert!(
            !codes_of(
                r#"{"tiles": [{"name": "a", "rotations": 4,
                     "faces": {"px": "join", "nx": "0", "pz": "0", "nz": "0",
                               "py": "0", "ny": "0"}}]}"#
            )
            .contains(&codes::TILE_SOCKET_ORPHANED),
            "px at rotation 0 meets nx at rotation 2"
        );
    }

    #[test]
    fn a_tile_name_may_not_hold_what_a_layout_row_reserves() {
        assert!(codes_of(r#"{"tiles": [{"name": "wall@2"}]}"#).contains(&codes::TILESET_MALFORMED));
        assert!(codes_of(r#"{"tiles": [{"name": "two words"}]}"#).contains(&codes::TILESET_MALFORMED));
        assert!(codes_of(r#"{"tiles": [{"name": "a"}, {"name": "a"}]}"#)
            .contains(&codes::TILESET_MALFORMED));
    }

    #[test]
    fn a_part_with_no_extent_is_an_error() {
        assert!(codes_of(
            r#"{"palette": {"s": {}}, "tiles": [{"name": "a", "parts":
               [{"kind": "box", "size": [1, 0, 1], "material": "s"}]}]}"#
        )
        .contains(&codes::INVALID_SHAPE_DIMENSION));
    }
}
