# chaos-check

Proc macro that runs tests on a 16 MB stack thread. Handles both
sync and async (Tokio) test bodies. Strips `#[tokio::test]` and
wires up the runtime internally.
