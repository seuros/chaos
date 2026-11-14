use super::*;
use crate::monitor::board::Board;
use crate::monitor::widgets::{
    Attention, Dashboard, Feed, Instruments, Machine, Map, Sessions, Summary,
};
use chaos_ipc::ProcessId;
use chaos_kern::activity::{Activity, ActivityEvent, Station};

fn busy_board() -> (Board, ProcessId) {
    let mut board = Board::default();
    let id = ProcessId::new();
    for activity in [
        Activity::TurnStarted,
        Activity::ModelStarted {
            request: 1,
            model: "model-a".into(),
        },
        Activity::CallStarted {
            call_id: "call-1".into(),
            station: Station::Network,
            label: "<curl>".into(),
            check: None,
        },
    ] {
        board.apply(&ActivityEvent {
            process_id: id,
            at_ms: 1,
            activity,
        });
    }
    (board, id)
}

fn summary() -> Summary {
    Summary {
        connected: true,
        version: "47.0.0",
        uptime: "5s".into(),
        triggers: 1,
        max_triggers: 4,
        sessions: 1,
        working: 1,
        waiting: 0,
    }
}

#[test]
fn every_template_parses() {
    for (name, _) in TEMPLATES {
        ENV.get_template(name)
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
    }
}

#[test]
fn the_dashboard_composes_every_widget_under_its_id() {
    let (board, id) = busy_board();
    let html = Dashboard {
        summary: summary(),
        machine: Machine::from(&board),
        instruments: Instruments::from(&board, 2_000),
        map: Map::from(&board, 2_000),
        attention: Attention::from(&board, 2_000),
        sessions: Sessions::from(&board),
        feed: Feed::from(&board),
    }
    .render()
    .unwrap();
    for anchor in [
        r#"id="dashboard""#,
        r#"id="w-summary""#,
        r#"id="w-machine""#,
        r#"id="w-instruments""#,
        r#"id="w-map""#,
        r#"id="w-attention""#,
        r#"id="w-sessions""#,
        r#"id="w-feed""#,
        &format!(r#"id="s-{id}""#),
    ] {
        assert!(html.contains(anchor), "{anchor} missing:\n{html}");
    }
    assert!(html.contains(r#"data-state="working""#), "{html}");
    assert!(html.contains(r#"data-station="network""#), "{html}");
    assert!(
        html.contains(r#"class="node" data-station="network" data-active="true""#),
        "the call in flight lights its station:\n{html}"
    );
    assert!(
        html.contains(r#"begin="-0.40s""#),
        "a packet picks up where the clock says it is:\n{html}"
    );
    assert!(html.contains("no warnings"), "{html}");
}

#[test]
fn a_minion_card_names_its_parent_and_orbits_the_core() {
    let mut board = Board::default();
    let parent = ProcessId::new();
    let child = ProcessId::new();
    for (process_id, activity) in [
        (parent, Activity::TurnStarted),
        (
            parent,
            Activity::AgentSpawned {
                child,
                nickname: Some("Ada".into()),
                role: Some("reviewer".into()),
            },
        ),
        (child, Activity::TurnStarted),
    ] {
        board.apply(&ActivityEvent {
            process_id,
            at_ms: 1,
            activity,
        });
    }
    let html = Sessions::from(&board).render().unwrap();
    let parent_card = html.find(&format!(r#"id="s-{parent}""#)).unwrap();
    let child_card = html.find(&format!(r#"id="s-{child}""#)).unwrap();
    assert!(parent_card < child_card, "a minion follows its parent");
    assert!(html.contains("reviewer minion of"), "{html}");
    assert!(html.contains(r#"class="minion-chip""#), "{html}");
    let map = Map::from(&board, 2_000).render().unwrap();
    assert!(map.contains(r#"class="orbit""#), "{map}");
}

#[test]
fn labels_are_escaped() {
    let (board, _) = busy_board();
    let html = Sessions::from(&board).render().unwrap();
    assert!(html.contains("&lt;curl&gt;"), "{html}");
    assert!(!html.contains("<curl>"), "{html}");
}

#[test]
fn the_page_links_the_stylesheet_of_this_build() {
    let html = page().unwrap();
    assert!(
        html.contains(&format!("/monitor/app.css?v={}", STYLESHEET.version)),
        "{html}"
    );
    assert!(html.contains(r#"id="dashboard""#), "{html}");
}

#[test]
fn the_stylesheet_carries_tokens_before_widgets() {
    let css = &STYLESHEET.css;
    let tokens = css.find("--accent:").unwrap();
    let session = css.find(".session-grid").unwrap();
    assert!(tokens < session);
    assert_eq!(STYLESHEET.version.len(), 12);
}
