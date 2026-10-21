use super::*;

#[test]
fn initialization_and_cleanup_are_once_only_and_terminal() {
    let mut connection = Connection::default();
    assert!(!connection.is_open());
    assert!(connection.initialize());
    assert!(!connection.initialize());
    assert!(connection.is_open());
    connection.close();
    connection.close();
    assert!(!connection.is_open());
    assert!(!connection.initialize());
    assert!(connection.begin_cleanup());
    assert!(!connection.begin_cleanup());
    connection.finish_cleanup(true);
    connection.finish_cleanup(false);
    connection.close();
    assert!(connection.cleanup_confirmed());
    assert!(!connection.begin_cleanup());
    assert!(!connection.initialize());
}

#[test]
fn cleanup_before_initialization_and_unconfirmed_cleanup_cannot_reopen() {
    let mut connection = Connection::default();
    assert!(connection.begin_cleanup());
    assert!(!connection.initialize());
    connection.finish_cleanup(false);
    connection.finish_cleanup(true);
    connection.close();
    assert!(!connection.cleanup_confirmed());
    assert!(!connection.begin_cleanup());
    assert!(!connection.initialize());
}
