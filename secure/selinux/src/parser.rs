use chaos_realpath::AbsolutePathBuf;
use mlua::{Lua, Table, Value};
use multimap::MultiMap;
use shlex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::decision::Decision;
use crate::error::Error;
use crate::error::ErrorLocation;
use crate::error::Result;
use crate::executable_name::executable_lookup_key;
use crate::executable_name::executable_path_lookup_key;
use crate::rule::NetworkRule;
use crate::rule::NetworkRuleProtocol;
use crate::rule::PatternToken;
use crate::rule::PrefixPattern;
use crate::rule::PrefixRule;
use crate::rule::RuleRef;
use crate::rule::validate_match_examples;
use crate::rule::validate_not_match_examples;

/// Globals stripped from the policy Lua VM. Policy files are config, not scripts.
const DANGEROUS_GLOBALS: &[&str] = &[
    "os",
    "io",
    "debug",
    "package",
    "loadfile",
    "dofile",
    "collectgarbage",
    "require",
    "load",
];

pub struct PolicyParser {
    builder: RefCell<PolicyBuilder>,
}

impl Default for PolicyParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyParser {
    pub fn new() -> Self {
        Self {
            builder: RefCell::new(PolicyBuilder::new()),
        }
    }

    /// Parses a policy, tagging parser errors with `policy_identifier` so failures include the
    /// identifier alongside line numbers.
    pub fn parse(&mut self, policy_identifier: &str, policy_file_contents: &str) -> Result<()> {
        let pending_validation_count = self.builder.borrow().pending_example_validations.len();

        let lua = Lua::new();
        strip_dangerous_globals(&lua).map_err(|e| Error::Lua(e.to_string()))?;

        register_policy_builtins_and_eval(
            &lua,
            &self.builder,
            policy_identifier,
            policy_file_contents,
        )?;

        self.builder
            .borrow()
            .validate_pending_examples_from(pending_validation_count)?;
        Ok(())
    }

    pub fn build(self) -> crate::policy::Policy {
        self.builder.into_inner().build()
    }
}

#[derive(Debug)]
struct PolicyBuilder {
    rules_by_program: MultiMap<String, RuleRef>,
    network_rules: Vec<NetworkRule>,
    host_executables_by_name: HashMap<String, Arc<[AbsolutePathBuf]>>,
    pending_example_validations: Vec<PendingExampleValidation>,
}

impl PolicyBuilder {
    fn new() -> Self {
        Self {
            rules_by_program: MultiMap::new(),
            network_rules: Vec::new(),
            host_executables_by_name: HashMap::new(),
            pending_example_validations: Vec::new(),
        }
    }

    fn add_rule(&mut self, rule: RuleRef) {
        self.rules_by_program
            .insert(rule.program().to_string(), rule);
    }

    fn add_network_rule(&mut self, rule: NetworkRule) {
        self.network_rules.push(rule);
    }

    fn add_host_executable(&mut self, name: String, paths: Vec<AbsolutePathBuf>) {
        self.host_executables_by_name.insert(name, paths.into());
    }

    fn add_pending_example_validation(
        &mut self,
        rules: Vec<RuleRef>,
        matches: Vec<Vec<String>>,
        not_matches: Vec<Vec<String>>,
        location: Option<ErrorLocation>,
    ) {
        self.pending_example_validations
            .push(PendingExampleValidation {
                rules,
                matches,
                not_matches,
                location,
            });
    }

    fn validate_pending_examples_from(&self, start: usize) -> Result<()> {
        for validation in &self.pending_example_validations[start..] {
            let mut rules_by_program = MultiMap::new();
            for rule in &validation.rules {
                rules_by_program.insert(rule.program().to_string(), rule.clone());
            }

            let policy = crate::policy::Policy::from_parts(
                rules_by_program,
                Vec::new(),
                self.host_executables_by_name.clone(),
            );
            validate_not_match_examples(&policy, &validation.rules, &validation.not_matches)
                .map_err(|error| attach_validation_location(error, validation.location.clone()))?;
            validate_match_examples(&policy, &validation.rules, &validation.matches)
                .map_err(|error| attach_validation_location(error, validation.location.clone()))?;
        }

        Ok(())
    }

