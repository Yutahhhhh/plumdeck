//! Turns a flat list of accessibility nodes into deck observations.
//!
//! rekordbox's window subtree is essentially flat: the deck header texts are
//! siblings of everything else, carry no identifier/title/help, and their order
//! is not the deck order. So decks are located structurally (roles) and then
//! spatially, anchored on the small "1".."4" labels rekordbox prints beside each
//! deck. Nothing here matches on a track title or on an absolute coordinate.

use crate::assist::DeckObservation;

pub const ROLE_STATIC_TEXT: &str = "AXStaticText";
pub const ROLE_TEXT_AREA: &str = "AXTextArea";
pub const ROLE_POPUP_BUTTON: &str = "AXPopUpButton";

/// A single accessibility node, flattened to the fields the parser uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub role: String,
    pub value: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Node {
    #[cfg(test)]
    pub fn new(role: &str, value: &str, x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            role: role.into(),
            value: value.into(),
            x,
            y,
            width,
            height,
        }
    }

    fn text(&self) -> &str {
        self.value.trim()
    }
}

/// The deck number labels rekordbox draws next to each deck.
fn deck_slot(node: &Node) -> Option<u32> {
    if node.role != ROLE_STATIC_TEXT || node.width > 32.0 || node.height > 32.0 {
        return None;
    }
    match node.text() {
        "1" => Some(1),
        "2" => Some(2),
        "3" => Some(3),
        "4" => Some(4),
        _ => None,
    }
}

/// Strict decimal parse: `" 88.00"` yes, `"00:00"` / `".5"` / `"±16"` no.
fn parse_decimal(text: &str) -> Option<f64> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text),
    };
    let mut parts = digits.split('.');
    let integer = parts.next()?;
    let fraction = parts.next().unwrap_or("0");
    if parts.next().is_some() || integer.is_empty() {
        return None;
    }
    if !integer.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !fraction.is_empty() && !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<f64>().ok().map(|value| value * sign)
}

/// A BPM readout. rekordbox prints `0.00` for a track it has no tempo for, and
/// that is an absence of information, not a 0 BPM track.
fn parse_bpm(text: &str) -> Option<f64> {
    let value = parse_decimal(text)?;
    if (20.0..=400.0).contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Whether a numeric-looking cell is a tempo cell at all (including "unknown").
fn is_bpm_cell(text: &str) -> bool {
    parse_decimal(text).is_some_and(|value| value == 0.0 || (20.0..=400.0).contains(&value))
}

/// Classical (`Bm`, `F#`, `Ab maj`) or Camelot (`6B`, `10A`) notation.
///
/// rekordbox renders whichever the user configured, so both must be accepted.
/// The string is passed through untouched; normalization happens in the backend.
fn is_key_text(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.len() > 8 {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let camelot_digits: String = chars.iter().take_while(|c| c.is_ascii_digit()).collect();
    if !camelot_digits.is_empty() {
        return camelot_digits
            .parse::<u32>()
            .is_ok_and(|n| (1..=12).contains(&n))
            && chars.len() == camelot_digits.chars().count() + 1
            && matches!(chars[chars.len() - 1], 'A' | 'B' | 'a' | 'b');
    }
    if !matches!(chars[0], 'A'..='G' | 'a'..='g') {
        return false;
    }
    let mut rest = &chars[1..];
    if rest
        .first()
        .is_some_and(|c| matches!(c, '#' | 'b' | '♯' | '♭'))
    {
        rest = &rest[1..];
    }
    let suffix: String = rest.iter().collect::<String>().trim().to_lowercase();
    matches!(
        suffix.as_str(),
        "" | "m" | "maj" | "min" | "major" | "minor"
    )
}

/// A deck-layout selector such as `"2Deck Horizontal"`.
///
/// Used to warn when fewer decks were found than the layout implies, and to
/// discard deck numbers above the layout's deck count. It never adds decks that
/// were not actually observed.
pub fn layout_hint(nodes: &[Node]) -> Option<String> {
    nodes
        .iter()
        .filter(|node| node.role == ROLE_POPUP_BUTTON)
        .map(|node| node.text())
        .find(|text| {
            let lowered = text.to_lowercase();
            lowered.starts_with(|c: char| c.is_ascii_digit()) && lowered.contains("deck")
        })
        .map(str::to_string)
}

fn layout_deck_count(hint: &str) -> Option<u32> {
    let digits: String = hint.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| (1..=4).contains(n))
}

struct DeckColumn<'a> {
    slot: u32,
    label: &'a Node,
    right_bound: f64,
}

