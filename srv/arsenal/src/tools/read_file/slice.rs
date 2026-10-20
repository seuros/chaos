use super::format_line;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

pub async fn read(path: &Path, offset: usize, limit: usize) -> Result<Vec<String>, String> {
    let file = File::open(path)
        .await
        .map_err(|err| format!("failed to read file: {err}"))?;
    read_from(BufReader::new(file), offset, limit).await
}

pub(super) async fn read_from(
    mut reader: impl AsyncBufRead + Unpin,
    offset: usize,
    limit: usize,
) -> Result<Vec<String>, String> {
    let mut collected = Vec::new();
    let mut seen = 0usize;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|err| format!("failed to read file: {err}"))?;
        if bytes_read == 0 {
            break;
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
        }
        seen += 1;
        if seen < offset {
            continue;
        }
        if collected.len() == limit {
            break;
        }
        let formatted = format_line(&buffer);
        collected.push(format!("L{seen}: {formatted}"));
        if collected.len() == limit {
            break;
        }
    }
    if seen < offset {
        return Err("offset exceeds file length".to_string());
    }
    Ok(collected)
}