    fn build(self) -> crate::policy::Policy {
        crate::policy::Policy::from_parts(
            self.rules_by_program,
            self.network_rules,
            self.host_executables_by_name,
        )
    }
}

#[derive(Debug)]
struct PendingExampleValidation {
    rules: Vec<RuleRef>,
    matches: Vec<Vec<String>>,
    not_matches: Vec<Vec<String>>,
    location: Option<ErrorLocation>,
}

/// Strip dangerous globals from the policy Lua VM.
fn strip_dangerous_globals(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    for name in DANGEROUS_GLOBALS {
        globals.raw_set(*name, mlua::Value::Nil)?;
    }
    Ok(())
}

fn register_policy_builtins_and_eval(
    lua: &Lua,
    builder: &RefCell<PolicyBuilder>,
    policy_identifier: &str,
    policy_file_contents: &str,
) -> Result<()> {
    // Use lua.scope() to create non-'static, non-Send closures that can borrow
    // the builder directly. The scope ensures all callbacks are dropped before
    // the borrow ends.
    lua.scope(|scope| {
        let globals = lua.globals();

        globals
            .set(
                "prefix_rule",
                scope
                    .create_function(|_lua, args: Table| {
                        handle_prefix_rule(builder, &args)
                            .map_err(|e| mlua::Error::runtime(e.to_string()))
                    })
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?,
            )
            .map_err(|e| mlua::Error::runtime(e.to_string()))?;

        globals
            .set(
                "network_rule",
                scope
                    .create_function(|_lua, args: Table| {
                        handle_network_rule(builder, &args)
                            .map_err(|e| mlua::Error::runtime(e.to_string()))
                    })
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?,
            )
            .map_err(|e| mlua::Error::runtime(e.to_string()))?;

        globals
            .set(
                "host_executable",
                scope
                    .create_function(|_lua, args: Table| {
                        handle_host_executable(builder, &args)
                            .map_err(|e| mlua::Error::runtime(e.to_string()))
                    })
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?,
            )
            .map_err(|e| mlua::Error::runtime(e.to_string()))?;

        lua.load(policy_file_contents)
            .set_name(policy_identifier)
            .exec()
            .map_err(|e| mlua::Error::runtime(e.to_string()))
    })
    .map_err(|e| Error::Lua(e.to_string()))
}

fn handle_prefix_rule(builder: &RefCell<PolicyBuilder>, args: &Table) -> Result<()> {
    let pattern_table: Table = args
        .get("pattern")
        .map_err(|e| Error::InvalidPattern(format!("missing 'pattern' field: {e}")))?;
    let pattern_tokens = parse_pattern_from_table(&pattern_table)?;

    let decision = match args.get::<Option<String>>("decision") {
        Ok(Some(raw)) => Decision::parse(&raw)?,
        Ok(None) => Decision::Allow,
        Err(e) => return Err(Error::InvalidRule(format!("invalid 'decision' field: {e}"))),
    };

    let justification = match args.get::<Option<String>>("justification") {
        Ok(Some(raw)) if raw.trim().is_empty() => {
            return Err(Error::InvalidRule(
                "justification cannot be empty".to_string(),
            ));
        }
        Ok(Some(raw)) => Some(raw),
        Ok(None) => None,
        Err(e) => {
            return Err(Error::InvalidRule(format!(
                "invalid 'justification' field: {e}"
            )));
        }
    };

    let matches: Vec<Vec<String>> = match args.get::<Option<Table>>("match") {
        Ok(Some(tbl)) => parse_examples_from_table(&tbl)?,
        Ok(None) => Vec::new(),
        Err(e) => return Err(Error::InvalidExample(format!("invalid 'match' field: {e}"))),
    };

    let not_matches: Vec<Vec<String>> = match args.get::<Option<Table>>("not_match") {
        Ok(Some(tbl)) => parse_examples_from_table(&tbl)?,
        Ok(None) => Vec::new(),
        Err(e) => {
            return Err(Error::InvalidExample(format!(
                "invalid 'not_match' field: {e}"
            )));
        }
    };

    let (first_token, remaining_tokens) = pattern_tokens
        .split_first()
        .ok_or_else(|| Error::InvalidPattern("pattern cannot be empty".to_string()))?;

    let rest: Arc<[PatternToken]> = remaining_tokens.to_vec().into();

    let rules: Vec<RuleRef> = first_token
        .alternatives()
        .iter()
        .map(|head| {
            Arc::new(PrefixRule {
                pattern: PrefixPattern {
                    first: Arc::from(head.as_str()),
                    rest: rest.clone(),
                },
                decision,
                justification: justification.clone(),
            }) as RuleRef
        })
        .collect();

    let mut b = builder.borrow_mut();
    b.add_pending_example_validation(rules.clone(), matches, not_matches, None);
    rules.into_iter().for_each(|rule| b.add_rule(rule));
    Ok(())
}

