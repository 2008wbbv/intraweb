//! The terminal dashboard.
//!
//! This reads the same `Roster` the JSON API serializes, rather than making
//! HTTP calls to itself. The single-source-of-truth promise is about the roster
//! being one object, not about forcing every reader through a socket.

use anyhow::Result;
use intraweb_core::peer::{Peer, TrustState, now_secs};
use intraweb_net::Roster;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, crossterm};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use intraweb_core::{Config, Identity, Store};

/// How often the roster is redrawn. Also the event poll timeout, so a keypress
/// is never waiting on the next tick.
const TICK: Duration = Duration::from_millis(250);

/// Lines the header draws, and the rows it needs once borders are added.
/// These drifted apart once and silently clipped the roster counts off the
/// bottom of the header, so the relationship is asserted in the tests.
const HEADER_LINES: usize = 3;
const HEADER_HEIGHT: u16 = HEADER_LINES as u16 + 2;

pub struct Dashboard {
    roster: Roster,
    identity: Arc<Identity>,
    store: Arc<Mutex<Store>>,
    config: Arc<Config>,
    selected: ListState,
    notice: Option<String>,
}

impl Dashboard {
    pub fn new(
        roster: Roster,
        identity: Arc<Identity>,
        store: Arc<Mutex<Store>>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            roster,
            identity,
            store,
            config,
            selected: ListState::default(),
            notice: None,
        }
    }

    /// Take over the terminal until the operator quits. Blocking by design;
    /// the caller runs it off the async runtime's worker threads.
    pub fn run(mut self) -> Result<()> {
        let mut terminal = ratatui::init();
        let outcome = self.event_loop(&mut terminal);
        ratatui::restore();
        outcome
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            let peers = self.roster.peers();
            terminal.draw(|frame| self.draw(frame, &peers))?;

            if !event::poll(TICK)? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c')
                    if key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    return Ok(());
                }
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, peers.len()),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, peers.len()),
                KeyCode::Char('v') => self.verify_selected(&peers),
                _ => {}
            }
        }
    }

    fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected.select(None);
            return;
        }
        let current = self.selected.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.selected.select(Some(next));
    }

    /// Record that the operator compared fingerprints out of band.
    fn verify_selected(&mut self, peers: &[Peer]) {
        let Some(peer) = self.selected.selected().and_then(|i| peers.get(i)) else {
            self.notice = Some("Select a neighbor first.".into());
            return;
        };
        let Ok(store) = self.store.lock() else {
            self.notice = Some("The keyring is busy.".into());
            return;
        };
        self.notice = match store.mark_verified(peer.peer_id) {
            Ok(true) => {
                // Show it at once instead of waiting for the next write-through.
                self.roster.note_verified(peer.peer_id);
                Some(format!("Marked {} as verified.", peer.nickname))
            }
            Ok(_) => Some("That peer is not in the keyring yet.".into()),
            Err(err) => Some(format!("Could not verify: {err}")),
        };
    }

    fn draw(&mut self, frame: &mut Frame, peers: &[Peer]) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(HEADER_HEIGHT),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(frame.area());

        frame.render_widget(self.header(peers), header);
        self.draw_roster(frame, body, peers);
        frame.render_widget(self.footer(), footer);
    }

    fn header(&self, peers: &[Peer]) -> Paragraph<'static> {
        let hubs = peers.iter().filter(|p| p.is_hub).count();
        let role = if self.config.hub {
            format!("hosting \"{}\"", self.config.hub_name)
        } else {
            "peer".to_string()
        };

        let unread = self
            .store
            .lock()
            .ok()
            .and_then(|store| store.unread_count().ok())
            .unwrap_or(0);

        Paragraph::new(header_lines(
            &self.config.sanitized_nickname(),
            &role,
            &self.identity.fingerprint(),
            peers.len(),
            hubs,
            unread,
        ))
        .block(Block::bordered())
    }

    fn draw_roster(&mut self, frame: &mut Frame, area: ratatui::layout::Rect, peers: &[Peer]) {
        if peers.is_empty() {
            let empty = Paragraph::new(
                "Nobody in range yet.\n\n\
                 If you expected company, quit and run `intraweb doctor` -- most missing \
                 neighbors are the access point isolating clients, not a broken node.",
            )
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::bordered().title(" Neighborhood "));
            frame.render_widget(empty, area);
            return;
        }

        let now = now_secs();
        let items: Vec<ListItem> = peers.iter().map(|peer| roster_row(peer, now)).collect();

        let list = List::new(items)
            .block(Block::bordered().title(" Neighborhood "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.selected);
    }

    fn footer(&self) -> Paragraph<'static> {
        let text = self.notice.clone().unwrap_or_else(|| {
            "j/k move   v mark verified after comparing fingerprints   q quit".to_string()
        });
        Paragraph::new(text)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true })
            .block(Block::bordered())
    }
}

