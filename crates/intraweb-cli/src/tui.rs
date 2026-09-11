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
        Self { roster, identity, store, config, selected: ListState::default(), notice: None }
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
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
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
            Ok(true) => Some(format!("Marked {} as verified.", peer.nickname)),
            Ok(_) => Some("That peer is not in the keyring yet.".into()),
            Err(err) => Some(format!("Could not verify: {err}")),
        };
    }

    fn draw(&mut self, frame: &mut Frame, peers: &[Peer]) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(3)])
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

        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    "intraweb",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  your neighborhood web", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled(
                    self.config.sanitized_nickname(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}  ", role)),
                Span::styled(self.identity.fingerprint(), Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(Span::styled(
                format!(
                    "{} neighbor(s) online, {hubs} hub(s) in range",
                    peers.len().saturating_sub(hubs)
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ])
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
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
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
        Span::styled(peer.nickname.clone(), label_style),
        Span::styled(
            if peer.is_hub { "  [hub]" } else { "" },
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("  {address}  {}  {}s ago", peer.fingerprint, now.saturating_sub(peer.last_seen)),
            Style::default().fg(Color::DarkGray),
        ),
    ])];

    if peer.trust == TrustState::NicknameConflict {
        lines.push(Line::from(Span::styled(
            format!("    not the \"{}\" you met before -- different key, same name", peer.nickname),
            Style::default().fg(Color::Red),
        )));
    }

    ListItem::new(lines)
}
