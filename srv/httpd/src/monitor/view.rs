//! Templates, widgets and the stylesheet, all compiled into the binary.
//!
//! A widget is a view model with a template of the same name whose root
//! element carries a stable id; Datastar patches replace it by that id.

use std::sync::LazyLock;

use chaos_snitch::tracing;
use minijinja::value::Serde;
use minijinja::{Environment, UndefinedBehavior, Value};
use rama::error::BoxError;
use rama::http::sse::Event;
use rama::http::sse::datastar::{ElementPatchMode, PatchElements};
use rama::utils::str::NonEmptyStr;
use serde::Serialize;
use sha2::{Digest, Sha256};

macro_rules! sources {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("templates/", $name)))),*]
    };
}

const TEMPLATES: &[(&str, &str)] = sources![
    "layout.html",
    "page.html",
    "partials/ui.html",
    "partials/icons.html",
    "widgets/dashboard.html",
    "widgets/summary.html",
    "widgets/machine.html",
    "widgets/instruments.html",
    "widgets/map.html",
    "widgets/attention.html",
    "widgets/sessions.html",
    "widgets/session.html",
    "widgets/feed.html",
    "widgets/entry.html",
];

/// Shared tokens and layout first, then one file per widget.
const STYLES: &[&str] = &[
    include_str!("assets/tokens.css"),
    include_str!("assets/base.css"),
    include_str!("assets/widgets/summary.css"),
    include_str!("assets/widgets/machine.css"),
    include_str!("assets/widgets/instruments.css"),
    include_str!("assets/widgets/map.css"),
    include_str!("assets/widgets/attention.css"),
    include_str!("assets/widgets/session.css"),
    include_str!("assets/widgets/feed.css"),
];

static ENV: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Booleans display as `True`; data attributes and CSS selectors want `true`.
    env.add_filter("flag", |value: bool| if value { "true" } else { "false" });
    for (name, source) in TEMPLATES {
        if let Err(error) = env.add_template(name, source) {
            tracing::error!(template = name, %error, "monitor template does not parse");
        }
    }
    env
});

pub(super) struct Stylesheet {
    pub css: String,
    pub version: String,
}

pub(super) static STYLESHEET: LazyLock<Stylesheet> = LazyLock::new(|| {
    let css = STYLES.join("\n");
    let digest = Sha256::digest(css.as_bytes());
    let version = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Stylesheet { css, version }
});

pub(super) trait Widget: Serialize {
    /// Template `widgets/{NAME}.html`, rendered with this value as `{NAME}`.
    const NAME: &'static str;

    fn render(&self) -> Result<String, minijinja::Error> {
        let context = Value::from_pairs([(Self::NAME, Value::from(Serde(self)))]);
        ENV.get_template(&format!("widgets/{}.html", Self::NAME))?
            .render(context)
    }
}

pub(super) fn page() -> Result<String, minijinja::Error> {
    ENV.get_template("page.html")?
        .render(minijinja::context! { css_version => STYLESHEET.version.as_str() })
}

pub(super) type Patch = Result<Event<PatchElements>, BoxError>;

/// Replace the widget whose id the rendered root carries.
pub(super) fn replace(widget: &impl Widget) -> Patch {
    let html = widget.render()?;
    Ok(PatchElements::new(NonEmptyStr::try_from(html)?).try_into_sse_event()?)
}

/// Put a rendered widget first inside `selector`.
pub(super) fn prepend(selector: &'static str, widget: &impl Widget) -> Patch {
    let html = widget.render()?;
    Ok(PatchElements::new(NonEmptyStr::try_from(html)?)
        .with_selector(NonEmptyStr::try_from(selector)?)
        .with_mode(ElementPatchMode::Prepend)
        .try_into_sse_event()?)
}

/// The serialized name of a contract enum, such as a station.
pub(super) fn name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
