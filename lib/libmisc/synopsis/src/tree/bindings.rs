use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// One explicit substitution from the result board into literal arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// JSON Pointer in the final tool arguments; empty replaces the root.
    pub target: String,
    /// JSON Pointer in the board (`input` plus previously stored results).
    pub source: String,
}

/// A binding failure. Dispatch must not run after one of these errors.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BindingError {
    /// Invalid RFC 6901 pointer syntax.
    #[error("invalid JSON Pointer `{pointer}`")]
    InvalidPointer {
        /// The invalid pointer.
        pointer: String,
    },
    /// The source did not exist; a present null is not missing.
    #[error("binding source `{pointer}` is unavailable")]
    MissingSource {
        /// The unresolved pointer.
        pointer: String,
    },
    /// The target's parent does not exist or is not a container.
    #[error("binding target `{pointer}` has no writable parent")]
    InvalidTarget {
        /// The target that could not be assigned.
        pointer: String,
    },
    /// Tool calls require an object after every substitution is applied.
    #[error("resolved tool arguments must be an object")]
    ArgumentsNotObject,
    /// Target overlap would make substitution order significant.
    #[error("binding targets `{first}` and `{second}` overlap")]
    OverlappingTargets {
        /// First target.
        first: String,
        /// Conflicting target.
        second: String,
    },
}

/// Resolve substitutions atomically on a clone of the literal arguments.
///
/// Sources always read the board, never arguments written by another binding.
/// Object properties may be inserted; array indexes must already exist.
/// Missing parents and the JSON Patch `-` append operation are rejected.
pub fn resolve_arguments(
    arguments: &Value,
    bindings: &[Binding],
    board: &Value,
) -> Result<Value, BindingError> {
    validate_bindings(bindings)?;
    let mut resolved = arguments.clone();
    for binding in bindings {
        let value = board
            .pointer(&binding.source)
            .ok_or_else(|| BindingError::MissingSource {
                pointer: binding.source.clone(),
            })?
            .clone();
        assign(&mut resolved, &binding.target, value)?;
    }
    if !resolved.is_object() {
        return Err(BindingError::ArgumentsNotObject);
    }
    Ok(resolved)
}

pub(super) fn validate_bindings(bindings: &[Binding]) -> Result<(), BindingError> {
    for (index, binding) in bindings.iter().enumerate() {
        validate_pointer(&binding.target)?;
        validate_pointer(&binding.source)?;
        for previous in &bindings[..index] {
            let left = &previous.target;
            let right = &binding.target;
            let nested = |parent: &str, child: &str| {
                child
                    .strip_prefix(parent)
                    .is_some_and(|tail| tail.starts_with('/'))
            };
            if left == right || nested(left, right) || nested(right, left) {
                return Err(BindingError::OverlappingTargets {
                    first: left.clone(),
                    second: right.clone(),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn validate_pointer(pointer: &str) -> Result<(), BindingError> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(BindingError::InvalidPointer {
            pointer: pointer.to_owned(),
        });
    }
    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(BindingError::InvalidPointer {
                pointer: pointer.to_owned(),
            });
        }
    }
    Ok(())
}

fn assign(arguments: &mut Value, target: &str, value: Value) -> Result<(), BindingError> {
    if target.is_empty() {
        *arguments = value;
        return Ok(());
    }
    let invalid = || BindingError::InvalidTarget {
        pointer: target.to_owned(),
    };
    let (parent, key) = target.rsplit_once('/').ok_or_else(invalid)?;
    let key = unescape(key);
    match arguments.pointer_mut(parent).ok_or_else(invalid)? {
        Value::Object(object) => {
            object.insert(key.into_owned(), value);
        }
        Value::Array(array) => {
            let index = array_index(&key).ok_or_else(invalid)?;
            *array.get_mut(index).ok_or_else(invalid)? = value;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

pub(super) fn validate_target(arguments: &Value, target: &str) -> Result<(), BindingError> {
    validate_pointer(target)?;
    if target.is_empty() {
        return Ok(());
    }
    let invalid = || BindingError::InvalidTarget {
        pointer: target.to_owned(),
    };
    let (parent, key) = target.rsplit_once('/').ok_or_else(invalid)?;
    match arguments.pointer(parent).ok_or_else(invalid)? {
        Value::Object(_) => Ok(()),
        Value::Array(array)
            if array_index(&unescape(key)).is_some_and(|index| index < array.len()) =>
        {
            Ok(())
        }
        _ => Err(invalid()),
    }
}

fn unescape(token: &str) -> Cow<'_, str> {
    if token.contains('~') {
        Cow::Owned(token.replace("~1", "/").replace("~0", "~"))
    } else {
        Cow::Borrowed(token)
    }
}

fn array_index(key: &str) -> Option<usize> {
    if key.starts_with('+') || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    key.parse().ok()
}
