use super::*;

#[test]
fn partial_overrides_and_round_trip() {
    let appearance: Appearance = toml::from_str(
        "[colors]\nfg = '#aAbBcC'\nbg = 'default'\n[user]\nbold = true\n[assistant]\nitalic = false",
    ).unwrap();
    assert_eq!(appearance.colors.fg.as_ref().unwrap().as_str(), "#aAbBcC");
    assert_eq!(appearance.user.bold, Some(true));
    assert_eq!(appearance.assistant.italic, Some(false));
    assert_eq!(appearance.user.italic, None);
    assert_eq!(
        appearance,
        toml::from_str(&toml::to_string(&appearance).unwrap()).unwrap()
    );
    assert_eq!(
        toml::from_str::<Appearance>("").unwrap(),
        Appearance::default()
    );
}

#[test]
fn rejects_invalid_colors_and_unknown_fields() {
    // Renderer aliases must not widen the public configuration vocabulary.
    for value in [
        "#fff",
        "#gg0000",
        "orange",
        "\u{1b}[31m",
        "#ééé",
        "Blue",
        "bright_blue",
        "light-blue",
        "reset",
        "12",
        " blue ",
    ] {
        assert!(ThemeColor::try_from(value.to_owned()).is_err(), "{value}");
    }
    for source in [
        "[colors]\nfg = 123",
        "[colors]\nforeground = 'blue'",
        "[user]\nfont = 'Mono'",
        "[assistant]\nbold = 'yes'",
        "unknown = true",
    ] {
        assert!(toml::from_str::<Appearance>(source).is_err(), "{source}");
    }
    for value in ["blue", "light_blue", "default", "#00FF00"] {
        assert!(ThemeColor::try_from(value.to_owned()).is_ok());
    }
}
