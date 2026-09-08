//! Fork-only: end-to-end cover for the status line as client chrome.
//!
//! These drive the real `compose()` path — layout, render, hit collection, and
//! mouse routing — rather than the pure builder, so they catch wiring mistakes
//! that the unit tests in `client/shell/statusline.rs` cannot see.

use super::*;
use crate::config::{StatusSegment, StatusWidget};

fn statusline_config() -> ClientShellConfig {
    let mut config = Config::default();
    config.ui.statusline.enabled = true;
    config.ui.statusline.left = vec![
        StatusSegment::Widget {
            widget: StatusWidget::Menu,
        },
        StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        },
    ];
    config.ui.statusline.right = vec![StatusSegment::Text("#{session}".into())];
    ClientShellConfig::from_config(&config)
}

fn two_workspace_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.active_tab_id = "tab_2".into();
    second.number = 2;
    second.label = "second-space".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    snapshot
}

/// Read row `y` of a composed frame back as a string.
fn row_text(frame: &FrameData, y: u16) -> String {
    let width = usize::from(frame.width);
    let start = usize::from(y) * width;
    frame.cells[start..start + width]
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

#[test]
fn statusline_is_drawn_on_its_own_row_and_registers_hits() {
    let mut state = ClientShellState::new(statusline_config());
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(80, 20).expect("composed frame");

    // Bottom position: the bar owns the last row.
    let bar_row = row_text(&frame, 19);
    assert!(bar_row.contains('☰'), "menu button missing: {bar_row:?}");
    assert!(
        bar_row.contains("client-shell"),
        "active space chip missing: {bar_row:?}"
    );
    assert!(
        bar_row.contains("second-space"),
        "second space chip missing: {bar_row:?}"
    );

    assert_eq!(state.hits.statusline.bar, Rect::new(0, 19, 80, 1));
    assert_ne!(state.hits.statusline.menu_button, Rect::default());
    assert_eq!(state.hits.statusline.workspace_entries.len(), 2);
    assert!(state.hits.statusline.has_workspaces_widget);
    // Every hit sits on the bar's row, not on the chrome above it.
    for hit in &state.hits.statusline.workspace_entries {
        assert_eq!(hit.rect.y, 19, "hit rect drifted off the bar row");
    }
}

#[test]
fn statusline_at_the_top_pushes_the_rest_of_the_shell_down() {
    let mut config = statusline_config();
    config.statusline.position = StatusLinePosition::Top;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(80, 20).expect("composed frame");

    assert!(row_text(&frame, 0).contains('☰'));
    assert_eq!(state.hits.statusline.bar, Rect::new(0, 0, 80, 1));
    // The sidebar starts below the bar, so its hits never collide with it.
    for hit in &state.hits.workspaces {
        assert!(hit.rect.y >= 1, "sidebar drew over the status line");
    }
}

#[test]
fn clicking_a_status_line_workspace_chip_focuses_that_workspace() {
    let mut state = ClientShellState::new(statusline_config());
    // Endpoint methods are dropped unless the active endpoint is online.
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("composed frame");

    let hit = state
        .hits
        .statusline
        .workspace_entries
        .iter()
        .find(|hit| hit.workspace_id == "ws_2")
        .cloned()
        .expect("second workspace chip");

    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x + 1,
        row: hit.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);

    let focused = outcome.actions.iter().any(|action| {
        matches!(
            action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    &request.method,
                    crate::api::schema::Method::WorkspaceFocus(target)
                        if target.workspace_id == "ws_2"
                )
        )
    });
    assert!(
        focused,
        "chip click did not focus ws_2: {} action(s)",
        outcome.actions.len()
    );
}

#[test]
fn clicking_the_status_line_menu_button_opens_the_menu_anchored_there() {
    let mut state = ClientShellState::new(statusline_config());
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("composed frame");

    let button = state.hits.statusline.menu_button;
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: button.x + 1,
        row: button.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.repaint);
    assert!(state.global_menu_anchored_at(ClientGlobalMenuAnchor::Statusline));
    // The sidebar launcher must not claim the menu the bar opened.
    assert!(!state.global_menu_anchored_at(ClientGlobalMenuAnchor::Launcher));
}

