use super::*;

#[tokio::test]
async fn a_closed_pause_channel_does_not_extend_the_deadline() {
    let (sender, receiver) = watch::channel(false);
    // A final, unseen update must not keep execution paused after the owner exits.
    sender.send(true).unwrap();
    drop(sender);
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut collection = Collection::new(deadline, Some(receiver));
    collection.wait_while_paused().await;
    assert!(collection.pause_receiver().is_none());
    assert_eq!(collection.deadline(), deadline);
}