fn handle_network_rule(builder: &RefCell<PolicyBuilder>, args: &Table) -> Result<()> {
    let host: String = args
        .get("host")
        .map_err(|e| Error::InvalidRule(format!("missing 'host' field: {e}")))?;
    let protocol_raw: String = args
        .get("protocol")
        .map_err(|e| Error::InvalidRule(format!("missing 'protocol' field: {e}")))?;
    let decision_raw: String = args
        .get("decision")
        .map_err(|e| Error::InvalidRule(format!("missing 'decision' field: {e}")))?;

    let protocol = NetworkRuleProtocol::parse(&protocol_raw)?;
    let decision = parse_network_rule_decision(&decision_raw)?;

    let justification = match args.get::<Option<String>>("justification") {
        Ok(Some(raw)) if raw.trim().is_empty() => {
            return Err(Error::InvalidRule(
                "justification cannot be empty".to_string(),
            ));
        }
        Ok(Some(raw)) => Some(raw),
        Ok(None) => None,
        Err(e) => {
            return Err(Error::InvalidRule(format!(
                "invalid 'justification' field: {e}"
            )));
        }
    };

    builder.borrow_mut().add_network_rule(NetworkRule {
        host: crate::rule::normalize_network_rule_host(&host)?,
        protocol,
        decision,
        justification,
    });
    Ok(())
}

fn handle_host_executable(builder: &RefCell<PolicyBuilder>, args: &Table) -> Result<()> {
    let name: String = args
        .get("name")
        .map_err(|e| Error::InvalidRule(format!("missing 'name' field: {e}")))?;
    validate_host_executable_name(&name)?;

    let paths_table: Table = args
        .get("paths")
        .map_err(|e| Error::InvalidRule(format!("missing 'paths' field: {e}")))?;

    let mut parsed_paths = Vec::new();
    for pair in paths_table.sequence_values::<String>() {
        let raw = pair.map_err(|e| {
            Error::InvalidRule(format!("host_executable paths must be strings: {e}"))
        })?;
        let path = parse_literal_absolute_path(&raw)?;
        let Some(path_name) = executable_path_lookup_key(path.as_path()) else {
            return Err(Error::InvalidRule(format!(
                "host_executable path `{raw}` must have basename `{name}`"
            )));
        };
        if path_name != executable_lookup_key(&name) {
            return Err(Error::InvalidRule(format!(
                "host_executable path `{raw}` must have basename `{name}`"
            )));
        }
        if !parsed_paths.iter().any(|existing| existing == &path) {
            parsed_paths.push(path);
        }
    }

    builder
        .borrow_mut()
        .add_host_executable(executable_lookup_key(&name), parsed_paths);
    Ok(())
}

fn parse_pattern_from_table(table: &Table) -> Result<Vec<PatternToken>> {
    let mut tokens = Vec::new();
    for pair in table.sequence_values::<Value>() {
        let value = pair.map_err(|e| Error::InvalidPattern(format!("bad pattern element: {e}")))?;
        tokens.push(parse_pattern_token_from_value(&value)?);
    }
    if tokens.is_empty() {
        return Err(Error::InvalidPattern("pattern cannot be empty".to_string()));
    }
    Ok(tokens)
}