/// Splits the window into one column per deck label found.
///
/// Deck labels that sit on the same row bound each other; the right-most one
/// runs to the edge of the window. Vertically stacked layouts therefore produce
/// full-width columns per row rather than a single wrong split.
fn deck_columns<'a>(labels: &[(u32, &'a Node)], window_right: f64) -> Vec<DeckColumn<'a>> {
    labels
        .iter()
        .map(|(slot, label)| {
            let row_tolerance = (label.height * 2.0).max(24.0);
            let right_bound = labels
                .iter()
                .filter(|(_, other)| {
                    other.x > label.x && (other.y - label.y).abs() <= row_tolerance
                })
                .map(|(_, other)| other.x)
                .fold(window_right, f64::min);
            DeckColumn {
                slot: *slot,
                label,
                right_bound,
            }
        })
        .collect()
}

/// Reads the decks out of one window's nodes.
///
/// Returns the observations plus any warnings worth showing the user verbatim.
pub fn parse_decks(nodes: &[Node], window_right: f64) -> (Vec<DeckObservation>, Vec<String>) {
    let mut warnings = Vec::new();

    let mut labels: Vec<(u32, &Node)> = nodes
        .iter()
        .filter_map(|node| deck_slot(node).map(|slot| (slot, node)))
        .collect();
    labels.sort_by(|a, b| {
        (a.1.y, a.1.x)
            .partial_cmp(&(b.1.y, b.1.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // A deck header has wide title text alongside it or a platter readout below.
    // Pads sit below the platter and cannot establish a deck on their own.
    labels.retain(|(_, label)| {
        nodes.iter().any(|node| {
            node.role == ROLE_TEXT_AREA
                && node.x > label.x
                && node.y > label.y + 48.0
                && node.y < label.y + 200.0
                && parse_bpm(node.text()).is_some()
        }) || nodes.iter().any(|node| {
            node.role == ROLE_STATIC_TEXT
                && node.x > label.x
                && node.x < label.x + 100.0
                && node.width >= 100.0
                && (node.y - label.y).abs() <= 8.0
        })
    });
    let before_dedupe = labels.len();
    let mut seen = Vec::new();
    labels.retain(|(slot, _)| {
        if seen.contains(slot) {
            false
        } else {
            seen.push(*slot);
            true
        }
    });
    if labels.len() != before_dedupe {
        warnings
            .push("同じデッキ番号のラベルが複数見つかったため、最初の1つだけを使いました".into());
    }

    if let Some(expected) = layout_hint(nodes).as_deref().and_then(layout_deck_count) {
        labels.truncate(expected as usize);
    }
    if labels.is_empty() {
        return (Vec::new(), warnings);
    }

    let columns = deck_columns(&labels, window_right);
    let decks: Vec<DeckObservation> = columns
        .iter()
        .map(|column| read_deck(nodes, column))
        .collect();

    if let Some(expected) = layout_hint(nodes).as_deref().and_then(layout_deck_count) {
        if (decks.len() as u32) < expected {
            warnings.push(format!(
                "レイアウトは {} デッキですが、読み取れたのは {} デッキ分です",
                expected,
                decks.len()
            ));
        }
    }

    (decks, warnings)
}

fn read_deck(nodes: &[Node], column: &DeckColumn) -> DeckObservation {
    let label = column.label;
    let in_column = |node: &Node| node.x >= label.x && node.x < column.right_bound;

    // The title is the one wide text vertically centred on the deck label. Both
    // conditions matter: on an empty deck the transport time readouts sit in the
    // same column and would otherwise be read as a title.
    let label_centre = label.y + label.height / 2.0;
    let centre_tolerance = label.height.max(16.0);
    let min_title_width = ((column.right_bound - label.x) * 0.15).max(40.0);
    let title_node = nodes
        .iter()
        .filter(|node| {
            node.role == ROLE_STATIC_TEXT
                && in_column(node)
                && node.x > label.x
                && !node.text().is_empty()
                && node.width >= min_title_width
                && (node.y + node.height / 2.0 - label_centre).abs() <= centre_tolerance
        })
        // Equal widths resolve to the left-most candidate so the choice never
        // depends on the order the accessibility tree happened to return.
        .max_by(|a, b| {
            (a.width, -a.x)
                .partial_cmp(&(b.width, -b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    let Some(title_node) = title_node else {
        return DeckObservation {
            slot: column.slot,
            loaded: false,
            tempo_bpm: tempo_readout(nodes, column),
            ..Default::default()
        };
    };

    // The artist sits directly under the title and shares its left edge; the
    // transport time readouts in the same band do not.
    let artist_node = nodes
        .iter()
        .filter(|node| {
            node.role == ROLE_STATIC_TEXT
                && (node.x - title_node.x).abs() <= 4.0
                && node.y > title_node.y + 1.0
                && node.y <= title_node.y + 48.0
                && !node.text().is_empty()
        })
        .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

    let (track_bpm, display_key) = match artist_node {
        Some(artist) => read_metadata_row(nodes, column, artist),
        None => {
            let row_y = nodes
                .iter()
                .filter(|node| {
                    node.role == ROLE_STATIC_TEXT
                        && node.x > title_node.x
                        && node.x < column.right_bound
                        && node.y > title_node.y + 1.0
                        && node.y <= title_node.y + 48.0
                        && !node.text().is_empty()
                })
                .map(|node| node.y)
                .reduce(f64::min);
            row_y
                .map(|y| {
                    let mut anchor = title_node.clone();
                    anchor.y = y;
                    read_metadata_row(nodes, column, &anchor)
                })
                .unwrap_or((None, None))
        }
    };

    DeckObservation {
        slot: column.slot,
        loaded: true,
        title: Some(title_node.text().to_string()),
        artist: artist_node.map(|node| node.text().to_string()),
        track_bpm,
        tempo_bpm: tempo_readout(nodes, column),
        display_key,
        elapsed_seconds: elapsed_readout(nodes, column, title_node),
    }
}

/// BPM and key share the artist's row, to the right of the artist name.
fn read_metadata_row(
    nodes: &[Node],
    column: &DeckColumn,
    artist: &Node,
) -> (Option<f64>, Option<String>) {
    let mut row: Vec<&Node> = nodes
        .iter()
        .filter(|node| {
            node.role == ROLE_STATIC_TEXT
                && node.x > artist.x
                && node.x < column.right_bound
                && (node.y - artist.y).abs() <= 4.0
                && !node.text().is_empty()
        })
        .collect();
    row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

    let bpm_index = row.iter().position(|node| is_bpm_cell(node.text()));
    let bpm = bpm_index.and_then(|index| parse_bpm(row[index].text()));
    let key_search_from = bpm_index.map_or(0, |index| index + 1);
    let key = row
        .get(key_search_from..)
        .and_then(|rest| rest.iter().find(|node| is_key_text(node.text())))
        .map(|node| node.text().to_string());
    (bpm, key)
}

/// The platter tempo readout: stored BPM with the pitch fader applied.
///
/// Reported only when the deck column contains exactly one plausible tempo
/// field, so an unfamiliar layout yields "unknown" instead of another deck's
/// number.
fn tempo_readout(nodes: &[Node], column: &DeckColumn) -> Option<f64> {
    let label = column.label;
    let mut found = nodes.iter().filter(|node| {
        node.role == ROLE_TEXT_AREA
            && node.x >= label.x
            && node.x < column.right_bound
            && node.y > label.y
            && node.y < label.y + 400.0
            && parse_bpm(node.text()).is_some()
    });
    let first = found.next()?;
    if found.next().is_some() {
        return None;
    }
    parse_bpm(first.text())
}

/// `mm:ss` (or `h:mm:ss`) without a sign: `"00:41"` yes, `"-04:21"` (remaining) no.
fn parse_clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    let number = |part: &str, min_digits: usize, max_digits: usize| {
        let ok = (min_digits..=max_digits).contains(&part.len())
            && part.bytes().all(|b| b.is_ascii_digit());
        ok.then(|| part.parse::<f64>().ok()).flatten()
    };
    match parts.as_slice() {
        [minutes, seconds] => {
            let seconds = number(seconds, 2, 2).filter(|s| *s < 60.0)?;
            Some(number(minutes, 1, 3)? * 60.0 + seconds)
        }
        [hours, minutes, seconds] => {
            let seconds = number(seconds, 2, 2).filter(|s| *s < 60.0)?;
            let minutes = number(minutes, 2, 2).filter(|m| *m < 60.0)?;
            Some(number(hours, 1, 2)? * 3600.0 + minutes * 60.0 + seconds)
        }
        _ => None,
    }
}

/// The tenths cell rekordbox prints right after a clock (`".3"`).
fn parse_clock_fraction(text: &str) -> Option<f64> {
    let digits = text.trim().strip_prefix('.')?;
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    format!("0.{digits}").parse().ok()
}

/// The elapsed readout in the deck header band, with its tenths cell if present.
///
/// The remaining-time readout beside it carries a leading minus and is skipped.
/// Reported only when exactly one unsigned clock sits in the band, so an
/// unfamiliar layout yields "unknown" instead of another readout's time.
fn elapsed_readout(nodes: &[Node], column: &DeckColumn, title: &Node) -> Option<f64> {
    let mut found = nodes.iter().filter(|node| {
        node.role == ROLE_STATIC_TEXT
            && node.x > title.x
            && node.x < column.right_bound
            && node.y > title.y + 1.0
            && node.y <= title.y + 48.0
            && parse_clock(node.text()).is_some()
    });
    let clock = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let right_edge = clock.x + clock.width;
    let fraction = nodes
        .iter()
        .filter(|node| {
            node.role == ROLE_STATIC_TEXT
                && (node.y - clock.y).abs() <= 4.0
                && node.x >= right_edge - 6.0
                && node.x <= right_edge + 16.0
        })
        .filter_map(|node| parse_clock_fraction(node.text()).map(|value| (node.x, value)))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0.0, |(_, value)| value);
    Some(parse_clock(clock.text())? + fraction)
}

/// Stable identity of what the decks are showing, used to trigger refreshes.
///
/// Deliberately excludes `elapsed_seconds`, which changes every second.
pub fn signature(decks: &[DeckObservation]) -> String {
    decks
        .iter()
        .map(|deck| {
            format!(
                "{}:{}:{}:{}",
                deck.slot,
                deck.loaded,
                deck.title.as_deref().unwrap_or(""),
                deck.artist.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real rekordbox 7.2.18 two-deck horizontal layout, as captured from
    /// the live accessibility tree.
    fn rekordbox_two_deck_nodes() -> Vec<Node> {
        vec![
            Node::new(
                ROLE_POPUP_BUTTON,
                "2Deck Horizontal",
                135.0,
                65.0,
                40.0,
                20.0,
            ),
            Node::new(ROLE_POPUP_BUTTON, "PERFORMANCE", 10.0, 67.0, 110.0, 16.0),
            Node::new(ROLE_STATIC_TEXT, "19:45", 1418.0, 69.0, 38.0, 12.0),
            // Deck 1 header.
            Node::new(ROLE_STATIC_TEXT, "Bad Girl", 64.0, 241.0, 484.0, 18.0),
            Node::new(ROLE_STATIC_TEXT, "Usher", 64.0, 262.0, 100.0, 14.0),
            Node::new(ROLE_STATIC_TEXT, " 88.00", 179.0, 262.0, 44.0, 14.0),
            Node::new(ROLE_STATIC_TEXT, "Bm", 228.0, 262.0, 40.0, 14.0),
            Node::new(ROLE_STATIC_TEXT, "-04:21", 431.0, 260.0, 46.0, 16.0),
            Node::new(ROLE_STATIC_TEXT, ".5", 477.0, 260.0, 12.0, 16.0),
            Node::new(ROLE_STATIC_TEXT, "00:00", 489.0, 260.0, 43.0, 16.0),
            Node::new(ROLE_STATIC_TEXT, ".3", 532.0, 260.0, 12.0, 16.0),
            // Deck 2 header.
            Node::new(
                ROLE_STATIC_TEXT,
                "Think About You",
                800.0,
                241.0,
                484.0,
                18.0,
            ),
            Node::new(
                ROLE_STATIC_TEXT,
                "Kygo, Valerie Broussard",
                800.0,
                262.0,
                100.0,
                14.0,
            ),
            Node::new(ROLE_STATIC_TEXT, "  0.00", 915.0, 262.0, 44.0, 14.0),
            Node::new(ROLE_STATIC_TEXT, "6B", 964.0, 262.0, 40.0, 14.0),
            Node::new(ROLE_STATIC_TEXT, "-03:28", 1167.0, 260.0, 46.0, 16.0),
            Node::new(ROLE_STATIC_TEXT, "00:00", 1225.0, 260.0, 43.0, 16.0),
            // Deck number labels come after the headers in document order.
            Node::new(ROLE_STATIC_TEXT, "1", 8.0, 239.0, 12.0, 12.0),
            Node::new(ROLE_STATIC_TEXT, "2", 744.0, 239.0, 12.0, 12.0),
            // Platter tempo readouts.
            Node::new(ROLE_TEXT_AREA, " 88.00", 602.0, 374.0, 54.0, 23.0),
            Node::new(ROLE_TEXT_AREA, " 124.00", 824.0, 374.0, 54.0, 23.0),
            Node::new(ROLE_STATIC_TEXT, " 0.0", 592.0, 397.0, 26.0, 11.0),
            Node::new(ROLE_STATIC_TEXT, "±16", 631.0, 397.0, 32.0, 11.0),
            // Hot cue grid, which also uses single letters and times.
            Node::new(ROLE_STATIC_TEXT, "00:11", 63.0, 338.0, 29.0, 19.0),
            Node::new(ROLE_STATIC_TEXT, "A", 46.0, 342.0, 11.0, 11.0),
            Node::new(ROLE_STATIC_TEXT, "B", 46.0, 363.0, 11.0, 11.0),
        ]
    }

    #[test]
    fn moved_window_pads_and_missing_artist() {
        let mut nodes = rekordbox_two_deck_nodes();
        nodes.retain(|n| n.value != "Usher");
        nodes.push(Node::new(ROLE_STATIC_TEXT, "4", 46.0, 384.0, 11.0, 11.0));
        nodes.push(Node::new(ROLE_STATIC_TEXT, "3", 46.0, 342.0, 11.0, 11.0));
        for node in &mut nodes {
            node.x += 800.0;
        }
        let (decks, _) = parse_decks(&nodes, 2270.0);
        assert_eq!(decks.len(), 2);
        assert_eq!(decks[0].artist, None);
        assert_eq!(decks[0].track_bpm, Some(88.0));
        assert_eq!(decks[0].display_key.as_deref(), Some("Bm"));
        assert_eq!(decks[1].title.as_deref(), Some("Think About You"));
    }

    #[test]
    fn reads_both_decks_from_the_live_layout() {
        let (decks, warnings) = parse_decks(&rekordbox_two_deck_nodes(), 1470.0);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(decks.len(), 2);

        assert_eq!(decks[0].slot, 1);
        assert!(decks[0].loaded);
        assert_eq!(decks[0].title.as_deref(), Some("Bad Girl"));
        assert_eq!(decks[0].artist.as_deref(), Some("Usher"));
        assert_eq!(decks[0].track_bpm, Some(88.0));
        assert_eq!(decks[0].display_key.as_deref(), Some("Bm"));
        assert_eq!(decks[0].tempo_bpm, Some(88.0));

        assert_eq!(decks[1].slot, 2);
        assert_eq!(decks[1].title.as_deref(), Some("Think About You"));
        assert_eq!(decks[1].artist.as_deref(), Some("Kygo, Valerie Broussard"));
        // rekordbox has no stored tempo for this track and prints 0.00.
        assert_eq!(decks[1].track_bpm, None);
        assert_eq!(decks[1].display_key.as_deref(), Some("6B"));
        assert_eq!(decks[1].tempo_bpm, Some(124.0));
    }

    #[test]
    fn transport_times_are_never_mistaken_for_tempo_or_key() {
        let (decks, _) = parse_decks(&rekordbox_two_deck_nodes(), 1470.0);
        assert_ne!(decks[0].display_key.as_deref(), Some("-04:21"));
        assert_eq!(decks[0].track_bpm, Some(88.0));
        assert!(!is_bpm_cell("00:00"));
        assert!(!is_bpm_cell(".5"));
        assert!(!is_bpm_cell("±16"));
        assert!(!is_key_text("-04:21"));
        assert!(!is_key_text("00:00"));
    }

    #[test]
    fn an_empty_deck_is_reported_as_not_loaded_rather_than_guessed() {
        let mut nodes = rekordbox_two_deck_nodes();
        nodes.retain(|node| {
            !matches!(
                node.value.as_str(),
                "Think About You" | "Kygo, Valerie Broussard" | "  0.00" | "6B"
            )
        });
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks.len(), 2);
        assert!(decks[0].loaded);
        assert!(!decks[1].loaded);
        assert_eq!(decks[1].title, None);
        assert_eq!(decks[1].artist, None);
    }

    #[test]
    fn a_four_deck_row_splits_into_four_columns() {
        let mut nodes = Vec::new();
        for slot in 0..4u32 {
            let left = slot as f64 * 360.0;
            nodes.push(Node::new(
                ROLE_STATIC_TEXT,
                &(slot + 1).to_string(),
                left + 8.0,
                239.0,
                12.0,
                12.0,
            ));
            nodes.push(Node::new(
                ROLE_STATIC_TEXT,
                &format!("Track {}", slot + 1),
                left + 64.0,
                241.0,
                240.0,
                18.0,
            ));
            nodes.push(Node::new(
                ROLE_STATIC_TEXT,
                &format!("Artist {}", slot + 1),
                left + 64.0,
                262.0,
                80.0,
                14.0,
            ));
            nodes.push(Node::new(
                ROLE_STATIC_TEXT,
                &format!(" {}.00", 120 + slot),
                left + 160.0,
                262.0,
                44.0,
                14.0,
            ));
            nodes.push(Node::new(
                ROLE_STATIC_TEXT,
                "8A",
                left + 210.0,
                262.0,
                40.0,
                14.0,
            ));
        }
        let (decks, warnings) = parse_decks(&nodes, 1440.0);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(decks.len(), 4);
        for (index, deck) in decks.iter().enumerate() {
            assert_eq!(deck.slot, index as u32 + 1);
            assert_eq!(
                deck.title.as_deref(),
                Some(format!("Track {}", index + 1).as_str())
            );
            assert_eq!(deck.track_bpm, Some(120.0 + index as f64));
        }
    }

    #[test]
    fn a_layout_promising_more_decks_than_we_found_warns_instead_of_inventing_them() {
        let mut nodes = rekordbox_two_deck_nodes();
        nodes.retain(|node| node.value != "2Deck Horizontal");
        nodes.push(Node::new(
            ROLE_POPUP_BUTTON,
            "4Deck Horizontal",
            135.0,
            65.0,
            40.0,
            20.0,
        ));
        let (decks, warnings) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains('4'));
    }

    #[test]
    fn a_deck_with_no_labels_yields_no_decks() {
        let nodes = vec![Node::new(
            ROLE_STATIC_TEXT,
            "Bad Girl",
            64.0,
            241.0,
            484.0,
            18.0,
        )];
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert!(decks.is_empty());
    }

    #[test]
    fn signature_changes_when_a_deck_changes() {
        let nodes = rekordbox_two_deck_nodes();
        let (decks, _) = parse_decks(&nodes, 1470.0);
        let before = signature(&decks);
        let swapped: Vec<Node> = nodes
            .into_iter()
            .map(|mut node| {
                if node.value == "Bad Girl" {
                    node.value = "Other Track".into();
                }
                node
            })
            .collect();
        let (after_decks, _) = parse_decks(&swapped, 1470.0);
        assert_ne!(before, signature(&after_decks));
    }

    #[test]
    fn reads_the_elapsed_readout_and_not_the_remaining_one() {
        let (decks, _) = parse_decks(&rekordbox_two_deck_nodes(), 1470.0);
        // "00:00" + ".3"; the "-04:21" + ".5" beside it is the remaining time.
        assert_eq!(decks[0].elapsed_seconds, Some(0.3));
        // No tenths cell is printed for deck 2 in the capture.
        assert_eq!(decks[1].elapsed_seconds, Some(0.0));

        let mut nodes = rekordbox_two_deck_nodes();
        for node in &mut nodes {
            match (node.value.as_str(), node.x as i64) {
                ("00:00", 489) => node.value = "01:41".into(),
                (".3", 532) => node.value = ".7".into(),
                _ => {}
            }
        }
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks[0].elapsed_seconds, Some(101.7));
        assert_eq!(decks[1].elapsed_seconds, Some(0.0));
    }

    #[test]
    fn elapsed_ignores_other_clocks_and_stays_unknown_when_ambiguous() {
        // The hot cue time "00:11" sits far below the header band.
        let (decks, _) = parse_decks(&rekordbox_two_deck_nodes(), 1470.0);
        assert_ne!(decks[0].elapsed_seconds, Some(11.0));

        let mut nodes = rekordbox_two_deck_nodes();
        nodes.push(Node::new(ROLE_STATIC_TEXT, "03:12", 380.0, 260.0, 43.0, 16.0));
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks[0].elapsed_seconds, None);
        assert_eq!(decks[1].elapsed_seconds, Some(0.0));

        let mut nodes = rekordbox_two_deck_nodes();
        nodes.retain(|node| node.value != "00:00" || node.x < 1000.0);
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks[1].elapsed_seconds, None);
    }

    #[test]
    fn an_empty_deck_has_no_elapsed_readout() {
        let mut nodes = rekordbox_two_deck_nodes();
        nodes.retain(|node| {
            !matches!(
                node.value.as_str(),
                "Think About You" | "Kygo, Valerie Broussard" | "  0.00" | "6B"
            )
        });
        let (decks, _) = parse_decks(&nodes, 1470.0);
        assert_eq!(decks[1].elapsed_seconds, None);
    }

    #[test]
    fn clock_text_parsing_is_strict() {
        assert_eq!(parse_clock("00:41"), Some(41.0));
        assert_eq!(parse_clock(" 12:05 "), Some(725.0));
        assert_eq!(parse_clock("1:02:03"), Some(3723.0));
        for text in ["-04:21", "04:2", "04:61", "88.00", "4:21:5", "::", "", ".5"] {
            assert_eq!(parse_clock(text), None, "{text}");
        }
        assert_eq!(parse_clock_fraction(".3"), Some(0.3));
        assert_eq!(parse_clock_fraction(".35"), Some(0.35));
        for text in ["3", ".", ".123", ".x", "0.5"] {
            assert_eq!(parse_clock_fraction(text), None, "{text}");
        }
    }

    #[test]
    fn signature_ignores_the_elapsed_readout() {
        let nodes = rekordbox_two_deck_nodes();
        let (before, _) = parse_decks(&nodes, 1470.0);
        let advanced: Vec<Node> = nodes
            .into_iter()
            .map(|mut node| {
                if node.value == "00:00" && node.x < 1000.0 {
                    node.value = "02:30".into();
                }
                node
            })
            .collect();
        let (after, _) = parse_decks(&advanced, 1470.0);
        assert_ne!(before[0].elapsed_seconds, after[0].elapsed_seconds);
        assert_eq!(signature(&before), signature(&after));
    }

    #[test]
    fn camelot_and_classical_keys_are_both_accepted() {
        for key in ["6B", "10A", "1a", "Bm", "F#", "Ab maj", "C", "Gb minor"] {
            assert!(is_key_text(key), "expected {key} to parse as a key");
        }
        for text in ["13A", "0B", "Hm", "88.00", "", "Kygo"] {
            assert!(!is_key_text(text), "expected {text} not to parse as a key");
        }
    }
}