/// The header's contents. Must always be exactly `HEADER_LINES` lines.
fn header_lines(
    nickname: &str,
    role: &str,
    fingerprint: &str,
    peers: usize,
    hubs: usize,
    unread: u64,
) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(
                "intraweb",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  your neighborhood web",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                nickname.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {role}  ")),
            Span::styled(
                fingerprint.to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(
                    "{} neighbor(s) online, {hubs} hub(s) in range",
                    peers.saturating_sub(hubs)
                ),
                Style::default().fg(Color::DarkGray),
            ),
            // Only worth the space when there is actually something to read.
            Span::styled(
                if unread > 0 {
                    format!("   {unread} unread")
                } else {
                    String::new()
                },
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ]
}

/// Column widths chosen so a long nickname pushes its own row out rather than
/// knocking the whole list out of alignment.
const NICK_WIDTH: usize = 16;
const ADDR_WIDTH: usize = 17;

/// Pad to a column width, letting anything longer simply overflow.
fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return format!("{text} ");
    }
    format!("{text}{}", " ".repeat(width - len))
}

fn roster_row(peer: &Peer, now: u64) -> ListItem<'static> {
    let (marker, marker_style) = match peer.trust {
        // The one case worth shouting about: a known name on an unknown key.
        TrustState::NicknameConflict => (
            "!",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        TrustState::Verified => ("*", Style::default().fg(Color::Green)),
        _ => (" ", Style::default()),
    };

    let label_style = if peer.is_hub {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };

    let address = peer
        .preferred_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "no address".to_string());

    let mut lines = vec![Line::from(vec![
        Span::styled(marker, marker_style),
        Span::raw(" "),
        Span::styled(pad(&peer.nickname, NICK_WIDTH), label_style),
        Span::styled(
            pad(if peer.is_hub { "[hub]" } else { "" }, 7),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(
                "{}{}  {}s ago",
                pad(&address, ADDR_WIDTH),
                peer.fingerprint,
                now.saturating_sub(peer.last_seen),
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ])];

    if peer.trust == TrustState::NicknameConflict {
        lines.push(Line::from(Span::styled(
            format!(
                "    not the \"{}\" you met before -- different key, same name",
                peer.nickname
            ),
            Style::default().fg(Color::Red),
        )));
    }

    ListItem::new(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the header block was sized at four rows while drawing three
    /// lines, so "N neighbor(s) online" was clipped away with no error anywhere.
    #[test]
    fn the_header_block_is_tall_enough_for_what_it_draws() {
        let drawn = header_lines("carol", "peer", "844c-8e7b-9dfd-5d0a", 3, 1, 0);

        assert_eq!(drawn.len(), HEADER_LINES);
        assert!(
            HEADER_HEIGHT as usize >= drawn.len() + 2,
            "header needs a row per line plus two borders",
        );
    }

    #[test]
    fn the_counts_line_excludes_hubs_from_the_neighbor_total() {
        let drawn = header_lines("carol", "peer", "fp", 3, 1, 0);
        let counts: String = drawn[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(counts, "2 neighbor(s) online, 1 hub(s) in range");
    }

    #[test]
    fn the_counts_line_never_underflows() {
        // More hubs than peers should be impossible, but saturating here is
        // cheaper than a panic in a render loop.
        let drawn = header_lines("carol", "peer", "fp", 0, 2, 0);
        let counts: String = drawn[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert!(counts.starts_with("0 neighbor(s)"));
    }

    #[test]
    fn unread_mail_is_announced_only_when_there_is_some() {
        let quiet: String = header_lines("carol", "peer", "fp", 1, 0, 0)[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!quiet.contains("unread"), "an empty inbox needs no mention");

        let waiting: String = header_lines("carol", "peer", "fp", 1, 0, 3)[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(waiting.contains("3 unread"));
    }

    #[test]
    fn columns_pad_to_width_so_fingerprints_line_up() {
        assert_eq!(pad("bob", 8), "bob     ");
        assert_eq!(pad("", 4), "    ");
    }

    #[test]
    fn an_overlong_nickname_pushes_only_its_own_row() {
        // One long name must not silently truncate; it just costs a space.
        let long = "a".repeat(20);
        assert_eq!(pad(&long, NICK_WIDTH), format!("{long} "));
    }
}