#[test]
fn a_click_on_the_bar_never_reaches_the_pane_underneath() {
    let mut state = ClientShellState::new(statusline_config());
    // Online, so a leak would actually produce a request for us to catch.
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("composed frame");

    // Far right of the bar: inert chrome, no widget there.
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 79,
        row: 19,
        modifiers: KeyModifiers::empty(),
    })]);
    // Nothing is sent at all: no pane input, no endpoint action.
    assert!(
        outcome.requests.is_empty(),
        "bar click leaked {} request(s) to the endpoint",
        outcome.requests.len()
    );
    assert!(
        outcome.actions.is_empty(),
        "bar click leaked {} action(s) to the endpoint",
        outcome.actions.len()
    );
}

#[test]
fn disabling_the_bar_returns_its_row_to_the_pane_surface() {
    let with_bar = ClientShellState::new(statusline_config());
    let without_bar = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    assert_eq!(
        with_bar.surface_size(80, 20).rows + 1,
        without_bar.surface_size(80, 20).rows,
        "the bar must cost the pane surface exactly one row"
    );
}

/// An open overlay owns the mouse. The bar's own menu button must still toggle
/// the menu closed, via upstream's click-outside dismissal rather than the
/// bar's handler — otherwise a chip click would focus a space and leave the
/// menu stranded on screen.
#[test]
fn a_bar_click_while_the_menu_is_open_dismisses_it_instead_of_acting() {
    let mut state = ClientShellState::new(statusline_config());
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("composed frame");

    // Open the menu from the bar's button.
    let button = state.hits.statusline.menu_button;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: button.x + 1,
        row: button.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.global_menu_anchored_at(ClientGlobalMenuAnchor::Statusline));
    state.compose(80, 20).expect("composed frame with menu");

    // Clicking a workspace chip now dismisses the menu and does NOT focus.
    let hit = state
        .hits
        .statusline
        .workspace_entries
        .iter()
        .find(|hit| hit.workspace_id == "ws_2")
        .cloned()
        .expect("second workspace chip");
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x + 1,
        row: hit.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.overlay.is_none(), "menu was left stranded on screen");
    assert!(
        outcome.actions.is_empty(),
        "the dismissing click also acted on the bar"
    );
}

/// Clicking the bar's menu button a second time closes the menu.
#[test]
fn the_bar_menu_button_toggles_closed() {
    let mut state = ClientShellState::new(statusline_config());
    state.set_snapshot(Box::new(two_workspace_snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("composed frame");

    let button = state.hits.statusline.menu_button;
    let click = |state: &mut ClientShellState| {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x + 1,
            row: button.y,
            modifiers: KeyModifiers::empty(),
        })]);
    };
    click(&mut state);
    assert!(state.global_menu_anchored_at(ClientGlobalMenuAnchor::Statusline));
    state.compose(80, 20).expect("composed frame with menu");
    click(&mut state);
    assert!(state.overlay.is_none(), "menu button did not toggle closed");
}

/// Command segments must run in the workspace's directory, not the path the
/// `terminal.new_cwd` policy would hand a *new* pane.
#[test]
fn command_cwd_follows_the_workspace_not_the_new_pane_policy() {
    let mut snapshot = two_workspace_snapshot();
    // What `new_cwd = "home"` would put in the policy-resolved field.
    for workspace in &mut snapshot.workspaces {
        workspace.new_workspace_cwd = "/home/someone".into();
    }
    snapshot.panes[0].cwd = Some("/repo".into());
    snapshot.panes[0].foreground_cwd = Some("/repo/crates/inner".into());

    let cwd = crate::client::shell::statusline::statusline_command_cwd(&snapshot)
        .expect("a workspace cwd");
    assert_eq!(
        cwd,
        std::path::PathBuf::from("/repo/crates/inner"),
        "command cwd fell back to the new-pane policy path"
    );
}