fn parse_pattern_token_from_value(value: &Value) -> Result<PatternToken> {
    match value {
        Value::String(s) => Ok(PatternToken::Single(
            s.to_str()
                .map_err(|e| Error::InvalidPattern(format!("invalid utf-8 in pattern: {e}")))?
                .to_string(),
        )),
        Value::Table(tbl) => {
            let mut alts = Vec::new();
            for pair in tbl.sequence_values::<String>() {
                let s = pair.map_err(|e| {
                    Error::InvalidPattern(format!("pattern alternative must be a string: {e}"))
                })?;
                alts.push(s);
            }
            match alts.as_slice() {
                [] => Err(Error::InvalidPattern(
                    "pattern alternatives cannot be empty".to_string(),
                )),
                [single] => Ok(PatternToken::Single(single.clone())),
                _ => Ok(PatternToken::Alts(alts)),
            }
        }
        other => Err(Error::InvalidPattern(format!(
            "pattern element must be a string or table of strings (got {})",
            other.type_name()
        ))),
    }
}

fn parse_examples_from_table(table: &Table) -> Result<Vec<Vec<String>>> {
    let mut examples = Vec::new();
    for pair in table.sequence_values::<Value>() {
        let value = pair.map_err(|e| Error::InvalidExample(format!("bad example element: {e}")))?;
        examples.push(parse_example_from_value(&value)?);
    }
    Ok(examples)
}

fn parse_example_from_value(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::String(s) => {
            let raw = s
                .to_str()
                .map_err(|e| Error::InvalidExample(format!("invalid utf-8: {e}")))?;
            parse_string_example(&raw)
        }
        Value::Table(tbl) => parse_list_example_from_table(tbl),
        other => Err(Error::InvalidExample(format!(
            "example must be a string or table of strings (got {})",
            other.type_name()
        ))),
    }
}

fn parse_string_example(raw: &str) -> Result<Vec<String>> {
    let tokens = shlex::split(raw).ok_or_else(|| {
        Error::InvalidExample("example string has invalid shell syntax".to_string())
    })?;

    if tokens.is_empty() {
        Err(Error::InvalidExample(
            "example cannot be an empty string".to_string(),
        ))
    } else {
        Ok(tokens)
    }
}

fn parse_list_example_from_table(table: &Table) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    for pair in table.sequence_values::<String>() {
        let s = pair
            .map_err(|e| Error::InvalidExample(format!("example tokens must be strings: {e}")))?;
        tokens.push(s);
    }
    if tokens.is_empty() {
        Err(Error::InvalidExample(
            "example cannot be an empty table".to_string(),
        ))
    } else {
        Ok(tokens)
    }
}

fn parse_literal_absolute_path(raw: &str) -> Result<AbsolutePathBuf> {
    if !Path::new(raw).is_absolute() {
        return Err(Error::InvalidRule(format!(
            "host_executable paths must be absolute (got {raw})"
        )));
    }

    AbsolutePathBuf::try_from(raw.to_string())
        .map_err(|error| Error::InvalidRule(format!("invalid absolute path `{raw}`: {error}")))
}

fn validate_host_executable_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidRule(
            "host_executable name cannot be empty".to_string(),
        ));
    }

    let path = Path::new(name);
    if path.components().count() != 1
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return Err(Error::InvalidRule(format!(
            "host_executable name must be a bare executable name (got {name})"
        )));
    }

    Ok(())
}

fn parse_network_rule_decision(raw: &str) -> Result<Decision> {
    match raw {
        "deny" => Ok(Decision::Forbidden),
        other => Decision::parse(other),
    }
}

fn attach_validation_location(error: Error, location: Option<ErrorLocation>) -> Error {
    match location {
        Some(location) => error.with_location(location),
        None => error,
    }
}
