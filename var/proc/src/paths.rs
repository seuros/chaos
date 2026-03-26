use std::path::Path;

pub(crate) async fn file_modified_time_utc(path: &Path) -> Option<jiff::Timestamp> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    let ts = jiff::Timestamp::try_from(modified).ok()?;
    Some(jiff::Timestamp::from_second(ts.as_second()).unwrap_or(ts))
}
