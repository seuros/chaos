use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let out_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "sys/kern/kern/config.schema.json".to_string()),
    );
    chaos_kern::config::schema::write_config_schema(&out_path)?;
    println!("Written: {}", out_path.display());
    Ok(())
}
