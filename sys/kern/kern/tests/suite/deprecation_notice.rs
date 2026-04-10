use anyhow::Ok;
use chaos_ipc::api::ConfigLayerSource;
use chaos_ipc::protocol::DeprecationNoticeEvent;
use chaos_ipc::protocol::EventMsg;
use chaos_kern::config_loader::ConfigLayerEntry;
use chaos_kern::config_loader::ConfigLayerStack;
use chaos_kern::config_loader::ConfigRequirements;
use chaos_kern::config_loader::ConfigRequirementsToml;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_absolute_path;
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use toml::Value as TomlValue;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_deprecation_notice_for_experimental_instructions_file() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_chaos().with_config(|config| {
        let mut table = toml::map::Map::new();
        table.insert(
            "experimental_instructions_file".to_string(),
            TomlValue::String("legacy.md".to_string()),
        );
        let config_layer = ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: test_absolute_path("/tmp/config.toml"),
            },
            TomlValue::Table(table),
        );
        let config_layer_stack = ConfigLayerStack::new(
            vec![config_layer],
            ConfigRequirements::default(),
            ConfigRequirementsToml::default(),
        )
        .expect("build config layer stack");
        config.config_layer_stack = config_layer_stack;
    });

    let TestChaos { process: chaos, .. } = builder.build(&server).await?;

    let notice = wait_for_event_match(&chaos, |event| match event {
        EventMsg::DeprecationNotice(ev)
            if ev.summary.contains("experimental_instructions_file") =>
        {
            Some(ev.clone())
        }
        _ => None,
    })
    .await;

    let DeprecationNoticeEvent { summary, details } = notice;
    assert_eq!(
        summary,
        "`experimental_instructions_file` is deprecated and ignored. Use `model_instructions_file` instead."
            .to_string(),
    );
    assert_eq!(
        details.as_deref(),
        Some(
            "Move the setting to `model_instructions_file` in config.toml (or under a profile) to load instructions from a file."
        ),
    );

    Ok(())
}
